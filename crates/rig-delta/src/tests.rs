use std::collections::VecDeque;
use std::time::{Duration, Instant};

use rig_harness::rig_core::conversation::Entry;
use rig_harness::rig_core::message::{Message, ToolFunction, ToolName};

use super::*;

#[test]
fn independent_and_combined_plugins_have_unambiguous_agent_pass_ordering() {
    fn stock(app: &mut App) {
        app.add_plugins((
            rig_basics::BasicCommandsPlugin,
            rig_basics::subagents::SubagentsPlugin,
        ));
        if !app.is_plugin_added::<rig_harness::ask::AskPlugin>() {
            app.add_plugins(rig_harness::ask::AskPlugin);
        }
    }

    fn selected(app: &mut App, selection: &str) {
        match selection {
            "workspace" => {
                app.add_plugins(rig_workspace::WorkspacePlugin);
            }
            "native" => {
                app.add_plugins((
                    rig_native_shell::NativeCommandRuntimePlugin,
                    rig_auto_review::AutoReviewPlugin,
                ));
            }
            "review" => {
                app.add_plugins(rig_auto_review::AutoReviewPlugin);
            }
            "delta" => {
                app.add_plugins((DeltaPlugin, rig_conversation::ConversationPlugin));
            }
            "combined" => {
                app.add_plugins((
                    rig_workspace::WorkspacePlugin,
                    rig_native_shell::NativeCommandRuntimePlugin,
                    rig_auto_review::AutoReviewPlugin,
                    rig_conversation::ConversationPlugin,
                    DeltaPlugin,
                ));
            }
            _ => unreachable!(),
        }
    }

    for selection in ["workspace", "native", "review", "delta", "combined"] {
        for stock_first in [true, false] {
            let home = tempfile::tempdir().unwrap();
            let mut app = App::new();
            app.insert_resource(RigHome(Home::at(home.path())))
                .insert_resource(ProjectRoot(home.path().to_owned()))
                .insert_resource(Settings::from_toml(
                    "[native_shell]\ncodex = '/missing-schedule-fixture-codex'",
                ))
                .add_plugins(AgentPlugin);
            if stock_first {
                stock(&mut app);
                selected(&mut app, selection);
            } else {
                selected(&mut app, selection);
                stock(&mut app);
            }
            app.finish();
            app.cleanup();
            app.world_mut().schedule_scope(AgentPass, |world, pass| {
                let mut settings = pass.get_build_settings();
                settings.ambiguity_detection = bevy_ecs::schedule::LogLevel::Error;
                pass.set_build_settings(settings);
                pass.initialize(world).unwrap_or_else(|error| {
                    panic!("{selection}, stock_first={stock_first}: {error:?}")
                });
            });
            assert_eq!(
                app.is_plugin_added::<rig_workspace::WorkspacePlugin>(),
                matches!(selection, "workspace" | "combined"),
                "ordering alone must not install workspace policy",
            );
            assert_eq!(
                app.is_plugin_added::<rig_native_shell::NativeCommandRuntimePlugin>(),
                matches!(selection, "native" | "combined"),
                "ordering alone must not install native execution",
            );
        }
    }
}

#[derive(Resource, Default)]
struct FakeCli {
    replies: VecDeque<(i64, String, String)>,
    calls: Vec<Value>,
    hold: bool,
}

fn framed(exit: i64, stdout: &str, stderr: &str) -> ToolResult {
    ToolResult::success(ToolOutput::json(json!({
        "output": format!("RIG_DELTA_V1\n{}\n{}\n{}{}", stdout.len(), stderr.len(), stdout, stderr),
        "status": {"exited": exit}, "truncated": false,
    })))
}

#[derive(Deserialize, JsonSchema)]
struct ShellArgs {
    script: String,
    timeout_secs: u64,
    escalate: bool,
    justification: Option<String>,
}

