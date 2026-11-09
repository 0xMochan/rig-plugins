# rig-tui-history

Prefix history navigation and inline history suggestions for Rig 0.46's native
TUI. Add `rig_tui_history::TuiHistoryPlugin` after `rig_tui::TuiPlugin`.

- Type a nonempty prefix to see the newest matching single-line prompt as a
  gray suffix. **Right** accepts the whole suggestion into the draft; it does
  not send it.
- **Up** immediately recalls the newest matching prompt. Further Up/Down keys
  keep the original prefix, and Down past the newest match restores the draft.
  An empty draft browses all eligible history.
- History begins only at the end of a single-line draft. An unchanged recalled
  entry may contain multiple lines; editing it or moving its cursor returns
  control to normal editor navigation. Picker, question, completion, transcript
  and other panel focus retain their keys, as do modified arrow keys.
- Ghost text appears only when the prefix fits on one visual row. The suffix
  is clipped to the remaining columns; wrapped/multiline/control-character and
  joined-emoji suggestions are hidden. Character widths follow the pinned
  stock editor, including wide characters. The cursor and submitted text are
  unaffected until Right accepts a suggestion.

This plugin reads the existing `PromptHistory`; it creates no second store.
Rig persists this history in the project's plugin data at `tui/history.jsonl`,
with its existing retention and file permissions. That history includes sent
prompts, slash commands and cleared or dropped unsent drafts; it is **not** a
confirmed-submissions-only log. The plugin excludes obvious authentication
commands, `[REDACTED]` entries and secrets recognized by Rig's redactor from its
suggestions and arrow navigation. This read-time filter does not remove saved
entries or provide general secret detection; the stock history and other
plugins may still access them.

Navigation keeps one owned original draft and selected entry because both must
survive later key events. Inspection borrows the existing history. The plugin
uses `Editor::set`, which does not record each replaced text, and leaves stock
submit/clear behavior intact. Switching from an existing native Ctrl+P history
session may record its original draft according to Rig's normal editor rules.

`HistoryInput` is a public system set inside `InputSystems::Filter` in `TuiKeys`
for ordering another input filter. Suggestions update after `TuiSystems::Prepare`
and before `Layout`; drawing overlays the main editor's `PanelCanvas` after
`Draw` and before `Render`. No frontend fork or private between-stage state is
used. These layout details deliberately match the pinned Rig release.

The focused headless tests use Rig's real key pipeline and `TuiTestScreen` to
check render-only suggestions, acceptance, prefix traversal and editor/modal
boundaries. They require no model provider or real terminal.
