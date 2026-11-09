# rig-tui-scroll

Mouse and trackpad history scrolling for the original Rig 0.46 TUI. This plugin
enables terminal wheel reports and translates them into Rig's existing scroll
actions. Rig still renders the screen and owns transcript positions, resize
anchors and following new output at the bottom.

Add after the TUI in `plugins.toml`:

```toml
[[plugin]]
crate = "rig-tui-scroll"
path = "/absolute/path/to/rig-plugins/crates/rig-tui-scroll"
plugin = "rig_tui_scroll::TuiScrollPlugin"
```

Each wheel event moves three rows by default. In Rig's `settings.toml`:

```toml
[tui_scroll]
lines_per_event = 3
```

The effective value is clamped to 1–10 rows. Wheel events over the main
transcript or composer scroll the transcript without editing the unsent draft
or selecting prompt history. Open questions, pickers and focused other panels
retain their input. Keyboard scrolling remains Rig's own behavior.

`WheelInput` is a public set inside `InputSystems::Filter` in `TuiKeys`, so a
composition can order other input filters without adding cross-plugin dependencies.

Mouse reporting changes ordinary terminal click-and-drag selection. In
[Ghostty, hold Shift while dragging to select text natively](https://ghostty.org/docs/config/reference#mouse-shift-capture)
with its default configuration. Ghostty must allow application mouse reporting;
this plugin does not change global terminal settings. Trackpads are exposed by
the terminal as wheel events, so the terminal controls their event frequency.

Native mode ownership supports macOS and other Unix terminals with `/dev/tty`
and SGR mouse reporting. Print/JSON fronts do not enable it. Rig test screens
record mode bytes entirely in memory and never open a real TTY. Other platforms
can use the routing system but this crate does not enable native mouse modes
there yet.

Normal enable/disable bytes use Rig's `TerminalWrite` message. A small owning
guard keeps a `/dev/tty` file because Rig redirects stdout and its public message
writer flushes after external programs have run. Before `RunInTerminal` hands
the terminal to an editor, an observer disables reporting immediately through
that file. After Rig takes the terminal back, reporting resumes through
`TerminalWrite`. No child command, launcher or permission policy is changed.
The guard also attempts cleanup on drop, including unwinding; I/O failures are
reported and abrupt process termination cannot guarantee terminal cleanup.

The integration targets registry Rig `=0.46.0`, source commit
`6e039cd24994bc1e7d963af6f6278df73a81b130`, and Bevy `=0.20.0`. It uses public
input filters, panel geometry, scroll actions and terminal lifecycle components.