fn fake_shell(
    called: On<ToolCalled<ShellArgs>>,
    mut fake: ResMut<FakeCli>,
    mut decisions: Query<&mut Decision>,
) {
    fake.calls.push(
        json!({"script": called.args.script, "timeout_secs": called.args.timeout_secs,
        "escalate": called.args.escalate, "justification": called.args.justification}),
    );
    if fake.hold {
        return;
    }
    let (exit, stdout, stderr) = fake.replies.pop_front().expect("unexpected CLI invocation");
    decisions
        .get_mut(called.call)
        .unwrap()
        .decide(Decider::plugin("fake-cli"), framed(exit, &stdout, &stderr));
}

fn app(home: &std::path::Path, fake: bool) -> App {
    let mut app = App::new();
    app.insert_resource(RigHome(Home::at(home)))
        .insert_resource(ProjectRoot(home.to_owned()))
        .add_plugins(AgentPlugin)
        .add_plugins(DeltaPlugin);
    if fake {
        app.init_resource::<FakeCli>().add_open_tool(
            "native_shell",
            "test fixture",
            ToolOptions::default(),
            fake_shell,
        );
    }
    app.finish();
    app.cleanup();
    app
}

fn request(app: &mut App, agent: Entity, name: &str, args: Value) -> Entity {
    let call = ToolCall::from_wire(
        "test",
        ToolFunction::new(ToolName::new(name).unwrap(), args),
    );
    app.world_mut()
        .spawn((ToolRequest { call }, ForAgent(agent), Effect::default()))
        .id()
}

fn wait_for(app: &mut App, call: Entity) -> Outcome {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        app.update();
        if let Some(outcome) = app.world().get::<Outcome>(call) {
            return outcome.clone();
        }
        assert!(Instant::now() < deadline, "Delta tool did not settle");
    }
}

fn reply(app: &mut App, exit: i64, value: Value, stderr: &str) {
    app.world_mut()
        .resource_mut::<FakeCli>()
        .replies
        .push_back((exit, value.to_string(), stderr.into()));
}

#[test]
fn share_shortcuts_require_native_consent_without_starting_model_or_cli() {
    for line in [
        "share",
        "delta share",
        "share ada@example.com; $(touch never)",
        "delta share privately",
    ] {
        let scratch = tempfile::tempdir().unwrap();
        let home = scratch.path().join("rig");
        let project = scratch.path().join("project");
        std::fs::create_dir(&home).unwrap();
        std::fs::create_dir(&project).unwrap();
        std::fs::create_dir(project.join(".git")).unwrap();
        let mut app = app(&home, true);
        app.world_mut().insert_resource(ProjectRoot(project));
        app.world_mut().resource_mut::<DeltaConfig>().executable = "/bin/sh".into();
        let agent = app.world_mut().spawn(Agent).id();
        app.world_mut().spawn(Answerer { agent: Some(agent) });
        app.world_mut().trigger(RunCommand::new(agent, line));
        app.world_mut().flush();
        app.update();
        let questions: Vec<_> = app
            .world_mut()
            .query::<(Entity, &Ask)>()
            .iter(app.world())
            .map(|(entity, ask)| (entity, ask.clone()))
            .collect();
        assert_eq!(questions.len(), 1, "{line}");
        assert!(questions[0].1.body.contains("outside the command sandbox"));
        assert!(questions[0].1.body.contains("publication"));
        assert_eq!(
            app.world_mut()
                .query::<&Incoming>()
                .iter(app.world())
                .count(),
            0
        );
        assert_eq!(app.world().resource::<FakeCli>().calls.len(), 0);
        assert!(!home.join("delta").exists());
        let mut decision = app.world_mut().get_mut::<Decision>(questions[0].0).unwrap();
        decision.decide(Decider::Person, Answer::choice("cancel").outcome());
        app.update();
        app.update();
        assert!(!home.join("delta").exists());
    }
}

