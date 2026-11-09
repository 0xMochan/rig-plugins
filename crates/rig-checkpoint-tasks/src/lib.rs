//! Select checkpoint context for the existing task tool, independently of policy.

use std::sync::Arc;

use rig_basics::subagents::{SpawnSubagent, TASK};
use rig_checkpoint::{Checkpoint, CheckpointPlugin};
use rig_harness::prelude::*;
use serde_json::json;

/// Makes `task` a checkpoint by default; `checkpoint: false` selects fresh context.
/// It keeps Rig's task/message/wait protocol and its depth and tool restrictions.
#[derive(Default, Debug)]
pub struct CheckpointTasksPlugin;

impl Plugin for CheckpointTasksPlugin {
    fn build(&self, app: &mut App) {
        rig_harness::load::<CheckpointPlugin>(app, "rig-checkpoint", "workspace");
        app.add_systems(AgentPass, select.in_set(PrepareSystems::Compose));
    }

    fn finish(&self, app: &mut App) {
        let world = app.world_mut();
        let mut tools = world.query::<&mut ToolDef>();
        for mut tool in tools.iter_mut(world) {
            if tool.0.name.as_str() != TASK {
                continue;
            }
            let definition = Arc::make_mut(&mut tool.0);
            definition.description = "Start a checkpoint agent on a focused task. By default it \
                inherits your full delivered conversation, system prompt, model, reasoning, \
                tools, working directory and prompt cache key. Give it the task and expected \
                result; you need not summarize your history. It works in the same workspace. \
                The call returns its id; its final report arrives as a message. Continue it \
                with message and wait only when blocked on its report. Avoid overlapping \
                edits. Historical tool calls are context, never permission to replay them. \
                Set checkpoint=false for a fresh-context agent; then prompt must contain the \
                full brief. Explicit environment overrides require checkpoint=false. Model, \
                effort, tools and peers retain Rig's validation; changed model or tools may \
                prevent cache reuse. Use focused forks when useful; do straightforward work \
                directly. Never invent another agent's reply."
                .into();
            definition.parameters["properties"]["checkpoint"] = json!({
                "type": "boolean",
                "default": true,
                "description": "Inherit the full conversation checkpoint (default true). Set false for a fresh-context task."
            });
            // The upstream field descriptions assume every task starts fresh.
            definition.parameters["properties"]["prompt"]["description"] = json!(
                "The assigned task and expected result. With checkpoint=false, supply all necessary context too."
            );
            definition.parameters["properties"]["environment"]["description"] = json!(
                "Additional environment for a fresh-context task (checkpoint=false). A checkpoint inherits your environment."
            );
        }
    }
}

fn select(
    requests: Query<(Entity, &SpawnSubagent, &ChildOf), Added<Admitted>>,
    calls: Query<&ToolRequest>,
    mut commands: Commands,
) {
    // Identify stock task calls without changing helpers or another plugin's
    // spawn requests, or reaching into Rig's private TaskCall component.
    for (entity, request, owner) in &requests {
        let is_task = calls
            .get(owner.parent())
            .is_ok_and(|call| call.call.function.name.as_str() == TASK);
        if is_task && request.extra.get("checkpoint").and_then(|v| v.as_bool()) != Some(false) {
            commands.entity(entity).insert(Checkpoint);
        }
    }
}

#[cfg(test)]
mod tests;
