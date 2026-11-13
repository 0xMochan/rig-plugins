//! Explicitly opt-in real-service smoke test. Never run by the default suite.
//! The external caller must approve private disposable data and host execution.

use std::collections::VecDeque;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use bevy_app::TaskPoolPlugin;
use bevy_ecs::message::Messages;
use rig_harness::prelude::*;
use serde_json::{Value, json};

use super::{Binding, Runtime, disconnect};
use crate::DeltaPlugin;

struct ServiceClient {
    app: App,
    original: Entity,
    organization: String,
    checkout: PathBuf,
    notices: VecDeque<String>,
}

impl ServiceClient {
    fn new(
        root: &Path,
        checkout: &Path,
        home: &Path,
        executable: &Path,
        organization: &str,
        label: &str,
    ) -> Result<Self, String> {
        fs::create_dir_all(home).map_err(|e| e.to_string())?;
        let checkout = checkout.canonicalize().map_err(|e| e.to_string())?;
        let home = home.canonicalize().map_err(|e| e.to_string())?;
        if checkout.starts_with(&home) || home.starts_with(&checkout) {
            return Err("Private state and test checkout must be disjoint".into());
        }
        let settings = Settings::from_toml(&format!(
            "[delta]\nexecutable = {}\n",
            serde_json::to_string(executable).map_err(|e| e.to_string())?
        ));
        let mut app = App::new();
        app.insert_resource(RigHome(Home::at(&home)))
            .insert_resource(ProjectRoot(checkout.clone()))
            .insert_resource(settings)
            .add_plugins((TaskPoolPlugin::default(), AgentPlugin, DeltaPlugin));
        app.finish();
        app.cleanup();
        let identity = root.join(format!("{label}-producer.txt"));
        let id = match fs::read_to_string(&identity) {
            Ok(value) if !value.trim().is_empty() => AgentId(value.trim().to_owned()),
            Ok(_) => return Err("Saved test producer identity is empty; do not reset it".into()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let id = AgentId::default();
                fs::write(&identity, format!("{}\n", id.0)).map_err(|e| e.to_string())?;
                id
            }
            Err(error) => return Err(error.to_string()),
        };
        if id.0.len() > 128
            || !id
                .0
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        {
            return Err("Saved test identity is invalid; it was not reset".into());
        }
        let mut conversation = Conversation::default();
        conversation.push(rig_harness::rig_core::message::Message::user(format!("Synthetic private Delta interoperability test {label}; no user conversation or working repository content.")));
        let saved = home.join("delta").join(&id.0).join("binding.json");
        let binding = if saved.is_file() {
            Some(
                serde_json::from_slice::<Binding>(&fs::read(&saved).map_err(|e| e.to_string())?)
                    .map_err(|e| e.to_string())?,
            )
        } else {
            None
        };
        let original = app
            .world_mut()
            .spawn((Agent, id, WorkDir(checkout.clone()), conversation))
            .id();
        if let Some(binding) = binding {
            app.world_mut().entity_mut(original).insert(binding);
        }
        app.world_mut().spawn(Answerer { agent: None });
        Ok(Self {
            app,
            original,
            organization: organization.into(),
            checkout,
            notices: VecDeque::new(),
        })
    }
    fn command(&mut self, line: &str) {
        self.app
            .world_mut()
            .trigger(RunCommand::new(self.original, line));
        self.app.world_mut().flush();
    }
    fn update(&mut self) -> Result<(), String> {
        self.app.update();
        let asks: Vec<_> = self
            .app
            .world_mut()
            .query_filtered::<(Entity, &Ask, &Decision), (Without<Outcome>, Without<Settled>)>()
            .iter(self.app.world())
            .filter(|(_, _, decision)| !decision.is_decided())
            .map(|(entity, ask, _)| (entity, ask.clone()))
            .collect();
        for (entity, ask) in asks {
            let answer = match ask.title.as_str() {
                "Allow this native Delta host workflow?" => Answer::choice("allow"),
                "Who should see this live Delta session?"
                    if ask.choices.iter().any(|choice| choice.id == "private") =>
                {
                    Answer::choice("private")
                }
                "Who should see this live Delta session?"
                    if ask.body.contains("Current visibility: private")
                        && ask.choices.iter().any(|choice| choice.id == "unchanged") =>
                {
                    Answer::choice("unchanged")
                }
                "Which Delta organization owns this session?" => {
                    Answer::text(self.organization.clone())
                }
                "Choose the transcript export scope" => Answer::choice("public"),
                "Enable ongoing Delta sharing for this session?"
                    if ask.body.contains(&self.organization)
                        && (ask.body.contains("\"kind\":\"private\"")
                            || ask.body.contains("\"kind\":\"unchanged\""))
                        && ask
                            .body
                            .contains(self.checkout.to_str().ok_or("Non-UTF8 test path")?) =>
                {
                    Answer::choice("share")
                }
                "Confirm live participation in this shared Delta thread?"
                    if ask.body.contains("Existing visibility: private") =>
                {
                    Answer::choice("join")
                }
                "Trust these Delta-mounted worktrees for a fresh Rig session?" => {
                    Answer::choice("trust")
                }
                _ => {
                    return Err(format!(
                        "Unexpected native question; no approval inferred: {}",
                        ask.title
                    ));
                }
            };
            // This is a test-only frontend delegated by the user's explicit
            // private-disposable scope, not a production model approval path.
            println!(
                "NATIVE_TEST_DECISION title={:?} answer={:?}",
                ask.title, answer.choices
            );
            self.app
                .world_mut()
                .get_mut::<Decision>(entity)
                .ok_or("Question disappeared")?
                .decide(Decider::Person, answer.outcome());
        }
        if let Some(mut notices) = self.app.world_mut().get_resource_mut::<Messages<Notice>>() {
            for notice in notices.drain() {
                println!("DELTA_NOTICE {}", notice.text);
                if self.notices.len() == 32 {
                    self.notices.pop_front();
                }
                self.notices.push_back(notice.text);
            }
        }
        let runtime = self.app.world().resource::<Runtime>();
        for connection in runtime.connections.values() {
            if let Some(error) = &connection.failure {
                return Err(format!("Real Delta connection paused: {error}"));
            }
        }
        if runtime.flows.is_empty() && runtime.connections.is_empty() && runtime.draining.is_empty()
        {
            return Err(format!(
                "Delta workflow ended without a connection: {:?}",
                self.notices
            ));
        }
        if self
            .app
            .world_mut()
            .query::<&ModelRequest>()
            .iter(self.app.world())
            .next()
            .is_some()
        {
            return Err("The transport-only smoke test must not call a model".into());
        }
        Ok(())
    }
    fn ready(&self) -> Option<Entity> {
        self.app
            .world()
            .resource::<Runtime>()
            .connections
            .iter()
            .find(|(_, connection)| connection.ready() && connection.journal.pending_count() == 0)
            .map(|(&agent, _)| agent)
    }
    fn artifact(&self, agent: Entity) -> Value {
        let connection = &self.app.world().resource::<Runtime>().connections[&agent];
        json!({"producer":connection.binding.producer,"thread_id":connection.binding.thread,"url":connection.binding.url,
            "visibility":connection.binding.visibility,"joined":connection.binding.joined,"workdir":self.app.world().get::<WorkDir>(agent).map(|dir|&dir.0),
            "pending":connection.journal.pending_count(),"phase":connection.phase()})
    }
    fn stop(&mut self) {
        let agents: Vec<_> = self
            .app
            .world()
            .resource::<Runtime>()
            .connections
            .keys()
            .copied()
            .collect();
        for agent in agents {
            disconnect(self.app.world_mut(), agent);
        }
        disconnect(self.app.world_mut(), self.original);
    }
    fn drained(&self) -> bool {
        let runtime = self.app.world().resource::<Runtime>();
        runtime.connections.is_empty()
            && runtime.flows.is_empty()
            && runtime.draining.is_empty()
            && runtime.draining_children.is_empty()
            && runtime.draining_processes.is_empty()
    }
}

