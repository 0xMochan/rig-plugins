# rig-command

An execution boundary for trusted Rig plugins. The native-shell tool owns model
arguments and the automatic-review plugin owns approval. This crate owns the
prepared command, OS launcher arguments, bounded output and process lifetime.
It does not silently retry or broaden permissions.

```rust,no_run
use std::path::Path;
use rig_command::{ExecutionMode, NativeExecutor};

# async fn example() -> Result<(), Box<dyn std::error::Error>> {
let executor = NativeExecutor::new("/absolute/path/to/codex")?;
let prepared = executor.prepare(
    Path::new("/absolute/workspace"),
    Path::new("."),
    "cargo check --offline".to_owned(),
    60,
)?;
// The caller captures this immutable value and ExecutionMode for review.
let result = executor.execute(prepared, ExecutionMode::Sandboxed).await?;
println!("{:?}: {}", result.status, result.output);
# Ok(())
# }
```

## Why use Codex as an external launcher?

The first version delegates OS policy enforcement to the installed
`codex-cli 0.142.0` executable. Reusing the actual sandbox implementation is more
defensible than starting another policy compiler from a subset of its Seatbelt
rules. An explicit absolute path and exact version are required. No model call,
Codex account or Codex subscription is needed for `codex sandbox`.

This is an intentional stepping stone: the external CLI must be available on
each machine. Its version check is compatibility checking, not executable
signature verification. Trusted host code could replace that executable or use
the unrestricted execution mode. This is not a sandbox around third-party Rust
plugins themselves.

The policy grants broad filesystem reads, writes inside the canonical workspace,
read-only `.git`, `.agents`, `.codex`, and `.aws` metadata, and no networking.
It grants no shared temporary-directory writes. `TMPDIR` is frozen to the
workspace, so compilers and `mktemp` can create temporary files where writes are
already allowed. macOS enforcement was exercised through Seatbelt. The Linux
adapter remains source-only here; other platforms are outside this milestone.

Each launch gets a temporary `CODEX_HOME` outside the workspace and an explicit
permission profile under a fresh 128-bit OS-random name. Its complete filesystem
and network policy has no parent or symbolic temporary grants. A stable profile
name could inherit extra grants through recursively merged system/project
configuration; a fresh namespace prevents static config from predefining it.
Other trusted user processes actively changing configuration during launch are
outside this boundary. The random name changes identity, never authority.
The workspace is encoded as a scoped permission table so legal path characters
such as brackets remain literal rather than being parsed as policy globs.

This matters in Codex 0.142.0: explicitly selecting `-P :workspace` ignores
legacy workspace exclusions and grants temporary writes. The first runtime probe
caught that behavior, and the adapter now supplies the actual policy rather than
relying on those exclusions. An empty environment table also would not erase
ambient variables, so the adapter fixes the filter and every allowed value. The
first process uses `Command::env_clear()` before its exec; a child `env -i`
wrapper would clear the environment too late to prevent loader injection.
`BASH_ENV`, `ENV`, loader injection and language initialization hooks are absent.

The environment includes captured `PATH`, `HOME`, user identity, locale, and a
small set of toolchain/cache locations, plus workspace-local `TMPDIR`. These are visible in the
review snapshot. This is not credential secrecy: an allowed filesystem read can
still print a credential file, and the unrestricted mode has the user's normal
filesystem and network authority. Review is required for the unrestricted mode.

## Ownership and execution

`prepare` takes ownership of the script because that same string must survive
review and asynchronous execution. Paths are canonical `PathBuf` values and the
environment is an ordered map for stable review evidence. Getters borrow values;
there is no shared mutable policy, deserializer or public field mutation.
Cloning a prepared value is useful only at the caller's review/execution boundary.

The mode is a separate reviewed input. `ExecutionMode::Escalated` is intentionally
constructible by trusted caller code; this library does not pretend that a Rust
enum proves user authorization. The native-shell plugin binds its permit to the
entire prepared input and mode before calling this API.

The command crosses the process boundary as arguments to a fixed Bash wrapper.
The wrapper receives the reviewed cwd and script as positional arguments, changes
directory, and starts `/bin/bash --noprofile --norc -c`. The script is never pasted
into launcher source. Codex's policy cwd remains the workspace even when the
command starts in a subdirectory.

`start` validates and spawns synchronously, allowing the caller to check and
consume its permission immediately before the first exec. It returns an owned
`RunningCommand`; `wait` receives that command's result. A dedicated worker starts
immediately, so deadlines apply even if an async executor delays polling `wait`.

