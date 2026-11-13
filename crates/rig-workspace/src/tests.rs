use std::path::PathBuf;
use std::time::{Duration, Instant};

use bevy_app::TaskPoolPlugin;
use rig_harness::rig_core::message::{ToolFunction, ToolName};
use rig_harness::services::ServicesPlugin;
use serde_json::{Value, json};

use super::*;

struct Fixture {
    app: App,
    project: PathBuf,
    outside: PathBuf,
    _temporary: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        let temporary = tempfile::tempdir().unwrap();
        let base = temporary.path().canonicalize().unwrap();
        let project = base.join("project");
        let outside = base.join("outside");
        std::fs::create_dir(&project).unwrap();
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(outside.join("private.txt"), "private").unwrap();
        let mut app = App::new();
        app.insert_resource(RigHome(Home::at(base.join("rig"))))
            .insert_resource(ProjectRoot(project.clone()))
            .add_plugins((
                TaskPoolPlugin::default(),
                AgentPlugin,
                ServicesPlugin,
                WorkspacePlugin,
                rig_basics::coding_tools::ReadTool,
                rig_basics::coding_tools::WriteTool,
            ));
        app.finish();
        app.cleanup();
        Self {
            app,
            project,
            outside,
            _temporary: temporary,
        }
    }

    fn root(&mut self) -> Entity {
        let root = self.app.world_mut().spawn(Agent).id();
        self.app.world_mut().flush();
        root
    }

    fn call(&mut self, agent: Entity, name: &str, args: Value) -> Outcome {
        let call = ToolCall::from_wire(
            "workspace-test",
            ToolFunction::new(ToolName::new(name).unwrap(), args),
        );
        let call = self
            .app
            .world_mut()
            .spawn((ToolRequest { call }, ForAgent(agent)))
            .id();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            self.app.update();
            if let Some(outcome) = self.app.world().get::<Outcome>(call) {
                return outcome.clone();
            }
            assert!(Instant::now() < deadline, "workspace call did not settle");
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    fn write(&mut self, agent: Entity, filename: &str) -> Outcome {
        self.call(agent, "write", json!({"path": filename, "content": "ok"}))
    }
}

#[test]
fn policy_defaults_roots_and_preserves_narrow_child_file_confinement_without_adding_tools() {
    let mut fixture = Fixture::new();
    let mut names: Vec<_> = fixture
        .app
        .world_mut()
        .query::<&ToolDef>()
        .iter(fixture.app.world())
        .map(|tool| tool.0.name.as_str().to_owned())
        .collect();
    names.sort();
    assert_eq!(names, ["read", "write"]);

    let root = fixture.root();
    assert_eq!(
        fixture.app.world().get::<WorkDir>(root).unwrap().0,
        fixture.project
    );
    let child = fixture.app.world_mut().spawn((Agent, SpawnedBy(root))).id();
    fixture.app.world_mut().flush();
    for agent in [root, child] {
        assert_eq!(
            fixture.app.world().get::<WorkDir>(agent).unwrap().0,
            fixture.project
        );
        assert!(
            fixture
                .write(agent, "allowed.txt")
                .tool_result()
                .is_success()
        );
        let denied = fixture.call(
            agent,
            "read",
            json!({"path": fixture.outside.join("private.txt")}),
        );
        assert!(denied.tool_result().is_error(), "{denied:?}");
    }

    let narrow = fixture.project.join("nested");
    std::fs::create_dir(&narrow).unwrap();
    fixture
        .app
        .world_mut()
        .entity_mut(root)
        .insert(WorkDir(narrow.clone()));
    let narrow_child = fixture.app.world_mut().spawn((Agent, SpawnedBy(root))).id();
    fixture.app.world_mut().flush();
    assert_eq!(
        fixture.app.world().get::<WorkDir>(narrow_child).unwrap().0,
        narrow
    );
    let denied = fixture.call(
        narrow_child,
        "read",
        json!({"path": fixture.project.join("allowed.txt")}),
    );
    assert!(denied.tool_result().is_error(), "{denied:?}");

    fixture
        .app
        .world_mut()
        .entity_mut(child)
        .insert(WorkDir(fixture.outside.clone()));
    let denied = fixture.write(child, "escaped.txt");
    assert!(denied.tool_result().is_refused(), "{denied:?}");
    assert!(!fixture.outside.join("escaped.txt").exists());
}

