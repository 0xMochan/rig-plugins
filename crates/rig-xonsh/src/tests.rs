//! Frontend selection is observable without starting a worker or a model.

use bevy_app::TaskPoolPlugin;
use rig_monty::{MontyPlugin, RUN_MONTY};

use super::*;

fn app() -> App {
    let mut app = App::new();
    app.insert_resource(Settings::from_toml(
        "[monty]\nworker = '/missing-fixture-monty'\n[native_shell]\ncodex = '/missing-fixture-codex'", 
    ))
    .add_plugins((TaskPoolPlugin::default(), AgentPlugin));
    app
}

fn tools(app: &mut App) -> Vec<String> {
    let mut names: Vec<_> = app
        .world_mut()
        .query::<&ToolDef>()
        .iter(app.world())
        .map(|tool| tool.0.name.as_str().to_owned())
        .collect();
    names.sort();
    names
}

#[test]
fn xonsh_does_not_enable_public_python_or_shell_tools() {
    let mut app = app();
    app.add_plugins(XonshPlugin);
    app.finish();
    app.cleanup();
    app.update();

    assert!(app.is_plugin_added::<MontyRuntimePlugin>());
    assert!(!app.is_plugin_added::<MontyPlugin>());
    assert!(app.is_plugin_added::<rig_native_shell::NativeCommandRuntimePlugin>());
    assert!(!app.is_plugin_added::<rig_native_shell::NativeShellPlugin>());
    assert_eq!(tools(&mut app), [RUN_XONSH]);
}

#[test]
fn explicit_monty_frontend_shares_runtime_in_either_load_order() {
    for monty_first in [false, true] {
        let mut app = app();
        if monty_first {
            app.add_plugins((MontyPlugin, XonshPlugin));
        } else {
            app.add_plugins((XonshPlugin, MontyPlugin));
        }
        app.finish();
        app.cleanup();
        app.update();

        assert_eq!(tools(&mut app), [RUN_MONTY, RUN_XONSH]);
        let runtime_registrations = app
            .world_mut()
            .query::<(&Name, &PluginSource)>()
            .iter(app.world())
            .filter(|(name, _)| name.as_str() == MontyRuntimePlugin.name())
            .count();
        assert_eq!(runtime_registrations, 1);
    }
}
