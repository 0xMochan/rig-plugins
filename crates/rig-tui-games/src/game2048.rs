//! A self-contained 2048 board. The TUI shell owns focus and lifecycle keys.

use rig_tui::Theme;
use rig_tui::crossterm::event::KeyCode;
use rig_tui::ratatui::buffer::Buffer;
use rig_tui::ratatui::layout::Rect;
use rig_tui::ratatui::style::{Modifier, Style};
use rig_tui::ratatui::widgets::{Paragraph, Widget};

use crate::random::Random;

pub(super) const SIZE: (u16, u16) = (31, 17);
pub(super) const HELP: &str = "Arrows / WASD / hjkl: slide";

pub(super) struct Game2048 {
    board: [[u64; 4]; 4],
    score: u64,
    won: bool,
    random: Random,
}

#[derive(Clone, Copy)]
enum Direction {
    Left,
    Right,
    Up,
    Down,
}

impl Game2048 {
    pub(super) fn new(seed: u64) -> Self {
        let mut game = Self {
            board: [[0; 4]; 4],
            score: 0,
            won: false,
            random: Random::new(seed),
        };
        game.spawn();
        game.spawn();
        game
    }

    pub(super) fn key(&mut self, key: KeyCode) {
        let direction = match key {
            KeyCode::Left | KeyCode::Char('a' | 'h') => Direction::Left,
            KeyCode::Right | KeyCode::Char('d' | 'l') => Direction::Right,
            KeyCode::Up | KeyCode::Char('w' | 'k') => Direction::Up,
            KeyCode::Down | KeyCode::Char('s' | 'j') => Direction::Down,
            _ => return,
        };
        self.slide(direction);
    }

    fn slide(&mut self, direction: Direction) {
        let before = self.board;
        for lane in 0..4 {
            let line = std::array::from_fn(|offset| {
                let (row, column) = coordinate(direction, lane, offset);
                self.board[row][column]
            });
            let (merged, points) = merge(line);
            self.score = self.score.saturating_add(points);
            for (offset, value) in merged.into_iter().enumerate() {
                let (row, column) = coordinate(direction, lane, offset);
                self.board[row][column] = value;
            }
        }
        self.won |= self.board.iter().flatten().any(|&value| value >= 2048);
        // An unchanged board does not spend randomness or create a tile.
        if self.board != before {
            self.spawn();
        }
    }

    fn spawn(&mut self) {
        let empty: Vec<_> = (0..16)
            .filter(|&index| self.board[index / 4][index % 4] == 0)
            .collect();
        if empty.is_empty() {
            return;
        }
        let index = empty[self.random.index(empty.len())];
        self.board[index / 4][index % 4] = if self.random.index(10) == 0 { 4 } else { 2 };
    }

    pub(super) fn is_over(&self) -> bool {
        for row in 0..4 {
            for column in 0..4 {
                let value = self.board[row][column];
                if value == 0
                    || (column < 3 && mergeable(value, self.board[row][column + 1]))
                    || (row < 3 && mergeable(value, self.board[row + 1][column]))
                {
                    return false;
                }
            }
        }
        true
    }

    pub(super) fn draw(&self, area: Rect, buffer: &mut Buffer, theme: &Theme) {
        let area = area.intersection(buffer.area);
        if area.width < SIZE.0 || area.height < SIZE.1 {
            Paragraph::new("Enlarge terminal for 2048").render(area, buffer);
            return;
        }
        let bright = if theme.plain {
            Style::default()
        } else {
            Style::default().fg(theme.shade(0.95))
        };
        Paragraph::new(format!("Score {:>8}", self.score))
            .style(bright.add_modifier(Modifier::BOLD))
            .render(Rect::new(area.x, area.y, SIZE.0, 1), buffer);

        for row in 0..4 {
            for column in 0..4 {
                let value = self.board[row][column];
                let tile = Rect::new(
                    area.x + column as u16 * 8,
                    area.y + 2 + row as u16 * 3,
                    7,
                    3,
                );
                let label = tile_label(value);
                if theme.plain {
                    let style = if value == 0 {
                        bright.add_modifier(Modifier::DIM)
                    } else {
                        bright.add_modifier(Modifier::BOLD)
                    };
                    Paragraph::new(format!("+-----+\n|{label:^5}|\n+-----+"))
                        .style(style)
                        .render(tile, buffer);
                } else {
                    let tint = match value {
                        0 => theme.code,
                        2 | 4 => theme.input,
                        8 | 16 => theme.note,
                        32 | 64 => theme.important,
                        128 | 256 => theme.call_running,
                        _ => theme.added_word,
                    };
                    let style = bright.bg(tint).add_modifier(if value > 0 {
                        Modifier::BOLD
                    } else {
                        Modifier::DIM
                    });
                    buffer.set_style(tile, style);
                    Paragraph::new(label)
                        .centered()
                        .style(style)
                        .render(Rect::new(tile.x, tile.y + 1, tile.width, 1), buffer);
                }
            }
        }

        let status = if self.is_over() {
            "No moves left. R: fresh board."
        } else if self.won {
            "2048! Keep going for more."
        } else {
            "Join matching tiles. Reach 2048."
        };
        Paragraph::new(status)
            .style(bright)
            .render(Rect::new(area.x, area.y + 15, SIZE.0, 2), buffer);
    }
}

