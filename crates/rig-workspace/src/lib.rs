//! Working-directory policy, without selecting tools or installing execution.
//!
//! Root agents receive a `WorkDir`, which makes Rig's stock file tools confine
//! paths rather than merely resolve them relative to the project. Every tool
//! call checks that directory against the original project, or an explicitly
//! trusted, identity-bound worktree scope. ServicesPlugin owns child inheritance.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use rig_harness::prelude::*;

/// Confines tool calls to the project or native-user-approved worktrees.
///
/// Adds no tools, other plugins, process launcher or escalation permission.
#[derive(Default, Debug)]
pub struct WorkspacePlugin;

impl Plugin for WorkspacePlugin {
    fn build(&self, app: &mut App) {
        configure_gate_order(app);
        app.init_resource::<ProjectRoot>()
            .init_resource::<PolicyInstalled>()
            .add_observer(default_workdir)
            .add_observer(inherit_denial)
            .add_observer(revoke_descendants)
            .add_systems(AgentPass, restrict_workdir.in_set(WorkspaceSystems::Gate));
    }
}

/// Order the workspace boundary after stock command, question and depth checks.
///
/// Downstream gates call this even when workspace policy is not selected. It
/// configures only scheduling: no policy, tools or other plugins are installed.
pub fn configure_gate_order(app: &mut App) {
    app.configure_sets(
        AgentPass,
        WorkspaceSystems::Gate
            .in_set(Stage::Gate)
            .after(rig_harness::ask::AskSystems)
            .after(rig_basics::subagents::SubagentSystems::Gate),
    );
}

#[derive(Resource, Default)]
struct PolicyInstalled;

const INVALID_WORKDIR: &str = "The agent's work directory must resolve inside its project or explicitly approved worktree scope.";

/// Recheck the selected policy synchronously at a native launch boundary.
///
/// This neither selects policy nor grants scope. Hosts without WorkspacePlugin
/// retain their own policy; when selected, missing or revoked scope fails closed.
pub fn validate_workdir(world: &World, agent: Entity) -> Result<(), &'static str> {
    if !world.contains_resource::<PolicyInstalled>() {
        return Ok(());
    }
    let directory = world.get::<WorkDir>(agent).ok_or(INVALID_WORKDIR)?;
    let id = world.get::<AgentId>(agent).ok_or(INVALID_WORKDIR)?;
    let root = world.get_resource::<ProjectRoot>().ok_or(INVALID_WORKDIR)?;
    let project = root.0.canonicalize();
    allowed_directory(
        &directory.0,
        id,
        world.get::<WorkDirScope>(agent),
        project.as_deref().ok(),
    )
    .then_some(())
    .ok_or(INVALID_WORKDIR)
}

fn allowed_directory(
    directory: &Path,
    owner: &AgentId,
    scope: Option<&WorkDirScope>,
    project: Option<&Path>,
) -> bool {
    match scope {
        Some(scope) => scope.allows(owner, directory),
        None => project.is_some_and(|project| {
            directory
                .canonicalize()
                .is_ok_and(|directory| directory.is_dir() && directory.starts_with(project))
        }),
    }
}

/// Execution and collaboration gates run after the workspace policy.
#[derive(SystemSet, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum WorkspaceSystems {
    Gate,
}

/// Marks a non-tool native operation for the same work-directory gate as tools.
///
/// This marker grants no authority and installs no handler. Execution runtimes
/// require it on their concrete requests so workspace policy cannot be skipped
/// merely because an operation is not a model-visible tool call.
#[derive(Component, Default, Debug)]
pub struct WorkspaceOperation;

/// A trusted host's worktree grant for exactly one stable agent identity.
///
/// Construct only after native-user confirmation of the complete worktree
/// scope. This is not model input, a saved component or a transferable permit.
/// Rust plugins remain trusted code. A copy cannot authorize another identity,
/// and a stale or denied scope never falls back to the original project.
#[derive(Component, Clone, Debug, PartialEq, Eq)]
#[component(immutable)]
pub struct WorkDirScope(Option<TrustedWorktrees>);

#[derive(Clone, Debug, PartialEq, Eq)]
struct TrustedWorktrees {
    owner: AgentId,
    roots: Vec<PathBuf>,
}