#[test]
fn copied_or_revoked_worktree_grants_never_fall_back_to_the_original_project() {
    let mut fixture = Fixture::new();
    let joined = fixture.root();
    let owner = fixture.app.world().get::<AgentId>(joined).unwrap().clone();
    let grant = WorkDirScope::trusted_worktrees(&owner, &[fixture.outside.clone()]).unwrap();
    fixture
        .app
        .world_mut()
        .entity_mut(joined)
        .insert((WorkDir(fixture.outside.clone()), grant.clone()));
    assert!(
        fixture
            .write(joined, "joined.txt")
            .tool_result()
            .is_success()
    );

    let copied = fixture
        .app
        .world_mut()
        .spawn((Agent, WorkDir(fixture.project.clone()), grant))
        .id();
    let denied = fixture.write(copied, "copied.txt");
    assert!(denied.tool_result().is_refused(), "{denied:?}");
    assert!(!fixture.project.join("copied.txt").exists());

    let existing_child = fixture
        .app
        .world_mut()
        .spawn((Agent, SpawnedBy(joined), WorkDir(fixture.project.clone())))
        .id();
    fixture
        .app
        .world_mut()
        .entity_mut(joined)
        .insert((WorkDir(fixture.project.clone()), WorkDirScope::denied()));
    let future_child = fixture
        .app
        .world_mut()
        .spawn((Agent, SpawnedBy(joined), WorkDir(fixture.project.clone())))
        .id();
    for agent in [joined, existing_child, future_child] {
        let denied = fixture.write(agent, "revoked.txt");
        assert!(denied.tool_result().is_refused(), "{denied:?}");
    }
    assert!(!fixture.project.join("revoked.txt").exists());
}

#[cfg(unix)]
#[test]
fn worktree_symlink_replacement_revokes_the_frozen_root() {
    let mut fixture = Fixture::new();
    let joined = fixture.root();
    let owner = fixture.app.world().get::<AgentId>(joined).unwrap().clone();
    let grant = WorkDirScope::trusted_worktrees(&owner, &[fixture.outside.clone()]).unwrap();
    let mount = fixture.outside.clone();
    std::fs::rename(&mount, mount.with_extension("previous")).unwrap();
    std::os::unix::fs::symlink(&fixture.project, &mount).unwrap();
    fixture
        .app
        .world_mut()
        .entity_mut(joined)
        .insert((WorkDir(mount), grant));
    let denied = fixture.write(joined, "stale-mount.txt");
    assert!(denied.tool_result().is_refused(), "{denied:?}");
    assert!(!fixture.project.join("stale-mount.txt").exists());
}

#[test]
fn non_tool_native_operations_obey_the_same_workdir_gate() {
    let mut fixture = Fixture::new();
    let agent = fixture.root();
    fixture
        .app
        .world_mut()
        .entity_mut(agent)
        .insert(WorkDir(fixture.outside.clone()));
    let denied = fixture
        .app
        .world_mut()
        .spawn((Effect::default(), WorkspaceOperation, ForAgent(agent)))
        .id();
    fixture.app.update();
    assert!(
        fixture
            .app
            .world()
            .get::<Outcome>(denied)
            .unwrap()
            .tool_result()
            .is_refused()
    );
    assert!(fixture.app.world().get::<Started>(denied).is_none());

    fixture
        .app
        .world_mut()
        .entity_mut(agent)
        .insert(WorkDir(fixture.project.clone()));
    let allowed = fixture
        .app
        .world_mut()
        .spawn((Effect::default(), WorkspaceOperation, ForAgent(agent)))
        .id();
    fixture.app.update();
    assert!(fixture.app.world().get::<Started>(allowed).is_some());
    assert!(fixture.app.world().get::<Outcome>(allowed).is_none());
}
