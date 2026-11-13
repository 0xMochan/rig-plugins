//! A focused xonsh frontend over separately gated native command effects.

mod frontend;
mod shell;

use std::collections::BTreeMap;

use rig_harness::prelude::*;
use rig_harness::rig_core::tool::{Scope, ToolExecutionError, ToolResult};
use rig_monty::{MontyRuntimePlugin, Program};
use rig_native_shell::{NativeCommandRuntimePlugin, NativeExecution};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};

pub const RUN_XONSH: &str = "run_xonsh";
const PRELUDE: &str = include_str!("shell.py");
const USAGE: &str = include_str!("usage.md");

/// Adds shell forms over bounded Python and the shared gated native runtime.
/// Public shell and Python tools remain separately selectable frontends.
#[derive(Default, Debug)]
pub struct XonshPlugin;

impl Plugin for XonshPlugin {
    fn build(&self, app: &mut App) {
        rig_harness::load::<MontyRuntimePlugin>(app, "rig-monty", "dependency");
        rig_harness::load::<NativeCommandRuntimePlugin>(app, "rig-native-shell", "dependency");
        app.add_open_tool(
            RUN_XONSH,
            USAGE,
            ToolOptions {
                footprint: Some(Touches::Writes(Scope::Anything)),
            },
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

#[derive(Default, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum Mode {
    #[default]
    Mixed,
    Commands,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Request {
    code: String,
    /// mixed (default): Python with explicit command forms. commands: bare
    /// newline-separated pipelines; no Python/shell guessing or runtime fallback.
    #[serde(default)]
    mode: Mode,
    #[serde(default)]
    inputs: BTreeMap<String, Value>,
    #[serde(default)]
    backend: Backend,
    /// Virtual text files, resupplied to each independent shell form.
    #[serde(default)]
    files: BTreeMap<String, String>,
    /// Native command defaults. Each pipeline is frozen and reviewed separately;
    /// this does not authorize the outer Python program or retry failed work.
    #[serde(default)]
    execution: Option<NativeExecution>,
}

fn prepare(request: &Request) -> Result<Program, String> {
    if request.code.trim().is_empty()
        || request.code.len() > 48 * 1024
        || request.code.contains('\0')
    {
        return Err("xonsh source must contain 1..=49152 bytes and no NUL".to_owned());
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
    if matches!(request.backend, Backend::Virtual) && request.execution.is_some() {
        return Err(
            "execution options apply only to native commands, not the virtual backend".to_owned(),
        );
    }
    if let Some(execution) = &request.execution {
        execution.validate()?;
    }
    let lowered = match request.mode {
        Mode::Mixed => frontend::lower(&request.code)?,
        Mode::Commands => frontend::lower_paths(&shell::commands(&request.code)?)?,
    };
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
            let native = matches!(called.args.backend, Backend::Native)
                .then(|| called.args.execution.clone().unwrap_or_default());
            commands.queue(move |world: &mut World| match native {
                Some(execution) => {
                    rig_monty::start_native_xonsh(world, call, agent, program, execution)
                }
                None => rig_monty::start(world, call, agent, program),
            });
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

#[cfg(test)]
mod tests;
