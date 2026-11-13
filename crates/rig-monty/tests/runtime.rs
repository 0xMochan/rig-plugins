//! Real-worker contracts, run explicitly after setup. No provider or credentials.

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use bevy_app::TaskPoolPlugin;
use rig_harness::prelude::*;
use rig_harness::rig_core::message::{ToolFunction, ToolName};
use rig_harness::rig_core::tool::{ToolOutput, ToolResult};
use rig_harness::services::ServicesPlugin;
use rig_monty::MontyPlugin;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Deserialize, JsonSchema)]
struct Probe {
    #[serde(default)]
    value: Value,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    deny: bool,
    #[serde(default)]
    hold: bool,
}

#[derive(Resource, Default)]
struct Seen {
    executions: usize,
    held: Option<Entity>,
}

fn probe(called: On<ToolCalled<Probe>>, mut seen: ResMut<Seen>, mut commands: Commands) {
    seen.executions += 1;
    let output = match &called.args.text {
        Some(text) => ToolOutput::text(text),
        None => ToolOutput::json(called.args.value.clone()),
    };
    commands
        .entity(called.call)
        .insert(Outcome::from(ToolResult::success(output)));
}

fn gate(
    mut calls: Query<(Entity, &ToolRequest, &ChildOf, &mut Decision), ReadyToStart>,
    parents: Query<&ToolRequest>,
    mut seen: ResMut<Seen>,
    mut commands: Commands,
) {
    for (call, request, parent, mut decision) in &mut calls {
        if request.call.function.name.as_str() != "probe" {
            continue;
        }
        assert_eq!(
            parents
                .get(parent.parent())
                .unwrap()
                .call
                .function
                .name
                .as_str(),
            "run_monty"
        );
        let args: Probe =
            serde_json::from_value(Value::Object(request.call.function.arguments.clone())).unwrap();
        if args.deny {
            decision.decide(
                Decider::plugin("test-policy"),
                Outcome::refused("Denied by the actual Rig gate."),
            );
        } else if args.hold && seen.held.is_none() {
            seen.held = Some(call);
            commands.spawn((HoldOf(call), ChildOf(call)));
        }
    }
}

