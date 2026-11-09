//! Local, in-memory games over Rig's public terminal panels and input pipeline.
//!
//! The shell retains each game and owns one focused panel and its clock.
//! Each game owns only its rules and board; no prompts, tools, subprocesses
//! or model calls.

mod dino;
mod game2048;
mod random;
mod shell;
mod sudoku;
mod tetris;

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use bevy_app::prelude::*;
use bevy_ecs::prelude::*;
use rig_harness::commands::{AppCommandsExt, CommandArgs};
use rig_harness::prelude::AppSettingsExt;
use rig_tui::crossterm::event::KeyCode;
use rig_tui::ratatui::{buffer::Buffer, layout::Rect};
use rig_tui::{InputSystems, Theme, TuiKeys, TuiSystems, TuiViewPlugin};
use serde::Deserialize;

/// Adds `/tetris`, `/sudoku`, `/2048` and `/dino` to the terminal frontend.
/// Inert without a TUI view; does not add any execution capability.
#[derive(Default, Debug)]
pub struct TuiGamesPlugin;

/// Game input capture inside [`InputSystems::Filter`]. Compositions should
/// order it before other filters which rewrite or consume `PendingInput`.
#[derive(SystemSet, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct GamesInput;

impl Plugin for TuiGamesPlugin {
    fn build(&self, app: &mut App) {
        app.add_setting::<GamesSettings>("tui_games");
    }

    fn finish(&self, app: &mut App) {
        if !app.is_plugin_added::<TuiViewPlugin>() {
            return;
        }
        for (name, kind) in [
            ("tetris", Kind::Tetris),
            ("sudoku", Kind::Sudoku),
            ("2048", Kind::Game2048),
            ("dino", Kind::Dino),
        ] {
            app.add_command(
                name,
                &format!("Play {} locally; optional popup or right", kind.title()),
                move |In(args): In<CommandArgs>, mut commands: Commands| {
                    commands.trigger(shell::Open { args, kind });
                },
            );
        }
        app.add_observer(shell::open)
            .configure_sets(TuiKeys, GamesInput.in_set(InputSystems::Filter))
            .add_systems(TuiKeys, shell::input.in_set(GamesInput))
            .add_systems(PostUpdate, shell::prepare.in_set(TuiSystems::Prepare))
            .add_systems(PostUpdate, shell::fit.in_set(TuiSystems::Fit))
            .add_systems(PostUpdate, shell::draw.in_set(TuiSystems::Draw))
            .add_systems(
                PostUpdate,
                shell::clock
                    .after(TuiSystems::Draw)
                    .before(TuiSystems::Render),
            );
    }
}

#[derive(Resource, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct GamesSettings {
    placement: Location,
}

#[derive(Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum Location {
    #[default]
    Popup,
    Right,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Tetris,
    Sudoku,
    Game2048,
    Dino,
}

impl Kind {
    fn title(self) -> &'static str {
        match self {
            Self::Tetris => "Tetris",
            Self::Sudoku => "Sudoku",
            Self::Game2048 => "2048",
            Self::Dino => "Dino",
        }
    }

    fn size(self) -> (u16, u16) {
        match self {
            Self::Tetris => tetris::SIZE,
            Self::Sudoku => sudoku::SIZE,
            Self::Game2048 => game2048::SIZE,
            Self::Dino => dino::SIZE,
        }
    }

    fn help(self) -> &'static str {
        match self {
            Self::Tetris => tetris::HELP,
            Self::Sudoku => sudoku::HELP,
            Self::Game2048 => game2048::HELP,
            Self::Dino => dino::HELP,
        }
    }

    fn start(self) -> Game {
        let seed = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos() as u64;
        match self {
            Self::Tetris => Game::Tetris(tetris::Tetris::new(seed)),
            Self::Sudoku => Game::Sudoku(sudoku::Sudoku::new(seed)),
            Self::Game2048 => Game::Game2048(game2048::Game2048::new(seed)),
            Self::Dino => Game::Dino(dino::Dino::new(seed)),
        }
    }

    fn realtime(self) -> bool {
        matches!(self, Self::Tetris | Self::Dino)
    }
}

enum Game {
    Tetris(tetris::Tetris),
    Sudoku(sudoku::Sudoku),
    Game2048(game2048::Game2048),
    Dino(dino::Dino),
}

impl Game {
    fn key(&mut self, key: KeyCode) {
        match self {
            Self::Tetris(game) => game.key(key),
            Self::Sudoku(game) => game.key(key),
            Self::Game2048(game) => game.key(key),
            Self::Dino(game) => game.key(key),
        }
    }

    fn tick(&mut self, elapsed: Duration) {
        match self {
            Self::Tetris(game) => game.tick(elapsed),
            Self::Dino(game) => game.tick(elapsed),
            _ => {}
        }
    }

    fn draw(&self, area: Rect, buffer: &mut Buffer, theme: &Theme) {
        match self {
            Self::Tetris(game) => game.draw(area, buffer, theme),
            Self::Sudoku(game) => game.draw(area, buffer, theme),
            Self::Game2048(game) => game.draw(area, buffer, theme),
            Self::Dino(game) => game.draw(area, buffer, theme),
        }
    }

    fn is_over(&self) -> bool {
        match self {
            Self::Tetris(game) => game.is_over(),
            Self::Sudoku(game) => game.is_over(),
            Self::Game2048(game) => game.is_over(),
            Self::Dino(game) => game.is_over(),
        }
    }
}

#[cfg(test)]
mod tests;
