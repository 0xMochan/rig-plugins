//! Light the real welcome mark after its pane draws, before composition.
//!
//! Identity comes from the welcome aside, never a text search of arbitrary
//! messages. The public snapshot supplies actual wrapped rows and viewport
//! coordinates. Installing it copies all retained transcript text each layout;
//! reuse another plugin's snapshot and keep our scan inside the welcome aside.

use std::ops::Range;

use bevy_ecs::prelude::*;
use rig_basics::welcome::WELCOME;
use rig_harness::prelude::Conversation;
use rig_tui::{DrawnTranscript, MainPane, PanelCanvas, Theme, TranscriptItem};

use crate::{LoadingLine, LoadingSettings, render};

/// Whether this frame painted an actively animated native logo in the pane.
#[derive(Resource, Default)]
pub(super) struct VisibleLogo(pub(super) bool);

pub(super) fn track(
    panes: Query<Entity, (With<MainPane>, Without<DrawnTranscript>)>,
    mut commands: Commands,
) {
    for pane in &panes {
        commands.entity(pane).insert(DrawnTranscript::default());
    }
}

// Published Rig 0.46.0's full mark, including its leading padding. Native
// metadata starts after these columns and is never restyled by this plugin.
const PREFIXES: [&str; 3] = [
    " █▙ █▙ █▙  ▟█ ▟█ ▟█",
    " ██ ██ ██  ██ ██ ██",
    " █▛ █▛ █▛  ▜█ ▜█ ▜█",
];
const PANELS: [(u16, usize); 6] = [(1, 0), (4, 1), (7, 2), (11, 2), (14, 1), (17, 0)];
const WIDTH: u16 = 19;

fn logo_rows(drawn: &DrawnTranscript, range: Range<usize>) -> Option<[(usize, &'static str); 3]> {
    let mut logical = drawn
        .rows()
        .get(range.clone())?
        .iter()
        .enumerate()
        .filter(|(_, row)| row.continues.is_none() && !row.text.trim().is_empty());
    let mut found = [(0, ""); 3];
    for (slot, prefix) in found.iter_mut().zip(PREFIXES) {
        let (index, row) = logical.next()?;
        if !row.text.starts_with(prefix) {
            // Another renderer changed the welcome, or its mark itself is
            // horizontally wrapped. Leave that rendering completely alone.
            return None;
        }
        *slot = (range.start + index, prefix);
    }
    Some(found)
}

pub(super) fn paint(
    lines: Query<&LoadingLine>,
    conversations: Query<&Conversation>,
    mut panes: Query<(&DrawnTranscript, &mut PanelCanvas), With<MainPane>>,
    settings: Res<LoadingSettings>,
    theme: Res<Theme>,
    mut visible: ResMut<VisibleLogo>,
) {
    visible.0 = false;
    if !settings.animate {
        return;
    }
    let Some(run) = lines
        .iter()
        .find_map(|line| line.0.as_ref())
        .filter(|run| !run.waiting)
    else {
        return;
    };
    for (drawn, mut canvas) in &mut panes {
        if drawn.agent() != Some(run.agent) || canvas.area().is_empty() {
            continue;
        }
        let Ok(conversation) = conversations.get(run.agent) else {
            continue;
        };
        let area = drawn.area();
        if area.width < WIDTH {
            continue;
        }
        let on_screen = drawn.on_screen();
        let Some(range) = drawn.items().iter().find_map(|(item, range)| {
            let TranscriptItem::Aside(id) = item else {
                return None;
            };
            conversation
                .aside(*id)
                .is_some_and(|aside| aside.kind == WELCOME)
                .then_some(range)
        }) else {
            continue;
        };
        if range.end <= on_screen.start || range.start >= on_screen.end {
            continue;
        }
        let Some(rows) = logo_rows(drawn, range.clone()) else {
            continue;
        };
        let buffer = canvas.bypass_change_detection().buffer_mut();
        for (index, prefix) in rows {
            if !on_screen.contains(&index) {
                continue;
            }
            let Ok(offset) = u16::try_from(index - on_screen.start) else {
                continue;
            };
            let Some(y) = area.y.checked_add(offset) else {
                continue;
            };
            if !prefix.chars().enumerate().all(|(column, expected)| {
                let Some(x) = area.x.checked_add(column as u16) else {
                    return false;
                };
                buffer.cell((x, y)).is_some_and(|cell| {
                    let mut actual = cell.symbol().chars();
                    actual.next() == Some(expected) && actual.next().is_none()
                })
            }) {
                continue;
            }
            for (column, depth) in PANELS {
                let style = render::pulse_style(&theme, run.phase, depth, true);
                for inside in 0..2 {
                    let Some(x) = area.x.checked_add(column + inside) else {
                        continue;
                    };
                    if let Some(cell) = buffer.cell_mut((x, y)) {
                        // Patch only the pulse's foreground/emphasis;
                        // retain text, backgrounds and selection styling.
                        cell.set_style(style);
                        visible.0 = true;
                    }
                }
            }
        }
    }
}
