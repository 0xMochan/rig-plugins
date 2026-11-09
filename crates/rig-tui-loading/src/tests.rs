use bevy_app::TaskPoolPlugin;
use rig_harness::ask::{Ask, AskPlugin};
use rig_harness::pick::PickPlugin;
use rig_harness::prelude::{
    Agent, AgentPlugin, Conversation, EffectRequest, ForAgent, Invoked, Outcome, Settings,
};
use rig_harness::rig_core::completion::Message;
use rig_harness::rig_core::effect::EffectKind;
use rig_tui::ratatui::layout::Rect;
use rig_tui::ratatui::style::Color;
use rig_tui::{
    DrawnTranscript, Editor, ItemRow, MainEditor, MainPane, PromptHistory, RenderRevision,
    ScrollTo, Side, StatusItem, StatusItems, Theme, Tone, TranscriptItem, TranscriptView,
    TuiPlugin, TuiTestScreen,
};

use super::*;

fn app(settings: &str) -> (App, Entity, Entity, Entity, Entity) {
    let mut app = App::new();
    app.set_error_handler(rig_harness::error::panic)
        .insert_resource(Invoked::default())
        .insert_resource(Settings::from_toml(settings))
        .add_plugins((
            TaskPoolPlugin::default(),
            AgentPlugin,
            AskPlugin,
            PickPlugin,
            rig_basics::welcome::WelcomePlugin,
            // Listing the loading plugin before the frontend must not put
            // its bottom panel underneath the default composer.
            TuiLoadingPlugin,
            TuiPlugin,
        ));
    rig_tui::add_view(&mut app);
    app.insert_resource(TuiTestScreen::new(80, 24));
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
    let loading = world
        .query_filtered::<Entity, With<LoadingLine>>()
        .single(world)
        .unwrap();
    (app, agent, pane, editor, loading)
}

fn work(app: &mut App, agent: Entity, label: &'static str) -> Entity {
    app.world_mut()
        .spawn((
            EffectRequest {
                kind: EffectKind::Custom {
                    kind: "loading-test".into(),
                    payload: Default::default(),
                },
                handler: None,
            },
            ForAgent(agent),
            Name::new(label),
        ))
        .id()
}

fn area(app: &App, panel: Entity) -> Rect {
    app.world().get::<PanelCanvas>(panel).unwrap().area()
}

fn row(app: &App, panel: Entity) -> String {
    let area = area(app, panel);
    assert_eq!(area.height, 1);
    app.world().resource::<TuiTestScreen>().rows()[usize::from(area.y)].clone()
}

fn status_text(app: &App, agent: Entity) -> Option<String> {
    app.world()
        .get::<StatusItems>(agent)
        .unwrap()
        .iter()
        .find(|item| item.name == "status")
        .map(|item| item.text.clone())
}

fn age(app: &mut App, loading: Entity, elapsed: Duration) {
    app.world_mut()
        .get_mut::<LoadingLine>(loading)
        .unwrap()
        .0
        .as_mut()
        .unwrap()
        .started = Instant::now() - elapsed;
    app.world_mut().write_message(RequestRedraw);
    app.update();
}

