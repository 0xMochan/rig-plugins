//! Real Python/shell journeys; explicit worker setup is required.
//!
//! The native fixture only forwards sandbox argv. It tests routing, deadlines
//! and lifecycle, not OS containment; enforcement probes live in rig-command.

use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use bevy_app::TaskPoolPlugin;
use futures::channel::oneshot;
use futures::future::{FutureExt, Shared};
use rig_auto_review::AutoReviewPlugin;
use rig_bashkit::BashkitPlugin;
use rig_core::catalog::{Catalog, Connector, ModelSpec, Transport};
use rig_core::effect::{EffectKind, HandlerDescriptor, family};
use rig_core::operation::Completion;
use rig_core::providers::registry::ProviderId;
use rig_core::serve::adapters::ModelAdapter;
use rig_core::serve::{Dispatch, ErasedHandler, Reply, Serve};
use rig_core::test_utils::{MockCompletionModel, MockStreamEvent};
use rig_harness::prelude::*;
use rig_harness::rig_core::message::{Message, ToolFunction, ToolName};
use rig_harness::rig_core::tool::ToolResult;
use rig_harness::services::ServicesPlugin;
use rig_native_shell::{NativeCommandRequest, PendingCommand, ReviewPermit};
use rig_workspace::WorkspacePlugin;
use rig_xonsh::XonshPlugin;
use serde_json::{Value, json};

#[derive(Resource, Default)]
struct Calls(usize, usize);

#[derive(Resource, Default)]
struct NativeResults(Vec<Value>);

fn count_native(_: On<Add<NativeCommandRequest>>, mut count: ResMut<Calls>) {
    count.1 += 1;
}

fn record_native(
    settled: On<Add<Settled>>,
    calls: Query<&Outcome, With<NativeCommandRequest>>,
    mut results: ResMut<NativeResults>,
) {
    if let Ok(outcome) = calls.get(settled.entity)
        && let Some(value) = outcome.tool_result().output().as_json()
    {
        results.0.push(value.clone());
    }
}

fn count(added: On<Add<ToolRequest>>, calls: Query<&ToolRequest>, mut count: ResMut<Calls>) {
    if calls
        .get(added.entity)
        .is_ok_and(|request| request.call.function.name.as_str() == "virtual_bash")
    {
        count.0 += 1;
    }
}

