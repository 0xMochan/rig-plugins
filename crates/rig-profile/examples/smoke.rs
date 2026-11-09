//! Fly-by of the composed tools, without a frontend, credentials or model calls.

use std::error::Error;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use bevy_app::TaskPoolPlugin;
use rig_harness::prelude::*;
use rig_harness::rig_core::message::{Message, ToolFunction, ToolName};
use rig_harness::services::ServicesPlugin;
use rig_profile::RigProfilePlugin;
use serde_json::{Value, json};

fn main() -> Result<(), Box<dyn Error>> {
    let mut args = std::env::args_os().skip(1);
    let codex = PathBuf::from(
        args.next()
            .ok_or("usage: smoke /absolute/path/to/codex /absolute/path/to/monty")?,
    );
    let monty = PathBuf::from(args.next().ok_or("missing Monty worker path")?);
    let scratch = tempfile::tempdir()?;
    let project = scratch.path().join("project[smoke]");
    std::fs::create_dir(&project)?;
    let home = scratch.path().join("rig");
    std::fs::create_dir_all(home.join("tools/bin"))?;
    // Exercise normal profile resolution even when PATH points at an npm wrapper.
    std::os::unix::fs::symlink(&codex, home.join("tools/bin/codex-sandbox"))?;
    let settings = format!("[monty]\nworker = {}", serde_json::to_string(&monty)?);
    let mut app = App::new();
    app.insert_resource(RigHome(Home::at(home)))
        .insert_resource(ProjectRoot(project.clone()))
        .insert_resource(Settings::from_toml(&settings))
        .add_plugins((
            TaskPoolPlugin::default(),
            AgentPlugin,
            ServicesPlugin,
            RigProfilePlugin,
        ));
    app.finish();
    app.cleanup();
    // This harness supplies tool work directly instead of a print-mode prompt.
    app.world_mut().spawn(rig_basics::print::KeepRunning);
    let conversation =
        rig_harness::rig_core::conversation::Conversation::from_messages([Message::user(
            "Run checks only inside this disposable project.",
        )]);
    let agent = app
        .world_mut()
        .spawn((Agent, Conversation::from(conversation)))
        .id();
    let inventory = call(&mut app, agent, "agent_status", json!({}));
    let inventory = output(&inventory);
    assert_eq!(inventory["native_shell"]["state"], "ready");
    assert_eq!(
        inventory["native_shell"]["executable"],
        json!(codex.canonicalize()?)
    );
    assert!(
        inventory["registered_plugins"]
            .as_array()
            .unwrap()
            .iter()
            .any(|plugin| plugin["package"] == "rig-xonsh")
    );

    let virtual_output = call(
        &mut app,
        agent,
        "virtual_bash",
        json!({
            "script": "cat items | sort | uniq",
            "files": [{"path":"items", "content":"pear\napple\npear\n"}],
        }),
    );
    assert_eq!(output(&virtual_output)["stdout"], "apple\npear\n");
    let fresh = call(
        &mut app,
        agent,
        "virtual_bash",
        json!({"script":"test ! -e items"}),
    );
    assert_eq!(output(&fresh)["exit_code"], 0);

    let allowed = call(
        &mut app,
        agent,
        "native_shell",
        json!({
            "script":"printf native > inside.txt", "timeout_secs":5,
        }),
    );
    assert!(allowed.tool_result().output().as_json().is_some());
    assert_eq!(output(&allowed)["status"], json!({"exited":0}));
    assert_eq!(
        std::fs::read_to_string(project.join("inside.txt"))?,
        "native"
    );
    let denied = call(
        &mut app,
        agent,
        "native_shell",
        json!({
            "script":"printf forbidden > ../outside.txt", "timeout_secs":5,
        }),
    );
    assert_ne!(output(&denied)["status"], json!({"exited":0}));
    assert!(!scratch.path().join("outside.txt").exists());
    let isolated = call(
        &mut app,
        agent,
        "virtual_bash",
        json!({"script":"test ! -e inside.txt"}),
    );
    assert_eq!(output(&isolated)["exit_code"], 0);

    let refused = call(
        &mut app,
        agent,
        "native_shell",
        json!({
            "script":"printf forbidden > escalated.txt", "escalate":true,
            "justification":"Exercise the missing-reviewer refusal.",
        }),
    );
    assert!(refused.tool_result().is_refused(), "{refused:?}");
    assert!(
        refused
            .tool_result()
            .output()
            .render()
            .contains("No automatic reviewer model")
    );
    assert!(!project.join("escalated.txt").exists());
    let monty = call(
        &mut app,
        agent,
        "run_monty",
        json!({"code":
            "native = tool('native_shell', {'script': 'printf coded > monty.txt; printf native'})\nvirtual = tool('virtual_bash', {'script': 'printf virtual'})\ndenied = tool('native_shell', {'script': 'touch forbidden', 'escalate': True, 'justification': 'Exercise review refusal.'})\n[native['data']['output'], virtual['data']['stdout'], denied['status']]"
        }),
    );
    assert_eq!(
        output(&monty)["value"],
        json!(["native", "virtual", "denied"])
    );
    assert_eq!(std::fs::read_to_string(project.join("monty.txt"))?, "coded");
    assert!(!project.join("forbidden").exists());
    let xonsh = call(
        &mut app,
        agent,
        "run_xonsh",
        json!({
            "inputs":{"payload":"é'; touch injected; printf 'bad"},
            "code":"captured = $(printf '%s' @(payload))\npipeline = $(printf '%s\\n' z a | sort)\nuncaptured = $[printf 'done\\n']\n[captured, pipeline, uncaptured]",
        }),
    );
    let xonsh_output = output(&xonsh);
    assert_eq!(
        xonsh_output["value"],
        json!(["é'; touch injected; printf 'bad", "a\nz\n", null])
    );
    assert_eq!(xonsh_output["stdout"], "done\n");
    assert!(
        !project.join("injected").exists(),
        "Python argument became shell source"
    );
    println!(
        "PASS: composed virtual/native tools, isolation, review refusal, Monty tool effects, native xonsh pipelines and literal interpolation"
    );
    Ok(())
}

fn output(outcome: &Outcome) -> Value {
    let result = outcome.tool_result();
    assert!(result.is_success(), "{}", result.output().render());
    serde_json::from_str(&result.output().render()).expect("tool output is JSON")
}

fn call(app: &mut App, agent: Entity, name: &str, arguments: Value) -> Outcome {
    let call = ToolCall::from_wire(
        "smoke",
        ToolFunction::new(ToolName::new(name).unwrap(), arguments),
    );
    let call = app
        .world_mut()
        .spawn((ToolRequest { call }, ForAgent(agent), Effect::default()))
        .id();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        app.update();
        if let Some(outcome) = app.world().get::<Outcome>(call) {
            return outcome.clone();
        }
        assert!(Instant::now() < deadline, "tool {name} did not settle");
        std::thread::sleep(Duration::from_millis(5));
    }
}
