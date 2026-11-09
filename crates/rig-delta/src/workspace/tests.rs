use std::fs;
use std::os::unix::fs::symlink;

use bevy_ecs::system::{RunSystemOnce, SystemState};
use rig_harness::rig_core::completion::{CompletionRequest, SystemPrompt as CorePrompt};
use rig_harness::rig_core::message::Message;
use rig_native_shell::{Authorization, NativeShellPlugin};

use super::*;

struct Fixture {
    _temporary: tempfile::TempDir,
    home: PathBuf,
    one: PathBuf,
    two: PathBuf,
    outside: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().canonicalize().unwrap();
        let home = root.join("rig");
        let one = root.join("first-worktree");
        let two = root.join("second-worktree");
        let outside = root.join("unapproved");
        for directory in [&home, &one, &two, &outside] {
            fs::create_dir(directory).unwrap();
        }
        for worktree in [&one, &two] {
            fs::create_dir(worktree.join(".git")).unwrap();
        }
        fs::write(home.join("AGENTS.md"), "Home restriction.").unwrap();
        fs::write(one.join("AGENTS.md"), "First worktree restriction.").unwrap();
        fs::write(two.join("AGENTS.md"), "Second worktree restriction.").unwrap();
        Self {
            _temporary: temporary,
            home,
            one,
            two,
            outside,
        }
    }

    fn roots(&self) -> Vec<(String, PathBuf)> {
        vec![
            ("first#t=main".into(), self.one.clone()),
            ("second#t=main".into(), self.two.clone()),
        ]
    }

    fn scope(&self, owner: &AgentId) -> LiveWorkspace {
        LiveWorkspace::prepare(owner, &self.home, &self.one, &self.roots()).unwrap()
    }
}

#[test]
fn explicit_mount_roots_never_grant_their_common_parent_or_another_agent() {
    let fixture = Fixture::new();
    let owner = AgentId("joined-agent".into());
    let scope = fixture.scope(&owner);
    let nested = fixture.one.join("src");
    fs::create_dir(&nested).unwrap();
    assert!(scope.allows(&owner, &fixture.one));
    assert!(scope.allows(&owner, &fixture.two));
    assert!(scope.allows(&owner, &nested));
    assert!(!scope.allows(&owner, fixture.one.parent().unwrap()));
    assert!(!scope.allows(&owner, &fixture.outside));
    assert!(!scope.allows(&AgentId("forked-agent".into()), &fixture.one));
    symlink(&fixture.outside, fixture.one.join("escape")).unwrap();
    assert!(!scope.allows(&owner, &fixture.one.join("escape")));
    assert_eq!(scope.find("second#t=main"), Some(fixture.two.as_path()));
    assert!(scope.find("unknown").is_none());
    assert_eq!(scope.instructions().matches("Home restriction.").count(), 1);
    assert!(scope.instructions().contains("First worktree restriction."));
    assert!(
        scope
            .instructions()
            .contains("Second worktree restriction.")
    );
    assert!(
        LiveWorkspace::prepare(
            &owner,
            &fixture.home,
            fixture.one.parent().unwrap(),
            &fixture.roots()
        )
        .is_err()
    );
    assert!(
        LiveWorkspace::prepare(
            &owner,
            &fixture.home,
            &fixture.home,
            &[("home".into(), fixture.home.clone())]
        )
        .is_err()
    );
    assert!(
        LiveWorkspace::prepare(
            &owner,
            &fixture.home,
            Path::new("/"),
            &[("root".into(), PathBuf::from("/"))]
        )
        .is_err()
    );
}

#[test]
fn changed_mount_links_and_untrusted_instruction_links_fail_closed() {
    let fixture = Fixture::new();
    let owner = AgentId("joined-agent".into());
    let scope = fixture.scope(&owner);
    fs::rename(&fixture.two, fixture.two.with_extension("previous")).unwrap();
    symlink(&fixture.outside, &fixture.two).unwrap();
    assert!(!scope.allows(&owner, &fixture.two));
    fs::remove_file(fixture.one.join("AGENTS.md")).unwrap();
    let secret = fixture.outside.join("not-project-instructions");
    fs::write(&secret, "Must never become project context").unwrap();
    symlink(&secret, fixture.one.join("AGENTS.md")).unwrap();
    assert!(
        LiveWorkspace::prepare(
            &owner,
            &fixture.home,
            &fixture.one,
            &[("first".into(), fixture.one.clone())]
        )
        .is_err()
    );
    // Non-regular files are refused without waiting for FIFO input.
    let fifo = fixture.one.join("instructions.pipe");
    assert!(
        std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .unwrap()
            .success()
    );
    assert!(instruction_file(&fifo, None, &[("first".into(), fixture.one.clone())]).is_err());
}

fn app(fixture: &Fixture) -> App {
    let mut app = App::new();
    app.insert_resource(RigHome(Home::at(&fixture.home)))
        .insert_resource(Settings::from_toml(
            "[native_shell]\ncodex = '/missing-workspace-fixture-codex'",
        ))
        .add_plugins((AgentPlugin, NativeShellPlugin));
    install(&mut app);
    app.finish();
    app.cleanup();
    app.world_mut().spawn((
        Name::new("prompt:project_instructions"),
        PromptSection::new(
            PromptSection::ORDER_PROJECT,
            "project_instructions",
            "Unrelated launch project rule",
        ),
    ));
    app.world_mut().spawn(PromptSection::new(
        PromptSection::ORDER_PROJECT,
        "project_instructions",
        "Global policy remains",
    ));
    // An unrelated policy can even have the same text as the named loader.
    app.world_mut().spawn(PromptSection::new(
        PromptSection::ORDER_PROJECT,
        "project_instructions",
        "Unrelated launch project rule",
    ));
    app
}

