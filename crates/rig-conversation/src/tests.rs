//! Verify prompt delivery and ordinary assistant-text/tool-loop behavior.

use std::time::Instant;

use bevy_app::TaskPoolPlugin;
use rig_core::{
    catalog::{Catalog, Connector, ModelSpec, Transport},
    completion::{CompletionRequest, Section, SystemPrompt as PromptParts},
    message::Message,
    operation::Completion,
    providers::registry::ProviderId,
    serve::{ErasedHandler, adapters::ModelAdapter},
    test_utils::{MockCompletionModel, MockStreamEvent},
};
use serde_json::{Value, json};

use super::*;

fn prepare_app() -> App {
    let mut app = App::new();
    // Exercise the real Prepare systems without admitting or launching effects.
    // AgentPlugin owns this same phase order in the production schedule.
    app.configure_sets(
        AgentPass,
        (PrepareSystems::Compose, PrepareSystems::Edit).chain(),
    )
    .add_plugins(ConversationPlugin);
    app
}

fn request(role: &str) -> ModelRequest {
    ModelRequest {
        prompt: Some(PromptParts {
            role: role.to_owned(),
            sections: vec![Section::new("project_instructions", "Never publish.")],
        }),
        ..ModelRequest::new(CompletionRequest::new("Investigate the parser."))
    }
}

#[test]
fn prepare_replaces_only_the_stock_quiet_rule_and_preserves_sections() {
    let mut app = prepare_app();
    // The stock Compose system supplies the empty role before our Edit system.
    let call = app
        .world_mut()
        .spawn((request(""), Admitted, TurnCall))
        .id();
    app.world_mut().run_schedule(AgentPass);

    let prepared = app.world().get::<ModelRequest>(call).unwrap();
    let prompt = prepared.prompt.as_ref().unwrap();
    assert!(DEFAULT_SYSTEM_PROMPT.contains(QUIET_RULE));
    assert_eq!(
        prompt.role,
        DEFAULT_SYSTEM_PROMPT.replacen(QUIET_RULE, PROGRESS_GUIDANCE.trim(), 1)
    );
    assert!(!prompt.role.contains(QUIET_RULE));
    assert_eq!(
        prompt.sections,
        [Section::new("project_instructions", "Never publish.")]
    );
    assert_eq!(prepared.request.chat_history.len(), 1);
    assert!(
        matches!(&*prepared.request.chat_history[0], Message::User { content } if content.iter().any(|part| {
            matches!(part, UserContent::Text(text) if text.text == "Investigate the parser.")
        }))
    );

    // A later ordinary call receives the same guidance, not accumulated copies.
    let next = app
        .world_mut()
        .spawn((request(""), Admitted, TurnCall))
        .id();
    app.world_mut().run_schedule(AgentPass);
    assert_eq!(
        app.world().get::<ModelRequest>(call).unwrap().prompt,
        app.world().get::<ModelRequest>(next).unwrap().prompt
    );
}

#[test]
fn prepare_leaves_custom_roles_and_structured_side_calls_unchanged() {
    let mut app = prepare_app();
    // Similar wording is still an explicit custom role, not ours to rewrite.
    let custom_role = format!("You are a specialist.\n{QUIET_RULE}");
    let custom = app
        .world_mut()
        .spawn((request(&custom_role), Admitted, TurnCall))
        .id();
    let agent = app.world_mut().spawn_empty().id();
    let side = app
        .world_mut()
        .spawn((request(DEFAULT_SYSTEM_PROMPT), Admitted, SideCall(agent)))
        .id();
    app.world_mut().run_schedule(AgentPass);

    for (call, role) in [
        (custom, custom_role.as_str()),
        (side, DEFAULT_SYSTEM_PROMPT),
    ] {
        let prompt = app
            .world()
            .get::<ModelRequest>(call)
            .unwrap()
            .prompt
            .as_ref()
            .unwrap();
        assert_eq!(prompt.role, role);
        assert_eq!(
            prompt.sections,
            [Section::new("project_instructions", "Never publish.")]
        );
    }
}

#[derive(Resource, Default)]
struct ToolRuns(usize);

