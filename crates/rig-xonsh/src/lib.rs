//! A deliberately small xonsh frontend; host execution belongs to existing tools.

mod frontend;
mod shell;

use std::collections::BTreeMap;

use rig_harness::prelude::*;
use rig_harness::rig_core::tool::{Scope, ToolExecutionError, ToolResult};
use rig_monty::{MontyPlugin, Program};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};

pub const RUN_XONSH: &str = "run_xonsh";
const PRELUDE: &str = include_str!("shell.py");

/// Adds explicit shell forms to Monty's bounded Python, with no new host authority.
#[derive(Default, Debug)]
pub struct XonshPlugin;

impl Plugin for XonshPlugin {
    fn build(&self, app: &mut App) {
        if !app.is_plugin_added::<MontyPlugin>() {
            app.add_plugins(MontyPlugin);
        }
        app.add_open_tool(
            RUN_XONSH,
            "Run a focused xonsh subset in a fresh Monty worker: Python plus $(command) captured text and $[command] printed output/None. Commands support literal argv, quotes, complete | pipelines and standalone @(expression) arguments (string or list of strings). Interpolation is argument data, never shell source. backend selects native (default, sandboxed native_shell) or virtual (stateless virtual_bash with explicit files mapping). Native capture includes combined stdout/stderr. Inspect or handle failures; denied, failed or truncated shell results raise RuntimeError. No outer escalation, redirection, background jobs, env/glob expansion, bare command fallback, nested shell forms or f-strings. All Monty limits apply; xonsh source is at most 48 KiB.",
            ToolOptions { footprint: Some(Touches::Writes(Scope::Anything)) },
            execute,
        );
    }
}

#[derive(Default, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum Backend {
    #[default]
    Native,
    Virtual,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Request {
    code: String,
    #[serde(default)]
    inputs: BTreeMap<String, Value>,
    #[serde(default)]
    backend: Backend,
    /// Virtual text files, resupplied to each independent shell form.
    #[serde(default)]
    files: BTreeMap<String, String>,
}

fn prepare(request: &Request) -> Result<Program, String> {
    if request.code.trim().is_empty() || request.code.len() > 48 * 1024 {
        return Err("xonsh source must contain 1..=49152 bytes".to_owned());
    }
    if request
        .inputs
        .keys()
        .any(|name| name.starts_with("__rig_xonsh_"))
    {
        return Err("input names beginning __rig_xonsh_ are reserved".to_owned());
    }
    if matches!(request.backend, Backend::Native) && !request.files.is_empty() {
        return Err(
            "files are explicit virtual inputs; native execution uses workspace files".to_owned(),
        );
    }
    let lowered = frontend::lower(&request.code)?;
    let code = format!("{PRELUDE}\n{lowered}");
    if code.len() > 64 * 1024 {
        return Err("lowered Python exceeds Monty's 64 KiB source limit".to_owned());
    }
    let mut inputs = request.inputs.clone();
    inputs.insert(
        "__rig_xonsh_backend".into(),
        json!(match request.backend {
            Backend::Native => "native",
            Backend::Virtual => "virtual",
        }),
    );
    inputs.insert(
        "__rig_xonsh_files".into(),
        serde_json::to_value(&request.files).map_err(|error| error.to_string())?,
    );
    Ok(Program { code, inputs })
}

fn execute(called: On<ToolCalled<Request>>, mut commands: Commands) {
    match prepare(&called.args) {
        Ok(program) => {
            let (call, agent) = (called.call, called.agent);
            commands.queue(move |world: &mut World| rig_monty::start(world, call, agent, program));
        }
        Err(error) => {
            commands
                .entity(called.call)
                .insert(Outcome::from(ToolResult::failed(
                    ToolExecutionError::invalid_args(error),
                )));
        }
    }
}
