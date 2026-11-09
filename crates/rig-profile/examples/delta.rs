//! Read-only fly-by of Delta through the profile's real native command gates.
//! Usage: cargo run -p rig-profile --example delta -- /path/to/codex /path/to/delta

use std::error::Error;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use bevy_app::TaskPoolPlugin;
use rig_harness::prelude::*;
use rig_harness::rig_core::message::{ToolFunction, ToolName};
use rig_harness::services::ServicesPlugin;
use rig_profile::RigProfilePlugin;
use serde_json::json;

fn main() -> Result<(), Box<dyn Error>> {
    let mut args = std::env::args_os().skip(1);
    let codex = PathBuf::from(args.next().ok_or("missing absolute Codex sandbox path")?);
    let delta = PathBuf::from(args.next().ok_or("missing absolute Delta CLI path")?);
    if !codex.is_absolute() || !delta.is_absolute() || args.next().is_some() {
        return Err("usage: delta /absolute/codex /absolute/delta".into());
    }
    let scratch = tempfile::tempdir()?;
    let project = scratch.path().join("project[delta]");
    let home = scratch.path().join("rig");
    std::fs::create_dir(&project)?;
    std::fs::create_dir(&home)?;
    let settings = format!(
        "[native_shell]\ncodex = {}\n[delta]\nexecutable = {}\n",
        serde_json::to_string(&codex)?,
        serde_json::to_string(&delta)?,
    );
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
    app.world_mut().spawn(rig_basics::print::KeepRunning);
    let agent = app.world_mut().spawn(Agent).id();
    let call = ToolCall::from_wire(
        "delta-contract",
        ToolFunction::new(
            ToolName::new("delta")?,
            json!({"command":{"action":"contract"}}),
        ),
    );
    let call = app
        .world_mut()
        .spawn((ToolRequest { call }, ForAgent(agent), Effect::default()))
        .id();
    let deadline = Instant::now() + Duration::from_secs(15);
    let outcome = loop {
        app.update();
        if let Some(outcome) = app.world().get::<Outcome>(call) {
            break outcome.clone();
        }
        if Instant::now() >= deadline {
            return Err("Delta contract tool did not settle within 15 seconds".into());
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    let result = outcome.tool_result();
    if !result.is_success() {
        return Err(result.output().render().into());
    }
    let output = result
        .output()
        .as_json()
        .ok_or("Delta tool returned no JSON")?;
    assert_eq!(output["result"]["operation"], "contract");
    assert!(output["result"]["min"].as_u64().is_some_and(|min| min <= 8));
    assert!(output["result"]["max"].as_u64().is_some_and(|max| max >= 8));
    assert_eq!(
        std::fs::read_dir(&project)?.count(),
        0,
        "transport spool was not cleaned"
    );
    println!(
        "PASS: native Delta contract through profile + sandbox; spool cleaned; no authentication or upload: {output}"
    );
    Ok(())
}