fn contains(client: &ServiceClient, agent: Entity, marker: &str) -> bool {
    client
        .app
        .world()
        .get::<Conversation>(agent)
        .is_some_and(|conversation| {
            conversation
                .entries()
                .any(|entry| format!("{:?}", entry.message).contains(marker))
        })
}

fn run(root: &Path, mut manifest: Value) -> Result<Value, String> {
    let checkout = PathBuf::from(
        manifest["checkout"]
            .as_str()
            .ok_or("Missing approved disposable checkout")?,
    );
    if checkout.canonicalize().map_err(|e| e.to_string())?
        != root
            .join("checkout")
            .canonicalize()
            .map_err(|e| e.to_string())?
    {
        return Err("The approved disposable checkout path changed".into());
    }
    let organization = manifest["organization"]
        .as_str()
        .ok_or("Missing approved private organization")?
        .to_owned();
    if manifest["visibility"] != "private"
        || manifest["source_repository"] != "https://github.com/octocat/Hello-World.git"
    {
        return Err("This test requires the approved private public-template scope".into());
    }
    let origin = std::process::Command::new("git")
        .args(["remote", "get-url", "origin"])
        .current_dir(&checkout)
        .output()
        .map_err(|e| e.to_string())?;
    if !origin.status.success()
        || String::from_utf8_lossy(&origin.stdout).trim()
            != "https://github.com/octocat/Hello-World.git"
    {
        return Err("The disposable repository origin changed".into());
    }
    let executable = PathBuf::from("/usr/local/bin/delta")
        .canonicalize()
        .map_err(|e| e.to_string())?;
    let bootstrap = root.join("join-bootstrap");
    fs::create_dir_all(&bootstrap).map_err(|e| e.to_string())?;
    let mut a = ServiceClient::new(
        root,
        &checkout,
        &root.join("rig-home"),
        &executable,
        &organization,
        "source",
    )?;
    let mut b = ServiceClient::new(
        root,
        &bootstrap,
        &root.join("rig-join-home"),
        &executable,
        &organization,
        "join",
    )?;
    let result = (|| {
        a.command("share privately");
        let deadline = Instant::now() + Duration::from_secs(90);
        let source = loop {
            a.update()?;
            if let Some(agent) = a.ready() {
                break agent;
            }
            if Instant::now() >= deadline {
                return Err("Private share did not become ready within 90 seconds".into());
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        let source_info = a.artifact(source);
        if source_info["visibility"] != "private" {
            return Err("The service did not report private visibility".into());
        }
        let link = source_info["url"]
            .as_str()
            .ok_or("Private thread has no returned link yet; no link was fabricated")?
            .to_owned();
        manifest["source"] = source_info;
        fs::write(
            root.join("results.json"),
            serde_json::to_vec_pretty(&manifest).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        println!("PRIVATE_TEST_URL={link}");
        let opened = std::process::Command::new(&executable)
            .args(["open", &link])
            .status()
            .map_err(|e| e.to_string())?;
        if !opened.success() {
            return Err("Could not open the private test thread in Delta app".into());
        }
        b.command(&format!("delta join {link}"));
        let deadline = Instant::now() + Duration::from_secs(90);
        let joined = loop {
            a.update()?;
            b.update()?;
            if let Some(agent) = b.ready() {
                break agent;
            }
            if Instant::now() >= deadline {
                return Err("Fresh real Rig join did not become ready within 90 seconds".into());
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        if joined == b.original || b.app.world().get::<SpawnedBy>(joined).is_some() {
            return Err("Join did not create an independent fresh root".into());
        }
        let directory = b
            .app
            .world()
            .get::<WorkDir>(joined)
            .ok_or("No joined workdir")?
            .0
            .clone();
        let marker = format!(
            "RIG_REAL_SERVICE_{}",
            a.app
                .world()
                .get::<AgentId>(source)
                .ok_or("Missing source identity")?
                .0
        );
        let file = directory.join("rig-delta-handoff.txt");
        fs::write(&file, format!("{marker}\n")).map_err(|e| e.to_string())?;
        a.app
            .world_mut()
            .get_mut::<Conversation>(source)
            .ok_or("No source conversation")?
            .push(rig_harness::rig_core::message::Message::assistant(format!(
                "Synthetic source test marker {marker}"
            )));
        let deadline = Instant::now() + Duration::from_secs(45);
        loop {
            a.update()?;
            b.update()?;
            let bytes =
                fs::read_to_string(checkout.join("rig-delta-handoff.txt")).unwrap_or_default();
            if bytes.contains(&marker) && contains(&b, joined, &marker) {
                break;
            }
            if Instant::now() >= deadline {
                return Err("Real append/watch or joined-to-source file handoff did not arrive within 45 seconds".into());
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        manifest["joined"] = b.artifact(joined);
        manifest["checks"] = json!({"private_visibility":true,"opened_delta_app":true,"fresh_rig_join":true,"real_append_watch_context":true,"real_file_handoff":true,
            "claude_handoff":"not exercised by this transport-only harness; existing Claude API-key authentication is available through the user's normal shell, not Rig's filtered command environment","model_calls":0});
        Ok(manifest.clone())
    })();
    a.stop();
    b.stop();
    let deadline = Instant::now() + Duration::from_secs(20);
    while (!a.drained() || !b.drained()) && Instant::now() < deadline {
        a.app.update();
        b.app.update();
        std::thread::sleep(Duration::from_millis(20));
    }
    if !a.drained() || !b.drained() {
        return Err(format!(
            "Test cleanup did not finish within 20 seconds; original result: {result:?}"
        ));
    }
    result
}

#[test]
#[ignore = "requires explicit user-approved private disposable real Delta account test"]
fn real_private_delta_roundtrip() {
    assert_eq!(
        std::env::var("RIG_DELTA_REAL_SERVICE_CONFIRM").as_deref(),
        Ok("private-disposable-only"),
        "No real service operation is authorized by the default suite"
    );
    let path = PathBuf::from(
        std::env::var_os("RIG_DELTA_REAL_SERVICE_MANIFEST")
            .expect("Explicit approved manifest path required"),
    );
    let root = path.parent().expect("Manifest has a parent");
    let manifest: Value =
        serde_json::from_slice(&fs::read(&path).expect("Read approved disposable manifest"))
            .expect("Valid manifest JSON");
    let result = run(root, manifest);
    let output = match &result {
        Ok(value) => value.clone(),
        Err(error) => {
            let mut partial = fs::read(root.join("results.json"))
                .ok()
                .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
                .unwrap_or_else(|| json!({}));
            partial["error"] = json!(error);
            partial["real_service"] = json!(true);
            partial["private_disposable_only"] = json!(true);
            partial["no_claude_login"] = json!(true);
            partial
        }
    };
    fs::write(
        root.join("results.json"),
        serde_json::to_vec_pretty(&output).unwrap(),
    )
    .expect("Preserve test outcome and producer state");
    assert!(result.is_ok(), "{result:?}");
    println!("REAL_PRIVATE_DELTA_RESULT={output}");
}
