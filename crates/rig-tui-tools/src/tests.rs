use rig_harness::ask::AskPlugin;
use rig_harness::pick::{PickPlugin, Picker};
use rig_harness::prelude::{Agent, AgentPlugin, Conversation, Invoked, Settings};
use rig_harness::rig_core::completion::{AssistantContent, AssistantMessage, Message};
use rig_harness::rig_core::message::{ToolCall, ToolFunction, ToolName, UserContent};
use rig_harness::rig_core::tool::{ToolOutput, ToolResult};
use rig_tui::crossterm::event::{
    Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use rig_tui::{
    DrawnTranscript, Editor, ItemRow, KeyFocus, KeyInput, KeyPressed, MainEditor, MainPane,
    PendingInput, RenderBatch, RequestRedraw, ScrollTo, TranscriptItem, TranscriptLayout,
    TranscriptView, TuiPanel, TuiPlugin, TuiTestScreen,
};
use serde_json::{Value, json};

use super::*;

#[derive(Resource, Default)]
struct PanelPresses(usize);

fn app(settings: &str, width: u16) -> (App, Entity, Entity, Entity) {
    let mut app = App::new();
    app.set_error_handler(rig_harness::error::panic)
        .insert_resource(Invoked::default())
        .insert_resource(Settings::from_toml(settings))
        .add_plugins((
            AgentPlugin,
            AskPlugin,
            PickPlugin,
            TuiPlugin,
            TuiToolsPlugin,
        ));
    rig_tui::add_view(&mut app);
    app.insert_resource(TuiTestScreen::new(width, 30));
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
    assert!(
        world.get::<DrawnTranscript>(pane).is_some(),
        "mouse hit testing installs its own snapshot"
    );
    (app, agent, pane, editor)
}

fn call(name: &str, id: &str, args: Value) -> ToolCall {
    ToolCall::from_wire(id, ToolFunction::new(ToolName::new(name).unwrap(), args))
}

fn push(app: &mut App, agent: Entity, call: &ToolCall, value: Value) {
    let mut conversation = app.world_mut().get_mut::<Conversation>(agent).unwrap();
    conversation.push(Message::Assistant(AssistantMessage::new(vec![
        AssistantContent::ToolCall(call.clone()),
    ])));
    conversation.push(Message::User {
        content: vec![UserContent::ToolResult(
            call.answer(&ToolResult::success(ToolOutput::json(value))),
        )],
    });
}

fn text(app: &App, pane: Entity, call: &ToolCall) -> String {
    app.world()
        .get::<DrawnTranscript>(pane)
        .unwrap()
        .item(&TranscriptItem::Call(call.id.clone()))
        .unwrap()
        .lines()
        .map(|line| line.text)
        .collect::<Vec<_>>()
        .join("\n")
}

fn press(app: &mut App, modifiers: KeyModifiers) {
    app.world_mut()
        .resource_mut::<PendingInput>()
        .0
        .push(KeyInput::Event(Event::Key(KeyEvent::new(
            KeyCode::Char('o'),
            modifiers,
        ))));
    app.world_mut().write_message(RequestRedraw);
    app.update();
}

/// Use the real transcript and typed JSON results. The stock generic renderer
/// drops JSON results entirely and escapes script newlines into its header.
#[test]
fn script_tools_show_source_inputs_files_and_typed_results_without_changing_history() {
    let (mut app, agent, pane, _) = app("", 100);
    let cases = [
        (
            "run_xonsh",
            json!({"code":"target = \"work\"\nlisting = $(ls -1 @(target))\nlisting", "backend":"virtual", "files":{"work/note.txt":"one\ntwo"}}),
            json!({"value":{"entries":["note.txt"]}, "stdout":"xonsh printed\nsecond line", "stderr":"", "output_truncated":false}),
        ),
        (
            "run_monty",
            json!({"code":"total = sum(items)\ntotal", "inputs":{"items":[3,14,15]}}),
            json!({"value":32,"stdout":"","stderr":"","output_truncated":false}),
        ),
        (
            "native_shell",
            json!({"script":"printf 'one\\ntwo\\n'", "cwd":"work", "timeout_secs":10}),
            json!({"output":"native first\nnative second\n","status":{"exited":0},"truncated":false}),
        ),
        (
            "virtual_bash",
            json!({"script":"cat /workspace/note.txt", "files":[{"path":"note.txt","content":"alpha\nbeta"}]}),
            json!({"stdout":"virtual result\n","stderr":"","exit_code":0,"stdout_truncated":false,"stderr_truncated":false}),
        ),
    ];
    let calls: Vec<_> = cases
        .into_iter()
        .enumerate()
        .map(|(index, (name, args, output))| {
            let call = call(name, &format!("call-{index}"), args);
            push(&mut app, agent, &call, output);
            call
        })
        .collect();
    let original = app
        .world()
        .get::<Conversation>(agent)
        .unwrap()
        .messages()
        .to_vec();
    app.update();
    let xonsh = text(&app, pane, &calls[0]);
    assert!(xonsh.contains("listing = $(ls -1 @(target))"), "{xonsh}");
    assert!(!xonsh.contains(r#"\"code\":"#), "{xonsh}");
    assert!(
        xonsh.contains("file work/note.txt\n   one\n   two"),
        "{xonsh}"
    );
    assert!(xonsh.contains("xonsh printed\n   second line"), "{xonsh}");
    assert!(xonsh.contains("\"entries\""), "{xonsh}");
    let monty = text(&app, pane, &calls[1]);
    assert!(monty.contains("inputs") && monty.contains("32"), "{monty}");
    let native = text(&app, pane, &calls[2]);
    assert!(
        native.contains("exit 0") && native.contains("native second"),
        "{native}"
    );
    let virtual_bash = text(&app, pane, &calls[3]);
    assert!(
        virtual_bash.contains("file note.txt\n   alpha\n   beta"),
        "{virtual_bash}"
    );
    assert!(virtual_bash.contains("virtual result"), "{virtual_bash}");
    assert_eq!(
        app.world().get::<Conversation>(agent).unwrap().messages(),
        original
    );
}

/// Exercise real keys, cached redraws and modal ownership. The row budget must
/// also hold when a single Unicode line wraps across a narrow pane.
#[test]
fn folding_and_expansion_target_selected_or_latest_call_and_preserve_draft() {
    let (mut app, agent, pane, editor) = app("[tui_tools]\ninput_rows=3\noutput_rows=2", 54);
    let source = format!("{}\nsource_end", "界🙂 value ".repeat(100));
    let first = call("run_xonsh", "first", json!({"code":source}));
    let latest = call("run_monty", "latest", json!({"code":"1 + 2"}));
    push(
        &mut app,
        agent,
        &first,
        json!({"stdout":"a\nb\nc\nd", "value":5}),
    );
    push(&mut app, agent, &latest, json!({"value":3}));
    app.world_mut()
        .get_mut::<Editor>(editor)
        .unwrap()
        .set("unfinished draft");
    app.update();
    let drawn = app.world().get::<DrawnTranscript>(pane).unwrap();
    let preview = drawn.item(&TranscriptItem::Call(first.id.clone())).unwrap();
    // Header + at most 3 input and 2 output rows + one marker per section.
    assert!(preview.rows().len() <= 8, "{:?}", preview.rows());
    assert!(!text(&app, pane, &first).contains("source_end"));
    press(&mut app, KeyModifiers::CONTROL);
    assert!(text(&app, pane, &latest).contains("▸"));
    assert!(!text(&app, pane, &first).contains("▸"));
    app.world_mut()
        .get_mut::<TranscriptView>(pane)
        .unwrap()
        .selected = Some(TranscriptItem::Call(first.id.clone()));
    press(&mut app, KeyModifiers::ALT);
    assert!(text(&app, pane, &first).contains("source_end"));
    assert!(!text(&app, pane, &first).contains("more input"));
    press(&mut app, KeyModifiers::CONTROL);
    assert!(text(&app, pane, &first).contains("▸"));
    assert!(!text(&app, pane, &first).contains("source_end"));
    press(&mut app, KeyModifiers::CONTROL);
    assert!(text(&app, pane, &first).contains("more input"));
    assert_eq!(
        app.world().get::<Editor>(editor).unwrap().text(),
        "unfinished draft"
    );
    let before = text(&app, pane, &first);
    let picker = app
        .world_mut()
        .spawn(Picker::new("Choose").item("one", ""))
        .id();
    app.update();
    press(&mut app, KeyModifiers::CONTROL);
    assert_eq!(text(&app, pane, &first), before);
    app.world_mut().despawn(picker);
    let panel = app
        .world_mut()
        .spawn(TuiPanel::new(rig_tui::Placement::Left(
            rig_tui::ratatui::layout::Constraint::Length(0),
        )))
        .id();
    app.init_resource::<PanelPresses>();
    assert!(
        app.add_key_binding(
            KeyBinding::new(KeyPress::ctrl('o'), "Panel's own action")
                .in_scope(KeyScope::Panel(panel)),
            |_: On<KeyPressed>, mut presses: ResMut<PanelPresses>| presses.0 += 1,
        )
        .is_some()
    );
    app.world_mut().resource_mut::<KeyFocus>().0 = Some(panel);
    press(&mut app, KeyModifiers::CONTROL);
    assert_eq!(app.world().resource::<PanelPresses>().0, 1);
    assert_eq!(text(&app, pane, &first), before);
    // An export must never silently lose folded content.
    let export =
        TranscriptLayout::lay_out(app.world_mut(), &TranscriptView::default(), agent, 54).unwrap();
    assert!(
        export
            .rows()
            .iter()
            .any(|row| row.text.contains("source_end"))
    );
}

#[test]
fn a_folded_failure_keeps_exit_and_tool_truncation_visible() {
    let (mut app, agent, pane, _) = app("", 120);
    let failed = call("native_shell", "failed", json!({"script":"exit 7"}));
    push(
        &mut app,
        agent,
        &failed,
        json!({"output":"failure detail", "status":{"exited":7},"truncated":true}),
    );
    app.update();
    let open = text(&app, pane, &failed);
    assert!(
        open.contains("failure detail") && open.contains("cannot recover"),
        "{open}"
    );
    press(&mut app, KeyModifiers::CONTROL);
    let folded = text(&app, pane, &failed);
    assert!(
        folded.contains("exit 7") && folded.contains("truncated by tool"),
        "{folded}"
    );
    assert!(!folded.contains("failure detail"), "{folded}");
}

/// Folding is an edit of another tool's presentation, not a replacement for
/// its file/diff renderer. Both the latest and explicitly selected call work.
#[test]
fn ordinary_tool_previews_preserve_specialized_rows_and_export_full_content() {
    use rig_tui::ratatui::{style::Stylize, text::Line};

    let (mut app, agent, pane, editor) = app("[tui_tools]\noutput_rows=2", 100);
    app.add_systems(
        TranscriptRender,
        (|mut batch: ResMut<RenderBatch>| {
            for (input, output) in batch.calls_mut() {
                if matches!(input.call.function.name.as_str(), "read" | "edit") {
                    output.lines = vec![
                        input.view().header("File preview", "src/main.rs").into(),
                        Line::from("   - old value").red().into(),
                        Line::from("   + new value").green().into(),
                        Line::from("   specialized_tail").into(),
                    ];
                    output.decorate("file-details", vec![Line::from("   decorated_tail")]);
                }
            }
        })
        .in_set(RenderSystems::Render),
    );
    let read = call("read", "read-file", json!({"path":"src/main.rs"}));
    let edit = call("edit", "edit-file", json!({"path":"src/main.rs"}));
    push(&mut app, agent, &read, json!({"ok":true}));
    push(&mut app, agent, &edit, json!({"ok":true}));
    app.world_mut()
        .get_mut::<Editor>(editor)
        .unwrap()
        .set("do not submit this draft");
    let original = app
        .world()
        .get::<Conversation>(agent)
        .unwrap()
        .messages()
        .to_vec();
    app.update();
    for call in [&read, &edit] {
        let preview = text(&app, pane, call);
        assert!(
            preview.contains("File preview") && preview.contains("src/main.rs"),
            "{preview}"
        );
        assert!(
            preview.contains("- old value") && preview.contains("+ new value"),
            "{preview}"
        );
        assert!(!preview.contains("specialized_tail"), "{preview}");
    }
    press(&mut app, KeyModifiers::CONTROL);
    assert!(!text(&app, pane, &edit).contains("old value"));
    assert!(text(&app, pane, &read).contains("old value"));
    app.world_mut()
        .get_mut::<TranscriptView>(pane)
        .unwrap()
        .selected = Some(TranscriptItem::Decoration(
        read.id.clone(),
        render::DETAIL.to_owned(),
    ));
    press(&mut app, KeyModifiers::ALT);
    assert!(text(&app, pane, &read).contains("specialized_tail"));
    let decoration = TranscriptItem::Decoration(read.id.clone(), "file-details".to_owned());
    assert!(
        app.world()
            .get::<DrawnTranscript>(pane)
            .unwrap()
            .item(&decoration)
            .is_some()
    );
    press(&mut app, KeyModifiers::CONTROL);
    assert!(!text(&app, pane, &read).contains("old value"));
    let export =
        TranscriptLayout::lay_out(app.world_mut(), &TranscriptView::default(), agent, 100).unwrap();
    assert_eq!(
        export
            .rows()
            .iter()
            .filter(|row| row.text.contains("specialized_tail"))
            .count(),
        2
    );
    assert_eq!(
        export
            .rows()
            .iter()
            .filter(|row| row.text.contains("decorated_tail"))
            .count(),
        2,
    );
    assert_eq!(
        app.world().get::<Editor>(editor).unwrap().text(),
        "do not submit this draft"
    );
    assert_eq!(
        app.world().get::<Conversation>(agent).unwrap().messages(),
        original
    );
}

/// Unknown tools receive the same controls, and their typed results must not
/// disappear just because Rig's default look only understands text results.
#[test]
fn arbitrary_typed_result_boxes_have_bounded_previews_and_mouse_controls() {
    let (mut app, agent, pane, _) = app("[tui_tools]\noutput_rows=2", 100);
    let tool = call("lookup_record", "lookup", json!({"key":"sample"}));
    push(
        &mut app,
        agent,
        &tool,
        json!({"answer":42, "proof":"retained_json_tail"}),
    );
    app.update();
    let preview = text(&app, pane, &tool);
    assert!(!preview.contains("retained_json_tail"), "{preview}");
    let drawn = app.world().get::<DrawnTranscript>(pane).unwrap();
    assert!(
        drawn
            .item(&TranscriptItem::Call(tool.id.clone()))
            .unwrap()
            .rows()
            .len()
            <= 4
    );
    let detail = TranscriptItem::Decoration(tool.id.clone(), render::DETAIL.to_owned());
    let more = show_item(&mut app, pane, detail.clone(), 0);
    click(&mut app, more);
    let full = text(&app, pane, &tool);
    assert!(
        full.contains("answer") && full.contains("42") && full.contains("retained_json_tail"),
        "{full}"
    );
    let less = show_item(&mut app, pane, detail, 0);
    click(&mut app, less);
    assert_eq!(text(&app, pane, &tool), preview);
    let header = show_item(&mut app, pane, TranscriptItem::Call(tool.id.clone()), 0);
    click(&mut app, header);
    assert!(!text(&app, pane, &tool).contains("answer"));
    let header = show_item(&mut app, pane, TranscriptItem::Call(tool.id.clone()), 0);
    click(&mut app, header);
    assert_eq!(text(&app, pane, &tool), preview);
}

/// A display preview is recoverable; tool-side omission is not. The latter
/// must remain explicit even when its stock notice falls beyond the budget.
#[test]
fn ordinary_tool_truncation_facts_stay_visible_when_folded() {
    use rig_harness::rig_core::tool::{KeptEnd, OutputCut, ToolContext};
    use rig_tui::RenderInput;

    let (mut app, agent, pane, _) = app("[tui_tools]\noutput_rows=1", 120);
    app.add_systems(
        TranscriptRender,
        (|mut batch: ResMut<RenderBatch>| {
            for request in batch.requests.values_mut() {
                if let RenderInput::Tool(input) = &mut request.input {
                    let mut context = ToolContext::new();
                    context
                        .insert_result(OutputCut {
                            kept: KeptEnd::Start,
                            kept_lines: 3,
                            total_lines: 30,
                            spill: None,
                        })
                        .unwrap();
                    input.facts = Some(context.result_context());
                    request.output.lines = input.view().default_lines();
                }
            }
        })
        .in_set(RenderSystems::Render),
    );
    let tool = call("shell", "cut-output", json!({}));
    {
        let mut conversation = app.world_mut().get_mut::<Conversation>(agent).unwrap();
        conversation.push(Message::Assistant(AssistantMessage::new(vec![
            AssistantContent::ToolCall(tool.clone()),
        ])));
        conversation.push(Message::User {
            content: vec![UserContent::ToolResult(tool.answer(&ToolResult::success(
                ToolOutput::text("one\ntwo\nthree\n[tool cut notice]"),
            )))],
        });
    }
    app.update();
    assert!(text(&app, pane, &tool).contains("27 lines omitted"));
    assert!(!text(&app, pane, &tool).contains("three"));
    press(&mut app, KeyModifiers::CONTROL);
    let folded = text(&app, pane, &tool);
    assert!(
        folded.contains("27 lines omitted") && folded.contains("▸"),
        "{folded}"
    );
    press(&mut app, KeyModifiers::ALT);
    let full = text(&app, pane, &tool);
    assert!(
        full.contains("three") && full.contains("27 lines omitted"),
        "{full}"
    );
}

fn pointer(app: &mut App, position: (u16, u16), kinds: &[MouseEventKind], modifiers: KeyModifiers) {
    let events = kinds.iter().map(|kind| {
        KeyInput::Event(Event::Mouse(MouseEvent {
            kind: *kind,
            column: position.0,
            row: position.1,
            modifiers,
        }))
    });
    app.world_mut()
        .resource_mut::<PendingInput>()
        .0
        .extend(events);
    app.world_mut().write_message(RequestRedraw);
    app.update();
}

fn click(app: &mut App, position: (u16, u16)) {
    pointer(
        app,
        position,
        &[
            MouseEventKind::Down(MouseButton::Left),
            MouseEventKind::Up(MouseButton::Left),
        ],
        KeyModifiers::NONE,
    );
}

fn show_item(app: &mut App, pane: Entity, item: TranscriptItem, offset: usize) -> (u16, u16) {
    app.world_mut().trigger(ScrollTo {
        pane,
        item: Some(item.clone()),
        row: ItemRow::Top(0),
    });
    app.update();
    let drawn = app.world().get::<DrawnTranscript>(pane).unwrap();
    let row = drawn.rows_of(&item).unwrap().start + offset;
    assert!(drawn.on_screen().contains(&row));
    (
        drawn.area().x + 1,
        drawn.area().y + u16::try_from(row - drawn.on_screen().start).unwrap(),
    )
}

/// Click through the public input pipeline using coordinates from actual
/// scrolled, wrapped frames. Source that resembles a control is still source.
#[test]
fn mouse_headers_and_detail_controls_follow_scrolled_rows_and_ignore_drags_and_modals() {
    let (mut app, agent, pane, editor) = app("[tui_tools]\ninput_rows=3\noutput_rows=2", 54);
    app.world_mut()
        .get_mut::<Conversation>(agent)
        .unwrap()
        .push(Message::assistant("earlier\n\n".repeat(40)));
    let first = call(
        "run_xonsh",
        "older-call",
        json!({"code":"… more input · click or Alt+O show all\nsecond\nthird\nfourth\nsource_end"}),
    );
    let later = call("run_monty", "newer-call", json!({"code":"1 + 2"}));
    push(&mut app, agent, &first, json!({"value":42}));
    push(&mut app, agent, &later, json!({"value":3}));
    app.world_mut()
        .get_mut::<Conversation>(agent)
        .unwrap()
        .push(Message::assistant("later\n\n".repeat(40)));
    app.world_mut()
        .get_mut::<Editor>(editor)
        .unwrap()
        .set("keep my draft");
    app.update();
    let original = app
        .world()
        .get::<Conversation>(agent)
        .unwrap()
        .messages()
        .to_vec();
    let later_before = text(&app, pane, &later);
    let item = TranscriptItem::Call(first.id.clone());
    let header = show_item(&mut app, pane, item.clone(), 0);
    assert!(
        app.world()
            .get::<DrawnTranscript>(pane)
            .unwrap()
            .on_screen()
            .start
            > 0
    );
    let body = (header.0, header.1 + 1);
    let preview = text(&app, pane, &first);
    click(&mut app, body);
    assert_eq!(
        text(&app, pane, &first),
        preview,
        "source text is not a control"
    );

    // A drag is not a click, even if its release returns to the header.
    pointer(
        &mut app,
        header,
        &[
            MouseEventKind::Down(MouseButton::Left),
            MouseEventKind::Drag(MouseButton::Left),
            MouseEventKind::Up(MouseButton::Left),
        ],
        KeyModifiers::NONE,
    );
    assert_eq!(text(&app, pane, &first), preview);
    pointer(
        &mut app,
        header,
        &[
            MouseEventKind::Down(MouseButton::Left),
            MouseEventKind::Up(MouseButton::Left),
        ],
        KeyModifiers::SHIFT,
    );
    assert_eq!(text(&app, pane, &first), preview);
    click(&mut app, header);
    assert!(text(&app, pane, &first).contains("▸"));

    // The folded header wraps at this width; its continuation also opens it.
    let wrapped_header = show_item(&mut app, pane, item.clone(), 1);
    click(&mut app, wrapped_header);
    assert_eq!(text(&app, pane, &first), preview);
    let detail = TranscriptItem::Decoration(first.id.clone(), render::DETAIL.to_owned());
    let more = show_item(&mut app, pane, detail.clone(), 0);
    click(&mut app, more);
    assert!(text(&app, pane, &first).contains("source_end"));
    let less = show_item(&mut app, pane, detail, 0);
    click(&mut app, less);
    assert_eq!(text(&app, pane, &first), preview);

    let header = show_item(&mut app, pane, item, 0);
    app.world_mut()
        .spawn(Picker::new("Owns the mouse").item("one", ""));
    app.update();
    click(&mut app, header);
    assert_eq!(text(&app, pane, &first), preview);
    assert_eq!(
        text(&app, pane, &later),
        later_before,
        "clicking the older call must not toggle the newest"
    );
    assert_eq!(
        app.world().get::<Editor>(editor).unwrap().text(),
        "keep my draft"
    );
    assert_eq!(
        app.world().get::<Conversation>(agent).unwrap().messages(),
        original
    );
}
