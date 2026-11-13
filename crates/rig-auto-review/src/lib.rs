//! Independent model review for explicit native shell escalations.
//!
//! A review is a tool-free model effect that holds one immutable command.
//! Errors, invalid output, changed authorization and timeouts refuse that
//! command. The plugin never retries the native action or asks a user itself.

use rig_harness::clock::ClockPlugin;
use rig_harness::prelude::*;
use rig_native_shell::NativeShellSystems;
mod review;
mod status;
use review::{ReviewConfig, ReviewJob, apply, bind_owner, request, timeout};

/// Reviews each explicit native shell escalation independently.
///
/// Does not install execution tools. The host selects native execution separately;
/// without it this policy has no prepared commands to review.
#[derive(Default, Debug)]
pub struct AutoReviewPlugin;

impl Plugin for AutoReviewPlugin {
    fn build(&self, app: &mut App) {
        rig_native_shell::configure_gate_order(app);
        status::install(app);
        if !app.is_plugin_added::<ClockPlugin>() {
            app.add_plugins(ClockPlugin);
        }
        app.add_setting::<ReviewConfig>("auto_review")
            .configure_sets(
                AgentPass,
                AutoReviewSystems::Request.in_set(NativeShellSystems::Review),
            )
            .configure_sets(
                AgentPass,
                AutoReviewSystems::Apply
                    .in_set(Stage::Review)
                    .after(ReviewSystems::Act),
            )
            .add_systems(AgentPass, request.in_set(AutoReviewSystems::Request))
            .add_systems(AgentPass, apply.in_set(AutoReviewSystems::Apply))
            .add_observer(timeout)
            .add_observer(follow_successor::<ReviewJob>)
            .add_observer(bind_owner)
            .add_observer(despawn_open_children::<ReviewJob>);
    }
}

/// Ordering points for hosts that add another approval policy.
#[derive(SystemSet, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AutoReviewSystems {
    /// After the shell freezes an action, hold it for independent review.
    Request,
    /// Apply a completed review before the original action may start.
    Apply,
}

#[cfg(all(test, unix))]
mod tests;
