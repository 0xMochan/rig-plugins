//! A borderless activity line above the composer, not inside its status row.
//!
//! Rig owns activity, transcript layout and the editor. This plugin owns only
//! one panel, a shared animation clock, and relocation of the shown activity
//! item. The native welcome logo is recolored in place only while visible.

mod render;
mod status;
mod welcome;

use std::time::{Duration, Instant};

use bevy_app::prelude::*;
use bevy_ecs::prelude::*;
use rig_basics::activity::{Activity, ActivityPlugin, ActivityStatus, ActivitySystems};
use rig_harness::main_agent::ShownAgent;
use rig_harness::prelude::{AppSettingsExt, KeepAwake};
use rig_tui::ratatui::layout::Constraint;
use rig_tui::{PanelCanvas, Placement, RequestRedraw, TuiPanel, TuiSystems, TuiViewPlugin};
use serde::Deserialize;

/// Shows activity with a compact traveling bar wave and pulses the native
/// welcome logo in view. Inert without a terminal view; no execution capability.
#[derive(Default, Debug)]
pub struct TuiLoadingPlugin;

impl Plugin for TuiLoadingPlugin {
    fn build(&self, app: &mut App) {
        app.add_setting::<LoadingSettings>("tui_loading");
        rig_harness::load::<ActivityPlugin>(app, "rig-basics", "version 0.46.0");
    }

    fn finish(&self, app: &mut App) {
        if !app.is_plugin_added::<TuiViewPlugin>() {
            return;
        }
        app.init_resource::<status::Relocated>()
            .init_resource::<welcome::VisibleLogo>()
            .add_systems(Startup, spawn)
            .add_systems(
                PostUpdate,
                (
                    prepare.after(ActivitySystems).in_set(TuiSystems::Prepare),
                    welcome::track.in_set(TuiSystems::Prepare),
                    status::relocate.in_set(TuiSystems::Fit),
                    render::draw.in_set(TuiSystems::Draw),
                    (welcome::paint, fit_clock)
                        .chain()
                        .after(TuiSystems::Draw)
                        .before(TuiSystems::Render),
                ),
            );
    }
}

#[derive(Resource, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct LoadingSettings {
    animate: bool,
}

impl Default for LoadingSettings {
    fn default() -> Self {
        Self { animate: true }
    }
}

const STEP: Duration = Duration::from_millis(120);
const PHASES: u128 = 12;

#[derive(Component, Default)]
struct LoadingLine(Option<Run>);

struct Run {
    agent: Entity,
    started: Instant,
    label: String,
    waiting: bool,
    phase: usize,
    seconds: u64,
}

fn spawn(mut commands: Commands) {
    // Startup runs after the default screen's build-time spawns, even when
    // this plugin was listed before TuiPlugin. Bottom panels stack upward.
    commands.spawn((
        Name::new("tui:loading"),
        LoadingLine::default(),
        TuiPanel::new(Placement::Bottom(Constraint::Length(0))),
    ));
}

fn prepare(
    shown: Res<ShownAgent>,
    activities: Query<Ref<Activity>>,
    settings: Res<LoadingSettings>,
    mut panels: Query<(&mut LoadingLine, &mut TuiPanel)>,
    mut redraw: MessageWriter<RequestRedraw>,
) {
    let activity = shown
        .0
        .and_then(|agent| activities.get(agent).ok().map(|activity| (agent, activity)))
        .filter(|(_, activity)| activity.is_busy());
    let now = Instant::now();
    for (mut line, mut panel) in &mut panels {
        let Some((agent, activity)) = &activity else {
            if line.0.take().is_some() {
                panel.placement = Placement::Bottom(Constraint::Length(0));
                redraw.write(RequestRedraw);
            }
            continue;
        };
        let fresh = line.0.as_ref().is_none_or(|run| run.agent != *agent);
        if fresh {
            line.0 = Some(Run {
                agent: *agent,
                started: now,
                label: String::new(),
                waiting: false,
                phase: 0,
                seconds: 0,
            });
            panel.set_if_neq(TuiPanel::new(Placement::Bottom(Constraint::Length(1))));
        }
        let run = line.0.as_mut().expect("active loading line");
        let mut changed = fresh || settings.is_changed();
        if fresh || activity.is_changed() {
            // Status labels can originate in plugins or questions. Keep the
            // physical line single-row, including on a narrow terminal.
            let label = activity
                .status
                .to_string()
                .chars()
                .map(|ch| if ch.is_control() { ' ' } else { ch })
                .collect::<String>();
            let waiting = matches!(activity.status, ActivityStatus::Asking(_));
            changed |= run.label != label || run.waiting != waiting;
            run.label = label;
            run.waiting = waiting;
        }
        let elapsed = now.duration_since(run.started);
        let phase = if settings.animate && !run.waiting {
            ((elapsed.as_millis() / STEP.as_millis()) % PHASES) as usize
        } else {
            0
        };
        let seconds = elapsed.as_secs();
        changed |= run.phase != phase || (!run.waiting && run.seconds != seconds);
        run.phase = phase;
        run.seconds = seconds;
        if changed {
            redraw.write(RequestRedraw);
        }
    }
}

fn fit_clock(
    panels: Query<(Entity, &LoadingLine, &PanelCanvas, Option<&KeepAwake>)>,
    settings: Res<LoadingSettings>,
    logo: Res<welcome::VisibleLogo>,
    mut commands: Commands,
) {
    // Run after final layout and drawing: either the line or the native logo
    // must actually be in view. Share one clock, including when a tiny screen
    // suppresses the line but still shows the banner. No worker or idle loop.
    for (entity, line, canvas, awake) in &panels {
        let interval = line.0.as_ref().and_then(|run| {
            if run.waiting || (canvas.area().is_empty() && !logo.0) {
                None
            } else if settings.animate {
                Some(STEP)
            } else {
                Some(Duration::from_secs(1))
            }
        });
        match interval {
            Some(interval) if awake.is_none_or(|awake| awake.0 != interval) => {
                commands.entity(entity).insert(KeepAwake(interval));
            }
            None if awake.is_some() => {
                commands.entity(entity).remove::<KeepAwake>();
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests;
