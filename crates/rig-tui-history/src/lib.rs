//! Prefix history and inline suggestions for Rig's native prompt.
//!
//! Add [`TuiHistoryPlugin`] after [`rig_tui::TuiPlugin`]. The plugin reads
//! Rig's existing [`rig_tui::PromptHistory`]; it does not submit prompts,
//! replace the editor, or maintain another history file.

mod navigation;
mod suggestion;

use bevy_app::prelude::*;
use bevy_ecs::prelude::*;
use rig_harness::rig_core::redact;
use rig_tui::{InputContext, InputSystems, KeyScope, TuiKeys, TuiSystems};

/// Prefix-filtered Up/Down history and a gray suffix accepted with Right.
#[derive(Default, Debug)]
pub struct TuiHistoryPlugin;

/// This plugin's key filter, inside [`InputSystems::Filter`] in [`TuiKeys`].
/// Other filters may order themselves before or after it explicitly.
#[derive(SystemSet, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct HistoryInput;

impl Plugin for TuiHistoryPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<navigation::Navigation>()
            .init_resource::<suggestion::Ghost>()
            .configure_sets(TuiKeys, HistoryInput.in_set(InputSystems::Filter))
            .add_systems(TuiKeys, navigation::filter.in_set(HistoryInput))
            .add_systems(
                PostUpdate,
                suggestion::prepare
                    .after(TuiSystems::Prepare)
                    .before(TuiSystems::Layout),
            )
            .add_systems(
                PostUpdate,
                suggestion::draw
                    .after(TuiSystems::Draw)
                    .before(TuiSystems::Render),
            );
    }
}

/// A completion token owns its keys even before its list first opens.
fn owns_input(context: &InputContext, editor: Entity) -> bool {
    context.scope() == KeyScope::Input
        && context.editor() == Some(editor)
        && context.completion().is_none()
        && context.typed().is_none()
        && context.answering().is_none()
        && context.picking().is_none()
}

/// Read-time filtering only: the existing history's persistence is untouched.
fn eligible(entry: &str) -> bool {
    let command = entry.split_whitespace().next().unwrap_or_default();
    let authentication = [
        "/login",
        "/logout",
        "/auth",
        "/token",
        "/key",
        "/apikey",
        "/api-key",
        "/credentials",
        "/secret",
    ]
    .iter()
    .any(|excluded| command.eq_ignore_ascii_case(excluded));
    !entry.trim().is_empty()
        && !authentication
        && !entry.contains(redact::REDACTED)
        && !redact::has_secret(entry)
}

#[cfg(test)]
mod tests;
