# The synced agent

`plugins.toml`, `settings.toml` and `AGENTS.md` are the agent's tracked definition.
The generated agent's `project/Cargo.lock` is also tracked, so a fresh setup
uses the production dependency graph we built. Other generated project files,
credentials, sessions, caches and runtime defaults in this directory are private
and ignored. No credentials are copied from your existing
Rig home, and the normal `~/.rig` installation is separate.

From anywhere, run `/path/to/rig-plugins/agent setup` once, then
`/path/to/rig-plugins/agent`. The current directory is the project you want to
work on. Use `/login chatgpt` for this profile's own subscription login and
`/model` for the coding model. The reviewer model is explicitly configured in
`settings.toml`; absent or invalid reviewer configuration denies escalation.

Setup uses uv to install the hash-pinned Monty 1.4.0 compiled worker in this
profile's ignored `tools/` directory. Python hosts installation only; the agent
executes the Rust worker. `run_monty` uses fresh workers and ordinary child Rig
tools. Later rebuilds use `agent build`; worker setup stays separate from tool
calls and requires no account credentials.

The launcher requires Rig 0.46.0 and codex-cli 0.142.0. Setup copies a trusted
native Codex executable into `tools/bin/codex-sandbox`; this profile-local copy
is preferred over PATH. Setup first checks the existing compatible profile copy,
then the Codex standalone installation, then PATH. To choose another source,
run `CODEX_SANDBOX_BIN=/absolute/native/codex ./agent setup`. npm wrappers need
Node and cannot start in the adapter's deliberately cleared environment.
For an explicit runtime override, set `[native_shell] codex` to an absolute path.
The adapter
uses Codex only as an OS sandbox launcher; it does not run a second coding agent
or use Codex account credentials. The profile deliberately exposes no stock
shell, remote execution, reload tool, helpers, or language-runtime bypass.

T3/ACP is optional future integration. This profile runs standalone through Rig's
terminal and JSON fronts. Add a transport only after verifying its execution and
permission paths against this composition.

The composition's default `tui` feature enables Rig's normal welcome, model
pickers and tool renderers. Loading `TuiPlugin` alone only provides the terminal
frontend; it does not enable those feature-gated renderers in `rig-basics`.

The composition also includes `rig-checkpoint-tasks`: `task` inherits full
delivered history by default, with `checkpoint:false` for a fresh context.
`message` and `wait` retain their normal behavior. See
[the checkpoint behavior and limits](../crates/rig-checkpoint).

The independently selected `rig-tui-scroll` and `rig-tui-history` plugins add
wheel transcript scrolling, prefix-filtered Up/Down history and gray suggestions
accepted with Right. The composition orders their input filters explicitly;
each crate remains usable by itself. Ghostty's default Shift-drag selects text
while application mouse reporting is enabled. History uses Rig's existing
per-project store, including unsent drafts and slash commands.

`rig-tui-tools`, also included by the profile composition, renders script tools
as multiline highlighted source with separate inputs/files and structured
results. Ctrl+O folds the selected or latest script call; Alt+O toggles its full
view. Alt+Up/Down selects earlier calls. Headers are clickable, and the “more” /
“Show preview” rows toggle full detail. Previews default to 24 wrapped input
rows and 12 output rows; `[tui_tools] input_rows` and `output_rows` customize
them. See [the display plugin](../crates/rig-tui-tools) for its boundaries.

`rig-tui-loading` adds a borderless parenthesis ripple and elapsed time above
the composer. The complete native welcome logo pulses in place while visible.
It follows the shown agent, disappears when idle, and stays static for questions.
Model/context/cost metadata stays in the composer; only active status moves.
Set `[tui_loading] animate = false` for reduced motion. Narrow terminals use
`(·)`; tiny ones retain the stock in-box status. See
[the loading plugin](../crates/rig-tui-loading) for ownership and limits.

`rig-conversation`, included by the composition, replaces the stock rule
suppressing between-tool narration with brief, natural progress updates.
Custom roles and structured permission reviews are left alone. Updates use
ordinary assistant text; timing depends on the model and does not interrupt
running tools. See [the conversation plugin's behavior and limits](../crates/rig-conversation).

`rig-delta` adds native live controls: `/share`, `/delta join LINK`, `status`,
`reconnect`, `disconnect`, `setup` and account actions. Native confirmation
separately approves Delta host access, ongoing transcript/files and joined roots;
ordinary model-invoked `native_shell` remains sandboxed/reviewed as before. Models
cannot self-approve live mode. `delta_session` inspects local state and
`delta_worktree` selects only already approved roots; bounded `delta`/`delta_sync`
remain available for unbound snapshot sessions. Keep Rig home and the installed
Delta executable outside writable roots: this standalone home cannot live-share
its own containing checkout. Use the normal `~/.rig` installation, or launch this
profile from another project. The two-client fixture passes; real Delta/Claude
handoff and full reference parity still need validation. Rebuild and restart.
[Setup, usage and limits](../crates/rig-delta).

`agent_status` reports harness-registered plugins, tools allowed for the calling
agent, its work directory, and native-shell startup diagnostics. It runs no
command. A ready startup diagnostic checks executable compatibility; it is not
an enforcement probe. Restart the agent after repairing an unavailable backend.

`rig-tui-games` supplies `/tetris`, `/sudoku`, `/2048`, and `/dino` as local
terminal popups. **Esc** pauses and hides; the same command resumes its board.
**P** pauses, **R** starts again and **C** holds/swaps a Tetris piece. Add
`[tui_games] placement = "right"` in settings for a side panel, or pass `right`
or `popup` to a game command. Narrow terminals fall back to the popup. Boards
and scores stay in memory until Rig quits. [Controls and limits](../crates/rig-tui-games).
