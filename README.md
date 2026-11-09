# Rig plugins

A personal Rig agent and a collection of independently usable plugin crates.
The first milestone establishes virtual Bash, native sandbox execution, and
independent permission review. Monty adds process-isolated Python code mode;
a focused xonsh frontend combines it with explicit shell forms.

| Crate | Responsibility |
| --- | --- |
| [rig-bashkit](crates/rig-bashkit) | Stateless `virtual_bash` over explicit text inputs; no host capabilities |
| [rig-command](crates/rig-command) | Frozen native commands, OS sandbox adapter, deadlines, output and process ownership |
| [rig-native-shell](crates/rig-native-shell) | `native_shell`, exact per-call permission binding and execution handoff |
| [rig-auto-review](crates/rig-auto-review) | Tool-free model review of explicit escalations; failure denies permission |
| [rig-monty](crates/rig-monty) | Bounded Python in fresh workers; structured, gated child tool calls |
| [rig-xonsh](crates/rig-xonsh) | Python plus explicit shell forms, pipelines and argument-safe interpolation |
| [rig-tui-scroll](crates/rig-tui-scroll) | Mouse/trackpad transcript scrolling through Rig's native scroll actions |
| [rig-tui-history](crates/rig-tui-history) | Prefix-filtered prompt history and gray suggestions accepted with Right |
| [rig-tui-tools](crates/rig-tui-tools) | Foldable previews for every tool box, quiet headers and highlighted scripts |
| [rig-tui-loading](crates/rig-tui-loading) | Compact thinking ripple above the composer; the real welcome logo pulses in view |
| [rig-tui-games](crates/rig-tui-games) | `/tetris`, `/sudoku`, `/2048` and `/dino` in a popup or right-side terminal panel |
| [rig-checkpoint](crates/rig-checkpoint) | Full conversation checkpoints, local prompt inheritance and shared cache identity |
| [rig-checkpoint-tasks](crates/rig-checkpoint-tasks) | Checkpoint context by default for `task`; explicit fresh-context opt-out |
| [rig-conversation](crates/rig-conversation) | Natural progress updates between tool calls, without changing execution or review policy |
| [rig-delta](crates/rig-delta) | Native live sharing/join, owned Delta watches/file mounts, fenced recording and bounded CLI tools |
| [rig-profile](crates/rig-profile) | The personal agent composition, including instructions, skills, sessions and checkpoint tasks |

Each capability has its own crate. The composition plugin adds the chosen
capabilities; the terminal frontend remains an independently selected plugin.

