//! An automatic runner with bounded, local physics. The shell owns lifecycle.

use std::time::Duration;

use rig_tui::Theme;
use rig_tui::crossterm::event::KeyCode;
use rig_tui::ratatui::buffer::Buffer;
use rig_tui::ratatui::layout::Rect;
use rig_tui::ratatui::style::{Modifier, Style};
use rig_tui::ratatui::widgets::{Paragraph, Widget};

use crate::random::Random;

pub(super) const SIZE: (u16, u16) = (48, 14);
pub(super) const HELP: &str = "Space / Up: jump over the cacti";

const RUNNER_X: f64 = 5.0;
const RUNNER_WIDTH: f64 = 4.0;
const GROUND: i32 = 11;
const JUMP_SPEED: f64 = 19.0;
const GRAVITY: f64 = 32.0;
const MAX_TICK: Duration = Duration::from_millis(250);
const STEP_SECONDS: f64 = 0.01;

struct Cactus {
    x: f64,
    tall: bool,
}

impl Cactus {
    fn width(&self) -> f64 {
        if self.tall { 3.0 } else { 2.0 }
    }

    fn height(&self) -> f64 {
        if self.tall { 3.0 } else { 2.0 }
    }
}

pub(super) struct Dino {
    random: Random,
    cacti: Vec<Cactus>,
    next_gap: f64,
    elevation: f64,
    velocity: f64,
    distance: f64,
    over: bool,
}

impl Dino {
    pub(super) fn new(seed: u64) -> Self {
        let mut game = Self {
            random: Random::new(seed),
            cacti: Vec::new(),
            next_gap: 0.0,
            elevation: 0.0,
            velocity: 0.0,
            distance: 0.0,
            over: false,
        };
        game.spawn();
        game
    }

    pub(super) fn key(&mut self, key: KeyCode) {
        if !self.over
            && self.elevation == 0.0
            && self.velocity == 0.0
            && matches!(key, KeyCode::Up | KeyCode::Char(' '))
        {
            self.velocity = JUMP_SPEED;
        }
    }

    fn speed(&self) -> f64 {
        12.0 + (self.distance / 250.0).min(10.0)
    }

    pub(super) fn tick(&mut self, elapsed: Duration) {
        // Never catch up an unbounded stall. Substeps keep even the fastest
        // cactus below a quarter cell per collision check.
        let mut remaining = elapsed.min(MAX_TICK).as_secs_f64();
        while remaining > 0.0 && !self.over {
            let step = remaining.min(STEP_SECONDS);
            self.step(step);
            remaining -= step;
        }
    }

    fn step(&mut self, seconds: f64) {
        let advance = self.speed() * seconds;
        self.distance += advance;
        self.elevation += self.velocity * seconds;
        self.velocity -= GRAVITY * seconds;
        if self.elevation <= 0.0 {
            self.elevation = 0.0;
            self.velocity = 0.0;
        }
        for cactus in &mut self.cacti {
            cactus.x -= advance;
            if cactus.x < RUNNER_X + RUNNER_WIDTH
                && cactus.x + cactus.width() > RUNNER_X
                && self.elevation < cactus.height()
            {
                self.over = true;
            }
        }
        self.cacti.retain(|cactus| cactus.x + cactus.width() >= 0.0);
        if self
            .cacti
            .last()
            .is_none_or(|cactus| cactus.x < f64::from(SIZE.0) - self.next_gap)
        {
            self.spawn();
        }
    }

    fn spawn(&mut self) {
        self.cacti.push(Cactus {
            x: f64::from(SIZE.0),
            tall: self.random.index(3) == 0,
        });
        // Even at maximum speed, a full jump can finish before the next one.
        self.next_gap = 30.0 + self.random.index(14) as f64;
    }

    pub(super) fn is_over(&self) -> bool {
        self.over
    }

