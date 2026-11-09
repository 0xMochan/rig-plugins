//! Only navigation owns the original draft; recalled text lives in the editor.

use bevy_ecs::prelude::*;
use rig_harness::main_agent::ShownAgent;
use rig_tui::crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers};
use rig_tui::{
    Editor, InputAction, InputContext, KeyInput, MainEditor, PanelCanvas, PendingInput,
    PromptHistory,
};

use crate::{eligible, owns_input, suggestion};

#[derive(Resource, Default)]
pub(crate) struct Navigation {
    owner: Option<(Entity, Option<Entity>)>,
    browsing: Option<Browsing>,
}

struct Browsing {
    draft: String,
    selected: String,
}

impl Navigation {
    /// An edit or a moved cursor starts a new search. This also runs after
    /// the frame's keys, so editing away and back cannot revive an old draft.
    pub(crate) fn synchronize(&mut self, entity: Entity, shown: Option<Entity>, editor: &Editor) {
        let owner = Some((entity, shown));
        let edited = self.browsing.as_ref().is_some_and(|browsing| {
            editor.text() != browsing.selected || editor.cursor() != browsing.selected.len()
        });
        if self.owner != owner || edited {
            self.browsing = None;
        }
        self.owner = owner;
    }

    fn up(&mut self, editor: &mut Editor, history: &[String]) {
        let (prefix, before) = match &self.browsing {
            Some(browsing) => (
                browsing.draft.as_str(),
                history
                    .iter()
                    .rposition(|entry| *entry == browsing.selected)
                    .unwrap_or(history.len()),
            ),
            None => (editor.text(), history.len()),
        };
        let previous = history[..before]
            .iter()
            .rev()
            .find(|entry| entry.starts_with(prefix) && eligible(entry));
        let Some(previous) = previous else { return };
        if let Some(browsing) = &mut self.browsing {
            browsing.selected.clone_from(previous);
        } else {
            self.browsing = Some(Browsing {
                draft: editor.text().to_owned(),
                selected: previous.clone(),
            });
        }
        // Unlike clear/take, set does not keep each replaced value in history.
        editor.set(previous.clone());
    }

    fn down(&mut self, editor: &mut Editor, history: &[String]) {
        let Some(browsing) = &mut self.browsing else {
            return;
        };
        let after = history
            .iter()
            .rposition(|entry| *entry == browsing.selected)
            .map_or(history.len(), |index| index + 1);
        let next = history[after..]
            .iter()
            .find(|entry| entry.starts_with(&browsing.draft) && eligible(entry));
        if let Some(next) = next {
            browsing.selected.clone_from(next);
            editor.set(next.clone());
        } else if let Some(browsing) = self.browsing.take() {
            editor.set(browsing.draft);
        }
    }
}

#[derive(Clone, Copy)]
enum Key {
    Up,
    Down,
    Right,
}

fn history_key(input: &KeyInput) -> Option<Key> {
    match input {
        KeyInput::Event(Event::Key(event))
            if event.modifiers == KeyModifiers::NONE
                && matches!(event.kind, KeyEventKind::Press | KeyEventKind::Repeat) =>
        {
            match event.code {
                KeyCode::Up => Some(Key::Up),
                KeyCode::Down => Some(Key::Down),
                KeyCode::Right => Some(Key::Right),
                _ => None,
            }
        }
        KeyInput::Action(InputAction::Up) => Some(Key::Up),
        KeyInput::Action(InputAction::Down) => Some(Key::Down),
        KeyInput::Action(InputAction::Right) => Some(Key::Right),
        _ => None,
    }
}

pub(crate) fn filter(
    mut pending: ResMut<PendingInput>,
    context: Res<InputContext>,
    shown: Option<Res<ShownAgent>>,
    history: Res<PromptHistory>,
    mut navigation: ResMut<Navigation>,
    mut editors: Query<(Entity, &mut Editor, &PanelCanvas), With<MainEditor>>,
) {
    let Ok((entity, mut editor, canvas)) = editors.single_mut() else {
        return;
    };
    navigation.synchronize(entity, shown.and_then(|shown| shown.0), &editor);
    if !owns_input(&context, entity) {
        return;
    }
    // TuiKeys normally supplies one item. Retaining unrelated replacements
    // also composes with an earlier plugin that expands an input event.
    pending.0.retain(|input| {
        let Some(key) = history_key(input) else {
            return true;
        };
        if matches!(key, Key::Right) {
            let candidate = suggestion::candidate(&history.entries, &editor);
            if let Some(candidate) = candidate
                && suggestion::area(&editor, canvas.area()).is_some()
            {
                editor.set(candidate.to_owned());
                navigation.browsing = None;
                return false;
            }
            return true;
        }
        let browsing = navigation.browsing.is_some();
        let initial = editor.cursor() == editor.text().len() && !editor.text().contains('\n');
        if !browsing && !initial {
            return true;
        }
        match key {
            // Even no match consumes Up: native fallback must not recall an
            // unrelated prompt after moving the cursor to the line's start.
            Key::Up => navigation.up(&mut editor, &history.entries),
            Key::Down if browsing => navigation.down(&mut editor, &history.entries),
            Key::Down => return true,
            Key::Right => return true,
        }
        false
    });
}