fn coordinate(direction: Direction, lane: usize, offset: usize) -> (usize, usize) {
    match direction {
        Direction::Left => (lane, offset),
        Direction::Right => (lane, 3 - offset),
        Direction::Up => (offset, lane),
        Direction::Down => (3 - offset, lane),
    }
}

fn mergeable(left: u64, right: u64) -> bool {
    left != 0 && left == right && left.checked_mul(2).is_some()
}

/// Compact one directional lane. Each source tile participates in one merge.
/// The largest representable tile stays separate instead of wrapping to zero.
fn merge(line: [u64; 4]) -> ([u64; 4], u64) {
    let mut packed = [0; 4];
    let mut count = 0;
    for value in line.into_iter().filter(|&value| value != 0) {
        packed[count] = value;
        count += 1;
    }
    let mut merged = [0; 4];
    let mut source = 0;
    let mut target = 0;
    let mut score: u64 = 0;
    while source < count {
        let value = packed[source];
        if source + 1 < count && mergeable(value, packed[source + 1]) {
            let value = value * 2;
            merged[target] = value;
            score = score.saturating_add(value);
            source += 2;
        } else {
            merged[target] = value;
            source += 1;
        }
        target += 1;
    }
    (merged, score)
}

fn tile_label(value: u64) -> String {
    match value {
        0 => ".".to_owned(),
        1..=99_999 => value.to_string(),
        _ => format!("2^{}", value.trailing_zeros()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_move_compacts_and_merges_each_source_tile_once() {
        assert_eq!(merge([2, 2, 2, 2]), ([4, 4, 0, 0], 8));
        assert_eq!(merge([2, 0, 2, 4]), ([4, 4, 0, 0], 4));
        assert_eq!(merge([4, 4, 8, 0]), ([8, 8, 0, 0], 8));
        assert_eq!(
            merge([1 << 63, 1 << 63, 0, 0]),
            ([1 << 63, 1 << 63, 0, 0], 0)
        );
    }

    #[test]
    fn changing_moves_spawn_one_tile_but_noop_preserves_board_and_score() {
        let mut game = Game2048::new(42);
        game.board = [[2, 4, 0, 0], [0; 4], [0; 4], [0; 4]];
        let before = game.board;
        game.key(KeyCode::Left);
        assert_eq!(game.board, before);
        assert_eq!(game.score, 0);

        game.board[0] = [2, 2, 0, 0];
        game.key(KeyCode::Right);
        assert_eq!(game.board[0][3], 4);
        assert_eq!(
            game.board
                .iter()
                .flatten()
                .filter(|&&value| value != 0)
                .count(),
            2
        );
        assert_eq!(game.score, 4);
    }

    #[test]
    fn full_board_only_ends_without_a_safe_merge_and_winning_can_continue() {
        let mut game = Game2048::new(42);
        game.board = [[2, 4, 2, 4], [4, 2, 4, 2], [2, 4, 2, 4], [4, 2, 4, 2]];
        assert!(game.is_over());
        game.board[3][3] = 4;
        assert!(!game.is_over());
        game.board = [[1024, 1024, 0, 0], [0; 4], [0; 4], [0; 4]];
        game.key(KeyCode::Left);
        assert!(game.won);
        assert!(!game.is_over());
        assert_eq!(game.board[0][0], 2048);
        game.key(KeyCode::Right);
        assert_eq!(game.board[0][3], 2048);
    }
}
