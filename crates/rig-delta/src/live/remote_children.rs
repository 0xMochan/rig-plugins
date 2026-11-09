//! Invisible, owned observations of a shared thread's reported children.
//!
//! The caller must hold the parent's native collaboration consent before
//! supplying its authoritative child directory. This module cannot publish,
//! append, mount, participate, claim a turn, or execute received tool data.

use std::collections::BTreeMap;
use std::io::{self, Write};

use rig_harness::prelude::Wake;
use serde::{Deserialize, Serialize};

use super::Client;
use crate::process::{Event, Process};
use crate::watch::{Child, ContextChange, ContextLedger, WatchEffect, WatchPhase, WatchView};

const MAX_CHILDREN: usize = 8;
const MAX_LEDGER_BYTES: usize = 8 * 1024 * 1024;
const EVENTS_PER_CHILD: usize = 64;

/// Persist this inside the parent's producer/thread-bound context checkpoint.
/// It is delivered-context state, never process or execution authorization.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub(crate) struct RemoteLedgers {
    children: BTreeMap<String, DeliveredChild>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct DeliveredChild {
    thread_id: String,
    name: String,
    ledger: ContextLedger,
}

/// Inert data to insert as plugin-authored context. The child name is display
/// metadata, not an executable, reference, or local-user instruction.
#[derive(Clone, Debug)]
pub(crate) struct RemoteChange {
    pub reference: String,
    pub name: String,
    pub change: ContextChange,
}

struct Observation {
    child: Child,
    process: Process,
    view: WatchView,
}

pub(crate) struct RemoteChildren {
    desired: BTreeMap<String, Child>,
    directory_ready: bool,
    active: BTreeMap<String, Observation>,
    draining: Vec<Process>,
    ledgers: RemoteLedgers,
    failure: Option<String>,
    stopping: bool,
}

impl RemoteChildren {
    pub(crate) fn new(saved: RemoteLedgers) -> Result<Self, String> {
        validate_ledgers(&saved)?;
        Ok(Self {
            desired: BTreeMap::new(),
            directory_ready: false,
            active: BTreeMap::new(),
            draining: Vec::new(),
            ledgers: saved,
            failure: None,
            stopping: false,
        })
    }

    /// Call only after an actual `children`, `child`, or `child.remove` parent
    /// frame. In particular, the parent's initial `live` frame does not prove
    /// an empty directory: Delta sends the children snapshot after `live`.
    pub(crate) fn sync(
        &mut self,
        children: &BTreeMap<String, Child>,
        client: &Client,
        wake: Wake,
    ) -> Result<(), String> {
        if self.stopping {
            return Err(self.failure.clone().unwrap_or_else(|| {
                "Remote child observations have stopped; reconnect explicitly".into()
            }));
        }
        let result = self.sync_inner(children, client, wake);
        if let Err(error) = &result {
            self.fail(error.clone());
        }
        result
    }

