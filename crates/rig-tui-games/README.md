# rig-tui-games

Four tiny games inside Rig's terminal, while the agent keeps working:

| Command | Play |
| --- | --- |
| `/tetris` | Arrows move/rotate/drop; Space hard-drops; Z rotates back; C holds/swaps |
| `/sudoku` | Arrows or hjkl move; 1–9 enter; 0, Delete or Backspace clear |
| `/2048` | Arrows, WASD or hjkl slide and merge tiles |
| `/dino` | Space or Up jumps over cacti |

Every game uses **P** to pause, **R** for a new game, and **Esc** to pause and hide.
Ctrl+C still reaches Rig's interrupt handler. A picker or question takes
priority and pauses the game. Hiding restores the previous panel focus.
Run the same slash command again to resume its board; opening another game
keeps the previous one paused. Boards and scores stay in memory until Rig
quits or restarts; **R** explicitly resets the visible game. A submitted slash
command uses Rig's normal command history; playing does not type into the draft
or send model messages.

## Placement

Games open in a centered popup by default. In your Rig home's `settings.toml`:

```toml
[tui_games]
placement = "right" # "popup" is the default
```

You can also choose per launch: `/tetris right`, `/sudoku popup`, etc.
A right panel needs its game width plus forty columns for chat and enough
height above the composer; otherwise it falls back to a popup. All four games
fit an 80×24 terminal with popup placement. Smaller terminals show a resize
message and stop the game clock. Tetris and Dino also stop ticking when paused,
finished, covered by a picker/question, or when reported terminal focus is lost.
They use a panel-owned `KeepAwake`; turn-based games have no animation clock.

## Use independently

```toml
[[plugin]]
crate = "rig-tui-games"
path = "/absolute/path/to/rig-plugins/crates/rig-tui-games"
plugin = "rig_tui_games::TuiGamesPlugin"
```

Select `rig_tui::TuiPlugin` separately. The games plugin works before or after
it in the plugin list. A composition with other input filters should put
`rig_tui_games::GamesInput` first; the supplied profile already does this.
The plugin registers no commands without `TuiViewPlugin`, and game commands
refuse a run without a terminal surface.

## Boundaries and limits

The crate uses registry Rig 0.46.0 and Bevy 0.20.0. It adds no model, network,
process, audio, storage, or execution permissions. Game rules are ordinary
private Rust values, independent of ECS. The shared shell alone owns focus,
placement and lifecycle. A small seeded RNG is for game variety only.

Tetris uses a seven-piece bag, a hold slot (**C**, once per falling piece),
simple wall/floor kicks, a brief landing highlight and a line-clear flash/wipe
before rows collapse. Locking is immediate; it does not implement full SRS.
Sudoku varies one curated, uniquely solvable puzzle using legal transformations,
with conflict feedback and no notes/hints/difficulty selector. 2048 allows continued play after winning;
scores saturate and the largest representable tile cannot merge further. Dino
is an original ASCII runner inspired by Chrome's game, with cacti and jumping,
without ducking or flying obstacles. Timed games bound catch-up after stalls,
so their simulated time can slow under load.