struct Fixture {
    app: App,
    agent: Entity,
    directory: tempfile::TempDir,
    pid: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let worker = PathBuf::from(
            std::env::var_os("RIG_MONTY_WORKER")
                .expect("set RIG_MONTY_WORKER to the installed Monty 1.4.0 binary"),
        );
        let directory = tempfile::tempdir().unwrap();
        let pid = directory.path().join("worker.pid");
        let wrapper = directory.path().join("worker");
        fs::write(
            &wrapper,
            format!(
                "#!/bin/sh\nprintf '%s' \"$$\" > {}\nexec {} \"$@\"\n",
                shell_words::quote(pid.to_str().unwrap()),
                shell_words::quote(worker.to_str().unwrap())
            ),
        )
        .unwrap();
        fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o755)).unwrap();
        let settings = format!(
            "[monty]\nworker = {}",
            serde_json::to_string(&wrapper).unwrap()
        );
        let mut app = App::new();
        app.insert_resource(Settings::from_toml(&settings))
            .insert_resource(ProjectRoot(directory.path().to_owned()))
            .init_resource::<Seen>()
            .add_plugins((
                TaskPoolPlugin::default(),
                AgentPlugin,
                ServicesPlugin,
                MontyPlugin,
            ))
            .add_systems(AgentPass, gate.in_set(Stage::Gate))
            .add_open_tool(
                "probe",
                "test probe",
                ToolOptions {
                    footprint: Some(Touches::Independent),
                },
                probe,
            )
            .add_open_tool(
                "blocked",
                "test blocked tool",
                ToolOptions {
                    footprint: Some(Touches::Independent),
                },
                probe,
            );
        app.finish();
        app.cleanup();
        let agent = app
            .world_mut()
            .spawn((
                Agent,
                ToolAccess::Only(vec!["run_monty".into(), "probe".into()]),
            ))
            .id();
        Self {
            app,
            agent,
            directory,
            pid,
        }
    }

    fn submit(&mut self, code: &str) -> Entity {
        let call = ToolCall::from_wire(
            "test",
            ToolFunction::new(ToolName::new("run_monty").unwrap(), json!({"code":code})),
        );
        self.app
            .world_mut()
            .spawn((ToolRequest { call }, ForAgent(self.agent)))
            .id()
    }

    fn until(&mut self, condition: impl Fn(&World) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(12);
        loop {
            self.app.update();
            if condition(self.app.world()) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "real Monty effect did not reach expected state"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn run(&mut self, code: &str) -> ToolResult {
        let call = self.submit(code);
        self.until(|world| world.get::<Outcome>(call).is_some());
        self.app.world().get::<Outcome>(call).unwrap().tool_result()
    }
}

#[test]
#[ignore = "requires the real Monty worker; see crate README"]
fn python_host_calls_preserve_structured_results_and_real_permission_gates() {
    let mut fixture = Fixture::new();
    let result = fixture.run("a = tool('probe', {'value': {'n': 21}})\nb = tool('probe', {'deny': True})\nc = tool('blocked', {})\ntext = tool('probe', {'text': 'null'})\nnumber = tool('probe', {'text': '123'})\nstructured = tool('probe', {'value': None})\n[a['data']['n'] * 2, b['status'], c['status'], text['data'], number['data'], structured['data']]");
    assert!(result.is_success(), "{}", result.output().render());
    assert_eq!(
        result.output().as_json().unwrap()["value"],
        json!([42, "denied", "denied", "null", "123", null])
    );
    assert_eq!(fixture.app.world().resource::<Seen>().executions, 4);
}

#[test]
#[ignore = "requires the real Monty worker; see crate README"]
fn worker_isolation_and_limits_fail_explicitly_without_poisoning_later_calls() {
    let mut fixture = Fixture::new();
    for (code, reason) in [
        (
            "from pathlib import Path\nPath('/etc/passwd').read_text()",
            "permission",
        ),
        ("[0] * 100_000_000", "memory"),
        ("while True:\n    pass", "time"),
        ("'x' * 300_000", "byte limit"),
    ] {
        let result = fixture.run(code);
        assert!(!result.is_success(), "unexpected success for {code}");
        assert!(
            result.output().render().to_lowercase().contains(reason),
            "unexpected failure: {}",
            result.output().render()
        );
    }
    let printed = fixture.run("print('x' * 70_000)\n{'ok': True}");
    assert!(printed.is_success(), "{}", printed.output().render());
    let output = printed.output().as_json().unwrap();
    assert_eq!(output["value"], json!({"ok":true}));
    assert_eq!(output["output_truncated"], true);
    assert_eq!(output["stdout"].as_str().unwrap().len(), 65_536);

    let native = fixture.run("__rig_xonsh_native([['touch', 'forbidden-native']])");
    assert!(
        !native.is_success(),
        "standalone Monty gained xonsh's native capability"
    );
    assert!(!fixture.directory.path().join("forbidden-native").exists());
}

#[test]
#[ignore = "requires the real Monty worker; see crate README"]
fn cancellation_ends_the_held_child_and_its_actual_worker() {
    let mut fixture = Fixture::new();
    let call = fixture.submit("tool('probe', {'hold': True})");
    fixture.until(|world| world.resource::<Seen>().held.is_some());
    let held = fixture.app.world().resource::<Seen>().held.unwrap();
    let pid = fs::read_to_string(&fixture.pid).unwrap();
    fixture.app.world_mut().entity_mut(call).despawn();
    fixture.until(|world| world.get_entity(held).is_err());
    let deadline = Instant::now() + Duration::from_secs(3);
    while std::process::Command::new("/bin/kill")
        .args(["-0", &pid])
        .stderr(std::process::Stdio::null())
        .status()
        .unwrap()
        .success()
    {
        assert!(Instant::now() < deadline, "cancelled worker still exists");
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(fixture.app.world().resource::<Seen>().executions, 0);
    assert!(fixture.directory.path().exists());
}
