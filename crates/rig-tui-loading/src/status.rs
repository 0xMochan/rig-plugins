//! Move only the activity item when its panel fits the default layout.

use bevy_ecs::prelude::*;
use rig_tui::{PanelCanvas, StatusItem, StatusItems};

use crate::LoadingLine;

#[derive(Resource, Default)]
pub(super) struct Relocated(Option<(Entity, StatusItem)>);

// Fit follows StatusSystems::Show/Edit and initial layout, before drawing.
// Custom Fit-phase resizers may cause another private layout afterward; see
// the README's one-update visibility limitation for those compositions.
// Retain the removed item so a hidden/suppressed panel or an agent switch can
// restore it even when upstream Activity has not changed since we hid it.
pub(super) fn relocate(
    panels: Query<(&LoadingLine, &PanelCanvas)>,
    mut agents: Query<&mut StatusItems>,
    mut moved: ResMut<Relocated>,
) {
    let target = panels.iter().find_map(|(line, canvas)| {
        (!canvas.area().is_empty())
            .then(|| line.0.as_ref().map(|run| run.agent))
            .flatten()
    });
    if moved
        .0
        .as_ref()
        .is_some_and(|(agent, _)| Some(*agent) != target)
    {
        let (agent, item) = moved.0.take().expect("previous activity item");
        if let Ok(mut items) = agents.get_mut(agent)
            && !items.iter().any(|item| item.name == "status")
        {
            items.show(item);
        }
    }
    let Some(agent) = target else { return };
    let Ok(mut items) = agents.get_mut(agent) else {
        return;
    };
    let item = items.iter().find(|item| item.name == "status").cloned();
    if let Some(item) = item {
        items.hide("status");
        moved.0 = Some((agent, item));
    }
}
