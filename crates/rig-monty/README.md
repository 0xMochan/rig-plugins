# rig-monty

`MontyPlugin` adds `run_monty`, process-isolated Python code mode. Each invocation
owns a fresh Monty 1.4.0 worker. No host directories are mounted, no Python OS
handlers are installed, and no worker environment is inherited.

`MontyRuntimePlugin` provides just the worker runtime, host bridge and
cancellation for language frontends. It exposes no public tool. Xonsh installs
that runtime dependency without enabling `run_monty`; select `MontyPlugin`
explicitly if you want the standalone Python tool. Neither Monty plugin installs
native execution. Xonsh separately supplies its own typed native host capability;
that private operation is unavailable to standalone Monty programs.

```json
{"code":"r = tool('virtual_bash', {'script': 'printf 21'})\nassert r['status'] == 'success'\nint(r['data']['stdout']) * 2"}
```

The last expression becomes `value` in a JSON result with `stdout`, `stderr`, and
`output_truncated`. Optional `inputs` provides JSON globals. Only JSON values
cross the boundary: null, booleans, signed 64-bit integers, finite floats,
strings, lists, and dictionaries with string keys. Unsupported types, cycles,
excessive nesting and oversized structured values fail explicitly.

`tool(name, arguments)` returns `{status, data}`. Status is Rig's `success`,
`error`, `denied`, or `skipped`; data preserves structured JSON or text. The
underlying tool may have its own execution status: for `native_shell`, a
successful dispatch still requires checking `data.status` for command exit.
Nothing retries automatically. Recursive `run_monty`/`run_xonsh` calls and async
Python host futures are unavailable.

## Ownership and permission

Monty suspends on a host call. A bounded queue carries owned arguments into
the ECS world, where a normal `ToolRequest` is created for the same agent,
`ChildOf` the interpreter call and with its turn association. Rig's tool access,
gates, effect recording, redaction, successor handling and cancellation apply.
The settled result resumes Python. This follows Rig Steel's effect lifecycle
without importing its Scheme engine or flattening results to strings.

The open tool declares a write footprint because it can request any tool its
agent is allowed. A runtime is not a read-only capability merely because one
script appears harmless. A standalone Monty call to `tool('native_shell', ...)`
requires that public tool to be separately enabled and allowed; it uses the
shared native sandbox and one-use review permit. There is no fallback to
xonsh's private native operation and no approval belonging to the Python session.

The Tokio task owns the checkout; dropping Rig's task aborts it. Upstream's
kill-on-drop worker handle then requests termination. Normal completion finishes
the session and explicitly closes/reaps the idle worker. No snapshots, persistent
REPL, replay or durable native continuation are exposed in this version.

## Setup and bounds

Run `scripts/setup-monty` explicitly to install the official hash-pinned binary
wheel into `${RIG_HOME:-$HOME/.rig}/tools`. This requires `uv`; it does not build
or launch an agent. The installed worker is a compiled Rust executable, not
CPython. Its wheel package is `pydantic-monty-runtime==1.4.0`. Alternatively build
the matching `monty-runtime` crate. To expose `run_monty`, add the plugin to your
existing global `~/.rig/plugins.toml` (not needed when using only xonsh):

```toml
[[plugin]]
crate = "rig-monty"
path = "/absolute/path/to/rig-plugins/crates/rig-monty"
plugin = "rig_monty::MontyPlugin"
```

Run `rig plugin check` and `rig build`, then restart/reload Rig. An explicit worker
path can be configured in your Rig home's `settings.toml`:

```toml
[monty]
worker = "/absolute/path/to/trusted/monty"
```

The default is `RIG_HOME/tools/bin/monty`. Missing workers, wrong versions and
failed version checks return an explicit failure. Version checking is bounded
to two seconds/256 bytes; it proves compatibility, not executable integrity.
Loading arbitrary Rust plugins remains trusted code.

Host-owned limits are 64 KiB source and input JSON, 32 MiB worker allocations
plus Monty's baseline/headroom, five seconds of interpreter execution per feed,
128 recursion depth, 64 suspensions, and 300 seconds including host waits.
Protocol requests have a six-second backstop. Captured print output retains
64 KiB combined; final values, host arguments and replies permit 256 KiB JSON
and 32 nesting levels. Only printed output can truncate.

The 32 MiB bound is allocator accounting, not whole-process RSS. Monty 1.4.0
also has fixed protocol frame/decode budgets that cannot be reduced through
`PoolConfig`; retained-output limits are not transport-allocation limits.
Cancelled checkouts have no public confirmed-reap method in this release.
The real cancellation check observed worker exit locally; it does not strengthen
the upstream API guarantee. Interpreter isolation is separate from the OS
sandbox used by native tools.

## Focused verification

The normal suite checks lossless bounded JSON conversion. Real-worker checks
are explicit so a missing installation never silently passes them:

```sh
scripts/setup-monty
RIG_MONTY_WORKER="${RIG_HOME:-$HOME/.rig}/tools/bin/monty" cargo test --locked -p rig-monty --test runtime -- --ignored
```

They exercise actual tool effects with structured replies and denied gates/access,
file/memory/time/value/output boundaries, and cancellation of a held child plus
the actual worker. No credentials or provider calls are involved.

Pinned upstream source: Monty 1.4.0,
[`4b015dc4a1e08394b0a98e0255ed8c81de1fbbf6`](https://github.com/pydantic/monty/tree/4b015dc4a1e08394b0a98e0255ed8c81de1fbbf6).
