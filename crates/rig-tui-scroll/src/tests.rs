use bevy_app::AppExit;
use rig_harness::ask::AskPlugin;
use rig_harness::pick::PickPlugin;
use rig_harness::prelude::*;
use rig_harness::rig_core::completion::Message;
use rig_tui::crossterm::event::{Event, KeyModifiers, MouseEvent};
use rig_tui::{Editor, RunInTerminal, TranscriptView, TuiPlugin, TuiTestScreen};

use super::*;

#[test]
fn wheel_preserves_draft_and_history_anchor_and_releases_reporting() {
    let mut app = App::new();
    app.set_error_handler(rig_harness::error::panic)
        .insert_resource(Invoked::default())
        .insert_resource(Settings::from_toml("[tui_scroll]\nlines_per_event = 3"))
        .add_plugins((
            AgentPlugin,
            AskPlugin,
            PickPlugin,
            TuiPlugin,
            TuiScrollPlugin,
        ));
    rig_tui::add_view(&mut app);
    app.insert_resource(TuiTestScreen::new(70, 16));
    app.finish();
    app.update();

    let world = app.world_mut();
    let agent = world
        .query_filtered::<Entity, With<Agent>>()
        .single(world)
        .expect("main agent");
    let pane = world
        .query_filtered::<Entity, With<MainPane>>()
        .single(world)
        .expect("main pane");
    let editor = world
        .query_filtered::<Entity, With<MainEditor>>()
        .single(world)
        .expect("main editor");
    let history = (0..60)
        .map(|row| format!("history row {row:02}"))
        .collect::<Vec<_>>()
        .join("\n\n");
    world
        .get_mut::<Conversation>(agent)
        .expect("conversation")
        .push(Message::assistant(history));
    world
        .get_mut::<Editor>(editor)
        .expect("editor")
        .set("unfinished draft");
    app.update();
    assert!(top(&app, pane).is_none());
    assert!(
        screen(&app)
            .written()
            .windows(reporting::ENABLE.len())
            .any(|bytes| bytes == reporting::ENABLE)
    );

    // Wheel events over the composer belong to the history, too.
    wheel(&mut app, editor, MouseEventKind::ScrollUp, 1);
    let anchored = top(&app, pane);
    assert!(
        anchored.is_some(),
        "wheel should leave the bottom-follow position"
    );
    assert_eq!(
        app.world().get::<Editor>(editor).expect("editor").text(),
        "unfinished draft"
    );
    app.world_mut()
        .get_mut::<Conversation>(agent)
        .expect("conversation")
        .push(Message::assistant("new output while reading"));
    app.update();
    assert_eq!(
        top(&app, pane),
        anchored,
        "new output must preserve the reading anchor"
    );

    let picker = app
        .world_mut()
        .spawn(Picker::new("Choose").item("one", ""))
        .id();
    app.update();
    assert_eq!(
        app.world().resource::<InputContext>().picking(),
        Some(picker)
    );
    let before_modal_wheel = top(&app, pane);
    wheel(&mut app, pane, MouseEventKind::ScrollUp, 1);
    assert_eq!(
        top(&app, pane),
        before_modal_wheel,
        "a modal must own its input"
    );
    app.world_mut().despawn(picker);
    app.update();

    wheel(&mut app, pane, MouseEventKind::ScrollDown, 100);
    assert!(
        top(&app, pane).is_none(),
        "reaching the bottom should resume following output"
    );
    assert!(
        screen(&app)
            .rows()
            .iter()
            .any(|row| row.contains("new output while reading"))
    );
    assert_eq!(
        app.world().get::<Editor>(editor).expect("editor").text(),
        "unfinished draft"
    );

    // Exercise the public handoff observer without running a child program.
    let before_handoff = screen(&app).written().len();
    let run = app
        .world_mut()
        .spawn(RunInTerminal(std::process::Command::new("unused")))
        .id();
    app.world_mut().entity_mut(run).remove::<RunInTerminal>();
    app.update();
    let handoff = &screen(&app).written()[before_handoff..];
    assert!(handoff.starts_with(reporting::DISABLE));
    assert!(handoff.ends_with(reporting::ENABLE));

    let before_exit = screen(&app).written().len();
    app.world_mut().write_message(AppExit::Success);
    app.update();
    assert!(
        screen(&app).written()[before_exit..]
            .windows(reporting::DISABLE.len())
            .any(|bytes| bytes == reporting::DISABLE)
    );
}

fn screen(app: &App) -> &TuiTestScreen {
    app.world().resource::<TuiTestScreen>()
}

fn top(app: &App, pane: Entity) -> Option<(rig_tui::TranscriptItem, usize)> {
    app.world()
        .get::<TranscriptView>(pane)
        .expect("transcript view")
        .top()
        .cloned()
}

fn wheel(app: &mut App, panel: Entity, kind: MouseEventKind, count: usize) {
    let area = app
        .world()
        .get::<PanelCanvas>(panel)
        .expect("panel canvas")
        .area();
    assert!(!area.is_empty());
    let mouse = MouseEvent {
        kind,
        column: area.x,
        row: area.y,
        modifiers: KeyModifiers::NONE,
    };
    app.world_mut()
        .resource_mut::<PendingInput>()
        .0
        .extend(std::iter::repeat_n(
            KeyInput::Event(Event::Mouse(mouse)),
            count,
        ));
    app.update();
}
