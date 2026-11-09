use std::collections::HashMap;

use bevy_ecs::prelude::*;
use rig_harness::prelude::Conversation;
use rig_harness::rig_core::{
    completion::{AssistantContent, Message},
    message::CallId,
};
use rig_tui::{KeyFocus, KeyPressed, MainPane, RenderRevision, TranscriptItem, TranscriptView};

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub(super) enum Fold {
    Collapsed,
    #[default]
    Preview,
    Expanded,
}

/// Only user overrides are owned here. Agent identity prevents provider call
/// IDs reused by another agent from sharing a fold. Nothing is persisted.
#[derive(Resource, Default)]
pub(super) struct Folds(HashMap<(Entity, CallId), Fold>);

impl Folds {
    pub(super) fn get(&self, agent: Option<Entity>, call: &CallId) -> Fold {
        agent
            .and_then(|agent| self.0.get(&(agent, call.clone())).copied())
            .unwrap_or_default()
    }

    pub(super) fn change(&mut self, agent: Entity, call: CallId, expand: bool) {
        let current = self.get(Some(agent), &call);
        let next = match (expand, current) {
            (true, Fold::Expanded) | (false, Fold::Collapsed) => Fold::Preview,
            (true, _) => Fold::Expanded,
            (false, _) => Fold::Collapsed,
        };
        if next == Fold::Preview {
            self.0.remove(&(agent, call));
        } else {
            self.0.insert((agent, call), next);
        }
    }
}

pub(super) fn fold(
    key: On<KeyPressed>,
    focus: Res<KeyFocus>,
    panes: Query<&TranscriptView, With<MainPane>>,
    conversations: Query<&Conversation>,
    mut folds: ResMut<Folds>,
    mut revision: ResMut<RenderRevision>,
) {
    toggle(
        &key,
        &focus,
        &panes,
        &conversations,
        &mut folds,
        &mut revision,
        false,
    );
}

pub(super) fn expand(
    key: On<KeyPressed>,
    focus: Res<KeyFocus>,
    panes: Query<&TranscriptView, With<MainPane>>,
    conversations: Query<&Conversation>,
    mut folds: ResMut<Folds>,
    mut revision: ResMut<RenderRevision>,
) {
    toggle(
        &key,
        &focus,
        &panes,
        &conversations,
        &mut folds,
        &mut revision,
        true,
    );
}

fn toggle(
    key: &KeyPressed,
    focus: &KeyFocus,
    panes: &Query<&TranscriptView, With<MainPane>>,
    conversations: &Query<&Conversation>,
    folds: &mut Folds,
    revision: &mut RenderRevision,
    expand: bool,
) {
    // Only the composer and transcript bind these keys. This also protects
    // direct triggers; Rig excludes modal pickers/questions before dispatch.
    if key.released || focus.0.is_some() {
        return;
    }
    let Some(agent) = key.agent else { return };
    let Ok(conversation) = conversations.get(agent) else {
        return;
    };
    let selected = panes.iter().find_map(|pane| pane.selected.as_ref());
    let calls = conversation
        .messages()
        .iter()
        .rev()
        .filter_map(|message| match &**message {
            Message::Assistant(message) => Some(&message.content),
            _ => None,
        })
        .flat_map(|content| content.iter().rev())
        .filter_map(|content| match content {
            AssistantContent::ToolCall(call) => Some(call),
            _ => None,
        });
    let target = calls.into_iter().find(|call| match selected {
        Some(TranscriptItem::Call(id) | TranscriptItem::Decoration(id, _)) => call.id == *id,
        Some(_) => false,
        None => true,
    });
    if let Some(call) = target {
        folds.change(agent, call.id.clone(), expand);
        revision.0 = revision.0.wrapping_add(1);
    }
}