impl WorkDirScope {
    /// Freeze already-confirmed, canonical worktree roots for this identity.
    ///
    /// The confirming integration must enforce its own additional exclusions
    /// (for example, Delta refuses Rig home and any directory containing it).
    /// Inputs are borrowed during inspection and copied into the immutable grant.
    pub fn trusted_worktrees(owner: &AgentId, roots: &[PathBuf]) -> Result<Self, String> {
        if owner.0.trim().is_empty() || roots.is_empty() || roots.len() > 32 {
            return Err("A worktree scope needs an agent identity and 1 to 32 roots".into());
        }
        let mut distinct = BTreeSet::new();
        for root in roots {
            if !root.is_absolute()
                || root.parent().is_none()
                || !root.is_dir()
                || !root
                    .canonicalize()
                    .is_ok_and(|canonical| canonical == *root)
                || !distinct.insert(root)
            {
                return Err("Worktree roots must be distinct existing canonical directories, not the filesystem root".into());
            }
        }
        Ok(Self(Some(TrustedWorktrees {
            owner: owner.clone(),
            roots: roots.to_owned(),
        })))
    }

    /// Revoke a joined scope without allowing fallback to the launch project.
    pub fn denied() -> Self {
        Self(None)
    }

    /// Check identity, canonical directory and the continued identity of roots.
    pub fn allows(&self, owner: &AgentId, directory: &Path) -> bool {
        let Some(scope) = &self.0 else {
            return false;
        };
        if owner != &scope.owner {
            return false;
        }
        let Ok(directory) = directory.canonicalize() else {
            return false;
        };
        directory.is_dir()
            && scope.roots.iter().any(|root| {
                root.canonicalize().is_ok_and(|current| current == *root)
                    && directory.starts_with(root)
            })
    }
}

fn default_workdir(
    added: On<Add<Agent>>,
    agents: Query<(), (Without<WorkDir>, Without<SpawnedBy>)>,
    root: Res<ProjectRoot>,
    mut commands: Commands,
) {
    // Defaulting children here would race ServicesPlugin's narrower inheritance.
    if agents.contains(added.entity) {
        commands
            .entity(added.entity)
            .insert(WorkDir(root.0.clone()));
    }
}

// Negative scope changes propagate through real ancestry; positive grants do
// not. The trusted integration alone may rebind a grant to a child identity.
fn inherit_denial(
    added: On<Add<SpawnedBy>>,
    children: Query<&SpawnedBy>,
    parents: Query<(&AgentId, &WorkDirScope, Option<&WorkDir>)>,
    mut commands: Commands,
) {
    let Ok(parent) = children.get(added.entity) else {
        return;
    };
    if let Ok((id, scope, dir)) = parents.get(parent.0)
        && !dir.is_some_and(|dir| scope.allows(id, &dir.0))
    {
        commands.entity(added.entity).insert(WorkDirScope::denied());
    }
}

fn revoke_descendants(
    inserted: On<Insert<WorkDirScope>>,
    parents: Query<(&AgentId, &WorkDirScope, Option<&WorkDir>)>,
    children: Query<(Entity, &SpawnedBy, Option<&WorkDirScope>)>,
    mut commands: Commands,
) {
    let Ok((id, scope, dir)) = parents.get(inserted.entity) else {
        return;
    };
    if dir.is_some_and(|dir| scope.allows(id, &dir.0)) {
        return;
    }
    let denied = WorkDirScope::denied();
    for (child, parent, existing) in &children {
        if parent.0 == inserted.entity && existing != Some(&denied) {
            commands.entity(child).insert(denied.clone());
        }
    }
}

fn restrict_workdir(
    mut calls: Query<
        (Option<&ToolRequest>, &ForAgent, &mut Decision),
        (
            ReadyToStart,
            Or<(With<ToolRequest>, With<WorkspaceOperation>)>,
        ),
    >,
    agents: Query<(&WorkDir, &AgentId, Option<&WorkDirScope>)>,
    root: Res<ProjectRoot>,
) {
    let project = root.0.canonicalize();
    for (request, &ForAgent(agent), mut decision) in &mut calls {
        if request.is_some_and(|request| {
            matches!(
                request.call.function.name.as_str(),
                "agent_status" | "auto_review_status" | "delta_session"
            )
        }) {
            continue; // Metadata remains available to diagnose a broken workdir.
        }
        let allowed = agents.get(agent).ok().is_some_and(|(dir, id, scope)| {
            allowed_directory(&dir.0, id, scope, project.as_deref().ok())
        });
        if !allowed {
            decision.decide(
                Decider::plugin("workspace"),
                Outcome::refused(INVALID_WORKDIR),
            );
        }
    }
}

#[cfg(test)]
mod tests;
