Run a focused xonsh subset on bounded Monty Python with separately guarded native commands. Choose the input mode instead of wrapping code in another interpreter.

For command-only work, use `mode: "commands"`: write bare newline-separated commands/pipelines, without `$()` or `$[]` wrappers. Each pipeline prints output and has its own native execution/review lifecycle. `inputs` supplies values for `@(...)` interpolation; Python statements do not belong in this mode.

```json
{"mode":"commands","code":"git status --short\ncargo test --locked -p rig-xonsh --lib"}
```

For Python mixed with commands, use `mode: "mixed"` (default). Write supported Python directly for calculations, parsing captured text, string/list transformations, variables, functions, loops and branching. Do not launch `python3 -c`, a Python heredoc or `sh -c` merely to wrap logic Monty can run itself. Use commands for real project/OS executables; external Python is justified only for CPython-only libraries or OS operations unavailable in Monty.

- `$(command)` captures text, preserving newlines. Process it directly in Python.
- `$[command]` prints output and returns None.
- Standalone `@(expression)` passes a string, Path, or list of strings/Paths as literal arguments, never executable shell source.
- Commands support literal argv, single/double quotes and complete `|` pipelines.

For example, Python owns the loop and text processing; only printf runs as a command:

```xonsh
names = ["pear", "a; b"]
values = []
for name in names:
    text = $(printf '%s' @(name))
    values.append(text.upper())
$[printf 'done\n']
values
```

Plain `p'...'` / `p"..."` literals create lexical Paths. Join with `/`, inspect `.name`/`.parent`, and pass through `@(...)` or as standalone path-literal command arguments. For example, `root = p"crates" / "rig-xonsh"` in mixed mode, or `rg --files p"crates/rig-xonsh"` in commands mode. Paths do not grant filesystem access. No `~`/`$ENV` expansion or combined raw/formatted path prefixes; filesystem methods remain blocked. Use available file tools or separately gated native commands for necessary OS work.

Permissions: ordinary native commands default to the OS sandbox. When broader authority is already known to be needed, request `execution.escalate=true` with a concrete `justification` on the FIRST invocation. For example, network access under the network-disabled sandbox can request review up front. Do not intentionally run a failing sandbox permission probe. Each native pipeline is frozen independently; each escalated pipeline is reviewed BEFORE execution. Approval is not guaranteed and never authorizes the outer Python program, future pipelines or retries. Do not elevate ordinary commands unnecessarily. A denial, failure, cancellation or interruption never authorizes an automatic retry, fallback or escalation.

`execution` also supplies native `cwd` and per-command `timeout_secs` (1..300). `backend` selects native (default, shared OS-sandboxed executor) or virtual (stateless virtual_bash with explicit files). Native means commands run on the host, not that Python becomes CPython. No public native_shell tool is required. Native capture combines stdout/stderr. Virtual files are resupplied to each form, separate from the native workspace; virtual rejects native execution options.

This is not official xonsh or Bash compatibility. Mixed mode still requires explicit command forms; commands mode is not automatic Python/shell disambiguation. No redirection, background jobs, environment/glob expansion, nested shell forms or f/template strings. Direct Python has no host filesystem access, user rc files, persistent shell or ambient environment integration. Denied, failed or truncated shell results raise RuntimeError. Source is at most 48 KiB; all Monty limits apply.
