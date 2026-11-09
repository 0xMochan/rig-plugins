//! Hit test the last frame Rig actually drew, including its scroll and wraps.

use bevy_ecs::prelude::*;
use rig_harness::prelude::{Conversation, ShownAgent};
use rig_harness::rig_core::{
    completion::{AssistantContent, Message},
    message::CallId,
};
use rig_tui::crossterm::event::{Event, MouseButton, MouseEvent, MouseEventKind};
use rig_tui::{
    DrawnTranscript, InputContext, KeyFocus, KeyInput, MainPane, PendingInput, RenderRevision,
    TranscriptItem, TranscriptPane,
};

use crate::{input::Folds, render::DETAIL};

#[derive(Clone, PartialEq, Eq)]
struct Hit {
    pane: Entity,
    agent: Entity,
    call: CallId,
    expand: bool,
    column: u16,
    row: u16,
}

/// A press alone does not fold. Release must hit the same control without a
/// drag, modifier, modal or agent switch intervening.
#[derive(Resource, Default)]
pub(super) struct Pressed(Option<Hit>);

pub(super) fn track(
    panes: Query<
        Entity,
        (
            With<MainPane>,
            With<TranscriptPane>,
            Without<DrawnTranscript>,
        ),
    >,
    mut commands: Commands,
) {
    for pane in &panes {
        commands.entity(pane).insert(DrawnTranscript::default());
    }
}

pub(super) fn click(
    mut pending: ResMut<PendingInput>,
    context: Res<InputContext>,
    focus: Res<KeyFocus>,
    shown: Res<ShownAgent>,
    panes: Query<(Entity, &DrawnTranscript), With<MainPane>>,
    conversations: Query<&Conversation>,
    mut pressed: ResMut<Pressed>,
    mut folds: ResMut<Folds>,
    mut revision: ResMut<RenderRevision>,
) {
    if context.picking().is_some()
        || context.answering().is_some()
        || context.completion().is_some()
        || context.typed().is_some()
        || focus.0.is_some()
    {
        pressed.0 = None;
        return;
    }
    let Some(KeyInput::Event(Event::Mouse(mouse))) = pending.0.first() else {
        // Keyboard/paste/resize between a press and release invalidates the hit.
        pressed.0 = None;
        return;
    };
    if !mouse.modifiers.is_empty() {
        pressed.0 = None;
        return;
    }
    let target = || {
        panes.iter().find_map(|(pane, drawn)| {
            let hit = hit(pane, drawn, shown.0, mouse)?;
            // Only committed supported calls acquire folds. A synthetic
            // streaming id or a stale frame cannot target another call.
            let conversation = conversations.get(hit.agent).ok()?;
            supported(conversation, &hit.call).then_some(hit)
        })
    };
    match mouse.kind {
        MouseEventKind::Down(MouseButton::Left) => {
            pressed.0 = target();
            if pressed.0.is_some() {
                pending.0.remove(0);
            }
        }
        MouseEventKind::Up(MouseButton::Left) => {
            let down = pressed.0.take();
            if let Some(hit) = target().filter(|hit| down.as_ref() == Some(hit)) {
                folds.change(hit.agent, hit.call, hit.expand);
                revision.0 = revision.0.wrapping_add(1);
                pending.0.remove(0);
            }
        }
        _ => pressed.0 = None,
    }
}

fn supported(conversation: &Conversation, id: &CallId) -> bool {
    conversation.messages().iter().rev().any(|message| {
        let Message::Assistant(message) = &**message else {
            return false;
        };
        message.content.iter().any(|content| {
            matches!(content, AssistantContent::ToolCall(call)
                if call.id == *id)
        })
    })
}

fn hit(
    pane: Entity,
    drawn: &DrawnTranscript,
    shown: Option<Entity>,
    mouse: &MouseEvent,
) -> Option<Hit> {
    let agent = drawn.agent().filter(|agent| Some(*agent) == shown)?;
    let area = drawn.area();
    if !area.contains((mouse.column, mouse.row).into()) {
        return None;
    }
    let on_screen = drawn.on_screen();
    let row = on_screen.start + usize::from(mouse.row - area.y);
    if !on_screen.contains(&row) {
        return None;
    }
    let (call, expand) = match drawn.item_at(row)? {
        TranscriptItem::Decoration(call, name) if name == DETAIL => (call, true),
        TranscriptItem::Call(call) => {
            let start = drawn.rows_of(&TranscriptItem::Call(call.clone()))?.start;
            // The first logical line is the header, even when narrow panes
            // wrap it over more than one terminal row.
            if drawn
                .rows()
                .get(start + 1..row + 1)?
                .iter()
                .any(|row| row.continues.is_none())
            {
                return None;
            }
            (call, false)
        }
        _ => return None,
    };
    Some(Hit {
        pane,
        agent,
        call: call.clone(),
        expand,
        column: mouse.column,
        row: mouse.row,
    })
}