fn model_request(app: &mut App, agent: Entity) -> Entity {
    let mut request = ModelRequest::new(CompletionRequest::new("fixture request"));
    request.prompt = Some(CorePrompt {
        role: "Coding agent".into(),
        sections: vec![
            Section::new("project_instructions", "Unrelated launch project rule"),
            Section::new("project_instructions", "Global policy remains"),
            Section::new("project_instructions", "Unrelated launch project rule"),
            Section::new("other", "Keep this section"),
        ],
    });
    app.world_mut()
        .spawn((request, ForAgent(agent), Admitted, Decision::default()))
        .id()
}

#[test]
fn actual_prompt_and_native_evidence_share_joined_restrictions_only() {
    let fixture = Fixture::new();
    let mut app = app(&fixture);
    let ordinary = app.world_mut().spawn(Agent).id();
    let owner = AgentId("joined-agent".into());
    let joined = app
        .world_mut()
        .spawn((
            Agent,
            owner.clone(),
            WorkDir(fixture.one.clone()),
            fixture.scope(&owner),
        ))
        .id();
    app.world_mut()
        .get_mut::<Conversation>(joined)
        .unwrap()
        .push(Message::user("Run the checks."));
    app.world_mut().flush();
    let ordinary_call = model_request(&mut app, ordinary);
    let joined_call = model_request(&mut app, joined);
    app.world_mut().run_system_once(apply_prompt).unwrap();
    let ordinary = app
        .world()
        .get::<ModelRequest>(ordinary_call)
        .unwrap()
        .prompt
        .as_ref()
        .unwrap();
    assert_eq!(
        ordinary.section("project_instructions").unwrap().text,
        "Unrelated launch project rule"
    );
    let joined_prompt = app
        .world()
        .get::<ModelRequest>(joined_call)
        .unwrap()
        .prompt
        .as_ref()
        .unwrap();
    let restrictions = joined_prompt
        .section("project_instructions")
        .unwrap()
        .text
        .clone();
    assert!(!restrictions.contains("Unrelated launch project rule"));
    assert!(restrictions.contains("Home restriction."));
    assert!(restrictions.contains("Second worktree restriction."));
    assert_eq!(
        joined_prompt.section("other").unwrap().text,
        "Keep this section"
    );
    assert!(
        joined_prompt
            .sections
            .iter()
            .any(|section| section.text == "Global policy remains")
    );
    assert_eq!(
        joined_prompt
            .sections
            .iter()
            .filter(|section| section.text == "Unrelated launch project rule")
            .count(),
        1
    );
    let expected_prompt = joined_prompt.clone();
    app.world_mut().run_system_once(apply_prompt).unwrap();
    assert_eq!(
        app.world()
            .get::<ModelRequest>(joined_call)
            .unwrap()
            .prompt
            .as_ref()
            .unwrap(),
        &expected_prompt
    );
    let mut authorization = SystemState::<Authorization>::new(app.world_mut());
    let evidence = authorization
        .get(app.world())
        .unwrap()
        .capture(joined, 1024 * 1024)
        .unwrap();
    let evidence = serde_json::to_value(evidence).unwrap();
    assert_eq!(
        evidence["project_restrictions"],
        serde_json::json!([
            restrictions,
            "Global policy remains",
            "Unrelated launch project rule"
        ])
    );
    assert!(
        app.world()
            .get::<Decision>(joined_call)
            .unwrap()
            .get()
            .is_none()
    );
}

#[test]
fn real_descendants_inherit_scope_but_copied_root_identity_does_not() {
    let fixture = Fixture::new();
    let mut app = app(&fixture);
    let owner = AgentId("joined-agent".into());
    let root = app
        .world_mut()
        .spawn((Agent, owner.clone(), fixture.scope(&owner)))
        .id();
    app.world_mut().flush();
    let child = app
        .world_mut()
        .spawn((Agent, SpawnedBy(root), WorkDir(fixture.two.clone())))
        .id();
    app.world_mut().flush();
    let child_id = app.world().get::<AgentId>(child).unwrap();
    let child_scope = app.world().get::<LiveWorkspace>(child).unwrap();
    assert_eq!(child_scope.owner(), child_id);
    assert!(child_scope.allows(child_id, &fixture.two));
    assert_eq!(
        app.world()
            .get::<ScopedProjectInstructions>(child)
            .unwrap()
            .owner(),
        child_id
    );
    let copied = app
        .world_mut()
        .spawn((Agent, fixture.scope(&owner), WorkDir(fixture.one.clone())))
        .id();
    app.world_mut().flush();
    assert!(
        app.world()
            .get::<ScopedProjectInstructions>(copied)
            .is_none()
    );
    let copied_call = model_request(&mut app, copied);
    app.world_mut().run_system_once(apply_prompt).unwrap();
    assert!(
        app.world()
            .get::<Decision>(copied_call)
            .unwrap()
            .get()
            .is_some()
    );
    app.world_mut().entity_mut(root).remove::<LiveWorkspace>();
    app.world_mut().flush();
    assert!(app.world().get::<LiveWorkspace>(child).is_none());
    assert!(
        app.world()
            .get::<ScopedProjectInstructions>(child)
            .is_none()
    );
}