    pub(super) fn draw(&self, area: Rect, buffer: &mut Buffer, theme: &Theme) {
        let area = area.intersection(buffer.area);
        if area.width < SIZE.0 || area.height < SIZE.1 {
            Paragraph::new("Enlarge terminal for Dino").render(area, buffer);
            return;
        }
        let ink = if theme.plain {
            Style::default()
        } else {
            Style::default().fg(theme.shade(0.95))
        };
        paint(
            area,
            buffer,
            0,
            0,
            &format!("DINO RUN                 SCORE {:06}", self.distance as u64),
            ink.add_modifier(Modifier::BOLD),
        );

        let dim = ink.add_modifier(Modifier::DIM);
        let drift = (self.distance * 0.18).rem_euclid(84.0) as i64;
        for origin in [15_i64, 43, 72] {
            let x = (origin - drift).rem_euclid(84) as i32 - 12;
            paint(area, buffer, x, 2, " .--. ", dim);
            paint(area, buffer, x, 3, "(____)", dim);
        }
        for x in 0..i32::from(SIZE.0) {
            let mark = if (x as i64 + self.distance.rem_euclid(9.0) as i64).rem_euclid(9) == 0 {
                "."
            } else {
                "_"
            };
            paint(area, buffer, x, GROUND, mark, dim);
        }

        let cactus_style = if theme.plain {
            ink
        } else {
            ink.bg(theme.added)
        };
        for cactus in &self.cacti {
            let x = cactus.x.round() as i32;
            if cactus.tall {
                paint(area, buffer, x, GROUND - 3, " | ", cactus_style);
                paint(area, buffer, x, GROUND - 2, "\\|/", cactus_style);
                paint(area, buffer, x, GROUND - 1, " | ", cactus_style);
            } else {
                paint(area, buffer, x, GROUND - 2, "|,", cactus_style);
                paint(area, buffer, x, GROUND - 1, "||", cactus_style);
            }
        }

        let dino_style = if self.over {
            ink.add_modifier(Modifier::BOLD | Modifier::REVERSED)
        } else {
            ink.add_modifier(Modifier::BOLD)
        };
        let top = GROUND - 4 - self.elevation.round() as i32;
        let feet = if self.elevation > 0.0 || (self.distance as u64 / 2).is_multiple_of(2) {
            " / > "
        } else {
            " < \\ "
        };
        for (row, text) in ["  __ ", " /o_)", "//|  ", feet].into_iter().enumerate() {
            paint(
                area,
                buffer,
                RUNNER_X as i32 - 1,
                top + row as i32,
                text,
                dino_style,
            );
        }
        let status = if self.over {
            "CRASH! Press r to run again."
        } else {
            "Tiny legs. Big desert. Keep hopping."
        };
        paint(area, buffer, 0, 13, status, ink);
    }
}

/// All sprites are ASCII, and each cell is clipped to the borrowed game area.
fn paint(area: Rect, buffer: &mut Buffer, x: i32, y: i32, text: &str, style: Style) {
    if y < 0 || y >= i32::from(area.height) {
        return;
    }
    for (offset, character) in text.chars().enumerate() {
        let x = x + offset as i32;
        if x < 0 || x >= i32::from(area.width) {
            continue;
        }
        if let Some(cell) = buffer.cell_mut((area.x + x as u16, area.y + y as u16)) {
            cell.set_char(character).set_style(style);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jumping_ignores_midair_repeats_and_lands_ready_for_the_next_jump() {
        let mut game = Dino::new(1);
        game.key(KeyCode::Char(' '));
        game.tick(Duration::from_millis(200));
        let velocity = game.velocity;
        let elevation = game.elevation;
        game.key(KeyCode::Up);
        assert_eq!(game.velocity, velocity);
        assert_eq!(game.elevation, elevation);
        for _ in 0..5 {
            game.tick(Duration::from_millis(200));
        }
        assert_eq!(game.elevation, 0.0);
        assert_eq!(game.velocity, 0.0);
        game.key(KeyCode::Up);
        assert_eq!(game.velocity, JUMP_SPEED);
    }

    #[test]
    fn collision_ends_the_run_but_a_well_timed_jump_clears_the_cactus() {
        let fixture = || {
            let mut game = Dino::new(2);
            game.cacti = vec![Cactus {
                x: 12.0,
                tall: true,
            }];
            game
        };
        let mut grounded = fixture();
        grounded.tick(Duration::from_millis(250));
        grounded.tick(Duration::from_millis(250));
        assert!(grounded.is_over());
        let distance = grounded.distance;
        grounded.tick(Duration::from_secs(10));
        grounded.key(KeyCode::Char(' '));
        assert_eq!(grounded.distance, distance);
        assert_eq!(grounded.velocity, 0.0);

        let mut jumping = fixture();
        jumping.key(KeyCode::Up);
        for _ in 0..4 {
            jumping.tick(Duration::from_millis(250));
        }
        assert!(!jumping.is_over());
        assert!(jumping.cacti[0].x + jumping.cacti[0].width() < RUNNER_X);
    }

    #[test]
    fn a_stall_has_bounded_work_and_fast_obstacles_cannot_tunnel() {
        let mut game = Dino::new(3);
        game.tick(Duration::from_secs(u64::MAX));
        assert!(game.distance < 4.0);
        game.distance = 10_000.0;
        game.cacti = vec![Cactus {
            x: 10.0,
            tall: false,
        }];
        game.tick(Duration::from_secs(1));
        assert!(game.is_over());
    }
}
