use bevy_app::prelude::*;
use bevy_ecs::prelude::*;
use rig_harness::ask::AskPlugin;
use rig_harness::pick::{PickPlugin, Picker};
use rig_harness::prelude::{AgentPlugin, Invoked};
use rig_tui::crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use rig_tui::{
    CompletionSource, Editor, InputContext, KeyInput, MainEditor, PanelCanvas, PendingInput,
    PromptHistory, RequestRedraw, Sent, Theme, TuiPlugin, TuiTestScreen,
};

use crate::{TuiHistoryPlugin, eligible, suggestion};

#[derive(Resource, Default)]
struct SentPrompts(Vec<String>);

fn app() -> (App, Entity) {
    let mut app = App::new();
    app.set_error_handler(rig_harness::error::panic)
        .insert_resource(Invoked::default())
        .init_resource::<SentPrompts>()
        .add_plugins((
            AgentPlugin,
            AskPlugin,
            PickPlugin,
            TuiPlugin,
            TuiHistoryPlugin,
        ))
        .add_systems(
            Update,
            |mut sent: MessageReader<Sent>, mut prompts: ResMut<SentPrompts>| {
                prompts.0.extend(sent.read().map(|sent| sent.0.clone()));
            },
        );
    rig_tui::add_view(&mut app);
    app.insert_resource(TuiTestScreen::new(60, 20));
    app.update();
    let world = app.world_mut();
    let editor = world
        .query_filtered::<Entity, With<MainEditor>>()
        .single(world)
        .expect("default screen has one main editor");
    (app, editor)
}

fn draft(app: &mut App, editor: Entity, text: &str) {
    app.world_mut()
        .get_mut::<Editor>(editor)
        .expect("main editor")
        .set(text);
    app.update();
}

fn press(app: &mut App, code: KeyCode) {
    app.world_mut()
        .resource_mut::<PendingInput>()
        .0
        .push(KeyInput::Event(Event::Key(KeyEvent::new(
            code,
            KeyModifiers::NONE,
        ))));
    app.world_mut().write_message(RequestRedraw);
    app.update();
}

fn text(app: &App, editor: Entity) -> &str {
    app.world()
        .get::<Editor>(editor)
        .expect("main editor")
        .text()
}

#[test]
fn ghost_and_prefix_navigation_use_real_keys_without_submitting_or_recording_replacements() {
    let (mut app, editor) = app();
    let entries = [
        "cargo check\nexplain failures",
        "git status",
        "cargo check",
        "cargo [REDACTED]",
        "界面 assessment with a suffix longer than a narrow screen",
    ]
    .map(str::to_owned)
    .to_vec();
    app.world_mut()
        .resource_mut::<PromptHistory>()
        .entries
        .clone_from(&entries);
    draft(&mut app, editor, "cargo ");
    let input = app.world().get::<Editor>(editor).expect("main editor");
    let canvas = app
        .world()
        .get::<PanelCanvas>(editor)
        .expect("editor canvas");
    let suffix = suggestion::area(input, canvas.area()).expect("inline suffix area");
    let screen = app.world().resource::<TuiTestScreen>();
    assert!(
        screen
            .rows()
            .iter()
            .any(|row| row.contains("› cargo check"))
    );
    let cell = screen
        .buffer()
        .cell((suffix.x, suffix.y))
        .expect("suffix cell");
    assert_eq!(cell.symbol(), "c");
    assert_eq!(cell.fg, app.world().resource::<Theme>().shade(0.45));
    assert_eq!(input.text(), "cargo ");
    assert_eq!(input.cursor(), "cargo ".len());

    press(&mut app, KeyCode::Right);
    assert_eq!(text(&app, editor), "cargo check");
    draft(&mut app, editor, "cargo ");
    press(&mut app, KeyCode::Up);
    assert_eq!(text(&app, editor), "cargo check");
    press(&mut app, KeyCode::Up);
    assert_eq!(text(&app, editor), "cargo check\nexplain failures");
    press(&mut app, KeyCode::Down);
    assert_eq!(text(&app, editor), "cargo check");
    press(&mut app, KeyCode::Down);
    assert_eq!(text(&app, editor), "cargo ");

    app.world_mut()
        .resource_mut::<TuiTestScreen>()
        .resize(20, 12);
    draft(&mut app, editor, "界");
    let input = app.world().get::<Editor>(editor).expect("main editor");
    let canvas = app
        .world()
        .get::<PanelCanvas>(editor)
        .expect("editor canvas");
    let suffix = suggestion::area(input, canvas.area()).expect("wide prefix inline area");
    let screen = app.world().resource::<TuiTestScreen>();
    assert_eq!(
        screen
            .buffer()
            .cell((suffix.x, suffix.y))
            .expect("wide suffix")
            .symbol(),
        "面"
    );
    press(&mut app, KeyCode::Right);
    assert_eq!(text(&app, editor), entries[4]);
    assert!(app.world().resource::<SentPrompts>().0.is_empty());
    assert_eq!(app.world().resource::<PromptHistory>().entries, entries);
}

#[test]
fn unmatched_prefixes_stay_put_and_modals_and_multiline_editing_keep_their_keys() {
    let (mut app, editor) = app();
    app.world_mut().resource_mut::<PromptHistory>().entries = vec!["cargo check".to_owned()];
    draft(&mut app, editor, "unrelated");
    press(&mut app, KeyCode::Up);
    press(&mut app, KeyCode::Up);
    assert_eq!(text(&app, editor), "unrelated");
    assert_eq!(
        app.world()
            .get::<Editor>(editor)
            .expect("main editor")
            .cursor(),
        "unrelated".len()
    );

    draft(&mut app, editor, "one\ntwo");
    press(&mut app, KeyCode::Up);
    assert_eq!(text(&app, editor), "one\ntwo");
    assert_eq!(
        app.world()
            .get::<Editor>(editor)
            .expect("main editor")
            .cursor(),
        3
    );

    draft(&mut app, editor, "ca");
    let picker = app
        .world_mut()
        .spawn(Picker::new("Choose").item("first", "").item("second", ""))
        .id();
    app.update();
    assert_eq!(
        app.world().resource::<InputContext>().picking(),
        Some(picker)
    );
    press(&mut app, KeyCode::Up);
    press(&mut app, KeyCode::Right);
    assert_eq!(text(&app, editor), "ca");
    assert!(
        !app.world()
            .resource::<TuiTestScreen>()
            .rows()
            .iter()
            .any(|row| row.contains("› cargo check"))
    );
    app.world_mut().despawn(picker);
    app.update();

    app.world_mut()
        .spawn(CompletionSource::marker('#').candidate("issue", ""));
    app.world_mut()
        .resource_mut::<PromptHistory>()
        .entries
        .push("#interesting history".to_owned());
    draft(&mut app, editor, "#i");
    app.update();
    assert!(app.world().resource::<InputContext>().typed().is_some());
    press(&mut app, KeyCode::Right);
    assert_eq!(text(&app, editor), "#i");
    assert!(app.world().resource::<SentPrompts>().0.is_empty());
    assert!(!eligible("/login token"));
    assert!(!eligible("cargo [REDACTED]"));
}
