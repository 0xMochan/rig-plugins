//! A fixed-clue Sudoku puzzle with local edits and conflict feedback.

use rig_tui::Theme;
use rig_tui::crossterm::event::KeyCode;
use rig_tui::ratatui::buffer::Buffer;
use rig_tui::ratatui::layout::Rect;
use rig_tui::ratatui::style::{Color, Modifier, Style};
use rig_tui::ratatui::text::{Line, Span};
use rig_tui::ratatui::widgets::{Paragraph, Widget};

use crate::random::Random;

pub(super) const SIZE: (u16, u16) = (31, 16);
pub(super) const HELP: &str = "Arrows/hjkl | 1-9 | 0/Del clear";

type Board = [[u8; 9]; 9];

// This classic puzzle has one solution. Digit relabeling, transposition, and
// legal row/column permutations preserve it without an interactive generator.
const PUZZLE: Board = [
    [5, 3, 0, 0, 7, 0, 0, 0, 0],
    [6, 0, 0, 1, 9, 5, 0, 0, 0],
    [0, 9, 8, 0, 0, 0, 0, 6, 0],
    [8, 0, 0, 0, 6, 0, 0, 0, 3],
    [4, 0, 0, 8, 0, 3, 0, 0, 1],
    [7, 0, 0, 0, 2, 0, 0, 0, 6],
    [0, 6, 0, 0, 0, 0, 2, 8, 0],
    [0, 0, 0, 4, 1, 9, 0, 0, 5],
    [0, 0, 0, 0, 8, 0, 0, 7, 9],
];

pub(super) struct Sudoku {
    clues: Board,
    cells: Board,
    selected: (usize, usize),
}

impl Sudoku {
    pub(super) fn new(seed: u64) -> Self {
        let mut random = Random::new(seed);
        let mut digits = [1, 2, 3, 4, 5, 6, 7, 8, 9];
        shuffle(&mut digits, &mut random);
        let rows = groups(&mut random);
        let columns = groups(&mut random);
        let transpose = random.index(2) == 0;
        let mut clues = [[0; 9]; 9];
        for (row, cells) in clues.iter_mut().enumerate() {
            for (column, cell) in cells.iter_mut().enumerate() {
                let source = if transpose {
                    PUZZLE[columns[column]][rows[row]]
                } else {
                    PUZZLE[rows[row]][columns[column]]
                };
                if source != 0 {
                    *cell = digits[usize::from(source - 1)];
                }
            }
        }
        let selected = clues
            .iter()
            .enumerate()
            .find_map(|(row, cells)| {
                cells
                    .iter()
                    .position(|cell| *cell == 0)
                    .map(|column| (row, column))
            })
            .unwrap_or((0, 0));
        Self {
            clues,
            cells: clues,
            selected,
        }
    }

    pub(super) fn key(&mut self, key: KeyCode) {
        if self.is_over() {
            return;
        }
        let (row, column) = self.selected;
        match key {
            KeyCode::Up | KeyCode::Char('k') => self.selected.0 = row.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => self.selected.0 = (row + 1).min(8),
            KeyCode::Left | KeyCode::Char('h') => self.selected.1 = column.saturating_sub(1),
            KeyCode::Right | KeyCode::Char('l') => self.selected.1 = (column + 1).min(8),
            _ if self.clues[row][column] != 0 => {}
            KeyCode::Char(digit @ '1'..='9') => self.cells[row][column] = digit as u8 - b'0',
            KeyCode::Backspace | KeyCode::Delete | KeyCode::Char('0') => {
                self.cells[row][column] = 0;
            }
            _ => {}
        }
    }

    pub(super) fn is_over(&self) -> bool {
        self.cells.iter().flatten().all(|cell| *cell != 0)
            && (0..9).all(|row| (0..9).all(|column| !conflict(&self.cells, row, column)))
    }

    pub(super) fn draw(&self, area: Rect, buffer: &mut Buffer, theme: &Theme) {
        let mut lines = Vec::with_capacity(usize::from(SIZE.1));
        let grid_style = if theme.plain {
            Style::default().add_modifier(Modifier::DIM)
        } else {
            Style::default().fg(theme.shade(0.4))
        };
        let (selected_row, selected_column) = self.selected;
        let selected_value = self.cells[selected_row][selected_column];
        let mut conflicts = 0;
        for row in 0..9 {
            if row % 3 == 0 {
                lines.push(Line::styled("+---------+---------+---------+", grid_style));
            }
            let mut spans = vec![Span::styled("|", grid_style)];
            for column in 0..9 {
                let value = self.cells[row][column];
                let conflicting = conflict(&self.cells, row, column);
                conflicts += usize::from(conflicting);
                let mut style = if theme.plain {
                    Style::default()
                } else {
                    Style::default().fg(theme.shade(0.9))
                };
                if self.clues[row][column] != 0 {
                    style = style.add_modifier(Modifier::BOLD);
                }
                if selected_value != 0 && value == selected_value {
                    style = style.add_modifier(Modifier::UNDERLINED);
                }
                if conflicting {
                    style = style.add_modifier(Modifier::BOLD);
                    if !theme.plain {
                        style = style.fg(Color::Red);
                    }
                }
                if (row, column) == self.selected {
                    style = style.patch(theme.selection());
                }
                let digit = if value == 0 {
                    '.'
                } else {
                    char::from(b'0' + value)
                };
                let marker = if conflicting { '!' } else { ' ' };
                spans.push(Span::styled(format!("{marker}{digit} "), style));
                if column % 3 == 2 {
                    spans.push(Span::styled("|", grid_style));
                }
            }
            lines.push(Line::from(spans));
        }
        lines.push(Line::styled("+---------+---------+---------+", grid_style));
        lines.push(Line::default());
        let filled = self
            .cells
            .iter()
            .flatten()
            .filter(|cell| **cell != 0)
            .count();
        if self.is_over() {
            lines.push(Line::from("Solved! Nicely done."));
            lines.push(Line::from("r starts another puzzle"));
        } else {
            lines.push(Line::from(format!(
                "Filled {filled}/81 | {conflicts} conflicts"
            )));
            let kind = if self.clues[selected_row][selected_column] == 0 {
                "entry"
            } else {
                "clue"
            };
            lines.push(Line::from(format!(
                "Row {}, col {} | {kind}",
                selected_row + 1,
                selected_column + 1
            )));
        }
        Paragraph::new(lines).render(area, buffer);
    }
}

