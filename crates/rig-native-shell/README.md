# rig-native-shell

A shared native command runtime with an optional shell tool frontend:

- `NativeCommandRuntimePlugin` freezes typed command requests at Gate and runs
  them through `rig-command`. It owns sandbox startup, process lifetime and
  exact-command permission revalidation; it exposes no model tool.
- `NativeShellPlugin` adds the public `native_shell` tool and installs the same
  runtime. Its arguments are `script`, optional `cwd` and `timeout_secs`, and
  optional `escalate` with a required `justification`.

Xonsh installs the runtime directly. Its native shell forms submit typed
`NativeCommandRequest` child effects, so they work without exposing or calling
`native_shell`. The default remains sandboxed native execution. A failure never
retries automatically, and no permit belongs to an outer interpreter session.

To expose the optional shell tool, add it explicitly to your global
`~/.rig/plugins.toml`:

```toml
[[plugin]]
crate = "rig-native-shell"
path = "/absolute/path/to/rig-plugins/crates/rig-native-shell"
plugin = "rig_native_shell::NativeShellPlugin"
```

Run `rig plugin check` and `rig build`, then restart/reload Rig. Omitting this
public frontend does not disable xonsh's native runtime dependency. Auto-review
installs neither the runtime nor this frontend.

A completed native command returns structured JSON with `output`, `status`, and
`truncated`. A nonzero command exit is represented in `status`; it is distinct
from refusal or a failure to launch. The optional tool exposes that JSON
directly. Xonsh checks it before returning captured text or printed output.

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
`NativeShellStatus` exposes the runtime's startup result as diagnostic metadata
for `rig-status` and other inspectors; it is not permission or evidence of OS
enforcement. `agent_status.native_execution` reports that backend status object,
while `native_execution_validation` describes startup validation only.
`native_shell_tool_registered` is a separate boolean for public tool registration;
it is not an allowlist grant. The retained `[native_shell]` settings configure the
shared runtime even when the optional public shell tool is absent.

This runtime governs its typed commands and the optional shell frontend. It does
not sandbox unrelated tools, file access, or arbitrary other plugins. Standalone
Monty does not acquire the xonsh-specific native host capability. Delta's bounded
legacy tools still require the public `native_shell` tool; they have not migrated
to typed native requests.
