# rig-xonsh

`XonshPlugin` adds `run_xonsh`: bounded Python, lexical paths and guarded command
pipelines, with separate mixed-code and bare-command input modes.
It installs `MontyRuntimePlugin` and `NativeCommandRuntimePlugin`, reusing the
bounded worker and the shared native sandbox/review lifecycle. It does not enable
public `run_monty` or `native_shell` tools; their frontend plugins remain opt-in.
This is a focused xonsh subset, **not the official xonsh interpreter**. The
`native` backend runs commands on the host; Python still runs in bounded Monty.
Bare command blocks and plain `p'...'` paths are supported; automatic mixed
Python/shell disambiguation and full xonsh semantics are not.

## Bare command blocks

For command-only work, set `mode: "commands"` and write one pipeline per logical
line, without `$()` / `$[]` wrappers:

```json
{"mode":"commands","code":"git status --short\ncargo test --locked -p rig-xonsh --lib"}
```

Every pipeline prints output and follows the same independent native gate.
Blank/comment-only lines are allowed. Quotes and `@(...)` expressions can span
physical lines; the block is parsed completely before any host effect. Inline
comments are not supported. Keep a pipeline on one line or use an escaped
newline after `|`. Python statements are not interpreted in this mode. `inputs`
supplies interpolation values, and lexical paths can be arguments directly:

```json
{"mode":"commands","inputs":{"paths":["src","tests"]},"code":"printf '%s\\n' @(paths)\nprintf '%s' p'a; b'"}
```

`mode: "mixed"` is the default and preserves Python with explicit command forms.
Mode selection is explicit, never a runtime-error fallback or command-name guess.

## Write Python directly

Use Python directly in `code` for calculations, parsing captured output, string
and list processing, variables, functions, loops and branching. Mix it with
commands instead of wrapping supported Python logic in `python3 -c`, heredocs or
`sh -c`:

```xonsh
names = ["pear", "a; b"]
values = []
for name in names:
    text = $(printf '%s' @(name))
    values.append(text.upper())
$[printf 'done\n']
values
```

This returns `["PEAR", "A; B"]` and prints `done`. Python owns the loop and
transformations; only `printf` starts native commands. `@(...)` passes the
semicolon as literal argument data, not shell syntax. These are contents of the
`code` field, not a shell script to pass to another interpreter.

Direct Python cannot access the host filesystem. Use available file tools for
reading/editing files or separately gated native commands for necessary OS work.
An external Python command can be appropriate for CPython-only libraries or
filesystem operations unavailable in Monty; it is not needed for ordinary
computation. Lexical Path operations are available, but direct
`pathlib`/`shutil` filesystem operations do not gain host authority. Every native
effect retains the same sandbox/review boundary.

## Lexical path literals

Plain `p'...'` / `p"..."` literals (also uppercase `P` and triple quotes) create
Monty `pathlib.Path` values with ordinary Python string escape semantics:

```xonsh
root = p"crates" / "rig-xonsh"
text = $(printf '%s' @(root / "src"))
[str(root), root.name, str(root.parent), text]
```

Paths and lists of strings/Paths work through `@(...)`. A standalone path literal
also works directly in command argv, for example `$(printf '%s' p'a; b')`.
Conversion is lexical argument data, not an implicit filesystem operation or a
shell fragment. Path values in the final result still need explicit `str(...)`
because the worker returns JSON values.

This intentionally does **not** expand `~` or `$ENV`, load an environment, or
support combined `pr`/`pf` path prefixes. `.name`, `.parent` and `/` stay pure;
filesystem methods such as `.read_text()`, `.exists()` and `.glob()` remain
blocked by Monty's OS-call policy. Use explicit file tools or separately gated
native commands when host filesystem operations are needed.

The [model-visible usage guidance](src/usage.md) teaches the same distinction.
Changing that guidance does not add full xonsh syntax or CPython compatibility.

## Capturing output and passing arguments

```json
{"code":"items = $(printf '%s\\n' z a | sort)\nitems.splitlines()"}
```

```json
{"inputs":{"name":"a; b"},"code":"$(printf '%s\\n' @(name))"}
```

The second example returns captured text containing the literal argument `a; b`.
Interpolation never
concatenates Python values into executable shell source.

## Supported language

- `mode: "commands"` accepts bare newline-separated pipelines and prints output.
- `mode: "mixed"` (default) keeps Python with explicit command forms.
- `$(command)` returns captured text, preserving newlines.
- `$[command]` appends output to Monty's captured print streams and returns None.
- Commands contain literal words, single/double quoted literals, escapes and
  complete `|` pipelines. Quoted literals have no environment/glob expansion.
- Standalone `@(expression)` arguments accept a string, Path, or list of either.
  List values splice arguments; every argument is independently shell-quoted.
- Plain path literals are lexical values in Python and standalone argv words.
- Ordinary Python strings, raw/triple strings, comments, functions and loops
  remain Python. Forms inside strings/comments remain literal source.

Control operators, redirection/heredocs, background jobs, environment/glob/brace
expansion, macros, rich `!` forms, nested shell forms, mixed literal/interpolated
words and f/template strings are rejected. Bare commands require commands mode;
there is no automatic mixed-mode fallback to a shell. Quote special characters
when they are intended as literal arguments.
The complete source is lowered, then Monty compiles it before any host effect.
Runtime failures can follow already completed effects; execution is not a
transaction, and nothing is automatically rolled back or replayed.

## Backends and permissions

`backend` selects `native` (default) or `virtual` for every complete pipeline:

