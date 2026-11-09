//! A native shell whose escalations need a permit for the exact prepared call.
//!
//! The Gate stage freezes command source, directory and environment. The open
//! tool executes that owned snapshot, and checks the reviewer's user evidence
//! again immediately before starting broader execution.

mod authorization;
mod command;
mod launch;

use std::path::PathBuf;
use std::sync::Arc;

pub use command::{PendingCommand, ReviewPermit};
use command::{ShellArgs, freeze, require_permit};
use rig_command::NativeExecutor;
use rig_harness::prelude::*;
use rig_harness::rig_core::tool::{Scope, Touches};
use serde::{Deserialize, Serialize};

pub use authorization::{
    Authorization, AuthorizationError, EvidenceSize, ScopedProjectInstructions, UserEvidence,
};

pub const NATIVE_SHELL: &str = "native_shell";

/// Runs native commands under the configured sandbox, or a separately reviewed exception.
#[derive(Default, Debug)]
pub struct NativeShellPlugin;

impl Plugin for NativeShellPlugin {
    fn build(&self, app: &mut App) {
        app.add_setting::<ShellConfig>("native_shell")
            .add_observer(authorization::track_generation)
            .init_resource::<ProjectRoot>()
            .configure_sets(
                AgentPass,
                (
                    NativeShellSystems::Freeze,
                    NativeShellSystems::Review,
                    NativeShellSystems::RequirePermit,
                )
                    .chain()
                    .in_set(Stage::Gate),
            )
            .add_systems(AgentPass, freeze.in_set(NativeShellSystems::Freeze))
            .add_systems(AgentPass, require_permit.in_set(NativeShellSystems::RequirePermit))
            .add_open_tool(
                NATIVE_SHELL,
                "Run a native command in the workspace through the OS sandbox. Native commands can run cargo, git, and other real programs. Set escalate=true only to request a separately reviewed exception, and explain the required permission in justification. A failed command never retries automatically. Commands have a maximum timeout of 300 seconds.",
                ToolOptions { footprint: Some(Touches::Writes(Scope::Anything)) },
                execute,
            );
    }

    fn finish(&self, app: &mut App) {
        let config = app.world().resource::<ShellConfig>();
        let executable = match &config.codex {
            Some(path) => Ok(path.clone()),
            None => find_codex(RigHome::of(app.world()).root()),
        };
        let selected = executable.as_ref().ok().cloned();
        let executor = executable
            .and_then(|path| NativeExecutor::new(path).map_err(|error| error.to_string()))
            .map(Arc::new);
        let status = match &executor {
            Ok(executor) => NativeShellStatus::Ready {
                executable: executor.executable().to_owned(),
            },
            Err(error) => NativeShellStatus::Unavailable {
                executable: selected,
                error: error.clone(),
            },
        };
        app.insert_resource(status)
            .insert_resource(Executor(executor));
    }
}

/// Other plugins review snapshots after `Freeze` and before `RequirePermit`.
#[derive(SystemSet, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum NativeShellSystems {
    /// Freeze final tool arguments into a validated command.
    Freeze,
    /// Other policy plugins may hold or refuse the frozen command here.
    Review,
    /// Refuse escalations that no policy held or approved.
    RequirePermit,
}

#[derive(Resource, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct ShellConfig {
    codex: Option<PathBuf>,
    timeout_secs: u64,
}

impl Default for ShellConfig {
    fn default() -> Self {
        Self {
            codex: None,
            timeout_secs: 60,
        }
    }
}

#[derive(Resource)]
struct Executor(Result<Arc<NativeExecutor>, String>);

/// Startup diagnostics, not an execution permit or proof of OS containment.
#[derive(Resource, Debug, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum NativeShellStatus {
    Ready {
        executable: PathBuf,
    },
    Unavailable {
        executable: Option<PathBuf>,
        error: String,
    },
}

fn find_codex(home: &std::path::Path) -> Result<PathBuf, String> {
    // The curated setup copies a known native release here. PATH may instead
    // select an npm wrapper whose Node runtime is deliberately not inherited.
    let pinned = home.join("tools/bin/codex-sandbox");
    if pinned.is_file() {
        return Ok(pinned);
    }
    let path = std::env::var_os("PATH").ok_or("PATH is unset; configure [native_shell] codex")?;
    std::env::split_paths(&path)
        .filter(|path| path.is_absolute())
        .map(|path| path.join("codex"))
        .find(|path| path.is_file())
        .ok_or_else(|| "codex was not found; configure [native_shell] codex".to_owned())
}

fn execute(called: On<ToolCalled<ShellArgs>>, mut commands: Commands) {
    let call = called.call;
    let agent = called.agent;
    let arguments = called.tool_call.function.arguments.clone();
    commands.queue(move |world: &mut World| launch::start(world, call, agent, arguments));
}

fn refuse(decision: &mut Decision, message: impl Into<String>) {
    decision.decide(
        Decider::plugin("native-shell"),
        Outcome::refused(message.into()),
    );
}
