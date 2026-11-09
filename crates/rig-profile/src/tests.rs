use std::time::{Duration, Instant};

use bevy_app::TaskPoolPlugin;
use rig_harness::rig_core::message::{ToolFunction, ToolName};
use serde_json::json;

use super::*;

#[test]
fn inventory_reports_disabled_native_execution_without_a_backend() {
    let mut app = App::new();
    app.add_plugins(AgentPlugin);
    status::install(&mut app);
    app.finish();
    app.cleanup();
    let agent = app.world_mut().spawn(Agent).id();
    let outcome = tool_call(&mut app, agent, "agent_status", json!({}));
    let result = outcome.tool_result();
    let inventory = result.output().as_json().unwrap();
    assert_eq!(inventory["native_shell"]["state"], "disabled");
    assert!(inventory["native_shell_validation"].is_null());
    assert!(
        !inventory["allowed_tools"]
            .as_array()
            .unwrap()
            .contains(&json!("native_shell"))
    );
}

// Exercise the actual composition and per-call file handlers, without starting
// a frontend, reading account credentials, or making provider requests.
#[test]
fn profile_exposes_intended_tools_and_confines_root_and_child_files() {
    let temp = tempfile::tempdir().unwrap();
    let project = temp.path().join("project");
    std::fs::create_dir(&project).unwrap();
    let project = project.canonicalize().unwrap();
    let outside = temp.path().join("outside.txt");
    std::fs::write(&outside, "private").unwrap();
    let mut app = App::new();
    app.insert_resource(RigHome(Home::at(temp.path().join("rig"))))
        .insert_resource(ProjectRoot(project.clone()))
        .insert_resource(Settings::from_toml(
            "[native_shell]\ncodex = '/missing-fixture-codex'",
        ))
        .add_plugins((
            TaskPoolPlugin::default(),
            AgentPlugin,
            rig_harness::services::ServicesPlugin,
            RigProfilePlugin,
        ));
    app.finish();
    app.cleanup();
    app.world_mut().spawn(rig_basics::print::KeepRunning);

    let mut names: Vec<_> = app
        .world_mut()
        .query::<&ToolDef>()
        .iter(app.world())
        .map(|tool| tool.0.name.as_str().to_owned())
        .collect();
    names.sort();
    assert_eq!(
        names,
        [
            "agent_status",
            "auto_review_status",
            "delta",
            "delta_session",
            "delta_sync",
            "delta_worktree",
            "edit",
            "message",
            "native_shell",
            "read",
            "run_monty",
            "run_xonsh",
            "task",
            "virtual_bash",
            "wait",
            "write"
        ]
    );
    for name in ["delta_session", "delta_worktree"] {
        let tool = app
            .world_mut()
            .query::<&ToolDef>()
            .iter(app.world())
            .find(|tool| tool.0.name.as_str() == name)
            .unwrap();
        assert_eq!(
            tool.0.parameters["type"], "object",
            "provider-compatible schema for {name}"
        );
    }
    let root = app.world_mut().spawn(Agent).id();
    let review_status = tool_call(&mut app, root, "auto_review_status", json!({}));
    let result = review_status.tool_result();
    let review_status = result.output().as_json().unwrap();
    assert_eq!(review_status["measurement_only"], true);
    assert_eq!(review_status["native_shell"], "unavailable");
    let inventory = tool_call(&mut app, root, "agent_status", json!({}));
    let result = inventory.tool_result();
    let inventory = result.output().as_json().unwrap();
    assert_eq!(inventory["native_shell"]["state"], "unavailable");
    assert!(
        inventory["registered_plugins"]
            .as_array()
            .unwrap()
            .iter()
            .any(|plugin| plugin["package"] == "rig-xonsh")
    );
    for package in [
        "rig-checkpoint",
        "rig-checkpoint-tasks",
        "rig-conversation",
        "rig-delta",
    ] {
        assert!(
            inventory["registered_plugins"]
                .as_array()
                .unwrap()
                .iter()
                .any(|plugin| plugin["package"] == package)
        );
    }
    #[cfg(feature = "tui")]
    for package in ["rig-tui-loading", "rig-tui-games"] {
        assert!(
            inventory["registered_plugins"]
                .as_array()
                .unwrap()
                .iter()
                .any(|plugin| plugin["package"] == package)
        );
    }
    let task = app
        .world_mut()
        .query::<&ToolDef>()
        .iter(app.world())
        .find(|tool| tool.0.name.as_str() == "task")
        .unwrap();
    assert_eq!(
        task.0.parameters["properties"]["checkpoint"]["type"],
        "boolean"
    );
    assert!(
        inventory["allowed_tools"]
            .as_array()
            .unwrap()
            .contains(&json!("run_monty"))
    );
    let child = app.world_mut().spawn((Agent, SpawnedBy(root))).id();
    for agent in [root, child] {
        assert_eq!(app.world().get::<WorkDir>(agent).unwrap().0, project);
        let allowed = tool_call(
            &mut app,
            agent,
            "write",
            json!({"path":"inside.txt", "content":"ok"}),
        );
        assert!(allowed.0.is_ok(), "{allowed:?}");
        let denied = tool_call(&mut app, agent, "read", json!({"path":outside}));
        assert!(denied.tool_result().is_error(), "{denied:?}");
    }
    assert_eq!(
        std::fs::read_to_string(project.join("inside.txt")).unwrap(),
        "ok"
    );
    assert_eq!(std::fs::read_to_string(outside).unwrap(), "private");

    // Subagents inherit a narrower parent directory, not the enclosing project.
    let narrow = project.join("subdir");
    std::fs::create_dir(&narrow).unwrap();
    app.world_mut()
        .entity_mut(root)
        .insert(WorkDir(narrow.clone()));
    let descendant = app.world_mut().spawn((Agent, SpawnedBy(root))).id();
    assert_eq!(app.world().get::<WorkDir>(descendant).unwrap().0, narrow);
    let denied = tool_call(
        &mut app,
        descendant,
        "read",
        json!({"path": project.join("inside.txt")}),
    );
    assert!(denied.tool_result().is_error(), "{denied:?}");

    // A caller cannot widen file or native execution authority by assigning a
    // work directory outside the enclosing project.
    app.world_mut()
        .entity_mut(child)
        .insert(WorkDir(temp.path().to_owned()));
    let denied = tool_call(
        &mut app,
        child,
        "write",
        json!({"path":"escaped.txt", "content":"bad"}),
    );
    assert!(denied.0.is_err(), "{denied:?}");
    assert!(!temp.path().join("escaped.txt").exists());
}

fn tool_call(app: &mut App, agent: Entity, name: &str, args: serde_json::Value) -> Outcome {
    let call = ToolCall::from_wire(
        "smoke",
        ToolFunction::new(ToolName::new(name).unwrap(), args),
    );
    let call = app
        .world_mut()
        .spawn((ToolRequest { call }, ForAgent(agent), Effect::default()))
        .id();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        app.update();
        if let Some(outcome) = app.world().get::<Outcome>(call) {
            return outcome.clone();
        }
        assert!(Instant::now() < deadline, "profile tool did not settle");
        std::thread::sleep(Duration::from_millis(5));
    }
}