The runner is a separate module because process ownership and pipe draining
are different concerns from command preparation and sandbox policy. Its worker
owns the child and nonblocking pipes. The result has a single-use channel;
output never enters a queue. Reads use fixed 8 KiB buffers, with a 128 KiB turn
limit per stream so a flood cannot starve the deadline or cancellation check.
It retains at most 64 KiB of output while discarding the rest immediately.

Dropping `RunningCommand` or its wait future sets cancellation; the worker checks
every 10 ms, kills the process group and reaps the leader. Cleanup also runs on
normal completion. `waitid(WNOWAIT)` observes exit without reaping, reserving the
leader's identifier until group cleanup completes; the runner never signals a
group after reaping its leader. This version uses immediate `SIGKILL`, including
on cancellation, rather than promising graceful shutdown. A 1..=3600 second
deadline is mandatory; the tool plugin may set a smaller bound. A failed exit is
reported as a status, never interpreted as permission to retry.

The reviewed script, environment and paths are frozen; filesystem contents are
not. Files and executables can change during review. Canonical paths are checked
again before launch, but this does not close every concurrent filesystem race.
This boundary assumes other trusted user processes are not actively replacing
the workspace or launcher. Processes that deliberately change process groups or
detach into another session can escape group cleanup. Their inherited pipes are
closed after a bounded final drain; they cannot keep the output worker alive.
This executor is for bounded foreground work; it is not a durable job manager
or an OS process containment layer.

Darwin can return `EPERM` when group signalling finds only an unreaped zombie;
the runner accepts that case only after observing leader exit. This was reproduced
with real child processes and matches [XNU's group iteration](https://github.com/apple-oss-distributions/xnu/blob/main/bsd/kern/kern_sig.c),
which excludes zombies. Privileged or otherwise unsignalable descendants remain
outside the same-user cleanup guarantee.

## Validation worth running

Tests check argument boundaries and explicit policy fields, flood both real
output pipes past their capacity, enforce a deadline, and drop an active wait to
verify the leader is reaped. They cannot prove OS containment. Run the real
containment probe from an ordinary terminal:

```sh
cargo run -p rig-command --example sandbox_probe -- /absolute/path/to/codex
```

It uses a bracket-named checkout and verifies allowed workspace writes and temporary-file creation, denied sibling,
shared-temporary and metadata writes, and denial of a locally reachable loopback
connection. It supplies hostile Codex config and shell initialization variables.
The probe first confirms
the listener is reachable outside the sandbox. A nested Codex sandbox may be
rejected by macOS before any child starts; that is a failed/unavailable sandbox,
not a passed containment test.

## Source comparison

The installed version is pinned to
[`rust-v0.142.0`, commit `3a76f3ac68c8949d1cac6ea769b6ec7b8953a415`](https://github.com/openai/codex/tree/3a76f3ac68c8949d1cac6ea769b6ec7b8953a415).
The relevant implementation is
[`cli/src/debug_sandbox.rs`](https://github.com/openai/codex/blob/3a76f3ac68c8949d1cac6ea769b6ec7b8953a415/codex-rs/cli/src/debug_sandbox.rs),
[`core/src/config/permissions.rs`](https://github.com/openai/codex/blob/3a76f3ac68c8949d1cac6ea769b6ec7b8953a415/codex-rs/core/src/config/permissions.rs),
and [`sandboxing/src/seatbelt.rs`](https://github.com/openai/codex/blob/3a76f3ac68c8949d1cac6ea769b6ec7b8953a415/codex-rs/sandboxing/src/seatbelt.rs).

The broader automatic-review study used newer upstream
[`edd0d898204eba8397181deae8e4a238d1f7df27`](https://github.com/openai/codex/tree/edd0d898204eba8397181deae8e4a238d1f7df27).
Useful boundaries there are the
[`guardian-reviewer` crate](https://github.com/openai/codex/tree/edd0d898204eba8397181deae8e4a238d1f7df27/codex-rs/ext/guardian-reviewer),
[`review_request.rs`](https://github.com/openai/codex/blob/edd0d898204eba8397181deae8e4a238d1f7df27/codex-rs/core/src/guardian/review_request.rs),
and [`tools/orchestrator.rs`](https://github.com/openai/codex/blob/edd0d898204eba8397181deae8e4a238d1f7df27/codex-rs/core/src/tools/orchestrator.rs).
They distinguish policy from mechanism, invalidate stale authorization, and
review the actual broader retry authority. Reviewer pooling, speculative scores,
approval caching and history compaction are deliberately outside this first
executor. No Codex source or prompts are copied into this crate.
