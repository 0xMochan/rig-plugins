use std::time::{Duration, Instant};

use rig_harness::rig_core::message::{ToolFunction, ToolName};
use serde_json::{Value, json};

use super::*;

#[test]
fn inventory_without_execution_reports_only_actual_registrations_and_allowed_tools() {
    let directory = tempfile::tempdir().unwrap();
    let mut app = App::new();
    app.insert_resource(ProjectRoot(directory.path().to_owned()))
        .add_plugins(AgentPlugin);
    rig_harness::load::<StatusPlugin>(&mut app, "rig-status", "workspace");
    app.finish();
    app.cleanup();
    let agent = app
        .world_mut()
        .spawn((Agent, ToolAccess::Only(vec![AGENT_STATUS.into()])))
        .id();

    let inventory = report_for(&mut app, agent);
    assert_eq!(inventory["allowed_tools"], json!([AGENT_STATUS]));
    assert_eq!(inventory["work_dir"], json!(directory.path()));
    assert_eq!(inventory["native_execution"]["state"], "disabled");
    assert!(inventory["native_execution_validation"].is_null());
    assert_eq!(inventory["native_shell_tool_registered"], false);
    assert_eq!(inventory["registered_plugins"].as_array().unwrap().len(), 1);
    assert_eq!(inventory["registered_plugins"][0]["package"], "rig-status");
    assert!(!app.is_plugin_added::<rig_native_shell::NativeShellPlugin>());
    assert!(app.world().get_resource::<NativeShellStatus>().is_none());
}

#[test]
fn separately_selected_unavailable_backend_and_narrow_workdir_are_reported() {
    let directory = tempfile::tempdir().unwrap();
    let narrow = directory.path().join("narrow");
    std::fs::create_dir(&narrow).unwrap();
    for public_tool in [false, true] {
        let mut app = App::new();
        app.insert_resource(ProjectRoot(directory.path().to_owned()))
            .insert_resource(RigHome(Home::at(directory.path().join("rig"))))
            .insert_resource(Settings::from_toml(
                "[native_shell]\ncodex = '/missing-status-fixture-codex'",
            ))
            .add_plugins((AgentPlugin, StatusPlugin));
        if public_tool {
            app.add_plugins(rig_native_shell::NativeShellPlugin);
        } else {
            app.add_plugins(rig_native_shell::NativeCommandRuntimePlugin);
        }
        app.finish();
        app.cleanup();
        let agent = app
            .world_mut()
            .spawn((
                Agent,
                WorkDir(narrow.clone()),
                ToolAccess::Only(vec![AGENT_STATUS.into()]),
            ))
            .id();

        let inventory = report_for(&mut app, agent);
        assert_eq!(inventory["allowed_tools"], json!([AGENT_STATUS]));
        assert_eq!(inventory["work_dir"], json!(narrow));
        assert_eq!(inventory["native_execution"]["state"], "unavailable");
        assert_eq!(
            inventory["native_execution_validation"],
            "startup_compatibility_only"
        );
        assert_eq!(inventory["native_shell_tool_registered"], public_tool);
    }
}

fn report_for(app: &mut App, agent: Entity) -> Value {
    let call = ToolCall::from_wire(
        "status-test",
        ToolFunction::new(ToolName::new(AGENT_STATUS).unwrap(), json!({})),
    );
    let call = app
        .world_mut()
        .spawn((ToolRequest { call }, ForAgent(agent)))
        .id();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        app.update();
        if let Some(outcome) = app.world().get::<Outcome>(call) {
            let result = outcome.tool_result();
            assert!(result.is_success(), "{outcome:?}");
            return result.output().as_json().unwrap().clone();
        }
        assert!(Instant::now() < deadline, "status did not settle");
    }
}