    fn sync_inner(
        &mut self,
        children: &BTreeMap<String, Child>,
        client: &Client,
        wake: Wake,
    ) -> Result<(), String> {
        if children.len() > MAX_CHILDREN {
            return Err(format!(
                "Delta reports more than {MAX_CHILDREN} remote children; observation paused without dropping activity"
            ));
        }
        client.valid()?;
        let mut desired = BTreeMap::new();
        for (key, child) in children {
            validate_child(child)?;
            if key != &child.thread_id {
                return Err("Delta child directory key does not match its thread identity".into());
            }
            if desired
                .insert(child.reference.clone(), child.clone())
                .is_some()
            {
                return Err("Delta child directory repeats an opaque reference".into());
            }
            if self
                .active
                .get(&child.reference)
                .is_some_and(|old| old.child.thread_id != child.thread_id)
                || self
                    .ledgers
                    .children
                    .get(&child.reference)
                    .is_some_and(|old| old.thread_id != child.thread_id)
            {
                return Err("An observed child reference changed canonical thread identity; reconnect rather than transferring its context".into());
            }
        }
        self.drain_retired();
        let additions = desired
            .keys()
            .filter(|reference| !self.active.contains_key(*reference))
            .count();
        // Cancelled owners still consume a slot until their terminal event. A
        // hostile add/remove stream must not grow an unbounded grace-period set.
        if self
            .active
            .len()
            .saturating_add(self.draining.len())
            .saturating_add(additions)
            > MAX_CHILDREN
        {
            return Err("Remote child process limit includes owners still stopping; reconnect after they have drained".into());
        }
        let removed: Vec<_> = self
            .active
            .keys()
            .filter(|reference| !desired.contains_key(*reference))
            .cloned()
            .collect();
        for reference in removed {
            if let Some(observation) = self.active.remove(&reference) {
                observation.process.request_stop();
                self.draining.push(observation.process);
            }
        }
        for (reference, child) in &desired {
            if let Some(observation) = self.active.get_mut(reference) {
                observation.child = child.clone();
                continue;
            }
            // There is intentionally no --source, --external-id, --participate,
            // shell, stdin, or constructed locator. This is an invisible read.
            let args = vec![
                "--json".into(),
                "thread".into(),
                "watch".into(),
                reference.clone(),
            ];
            let view =
                WatchView::new(child.thread_id.clone()).map_err(|error| error.to_string())?;
            let process = Process::start(client.request(args, None, true), wake.clone())?;
            self.active.insert(
                reference.clone(),
                Observation {
                    child: child.clone(),
                    process,
                    view,
                },
            );
        }
        self.desired = desired;
        self.directory_ready = true;
        Ok(())
    }

    /// Poll without blocking. The complete bounded change plan commits its
    /// delivered ledger only if every observation and projection succeeded.
    pub(crate) fn poll(&mut self) -> Result<Vec<RemoteChange>, String> {
        if self.stopping {
            self.drain_retired();
            return Err(self
                .failure
                .clone()
                .unwrap_or_else(|| "Remote child observations are stopped".into()));
        }
        let result = self.poll_inner();
        if let Err(error) = &result {
            self.fail(error.clone());
        }
        result
    }

    fn poll_inner(&mut self) -> Result<Vec<RemoteChange>, String> {
        self.drain_retired();
        if !self.directory_ready {
            return Ok(Vec::new());
        }
        let mut next = self.ledgers.clone();
        let mut changes = Vec::new();
        let removed: Vec<_> = next
            .children
            .keys()
            .filter(|reference| !self.desired.contains_key(*reference))
            .cloned()
            .collect();
        for reference in removed {
            let mut old = next
                .children
                .remove(&reference)
                .expect("known delivered child");
            let mut empty =
                WatchView::new(old.thread_id.clone()).map_err(|error| error.to_string())?;
            empty
                .apply(crate::watch::WatchEvent::Ready {
                    thread_id: old.thread_id,
                    producer: None,
                })
                .map_err(|error| error.to_string())?;
            empty
                .apply(crate::watch::WatchEvent::Live)
                .map_err(|error| error.to_string())?;
            for change in empty
                .context_changes(&mut old.ledger)
                .map_err(|error| error.to_string())?
            {
                changes.push(RemoteChange {
                    reference: reference.clone(),
                    name: old.name.clone(),
                    change,
                });
            }
        }
        for (reference, observation) in &mut self.active {
            for _ in 0..EVENTS_PER_CHILD {
                let Some(event) = observation.process.try_event() else {
                    break;
                };
                match event {
                    Event::Line(line) => {
                        match observation.view.apply_line(&line).map_err(|error| format!("Child {reference}: {error}"))? {
                            WatchEffect::None => (),
                            WatchEffect::Pause { error } => return Err(format!("Child {reference} watch paused: {}: {}", error.code, error.message)),
                            WatchEffect::StopLocalTurn { .. } => return Err("An invisible child observer unexpectedly claimed local turn ownership".into()),
                        }
                        if observation.view.phase() == WatchPhase::Live {
                            observation.process.mark_ready();
                        }
                    }
                    Event::Exited {
                        code, diagnostics, ..
                    } => {
                        return Err(format!(
                            "Child {reference} watch exited {code:?}: {diagnostics}; no automatic retry"
                        ));
                    }
                    Event::Failed { error, diagnostics } => {
                        return Err(format!(
                            "Child {reference} watch failed: {error}; {diagnostics}"
                        ));
                    }
                }
            }
            if observation.view.phase() != WatchPhase::Live {
                continue;
            }
            let delivered =
                next.children
                    .entry(reference.clone())
                    .or_insert_with(|| DeliveredChild {
                        thread_id: observation.child.thread_id.clone(),
                        name: observation.child.name.clone(),
                        ledger: ContextLedger::default(),
                    });
            delivered.name = observation.child.name.clone();
            for change in observation
                .view
                .context_changes(&mut delivered.ledger)
                .map_err(|error| format!("Child {reference} context: {error}"))?
            {
                changes.push(RemoteChange {
                    reference: reference.clone(),
                    name: delivered.name.clone(),
                    change,
                });
            }
        }
        validate_ledgers(&next)?;
        self.ledgers = next;
        Ok(changes)
    }

