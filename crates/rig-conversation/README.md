# rig-conversation

Brief, natural progress updates for Rig's default coding agent. Independently
usable with Rig 0.46.0; no TUI, execution backend, timer or extra model required.

```toml
[[plugin]]
crate = "rig-conversation"
path = "/absolute/path/to/rig-plugins/crates/rig-conversation"
plugin = "rig_conversation::ConversationPlugin"
```

In Rust, add `ConversationPlugin` alongside `AgentPlugin`. It installs Rig's
`DefaultPromptPlugin` if absent. The personal `RigProfilePlugin` includes it
already, with the normal registration guard against duplicate installations.
Rebuild and restart/reload the agent to apply it; this does not change an
already-running binary.

## Behavior

The plugin replaces only the stock instruction discouraging between-tool
narration. It asks the agent to explain its initial approach, share discoveries
and concise decision explanations between groups of calls, and announce
substantial edits/checks. Updates should read like a teammate talking, usually
one or two sentences, with varied wording and no mechanical per-tool preamble.
Simple tasks stay simple; explicit requests for quiet are respected.

Progress is ordinary assistant text, streamed and retained by Rig's existing
conversation machinery alongside tool calls. There is no second transcript or
`progress` tool. The guidance asks for public observations and summaries, not
private internal reasoning. It forbids invented progress and does not change
permissions, retries or tool availability.

## Boundaries and limits

- Only an exact stock `DEFAULT_SYSTEM_PROMPT` on a `TurnCall` is adapted, in
  `PrepareSystems::Edit`, after default prompt composition. All other stock
  instructions and prompt sections remain intact.
- Custom role prompts and `SideCall`s (including structured permission reviews)
  remain untouched. Another plugin editing the role can prevent this exact
  match; there is no attempt to override arbitrary third-party prompts.
- Roughly 30-second updates are model guidance, not a scheduler guarantee. The
  plugin cannot interrupt a running tool or force a silent model to speak.
- Rig 0.46.0 continues tool-bearing replies. A completed text-only reply ends
  the turn even if the provider labels it commentary. This plugin does not
  change kernel continuation or add automatic model calls.
- Scripted tests establish prompt delivery and the text/tool/history path, not
  live-model conversational quality, exact cadence, or full Codex parity.
