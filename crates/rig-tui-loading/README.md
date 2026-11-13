# rig-tui-loading

A borderless thinking line **above** Rig's input box, with a small traveling
bar wave inspired by the Rig logo's three-plus-three panels:

```text
 ▂▄▆ ▄▂▁   thinking                                      3s
 ▁▂▄ ▆▄▂   thinking                                      3s
 ▄▂▁ ▂▄▆   thinking                                      3s
```

These are successive samples, not three stacked rows. Six bars rise and fall
as a low crest travels right on a 1.44-second loop. The seven-column footprint
and open center gap stay fixed; no rings, center dot or label movement. Narrow
terminals use three bars without the gap. Reduced motion and questions use a
static `▂▄▆ ▆▄▂` mark (`▂▆▂` on narrow terminals).

The **real three-row Rig logo in the welcome banner** pulses in sync whenever
it is in view, including when only part of it is visible. Its geometry, version,
model and directory are left untouched; no second banner is created.

Labels come from the selected agent's real `Activity`: thinking, writing,
running tools, named work, retry countdowns, or questions. Elapsed time covers
the continuous busy period, not each individual model/tool phase. Switching
agents starts a new local timer. Idle, a missing agent, or missing activity
reclaims the row. Waiting for your answer uses a static mark and banner, with
no animation timer or elapsed counter.

## Use

Pinned to registry Rig **0.46.0** and Bevy **0.20.0**. Add the crate by path and
select `rig_tui_loading::TuiLoadingPlugin` alongside `rig_tui::TuiPlugin` (or
`TuiViewPlugin` with your own screen). Plugin ordering does not matter for the
default screen. It installs Rig's stock `ActivityPlugin` if absent; it does not
install a frontend, welcome banner or execution capability. Without a TUI view
it is inert. Add it explicitly to your existing global `~/.rig/plugins.toml`:

```toml
[[plugin]]
crate = "rig-tui-loading"
path = "/absolute/path/to/rig-plugins/crates/rig-tui-loading"
plugin = "rig_tui_loading::TuiLoadingPlugin"
```

Run `rig plugin check` and `rig build`, then restart/reload Rig.

For reduced motion in your Rig settings:

```toml
[tui_loading]
animate = false
```

This keeps both animations still; active elapsed time refreshes once a second.
Terminal apps have no standard browser-style reduced-motion preference, so
this is explicit. Colors use theme-relative grays on light/dark backgrounds;
`NO_COLOR` uses dim/normal emphasis while preserving other styles.

## Ownership and limits

- Uses public activity, canvas, snapshot and redraw APIs. No frontend fork,
  conversation changes, model calls, terminal escape writes or execution hooks.
- Only the selected agent's activity item (`"status"`) moves out of the bottom
  box while the replacement fits. Model, context, cost and other status items
  remain. The original item returns if a small terminal suppresses the line or
  you switch agents. Idle's stock status is unchanged.
- Text clips to one row; label control characters become spaces. Tiny screens
  may suppress the line to preserve Rig's minimum transcript area. A visible
  banner can still animate; with both out of view there is no animation wake.
- Only cells belonging to the authentic `rig.welcome` aside and matching its
  complete native logo are restyled. Ordinary logo-shaped messages are not.
  Unfamiliar/custom logo rendering is left alone. Compositor overlays still
  cover the banner normally.
- Banner visibility uses the main pane's public `DrawnTranscript`, reusing the
  snapshot already installed by `rig-tui-tools`. Standalone use installs it if
  missing; this copies all retained transcript text on each layout, with work
  and memory proportional to transcript size. Animation does not invalidate
  cached transcript rendering on each tick.
- One scoped `KeepAwake` serves both animations. It owns no background thread,
  percentage estimate, retry, tool authorization or agent lifetime. There is no
  idle animation, multi-agent rollup or private reasoning display.
- Bar and block-logo rendering depend on terminal font support. There is no
  persisted animation state or reduced-motion autodetection.
- A custom plugin resizing panels during `TuiSystems::Fit` can cause one update
  of stale status relocation. The animation clock and banner use final layout;
  no private frontend systems are patched.
