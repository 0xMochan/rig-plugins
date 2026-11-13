# rig-checkpoint-tasks

Makes Rig's existing `task` tool fork full conversation context by default.
`message`, `wait`, background reports, nesting limits and the normal permission
gates remain Rig's. This crate depends on the independently usable
[checkpoint mechanism](../rig-checkpoint).

```toml
[[plugin]]
crate = "rig-checkpoint-tasks"
path = "/absolute/path/to/rig-plugins/crates/rig-checkpoint-tasks"
plugin = "rig_checkpoint_tasks::CheckpointTasksPlugin"
```

Add this entry to your existing global `~/.rig/plugins.toml`, then run
`rig plugin check` and `rig build`. Restart/reload Rig to apply the new build.

```json
{"description":"Inspect parser failure","prompt":"Find the E_417 cause and return concrete evidence."}
```

The child sees the delivered history, including earlier user constraints and
exact tool outputs. The prompt only needs to assign its task. It reports its
final answer to the parent; its intermediate work stays in its own conversation.

For an intentionally independent context, opt out:

```json
{"description":"Independent review","prompt":"Complete self-contained review brief...","checkpoint":false}
```

`environment` overrides require `checkpoint:false`. Explicit model/effort/tools
and `peers` retain Rig's behavior; a changed model or tool catalog can prevent
cache reuse. No extra delegation tool bypasses Rig's depth limit. This plugin
only selects checkpoints for actual `task` calls, leaving plugin-owned helpers
and other programmatic spawn effects alone.

It uses the release-pinned `task` schema's extension map and public spawn
components. See [the checkpoint crate](../rig-checkpoint) for ownership and limits.
