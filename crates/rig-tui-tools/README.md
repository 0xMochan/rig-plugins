# Foldable tool boxes in the terminal

`TuiToolsPlugin` gives **every tool call** a bounded preview and independent
fold/expand controls, including basic `read`, edits, searches and plugin tools.
It runs after specialized rendering so file previews, syntax colors, diffs and
named decorations keep their existing presentation. Headers use quiet native
terminal backgrounds, bold titles and disclosure arrows instead of colored bands.
Unknown tools get readable JSON inputs and typed results instead of escaped
arguments or missing JSON output.

It also draws `run_xonsh`, `run_monty`, `native_shell` and
`virtual_bash` as highlighted multiline source rather than escaped JSON.
Inputs and virtual files appear separately. Typed JSON results show their
stdout, stderr and returned value; exit status, duration and tool truncation
remain in the header. Live child calls show their state and script while the
parent runs, and remain available in the full view after it ends.

The default script view allows **8 wrapped input rows and 10 wrapped output
rows**, plus headers and clipping notices. Other tool boxes allow **10 wrapped
body rows**, shared with any appended decorations. Small calls appear in full.
These limits only affect presentation, and do not alter the tool call or its result.

- **Ctrl+O** collapses a call to its header, or restores its bounded preview.
- **Alt+O** shows the full retained input/output, or restores the preview.
- **Click a header** to fold/unfold that call. Click **more** to show the
  full call, and **Show preview** at its bottom to restore the bounded view.
- **Alt+Up / Alt+Down** use Rig's transcript selection to choose a call.
  With no selection, the shortcuts affect the latest tool call. Selection on a
  call's detail decoration also targets that call.
  **Esc** leaves selection and returns to the composer.

The shortcuts preserve the draft and do not answer approval questions or
steal a focused panel's keys. Fold state is local to each agent in this run;
reload/resume resets it. Calls whose arguments are still streaming have no
stable transcript selection yet, but still receive bounded readable previews.
Drags and modified clicks do not fold calls. Transcript exports always include
the full retained content, regardless of interactive folds.

Mouse reporting comes from `rig-tui-scroll`, already enabled in the main and
standalone profiles, or another reporting owner in a custom composition.
Keep Shift held while dragging in Ghostty to select text. The display plugin
does not change terminal mouse modes itself.

## Use independently

Add this alongside `rig_tui::TuiPlugin` (either plugin order works):

```toml
[[plugin]]
crate = "rig-tui-tools"
path = "/absolute/path/to/rig-plugins/crates/rig-tui-tools"
plugin = "rig_tui_tools::TuiToolsPlugin"
```

The personal `RigProfilePlugin` already includes it with its default `tui`
feature. An independently listed duplicate is skipped by Rig's loader.
Optional settings in the selected Rig home's `settings.toml`:

```toml
[tui_tools]
input_rows = 8
output_rows = 10
```

Preview limits are clamped to 1–1000 rows each. Expansion removes those
display limits; it cannot recover output already truncated by a tool.
Rig supplies Python highlighting for xonsh, so its shell forms have no special
xonsh syntax colors. Live stdout requires the execution tool to publish it;
the renderer can show existing live output and child state, but does not add
streaming to the execution plugins. Other tools keep their existing renderers,
with folding applied in Rig's final Edit phase. Custom Edit-phase renderers must
order themselves before `rig_tui_tools::ToolBoxFolds` to participate.

Folding covers tool-call boxes, not assistant prose, approval dialogs, standalone
attachments or notes. These have different identities and input ownership;
this plugin deliberately does not hide approval UI or alter normal message text.

The crate depends only on Rig's pinned harness/TUI, matching Bevy, and serde.
It does not load execution plugins, start commands, gate work, change model
requests or save a second transcript. Render inputs are borrowed; only displayed
rows and per-agent fold choices are owned here.
Mouse hit testing requests Rig's `DrawnTranscript` snapshot only on the main
pane. That snapshot copies the laid-out transcript when it changes, so its
memory and update cost grow with transcript length.
