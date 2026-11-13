# Rig plugins

Independently usable plugins for an existing [Rig](https://github.com/0xPlaygrounds/rig)
installation. This repository is a Rust workspace, not a separate coding agent:
choose plugins in your own `~/.rig/plugins.toml` and keep using your existing
settings, login, models and sessions.

The workspace pins Rig **0.46.0**, matching Bevy **0.20.0**, and Rust **1.98.1**.
Each crate owns one capability; there is no all-in-one agent composition plugin.

| Crate | Responsibility |
| --- | --- |
| [rig-workspace](crates/rig-workspace) | Explicit work-directory defaults and canonical project/joined-root confinement; no tools or plugin bundle |
| [rig-status](crates/rig-status) | Read-only `agent_status` inventory and startup diagnostics; no execution dependency installation |
| [rig-bashkit](crates/rig-bashkit) | Stateless `virtual_bash` over explicit text inputs; no host capabilities |
| [rig-command](crates/rig-command) | Library for frozen native commands, OS sandboxing, deadlines, output and process ownership |
| [rig-native-shell](crates/rig-native-shell) | Shared native command runtime, exact per-command permission binding, and an opt-in `native_shell` tool |
| [rig-auto-review](crates/rig-auto-review) | Tool-free model review of explicit escalations; failure denies permission |
| [rig-monty](crates/rig-monty) | Bounded Python in fresh workers; structured, gated child tool calls |
| [rig-xonsh](crates/rig-xonsh) | Bounded Python, bare command blocks, lexical paths and argument-safe pipelines |
| [rig-tui-scroll](crates/rig-tui-scroll) | Mouse/trackpad transcript scrolling through Rig's native scroll actions |
| [rig-tui-history](crates/rig-tui-history) | Prefix-filtered prompt history and gray suggestions accepted with Right |
| [rig-tui-tools](crates/rig-tui-tools) | Foldable previews for every tool box, quiet headers and highlighted scripts |
| [rig-tui-loading](crates/rig-tui-loading) | Thinking line above the composer; the real welcome logo pulses in view |
| [rig-tui-games](crates/rig-tui-games) | `/tetris`, `/sudoku`, `/2048` and `/dino` in a popup or right-side terminal panel |
| [rig-checkpoint](crates/rig-checkpoint) | Full conversation checkpoints, local prompt inheritance and shared cache identity |
| [rig-checkpoint-tasks](crates/rig-checkpoint-tasks) | Checkpoint context by default for `task`; explicit fresh-context opt-out |
| [rig-conversation](crates/rig-conversation) | Natural progress updates between tool calls, without changing execution or review policy |
| [rig-model-catelog](crates/rig-model-catelog) | Day-one model definitions across supported providers in the shared ACP/terminal catalog; no provider or authentication changes |
| [rig-delta](crates/rig-delta) | Native live sharing/join, owned Delta watches/file mounts, fenced recording and bounded CLI tools |

## Install selected plugins

Keep your existing global `~/.rig/plugins.toml`. Add only the entries you want,
using an absolute path to this checkout. For example, alongside Rig's terminal
frontend:

```toml
[[plugin]]
crate = "rig-tui-tools"
path = "/absolute/path/to/rig-plugins/crates/rig-tui-tools"
plugin = "rig_tui_tools::TuiToolsPlugin"
```

Each crate's README describes its plugin type, dependencies, settings and limits.
Configuration stays in your Rig home's `settings.toml`; the repository does not
supply another home, launcher, login or agent settings file. Then validate and
build your existing agent:

```sh
rig plugin check
rig build
```

Restart or use Rig's native `/reload` workflow to load the new build. Source edits
do not change an already-running binary. If installing Rig for the first time,
use `cargo install rig --version 0.46.0 --locked`. Do not mix the pinned registry
APIs with an ambient development checkout through `RIG_SOURCE`.

### Workspace policy and diagnostics

For a coding agent with file tools, select the workspace guard explicitly:

```toml
[[plugin]]
crate = "rig-workspace"
path = "/absolute/path/to/rig-plugins/crates/rig-workspace"
plugin = "rig_workspace::WorkspacePlugin"

[[plugin]]
crate = "rig-status"
path = "/absolute/path/to/rig-plugins/crates/rig-status"
plugin = "rig_status::StatusPlugin"
```

`WorkspacePlugin` supplies root `WorkDir` defaults and confines tool workdirs to
canonical project roots or explicitly approved joined worktrees. **This guard is
required for the file-tool confinement described here:** stock Rig without a
`WorkDir` falls back to an unconfined `ProjectRoot`. The guard adds no tools and
does not enable execution, review or Delta. It is distinct from native OS
sandboxing, whose broad-read limitations are described below.

The optional `StatusPlugin` exposes `agent_status`: harness-registered plugins,
allowed tools, workdir and native-runtime startup diagnostics. It works without
the public `native_shell` tool and starts no process. Startup compatibility is
not proof of OS enforcement. `native_execution.state` reports the backend;
`native_execution_validation` describes startup validation only.
`native_shell_tool_registered` separately reports whether the optional public
tool exists, not whether the calling agent may use it; check `allowed_tools`
for that. An absent native runtime is reported as disabled.

### Execution dependencies

Select execution capabilities separately from permission policy:

- `rig-xonsh` installs `MontyRuntimePlugin` and `NativeCommandRuntimePlugin`,
  exposing `run_xonsh` without enabling public `run_monty` or `native_shell`
  tools. Native execution is the default. Each pipeline becomes a typed native
  command effect, not a call through `tool('native_shell', ...)`.
- `MontyPlugin` and `NativeShellPlugin` are optional public frontends. Select them
  separately only if you want `run_monty` or `native_shell`. Turning off the
  public native-shell tool does not turn off xonsh's native runtime; selecting
  xonsh explicitly selects that backend dependency.
- Xonsh's explicit `backend: "virtual"` requires `BashkitPlugin` and access to
  `virtual_bash`. Virtual and native filesystems are different; neither falls
  back to the other after failure. Virtual requests reject native execution
  options.
- `AutoReviewPlugin` reviews explicit escalations of frozen native commands from
  either frontend. It does **not** install a runtime or enable a tool. Without a
  reviewer, sandboxed commands can run but escalation is refused.
- Delta's bounded legacy `delta`/`delta_sync` tools still require the separately
  enabled public `native_shell` tool and access to it. They have not been migrated
  to xonsh's typed native-command path.
- `rig-command` is a library, not a plugin to list in `plugins.toml`.

Use bare command blocks for command-only work, or mixed Python with explicit
shell forms. Both modes use the same guarded runtime:

```json
{"mode":"commands","code":"cargo test --locked -p rig-xonsh --lib","execution":{"cwd":".","timeout_secs":120}}
```

Plain `p'...'` paths support lexical joins and argument passing, not host
filesystem access or environment/home expansion. Automatic mixed Python/shell
disambiguation, persistent shells and user rc loading remain unsupported.

`execution` supplies defaults independently to every native pipeline in that
invocation. An explicit `escalate: true` with a justification requests separate
review of **each exact expanded command**; it is not approval for the outer
Python program or permission inherited by future commands. Request elevation on
the first invocation when broader authority is already known to be required;
there is no mandatory failed sandbox probe. No failure retries or escalates
automatically. See [xonsh's execution options](crates/rig-xonsh#execution-options).

Native execution uses a trusted **codex-cli 0.142.0** executable as its OS sandbox
launcher. It does not run another coding agent or use Codex account credentials.
Obtain that version from its [official release](https://github.com/openai/codex/releases/tag/rust-v0.142.0).
Monty uses the compiled **1.4.0** Rust worker, not CPython.

Install only the optional runtimes you need, explicitly:

```sh
scripts/setup-monty
scripts/setup-codex-sandbox
```

These scripts install into `${RIG_HOME:-$HOME/.rig}/tools`; they do not build or
launch an agent. Monty setup requires `uv` and uses hash-pinned binary wheels.
Codex setup copies a compatible trusted native executable, avoiding PATH's npm
wrapper. Set `CODEX_SANDBOX_BIN` to an absolute trusted executable path to select
a source. Runtime paths can also be configured in `[monty] worker` and
`[native_shell] codex`. See the [Monty](crates/rig-monty) and
[native-shell](crates/rig-native-shell) guides.

## Selected capabilities

The terminal plugins can be enabled independently. Wheel scrolling and history
suggestions use Rig's existing transcript and history. Hold Shift while dragging
in Ghostty to select text. Tool boxes have bounded previews; **Ctrl+O** folds the
selected/latest call, **Alt+O** shows full retained content, and headers are
clickable. [Display settings and limits](crates/rig-tui-tools).

The loading line follows real activity and disappears when idle. The complete
welcome logo animates only while visible; questions and reduced motion stay
static. [Loading-line behavior](crates/rig-tui-loading).

For a small break, the games plugin adds `/tetris`, `/sudoku`, `/2048` and `/dino`.
**Esc** pauses and hides; rerunning resumes. **P** pauses, **R** starts again and
**C** holds/swaps the Tetris piece. Boards stay in memory until Rig quits.
[Controls and limits](crates/rig-tui-games).

With `rig-checkpoint-tasks`, `task` inherits the delivered conversation, including
earlier user instructions, and reports its final answer to the parent. Use
`checkpoint:false` for an explicitly fresh context. Forks share workspace files
and ordinary permission gates. Cache reuse and performance gains remain
unmeasured; existing compaction is not undone. [Checkpoint behavior](crates/rig-checkpoint-tasks).

`rig-conversation` asks for brief, natural progress updates between groups of
tool calls. Custom roles and structured permission reviews stay unchanged.
Cadence is model guidance, not a timer; a running tool cannot be interrupted for
an update. [Behavior and limits](crates/rig-conversation).

With `rig-delta`, `/share` and `/delta join LINK` start native, explicitly
consented live workflows. Confirm host-app access, audience, ongoing
transcript/file scope and joined roots separately. Remote context never becomes
local user authorization. The two-client fixture passes; real cross-client/account
validation and full Claude plugin parity remain outstanding. Install Delta
separately, and keep Rig home and its executable outside writable worktrees.
[Implemented features and gaps](crates/rig-delta#remaining-parity-and-validation-gaps).

## Execution contract

Native commands normally use the OS sandbox. Xonsh's `execution.escalate: true`
(or the optional native-shell tool's `escalate: true`) with a justification asks
the separate reviewer to authorize that exact complete command outside the
sandbox. Each expanded xonsh pipeline is frozen and reviewed independently; no
permit belongs to the outer interpreter. Failures never retry automatically.
Loaded home/project `AGENTS.md` restrictions are included in approval evidence.

After conversation compaction, automatic escalation refuses rather than trusting
a summary that may omit older restrictions; normal sandboxed work can continue.
Long uncompacted threads can also exhaust the evidence bound. `auto_review_status`
reports configured and required bytes/counts without a shell, model call, message
contents, settings change or permit. Neither it nor repeated consent bypasses
overflow or compaction. [Capacity limits and recovery](crates/rig-auto-review/README.md#long-session-capacity-and-recovery).

Delta's **native user controls** use a separate, closed host-app capability:
local `Person` confirmation grants only the fixed Delta workflow, with its full
OS-user app/network/filesystem authority disclosed. Publication scope and joined
folders need separate confirmation. This is not OS containment and never grants
`native_shell` an exception. Models cannot self-approve it; restored bindings and
saved setup JSON are not process permits. Watches/mounts have owned lifetimes,
bounded output, graceful stop and no automatic retry. [Authority and limits](crates/rig-delta).

`virtual_bash` handles text calculations with explicitly supplied files. Its
filesystem is separate and discarded after the call. It cannot run `cargo`,
access host files, or acquire native execution by falling back after failure.

Workspace-write isolation permits broad host reads and potentially destructive
changes inside the workspace. Printed secrets can reach the model provider.
Approved escalation has the OS user's filesystem and network authority. Review
is fallible judgment; it does not freeze mutable scripts, files or executables.
The runner owns foreground process-group cleanup, with documented limits for
descendants that deliberately change groups or sessions. Rust plugins are trusted
code, and adding another execution plugin requires checking its authority path.

## Development and publication

Use the smallest relevant crate checks, for example:

```sh
cargo test --locked -p rig-xonsh --lib
```

The Delta library suite also builds strict `AgentPass` schedules for standalone
and combined workspace, native, review and Delta plugins, with stock plugins
registered before and after them. Unordered shared access fails that check;
ordering setup itself must not install policy or execution. This checks the
pinned scheduling contract, not the user's assembled global agent.

Real-worker and OS-enforcement checks are explicit in the crate guides. Test an
assembled agent manually using your own selected global plugins; this workspace
does not maintain a second agent. [Rust design guidance](docs/rust-design.md)
records ownership and API conventions. The crate READMEs describe material
limitations; this repository does not claim Codex security parity, a persistent
VM, or full xonsh/CPython compatibility.

Source studies, implementation plans, private/session notes, machine-specific
installation records, detailed validation diaries and downloaded third-party
research live under the ignored `docs/research/` directory. Public documentation
stays in the root and crate READMEs plus `docs/rust-design.md`. Runtime prompt
Markdown remains tracked. Credentials, runtime state and generated caches must
stay untracked; `.gitignore` is a guard against accidental additions, not a secret
scanner.

Ignoring or untracking a file does not remove earlier commits. Review history
before publishing, and push only the intended branch, not local T3 checkpoint
refs via `git push --mirror`.