The profile includes native [Delta integration](crates/rig-delta). `/share`
(or `/delta share`) now starts a native live workflow: confirm host-app access,
the audience and ongoing transcript/file scope. `/delta join LINK` creates a fresh
agent in explicitly trusted mounted worktrees; `/delta status`, `reconnect`,
`disconnect` and `setup` provide controls. Messages remain non-user remote context;
turn takeover stops local work and descendants. The standard Delta protocol is
intended to interoperate with the Delta app and Claude Code, not a Rig-only server.
The two-client fixture passes; real cross-client/account validation and full
Claude plugin parity remain outstanding. [Implemented features and gaps](crates/rig-delta#remaining-parity-and-validation-gaps).
Install Delta separately, then rebuild/reload Rig. Keep Rig home and the installed
Delta executable outside all writable project/worktree roots.

For a small break while the agent works, run `/tetris`, `/sudoku`, `/2048`, or
`/dino`. **Esc** pauses and hides; rerun the command to resume. **P** pauses,
**R** starts a new game, and **C** holds/swaps the Tetris piece. Set
`[tui_games] placement = "right"` for a side panel, or use `/2048 right` per
launch; the default is a popup. Boards and scores stay in memory until Rig
quits. [Controls and limits](crates/rig-tui-games).

The profile's `task` tool now starts a [checkpoint agent](crates/rig-checkpoint-tasks):
it inherits the delivered conversation, including earlier user instructions,
and returns its final report while keeping intermediate work in its own history.
Use `checkpoint:false` for an explicitly fresh context. Forks share workspace
files and ordinary permission gates. Provider cache reuse and performance gains
remain unmeasured; existing conversation compaction is not undone.

## Run the agent

Prerequisites: Rust 1.98.1, Rig 0.46.0, uv for worker setup, and a trusted codex-cli 0.142.0 installation.
The repository pins the Rust toolchain and matching Rig/Bevy APIs. Codex supplies
the OS sandbox launcher; it does not run another coding agent or require a Codex
account for that function.

If Rig is missing, install the pinned launcher with
`cargo install rig --version 0.46.0 --locked`. Obtain the pinned Codex CLI from
its [official release](https://github.com/openai/codex/releases/tag/rust-v0.142.0).

```sh
./agent setup
./agent
```

Setup installs the official Monty 1.4.0 Rust worker from hash-pinned binary
wheels and copies a compatible trusted native Codex executable into ignored
`profile/tools`, then builds the agent. This keeps sandbox startup independent
of terminal PATH order and npm's Node wrapper. Set `CODEX_SANDBOX_BIN` to an
absolute native executable path if setup cannot find the pinned release.
Later source changes
can use `./agent build`. [Monty setup and limits](crates/rig-monty/README.md).

From another project, run `/path/to/rig-plugins/agent` there. Its current directory
is the workspace. On first use, `/login chatgpt` signs in for this profile and
`/model` selects the coding model. Configure the permission reviewer separately
in [profile/settings.toml](profile/settings.toml); unavailable review models deny
escalation rather than silently selecting another model.

For a T3 command button on macOS, replace the example path with your checkout:

```sh
open -na Ghostty --args --working-directory="$PWD" -e /path/to/rig-plugins/agent
```

[profile/](profile/README.md) holds the tracked plugin inventory, settings,
instructions and generated agent dependency lock. Runtime credentials, sessions and build output are ignored. The
launcher uses this Rig home and leaves the existing `~/.rig` profile separate.

The profile includes wheel scrolling over the transcript or composer. Hold
Shift while dragging in Ghostty to select text. Type a prefix for a gray history
suggestion, press Right to accept it, or use Up/Down to browse matching prompts.
Down past the newest match restores the original draft. These reuse Rig's
existing history, which also contains cleared unsent drafts and slash commands.
Every tool box—including reads, edits and searches—has a bounded preview with
quiet disclosure headers. Script tools show multiline code, inputs/files and
structured results, with 8 wrapped input rows and 10 output rows by default;
other tools retain 10 body rows. Ctrl+O folds the selected or latest tool call,
and Alt+O shows its full retained content. Alt+Up/Down selects older calls. [Tool display settings and limits](crates/rig-tui-tools).
Headers also fold/unfold on click; click a “more” row to expand the call and
“Show preview” to return to the bounded view.
A separate loading line above the input box uses a compact parenthesis ripple
and shows real activity plus elapsed time. Rig's complete welcome logo pulses
in sync while in view. Both stay still for questions; the line disappears when
idle. Set `[tui_loading] animate = false` for reduced motion. Narrow screens use
`(·)` and tiny screens fall back to the stock status.
[Loading-line behavior and limits](crates/rig-tui-loading).
The profile also includes `rig-conversation`: brief, natural updates about what
it is investigating, learning and changing between groups of tool calls. This
replaces Rig's stock no-narration rule for ordinary default-prompt turns;
custom roles and structured permission reviews stay unchanged. It uses existing
assistant messages, not private reasoning or a new progress tool. Cadence is
model guidance, not a timed guarantee; a running tool cannot be interrupted for
an update. [Behavior and limits](crates/rig-conversation).
Ask the agent for its loaded plugins to use `agent_status`, a runtime inventory
that works even when shell startup is unavailable.

## Execution contract

Use `native_shell` for real programs and workspace files. Calls normally use the
OS sandbox. An explicit `escalate=true` request with a justification asks the
separate reviewer to authorize that exact complete command outside the sandbox.
It does not create a reusable approval, and failures never retry automatically.
Loaded home/project `AGENTS.md` restrictions are included in approval evidence.
After conversation compaction, automatic escalation refuses rather than trusting
a summary that may omit older restrictions; normal sandboxed work can continue.
Long uncompacted threads can also exhaust the evidence bound. `auto_review_status`
reports the running bound and complete required bytes/counts without a shell,
model call, message contents, settings change or permit. Neither it nor repeated
consent bypasses overflow or compaction. [Capacity limits and legitimate recovery](crates/rig-auto-review/README.md#long-session-capacity-and-recovery).

Delta's **native user controls** use a separate, closed host-app capability:
local `Person` confirmation grants only the fixed Delta workflow, with its full
OS-user app/network/filesystem authority disclosed. Publication scope and joined
folders need separate confirmation. This is not OS containment and never grants
`native_shell` an exception. Models cannot self-approve it; restored bindings and
saved setup JSON are not process permits. Watches/mounts have owned lifetimes,
bounded output, graceful stop and no automatic retry. [Authority and limits](crates/rig-delta).

Use `virtual_bash` for text calculations with explicitly supplied files. Its
filesystem is separate and discarded after the call. It cannot run `cargo`,
access host files, or acquire native execution by falling back after failure.

Workspace-write isolation permits broad host reads and potentially destructive
changes inside the workspace. Printed secrets can reach the model provider.
Approved escalation has the OS user's filesystem and network authority. Review
is fallible judgment; it does not freeze mutable scripts, files or executables.
The runner owns foreground process-group cleanup, with documented limits for
descendants that deliberately change groups or sessions. Rust plugins are trusted
code, and adding another execution plugin requires checking its authority path.

## Documentation

Use the crate guides linked in the table above for setup, behavior and limits.
The [profile guide](profile/README.md) covers the assembled agent, and
[Rust design guidance](docs/rust-design.md) records the repository's ownership
and API conventions.

The crate READMEs describe material limitations. This milestone does not claim
Codex security parity, a persistent VM, or full xonsh/CPython compatibility.

## Publication boundary

Source studies, implementation plans, private/session notes, machine-specific
installation records, detailed validation diaries and downloaded third-party
research live under the ignored `docs/research/` directory. Public documentation
stays in the root, crate and profile READMEs plus `docs/rust-design.md`. Runtime
prompt Markdown remains tracked. Credentials, runtime state and generated caches
must stay untracked; `.gitignore` is a guard against accidental additions, not a
secret scanner.

Ignoring or untracking a file does not remove earlier commits. Review the history
before publishing, and push only the intended branch, not local T3 checkpoint
refs via `git push --mirror`.
