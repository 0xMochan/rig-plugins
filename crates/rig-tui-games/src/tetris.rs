//! Falling-block rules and drawing. The panel shell owns focus and lifecycle.

use std::time::Duration;

use rig_tui::Theme;
use rig_tui::crossterm::event::KeyCode;
use rig_tui::ratatui::buffer::Buffer;
use rig_tui::ratatui::layout::Rect;
use rig_tui::ratatui::style::{Color, Modifier, Style};

use crate::random::Random;

pub(super) const SIZE: (u16, u16) = (36, 20);
pub(super) const HELP: &str = "←→ move ↑/z turn ↓/Space drop C hold";

const WIDTH: usize = 10;
const HEIGHT: usize = 20;
const CLEAR_FLASH: Duration = Duration::from_millis(100);
const CLEAR_DURATION: Duration = Duration::from_millis(300);
const LANDING_DURATION: Duration = Duration::from_millis(140);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    I,
    O,
    T,
    S,
    Z,
    J,
    L,
}

impl Kind {
    const ALL: [Self; 7] = [
        Self::I,
        Self::O,
        Self::T,
        Self::S,
        Self::Z,
        Self::J,
        Self::L,
    ];

    fn cells(self, rotation: u8) -> [(i8, i8); 4] {
        let mut cells = match self {
            Self::I => [(0, 1), (1, 1), (2, 1), (3, 1)],
            Self::O => return [(1, 0), (2, 0), (1, 1), (2, 1)],
            Self::T => [(1, 0), (0, 1), (1, 1), (2, 1)],
            Self::S => [(1, 0), (2, 0), (0, 1), (1, 1)],
            Self::Z => [(0, 0), (1, 0), (1, 1), (2, 1)],
            Self::J => [(0, 0), (0, 1), (1, 1), (2, 1)],
            Self::L => [(2, 0), (0, 1), (1, 1), (2, 1)],
        };
        let edge = if self == Self::I { 3 } else { 2 };
        for _ in 0..rotation % 4 {
            for (x, y) in &mut cells {
                (*x, *y) = (edge - *y, *x);
            }
        }
        cells
    }

    fn style(self, theme: &Theme) -> Style {
        if theme.plain {
            return Style::default().add_modifier(Modifier::BOLD);
        }
        let color = match self {
            Self::I => Color::Cyan,
            Self::O => Color::Yellow,
            Self::T => Color::Magenta,
            Self::S => Color::Green,
            Self::Z => Color::Red,
            Self::J => Color::Blue,
            Self::L => Color::LightRed,
        };
        Style::default().fg(Color::Black).bg(color)
    }
}

#[derive(Clone, Copy)]
struct Piece {
    kind: Kind,
    rotation: u8,
    x: i8,
    y: i8,
}

impl Piece {
    fn new(kind: Kind) -> Self {
        Self {
            kind,
            rotation: 0,
            x: 3,
            y: 0,
        }
    }

    fn cells(self) -> [(i8, i8); 4] {
        self.kind
            .cells(self.rotation)
            .map(|(x, y)| (x + self.x, y + self.y))
    }
}

/// Keeps the locked rows intact while their presentation finishes. No new
/// piece or gravity work can run until this owned transition is consumed.
struct Clear {
    rows: [bool; HEIGHT],
    elapsed: Duration,
}

struct Landing {
    cells: [(i8, i8); 4],
    remaining: Duration,
}

pub(super) struct Tetris {
    board: [[Option<Kind>; WIDTH]; HEIGHT],
    falling: Piece,
    next: Kind,
    held: Option<Kind>,
    hold_used: bool,
    clearing: Option<Clear>,
    landing: Option<Landing>,
    bag: [Kind; 7],
    bag_at: usize,
    random: Random,
    score: u64,
    lines: u32,
    elapsed: Duration,
    over: bool,
}

impl Tetris {
    pub(super) fn new(seed: u64) -> Self {
        let mut game = Self {
            board: [[None; WIDTH]; HEIGHT],
            falling: Piece::new(Kind::I),
            next: Kind::I,
            held: None,
            hold_used: false,
            clearing: None,
            landing: None,
            bag: Kind::ALL,
            bag_at: 7,
            random: Random::new(seed),
            score: 0,
            lines: 0,
            elapsed: Duration::ZERO,
            over: false,
        };
        game.falling = Piece::new(game.take_piece());
        game.next = game.take_piece();
        game
    }

