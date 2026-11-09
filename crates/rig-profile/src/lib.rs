//! Agent composition, separate from reusable execution and approval plugins.

mod status;

use rig_basics::{
    BasicCommandsPlugin, activity, agents_md, coding_tools, compaction, login, models, print,
    prompt, sessions, skills, subagents, telemetry, welcome,
};
use rig_harness::prelude::*;

/// The personal agent's capabilities. The frontend remains independently selectable.
#[derive(Default, Debug)]
pub struct RigProfilePlugin;

impl Plugin for RigProfilePlugin {
    fn build(&self, app: &mut App) {
        add_builtin::<prompt::DefaultPromptPlugin>(app);
        add_local::<rig_conversation::ConversationPlugin>(app, "rig-conversation");
        add_builtin::<agents_md::AgentsMdPlugin>(app);
        add_builtin::<skills::SkillsPlugin>(app);
        add_builtin::<models::ModelsPlugin>(app);
        add_builtin::<login::LoginPlugin>(app);
        add_builtin::<models::defaults::DefaultsPlugin>(app);
        add_builtin::<sessions::SessionsPlugin>(app);
        add_builtin::<sessions::agents::AgentsPlugin>(app);
        add_builtin::<compaction::CompactionPlugin>(app);
        add_builtin::<telemetry::usage::UsagePlugin>(app);
        add_builtin::<telemetry::effect_log::EffectLogPlugin>(app);
        add_builtin::<activity::ActivityPlugin>(app);
        add_builtin::<coding_tools::ReadTool>(app);
        add_builtin::<coding_tools::EditTool>(app);
        add_builtin::<coding_tools::WriteTool>(app);
        add_builtin::<coding_tools::AttachPlugin>(app);
        add_builtin::<subagents::SubagentsPlugin>(app);
        add_local::<rig_checkpoint_tasks::CheckpointTasksPlugin>(app, "rig-checkpoint-tasks");
        add_builtin::<BasicCommandsPlugin>(app);
        add_builtin::<welcome::WelcomePlugin>(app);
        add_builtin::<print::PrintPlugin>(app);
        add_local::<rig_bashkit::BashkitPlugin>(app, "rig-bashkit");
        add_local::<rig_native_shell::NativeShellPlugin>(app, "rig-native-shell");
        add_local::<rig_delta::DeltaPlugin>(app, "rig-delta");
        add_local::<rig_auto_review::AutoReviewPlugin>(app, "rig-auto-review");
        add_local::<rig_monty::MontyPlugin>(app, "rig-monty");
        add_local::<rig_xonsh::XonshPlugin>(app, "rig-xonsh");
        #[cfg(feature = "tui")]
        {
            add_local::<rig_tui_tools::TuiToolsPlugin>(app, "rig-tui-tools");
            add_local::<rig_tui_loading::TuiLoadingPlugin>(app, "rig-tui-loading");
            add_local::<rig_tui_games::TuiGamesPlugin>(app, "rig-tui-games");
        }
        status::install(app);

        app.init_resource::<ProjectRoot>()
            .add_observer(default_workdir)
            .add_systems(
                AgentPass,
                restrict_workdir
                    .in_set(Stage::Gate)
                    .before(rig_delta::DeltaSystems::Ownership)
                    .before(rig_native_shell::NativeShellSystems::Freeze),
            );
        app.configure_sets(
            AgentPass,
            rig_delta::DeltaSystems::WorkspacePrompt.after(rig_conversation::ConversationPrompt),
        );
        app.world_mut().spawn(PromptSection::new(
            PromptSection::ORDER_ENVIRONMENT,
            "runtime_capabilities",
            "When asked which plugins or tools are loaded, call agent_status. It reads \
             actual runtime registrations and native-shell startup diagnostics without \
             executing a shell. Skills are instructions, not proof of active integrations. \
             If a backend is unavailable at startup, repeating its command will not repair \
             it; use the diagnostic and repair setup before restarting the agent.",
        ));
        app.world_mut().spawn(PromptSection::new(
            PromptSection::ORDER_ENVIRONMENT,
            "execution_backends",
            "Use native_shell for real workspace commands. Its default is an OS sandbox with \
             workspace writes and no network. An explicit escalation asks a separate reviewer \
             for permission to run that one complete command outside the sandbox. Explain \
             the necessary permission in justification; never retry automatically after a \
             failure or interruption. virtual_bash is a separate, stateless in-memory shell: \
             supply text files explicitly; it cannot see workspace files or run native programs. \
             File tools are confined to each agent's work directory inside the project. \
             run_monty runs bounded Python in a fresh worker; tool(name, arguments) \
             returns {status, data} from ordinary gated child tools. Check status before \
             using data; code mode does not grant extra permission. \
             run_xonsh adds explicit $(...) and $[...] shell forms and whole-word \
             @(expression) argument interpolation. Its native capture combines stdout \
             and stderr; virtual files are explicit and stateless for each form. \
             Do not publish or comment externally without explicit user authorization.",
        ));
        app.world_mut().spawn(PromptSection::new(
            PromptSection::ORDER_PROJECT + 20,
            "installed_skill_files",
            "When a discovered installed skill or its references live outside the project, \
             read those specific skill files with native_shell without escalation. Its \
             sandbox permits those reads; the read/edit/write tools remain confined to \
             the agent's work directory. This exception is for loading discovered skills, \
             not permission to inspect unrelated credentials or private files.",
        ));
    }