#[test]
fn real_named_activity_moves_above_the_composer_without_changing_owned_user_state() {
    let (mut app, agent, pane, editor, loading) = app("");
    assert!(area(&app, loading).is_empty());
    let idle_height = area(&app, pane).height;
    let history = vec!["cargo test".to_owned(), "git status".to_owned()];
    app.world_mut()
        .resource_mut::<PromptHistory>()
        .entries
        .clone_from(&history);
    app.world_mut()
        .get_mut::<Editor>(editor)
        .unwrap()
        .set("unfinished draft");
    app.world_mut()
        .get_mut::<Conversation>(agent)
        .unwrap()
        .push(Message::assistant(
            (0..80)
                .map(|n| format!("history row {n:02}"))
                .collect::<Vec<_>>()
                .join("\n\n"),
        ));
    app.world_mut()
        .get_mut::<StatusItems>(agent)
        .unwrap()
        .show(StatusItem::new("model", Side::Left, 20, u8::MAX).says("fixture-model", Tone::Bold));
    app.world_mut()
        .get_mut::<StatusItems>(agent)
        .unwrap()
        .show(StatusItem::new("tokens", Side::Right, 40, 5).says("123 tokens", Tone::Dim));
    app.update();
    let conversation = app.world().get::<Conversation>(agent).unwrap();
    let original = conversation.messages().to_vec();
    let item = TranscriptItem::Message(conversation.id(0).unwrap());
    app.world_mut().trigger(ScrollTo {
        pane,
        item: Some(item),
        row: ItemRow::Top(8),
    });
    app.update();
    let anchor = app
        .world()
        .get::<TranscriptView>(pane)
        .unwrap()
        .top()
        .cloned();
    assert!(anchor.is_some());

    let effect = work(&mut app, agent, "reviewing");
    app.update();
    assert_eq!(
        app.world().get::<Activity>(agent).unwrap().status,
        ActivityStatus::Busy("reviewing".into())
    );
    let loading_area = area(&app, loading);
    assert_eq!(loading_area.bottom(), area(&app, editor).y);
    assert_eq!(area(&app, pane).height + 1, idle_height);
    let text = row(&app, loading);
    assert!(text.contains("▂▄▆ ▄▂▁"), "{text}");
    assert!(
        !text.contains('█'),
        "the line must not slice the block logo"
    );
    assert!(text.contains("reviewing"), "{text}");
    assert!(
        status_text(&app, agent).is_none(),
        "activity must not be duplicated in the composer"
    );
    let items = app.world().get::<StatusItems>(agent).unwrap();
    assert!(
        items
            .iter()
            .any(|item| item.name == "model" && item.text == "fixture-model")
    );
    assert!(
        items
            .iter()
            .any(|item| item.name == "tokens" && item.text == "123 tokens")
    );
    let cell = app
        .world()
        .resource::<TuiTestScreen>()
        .buffer()
        .cell((loading_area.x + 1, loading_area.y))
        .unwrap();
    assert_eq!(
        cell.bg,
        Color::Reset,
        "the loading line is not an input-colored box"
    );
    assert_eq!(
        app.world().get::<Editor>(editor).unwrap().text(),
        "unfinished draft"
    );
    assert_eq!(app.world().resource::<PromptHistory>().entries, history);
    assert_eq!(
        app.world().get::<Conversation>(agent).unwrap().messages(),
        original
    );
    assert_eq!(
        app.world()
            .get::<TranscriptView>(pane)
            .unwrap()
            .top()
            .cloned(),
        anchor
    );

    app.world_mut()
        .entity_mut(effect)
        .insert(Outcome::custom("done"));
    app.update();
    assert!(area(&app, loading).is_empty());
    assert_eq!(area(&app, pane).height, idle_height);
    assert!(app.world().get::<KeepAwake>(loading).is_none());
    assert_eq!(status_text(&app, agent).as_deref(), Some("idle"));
    assert_eq!(
        app.world()
            .get::<TranscriptView>(pane)
            .unwrap()
            .top()
            .cloned(),
        anchor
    );
}

#[test]
fn traveling_wave_keeps_an_open_fixed_width_frame_and_tracks_the_selected_agent() {
    let (mut app, agent, _, _, loading) = app("");
    work(&mut app, agent, "reviewing");
    app.update();
    age(&mut app, loading, Duration::ZERO);
    let before = row(&app, loading);
    let loading_area = area(&app, loading);
    let first = (loading_area.x + 1, loading_area.y);
    let screen = app.world().resource::<TuiTestScreen>();
    let first_color = screen.buffer().cell(first).unwrap().fg;
    assert_eq!(app.world().get::<KeepAwake>(loading).unwrap().0, STEP);

    age(&mut app, loading, Duration::from_millis(600));
    let after = row(&app, loading);
    assert!(before.contains("▂▄▆ ▄▂▁"), "{before}");
    assert!(after.contains("▃▁▁ ▃▅▅"), "{after}");
    assert_eq!(
        after.chars().skip(8).collect::<String>(),
        before.chars().skip(8).collect::<String>(),
        "the wave never shifts its label or timer"
    );
    let screen = app.world().resource::<TuiTestScreen>();
    let next_color = screen.buffer().cell(first).unwrap().fg;
    assert_ne!(next_color, first_color);
    let mut frames = std::collections::BTreeSet::new();
    for phase in 0..12 {
        age(&mut app, loading, Duration::from_millis(phase * 120));
        let text = row(&app, loading);
        let icon = text.chars().skip(1).take(7).collect::<String>();
        assert_eq!(icon.chars().nth(3), Some(' '), "{icon}");
        assert_eq!(
            icon.chars().filter(|ch| "▁▂▃▄▅▆".contains(*ch)).count(),
            6,
            "every frame has six bars and no enclosed dot: {icon}"
        );
        assert_eq!(text.find("reviewing"), before.find("reviewing"));
        frames.insert(icon);
    }
    assert_eq!(frames.len(), 12, "the crest travels throughout the loop");
    age(&mut app, loading, Duration::from_millis(1440));
    assert_eq!(
        row(&app, loading)
            .chars()
            .skip(1)
            .take(7)
            .collect::<String>(),
        before.chars().skip(1).take(7).collect::<String>(),
        "the cycle loops without an empty or reset frame"
    );

    let child = app.world_mut().spawn(Agent).id();
    work(&mut app, child, "compacting");
    app.world_mut().resource_mut::<ShownAgent>().0 = Some(child);
    app.update();
    let run = app
        .world()
        .get::<LoadingLine>(loading)
        .unwrap()
        .0
        .as_ref()
        .unwrap();
    assert_eq!(run.agent, child);
    assert_eq!(run.seconds, 0);
    assert!(row(&app, loading).contains("compacting"));
    assert_eq!(status_text(&app, agent).as_deref(), Some("reviewing…"));
    assert!(status_text(&app, child).is_none());

    app.world_mut().entity_mut(child).remove::<Activity>();
    app.update();
    assert!(area(&app, loading).is_empty());
    assert!(app.world().get::<KeepAwake>(loading).is_none());
    assert_eq!(status_text(&app, child).as_deref(), Some("compacting…"));
}

