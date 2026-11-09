//! Mouse and trackpad scrolling through Rig's existing transcript actions.
//!
//! Add [`TuiScrollPlugin`] after [`rig_tui::TuiPlugin`]. The plugin owns
//! mouse reporting and wheel routing; Rig keeps ownership of layout, the
//! unsent draft, transcript anchors and following the newest output.

mod reporting;

use bevy_app::prelude::*;
use bevy_ecs::prelude::*;
use rig_harness::prelude::{AppCliExt, AppSettingsExt};
use rig_tui::crossterm::event::{Event, MouseEventKind};
use rig_tui::{
    InputAction, InputContext, InputSystems, KeyFocus, KeyInput, MainEditor, MainPane, PanelCanvas,
    PendingInput, TuiKeys, TuiViewPlugin,
};
use serde::Deserialize;

/// Scroll the main transcript with a mouse wheel or trackpad.
#[derive(Default, Debug)]
pub struct TuiScrollPlugin;

/// Wheel routing within [`InputSystems::Filter`] in [`TuiKeys`].
/// Compositions can order this before other filters without coupling crates.
#[derive(SystemSet, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct WheelInput;

impl Plugin for TuiScrollPlugin {
    fn build(&self, app: &mut App) {
        app.add_setting::<ScrollSettings>("tui_scroll");
    }

    fn finish(&self, app: &mut App) {
        assert!(
            app.is_plugin_added::<TuiViewPlugin>(),
            "TuiScrollPlugin requires rig_tui::TuiPlugin or TuiViewPlugin"
        );
        let terminal_front = app.runs_front(rig_tui::FRONT);
        reporting::install(app, terminal_front);
        app.configure_sets(TuiKeys, WheelInput.in_set(InputSystems::Filter))
            .add_systems(TuiKeys, scroll_wheel.in_set(WheelInput));
    }
}

#[derive(Resource, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct ScrollSettings {
    lines_per_event: u8,
}

impl Default for ScrollSettings {
    fn default() -> Self {
        Self { lines_per_event: 3 }
    }
}

fn scroll_wheel(
    mut pending: ResMut<PendingInput>,
    input: Res<InputContext>,
    focus: Res<KeyFocus>,
    settings: Res<ScrollSettings>,
    panels: Query<(Entity, &PanelCanvas), Or<(With<MainPane>, With<MainEditor>)>>,
) {
    // A modal owns input, even when its area overlaps the main pane.
    if input.picking().is_some() || input.answering().is_some() {
        return;
    }
    if focus.0.is_some_and(|entity| !panels.contains(entity)) {
        return;
    }
    let Some(KeyInput::Event(Event::Mouse(mouse))) = pending.0.first() else {
        return;
    };
    let action = match mouse.kind {
        MouseEventKind::ScrollUp => InputAction::ScrollUp,
        MouseEventKind::ScrollDown => InputAction::ScrollDown,
        _ => return,
    };
    if !panels
        .iter()
        .any(|(_, canvas)| canvas.area().contains((mouse.column, mouse.row).into()))
    {
        return;
    }
    // Actions bypass prompt-history and completion keys, and retain Rig's
    // own anchoring and bottom-follow behavior. Bound expansion per event.
    let rows = usize::from(settings.lines_per_event.clamp(1, 10));
    pending
        .0
        .splice(0..1, std::iter::repeat_n(KeyInput::Action(action), rows));
}

#[cfg(test)]
mod tests;