    fn finish(&self, app: &mut App) {
        // These independent filters mutate PendingInput. Composition owns
        // their deterministic order; the capabilities remain independent.
        #[cfg(feature = "tui")]
        if app.is_plugin_added::<rig_tui_scroll::TuiScrollPlugin>()
            && app.is_plugin_added::<rig_tui_history::TuiHistoryPlugin>()
        {
            app.configure_sets(
                rig_tui::TuiKeys,
                (
                    rig_tui_games::GamesInput,
                    rig_tui_scroll::WheelInput,
                    rig_tui_tools::ScriptToolClicks,
                    rig_tui_history::HistoryInput,
                )
                    .chain(),
            );
        }
        #[cfg(not(feature = "tui"))]
        let _ = app;
    }
}

// Composition must also work when a user independently lists one of these plugins.
fn add_builtin<P: Plugin + Default>(app: &mut App) {
    rig_harness::load::<P>(app, "rig-basics", "version 0.46.0");
}

fn add_local<P: Plugin + Default>(app: &mut App, package: &str) {
    rig_harness::load::<P>(app, package, "workspace");
}

fn default_workdir(
    added: On<Add<Agent>>,
    agents: Query<(), (Without<WorkDir>, Without<SpawnedBy>)>,
    root: Res<ProjectRoot>,
    mut commands: Commands,
) {
    // ServicesPlugin owns child inheritance. Defaulting a child here races its
    // try_insert_if_new and can replace a narrower parent directory with root.
    if agents.contains(added.entity) {
        commands
            .entity(added.entity)
            .insert(WorkDir(root.0.clone()));
    }
}

// Ordinary agents remain inside the original project. A fresh joined root may
// use only its native-user-approved, identity-bound mount roots; a stale scope
// must never fall back to the original root. File tools still confine each call
// to the selected WorkDir, and the native launcher sandboxes that directory.
fn restrict_workdir(
    mut calls: Query<(&ToolRequest, &ForAgent, &mut Decision), (ReadyToStart, ToolCalls)>,
    agents: Query<(&WorkDir, &AgentId, Option<&rig_delta::LiveWorkspace>)>,
    root: Res<ProjectRoot>,
) {
    let project = root.0.canonicalize();
    for (request, &ForAgent(agent), mut decision) in &mut calls {
        if matches!(
            request.call.function.name.as_str(),
            status::NAME | "delta_session"
        ) {
            continue; // Runtime metadata remains available to diagnose a broken workdir.
        }
        let allowed = agents
            .get(agent)
            .ok()
            .is_some_and(|(dir, id, scope)| match scope {
                Some(scope) => scope.allows(id, &dir.0),
                None => project.as_ref().ok().is_some_and(|project| {
                    dir.0
                        .canonicalize()
                        .is_ok_and(|dir| dir.starts_with(project))
                }),
            });
        if !allowed {
            decision.decide(
                Decider::plugin("rig-profile"),
                Outcome::refused("The agent's work directory must resolve inside its project or explicitly approved joined-worktree scope."),
            );
        }
    }
}

#[cfg(test)]
mod tests;