    fn take_piece(&mut self) -> Kind {
        if self.bag_at == self.bag.len() {
            self.bag = Kind::ALL;
            for at in (1..self.bag.len()).rev() {
                let other = self.random.index(at + 1);
                self.bag.swap(at, other);
            }
            self.bag_at = 0;
        }
        let piece = self.bag[self.bag_at];
        self.bag_at += 1;
        piece
    }

    fn fits(&self, piece: Piece) -> bool {
        piece.cells().into_iter().all(|(x, y)| {
            x >= 0
                && x < WIDTH as i8
                && y < HEIGHT as i8
                && (y < 0 || self.board[y as usize][x as usize].is_none())
        })
    }

    fn move_by(&mut self, dx: i8, dy: i8) -> bool {
        let moved = Piece {
            x: self.falling.x + dx,
            y: self.falling.y + dy,
            ..self.falling
        };
        if self.fits(moved) {
            self.falling = moved;
            true
        } else {
            false
        }
    }

    fn rotate(&mut self, clockwise: bool) {
        let rotation = (self.falling.rotation + if clockwise { 1 } else { 3 }) % 4;
        // Small horizontal kicks and one upward kick keep rotation usable by
        // walls and the floor. This deliberately is not the full SRS table.
        for (dx, dy) in [(0, 0), (-1, 0), (1, 0), (-2, 0), (2, 0), (0, -1), (0, -2)] {
            let moved = Piece {
                rotation,
                x: self.falling.x + dx,
                y: self.falling.y + dy,
                ..self.falling
            };
            if self.fits(moved) {
                self.falling = moved;
                return;
            }
        }
    }

    pub(super) fn key(&mut self, key: KeyCode) {
        if self.over || self.clearing.is_some() {
            return;
        }
        match key {
            KeyCode::Left | KeyCode::Char('a') => {
                self.move_by(-1, 0);
            }
            KeyCode::Right | KeyCode::Char('d') => {
                self.move_by(1, 0);
            }
            KeyCode::Up | KeyCode::Char('x') | KeyCode::Char('w') => self.rotate(true),
            KeyCode::Char('z') => self.rotate(false),
            KeyCode::Char('c' | 'C') => self.hold(),
            KeyCode::Down | KeyCode::Char('s') => {
                if self.move_by(0, 1) {
                    self.score = self.score.saturating_add(1);
                } else {
                    self.lock();
                }
            }
            KeyCode::Char(' ') => {
                while self.move_by(0, 1) {
                    self.score = self.score.saturating_add(2);
                }
                self.lock();
            }
            _ => {}
        }
    }

    pub(super) fn tick(&mut self, elapsed: Duration) {
        let elapsed = elapsed.min(Duration::from_millis(250));
        if let Some(landing) = &mut self.landing {
            landing.remaining = landing.remaining.saturating_sub(elapsed);
            if landing.remaining.is_zero() {
                self.landing = None;
            }
        }
        if self.over {
            return;
        }
        if let Some(clear) = &mut self.clearing {
            clear.elapsed += elapsed;
            if clear.elapsed >= CLEAR_DURATION {
                let rows = self.clearing.take().expect("active clear").rows;
                let cleared = rows.iter().filter(|&&full| full).count();
                let level = u64::from(self.lines / 10 + 1);
                self.clear_rows(&rows);
                self.lines = self.lines.saturating_add(cleared as u32);
                self.score = self
                    .score
                    .saturating_add([0, 100, 300, 500, 800][cleared] * level);
                self.spawn_next();
            }
            // Discard this frame's remainder: the new piece starts with a
            // fresh gravity interval, even after a delayed animation frame.
            return;
        }
        // Bound catch-up after a stalled terminal rather than instantly
        // dropping several pieces when the app becomes responsive again.
        self.elapsed += elapsed;
        let gravity = Duration::from_millis(
            800_u64
                .saturating_sub(u64::from(self.lines / 10) * 60)
                .max(100),
        );
        while self.elapsed >= gravity {
            self.elapsed -= gravity;
            if !self.move_by(0, 1) {
                self.lock();
                break;
            }
        }
    }

