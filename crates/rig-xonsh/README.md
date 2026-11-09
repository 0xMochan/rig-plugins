# rig-xonsh

`XonshPlugin` adds `run_xonsh`: Monty-supported Python with explicit shell forms.
It installs `MontyPlugin` when needed and reuses its worker, bounds, typed values,
host-effect bridge and cancellation. This is a focused xonsh subset.

```json
{"code":"items = $(printf '%s\\n' z a | sort)\nitems.splitlines()"}
```

```json
{"inputs":{"name":"a; b"},"code":"$(printf '%s\\n' @(name))"}
```

The second example prints the literal argument `a; b`. Interpolation never
concatenates Python values into executable shell source.

## Supported language

- `$(command)` returns captured text, preserving newlines.
- `$[command]` appends output to Monty's captured print streams and returns None.
- Commands contain literal words, single/double quoted literals, escapes and
  complete `|` pipelines. Quoted literals have no environment/glob expansion.
- Standalone `@(expression)` arguments accept a string or list of strings.
  List values splice arguments; every argument is independently shell-quoted.
- Ordinary Python strings, raw/triple strings, comments, functions and loops
  remain Python. Forms inside strings/comments remain literal source.

Control operators, redirection/heredocs, background jobs, environment/glob/brace
expansion, macros, rich `!` forms, nested shell forms, mixed literal/interpolated
words and f/template strings are rejected. Bare commands do not fall back to a
shell. Quote special characters when they are intended as literal arguments.
The complete source is lowered, then Monty compiles it before any host effect.
Runtime failures can follow already completed effects; execution is not a
transaction, and nothing is automatically rolled back or replayed.

## Backends and permissions

`backend` selects `native` (default) or `virtual` for every complete pipeline:

- Native calls the existing `native_shell` tool in its ordinary OS sandbox.
  Captured text currently includes **combined stdout/stderr**, matching that
  tool's existing contract. This differs from full xonsh's stdout-only capture.
- Virtual calls `virtual_bash`. Its stdout and stderr remain separate. Optional
  `files` is a mapping of relative text paths to content, resupplied to each form.
  State created by one virtual call is discarded before the next.

Each form uses a fresh shell process/interpreter. Directory/environment changes
do not persist between forms. Python variables last for the outer invocation.
Native and virtual filesystems remain separate; native requests reject `files`.

The helper quotes the entire argv pipeline, enables pipefail, and creates one
ordinary child tool effect. A denied/skipped/error tool result, nonzero command
exit, or truncated shell output raises RuntimeError rather than returning a
misleading successful string. There is no backend fallback or outer-script
escalation. A separate explicit `tool('native_shell', {...})` request can ask the
existing reviewer for that exact action's permission; the language adds none.

Inputs named `tool` or beginning `__rig_xonsh_` are reserved. Source is at most
48 KiB; generated Python and all inputs must fit Monty's limits. Unsupported
types, invalid interpolation and NUL arguments fail explicitly.

## Setup and verification

Use `./agent setup` for the curated profile, or configure the matching worker as
described in [rig-monty](../rig-monty/README.md). No xonsh/CPython installation is
needed. The normal suite checks lexical boundaries; explicit real-worker checks
exercise Python loops, pipelines, quote-safe interpolation, capture/None behavior,
and zero host requests for rejected source. Tool denial never becomes capture
success or a retry:

```sh
RIG_MONTY_WORKER="$PWD/profile/tools/bin/monty" cargo test --locked -p rig-xonsh --test runtime -- --ignored
```

The composed profile smoke also checks actual native pipelines and a hostile
argument containing quotes/semicolons, confirming it creates no injected file.

The Rust xonsh parser was studied at
[`219aa5f40e36d3871734b381a1c61bb7434ad0a0`](https://github.com/jnoortheen/xonsh-rd-parser/tree/219aa5f40e36d3871734b381a1c61bb7434ad0a0).
Its root requires nightly Cargo and exposes a PyO3 binding; the internal parser
lowers into Ruff's full Python AST. This milestone uses a small explicit lexical
frontend on the pinned stable toolchain and leaves Python parsing to Monty.
It copies no parser code and makes no full xonsh compatibility claim.
