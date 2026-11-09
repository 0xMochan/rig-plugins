//! A traveling six-bar wave with the native logo's three-plus-three spacing.

use bevy_ecs::prelude::*;
use rig_tui::ratatui::layout::{Constraint, Layout};
use rig_tui::ratatui::style::{Modifier, Style};
use rig_tui::ratatui::text::{Line, Span};
use rig_tui::ratatui::widgets::Widget;
use rig_tui::{PanelCanvas, Theme};

use crate::{LoadingLine, LoadingSettings};

// A low crest travels right through six bars, leaving the logo's center gap
// open. The repeated trough softens the loop without a blank or closed frame.
const WAVE: [usize; 12] = [1, 1, 2, 3, 4, 5, 6, 5, 4, 3, 2, 1];
const BARS: [char; 6] = ['▁', '▂', '▃', '▄', '▅', '▆'];

const LIGHT: [f32; 12] = [
    0.95, 0.88, 0.76, 0.60, 0.46, 0.35, 0.28, 0.30, 0.36, 0.46, 0.60, 0.78,
];

/// The welcome logo retains its mirrored light wave across all three rows.
pub(super) fn pulse_style(theme: &Theme, phase: usize, depth: usize, animate: bool) -> Style {
    let light = if animate {
        LIGHT[(phase + (2 - depth) * 4) % LIGHT.len()]
    } else {
        [0.6, 0.72, 0.85][depth]
    };
    light_style(theme, light)
}

fn light_style(theme: &Theme, light: f32) -> Style {
    if theme.plain {
        // Do not add/remove bold: selection and the native banner own it.
        let style = Style::default().remove_modifier(Modifier::DIM);
        if light > 0.65 {
            style
        } else {
            style.add_modifier(Modifier::DIM)
        }
    } else {
        Style::default().fg(theme.shade(light))
    }
}

fn mark(theme: &Theme, phase: usize, animate: bool, compact: bool) -> Line<'static> {
    let bars = if compact { 3 } else { 6 };
    let mut spans = Vec::with_capacity(7);
    for at in 0..bars {
        if !compact && at == 3 {
            spans.push(Span::raw(" "));
        }
        let height = if animate {
            // Sampling the same cycle at equal offsets keeps even the narrow
            // variant moving, including when terminal colors are disabled.
            WAVE[(phase + (bars - at - 1) * (WAVE.len() / bars)) % WAVE.len()]
        } else if compact {
            [2, 6, 2][at]
        } else {
            [2, 4, 6][at.min(bars - at - 1)]
        };
        let light = 0.16 + height as f32 * 0.13;
        spans.push(Span::styled(
            BARS[height - 1].to_string(),
            light_style(theme, light),
        ));
    }
    Line::from(spans)
}

pub(super) fn draw(
    mut panels: Query<(&LoadingLine, &mut PanelCanvas)>,
    settings: Res<LoadingSettings>,
    theme: Res<Theme>,
) {
    for (line, mut canvas) in &mut panels {
        let Some(run) = &line.0 else { continue };
        let area = canvas.area();
        if area.is_empty() {
            continue;
        }
        let mark = mark(
            &theme,
            run.phase,
            settings.animate && !run.waiting,
            area.width < 32,
        );
        let elapsed = if run.waiting {
            String::new()
        } else if run.seconds < 60 {
            format!(" {}s ", run.seconds)
        } else {
            format!(" {}m {:02}s ", run.seconds / 60, run.seconds % 60)
        };
        let [_, icon, _, label, timer] = Layout::horizontal([
            Constraint::Length(1),
            Constraint::Length(mark.width() as u16),
            Constraint::Length(2),
            Constraint::Min(0),
            Constraint::Length(elapsed.len() as u16),
        ])
        .areas(area);
        let buffer = canvas.bypass_change_detection().buffer_mut();
        mark.render(icon, buffer);
        let text_style = if theme.plain {
            Style::default()
        } else {
            Style::default().fg(theme.shade(0.72))
        };
        Line::styled(&run.label, text_style).render(label, buffer);
        Line::from(elapsed)
            .style(Style::default().add_modifier(Modifier::DIM))
            .right_aligned()
            .render(timer, buffer);
    }
}