    fn lock(&mut self) {
        let cells = self.falling.cells();
        if cells.iter().any(|&(_, y)| y < 0) {
            self.over = true;
            self.landing = None;
            return;
        }
        for (x, y) in cells {
            self.board[y as usize][x as usize] = Some(self.falling.kind);
        }
        self.elapsed = Duration::ZERO;
        let rows = self.board.map(|row| row.iter().all(Option::is_some));
        if rows.iter().any(|&full| full) {
            self.clearing = Some(Clear {
                rows,
                elapsed: Duration::ZERO,
            });
            self.landing = None;
        } else {
            self.landing = Some(Landing {
                cells,
                remaining: LANDING_DURATION,
            });
            self.spawn_next();
        }
    }

    fn spawn_next(&mut self) {
        self.falling = Piece::new(self.next);
        self.next = self.take_piece();
        self.hold_used = false;
        self.elapsed = Duration::ZERO;
        self.over = !self.fits(self.falling);
        if self.over {
            self.landing = None;
        }
    }

    fn hold(&mut self) {
        if self.hold_used {
            return;
        }
        let outgoing = self.falling.kind;
        self.falling = Piece::new(if let Some(held) = self.held.replace(outgoing) {
            held
        } else {
            let next = self.next;
            self.next = self.take_piece();
            next
        });
        self.hold_used = true;
        self.elapsed = Duration::ZERO;
        self.over = !self.fits(self.falling);
        if self.over {
            self.landing = None;
        }
    }

    fn clear_rows(&mut self, rows: &[bool; HEIGHT]) {
        let mut destination = HEIGHT;
        for source in (0..HEIGHT).rev() {
            if !rows[source] {
                destination -= 1;
                self.board[destination] = self.board[source];
            }
        }
        self.board[..destination].fill([None; WIDTH]);
    }

    pub(super) fn is_over(&self) -> bool {
        self.over
    }

    pub(super) fn draw(&self, area: Rect, buffer: &mut Buffer, theme: &Theme) {
        if area.width < SIZE.0 || area.height < SIZE.1 {
            text(
                buffer,
                area,
                0,
                0,
                "Tetris needs 36 × 20 cells",
                Style::default(),
            );
            return;
        }
        let dim = if theme.plain {
            Style::default().add_modifier(Modifier::DIM)
        } else {
            Style::default().fg(theme.shade(0.35))
        };
        let show_falling = !self.over && self.clearing.is_none();
        let falling = self.falling.cells();
        let ghost = show_falling.then(|| {
            let mut ghost = self.falling;
            while self.fits(Piece {
                y: ghost.y + 1,
                ..ghost
            }) {
                ghost.y += 1;
            }
            ghost.cells()
        });
        for y in 0..HEIGHT {
            text(buffer, area, 0, y as u16, "|", dim);
            text(buffer, area, 21, y as u16, "|", dim);
            for x in 0..WIDTH {
                let position = (x as i8, y as i8);
                let kind = if show_falling && falling.contains(&position) {
                    Some(self.falling.kind)
                } else {
                    self.board[y][x]
                };
                let (mut symbol, mut style) = if let Some(kind) = kind {
                    ("[]", kind.style(theme))
                } else if ghost.is_some_and(|cells| cells.contains(&position)) {
                    ("..", dim)
                } else {
                    ("  ", Style::default())
                };
                if let Some(clear) = &self.clearing {
                    if clear.rows[y] {
                        if clear.elapsed < CLEAR_FLASH {
                            symbol = "==";
                            style =
                                Style::default().add_modifier(Modifier::BOLD | Modifier::REVERSED);
                            if !theme.plain {
                                style = style
                                    .remove_modifier(Modifier::REVERSED)
                                    .fg(theme.shade(0.0))
                                    .bg(theme.shade(0.95));
                            }
                        } else {
                            let erased = (((clear.elapsed - CLEAR_FLASH).as_millis() * 5
                                / (CLEAR_DURATION - CLEAR_FLASH).as_millis())
                                as usize
                                + 1)
                            .min(5);
                            if (WIDTH / 2 - erased..WIDTH / 2 + erased).contains(&x) {
                                symbol = "  ";
                                style = Style::default();
                            }
                        }
                    }
                } else if self
                    .landing
                    .as_ref()
                    .is_some_and(|landing| landing.cells.contains(&position))
                {
                    style = style.add_modifier(Modifier::REVERSED);
                }
                text(buffer, area, 1 + x as u16 * 2, y as u16, symbol, style);
            }
        }
        let bold = Style::default().add_modifier(Modifier::BOLD);
        for (y, label, value) in [
            (0, "SCORE", self.score.to_string()),
            (3, "LINES", self.lines.to_string()),
            (6, "LEVEL", (self.lines / 10 + 1).to_string()),
        ] {
            text(buffer, area, 24, y, label, dim);
            text(buffer, area, 24, y + 1, &value, bold);
        }
        text(
            buffer,
            area,
            24,
            9,
            if self.hold_used {
                "HOLD (used)"
            } else {
                "HOLD [C]"
            },
            dim,
        );
        text(buffer, area, 24, 14, "NEXT", dim);
        for (top, kind) in [(11, self.held), (16, Some(self.next))] {
            if let Some(kind) = kind {
                for (x, y) in kind.cells(0) {
                    text(
                        buffer,
                        area,
                        24 + x as u16 * 2,
                        top + y as u16,
                        "[]",
                        kind.style(theme),
                    );
                }
            } else {
                text(buffer, area, 24, top, "empty", dim);
            }
        }
        if self.over {
            text(buffer, area, 24, 18, "GAME OVER", bold);
            text(buffer, area, 24, 19, "r to retry", dim);
        } else if self.clearing.is_some() {
            text(buffer, area, 24, 19, "LINE CLEAR!", bold);
        }
    }
}

