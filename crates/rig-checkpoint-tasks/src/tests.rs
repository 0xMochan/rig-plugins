//! Drive the real task/child/model lifecycle with scripted local models.

use std::{sync::Arc, time::Instant};

use bevy_app::TaskPoolPlugin;
use rig_basics::subagents::{Lifecycle, SubagentPolicy};
use rig_checkpoint::CheckpointContext;
use rig_core::{
    catalog::{Catalog, Connector, ModelSpec, Transport},
    completion::Reasoning,
    message::{Message, ToolFunction, ToolName},
    operation::Completion,
    providers::registry::ProviderId,
    serve::{ErasedHandler, adapters::ModelAdapter},
    test_utils::{MockCompletionModel, MockStreamEvent},
};
use rig_harness::rig_ecs::session_log::SessionStore;
use rig_harness::rig_memory::MemoryStore;
use serde_json::{Value, json};

use super::*;

fn reply(text: &str) -> Vec<MockStreamEvent> {
    vec![
        MockStreamEvent::text(text),
        MockStreamEvent::FinalResponse(Default::default()),
    ]
}

// Inspect only message text and tool output, never serialized call arguments.
fn contains_text(message: &Message, needle: &str) -> bool {
    match message {
        Message::User { content } => content.iter().any(|part| match part {
            UserContent::Text(text) => text.text.contains(needle),
            UserContent::ToolResult(result) => result
                .content
                .iter()
                .filter_map(|part| part.as_text())
                .any(|text| text.contains(needle)),
            _ => false,
        }),
        Message::Assistant(reply) => reply
            .content
            .iter()
            .any(|part| matches!(part, AssistantContent::Text(text) if text.text.contains(needle))),
        _ => false,
    }
}

struct Fixture {
    app: App,
    root: Entity,
    model: MockCompletionModel,
    original: Vec<Arc<Message>>,
    dir: tempfile::TempDir,
}

impl Fixture {
    fn new(store: &MemoryStore, turns: Vec<Vec<MockStreamEvent>>) -> Self {
        let model = MockCompletionModel::from_stream_turns(turns);
        let provided = model.clone();
        let vendor = ProviderId::declare("checkpoint-fixture").expect("fixture provider");
        let mut connector = Connector::new(Catalog::default());
        connector
            .catalog_mut()
            .insert(ModelSpec::new(vendor, "fixed").with_tools(true));
        connector.provide(vendor, move |_: &ModelSpec, _: &Catalog, _: &Transport| {
            Ok(ErasedHandler::new(ModelAdapter::<Completion>::new(
                "checkpoint-fixture/fixed",
                provided.clone(),
            )))
        });
        let dir = tempfile::tempdir().expect("fixture workdir");
        let mut app = App::new();
        app.insert_resource(Models(connector))
            .insert_resource(ProjectRoot(dir.path().to_owned()))
            .insert_resource(SessionStore::new(store.clone()))
            .add_plugins((
                TaskPoolPlugin::default(),
                AgentPlugin,
                SessionLogPlugin,
                rig_harness::services::ServicesPlugin,
                CheckpointTasksPlugin,
            ))
            .add_open_tool(
                "checkpoint_probe",
                "Return local fixture detail.",
                ToolOptions {
                    footprint: Some(Touches::Independent),
                },
                |called: On<ToolCalled<Value>>, mut commands: Commands| {
                    commands
                        .entity(called.call)
                        .insert(Outcome::from(ToolResult::success(
                            "DETAIL_ONLY_IN_CHILD".into(),
                        )));
                },
            )
            .add_open_tool(
                "checkpoint_block",
                "Hold the fixture call open until cancellation.",
                ToolOptions {
                    footprint: Some(Touches::Independent),
                },
                |_: On<ToolCalled<Value>>| {},
            );
        app.finish();
        app.cleanup();
        app.update();
        let mut conversation = Conversation::default();
        conversation.push(Message::user("Keep the exact error E_417; never publish."));
        conversation.push(Message::assistant("I will investigate E_417 locally."));
        conversation.push(Message::user(
            "Now check the parser; the earlier constraint still applies.",
        ));
        let original = conversation.messages().to_vec();
        let root = app
            .world_mut()
            .spawn((
                Agent,
                ModelChoice("checkpoint-fixture/fixed".into()),
                SystemPrompt("Fixture coding agent".into()),
                ReasoningSetting(Some(Reasoning::Off)),
                WorkDir(dir.path().to_owned()),
                conversation,
            ))
            .id();
        app.world_mut().spawn((
            PromptSection::new(210, "local_constraint", "Use the parser in crate A."),
            SectionOf(root),
        ));
        Self {
            app,
            root,
            model,
            original,
            dir,
        }
    }

    fn task(&mut self, agent: Entity, args: Value) -> Entity {
        let call = ToolCall::from_wire(
            "checkpoint-task",
            ToolFunction::new(ToolName::new(TASK).expect("task name"), args),
        );
        self.app
            .world_mut()
            .spawn((ToolRequest { call }, ForAgent(agent)))
            .id()
    }