- Native submits a typed `NativeCommandRequest` to the shared command runtime,
  in its ordinary OS sandbox. No public `native_shell` tool or access to that
  tool is needed. Captured text includes **combined stdout/stderr**, unlike
  full xonsh's stdout-only capture.
- Virtual calls `virtual_bash`, requiring a separately selected `BashkitPlugin`
  and access to that tool. Its stdout and stderr remain separate. Optional
  `files` is a mapping of relative text paths to content, resupplied to each form.
  State created by one virtual call is discarded before the next.

Each form uses a fresh shell process/interpreter. Directory/environment changes
do not persist between forms. Python variables last for the outer invocation.
Native and virtual filesystems remain separate; native requests reject `files`.

Native shell forms expand argv as data and suspend on a dedicated private host
operation. The bridge creates a typed child command effect for the same agent
and turn, not a `native_shell` `ToolRequest` or a hidden-tool allowlist exception.
The concrete pipeline is shell-quoted, frozen and gated before execution; the
shared runtime owns process lifetime and permission revalidation. The private
native operation is available to the xonsh frontend, not standalone Monty.
Virtual forms continue to use ordinary `virtual_bash` child tool effects.

A refused/failed command result, nonzero command exit or truncated shell output
raises RuntimeError rather than returning a misleading successful string.
Cancellation stops owned work; it does not authorize a retry. There is no backend
fallback, automatic retry or blanket approval for the outer program.
Use the typed `execution` options below instead of wrapping native work in
`tool('native_shell', ...)`.

Inputs named `tool` or beginning `__rig_xonsh_` are reserved. Source is at most
48 KiB; generated Python and all inputs must fit Monty's limits. Each native
pipeline must also fit the runtime's 65,536-byte command limit after quoting.
Unsupported types, invalid interpolation and NUL arguments fail explicitly.

## Execution options

For native shell forms, optional `execution` supplies typed defaults independently
to every pipeline in this invocation:

| Field | Behavior |
| --- | --- |
| `cwd` | Optional directory within the active workspace; otherwise use the agent's workdir |
| `timeout_secs` | Optional per-command deadline, 1–300 seconds; otherwise use the native runtime's configured default |
| `escalate` | Defaults to `false`; `true` requests independent review outside the OS sandbox |
| `justification` | Required and nonempty when escalating, at most 4,096 bytes; an agent explanation is not user authorization |

For example, run project checks directly:

```json
{"mode":"commands","code":"cargo test --locked -p rig-xonsh --lib","execution":{"cwd":".","timeout_secs":120}}
```

When an action is already known to need broader authority, request it on the
**first invocation**, rather than deliberately failing in the sandbox first:

```json
{"mode":"commands","code":"cargo fetch --locked","execution":{"escalate":true,"justification":"Fetch the locked dependencies from the configured registries; this requires network access."}}
```

This requests review before the first process start. Ordinary commands still
default to the sandbox; do not elevate them unnecessarily. A declaration of need
is not permission, and a failure or denial never authorizes automatic retries.

With `AutoReviewPlugin` enabled, **each exact expanded pipeline** receives its
own review and one-use permit. Options shared by the invocation are not a permit
for arbitrary later commands, and a failed sandboxed action never triggers an
automatic retry or elevation. Missing/failed review, stale authorization,
cancellation and timeout refuse broader execution. A native pipeline without
review can still use the ordinary sandbox; escalation without a reviewer fails
closed. [Review policy and limits](../rig-auto-review).

Do not supply native `execution` options with `backend: "virtual"`; they are
rejected rather than ignored. Virtual files and native workspaces remain separate.
The native per-command deadline does not remove Monty's outer resource and
wall-clock limits.

## Setup and verification

Install the matching worker with `scripts/setup-monty` or configure it as
described in [rig-monty](../rig-monty/README.md). No xonsh/CPython installation is
needed. Add xonsh to your existing global `~/.rig/plugins.toml`:

```toml
[[plugin]]
crate = "rig-xonsh"
path = "/absolute/path/to/rig-plugins/crates/rig-xonsh"
plugin = "rig_xonsh::XonshPlugin"
```

Run `rig plugin check` and `rig build`, then restart/reload Rig. The normal suite
checks lexical boundaries; explicit real-worker checks exercise Python loops,
pipelines, quote-safe interpolation, capture/None behavior, and zero host
requests for rejected source. The mixed Python/command example in the
model-visible guidance is executed verbatim against the real worker/native
command bridge, checking its returned values, printed output and command count.
Checks also exercise native/virtual bare blocks, lexical Path argv, continued
filesystem refusal and malformed later source starting zero host effects. The
upfront-elevation test holds a scripted reviewer before the first execution,
then verifies one approved write and zero preliminary sandbox attempts.
Tool denial never becomes capture success or a retry:

```sh
RIG_MONTY_WORKER="${RIG_HOME:-$HOME/.rig}/tools/bin/monty" cargo test --locked -p rig-xonsh --test runtime -- --ignored
```

The Rust xonsh parser was studied at
[`219aa5f40e36d3871734b381a1c61bb7434ad0a0`](https://github.com/jnoortheen/xonsh-rd-parser/tree/219aa5f40e36d3871734b381a1c61bb7434ad0a0).
Its root requires nightly Cargo and exposes a PyO3 binding; the internal parser
lowers into Ruff's full Python AST. This milestone uses a small explicit lexical
frontend on the pinned stable toolchain and leaves Python parsing to Monty.
It copies no parser code and makes no full xonsh compatibility claim.
