//! Process-isolated Python whose host operations remain ordinary Rig effects.

mod host;
mod runtime;
mod values;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use bevy_tasks::{AsyncComputeTaskPool, TaskPool};
use rig_harness::prelude::*;
use rig_harness::rig_core::tool::{Scope, ToolExecutionError, ToolOutput, ToolResult};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const RUN_MONTY: &str = "run_monty";

/// A bounded Python program; inputs become globals for this invocation only.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Program {
    pub code: String,
    #[serde(default)]
    pub inputs: BTreeMap<String, Value>,
}

/// The last Python value and separately captured streams. Values are never truncated.
#[derive(Debug, Serialize)]
pub struct Output {
    pub value: Value,
    pub stdout: String,
    pub stderr: String,
    pub output_truncated: bool,
}

/// Installs `run_monty`; worker setup is separate from tool execution.
#[derive(Default, Debug)]
pub struct MontyPlugin;

impl Plugin for MontyPlugin {
    fn build(&self, app: &mut App) {
        app.add_setting::<Config>("monty")
            .init_resource::<host::Requests>()
            .add_systems(
                Update,
                host::dispatch.before(PollTasks).before(AgentLoop),
            )
            .add_observer(host::answer)
            .add_observer(host::bind_owner)
            .add_observer(follow_successor::<host::HostCall>)
            .add_observer(despawn_open_children::<host::HostCall>)
            .add_open_tool(
                RUN_MONTY,
                "Run bounded Python in a fresh Monty worker. inputs supplies JSON globals; the last expression is returned as JSON. tool(name, arguments) calls one of your own Rig tools and returns {status, data}; inspect status before using data. Host calls use ordinary permission gates. Python has no host files, environment, network, or subprocesses. No persistent sessions, snapshots, automatic retries, or recursive interpreter calls. Limits: 64 KiB source/inputs, 32 MiB worker allocations plus runtime headroom, 5 seconds of interpreter execution, 64 suspensions, 300 seconds including host waits, 64 KiB captured print output, 256 KiB JSON values/replies.",
                ToolOptions { footprint: Some(Touches::Writes(Scope::Anything)) },
                execute,
            );
    }

    fn finish(&self, app: &mut App) {
        let path = app
            .world()
            .resource::<Config>()
            .worker
            .clone()
            .unwrap_or_else(|| RigHome::of(app.world()).root().join("tools/bin/monty"));
        let worker = if path.is_absolute() {
            path.canonicalize().map_err(|error| format!(
                "Monty worker {} is unavailable: {error}. Run the profile setup or configure [monty] worker with a trusted absolute Monty 1.4.0 binary.", path.display()
            ))
        } else {
            Err("[monty] worker must be an absolute executable path".to_owned())
        };
        app.insert_resource(Worker(worker));
    }
}

#[derive(Resource, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Config {
    worker: Option<PathBuf>,
}

#[derive(Resource)]
struct Worker(Result<PathBuf, String>);

fn execute(called: On<ToolCalled<Program>>, mut commands: Commands) {
    let program = Program {
        code: called.args.code.clone(),
        inputs: called.args.inputs.clone(),
    };
    let (call, agent) = (called.call, called.agent);
    commands.queue(move |world: &mut World| start(world, call, agent, program));
}

/// Start an admitted open tool's program using this plugin's fixed runtime.
///
/// Language frontends may lower into this boundary. Trusted Rust callers supply
/// their current effect/agent; no tool authority or escalation is granted here.
pub fn start(world: &mut World, call: Entity, agent: Entity, program: Program) {
    let open = world.get::<Started>(call).is_some()
        && world.get::<Settled>(call).is_none()
        && world.get::<Outcome>(call).is_none()
        && world.get::<Running>(call).is_none()
        && world
            .get::<ForAgent>(call)
            .is_some_and(|owner| owner.0 == agent)
        && world
            .get::<Decision>(call)
            .is_some_and(|decision| !decision.is_decided());
    if !open {
        return;
    }
    let worker = match world.resource::<Worker>().0.clone() {
        Ok(worker) => worker,
        Err(error) => {
            world.entity_mut(call).insert(Outcome::from(failure(error)));
            return;
        }
    };
    let Some(runtime) = world.get_resource::<TokioRuntime>().cloned() else {
        world.entity_mut(call).insert(Outcome::from(failure(
            "Monty requires Rig's TokioRuntime service",
        )));
        return;
    };
    let wake = world.resource::<Wake>().clone();
    let host = host::Host::new(
        world.resource::<host::Requests>(),
        wake.clone(),
        call,
        agent,
    );
    let future = async move {
        // Aborting the Rig future must abort its Tokio work, not detach a worker.
        let mut task = ExecutionTask(runtime.0.spawn(async move {
            tokio::time::timeout(
                Duration::from_secs(300),
                runtime::run(worker, program, host),
            )
            .await
            .map_err(|_| "Monty exceeded its 300-second wall deadline".to_owned())?
        }));
        match (&mut task.0).await {
            Ok(Ok(output)) => match serde_json::to_value(output) {
                Ok(output) => ToolResult::success(ToolOutput::json(output)),
                Err(error) => failure(error.to_string()),
            },
            Ok(Err(error)) => failure(error),
            Err(error) => failure(format!("Monty task stopped: {error}")),
        }
    };
    world.entity_mut(call).insert(Running::answering(
        AsyncComputeTaskPool::get_or_init(TaskPool::default),
        &wake,
        future,
    ));
}

struct ExecutionTask(tokio::task::JoinHandle<Result<Output, String>>);

impl Drop for ExecutionTask {
    fn drop(&mut self) {
        self.0.abort();
    }
}

fn failure(message: impl Into<String>) -> ToolResult {
    let mut message = message.into();
    if message.len() > 65_536 {
        message.truncate(message.floor_char_boundary(65_536));
        message.push_str("\n[Monty diagnostic truncated]");
    }
    ToolResult::failed(ToolExecutionError::other(message))
}
