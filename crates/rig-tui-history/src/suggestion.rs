//! Ghost text is drawn after the editor, never inserted into its draft.

use bevy_ecs::prelude::*;
use rig_harness::main_agent::ShownAgent;
use rig_tui::ratatui::layout::Rect;
use rig_tui::ratatui::style::{Modifier, Style};
use rig_tui::ratatui::widgets::{Paragraph, Widget};
use rig_tui::{Editor, InputContext, MainEditor, PanelCanvas, PromptHistory, RequestRedraw, Theme};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::{eligible, navigation::Navigation, owns_input};

#[derive(Resource, Default)]
pub(crate) struct Ghost(Option<Suggestion>);

#[derive(PartialEq, Eq)]
struct Suggestion {
    editor: Entity,
    prefix: String,
    completion: String,
}

/// Keep the drawing predictable: one physical line, no terminal controls or
/// joiners whose grapheme width differs from the stock editor's char layout.
fn inline(text: &str) -> bool {
    text.chars()
        .all(|character| !character.is_control() && character != '\u{200d}')
        && text
            .chars()
            .map(|character| character.width().unwrap_or(0))
            .sum::<usize>()
            == text.width()
}

pub(crate) fn candidate<'a>(history: &'a [String], editor: &Editor) -> Option<&'a str> {
    let prefix = editor.text();
    if prefix.is_empty() || editor.cursor() != prefix.len() || !inline(prefix) {
        return None;
    }
    history
        .iter()
        .rev()
        .find(|entry| {
            entry.len() > prefix.len()
                && entry.starts_with(prefix)
                && inline(entry)
                && eligible(entry)
        })
        .map(String::as_str)
}

/// The stock editor reserves a title row above/below and one column of
/// padding on either side; its first line starts after the two-column `› `.
/// A wrapped prefix has no simple inline position, so it receives no ghost.
pub(crate) fn area(editor: &Editor, canvas: Rect) -> Option<Rect> {
    if canvas.width < 5 || canvas.height < 3 || !inline(editor.text()) {
        return None;
    }
    let content = usize::from(canvas.width - 2);
    let column = editor
        .text()
        .chars()
        .try_fold(2usize, |column, character| {
            column.checked_add(character.width().unwrap_or(0))
        })?;
    if column >= content {
        return None;
    }
    let column = u16::try_from(column).ok()?;
    Some(Rect::new(
        canvas.x.saturating_add(1).saturating_add(column),
        canvas.y.saturating_add(1),
        canvas.width - 2 - column,
        1,
    ))
}

pub(crate) fn prepare(
    context: Res<InputContext>,
    shown: Option<Res<ShownAgent>>,
    history: Res<PromptHistory>,
    editors: Query<(Entity, &Editor), With<MainEditor>>,
    mut navigation: ResMut<Navigation>,
    mut ghost: ResMut<Ghost>,
    mut redraw: MessageWriter<RequestRedraw>,
) {
    let next = editors.single().ok().and_then(|(entity, editor)| {
        navigation.synchronize(entity, shown.and_then(|shown| shown.0), editor);
        if !owns_input(&context, entity) {
            return None;
        }
        let completion = candidate(&history.entries, editor)?;
        Some(Suggestion {
            editor: entity,
            prefix: editor.text().to_owned(),
            completion: completion.to_owned(),
        })
    });
    if ghost.0 != next {
        ghost.0 = next;
        redraw.write(RequestRedraw);
    }
}

pub(crate) fn draw(
    context: Res<InputContext>,
    ghost: Res<Ghost>,
    theme: Res<Theme>,
    mut editors: Query<(&Editor, &mut PanelCanvas), With<MainEditor>>,
) {
    let Some(suggestion) = &ghost.0 else { return };
    if !owns_input(&context, suggestion.editor) {
        return;
    }
    let Ok((editor, mut canvas)) = editors.get_mut(suggestion.editor) else {
        return;
    };
    if editor.text() != suggestion.prefix || editor.cursor() != suggestion.prefix.len() {
        return;
    }
    let Some(area) = area(editor, canvas.area()) else {
        return;
    };
    let Some(suffix) = suggestion.completion.get(suggestion.prefix.len()..) else {
        return;
    };
    let style = if theme.plain {
        Style::default().add_modifier(Modifier::DIM)
    } else {
        Style::default().fg(theme.shade(0.45))
    };
    // The cursor remains the stock editor's cursor. Paragraph clips the
    // suffix to the remaining columns and never changes editor layout.
    Paragraph::new(suffix)
        .style(style)
        .render(area, canvas.bypass_change_detection().buffer_mut());
}
