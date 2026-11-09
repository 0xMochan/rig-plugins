//! Lifecycle contracts for local ownership and native-consent entry points.
//! All processes below are disposable fixtures; no Delta account is contacted.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use bevy_app::TaskPoolPlugin;
use bevy_ecs::schedule::ScheduleLabel;
use bevy_tasks::{AsyncComputeTaskPool, TaskPool};
use rig_harness::rig_core::completion::CompletionRequest;
use rig_harness::rig_core::message::{ToolCall, ToolFunction, ToolName};

use super::*;
use crate::live_journal::InitialMode;

#[derive(Resource, Default)]
struct Executed(AtomicUsize);

fn protected_tool(
    called: On<ToolCalled<Value>>,
    executed: Res<Executed>,
    mut decisions: Query<&mut Decision>,
) {
    executed.0.fetch_add(1, Ordering::SeqCst);
    decisions.get_mut(called.call).unwrap().decide(
        Decider::plugin("live-test-tool"),
        ToolResult::success(ToolOutput::text("ran")),
    );
}

#[derive(ScheduleLabel, Clone, Debug, PartialEq, Eq, Hash)]
struct GateProbe;

fn app(home: &Path) -> App {
    let mut app = App::new();
    let project = home.join("launcher-workspace");
    fs::create_dir_all(&project).unwrap();
    app.insert_resource(RigHome(Home::at(home)))
        .insert_resource(ProjectRoot(project))
        .init_resource::<Runtime>()
        .init_resource::<Executed>()
        .add_plugins((TaskPoolPlugin::default(), AgentPlugin))
        .add_systems(AgentPass, ownership_gate.in_set(Stage::Gate))
        .init_schedule(GateProbe)
        .add_systems(GateProbe, ownership_gate)
        .add_open_tool(
            "protected_fixture",
            "test-only side effect",
            ToolOptions::default(),
            protected_tool,
        );
    app.finish();
    app.cleanup();
    app
}

fn binding(producer: &str, directory: &Path) -> Binding {
    Binding {
        producer: producer.into(),
        thread: "thread_test".into(),
        locator: None,
        joined: false,
        owner: producer.into(),
        checkout: directory.to_owned(),
        worktree: Some("history#t=fixture".into()),
        url: None,
        visibility: Some("private".into()),
        organization: Some("org-fixture".into()),
        options: ExportOptions::default(),
    }
}

fn user_conversation(text: &str) -> Conversation {
    let mut conversation = Conversation::default();
    conversation.push(Message::user(text));
    conversation
}

fn tool_request(app: &mut App, agent: Entity) -> Entity {
    let call = ToolCall::from_wire(
        "fixture",
        ToolFunction::new(ToolName::new("protected_fixture").unwrap(), json!({})),
    );
    app.world_mut()
        .spawn((ToolRequest { call }, ForAgent(agent), Effect::default()))
        .id()
}

fn outcome(app: &mut App, call: Entity) -> Outcome {
    let deadline = Instant::now() + Duration::from_secs(4);
    loop {
        app.update();
        if let Some(outcome) = app.world().get::<Outcome>(call) {
            return outcome.clone();
        }
        assert!(Instant::now() < deadline, "protected tool did not settle");
    }
}