/// Each write is bounded by both the content rectangle and the backing canvas.
fn text(buffer: &mut Buffer, area: Rect, x: u16, y: u16, value: &str, style: Style) {
    if x >= area.width || y >= area.height {
        return;
    }
    let row = area.y.saturating_add(y);
    for (offset, ch) in value.chars().take(usize::from(area.width - x)).enumerate() {
        let column = area.x.saturating_add(x).saturating_add(offset as u16);
        if let Some(cell) = buffer.cell_mut((column, row)) {
            cell.set_char(ch).set_style(style);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clearing_flashes_erases_then_compacts_and_scores_once() {
        let mut game = Tetris::new(7);
        game.board[19] = [Some(Kind::J); WIDTH];
        game.board[19][4] = None;
        game.board[19][5] = None;
        game.board[18][0] = Some(Kind::S);
        game.falling = Piece {
            kind: Kind::O,
            x: 3,
            y: 18,
            rotation: 0,
        };
        let preview = game.next;
        game.lock();
        assert_eq!(game.lines, 0);
        assert_eq!(game.score, 0);
        assert_eq!(game.next, preview);
        assert!(game.board[19].iter().all(Option::is_some));
        assert_eq!(game.board[18][0], Some(Kind::S));

        game.key(KeyCode::Char(' '));
        game.key(KeyCode::Char('C'));
        assert_eq!(game.score, 0);
        assert!(game.held.is_none());
        assert_eq!(game.falling.y, 18);

        let area = Rect::new(0, 0, SIZE.0, SIZE.1);
        let mut flash = Buffer::empty(area);
        game.tick(Duration::from_millis(90));
        game.draw(area, &mut flash, &Theme::default().without_color());
        let flash_cell = &flash[(1, 19)];
        assert_eq!(flash_cell.symbol(), "=");
        assert!(
            flash_cell
                .modifier
                .contains(Modifier::REVERSED | Modifier::BOLD)
        );
        assert_eq!(flash_cell.fg, Color::Reset);
        assert_eq!(flash_cell.bg, Color::Reset);
        // No stale ghost or next falling piece appears above the locked rows.
        assert_eq!(flash[(9, 0)].symbol(), " ");

        game.tick(Duration::from_millis(100));
        let mut erased = Buffer::empty(area);
        game.draw(area, &mut erased, &Theme::default());
        assert_eq!(erased[(1, 19)].symbol(), "[");
        assert_eq!(erased[(9, 19)].symbol(), " ");
        assert_eq!(erased[(11, 19)].symbol(), " ");
        assert_eq!(erased[(1, 18)].symbol(), "[");
        assert!(game.board[19].iter().all(Option::is_some));
        assert_eq!(game.lines, 0);
        assert_eq!(game.score, 0);

        game.tick(Duration::from_millis(110));
        assert_eq!(game.lines, 1);
        assert_eq!(game.score, 100);
        assert_eq!(game.board[19][0], Some(Kind::S));
        assert_eq!(game.board[19][4], Some(Kind::O));
        assert_eq!(game.board[18], [None; WIDTH]);
        assert_eq!(game.falling.kind, preview);
        assert_eq!(game.falling.y, 0);
        assert_eq!(game.elapsed, Duration::ZERO);
        assert!(game.clearing.is_none());
        assert!(!game.hold_used);
        assert!(!game.over);
        game.tick(Duration::from_millis(250));
        assert_eq!(game.lines, 1);
        assert_eq!(game.score, 100);
        assert_eq!(game.falling.y, 0);

        let mut compacted = Buffer::empty(area);
        game.draw(area, &mut compacted, &Theme::default());
        assert_eq!(compacted[(1, 19)].symbol(), "[");
        assert_eq!(compacted[(1, 18)].symbol(), " ");
    }

    #[test]
    fn hold_consumes_the_preview_once_then_swaps_fresh_pieces_after_lock() {
        let mut game = Tetris::new(9);
        game.falling = Piece {
            kind: Kind::I,
            rotation: 1,
            x: 0,
            y: 5,
        };
        game.next = Kind::O;
        game.elapsed = Duration::from_millis(700);
        let bag_at = game.bag_at;
        game.key(KeyCode::Char('C'));
        assert_eq!(game.held, Some(Kind::I));
        assert_eq!(game.falling.kind, Kind::O);
        assert_eq!(game.falling.rotation, 0);
        assert_eq!((game.falling.x, game.falling.y), (3, 0));
        assert_eq!(game.bag_at, bag_at + 1);
        assert!(game.hold_used);
        let preview = game.next;
        game.key(KeyCode::Char('c'));
        assert_eq!(game.held, Some(Kind::I));
        assert_eq!(game.falling.kind, Kind::O);
        assert_eq!(game.next, preview);
        game.tick(Duration::from_millis(100));
        assert_eq!(game.falling.y, 0, "holding resets accumulated gravity");

        game.key(KeyCode::Char(' '));
        assert!(!game.hold_used);
        assert!(game.landing.is_some());
        let landed = game.landing.as_ref().unwrap().cells[0];
        let area = Rect::new(0, 0, SIZE.0, SIZE.1);
        let mut landing_frame = Buffer::empty(area);
        game.draw(area, &mut landing_frame, &Theme::default().without_color());
        let cell = &landing_frame[(1 + landed.0 as u16 * 2, landed.1 as u16)];
        assert!(cell.modifier.contains(Modifier::REVERSED));
        game.tick(LANDING_DURATION);
        assert!(game.landing.is_none());

        let outgoing = game.falling.kind;
        let preview = game.next;
        let bag_at = game.bag_at;
        game.key(KeyCode::Char('c'));
        assert_eq!(game.held, Some(outgoing));
        assert_eq!(game.falling.kind, Kind::I);
        assert_eq!(game.falling.rotation, 0);
        assert_eq!((game.falling.x, game.falling.y), (3, 0));
        assert_eq!(game.next, preview, "a swap preserves the preview");
        assert_eq!(game.bag_at, bag_at);

        game.key(KeyCode::Char(' '));
        assert!(!game.hold_used);
        game.held = Some(Kind::O);
        game.board[0][4] = Some(Kind::J);
        game.key(KeyCode::Char('c'));
        assert!(game.is_over());
        assert!(
            game.landing.is_none(),
            "game-over cannot retain a timed highlight"
        );
        assert_eq!(game.elapsed, Duration::ZERO);
    }

    #[test]
    fn wall_kicks_keep_a_piece_legal_and_blocked_spawn_ends_play() {
        let mut game = Tetris::new(12);
        game.falling = Piece {
            kind: Kind::I,
            x: -2,
            y: 4,
            rotation: 1,
        };
        assert!(game.fits(game.falling));
        game.rotate(true);
        assert_eq!(game.falling.rotation, 2);
        assert!(game.fits(game.falling));
        assert_eq!(game.falling.x, 0);
        game.next = Kind::O;
        game.board[0][4] = Some(Kind::J);
        game.falling = Piece {
            kind: Kind::O,
            x: 6,
            y: 18,
            rotation: 0,
        };
        game.lock();
        assert!(game.is_over());
        assert!(game.landing.is_none());
        let score = game.score;
        game.key(KeyCode::Char(' '));
        game.tick(Duration::from_secs(2));
        assert_eq!(game.score, score);
    }

    #[test]
    fn every_refilled_bag_contains_each_kind_once() {
        let mut game = Tetris::new(42);
        game.bag_at = 7;
        for _ in 0..3 {
            let dealt: Vec<_> = (0..7).map(|_| game.take_piece()).collect();
            for kind in Kind::ALL {
                assert_eq!(dealt.iter().filter(|&&piece| piece == kind).count(), 1);
            }
        }
    }
}