#[test]
fn reduced_motion_questions_and_small_screens_keep_an_honest_safe_status() {
    let (mut app, agent, _, editor, loading) = app("[tui_loading]\nanimate = false");
    let plain = app.world().resource::<Theme>().without_color();
    app.insert_resource(plain);
    work(&mut app, agent, "scanning\n界面\u{1b}[31m");
    app.update();
    let initial = row(&app, loading);
    // The test screen's rows include the empty continuation cell of each
    // wide glyph. Check the owned label too, without assuming those cells
    // disappear in its textual diagnostic representation.
    assert!(initial.contains("scanning 界"), "{initial}");
    assert!(initial.contains("▂▄▆ ▆▄▂"), "{initial}");
    assert!(initial.contains("[31m"), "{initial}");
    assert_eq!(
        app.world()
            .get::<LoadingLine>(loading)
            .unwrap()
            .0
            .as_ref()
            .unwrap()
            .label,
        "scanning 界面 [31m"
    );
    assert_eq!(
        app.world().get::<KeepAwake>(loading).unwrap().0,
        Duration::from_secs(1)
    );
    let loading_area = area(&app, loading);
    let screen = app.world().resource::<TuiTestScreen>();
    let styles = (loading_area.x..loading_area.x + 19)
        .map(|x| {
            let cell = screen.buffer().cell((x, loading_area.y)).unwrap();
            assert_eq!(cell.fg, Color::Reset);
            assert_eq!(cell.bg, Color::Reset);
            cell.modifier
        })
        .collect::<Vec<_>>();
    age(&mut app, loading, Duration::from_secs(2));
    assert!(row(&app, loading).contains("2s"));
    let screen = app.world().resource::<TuiTestScreen>();
    let after = (loading_area.x..loading_area.x + 19)
        .map(|x| screen.buffer().cell((x, loading_area.y)).unwrap().modifier)
        .collect::<Vec<_>>();
    assert_eq!(
        after, styles,
        "reduced motion keeps the bar mark completely static"
    );

    let ask = app
        .world_mut()
        .spawn((Ask::yes_no("Keep the changes?"), ForAgent(agent)))
        .id();
    app.world_mut().resource_mut::<LoadingSettings>().animate = true;
    app.update();
    assert!(
        app.world()
            .get::<LoadingLine>(loading)
            .unwrap()
            .0
            .as_ref()
            .unwrap()
            .waiting
    );
    assert!(app.world().get::<KeepAwake>(loading).is_none());
    let asking = row(&app, loading);
    assert!(asking.contains("asking: Keep the changes?"), "{asking}");
    assert!(asking.contains("▂▄▆ ▆▄▂"), "{asking}");
    age(&mut app, loading, Duration::from_secs(70));
    assert_eq!(
        row(&app, loading),
        asking,
        "questions are static and show no running clock"
    );
    app.world_mut()
        .entity_mut(ask)
        .insert(Outcome::custom("dismissed"));
    app.update();

    app.world_mut()
        .resource_mut::<TuiTestScreen>()
        .resize(26, 12);
    app.update();
    age(&mut app, loading, Duration::ZERO);
    let narrow = row(&app, loading);
    assert!(narrow.contains("▄▄▁"), "{narrow}");
    assert!(
        narrow.contains("scanning"),
        "compact icon leaves room for the label: {narrow}"
    );
    let mut frames = std::collections::BTreeSet::new();
    for phase in 0..12 {
        age(&mut app, loading, Duration::from_millis(phase * 120));
        let text = row(&app, loading);
        let icon = text.chars().skip(1).take(3).collect::<String>();
        assert!(icon.chars().all(|ch| "▁▂▃▄▅▆".contains(ch)), "{icon}");
        assert_eq!(text.find("scanning"), narrow.find("scanning"));
        frames.insert(icon);
    }
    assert_eq!(frames.len(), 12, "narrow NO_COLOR mode still shows motion");
    app.world_mut().resource_mut::<LoadingSettings>().animate = false;
    app.update();
    assert!(row(&app, loading).contains("▂▆▂"));
    app.world_mut().resource_mut::<LoadingSettings>().animate = true;
    app.update();
    assert_eq!(area(&app, loading).bottom(), area(&app, editor).y);
    assert!(status_text(&app, agent).is_none());

    // Hide the banner too: a visible top logo is allowed to share the clock
    // even if a tiny screen suppresses the separate activity row.
    app.world_mut()
        .get_mut::<Conversation>(agent)
        .unwrap()
        .push(Message::assistant("ordinary output\n\n".repeat(20)));
    app.world_mut()
        .resource_mut::<TuiTestScreen>()
        .resize(26, 6);
    app.update();
    assert!(
        area(&app, loading).is_empty(),
        "keep Rig's minimum transcript height"
    );
    assert!(
        status_text(&app, agent).is_some(),
        "a suppressed replacement must restore the original status"
    );
    assert!(app.world().get::<KeepAwake>(loading).is_none());
    app.world_mut()
        .resource_mut::<TuiTestScreen>()
        .resize(26, 12);
    app.update();
    assert_eq!(area(&app, loading).height, 1);
    assert_eq!(app.world().get::<KeepAwake>(loading).unwrap().0, STEP);
    assert!(
        status_text(&app, agent).is_none(),
        "resizing back relocates unchanged activity again"
    );
}