struct Fixture {
    app: App,
    agent: Entity,
    _directory: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        Self::configured("", |_| {})
    }

    fn configured(extra_settings: &str, configure: impl FnOnce(&mut App)) -> Self {
        let worker = PathBuf::from(
            std::env::var_os("RIG_MONTY_WORKER")
                .expect("set RIG_MONTY_WORKER to the installed Monty binary"),
        );
        let directory = tempfile::tempdir().unwrap();
        let codex = directory.path().join("codex-fixture");
        std::fs::write(&codex, "#!/bin/sh\nif [ \"${1-}\" = --version ]; then printf 'codex-cli 0.142.0\\n'; exit 0; fi\n[ \"${1-}\" = sandbox ] || exit 97\nprintf x >> \"$0.starts\"\nwhile [ \"$#\" -gt 0 ] && [ \"$1\" != -- ]; do shift; done\n[ \"$#\" -gt 0 ] || exit 98\nshift\nexec \"$@\"\n").unwrap();
        std::fs::set_permissions(&codex, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut app = App::new();
        app.insert_resource(Settings::from_toml(&format!(
            "[monty]\nworker = {}\n[native_shell]\ncodex = {}\n{extra_settings}",
            serde_json::to_string(&worker).unwrap(),
            serde_json::to_string(&codex).unwrap(),
        )))
        .insert_resource(ProjectRoot(directory.path().to_owned()))
        .init_resource::<Calls>()
        .init_resource::<NativeResults>()
        .add_plugins((
            TaskPoolPlugin::default(),
            AgentPlugin,
            ServicesPlugin,
            WorkspacePlugin,
            BashkitPlugin,
            XonshPlugin,
        ))
        .add_observer(count)
        .add_observer(count_native)
        .add_observer(record_native);
        configure(&mut app);
        app.finish();
        app.cleanup();
        let agent = app.world_mut().spawn(Agent).id();
        Self {
            app,
            agent,
            _directory: directory,
        }
    }

    fn sandbox_starts(&self) -> usize {
        std::fs::read(self._directory.path().join("codex-fixture.starts"))
            .map_or(0, |bytes| bytes.len())
    }

    fn submit(&mut self, arguments: Value) -> Entity {
        let call = ToolCall::from_wire(
            "test",
            ToolFunction::new(ToolName::new("run_xonsh").unwrap(), arguments),
        );
        self.app
            .world_mut()
            .spawn((ToolRequest { call }, ForAgent(self.agent)))
            .id()
    }

    fn run(&mut self, arguments: Value) -> ToolResult {
        let call = self.submit(arguments);
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            self.app.update();
            if let Some(outcome) = self.app.world().get::<Outcome>(call) {
                return outcome.tool_result();
            }
            assert!(Instant::now() < deadline, "xonsh effect did not settle");
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

#[test]
#[ignore = "requires the real Monty worker; see crate README"]
fn lexical_paths_join_and_cross_the_native_boundary_as_argument_data() {
    let mut fixture = Fixture::new();
    let result = fixture.run(json!({
        "code": "root = p'dir'\npaths = [root / \"é'; touch injected\", P'other']\ntext = $(printf '%s\\n' @(paths))\ndirect = $(printf '%s' p'a; b')\ncommented = str(\n    # comment.\n    p'leaf'\n)\n[str(root / 'leaf'), root.name, str(root.parent), text, direct, str(p'~/$HOME'), commented]",
    }));
    assert!(result.is_success(), "{}", result.output().render());
    let output = result.output().as_json().unwrap();
    assert_eq!(
        output["value"],
        json!([
            "dir/leaf",
            "dir",
            ".",
            "dir/é'; touch injected\nother\n",
            "a; b",
            "~/$HOME",
            "leaf",
        ])
    );
    assert_eq!(fixture.app.world().resource::<Calls>().1, 2);
    assert_eq!(fixture.sandbox_starts(), 2);
    assert!(!fixture._directory.path().join("injected").exists());
}

#[test]
#[ignore = "requires the real Monty worker; see crate README"]
fn bare_command_blocks_use_the_same_argument_and_backend_boundaries() {
    for backend in ["native", "virtual"] {
        let mut fixture = Fixture::new();
        let result = fixture.run(json!({
            "mode": "commands",
            "backend": backend,
            "inputs": {"names": ["z", "a"]},
            "code": "# command-only block\nprintf '%s\\n' @(names) | sort\nprintf '%s' p'a; b'\n",
        }));
        assert!(result.is_success(), "{}", result.output().render());
        let output = result.output().as_json().unwrap();
        assert_eq!(output["value"], Value::Null);
        assert_eq!(output["stdout"], "a\nz\na; b");
        let calls = fixture.app.world().resource::<Calls>();
        if backend == "native" {
            assert_eq!((calls.0, calls.1), (0, 2));
            assert_eq!(fixture.sandbox_starts(), 2);
        } else {
            assert_eq!((calls.0, calls.1), (2, 0));
            assert_eq!(fixture.sandbox_starts(), 0);
        }
    }
}

#[test]
#[ignore = "requires the real Monty worker; see crate README"]
fn new_syntax_rejects_later_errors_and_does_not_expose_python_filesystem_access() {
    let mut fixture = Fixture::new();
    for arguments in [
        json!({"mode": "commands", "code": "printf first\nprintf bad; touch escaped"}),
        json!({"mode": "commands", "code": "printf first\nprintf '%s' @(p'bad)"}),
        json!({"code": "$(printf first)\npf'unsupported'"}),
        json!({"code": "$(printf first)\np'unclosed"}),
        json!({"code": "$(printf first)\nstr((obj.\n p'leaf'))"}),
        json!({"code": "$(printf first)\n'prefix' p'leaf'"}),
        json!({"code": "$(printf first)\nstr('prefix') p'leaf'"}),
    ] {
        let result = fixture.run(arguments);
        assert!(!result.is_success(), "unexpected success");
        assert_eq!(fixture.app.world().resource::<Calls>().1, 0);
        assert_eq!(fixture.sandbox_starts(), 0);
    }
    std::fs::write(fixture._directory.path().join("private"), "not mounted").unwrap();
    let denied = fixture.run(json!({"code": "p'private'.read_text()"}));
    assert!(!denied.is_success());
    assert_eq!(fixture.app.world().resource::<Calls>().1, 0);
    assert_eq!(fixture.sandbox_starts(), 0);
}

#[test]
#[ignore = "requires the real Monty worker; see crate README"]
fn model_visible_example_mixes_python_and_commands_without_a_python_subprocess() {
    let mut fixture = Fixture::new();
    let usage = include_str!("../src/usage.md");
    let code = usage
        .split_once("```xonsh\n")
        .unwrap()
        .1
        .split_once("```")
        .unwrap()
        .0;
    let result = fixture.run(json!({"code": code}));
    assert!(result.is_success(), "{}", result.output().render());
    let output = result.output().as_json().unwrap();
    assert_eq!(output["value"], json!(["PEAR", "A; B"]));
    assert_eq!(output["stdout"], "done\n");
    assert_eq!(output["stderr"], "");
    assert_eq!(output["output_truncated"], false);
    assert_eq!(fixture.app.world().resource::<Calls>().0, 0);
    assert_eq!(fixture.app.world().resource::<Calls>().1, 3);
    assert_eq!(fixture.sandbox_starts(), 3);
}

#[test]
#[ignore = "requires the real Monty worker; see crate README"]
fn python_shell_forms_preserve_arguments_pipelines_and_capture_semantics() {
    let mut fixture = Fixture::new();
    let result = fixture.run(json!({
        "backend":"virtual",
        "inputs":{"names":["pear", "é'; echo injected"]},
        "files":{"items":"pear\napple\npear\n"},
        "code":"values = []\nfor item in names:\n    values.append($(printf '%s\\n' @(item) | cat))\ntext = $(cat items | sort | uniq)\nuncaptured = $[printf 'done\\n']\n[values, text, uncaptured]",
    }));
    assert!(result.is_success(), "{}", result.output().render());
    let output = result.output().as_json().unwrap();
    assert_eq!(
        output["value"],
        json!([["pear\n", "é'; echo injected\n"], "apple\npear\n", null])
    );
    assert_eq!(output["stdout"], "done\n");
    assert_eq!(fixture.app.world().resource::<Calls>().0, 4);
}

#[test]
#[ignore = "requires the real Monty worker; see crate README"]
fn rejected_syntax_and_denied_tools_never_become_successful_capture() {
    let mut fixture = Fixture::new();
    for code in [
        "$(printf first)\n$(echo bad; echo second)",
        "$(printf first)\nif :",
        "f'{1 + 2}'",
    ] {
        let result = fixture.run(json!({"backend":"virtual", "code":code}));
        assert!(!result.is_success(), "unexpected success for {code}");
        assert_eq!(
            fixture.app.world().resource::<Calls>().0,
            0,
            "a rejected program started a host effect"
        );
    }
    fixture
        .app
        .world_mut()
        .entity_mut(fixture.agent)
        .insert(ToolAccess::Only(vec!["run_xonsh".into()]));
    let denied = fixture.run(json!({"backend":"virtual", "code":"$(printf denied)"}));
    assert!(!denied.is_success());
    assert!(denied.output().render().contains("denied"));
    assert_eq!(
        fixture.app.world().resource::<Calls>().0,
        1,
        "denied call retried or fell back"
    );
}

#[test]
#[ignore = "requires the real Monty worker; see crate README"]
fn native_forms_use_typed_commands_with_options_not_a_public_shell_tool() {
    let mut fixture = Fixture::new();
    let nested = fixture._directory.path().join("nested");
    std::fs::create_dir(&nested).unwrap();
    fixture
        .app
        .world_mut()
        .entity_mut(fixture.agent)
        .insert(ToolAccess::Only(vec!["run_xonsh".into()]));
    assert!(
        !fixture
            .app
            .is_plugin_added::<rig_native_shell::NativeShellPlugin>()
    );
    assert!(
        fixture
            .app
            .world_mut()
            .query::<&ToolDef>()
            .iter(fixture.app.world())
            .all(|tool| tool.0.name.as_str() != "native_shell")
    );
    let result = fixture.run(json!({
        "inputs": {"payload": "é'; touch injected; printf bad"},
        "execution": {"cwd": "nested", "timeout_secs": 2},
        "code": "text = $(printf '%s' @(payload) | cat)\ndirectory = $(pwd)\nuncaptured = $[printf done]\n[text, directory, uncaptured]",
    }));
    assert!(result.is_success(), "{}", result.output().render());
    let output = result.output().as_json().unwrap();
    assert_eq!(
        output["value"],
        json!([
            "é'; touch injected; printf bad",
            format!("{}\n", nested.canonicalize().unwrap().display()),
            null,
        ])
    );
    assert_eq!(output["stdout"], "done");
    assert_eq!(fixture.app.world().resource::<Calls>().1, 3);
    assert_eq!(fixture.sandbox_starts(), 3);
    assert!(!nested.join("injected").exists());
}

#[derive(Clone)]
struct PausedReviewer {
    handler: ErasedHandler,
    release: Shared<oneshot::Receiver<()>>,
}

impl Serve for PausedReviewer {
    type Family = family::Dynamic;

    fn descriptor(&self) -> HandlerDescriptor {
        self.handler.descriptor()
    }

    async fn serve(&self, kind: EffectKind, dispatch: Dispatch) -> Reply {
        let _ = self.release.clone().await;
        self.handler.serve(kind, dispatch).await
    }
}

#[test]
#[ignore = "requires the real Monty worker; see crate README"]
fn upfront_elevation_reviews_the_first_concrete_xonsh_command_before_any_execution() {
    #[derive(Resource, Default)]
    struct ReviewedCommands(Vec<(PendingCommand, bool)>);

    fn record_reviewed(
        settled: On<Add<Settled>>,
        calls: Query<(&PendingCommand, Has<ReviewPermit>), With<NativeCommandRequest>>,
        mut recorded: ResMut<ReviewedCommands>,
    ) {
        if let Ok((command, permit)) = calls.get(settled.entity) {
            recorded.0.push((command.clone(), permit));
        }
    }

    let model = MockCompletionModel::from_stream_turns([[
        MockStreamEvent::text(
            r#"{"verdict":"allow","reason":"The user authorized this exact write."}"#,
        ),
        MockStreamEvent::FinalResponse(Default::default()),
    ]]);
    let (release, released) = oneshot::channel();
    let reviewer = PausedReviewer {
        handler: ErasedHandler::new(ModelAdapter::<Completion>::new(
            "xonsh-review-fixture/fixed",
            model.clone(),
        )),
        release: released.shared(),
    };
    let vendor = ProviderId::declare("xonsh-review-fixture").unwrap();
    let mut connector = Connector::new(Catalog::default());
    connector
        .catalog_mut()
        .insert(ModelSpec::new(vendor, "fixed").with_tools(true));
    connector.provide(vendor, move |_: &ModelSpec, _: &Catalog, _: &Transport| {
        Ok(ErasedHandler::new(reviewer.clone()))
    });
    let mut fixture = Fixture::configured(
        "[auto_review]\nmodel = 'xonsh-review-fixture/fixed'",
        |app| {
            app.insert_resource(Models(connector))
                .init_resource::<ReviewedCommands>()
                .add_plugins(AutoReviewPlugin)
                .add_observer(record_reviewed);
        },
    );
    let conversation = rig_core::conversation::Conversation::from_messages([Message::user(
        "Append exactly one x to launches in this workspace.",
    )]);
    fixture.app.world_mut().entity_mut(fixture.agent).insert((
        Conversation::from(conversation),
        ToolAccess::Only(vec!["run_xonsh".into()]),
    ));
    assert!(
        !fixture
            .app
            .is_plugin_added::<rig_native_shell::NativeShellPlugin>()
    );
    let parent = fixture.submit(json!({
        "execution": {
            "escalate": true,
            "justification": "The user requested this exact workspace write.",
            "timeout_secs": 2,
        },
        "code": "script = 'printf x >> launches'\n$[bash -c @(script)]\n'approved'",
    }));
    let deadline = Instant::now() + Duration::from_secs(10);
    let (native, frozen) = loop {
        fixture.app.update();
        let world = fixture.app.world_mut();
        if let Some((native, command)) = world
            .query_filtered::<(Entity, &PendingCommand), (With<NativeCommandRequest>, With<Holds>)>(
            )
            .iter(world)
            .next()
        {
            break (native, command.clone());
        }
        assert!(Instant::now() < deadline, "upfront review was not held");
        std::thread::sleep(Duration::from_millis(5));
    };
    assert!(frozen.is_escalation());
    assert_eq!(
        frozen.justification(),
        Some("The user requested this exact workspace write.")
    );
    assert_eq!(
        frozen.command().script(),
        "set -o pipefail; bash -c 'printf x >> launches'"
    );
    assert_eq!(fixture.sandbox_starts(), 0);
    assert!(!fixture._directory.path().join("launches").exists());
    assert!(fixture.app.world().get::<Started>(native).is_none());
    assert!(fixture.app.world().get::<Outcome>(parent).is_none());
    assert_eq!(fixture.app.world().resource::<Calls>().1, 1);
    assert_eq!(model.request_count(), 0);

    release.send(()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    let result = loop {
        fixture.app.update();
        if let Some(outcome) = fixture.app.world().get::<Outcome>(parent) {
            break outcome.tool_result();
        }
        assert!(
            Instant::now() < deadline,
            "approved xonsh call did not settle"
        );
        std::thread::sleep(Duration::from_millis(5));
    };
    assert!(result.is_success(), "{}", result.output().render());
    assert_eq!(result.output().as_json().unwrap()["value"], "approved");
    let requests = model.requests();
    assert_eq!(requests.len(), 1);
    assert!(requests[0].tools.is_empty());
    assert!(
        serde_json::to_string(&requests[0])
            .unwrap()
            .contains(frozen.command().script())
    );
    assert_eq!(
        std::fs::read_to_string(fixture._directory.path().join("launches")).unwrap(),
        "x"
    );
    let recorded = &fixture.app.world().resource::<ReviewedCommands>().0;
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0].0, frozen);
    assert!(!recorded[0].1, "the one-use review permit was not consumed");
    assert_eq!(model.request_count(), 1);
    assert_eq!(fixture.app.world().resource::<Calls>().0, 0);
    assert_eq!(fixture.app.world().resource::<Calls>().1, 1);
    assert_eq!(
        fixture.sandbox_starts(),
        0,
        "a preliminary sandbox run occurred"
    );
}

#[test]
#[ignore = "requires the real Monty worker; see crate README"]
fn native_options_validate_before_execution_and_timeout_or_unreviewed_escalation_never_retry() {
    let mut fixture = Fixture::new();
    for arguments in [
        json!({"backend":"virtual", "execution":{"timeout_secs":2}, "code":"$(printf bad)"}),
        json!({"execution":{"timeout_secs":0}, "code":"$(printf bad)"}),
    ] {
        let result = fixture.run(arguments);
        assert!(!result.is_success());
        assert_eq!(fixture.app.world().resource::<Calls>().1, 0);
        assert_eq!(fixture.app.world().resource::<Calls>().0, 0);
    }
    let denied = fixture.run(json!({
        "execution":{"escalate":true,"justification":"Exercise missing-review refusal."},
        "code":"$(touch unapproved)",
    }));
    assert!(!denied.is_success());
    assert_eq!(fixture.sandbox_starts(), 0);
    assert!(!fixture._directory.path().join("unapproved").exists());
    assert_eq!(fixture.app.world().resource::<Calls>().1, 1);

    let timed_out = fixture.run(json!({
        "execution":{"timeout_secs":1}, "code":"$(sleep 10)",
    }));
    assert!(!timed_out.is_success());
    assert_eq!(fixture.sandbox_starts(), 1, "timeout retried or escalated");
    assert_eq!(fixture.app.world().resource::<Calls>().1, 2);
    assert_eq!(
        fixture
            .app
            .world()
            .resource::<NativeResults>()
            .0
            .last()
            .unwrap()["status"],
        "timed_out"
    );
}

#[test]
#[ignore = "requires the real Monty worker; see crate README"]
fn cancelling_xonsh_ends_its_running_native_child() {
    let mut fixture = Fixture::new();
    let parent = fixture.submit(json!({
        "execution":{"timeout_secs":5},
        "code":"$(sh -c 'touch running; sleep 1; touch survived')",
    }));
    let running = fixture._directory.path().join("running");
    let deadline = Instant::now() + Duration::from_secs(10);
    let child = loop {
        fixture.app.update();
        let world = fixture.app.world_mut();
        if let Some(child) = world
            .query_filtered::<Entity, (With<NativeCommandRequest>, With<Running>)>()
            .iter(world)
            .next()
            && running.exists()
        {
            // Running can precede the OS spawn; the child marker proves that
            // this test cancels actual execution rather than a scheduled task.
            break child;
        }
        assert!(Instant::now() < deadline, "native child did not start");
        std::thread::sleep(Duration::from_millis(5));
    };
    fixture.app.world_mut().entity_mut(parent).despawn();
    fixture.app.update();
    assert!(fixture.app.world().get_entity(child).is_err());
    std::thread::sleep(Duration::from_millis(1200));
    assert!(!fixture._directory.path().join("survived").exists());
    assert_eq!(fixture.sandbox_starts(), 1);
}

#[test]
#[ignore = "requires the real Monty worker; see crate README"]
fn held_native_commands_cannot_outlive_revoked_xonsh_access() {
    #[derive(Resource, Default)]
    struct Held(Option<Entity>);

    fn hold(
        calls: Query<Entity, (ReadyToStart, With<NativeCommandRequest>)>,
        mut held: ResMut<Held>,
        mut commands: Commands,
    ) {
        if held.0.is_none()
            && let Some(call) = calls.iter().next()
        {
            held.0 = Some(commands.spawn((HoldOf(call), ChildOf(call))).id());
        }
    }

    for erase_origin in [false, true] {
        let mut fixture = Fixture::new();
        fixture.app.init_resource::<Held>().add_systems(
            AgentPass,
            hold.in_set(rig_native_shell::NativeShellSystems::Review),
        );
        let parent = fixture.submit(json!({"code":"$(touch access-survived)"}));
        let deadline = Instant::now() + Duration::from_secs(10);
        while fixture.app.world().resource::<Held>().0.is_none() {
            fixture.app.update();
            assert!(
                Instant::now() < deadline,
                "native command did not reach hold"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        fixture
            .app
            .world_mut()
            .entity_mut(fixture.agent)
            .insert(ToolAccess::Only(vec![]));
        let hold = fixture.app.world().resource::<Held>().0.unwrap();
        if erase_origin {
            let child = fixture.app.world().get::<HoldOf>(hold).unwrap().0;
            let replacement = NativeCommandRequest::pipeline(
                vec![vec!["touch".into(), "access-survived".into()]],
                rig_native_shell::NativeExecution::default(),
            )
            .unwrap();
            fixture
                .app
                .world_mut()
                .entity_mut(child)
                .insert(replacement);
        }
        fixture.app.world_mut().entity_mut(hold).despawn();
        loop {
            fixture.app.update();
            if let Some(outcome) = fixture.app.world().get::<Outcome>(parent) {
                assert!(!outcome.tool_result().is_success());
                break;
            }
            assert!(
                Instant::now() < deadline,
                "revoked native command did not settle"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(fixture.sandbox_starts(), 0);
        assert!(!fixture._directory.path().join("access-survived").exists());
    }
}

#[test]
#[ignore = "requires the real Monty worker; see crate README"]
fn workspace_revoked_after_gate_cannot_start_a_native_process() {
    fn revoke(
        calls: Query<&ForAgent, (ReadyToStart, With<NativeCommandRequest>)>,
        mut commands: Commands,
    ) {
        for &ForAgent(agent) in &calls {
            commands
                .entity(agent)
                .insert(rig_workspace::WorkDirScope::denied());
        }
    }
    let mut fixture = Fixture::new();
    fixture
        .app
        .add_systems(AgentPass, revoke.in_set(GateSystems::After));
    let denied = fixture.run(json!({"code":"$(touch scope-survived)"}));
    assert!(!denied.is_success());
    assert_eq!(fixture.sandbox_starts(), 0);
    assert!(!fixture._directory.path().join("scope-survived").exists());
}
