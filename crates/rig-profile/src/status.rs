//! Read-only runtime inventory; never depends on a working shell backend.

use rig_harness::prelude::*;
use rig_harness::rig_core::tool::{ToolOutput, ToolResult};
use rig_native_shell::NativeShellStatus;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;

pub(super) const NAME: &str = "agent_status";

pub(super) fn install(app: &mut App) {
    app.add_open_tool(
        NAME,
        "Report actual harness-registered Rig plugins, the tools this agent may call, its working directory, and native-shell startup validation. This reads runtime metadata only; it needs no shell and does not probe filesystem/network enforcement. Skills are not counted as loaded plugins.",
        ToolOptions { footprint: Some(Touches::Independent) },
        report,
    );
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Request {}

fn report(
    called: On<ToolCalled<Request>>,
    plugins: Query<(&Name, &PluginSource)>,
    tools: Query<&ToolDef>,
    agents: Query<(&ToolAccess, Option<&WorkDir>)>,
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
    let mut tools: Vec<_> = tools
        .iter()
        .filter(|tool| agent.is_some_and(|(access, _)| access.allows(tool.0.name.as_str())))
        .map(|tool| tool.0.name.as_str())
        .collect();
    tools.sort_unstable();
    let result = json!({
        "registered_plugins": plugins,
        "allowed_tools": tools,
        "work_dir": agent.and_then(|(_, dir)| dir.map(|dir| &dir.0)),
        "native_shell": native.as_deref().map_or_else(
            || json!({"state": "disabled"}),
            |status| json!(status),
        ),
        "native_shell_validation": native.as_ref().map(|_| "startup_compatibility_only"),
    });
    commands
        .entity(called.call)
        .insert(Outcome::from(ToolResult::success(ToolOutput::json(result))));
}