fn native_logo_cells(app: &App, agent: Entity, pane: Entity) -> Vec<(u16, u16)> {
    let drawn = app.world().get::<DrawnTranscript>(pane).unwrap();
    let conversation = app.world().get::<Conversation>(agent).unwrap();
    let (_, range) = drawn
        .items()
        .iter()
        .find(|(item, _)| {
            matches!(item, TranscriptItem::Aside(id) if conversation.aside(*id)
            .is_some_and(|aside| aside.kind == rig_basics::welcome::WELCOME))
        })
        .unwrap();
    let mut cells = Vec::new();
    for index in range.clone() {
        let row = &drawn.rows()[index];
        if !drawn.on_screen().contains(&index)
            || ![
                " █▙ █▙ █▙  ▟█ ▟█ ▟█",
                " ██ ██ ██  ██ ██ ██",
                " █▛ █▛ █▛  ▜█ ▜█ ▜█",
            ]
            .iter()
            .any(|prefix| row.text.starts_with(prefix))
        {
            continue;
        }
        let y = drawn.area().y + (index - drawn.on_screen().start) as u16;
        for x in [1, 2, 4, 5, 7, 8, 11, 12, 14, 15, 17, 18] {
            cells.push((drawn.area().x + x, y));
        }
    }
    cells
}