#[test]
fn share_help_unknown_actions_and_restricted_access_do_not_start_a_turn() {
    use bevy_ecs::message::Messages;
    let home = tempfile::tempdir().unwrap();
    let mut app = app(home.path(), true);
    let agent = app.world_mut().spawn(Agent).id();
    for line in [
        "delta",
        "delta help",
        "share --help",
        "delta share --help",
        "delta shareholder",
    ] {
        app.world_mut().trigger(RunCommand::new(agent, line));
    }
    let restricted = app
        .world_mut()
        .spawn((Agent, ToolAccess::Only(vec![DELTA.into()])))
        .id();
    app.world_mut()
        .trigger(RunCommand::new(restricted, "share"));
    app.world_mut().flush();
    assert_eq!(
        app.world_mut()
            .query::<&Incoming>()
            .iter(app.world())
            .count(),
        0
    );
    assert_eq!(app.world().resource::<FakeCli>().calls.len(), 0);
    assert!(!home.path().join("delta").exists());
    let recalled: Vec<_> = app
        .world_mut()
        .resource_mut::<Messages<Recalled>>()
        .drain()
        .collect();
    assert!(
        recalled
            .iter()
            .any(|message| message.text == "/delta shareholder")
    );
    assert!(
        !app.world()
            .get::<ToolAccess>(restricted)
            .unwrap()
            .allows("native_shell")
    );
}

#[test]
fn native_handler_negotiates_contract_and_propagates_exact_execution_choice() {
    let home = tempfile::tempdir().unwrap();
    let mut app = app(home.path(), true);
    let agent = app
        .world_mut()
        .spawn((Agent, AgentId("session-1".into())))
        .id();
    reply(
        &mut app,
        0,
        json!({"operation":"contract", "min":8,"max":9,"future":true}),
        "contract diagnostic",
    );
    reply(
        &mut app,
        0,
        json!([{ "id":"thread-1" }]),
        "search diagnostic",
    );
    let call = request(
        &mut app,
        agent,
        DELTA,
        json!({
            "command":{"action":"thread_search", "terms":["a'; $(touch never)", "b"], "scope":"messages","limit":5},
            "execution":{"escalate":true,"justification":"Read the requested Delta conversations", "timeout_secs":15}
        }),
    );
    let result = wait_for(&mut app, call).tool_result();
    assert!(result.is_success(), "{result:?}");
    assert_eq!(
        result.output().as_json().unwrap()["diagnostics"],
        "search diagnostic"
    );
    let calls = &app.world().resource::<FakeCli>().calls;
    assert_eq!(calls.len(), 2);
    for call in calls {
        assert_eq!(call["escalate"], true);
        assert_eq!(call["timeout_secs"], 15);
    }
    assert!(
        calls[1]["script"]
            .as_str()
            .unwrap()
            .contains("thread search")
    );
    assert!(!home.path().join("delta").exists());
}

#[test]
fn compatibility_missing_backend_and_invalid_inputs_never_reach_a_mutation() {
    let home = tempfile::tempdir().unwrap();
    let mut app = app(home.path(), true);
    let agent = app.world_mut().spawn(Agent).id();
    for range in [
        json!({"min":7,"max":7}),
        json!({"min":9,"max":9}),
        json!({"min":8,"max":7}),
    ] {
        let mut contract = range;
        contract["operation"] = json!("contract");
        reply(&mut app, 0, contract, "");
        let call = request(
            &mut app,
            agent,
            DELTA,
            json!({"command":{"action":"thread_share","thread":"thread-1","confirm_upload":true}}),
        );
        assert!(wait_for(&mut app, call).tool_result().is_error());
    }
    assert_eq!(app.world().resource::<FakeCli>().calls.len(), 3);
    for command in [
        json!({"action":"thread_share","thread":"thread-1","confirm_upload":false}),
        json!({"action":"thread_show","thread":"--force"}),
        json!({"action":"thread_show","thread":"thread-1","format":"messages","messages":{"start":1,"end":21}}),
    ] {
        let call = request(&mut app, agent, DELTA, json!({"command": command}));
        assert!(wait_for(&mut app, call).tool_result().is_error());
    }
    assert_eq!(app.world().resource::<FakeCli>().calls.len(), 3);
    let restricted = app
        .world_mut()
        .spawn((Agent, ToolAccess::Only(vec![DELTA.into()])))
        .id();
    let call = request(
        &mut app,
        restricted,
        DELTA,
        json!({"command":{"action":"contract"}}),
    );
    let refused = wait_for(&mut app, call).tool_result();
    assert!(
        !refused.is_success(),
        "{}: {}",
        refused.status_name(),
        refused.output().render()
    );
    assert_eq!(app.world().resource::<FakeCli>().calls.len(), 3);
    assert!(
        !app.world()
            .get::<ToolAccess>(restricted)
            .unwrap()
            .allows("native_shell")
    );
    let mut unavailable = self::app(home.path(), false);
    let agent = unavailable.world_mut().spawn(Agent).id();
    let call = request(
        &mut unavailable,
        agent,
        DELTA,
        json!({"command":{"action":"contract"}}),
    );
    assert!(wait_for(&mut unavailable, call).tool_result().is_error());
}

