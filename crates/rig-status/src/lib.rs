//! Read-only runtime inventory, independent of agent composition and execution.
//!
//! This plugin installs no other plugins and starts no processes. A native-shell
//! status resource is inspected only when the host separately selected it.

use rig_harness::prelude::*;
use rig_harness::rig_core::tool::{ToolOutput, ToolResult};
use rig_native_shell::NativeShellStatus;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;

pub const AGENT_STATUS: &str = "agent_status";

/// Reports actual harness registrations and the calling agent's allowed tools.
#[derive(Default, Debug)]
pub struct StatusPlugin;

impl Plugin for StatusPlugin {
    fn build(&self, app: &mut App) {
        app.add_open_tool(
            AGENT_STATUS,
            "Report actual harness-registered Rig plugins, the tools this agent may call, its working directory, and shared native-execution startup validation separately from public native-shell tool registration. This reads runtime metadata only; it needs no shell and does not probe filesystem/network enforcement. Skills are not counted as loaded plugins.",
            ToolOptions { footprint: Some(Touches::Independent) },
            report,
        );
        app.world_mut().spawn(PromptSection::new(
            PromptSection::ORDER_ENVIRONMENT,
            "runtime_capabilities",
            "When asked which plugins or tools are loaded, call agent_status. It reads \
             actual runtime registrations and shared native-execution diagnostics without \
             executing a shell. Skills are instructions, not proof of active integrations. \
             If a backend is unavailable at startup, repeating its command will not repair \
             it; use the diagnostic and repair setup before restarting the agent.",
        ));
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Request {}

fn report(
    called: On<ToolCalled<Request>>,
    plugins: Query<(&Name, &PluginSource)>,
    tools: Query<&ToolDef>,
    agents: Query<(&ToolAccess, Option<&WorkDir>)>,
    root: Option<Res<ProjectRoot>>,
    native: Option<Res<NativeShellStatus>>,
    mut commands: Commands,
) {
    let mut plugins: Vec<_> = plugins.iter().collect();
    plugins.sort_by(|(left, _), (right, _)| left.as_str().cmp(right.as_str()));
    let plugins: Vec<_> = plugins
        .into_iter()
        .map(|(name, source)| {
            json!({
                "name": name.as_str(), "package": source.package, "source": source.source,
            })
        })
        .collect();
    let agent = agents.get(called.agent).ok();
    let native_shell_tool_registered = tools
        .iter()
        .any(|tool| tool.0.name.as_str() == rig_native_shell::NATIVE_SHELL);
    let mut tools: Vec<_> = tools
        .iter()
        .filter(|tool| agent.is_some_and(|(access, _)| access.allows(tool.0.name.as_str())))
        .map(|tool| tool.0.name.as_str())
        .collect();
    tools.sort_unstable();
    let result = json!({
        "registered_plugins": plugins,
        "allowed_tools": tools,
        "work_dir": agent.and_then(|(_, dir)| dir.map(|dir| &dir.0))
            .or_else(|| root.as_ref().map(|root| &root.0)),
        "native_execution": native.as_deref().map_or_else(
            || json!({"state": "disabled"}),
            |status| json!(status),
        ),
        "native_execution_validation": native.as_ref().map(|_| "startup_compatibility_only"),
        "native_shell_tool_registered": native_shell_tool_registered,
    });
    commands
        .entity(called.call)
        .insert(Outcome::from(ToolResult::success(ToolOutput::json(result))));
}

#[cfg(test)]
mod tests;