#[test]
fn the_actual_visible_banner_pulses_without_touching_metadata_lookalikes_or_render_cache() {
    let (mut app, agent, pane, _, loading) = app("");
    // Logo-shaped ordinary text must not be mistaken for the native aside.
    app.world_mut()
        .get_mut::<Conversation>(agent)
        .unwrap()
        .push(Message::assistant(
            " █▙ █▙ █▙  ▟█ ▟█ ▟█\n\n ██ ██ ██  ██ ██ ██\n\n █▛ █▛ █▛  ▜█ ▜█ ▜█",
        ));
    app.update();
    let cells = native_logo_cells(&app, agent, pane);
    assert_eq!(cells.len(), 36, "animate the complete three-row mark");
    let original = app.world().resource::<TuiTestScreen>().buffer().clone();
    let messages = app
        .world()
        .get::<Conversation>(agent)
        .unwrap()
        .messages()
        .to_vec();
    let asides = app
        .world()
        .get::<Conversation>(agent)
        .unwrap()
        .asides()
        .map(|(id, aside)| (id, aside.clone()))
        .collect::<Vec<_>>();
    let effect = work(&mut app, agent, "thinking");
    app.update();
    age(&mut app, loading, Duration::ZERO);
    let before = app.world().resource::<TuiTestScreen>().buffer().clone();
    let revision = app.world().resource::<RenderRevision>().0;
    age(&mut app, loading, Duration::from_millis(600));
    let after = app.world().resource::<TuiTestScreen>().buffer();
    let mut changed = 0;
    for position in before.area.positions() {
        let left = before.cell(position).unwrap();
        let right = after.cell(position).unwrap();
        if cells.contains(&(position.x, position.y)) {
            changed += usize::from(left.fg != right.fg);
            assert_eq!(left.symbol(), right.symbol());
            assert_eq!(left.bg, right.bg);
            assert_eq!(left.modifier, right.modifier);
        } else if position.y != area(&app, loading).y {
            assert_eq!(
                left, right,
                "only actual logo cells may pulse: {position:?}"
            );
        }
    }
    assert!(changed > 0);
    assert!(app.world().resource::<welcome::VisibleLogo>().0);
    assert_eq!(app.world().resource::<RenderRevision>().0, revision);
    assert_eq!(
        app.world().get::<Conversation>(agent).unwrap().messages(),
        messages
    );
    assert_eq!(
        app.world()
            .get::<Conversation>(agent)
            .unwrap()
            .asides()
            .map(|(id, aside)| (id, aside.clone()))
            .collect::<Vec<_>>(),
        asides
    );

    app.world_mut()
        .entity_mut(effect)
        .insert(Outcome::custom("done"));
    app.update();
    assert!(!app.world().resource::<welcome::VisibleLogo>().0);
    assert!(app.world().get::<KeepAwake>(loading).is_none());
    for position in &cells {
        assert_eq!(
            app.world()
                .resource::<TuiTestScreen>()
                .buffer()
                .cell(*position),
            original.cell(*position),
            "idle restores the native logo's original style"
        );
    }

    work(&mut app, agent, "thinking");
    app.world_mut()
        .get_mut::<Conversation>(agent)
        .unwrap()
        .push(Message::assistant("ordinary output\n\n".repeat(80)));
    app.update();
    assert!(
        !app.world().resource::<welcome::VisibleLogo>().0,
        "do not paint an offscreen banner"
    );
    app.world_mut()
        .resource_mut::<TuiTestScreen>()
        .resize(26, 6);
    app.update();
    assert!(area(&app, loading).is_empty());
    assert!(app.world().get::<KeepAwake>(loading).is_none());
    let welcome = asides
        .iter()
        .find(|(_, aside)| aside.kind == rig_basics::welcome::WELCOME)
        .unwrap()
        .0;
    app.world_mut().trigger(ScrollTo {
        pane,
        item: Some(TranscriptItem::Aside(welcome)),
        row: ItemRow::Top(1),
    });
    app.update();
    let partial = native_logo_cells(&app, agent, pane);
    assert!(
        !partial.is_empty() && partial.len() < 36,
        "exercise a partially visible logo"
    );
    assert!(app.world().resource::<welcome::VisibleLogo>().0);
    assert_eq!(
        app.world().get::<KeepAwake>(loading).unwrap().0,
        STEP,
        "a visible banner shares the clock even if the thinking row cannot fit"
    );
    app.world_mut().resource_mut::<LoadingSettings>().animate = false;
    app.update();
    assert!(!app.world().resource::<welcome::VisibleLogo>().0);
    assert!(app.world().get::<KeepAwake>(loading).is_none());
    let plain = app.world().resource::<Theme>().without_color();
    app.insert_resource(plain);
    app.world_mut().resource_mut::<LoadingSettings>().animate = true;
    app.update();
    assert!(app.world().resource::<welcome::VisibleLogo>().0);
    for position in native_logo_cells(&app, agent, pane) {
        assert_eq!(
            app.world()
                .resource::<TuiTestScreen>()
                .buffer()
                .cell(position)
                .unwrap()
                .fg,
            Color::Reset
        );
    }
}
