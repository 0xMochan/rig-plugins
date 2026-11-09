//! Real Python/shell journeys; explicit worker setup is required.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use bevy_app::TaskPoolPlugin;
use rig_bashkit::BashkitPlugin;
use rig_harness::prelude::*;
use rig_harness::rig_core::message::{ToolFunction, ToolName};
use rig_harness::rig_core::tool::ToolResult;
use rig_harness::services::ServicesPlugin;
use rig_xonsh::XonshPlugin;
use serde_json::{Value, json};

#[derive(Resource, Default)]
struct Calls(usize);

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
        let worker = PathBuf::from(
            std::env::var_os("RIG_MONTY_WORKER")
                .expect("set RIG_MONTY_WORKER to the installed Monty binary"),
        );
        let directory = tempfile::tempdir().unwrap();
        let mut app = App::new();
        app.insert_resource(Settings::from_toml(&format!(
            "[monty]\nworker = {}",
            serde_json::to_string(&worker).unwrap()
        )))
        .insert_resource(ProjectRoot(directory.path().to_owned()))
        .init_resource::<Calls>()
        .add_plugins((
            TaskPoolPlugin::default(),
            AgentPlugin,
            ServicesPlugin,
            BashkitPlugin,
            XonshPlugin,
        ))
        .add_observer(count);
        app.finish();
        app.cleanup();
        let agent = app.world_mut().spawn(Agent).id();
        Self {
            app,
            agent,
            _directory: directory,
        }
    }

    fn run(&mut self, arguments: Value) -> ToolResult {
        let call = ToolCall::from_wire(
            "test",
            ToolFunction::new(ToolName::new("run_xonsh").unwrap(), arguments),
        );
        let call = self
            .app
            .world_mut()
            .spawn((ToolRequest { call }, ForAgent(self.agent)))
            .id();
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
