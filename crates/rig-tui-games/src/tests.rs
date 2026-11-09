use rig_harness::ask::AskPlugin;
use rig_harness::commands::RunCommand;
use rig_harness::pick::{PickPlugin, Picker};
use rig_harness::prelude::{Agent, AgentPlugin, Conversation, Invoked, Settings};
use rig_tui::crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use rig_tui::ratatui::layout::{Constraint, Rect};
use rig_tui::{
    Editor, KeyFocus, KeyInput, MainEditor, MainPane, PanelCanvas, PendingInput, Placement,
    PromptHistory, RequestRedraw, TerminalSize, TuiPanel, TuiPlugin, TuiTestScreen,
};

use super::*;
use crate::shell::Session;

fn app(settings: &str, width: u16, height: u16) -> (App, Entity, Entity, Entity) {
    let mut app = App::new();
    app.set_error_handler(rig_harness::error::panic)
        .insert_resource(Invoked::default())
        .insert_resource(Settings::from_toml(settings))
        .add_plugins((
            AgentPlugin,
            AskPlugin,
            PickPlugin,
            TuiGamesPlugin,
            TuiPlugin,
        ));
    rig_tui::add_view(&mut app);
    app.insert_resource(TuiTestScreen::new(width, height));
    app.insert_resource(TerminalSize(Rect::new(0, 0, width, height)));
    app.finish();
    app.update();
    let world = app.world_mut();
    let agent = world
        .query_filtered::<Entity, With<Agent>>()
        .single(world)
        .unwrap();
    let pane = world
        .query_filtered::<Entity, With<MainPane>>()
        .single(world)
        .unwrap();
    let editor = world
        .query_filtered::<Entity, With<MainEditor>>()
        .single(world)
        .unwrap();
    (app, agent, pane, editor)
}

fn open(app: &mut App, agent: Entity, command: &str) -> Entity {
    app.world_mut().trigger(RunCommand::new(agent, command));
    app.update();
    let world = app.world_mut();
    world
        .query_filtered::<Entity, (With<Session>, With<TuiPanel>)>()
        .single(world)
        .unwrap()
}

fn press(app: &mut App, key: KeyCode) {
    app.world_mut()
        .resource_mut::<PendingInput>()
        .0
        .push(KeyInput::Event(Event::Key(KeyEvent::new(
            key,
            KeyModifiers::NONE,
        ))));
    app.update();
}

fn text(app: &App) -> String {
    app.world().resource::<TuiTestScreen>().rows().join("\n")
}

fn resize(app: &mut App, width: u16, height: u16) {
    app.insert_resource(TuiTestScreen::new(width, height));
    app.world_mut().resource_mut::<TerminalSize>().0 = Rect::new(0, 0, width, height);
    app.world_mut().write_message(RequestRedraw);
    app.update();
}

