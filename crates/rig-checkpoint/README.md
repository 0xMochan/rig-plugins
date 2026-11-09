# rig-checkpoint

Full delivered-conversation checkpoints for Rig 0.46.0 / Bevy 0.20.0.
This crate adds context inheritance to Rig's public `SpawnSubagent` effect.
It reuses Rig's task/message/wait tools and defines no new tool, process
launcher, or permission policy.

```rust
use rig_basics::subagents::SpawnSubagent;
use rig_checkpoint::{Checkpoint, CheckpointPlugin};
use rig_harness::prelude::*;

fn investigate(parent: Entity, commands: &mut Commands) {
    commands.spawn((
        SpawnSubagent::task("Inspect the parser", "Find the cause and report the evidence."),
        Checkpoint,
        ForAgent(parent),
    ));
}

let mut app = App::new();
app.add_plugins((AgentPlugin, CheckpointPlugin));
```

Add `Checkpoint` before the spawn reaches Gate. The plugin captures the parent's
current conversation there, retains message ids/authors and shared immutable
payloads, and installs the copy before Rig sends the child's task brief. Local
prompt sections and the effective `CacheKey` are inherited. Global prompt
sections stay live. `CheckpointContext` saves provenance and local sections so
the same prompt can be reconstructed after restoring a session.

Rig owns settings inheritance, connection sharing, tool restrictions, workdir,
depth limits, reports, follow-ups, and cancellation through `SpawnedBy`.
This plugin copies none of the parent's active effect entities, processes,
pending requests, or approval permits. Native execution remains subject to the
child's ordinary gates and the live root conversation's authorization evidence.

Unresolved parent calls receive explicit checkpoint placeholders in the child's
copy. They say the outcome is unknown and do not schedule or retry an action.
Invalid tool history refuses the spawn; it never silently falls back to a fresh
agent. The current user request is kept even when its turn is unfinished.

Only task agents with inherited system prompt/environment are supported. Model,
effort, and tool overrides still go through Rig. The parent and child share a
workspace, not a filesystem snapshot. A checkpoint includes any existing
compaction; it cannot recover messages already removed from the live history.
Cache eligibility is preserved where possible, but provider hits, identical
decisions, cost savings, and latency improvements are not guaranteed.

Use [rig-checkpoint-tasks](../rig-checkpoint-tasks) for the `task` adapter.