fn shuffle<T>(items: &mut [T], random: &mut Random) {
    for end in (1..items.len()).rev() {
        items.swap(end, random.index(end + 1));
    }
}

fn groups(random: &mut Random) -> [usize; 9] {
    let mut bands = [0, 1, 2];
    shuffle(&mut bands, random);
    let mut indices = [0; 9];
    for (at, band) in bands.into_iter().enumerate() {
        let mut within = [0, 1, 2];
        shuffle(&mut within, random);
        for (offset, index) in within.into_iter().enumerate() {
            indices[at * 3 + offset] = band * 3 + index;
        }
    }
    indices
}

fn conflict(board: &Board, row: usize, column: usize) -> bool {
    let value = board[row][column];
    value != 0
        && ((0..9).any(|other| other != column && board[row][other] == value)
            || (0..9).any(|other| other != row && board[other][column] == value)
            || (row / 3 * 3..row / 3 * 3 + 3).any(|other_row| {
                (column / 3 * 3..column / 3 * 3 + 3).any(|other_column| {
                    (other_row, other_column) != (row, column)
                        && board[other_row][other_column] == value
                })
            }))
}

#[cfg(test)]
mod tests {
    use super::*;

    // Count at most two solutions: the curated puzzle and transformations must
    // yield a solvable, unambiguous board rather than merely valid-looking clues.
    fn solutions(board: &mut Board, first: &mut Option<Board>) -> usize {
        let mut best = None;
        for row in 0..9 {
            for column in 0..9 {
                if board[row][column] != 0 {
                    continue;
                }
                let candidates = (1..=9)
                    .filter(|value| {
                        board[row][column] = *value;
                        let allowed = !conflict(board, row, column);
                        board[row][column] = 0;
                        allowed
                    })
                    .collect::<Vec<_>>();
                if candidates.is_empty() {
                    return 0;
                }
                if best
                    .as_ref()
                    .is_none_or(|(_, _, prior): &(usize, usize, Vec<u8>)| {
                        candidates.len() < prior.len()
                    })
                {
                    best = Some((row, column, candidates));
                }
            }
        }
        let Some((row, column, candidates)) = best else {
            first.get_or_insert(*board);
            return 1;
        };
        let mut total = 0;
        for value in candidates {
            board[row][column] = value;
            total += solutions(board, first);
            if total > 1 {
                break;
            }
        }
        board[row][column] = 0;
        total.min(2)
    }

    #[test]
    fn transformed_puzzle_has_one_solution_and_completes_through_edits() {
        let mut game = Sudoku::new(42);
        let mut solved = None;
        let mut clues = game.clues;
        assert_eq!(solutions(&mut clues, &mut solved), 1);
        assert!(!game.is_over());
        let solved = solved.expect("unique solution");
        for (row, values) in solved.iter().enumerate() {
            for (column, value) in values.iter().enumerate() {
                if game.clues[row][column] == 0 {
                    game.selected = (row, column);
                    game.key(KeyCode::Char(char::from(b'0' + *value)));
                }
            }
        }
        assert!(game.is_over());
        assert_eq!(game.cells, solved);
    }

    #[test]
    fn clues_are_fixed_and_conflicting_entries_can_be_cleared() {
        let mut game = Sudoku {
            clues: PUZZLE,
            cells: PUZZLE,
            selected: (0, 0),
        };
        game.key(KeyCode::Char('1'));
        game.key(KeyCode::Delete);
        assert_eq!(game.cells[0][0], 5);
        game.key(KeyCode::Right);
        game.key(KeyCode::Right);
        game.key(KeyCode::Char('5'));
        assert!(conflict(&game.cells, 0, 0));
        assert!(conflict(&game.cells, 0, 2));
        game.key(KeyCode::Backspace);
        assert_eq!(game.cells[0][2], 0);
        assert!(!conflict(&game.cells, 0, 0));
        assert!(!game.is_over());
    }
}