#[test]
fn cli_failure_and_truncation_are_not_success_and_do_not_retry() {
    let home = tempfile::tempdir().unwrap();
    let mut app = app(home.path(), true);
    let agent = app.world_mut().spawn(Agent).id();
    reply(
        &mut app,
        0,
        json!({"operation":"contract","min":8,"max":8}),
        "",
    );
    reply(
        &mut app,
        1,
        json!({"error":{"code":"authentication_required","retryable":true,"retry_after_ms":1,"message":"Sign in"}}),
        "no auth",
    );
    let call = request(
        &mut app,
        agent,
        DELTA,
        json!({"command":{"action":"thread_list"}}),
    );
    let result = wait_for(&mut app, call).tool_result();
    assert!(result.is_error());
    assert!(result.output().render().contains("authentication_required"));
    assert_eq!(app.world().resource::<FakeCli>().calls.len(), 2);
    let result = ToolResult::success(ToolOutput::json(
        json!({"output":"{}", "status":{"exited":0},"truncated":true}),
    ));
    assert!(Reply::from_tool(&result).is_err());
    assert!(Reply::from_tool(&framed(0, "λ", "diagnostic")).is_ok());
}

#[test]
fn cancellation_ends_child_without_starting_another_cli_command() {
    let home = tempfile::tempdir().unwrap();
    let mut app = app(home.path(), true);
    app.world_mut().resource_mut::<FakeCli>().hold = true;
    let agent = app.world_mut().spawn(Agent).id();
    let call = request(
        &mut app,
        agent,
        DELTA,
        json!({"command":{"action":"thread_list"}}),
    );
    app.update();
    assert_eq!(app.world().resource::<FakeCli>().calls.len(), 1);
    let child = app
        .world_mut()
        .query::<(Entity, &ShellCall)>()
        .iter(app.world())
        .next()
        .unwrap()
        .0;
    let spool = app
        .world()
        .get::<Job>(call)
        .unwrap()
        .spool
        .path()
        .to_owned();
    assert!(spool.exists());
    app.world_mut()
        .entity_mut(call)
        .insert(Outcome::refused("cancelled fixture"));
    app.update();
    assert!(app.world().get_entity(child).is_err());
    assert!(!spool.exists());
    assert_eq!(app.world().resource::<FakeCli>().calls.len(), 1);
}

