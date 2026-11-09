# rig-native-shell

`NativeShellPlugin` adds `native_shell`. The tool prepares an immutable command
at Gate, then runs that snapshot through `rig-command`. Its arguments are
`script`, optional `cwd` and `timeout_secs`, and optional `escalate` with a
required `justification`. The default is sandboxed native execution. A failure
never retries automatically.

A completed dispatch returns structured JSON with `output`, `status`, and
`truncated`. A nonzero command exit is represented in `status`; it is distinct
from a tool refusal or a failure to launch. Code-mode consumers receive this
declared JSON value directly.

Explicit escalation is refused unless another trusted plugin issues a permit
for this exact command and its root user's evidence. `rig-auto-review` provides
that policy. New user input, changed command arguments, changed agent directory,
missing authorization, and missing reviewer all prevent broader execution.
The evidence also binds the global `project_instructions` prompt section loaded
by Rig's `AgentsMdPlugin`; these instructions restrict authority rather than grant
it. Skills and other actor prompt sections are not copied into the reviewer.

Automatic escalation is refused after conversation compaction: older original
user messages are no longer available in the active conversation, and a generated
summary cannot replace them as authorization. Sandboxed commands still work.
Recovering complete original authorization from saved history is deferred;
adding another user message alone does not remove this refusal. A fresh thread
or generated handoff must not be used to shed earlier restrictions.

Evidence capture exposes a numeric `EvidenceSize` via `Authorization::measure`
and includes complete required bytes/counts in `AuthorizationError::TooLarge`.
Measurement shares capture's ancestry, provenance, pending-input and compaction
checks; it never issues permission. Capture checks the full text size before
copying it into the owned snapshot. The bound counts authorization/restriction
text, not the entire serialized request. If `rig-auto-review` is installed, its
read-only `auto_review_status` tool measures the running configuration without
retrying an action. [Capacity and legitimate recovery](../rig-auto-review/README.md#long-session-capacity-and-recovery).

User settings, never project settings:

```toml
[native_shell]
# Optional absolute path; otherwise prefer RIG_HOME/tools/bin/codex-sandbox,
# then resolve codex from PATH once at startup.
# codex = "/absolute/path/to/codex"
timeout_secs = 60
```

The executor checks its pinned Codex version with a cleared environment. Use
a trusted native executable, not an npm wrapper requiring Node from PATH.
Failed validation includes the executable path, exit status, stdout and stderr.
`NativeShellStatus` exposes this startup result as diagnostic metadata for
composition tools; it is not permission or evidence of OS enforcement.

This plugin governs its own tool;
it does not sandbox unrelated tools, file access, or arbitrary other plugins.