fn wait_until(mut condition: impl FnMut() -> bool, message: &str) {
    let deadline = Instant::now() + Duration::from_secs(4);
    loop {
        if condition() {
            return;
        }
        assert!(Instant::now() < deadline, "{message}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn restored_binding_refuses_coding_but_does_not_refuse_unbound_agents() {
    let directory = tempfile::tempdir().unwrap();
    let mut app = app(directory.path());
    let bound = app
        .world_mut()
        .spawn((
            Agent,
            AgentId("bound-agent".into()),
            binding("bound-agent", directory.path()),
        ))
        .id();
    let call = tool_request(&mut app, bound);
    let denied = outcome(&mut app, call);
    assert!(
        denied.tool_result().is_refused(),
        "{}",
        denied.tool_result().output().render()
    );
    assert_eq!(
        app.world().resource::<Executed>().0.load(Ordering::SeqCst),
        0
    );
    assert!(denied.tool_result().output().render().contains("reconnect"));

    let ordinary = app.world_mut().spawn(Agent).id();
    let call = tool_request(&mut app, ordinary);
    let allowed = outcome(&mut app, call);
    assert!(allowed.tool_result().is_success());
    assert_eq!(
        app.world().resource::<Executed>().0.load(Ordering::SeqCst),
        1
    );
}

fn executable(directory: &Path, script: &str) -> PathBuf {
    let executable = directory.join("delta-fixture");
    fs::write(&executable, format!("#!/bin/sh\n{script}\n")).unwrap();
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
    executable.canonicalize().unwrap()
}

fn client(directory: &Path, home: &Path, script: &str) -> Client {
    let executable = executable(home, script);
    let metadata = executable.metadata().unwrap();
    Client {
        executable,
        environment: BTreeMap::from([
            ("PATH".into(), "/usr/bin:/bin".into()),
            (
                "FIXTURE_ROOT".into(),
                directory.to_string_lossy().into_owned(),
            ),
        ]),
        cwd: directory.canonicalize().unwrap(),
        home: home.to_owned(),
        executable_len: metadata.len(),
        executable_modified: metadata.modified().ok(),
    }
}

fn fixture_process(directory: &Path, script: &str) -> Process {
    Process::start(
        Request {
            executable: PathBuf::from("/bin/sh").canonicalize().unwrap(),
            args: vec!["-c".into(), script.into()],
            cwd: directory.canonicalize().unwrap(),
            environment: BTreeMap::from([("PATH".into(), "/usr/bin:/bin".into())]),
            stdin: None,
            lifetime: Lifetime::Session {
                startup_timeout: Duration::from_secs(3),
            },
            shutdown_grace: Duration::from_millis(100),
        },
        Wake::default(),
    )
    .unwrap()
}

fn mount(directory: &Path) -> Process {
    let process = fixture_process(
        directory,
        "trap 'exit 0' TERM; while :; do sleep 0.02; done",
    );
    process.mark_ready();
    process
}

fn collaboration(app: &mut App, agent: Entity, directory: &Path, home: &Path) -> Collaboration {
    let producer = app.world().get::<AgentId>(agent).unwrap().0.clone();
    let body = format!(
        "trap 'exit 0' TERM\nprintf '%s\\n' '{{\"type\":\"ready\",\"thread_id\":\"thread_test\",\"producer\":{{\"source\":\"rig\",\"external_id\":\"{producer}\",\"next_sequence\":1,\"turn\":null}}}}' '{{\"type\":\"live\"}}'\nwhile [ ! -f \"$FIXTURE_ROOT/supersede\" ]; do sleep 0.02; done\nprintf '%s\\n' '{{\"type\":\"producer.turn\",\"sequence\":1,\"status\":\"superseded\"}}'\nwhile :; do sleep 0.02; done"
    );
    let journal = Journal::open(home, &producer, "thread_test", 1, InitialMode::Joined).unwrap();
    let mut connected = Collaboration::new(
        Connected {
            agent,
            binding: binding(&producer, directory),
            client: client(directory, home, &body),
            mount: mount(directory),
            journal,
            grant: "test-only native consent fixture".into(),
        },
        app.world_mut(),
    )
    .unwrap();
    // The test controls acknowledgment itself; no append command is started.
    connected.last_append = Instant::now() + Duration::from_secs(3600);
    connected
}

fn acknowledge_pending(journal: &mut Journal) {
    let input = journal.batch().unwrap().input;
    let receipts = input.lines().map(|line| {
        let record: Value = serde_json::from_str(line).unwrap();
        json!({"operation":"appended","thread_id":"thread_test","sequence":record["sequence"],
            "next_sequence":record["sequence"].as_u64().unwrap()+1,"duplicate":false,"disposition":"recorded",
            "turn_sequence":record.get("turnSequence").cloned().unwrap_or(Value::Null),"version":"actor.fixture"}).to_string()+"\n"
    }).collect::<String>();
    let ack = journal.acknowledge(&receipts).unwrap();
    assert!(ack.error.is_none(), "{:?}", ack.error);
    assert!(ack.fenced.is_empty());
}

#[test]
fn a_live_watch_does_not_let_a_new_unacknowledged_turn_start_coding() {
    let directory = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let mut app = app(home.path());
    let agent = app
        .world_mut()
        .spawn((
            Agent,
            AgentId("gate-agent".into()),
            WorkDir(directory.path().to_owned()),
            user_conversation("Use only this fixture workspace."),
        ))
        .id();
    let mut connection = collaboration(&mut app, agent, directory.path(), home.path());
    wait_until(
        || {
            connection.poll(app.world_mut());
            connection.view.phase() == WatchPhase::Live
        },
        "watch did not reach live",
    );
    assert!(connection.ready());
    let turn = app
        .world_mut()
        .spawn((TurnOf(agent), ForAgent(agent), Effect::default()))
        .id();
    app.world_mut().flush();
    let call = tool_request(&mut app, agent);
    app.world_mut()
        .entity_mut(call)
        .insert((Admitted, CallOf(turn)));
    app.world_mut()
        .resource_mut::<Runtime>()
        .connections
        .insert(agent, connection);
    app.world_mut().run_schedule(GateProbe);
    assert!(
        app.world().get::<Holds>(call).is_some(),
        "new local turn ran before its running-state receipt"
    );
    assert!(!app.world().get::<Decision>(call).unwrap().is_decided());
    assert_eq!(
        app.world().resource::<Executed>().0.load(Ordering::SeqCst),
        0
    );

    let mut connection = app
        .world_mut()
        .resource_mut::<Runtime>()
        .connections
        .remove(&agent)
        .unwrap();
    connection.poll(app.world_mut());
    let owned = connection.turn.as_ref().unwrap();
    assert_eq!(owned.entity, turn);
    assert!(connection.journal.confirmed_sequence() < owned.sequence);
    acknowledge_pending(&mut connection.journal);
    assert!(connection.ready());
    app.world_mut()
        .resource_mut::<Runtime>()
        .connections
        .insert(agent, connection);
    // The ordinary kernel hold is released only once the matching start receipt
    // exists; this probe then exercises the same Gate parameters again.
    let holds: Vec<_> = app
        .world_mut()
        .query::<(Entity, &OwnershipHold)>()
        .iter(app.world())
        .filter(|(_, hold)| hold.agent == agent)
        .map(|(entity, _)| entity)
        .collect();
    for hold in holds {
        app.world_mut().entity_mut(hold).despawn();
    }
    app.world_mut().run_schedule(GateProbe);
    assert!(app.world().get::<Holds>(call).is_none());
    assert!(!app.world().get::<Decision>(call).unwrap().is_decided());
}

struct DroppedFlag(Arc<AtomicBool>);
impl Drop for DroppedFlag {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

#[test]
fn matching_takeover_stops_the_kernel_model_descendants_and_owned_native_effect() {
    let directory = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let mut app = app(home.path());
    let root = app
        .world_mut()
        .spawn((
            Agent,
            AgentId("takeover-root".into()),
            WorkDir(directory.path().to_owned()),
            user_conversation("Inspect only this temporary fixture."),
        ))
        .id();
    let mut connection = collaboration(&mut app, root, directory.path(), home.path());
    wait_until(
        || {
            connection.poll(app.world_mut());
            connection.view.phase() == WatchPhase::Live
        },
        "watch did not reach live",
    );
    let root_turn = app
        .world_mut()
        .spawn((TurnOf(root), ForAgent(root), Effect::default()))
        .id();
    let child = app
        .world_mut()
        .spawn((
            Agent,
            SpawnedBy(root),
            user_conversation("Run the temporary child check."),
        ))
        .id();
    let child_turn = app
        .world_mut()
        .spawn((TurnOf(child), ForAgent(child), Effect::default()))
        .id();
    app.world_mut().flush();
    connection.poll(app.world_mut());
    assert_eq!(connection.turn.as_ref().unwrap().entity, root_turn);
    assert_eq!(connection.turn.as_ref().unwrap().sequence, 1);
    acknowledge_pending(&mut connection.journal);

    let mut native = fixture_process(
        directory.path(),
        "trap '' TERM; (sleep 1; printf survived > child-survived) & printf '%s\\n' $$; while :; do sleep 0.02; done",
    );
    let mut pid = None;
    wait_until(
        || {
            if let Some(Event::Line(line)) = native.try_event() {
                pid = Some(line.parse::<i32>().unwrap());
            }
            pid.is_some()
        },
        "native fixture did not start",
    );
    native.mark_ready();
    let dropped = Arc::new(AtomicBool::new(false));
    let drop_guard = DroppedFlag(Arc::clone(&dropped));
    let wake = app.world().resource::<Wake>().clone();
    let native_running = Running::answering(
        AsyncComputeTaskPool::get_or_init(TaskPool::default),
        &wake,
        async move {
            let _owner = native;
            let _guard = drop_guard;
            std::future::pending::<ToolResult>().await
        },
    );
    let native_call = app
        .world_mut()
        .spawn((
            ToolRequest {
                call: ToolCall::from_wire(
                    "native-fixture",
                    ToolFunction::new(ToolName::new("protected_fixture").unwrap(), json!({})),
                ),
            },
            ForAgent(child),
            CallOf(child_turn),
            Effect::default(),
            Started {
                at: SystemTime::now(),
            },
            native_running,
        ))
        .id();
    let model_dropped = Arc::new(AtomicBool::new(false));
    let model_guard = DroppedFlag(Arc::clone(&model_dropped));
    let model_running = Running::answering(
        AsyncComputeTaskPool::get_or_init(TaskPool::default),
        &wake,
        async move {
            let _guard = model_guard;
            std::future::pending::<ToolResult>().await
        },
    );
    let model_call = app
        .world_mut()
        .spawn((
            ModelRequest::new(CompletionRequest::new("fixture")),
            ForAgent(root),
            CallOf(root_turn),
            Effect::default(),
            Started {
                at: SystemTime::now(),
            },
            model_running,
        ))
        .id();
    app.world_mut().flush();
    assert!(app.world().get::<Running>(native_call).is_some());
    assert!(app.world().get::<Running>(model_call).is_some());
    fs::write(directory.path().join("supersede"), "take over").unwrap();
    wait_until(
        || {
            connection.poll(app.world_mut());
            app.world_mut().flush();
            app.world().get::<ActiveTurn>(root).is_none()
        },
        "matching supersession did not stop the local turn",
    );
    assert!(matches!(
        app.world().get::<TurnOutcome>(root_turn),
        Some(TurnOutcome::Stopped(_))
    ));
    assert!(matches!(
        app.world().get::<TurnOutcome>(child_turn),
        Some(TurnOutcome::Stopped(_))
    ));
    assert!(app.world().get::<Running>(native_call).is_none());
    assert!(app.world().get::<Running>(model_call).is_none());
    wait_until(
        || dropped.load(Ordering::SeqCst) && model_dropped.load(Ordering::SeqCst),
        "stopped Running futures retained their owners",
    );
    let leader = rustix::process::Pid::from_raw(pid.unwrap()).unwrap();
    wait_until(
        || rustix::process::test_kill_process(leader).is_err(),
        "cancelled native process was not reaped",
    );
    std::thread::sleep(Duration::from_millis(1100));
    assert!(
        !directory.path().join("child-survived").exists(),
        "a same-group child survived turn takeover"
    );
    assert!(
        !connection.watch.is_finished(),
        "turn cancellation must not detach or terminate its watch owner"
    );
    connection.stop(app.world_mut(), "fixture complete");
}

#[test]
fn native_permission_needs_a_person_and_an_unchanged_question_before_any_process() {
    for mutate in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let mut app = app(home.path());
        let agent = app
            .world_mut()
            .spawn((
                Agent,
                AgentId("consent-agent".into()),
                WorkDir(directory.path().to_owned()),
            ))
            .id();
        app.world_mut().spawn(Answerer { agent: Some(agent) });
        let client = client(
            directory.path(),
            home.path(),
            "printf started > \"$FIXTURE_ROOT/should-not-start\"; exit 1",
        );
        let mut flow = Flow::new(
            app.world_mut(),
            agent,
            client,
            Action::Join("thread_test".into()),
        )
        .unwrap();
        assert!(matches!(
            flow.tick(app.world_mut()).unwrap(),
            FlowProgress::Waiting
        ));
        app.world_mut().flush();
        let question = app
            .world_mut()
            .query::<(Entity, &Ask)>()
            .iter(app.world())
            .next()
            .unwrap()
            .0;
        if mutate {
            app.world_mut()
                .get_mut::<Ask>(question)
                .unwrap()
                .body
                .push_str(" changed after presentation");
        }
        let outcome = Answer::choice("allow").outcome();
        app.world_mut()
            .get_mut::<Decision>(question)
            .unwrap()
            .decide(
                if mutate {
                    Decider::Person
                } else {
                    Decider::plugin("not-a-local-person")
                },
                outcome.clone(),
            );
        app.world_mut()
            .entity_mut(question)
            .insert((outcome, Settled));
        let error = match flow.tick(app.world_mut()) {
            Err(error) => error,
            Ok(_) => panic!("invalid confirmation advanced the host workflow"),
        };
        assert!(
            error.contains("local user") && error.contains("no permission"),
            "{error}"
        );
        assert!(!directory.path().join("should-not-start").exists());
        assert!(!home.path().join("delta").exists());
        flow.cancel(app.world_mut());
    }
    let directory = tempfile::tempdir().unwrap();
    let mut app = app(directory.path());
    let agent = app.world_mut().spawn(Agent).id();
    start(app.world_mut(), agent, Action::Share(String::new()));
    assert!(app.world().resource::<Runtime>().flows.is_empty());
    assert!(!directory.path().join("delta").exists());
}