#[test]
fn explicit_sync_records_native_conversation_with_durable_acknowledgment() {
    let home = tempfile::tempdir().unwrap();
    let mut app = app(home.path(), true);
    let mut conversation = Conversation::default();
    conversation.push(Entry::new(Message::user("Public question")));
    let agent = app
        .world_mut()
        .spawn((Agent, AgentId("sync-session".into()), conversation))
        .id();
    reply(
        &mut app,
        0,
        json!({"operation":"contract","min":8,"max":8}),
        "",
    );
    reply(
        &mut app,
        0,
        json!({"operation":"status","thread_id":"thread-1","visibility":"private","upload":null,
        "access":{"state":"granted"},"producer":{"source":"rig","external_id":"sync-session","next_sequence":1,"turn":null}}),
        "",
    );
    reply(
        &mut app,
        0,
        json!({"operation":"appended","thread_id":"thread-1","sequence":1,"next_sequence":2,"duplicate":false,
        "disposition":"recorded","turn_sequence":null,"version":"actor.1"}),
        "",
    );
    let call = request(
        &mut app,
        agent,
        DELTA_SYNC,
        json!({"thread":"thread-1","confirm_upload":true}),
    );
    let result = wait_for(&mut app, call).tool_result();
    assert!(result.is_success(), "{result:?}");
    assert_eq!(
        result.output().as_json().unwrap()["recording"]["confirmed"],
        1
    );
    let fake = app.world().resource::<FakeCli>();
    assert_eq!(fake.calls.len(), 3);
    assert!(
        fake.calls[2]["script"]
            .as_str()
            .unwrap()
            .contains("Public question")
    );
    assert!(home.path().join("delta/sync-session/outbox.json").exists());
}

#[test]
fn shell_transport_preserves_literal_arguments_and_separates_diagnostics() {
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::tempdir().unwrap();
    let executable = temp.path().join("fake delta");
    std::fs::write(
        &executable,
        "#!/bin/sh\ncase \"$1\" in stdin) cat ;; large) dd if=/dev/zero bs=131072 count=1 2>/dev/null ;; *) printf '%s' \"$1\" ;; esac\nprintf 'diagnostics' >&2\nexit 7\n",
    )
    .unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
    let value = "a'; $(touch injected)\nλ";
    let spool = tempfile::tempdir_in(temp.path()).unwrap();
    let script = cli::script(
        executable.to_str().unwrap(),
        &[value.into()],
        None,
        spool.path().to_str().unwrap(),
    );
    let output = std::process::Command::new("/bin/bash")
        .args(["-c", &script])
        .current_dir(temp.path())
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(7),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty(), "{:?}", output.stderr);
    let result = ToolResult::success(ToolOutput::json(
        json!({"output":String::from_utf8(output.stdout).unwrap(),"status":{"exited":7},"truncated":false}),
    ));
    let reply = Reply::from_tool(&result).unwrap();
    assert_eq!(reply.stdout, value);
    assert_eq!(reply.diagnostics, "diagnostics");
    assert!(!temp.path().join("injected").exists());

    // The literal heredoc carries records without shell interpolation or argv
    // expansion, and bounded readers drain oversized streams without deadlock.
    let input = "{\"text\":\"'$(touch injected)'\"}\nRIG_DELTA_RECORDS\n";
    for (arg, input, oversized) in [("stdin", Some(input), false), ("large", None, true)] {
        let script = cli::script(
            executable.to_str().unwrap(),
            &[arg.into()],
            input,
            spool.path().to_str().unwrap(),
        );
        let output = std::process::Command::new("/bin/bash")
            .args(["-c", &script])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(7));
        let result = ToolResult::success(ToolOutput::json(
            json!({"output":String::from_utf8(output.stdout).unwrap(),"status":{"exited":7},"truncated":false}),
        ));
        if oversized {
            assert_eq!(
                std::fs::metadata(spool.path().join("out")).unwrap().len(),
                65_537
            );
            assert!(Reply::from_tool(&result).is_err());
        } else {
            assert_eq!(Reply::from_tool(&result).unwrap().stdout, input.unwrap());
        }
    }
    assert!(!temp.path().join("injected").exists());
    std::fs::remove_file(spool.path().join("out")).unwrap();
    std::fs::create_dir(spool.path().join("out")).unwrap();
    let script = cli::script(
        executable.to_str().unwrap(),
        &["capture-failure".into()],
        None,
        spool.path().to_str().unwrap(),
    );
    let output = std::process::Command::new("/bin/bash")
        .args(["-c", &script])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(125));
    assert!(!output.stdout.starts_with(b"RIG_DELTA_V1"));
}