    fn until(&mut self, condition: impl Fn(&World) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !condition(self.app.world()) && Instant::now() < deadline {
            self.app.update();
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(
            condition(self.app.world()),
            "checkpoint lifecycle did not reach expected state"
        );
    }

    fn child(&mut self, parent: Entity) -> Entity {
        self.until(|world| {
            world
                .get::<Spawned>(parent)
                .is_some_and(|children| !children.is_empty())
        });
        self.app
            .world()
            .get::<Spawned>(parent)
            .expect("spawned family")
            .iter()
            .last()
            .expect("child")
    }
}

#[test]
fn checkpoint_preserves_current_history_prompt_settings_and_reports_only_the_result() {
    let mut fixture = Fixture::new(
        &MemoryStore::default(),
        vec![
            vec![
                MockStreamEvent::tool_call("probe", "checkpoint_probe", json!({})),
                MockStreamEvent::FinalResponse(Default::default()),
            ],
            reply("parser result E_417"),
            reply("parent done"),
        ],
    );
    // Include two unresolved parent calls. Neither becomes a child effect or
    // gets replayed: the fork receives explicitly labelled boundary placeholders.
    fixture
        .app
        .add_observer(|child: On<Add<SpawnedBy>>, mut commands: Commands| {
            commands.spawn((
                PromptSection::new(220, "child_policy", "Do not write parser.rs."),
                SectionOf(child.entity),
            ));
        });
    let calls = ["task", "unavailable_parent_action"].map(|name| {
        AssistantContent::ToolCall(ToolCall::from_wire(
            name,
            ToolFunction::new(ToolName::new(name).expect("fixture tool"), json!({})),
        ))
    });
    fixture
        .app
        .world_mut()
        .get_mut::<Conversation>(fixture.root)
        .expect("root history")
        .push(Message::Assistant(AssistantMessage::new(calls.to_vec())));
    fixture.original = fixture
        .app
        .world()
        .get::<Conversation>(fixture.root)
        .expect("history")
        .messages()
        .to_vec();
    let root = fixture.root;
    let source = fixture
        .app
        .world()
        .get::<AgentId>(root)
        .expect("root id")
        .clone();
    let task = fixture.task(
        root,
        json!({"description":"Check parser", "prompt":"Return the parser result."}),
    );
    fixture.until(|world| world.get::<Outcome>(task).is_some());
    assert!(
        fixture
            .app
            .world()
            .get::<Outcome>(task)
            .expect("task answered")
            .0
            .is_ok()
    );
    let child = fixture.child(root);
    fixture.until(|world| matches!(world.get::<Lifecycle>(child), Some(Lifecycle::Finished(_))));
    let world = fixture.app.world();
    assert_eq!(
        world
            .get::<CheckpointContext>(child)
            .expect("checkpoint provenance")
            .source(),
        &source
    );
    assert_eq!(
        world.get::<CacheKey>(child),
        Some(&CacheKey(source.0.clone()))
    );
    assert_eq!(
        world.get::<ModelChoice>(child),
        world.get::<ModelChoice>(root)
    );
    assert_eq!(
        world.get::<ReasoningSetting>(child),
        world.get::<ReasoningSetting>(root)
    );
    assert_eq!(
        world.get::<WorkDir>(child).expect("child dir").0,
        fixture.dir.path()
    );
    assert_eq!(world.get::<SpawnedBy>(child).expect("child owner").0, root);
    let history = world.get::<Conversation>(child).expect("child history");
    assert_eq!(
        &history.messages()[..fixture.original.len()],
        fixture.original
    );
    assert!(history.validate().is_ok());
    assert!(history.pending_calls().is_empty());
    // Inspect the request actually delivered to the mock, including cache key
    // and local system instructions, rather than only installed ECS components.
    let requests = fixture.model.requests();
    let request = requests.first().expect("child model request");
    assert_eq!(
        request.options.cache_key.as_deref(),
        Some(source.0.as_str())
    );
    let prompt = request.system_instructions().expect("system prompt");
    assert!(prompt.contains("Use the parser in crate A."));
    assert!(prompt.contains("Do not write parser.rs."));
    assert!(!prompt.contains("<subagent_role>"));
    let sent: Vec<_> = request
        .chat_history
        .iter()
        .filter(|message| !matches!(message.as_ref(), Message::System { .. }))
        .cloned()
        .collect();
    assert_eq!(&sent[..fixture.original.len()], fixture.original);
    assert!(
        sent.iter()
            .any(|message| contains_text(message, "outcome is unknown here"))
    );
    fixture.until(|world| {
        world.get::<Conversation>(root).is_some_and(|history| {
            history
                .messages()
                .iter()
                .any(|m| contains_text(m, "parser result E_417"))
        })
    });
    assert!(
        !fixture
            .app
            .world()
            .get::<Conversation>(root)
            .expect("parent history")
            .messages()
            .iter()
            .any(|m| contains_text(m, "DETAIL_ONLY_IN_CHILD"))
    );
    assert!(
        fixture
            .app
            .world()
            .get::<Conversation>(child)
            .expect("child history")
            .messages()
            .iter()
            .any(|m| contains_text(m, "DETAIL_ONLY_IN_CHILD"))
    );
}

#[test]
fn fresh_context_is_explicit_and_checkpoint_rejections_do_not_spawn_an_agent() {
    let mut fixture = Fixture::new(
        &MemoryStore::default(),
        vec![reply("fresh result"), reply("parent done")],
    );
    let root = fixture.root;
    let rejected = fixture.task(root, json!({"description":"Override environment", "prompt":"Check it.", "environment":"different machine"}));
    fixture.until(|world| world.get::<Outcome>(rejected).is_some());
    assert!(
        fixture
            .app
            .world()
            .get::<Outcome>(rejected)
            .expect("refused task")
            .tool_result()
            .is_error()
    );
    assert!(fixture.app.world().get::<Spawned>(root).is_none());
    let fresh = fixture.task(root, json!({"description":"Fresh review", "prompt":"Only the explicit brief.", "checkpoint":false, "environment":"different machine"}));
    let child = fixture.child(root);
    fixture.until(|world| world.get::<Outcome>(fresh).is_some());
    assert!(
        fixture
            .app
            .world()
            .get::<CheckpointContext>(child)
            .is_none()
    );
    let history = fixture
        .app
        .world()
        .get::<Conversation>(child)
        .expect("fresh history");
    assert!(!history.messages().iter().any(|m| contains_text(m, "E_417")));
    // The stock depth gate still governs checkpoint selection, so the new
    // plugin cannot offer another way around the parent's nesting limit.
    fixture.app.insert_resource(SubagentPolicy { max_depth: 0 });
    let denied = fixture.task(
        root,
        json!({"description":"Too deep", "prompt":"Check it."}),
    );
    fixture.until(|world| world.get::<Outcome>(denied).is_some());
    assert!(
        fixture
            .app
            .world()
            .get::<Outcome>(denied)
            .expect("depth refusal")
            .tool_result()
            .is_error()
    );
    assert_eq!(
        fixture
            .app
            .world()
            .get::<Spawned>(root)
            .expect("one child")
            .len(),
        1
    );
}

#[test]
fn checkpoint_context_and_local_instructions_survive_session_restore() {
    let store = MemoryStore::default();
    let mut fixture = Fixture::new(&store, vec![reply("saved result"), reply("parent done")]);
    let root = fixture.root;
    fixture.task(
        root,
        json!({"description":"Saved checkpoint", "prompt":"Return the parser result."}),
    );
    let child = fixture.child(root);
    fixture.until(|world| matches!(world.get::<Lifecycle>(child), Some(Lifecycle::Finished(_))));
    let id = fixture
        .app
        .world()
        .get::<AgentId>(child)
        .expect("child id")
        .clone();
    fixture.app.update();
    drop(fixture);
    let mut restored = Fixture::new(&store, vec![reply("restored answer"), reply("parent done")]);
    let child = {
        let world = restored.app.world_mut();
        let mut agents = world.query::<(Entity, &AgentId)>();
        id.find_in(agents.iter(world)).expect("restored child")
    };
    assert!(
        restored
            .app
            .world()
            .get::<CheckpointContext>(child)
            .is_some()
    );
    restored
        .app
        .world_mut()
        .trigger(Deliver::new(child, "Recheck E_417.", DeliveryMode::Queue));
    let model = restored.model.clone();
    restored.until(|_| !model.requests().is_empty());
    let requests = restored.model.requests();
    assert!(
        requests
            .first()
            .expect("restored request")
            .system_instructions()
            .expect("prompt")
            .contains("Use the parser in crate A.")
    );
    assert!(
        restored
            .app
            .world()
            .get::<Conversation>(child)
            .expect("restored history")
            .messages()
            .iter()
            .any(|m| contains_text(m, "never publish"))
    );
}

#[test]
fn stopping_the_parent_cancels_an_active_checkpoint_without_retrying_its_work() {
    let mut fixture = Fixture::new(
        &MemoryStore::default(),
        vec![vec![
            MockStreamEvent::tool_call("block", "checkpoint_block", json!({})),
            MockStreamEvent::FinalResponse(Default::default()),
        ]],
    );
    let root = fixture.root;
    fixture.task(
        root,
        json!({"description":"Pending work", "prompt":"Call checkpoint_block."}),
    );
    let child = fixture.child(root);
    fixture.until(|world| {
        world.get::<ActiveTurn>(child).is_some()
            && world.get::<Conversation>(child).is_some_and(|history| {
                history
                    .pending_calls()
                    .iter()
                    .any(|call| call.function.name.as_str() == "checkpoint_block")
            })
    });
    fixture.app.world_mut().trigger(StopTurn::new(root));
    fixture.until(|world| world.get::<ActiveTurn>(child).is_none());
    assert_eq!(fixture.model.requests().len(), 1);
    assert!(
        fixture
            .app
            .world()
            .get::<Conversation>(child)
            .expect("cancelled history")
            .pending_calls()
            .is_empty()
    );
}
