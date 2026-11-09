//! Foldable tool presentation, independent of execution plugins.
//!
//! The native transcript still owns its history, selection and scroll anchors.
//! This plugin borrows render inputs and changes only their displayed rows.

mod input;
mod mouse;
mod render;

use bevy_app::prelude::*;
use bevy_ecs::prelude::*;
use rig_harness::prelude::AppSettingsExt;
use rig_tui::{
    AppKeyBindingsExt, InputSystems, KeyBinding, KeyPress, KeyScope, RenderRevision, RenderSystems,
    TranscriptRender, TuiKeys, TuiSystems,
};
use serde::Deserialize;

/// Bounded, foldable previews for every tool call, with readable source and
/// structured results for script tools.
#[derive(Default, Debug)]
pub struct TuiToolsPlugin;

/// The tool presentation phase. Order a replacing look after this set.
#[derive(SystemSet, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ScriptToolLooks;

/// Final folding and quiet header styling for tool boxes. Custom Edit-phase
/// renderers must run before this set to have their completed rows folded.
#[derive(SystemSet, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ToolBoxFolds;

/// Mouse hit testing in Rig's input-filter phase. Compositions order this
/// with other filters that borrow `PendingInput` mutably.
#[derive(SystemSet, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ScriptToolClicks;

impl Plugin for TuiToolsPlugin {
    fn build(&self, app: &mut App) {
        app.add_setting::<DisplaySettings>("tui_tools")
            .init_resource::<input::Folds>()
            .init_resource::<mouse::Pressed>()
            .init_resource::<RenderRevision>()
            .configure_sets(
                TranscriptRender,
                ScriptToolLooks.in_set(RenderSystems::Render),
            )
            .configure_sets(TranscriptRender, ToolBoxFolds.in_set(RenderSystems::Edit))
            .add_systems(TranscriptRender, render::draw.in_set(ScriptToolLooks))
            .add_systems(TranscriptRender, render::fold_tools.in_set(ToolBoxFolds))
            .configure_sets(TuiKeys, ScriptToolClicks.in_set(InputSystems::Filter))
            .add_systems(TuiKeys, mouse::click.in_set(ScriptToolClicks))
            .add_systems(PostUpdate, mouse::track.before(TuiSystems::Layout));
        for scope in [KeyScope::Input, KeyScope::Transcript] {
            app.add_key_binding(
                KeyBinding::new(
                    KeyPress::ctrl('o'),
                    "Fold/unfold the selected or latest tool call",
                )
                .in_scope(scope),
                input::fold,
            );
            app.add_key_binding(
                KeyBinding::new(
                    KeyPress::alt('o'),
                    "Show all / preview the selected or latest tool call",
                )
                .in_scope(scope),
                input::expand,
            );
        }
    }
}

/// Limits count terminal rows after Rig's own wrapping, independently for
/// input and output. Explicit expansion removes these presentation limits.
#[derive(Resource, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct DisplaySettings {
    input_rows: u16,
    output_rows: u16,
}

impl Default for DisplaySettings {
    fn default() -> Self {
        Self {
            input_rows: 8,
            output_rows: 10,
        }
    }
}

#[derive(Clone, Copy)]
enum ScriptTool {
    Xonsh,
    Monty,
    Native,
    Virtual,
}

impl ScriptTool {
    fn named(name: &str) -> Option<Self> {
        match name {
            "run_xonsh" => Some(Self::Xonsh),
            "run_monty" => Some(Self::Monty),
            "native_shell" => Some(Self::Native),
            "virtual_bash" => Some(Self::Virtual),
            _ => None,
        }
    }

    fn source(self) -> &'static str {
        match self {
            Self::Xonsh | Self::Monty => "code",
            Self::Native | Self::Virtual => "script",
        }
    }

    fn language(self) -> &'static str {
        match self {
            // Rig has no xonsh grammar; Python colors its Python expressions.
            Self::Xonsh | Self::Monty => "python",
            Self::Native | Self::Virtual => "bash",
        }
    }
}

#[cfg(test)]
mod tests;