#[test]
fn real_commands_draw_all_games_capture_keys_and_restore_the_previous_focus_and_draft() {
    let (mut app, agent, pane, editor) = app("", 80, 24);
    let history = vec!["keep this history".to_owned()];
    app.world_mut()
        .resource_mut::<PromptHistory>()
        .entries
        .clone_from(&history);
    app.world_mut()
        .get_mut::<Editor>(editor)
        .unwrap()
        .set("unfinished draft");
    app.update();
    let original = app
        .world()
        .get::<Conversation>(agent)
        .unwrap()
        .messages()
        .to_vec();
    let pane_area = app.world().get::<PanelCanvas>(pane).unwrap().area();
    let previous = app
        .world_mut()
        .spawn(TuiPanel::new(Placement::Left(Constraint::Length(0))))
        .id();
    app.world_mut().resource_mut::<KeyFocus>().0 = Some(previous);
    for (command, label) in [
        ("tetris", "Tetris"),
        ("sudoku", "Sudoku"),
        ("2048", "2048"),
        ("dino", "Dino"),
    ] {
        let session = open(&mut app, agent, command);
        assert_eq!(app.world().resource::<KeyFocus>().0, Some(session));
        assert!(text(&app).contains(&format!("{label}  /  pocket arcade")));
        assert!(
            !text(&app).contains("Resize to"),
            "{command} must fit 80x24"
        );
        assert_eq!(
            app.world().get::<PanelCanvas>(pane).unwrap().area(),
            pane_area
        );
        press(&mut app, KeyCode::Char('1'));
        press(&mut app, KeyCode::Left);
        press(&mut app, KeyCode::Char('r'));
        assert_eq!(
            app.world().get::<Editor>(editor).unwrap().text(),
            "unfinished draft"
        );
        let before_hiding = text(&app);
        press(&mut app, KeyCode::Esc);
        assert!(app.world().get::<Session>(session).is_some());
        assert!(app.world().get::<TuiPanel>(session).is_none());
        assert!(
            app.world()
                .get::<rig_harness::prelude::KeepAwake>(session)
                .is_none()
        );
        assert_eq!(app.world().resource::<KeyFocus>().0, Some(previous));
        assert_eq!(open(&mut app, agent, command), session);
        if matches!(command, "sudoku" | "2048") {
            assert_eq!(text(&app), before_hiding, "reopening must retain the board");
        }
        assert!(!text(&app).contains("PAUSED"));
        press(&mut app, KeyCode::Esc);
    }
    assert_eq!(app.world().resource::<PromptHistory>().entries, history);
    assert_eq!(
        app.world().get::<Conversation>(agent).unwrap().messages(),
        original
    );
    let world = app.world_mut();
    assert_eq!(
        world
            .query_filtered::<Entity, With<Session>>()
            .iter(world)
            .count(),
        4
    );
}

#[test]
fn right_placement_falls_back_on_resize_and_hidden_or_paused_games_release_the_clock() {
    let (mut app, agent, _, editor) = app("[tui_games]\nplacement='right'", 120, 32);
    let session = open(&mut app, agent, "tetris");
    let area = app.world().get::<PanelCanvas>(session).unwrap().area();
    assert!(area.x >= 60);
    assert_eq!(area.right(), 120);
    app.world_mut()
        .get_mut::<Editor>(editor)
        .unwrap()
        .set("line\n".repeat(12));
    app.update();
    assert!(matches!(
        app.world().get::<TuiPanel>(session).unwrap().placement,
        Placement::Over { .. }
    ));
    assert!(!text(&app).contains("Resize to"));
    app.world_mut().get_mut::<Editor>(editor).unwrap().set("");
    app.update();
    assert!(
        app.world()
            .get::<rig_harness::prelude::KeepAwake>(session)
            .is_some()
    );
    press(&mut app, KeyCode::Char('p'));
    assert!(text(&app).contains("PAUSED"));
    assert!(
        app.world()
            .get::<rig_harness::prelude::KeepAwake>(session)
            .is_none()
    );
    press(&mut app, KeyCode::Char('p'));
    assert!(
        app.world()
            .get::<rig_harness::prelude::KeepAwake>(session)
            .is_some()
    );
    resize(&mut app, 80, 24);
    assert!(matches!(
        app.world().get::<TuiPanel>(session).unwrap().placement,
        Placement::Over { .. }
    ));
    assert!(!text(&app).contains("Resize to"));
    resize(&mut app, 35, 12);
    assert!(text(&app).contains("Resize to"));
    assert!(
        app.world()
            .get::<rig_harness::prelude::KeepAwake>(session)
            .is_none()
    );
    resize(&mut app, 80, 24);
    assert!(
        app.world()
            .get::<rig_harness::prelude::KeepAwake>(session)
            .is_some()
    );
    app.world_mut().resource_mut::<rig_tui::TerminalFocus>().0 = Some(false);
    app.update();
    assert!(
        app.world()
            .get::<rig_harness::prelude::KeepAwake>(session)
            .is_none()
    );
    app.world_mut().resource_mut::<rig_tui::TerminalFocus>().0 = Some(true);
    app.update();
    let picker = app
        .world_mut()
        .spawn(Picker::new("Choose").item("one", ""))
        .id();
    app.update();
    assert!(
        app.world()
            .get::<rig_harness::prelude::KeepAwake>(session)
            .is_none()
    );
    press(&mut app, KeyCode::Esc);
    assert!(
        app.world().get_entity(session).is_ok(),
        "picker owns its Escape"
    );
    if app.world().get_entity(picker).is_ok() {
        app.world_mut().despawn(picker);
    }
    app.update();
    press(&mut app, KeyCode::Esc);
    assert!(app.world().get::<Session>(session).is_some());
    assert!(app.world().get::<TuiPanel>(session).is_none());
    assert!(
        app.world()
            .get::<rig_harness::prelude::KeepAwake>(session)
            .is_none()
    );
    assert!(app.world().resource::<KeyFocus>().0.is_none());
    assert!(app.world().get::<Editor>(editor).unwrap().text().is_empty());
    assert_eq!(open(&mut app, agent, "tetris"), session);
    assert!(
        app.world()
            .get::<rig_harness::prelude::KeepAwake>(session)
            .is_some()
    );
}

