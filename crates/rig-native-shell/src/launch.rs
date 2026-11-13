//! Final authorization handoff and ownership of the native process.

use bevy_ecs::system::SystemState;
use bevy_tasks::{AsyncComputeTaskPool, TaskPool};
use rig_command::ExecutionMode;
use rig_harness::prelude::*;
use rig_harness::rig_core::tool::{ToolExecutionError, ToolOutput, ToolResult};

use crate::{Authorization, Executor, NativeCommandRequest, PendingCommand, ReviewPermit, refuse};

#[derive(Component)]
struct Launched;

pub(crate) fn start(world: &mut World, call: Entity, agent: Entity) {
    let may_start = world.get::<Started>(call).is_some()
        && world.get::<Outcome>(call).is_none()
        && world.get::<Settled>(call).is_none()
        && world.get::<Launched>(call).is_none()
        && world
            .get::<Decision>(call)
            .is_some_and(|decision| !decision.is_decided());
    if !may_start {
        return;
    }
    let prepared = match prepare_launch(world, call, agent) {
        Ok(command) => command,
        Err(error) => {
            deny(world, call, error);
            return;
        }
    };
    if prepared.is_escalation() {
        // Taking, rather than cloning, makes this effect's permission single use.
        if world.entity_mut(call).take::<ReviewPermit>().is_none() {
            deny(world, call, "The escalation permit was already consumed.");
            return;
        }
    }
    world.entity_mut(call).insert(Launched);
    let executor = world
        .resource::<Executor>()
        .0
        .as_ref()
        .cloned()
        .map_err(Clone::clone);
    let executor = match executor {
        Ok(executor) => executor,
        Err(error) => {
            deny(world, call, error);
            return;
        }
    };
    let mode = if prepared.is_escalation() {
        ExecutionMode::Escalated
    } else {
        ExecutionMode::Sandboxed
    };
    // No await or scheduler handoff occurs between validation, permit consumption
    // and spawning. Once spawned, cancellation owns this process through its guard.
    let running = match executor.start(prepared.into_command(), mode) {
        Ok(running) => running,
        Err(error) => {
            deny(world, call, error.to_string());
            return;
        }
    };
    let wake = world.resource::<Wake>().clone();
    let task = async move {
        match running.wait().await {
            Ok(output) => match serde_json::to_value(output) {
                Ok(output) => ToolResult::success(ToolOutput::json(output)),
                Err(error) => failure(error.to_string()),
            },
            Err(error) => failure(error.to_string()),
        }
    };
    world.entity_mut(call).insert(Running::answering(
        AsyncComputeTaskPool::get_or_init(TaskPool::default),
        &wake,
        task,
    ));
}

fn prepare_launch(
    world: &mut World,
    call: Entity,
    agent: Entity,
) -> Result<PendingCommand, String> {
    let command = world
        .get::<PendingCommand>(call)
        .cloned()
        .ok_or("The command has no prepared execution snapshot.")?;
    if world
        .get::<ForAgent>(call)
        .is_none_or(|owner| owner.0 != agent)
        || world.get::<Agent>(agent).is_none()
        || world.get::<AgentId>(agent).is_none()
    {
        return Err("The native command's owning agent changed before launch.".to_owned());
    }
    rig_workspace::validate_workdir(world, agent).map_err(str::to_owned)?;
    let native = world.get::<NativeCommandRequest>(call);
    if native.is_some_and(|request| !crate::request::origin_is_current(world, request, agent)) {
        return Err("The native xonsh parent capability changed before launch.".to_owned());
    }
    let arguments = crate::command::arguments(world.get::<ToolRequest>(call), native)?
        .ok_or("The native command request is no longer available.")?;
    let base = world
        .get::<WorkDir>(agent)
        .map(|work| &work.0)
        .unwrap_or(&world.resource::<ProjectRoot>().0);
    if world
        .get::<rig_workspace::WorkDirScope>(agent)
        .is_some_and(|scope| {
            world
                .get::<AgentId>(agent)
                .is_none_or(|id| !scope.allows(id, base))
        })
    {
        return Err("The native command's worktree scope is no longer current.".to_owned());
    }
    let xonsh_parent = native.and_then(NativeCommandRequest::xonsh_parent);
    if !command.is_call_for(call, agent) || !command.matches(arguments, base, xonsh_parent) {
        return Err("The prepared command changed before launch.".to_owned());
    }
    if command.is_escalation() {
        let evidence = world
            .get::<ReviewPermit>(call)
            .and_then(|permit| permit.evidence_for(&command))
            .cloned()
            .ok_or("Escalation has no approval for this exact effect.")?;
        let mut authorization = SystemState::<Authorization>::new(world);
        let current = authorization
            .get(world)
            .map_err(|error| error.to_string())?
            .capture(agent, evidence.max_bytes())
            .map_err(|error| error.to_string())?;
        if current != evidence {
            return Err("User authorization changed before launch.".to_owned());
        }
    }
    Ok(command)
}

fn deny(world: &mut World, call: Entity, error: impl Into<String>) {
    if let Some(mut decision) = world.get_mut::<Decision>(call) {
        refuse(&mut decision, error);
    }
}

fn failure(message: String) -> ToolResult {
    ToolResult::failed(ToolExecutionError::other(message))
}