#[test]
fn progress_text_stays_before_the_tool_and_reaches_the_next_model_call() {
    let progress = "I found the parser entry point; I’ll check its result next.";
    let detail = "PARSER_DETAIL_417";
    let final_text = "The parser check passed.";
    let model = MockCompletionModel::from_stream_turns([
        vec![
            MockStreamEvent::text(progress),
            MockStreamEvent::tool_call("probe-call", "conversation_probe", json!({})),
            MockStreamEvent::FinalResponse(Default::default()),
        ],
        vec![
            MockStreamEvent::text(final_text),
            MockStreamEvent::FinalResponse(Default::default()),
        ],
    ]);
    let provided = model.clone();
    let vendor = ProviderId::declare("conversation-fixture").unwrap();
    let mut connector = Connector::new(Catalog::default());
    connector
        .catalog_mut()
        .insert(ModelSpec::new(vendor, "fixed").with_tools(true));
    connector.provide(vendor, move |_: &ModelSpec, _: &Catalog, _: &Transport| {
        Ok(ErasedHandler::new(ModelAdapter::<Completion>::new(
            "conversation-fixture/fixed",
            provided.clone(),
        )))
    });
    let mut app = App::new();
    app.insert_resource(Models(connector))
        .init_resource::<ToolRuns>()
        .add_plugins((TaskPoolPlugin::default(), AgentPlugin, ConversationPlugin))
        .add_open_tool(
            "conversation_probe",
            "Return a local parser detail.",
            ToolOptions {
                footprint: Some(Touches::Independent),
            },
            move |called: On<ToolCalled<Value>>,
                  mut runs: ResMut<ToolRuns>,
                  mut commands: Commands| {
                runs.0 += 1;
                commands
                    .entity(called.call)
                    .insert(Outcome::from(ToolResult::success(detail.into())));
            },
        );
    app.finish();
    app.cleanup();
    let agent = app
        .world_mut()
        .spawn((Agent, ModelChoice("conversation-fixture/fixed".into())))
        .id();
    app.world_mut().trigger(Deliver::new(
        agent,
        "Check the parser.",
        DeliveryMode::Steer,
    ));
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        app.update();
        let answered = app
            .world()
            .get::<Conversation>(agent)
            .is_some_and(|conversation| {
                conversation.messages().last().is_some_and(|message| {
                matches!(&**message, Message::Assistant(reply) if reply.text() == final_text)
            })
            });
        if answered && app.world().get::<ActiveTurn>(agent).is_none() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "scripted conversation did not finish"
        );
        std::thread::sleep(Duration::from_millis(1));
    }

    assert_eq!(app.world().resource::<ToolRuns>().0, 1);
    let conversation = app.world().get::<Conversation>(agent).unwrap();
    let messages = conversation.messages();
    assert_eq!(
        messages.len(),
        4,
        "no synthetic progress messages or extra turns"
    );
    let Message::Assistant(first) = &*messages[1] else {
        panic!("expected progress and tool call in the first assistant response");
    };
    assert!(matches!(&first.content[0], AssistantContent::Text(text) if text.text == progress));
    assert!(
        matches!(&first.content[1], AssistantContent::ToolCall(call) if call.function.name.as_str() == "conversation_probe")
    );
    assert!(
        matches!(&*messages[2], Message::User { content } if content.iter().any(|part| {
            matches!(part, UserContent::ToolResult(result) if result.content.iter().any(|part| part.as_text() == Some(detail)))
        }))
    );
    assert!(matches!(&*messages[3], Message::Assistant(reply) if reply.text() == final_text));

    let requests = model.requests();
    assert_eq!(
        requests.len(),
        2,
        "progress does not add a separate model call"
    );
    for request in &requests {
        let prompt = request.system_instructions().unwrap();
        assert!(prompt.contains(PROGRESS_GUIDANCE.trim()));
        assert!(!prompt.contains(QUIET_RULE));
    }
    let history = &requests[1].chat_history;
    let progress_at = history
        .iter()
        .position(
            |message| matches!(&**message, Message::Assistant(reply) if reply.text() == progress),
        )
        .expect("next model call retains public progress");
    let result_at = history.iter().position(|message| {
        matches!(&**message, Message::User { content } if content.iter().any(|part| {
            matches!(part, UserContent::ToolResult(result) if result.content.iter().any(|part| part.as_text() == Some(detail)))
        }))
    }).expect("next model call retains the actual tool result");
    assert!(progress_at < result_at);
}