    pub(crate) fn ledgers(&self) -> &RemoteLedgers {
        &self.ledgers
    }

    /// Retain this supervisor until `drain` observes every terminal event.
    pub(crate) fn request_stop(&mut self) {
        self.stopping = true;
        for (_, observation) in std::mem::take(&mut self.active) {
            observation.process.request_stop();
            self.draining.push(observation.process);
        }
        for process in &self.draining {
            process.request_stop();
        }
    }

    pub(crate) fn drain(&mut self) -> bool {
        self.request_stop();
        self.drain_retired();
        self.is_finished()
    }

    pub(crate) fn is_finished(&self) -> bool {
        self.active.is_empty() && self.draining.is_empty()
    }

    fn drain_retired(&mut self) {
        self.draining.retain_mut(|process| {
            process.request_stop();
            for _ in 0..EVENTS_PER_CHILD {
                if process.try_event().is_none() {
                    break;
                }
            }
            !process.is_finished()
        });
    }

    fn fail(&mut self, error: String) {
        if self.failure.is_none() {
            self.failure = Some(error);
        }
        self.request_stop();
    }
}

fn validate_child(child: &Child) -> Result<(), String> {
    identifier(&child.thread_id)?;
    identifier(&child.reference)?;
    if child.name.len() > 16 * 1024 || child.name.chars().any(char::is_control) {
        return Err("Delta child display name is unbounded or contains control characters".into());
    }
    Ok(())
}
fn identifier(value: &str) -> Result<(), String> {
    if value.trim().is_empty()
        || value.starts_with('-')
        || value.len() > 4096
        || value.chars().any(char::is_control)
    {
        Err(
            "Delta child identity/reference must be bounded, control-free, and not a CLI flag"
                .into(),
        )
    } else {
        Ok(())
    }
}
fn validate_ledgers(ledgers: &RemoteLedgers) -> Result<(), String> {
    if ledgers.children.len() > MAX_CHILDREN {
        return Err("Saved remote child ledger exceeds its eight-child bound".into());
    }
    for (reference, child) in &ledgers.children {
        identifier(reference)?;
        identifier(&child.thread_id)?;
        if child.name.len() > 16 * 1024 || child.name.chars().any(char::is_control) {
            return Err("Saved remote child name is not bounded display data".into());
        }
    }
    struct Budget(usize);
    impl Write for Budget {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if bytes.len() > MAX_LEDGER_BYTES - self.0 {
                return Err(io::Error::other(
                    "Remote child delivered context exceeds 8 MiB",
                ));
            }
            self.0 += bytes.len();
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    serde_json::to_writer(&mut Budget(0), ledgers)
        .map_err(|error| format!("Invalid remote child ledger: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::time::{Duration, Instant};

    fn child() -> Child {
        Child {
            thread_id: "child-thread".into(),
            reference: "parent#s=Opaque/Child".into(),
            kind: "subagent".into(),
            name: "Scout".into(),
            status: "running".into(),
        }
    }
    fn directory(child: Child) -> BTreeMap<String, Child> {
        BTreeMap::from([(child.thread_id.clone(), child)])
    }
    fn fixture() -> (tempfile::TempDir, Client) {
        let scratch = tempfile::tempdir().unwrap();
        let root = scratch.path().canonicalize().unwrap();
        let executable = root.join("fixture-delta");
        fs::write(&executable, r#"#!/bin/sh
[ "$#" -eq 4 ] && [ "$1" = --json ] && [ "$2" = thread ] && [ "$3" = watch ] || exit 19
printf '%s\n' "$4" >> "$FIXTURE_ROOT/argv"
trap 'printf stopped >> "$FIXTURE_ROOT/stopped"; exit 0' TERM
printf '%s\n' '{"type":"ready","thread_id":"child-thread","producer":null}'
printf '%s\n' '{"type":"item","item":{"id":"thought","kind":"agent_thought","after_id":null,"turn_id":null,"author":null,"created_at":null,"open":false,"content":{"text":"private remote thought"}}}'
printf '%s\n' '{"type":"item","item":{"id":"message","kind":"agent_message","after_id":"thought","turn_id":null,"author":null,"created_at":null,"open":false,"content":{"text":"Public child answer"}}}'
printf '%s\n' '{"type":"item","item":{"id":"tool","kind":"tool_call","after_id":"message","turn_id":null,"author":null,"created_at":null,"open":false,"content":{"toolCallId":"remote-call","name":"native_shell","rawInput":{"script":"must never execute"},"status":"completed"}}}'
printf '%s\n' '{"type":"live"}'
printf '%s\n' '{"type":"producer.turn","sequence":4,"status":"superseded"}'
while :; do
    if [ -f "$FIXTURE_ROOT/fail" ]; then
        printf '%s\n' '{"error":{"code":"disconnected","message":"fixture disconnected","retryable":true}}'
        rm -f "$FIXTURE_ROOT/fail"
    fi
    sleep 0.02
done
"#).unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
        let executable = executable.canonicalize().unwrap();
        let metadata = executable.metadata().unwrap();
        let client = Client {
            executable,
            environment: BTreeMap::from([
                ("PATH".into(), "/usr/bin:/bin".into()),
                ("FIXTURE_ROOT".into(), root.to_str().unwrap().into()),
            ]),
            cwd: root.clone(),
            home: root,
            executable_len: metadata.len(),
            executable_modified: metadata.modified().ok(),
        };
        (scratch, client)
    }
    fn changes(children: &mut RemoteChildren) -> Vec<RemoteChange> {
        let deadline = Instant::now() + Duration::from_secs(4);
        loop {
            let changes = children.poll().unwrap();
            if !changes.is_empty() {
                return changes;
            }
            assert!(
                Instant::now() < deadline,
                "child snapshot did not become live"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    fn drain(children: &mut RemoteChildren) {
        let deadline = Instant::now() + Duration::from_secs(7);
        while !children.drain() {
            assert!(Instant::now() < deadline, "child owner did not terminate");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn observed_child_data_stays_inert_and_opaque_references_are_not_rebuilt() {
        let (_scratch, client) = fixture();
        let mut children = RemoteChildren::new(RemoteLedgers::default()).unwrap();
        children
            .sync(&directory(child()), &client, Wake::default())
            .unwrap();
        let changes = changes(&mut children);
        assert_eq!(changes.len(), 2);
        assert!(changes.iter().all(|change| change.reference == "parent#s=Opaque/Child" && change.name == "Scout"));
        let rendered = format!("{changes:?}");
        assert!(rendered.contains("Public child answer"));
        assert!(rendered.contains("must never execute"));
        assert!(!rendered.contains("private remote thought"));
        assert_eq!(
            fs::read_to_string(client.cwd.join("argv")).unwrap(),
            "parent#s=Opaque/Child\n"
        );
        // A producer.turn frame is just remote state: this observer never has a
        // local turn and does not expose an execution or takeover effect.
        assert!(children.poll().unwrap().is_empty());
        drain(&mut children);
        assert_eq!(
            fs::read_to_string(client.cwd.join("stopped")).unwrap(),
            "stopped"
        );
    }

    #[test]
    fn reconnect_ledger_dedupes_and_removal_retires_context_and_process_owners() {
        let (_scratch, client) = fixture();
        let mut first = RemoteChildren::new(RemoteLedgers::default()).unwrap();
        first
            .sync(&directory(child()), &client, Wake::default())
            .unwrap();
        changes(&mut first);
        let saved = first.ledgers().clone();
        drain(&mut first);
        let mut reconnected = RemoteChildren::new(saved).unwrap();
        let before_directory = serde_json::to_value(reconnected.ledgers()).unwrap();
        assert!(
            reconnected.poll().unwrap().is_empty(),
            "live before the child directory must not remove saved context"
        );
        assert_eq!(
            serde_json::to_value(reconnected.ledgers()).unwrap(),
            before_directory
        );
        reconnected
            .sync(&directory(child()), &client, Wake::default())
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(4);
        loop {
            assert!(
                reconnected.poll().unwrap().is_empty(),
                "unchanged reconnect replayed delivered context"
            );
            if fs::read_to_string(client.cwd.join("argv"))
                .unwrap()
                .lines()
                .count()
                == 2
                && reconnected
                    .active
                    .values()
                    .all(|observation| observation.view.phase() == WatchPhase::Live)
            {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "reconnected child did not become live"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        reconnected
            .sync(&BTreeMap::new(), &client, Wake::default())
            .unwrap();
        let removed = reconnected.poll().unwrap();
        assert_eq!(removed.len(), 2);
        assert!(
            removed
                .iter()
                .all(|change| matches!(change.change, ContextChange::RemoveItem { .. }))
        );
        assert!(reconnected.ledgers().children.is_empty());
        drain(&mut reconnected);
        assert_eq!(
            fs::read_to_string(client.cwd.join("stopped")).unwrap(),
            "stoppedstopped"
        );
    }

    #[test]
    fn a_failed_child_watch_preserves_delivered_context_and_never_retries() {
        let (_scratch, client) = fixture();
        let mut children = RemoteChildren::new(RemoteLedgers::default()).unwrap();
        children
            .sync(&directory(child()), &client, Wake::default())
            .unwrap();
        changes(&mut children);
        let delivered = serde_json::to_value(children.ledgers()).unwrap();
        fs::write(client.cwd.join("fail"), "fail this watch").unwrap();
        let deadline = Instant::now() + Duration::from_secs(4);
        loop {
            match children.poll() {
                Err(error) => {
                    assert!(error.contains("disconnected"));
                    break;
                }
                Ok(changes) => assert!(changes.is_empty()),
            }
            assert!(Instant::now() < deadline, "watch error did not arrive");
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(serde_json::to_value(children.ledgers()).unwrap(), delivered);
        assert!(
            children
                .sync(&directory(child()), &client, Wake::default())
                .is_err()
        );
        drain(&mut children);
        assert_eq!(
            fs::read_to_string(client.cwd.join("argv"))
                .unwrap()
                .lines()
                .count(),
            1
        );
    }

    #[test]
    fn invalid_child_directories_and_saved_limits_start_no_process() {
        let (_scratch, client) = fixture();
        for bad in [
            Child {
                reference: "--participate".into(),
                ..child()
            },
            Child {
                reference: "bad\nreference".into(),
                ..child()
            },
        ] {
            let mut children = RemoteChildren::new(RemoteLedgers::default()).unwrap();
            assert!(
                children
                    .sync(&directory(bad), &client, Wake::default())
                    .is_err()
            );
            assert!(children.is_finished());
        }
        let many: BTreeMap<_, _> = (0..=MAX_CHILDREN)
            .map(|index| {
                let child = Child {
                    thread_id: format!("child-{index}"),
                    reference: format!("parent#s={index}"),
                    ..child()
                };
                (child.thread_id.clone(), child)
            })
            .collect();
        let mut children = RemoteChildren::new(RemoteLedgers::default()).unwrap();
        assert!(children.sync(&many, &client, Wake::default()).is_err());
        assert!(!client.cwd.join("argv").exists());
        let forged = json!({"children":{"--bad":{"thread_id":"child-thread","name":"Scout","ledger":{"items":{},"comments":{}}}}});
        assert!(RemoteChildren::new(serde_json::from_value(forged).unwrap()).is_err());
    }
}