#[test]
fn switching_games_preserves_progress_with_one_panel_and_no_hidden_animation_clocks() {
    let (mut app, agent, _, editor) = app("", 80, 24);
    // Exercise an actually submitted slash command through the native editor,
    // incoming-message gate and command runner, without a configured model.
    app.world_mut()
        .get_mut::<Editor>(editor)
        .unwrap()
        .set("/tetris");
    press(&mut app, KeyCode::Enter);
    let world = app.world_mut();
    let tetris = world
        .query_filtered::<Entity, With<Session>>()
        .single(world)
        .unwrap();
    assert!(app.world().get::<Editor>(editor).unwrap().text().is_empty());
    let sudoku = open(&mut app, agent, "sudoku right");
    assert!(app.world().get::<Session>(tetris).is_some());
    assert!(app.world().get::<TuiPanel>(tetris).is_none());
    assert!(
        app.world()
            .get::<rig_harness::prelude::KeepAwake>(tetris)
            .is_none()
    );
    assert!(
        app.world()
            .get::<rig_harness::prelude::KeepAwake>(sudoku)
            .is_none()
    );
    let board = open(&mut app, agent, "2048 popup");
    assert!(app.world().get::<Session>(sudoku).is_some());
    assert!(app.world().get::<TuiPanel>(sudoku).is_none());
    assert!(
        app.world()
            .get::<rig_harness::prelude::KeepAwake>(board)
            .is_none()
    );
    for key in [KeyCode::Left, KeyCode::Up, KeyCode::Right, KeyCode::Down] {
        press(&mut app, key);
    }
    let progress = text(&app);
    // Changing the shown agent hides the game without destroying it.
    app.world_mut()
        .resource_mut::<rig_harness::main_agent::ShownAgent>()
        .0 = None;
    app.world_mut().run_schedule(PostUpdate);
    assert!(app.world().get::<TuiPanel>(board).is_none());
    assert!(app.world().get::<Session>(board).is_some());
    app.world_mut()
        .resource_mut::<rig_harness::main_agent::ShownAgent>()
        .0 = Some(agent);
    assert_eq!(open(&mut app, agent, "tetris"), tetris);
    assert_eq!(open(&mut app, agent, "2048"), board);
    assert_eq!(
        text(&app),
        progress,
        "switching games must preserve tile progress"
    );
    let world = app.world_mut();
    assert_eq!(
        world
            .query_filtered::<Entity, (With<Session>, With<TuiPanel>)>()
            .iter(world)
            .count(),
        1
    );
    app.world_mut()
        .trigger(RunCommand::new(agent, "tetris nonsense"));
    app.update();
    assert_eq!(app.world().resource::<KeyFocus>().0, Some(board));
    press(&mut app, KeyCode::Esc);
    assert!(app.world().resource::<KeyFocus>().0.is_none());
}
