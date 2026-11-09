//! A subagent's producer bootstrap under its live parent's confirmed scope.
//!
//! The caller supplies an actual, healthy parent connection and its in-memory
//! native grant. Audit JSON is recorded provenance, not an authority factory.
//! This module creates no mounts and grants no filesystem or execution scope.

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use rig_basics::subagents::{SpawnSubagent, SpawnedAgent};
use rig_harness::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{Binding, Client, Command, CommandReply};
use crate::LiveWorkspace;
use crate::live_journal::{InitialMode, Journal};
use crate::process::Event;

const MAX_STATE_BYTES: usize = 2 * 1024 * 1024;
const MAX_ANCESTRY: usize = 64;
const MAX_SEQUENCE: u64 = 9_007_199_254_740_991;

pub(crate) struct ChildConnected {
    pub agent: Entity,
    pub parent: Entity,
    pub binding: Binding,
    pub client: Client,
    pub journal: Journal,
    /// Derived scope retaining the real root consent, not a new Person answer.
    pub grant: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stage {
    Create,
    Creating,
    Status,
    ReadingStatus,
    Complete,
}

/// Durable child identity saved before the next fallible command. A returned
/// reference is opaque and is compared verbatim on a later explicit recovery.
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Bootstrap {
    version: u32,
    producer: String,
    parent_producer: String,
    parent_reference: String,
    directory: PathBuf,
    reference: Option<String>,
    thread: Option<String>,
}

struct BootstrapLock(PathBuf);
impl Drop for BootstrapLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

/// One bootstrap owns one bounded command at a time. Cancellation and errors
/// retain it until `drained()` observes the terminal result; there is no retry.
pub(crate) struct ChildFlow {
    agent: Entity,
    parent: Entity,
    id: AgentId,
    parent_id: AgentId,
    parent_binding: Binding,
    client: Client,
    grant: Value,
    title: String,
    brief: Option<String>,
    bootstrap: Bootstrap,
    path: PathBuf,
    stage: Stage,
    command: Option<Command>,
    cancelled: bool,
    failure: Option<String>,
    _lock: BootstrapLock,
}

impl ChildFlow {
    /// The controller must call this only for a healthy parent whose native
    /// confirmation included this session's actual descendants. The supplied
    /// `brief`, when present, must be cached from that child's SpawnSubagent.
    pub(crate) fn new(
        world: &mut World,
        agent: Entity,
        parent: Entity,
        mut client: Client,
        parent_binding: Binding,
        parent_grant: String,
        brief: Option<String>,
    ) -> Result<Self, String> {
        let id = world
            .get::<AgentId>(agent)
            .cloned()
            .ok_or("The Delta subagent has no stable identity")?;
        let parent_id = world
            .get::<AgentId>(parent)
            .cloned()
            .ok_or("The Delta parent disappeared")?;
        safe_producer(&id.0)?;
        if parent_id.0 != parent_binding.producer {
            return Err("The live Delta parent does not own the supplied producer".into());
        }
        validate_ancestry(world, agent, parent)?;
        let directory = child_directory(world, agent, parent, &parent_binding)?;
        client.valid()?;
        let mut grant: Value = serde_json::from_str(&parent_grant).map_err(|error| {
            format!("The live parent's Delta consent audit is invalid: {error}")
        })?;
        validate_parent_grant(&grant, &client, &parent_binding)?;
        // This retargets only fixed control commands to an already approved
        // descendant directory; it does not install or widen a coding scope.
        client.cwd = directory.clone();
        client.valid()?;

        let actual = actual_assignment(world, agent);
        if let Some((_, actual_brief)) = &actual
            && brief.as_ref().is_some_and(|cached| cached != actual_brief)
        {
            return Err("The cached child assignment differs from its actual spawn effect".into());
        }
        let title = actual
            .as_ref()
            .map(|(title, _)| single_line_title(title))
            .unwrap_or_else(|| "Rig subagent".into());
        let brief = actual.map(|(_, brief)| brief).or(brief);
        if brief
            .as_ref()
            .is_some_and(|brief| brief.len() > 16 * 1024 * 1024)
        {
            return Err(
                "The parent assignment exceeds the Delta record bound; no content was truncated"
                    .into(),
            );
        }
        let parent_reference = parent_binding
            .locator
            .clone()
            .unwrap_or_else(|| parent_binding.thread.clone());
        argument(&parent_reference)?;
        let directory_state = producer_directory(&client.home, &id.0)?;
        match fs::symlink_metadata(directory_state.join("recording.lock")) {
            Ok(_) => return Err("The child already has an active or crash-preserved recording lock; no bootstrap was started".into()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
            Err(error) => return Err(format!("Cannot inspect the child's recording ownership: {error}")),
        }
        let lock_path = directory_state.join("child-bootstrap.lock");
        let mut lock_file = OpenOptions::new().write(true).create_new(true).open(&lock_path)
            .map_err(|error| format!("Cannot lock Delta child bootstrap at {}: {error}. Inspect a stale crash lock before removing only the lock.", lock_path.display()))?;
        let lock = BootstrapLock(lock_path);
        writeln!(lock_file, "process={}", std::process::id())
            .and_then(|()| lock_file.sync_all())
            .map_err(|error| format!("Cannot flush the child bootstrap lock: {error}"))?;
        let path = directory_state.join("child-bootstrap.json");
        let mut bootstrap = match read_regular(&path)? {
            Some(bytes) => {
                let state: Bootstrap = serde_json::from_slice(&bytes).map_err(|error| {
                    format!("Invalid saved child bootstrap; preserved without sending: {error}")
                })?;
                if state.version != 1
                    || state.producer != id.0
                    || state.parent_producer != parent_binding.producer
                    || state.parent_reference != parent_reference
                    || state.directory != directory
                {
                    return Err("The child bootstrap belongs to a different producer, parent or approved directory; it was not reset".into());
                }
                if let Some(reference) = &state.reference {
                    argument(reference)?;
                }
                if let Some(thread) = &state.thread {
                    argument(thread)?;
                }
                state
            }
            None => Bootstrap {
                version: 1,
                producer: id.0.clone(),
                parent_producer: parent_binding.producer.clone(),
                parent_reference,
                directory,
                reference: None,
                thread: None,
            },
        };
        if let Some(binding) = world.get::<Binding>(agent) {
            if binding.producer != id.0 || binding.joined || binding.owner != id.0 {
                return Err("The subagent carries a copied or joined Delta binding and cannot reuse its producer".into());
            }
            let reference = binding
                .locator
                .as_ref()
                .ok_or("The saved child binding lost its opaque reference")?;
            argument(reference)?;
            if bootstrap
                .reference
                .as_ref()
                .is_some_and(|saved| saved != reference)
                || bootstrap
                    .thread
                    .as_ref()
                    .is_some_and(|saved| saved != &binding.thread)
                || !same_directory(&binding.checkout, &client.cwd)
                || binding.options != parent_binding.options
            {
                return Err("The saved child binding conflicts with its parent/bootstrap; no identity was replaced".into());
            }
            bootstrap.reference = Some(reference.clone());
            bootstrap.thread = Some(binding.thread.clone());
        }
        // Preserve the actual native consent records while making delegation
        // explicit. A JSON field saying 'person' is never accepted by itself:
        // the caller already owns the healthy parent connection's live grant.
        let object = grant
            .as_object_mut()
            .ok_or("Delta parent grant must be an object")?;
        object.insert(
            "authority".into(),
            json!("derived_native_parent_subagent_scope"),
        );
        object.insert("producer".into(), json!(id.0));
        object.insert(
            "client".into(),
            serde_json::to_value(&client).map_err(|error| error.to_string())?,
        );
        object.insert("derived_scope".into(), json!({
            "kind":"actual_rig_subagent", "parent_producer":parent_binding.producer,
            "parent_reference":bootstrap.parent_reference, "directory":client.cwd,
            "processes":"one bounded child create/status bootstrap, then child watch and immutable appends",
            "mounts":"borrow the existing parent mount; no new root, mount or execution grant",
        }));
        let flow = Self {
            agent,
            parent,
            id,
            parent_id,
            parent_binding,
            client,
            grant,
            title,
            brief,
            bootstrap,
            path,
            stage: Stage::Create,
            command: None,
            cancelled: false,
            failure: None,
            _lock: lock,
        };
        flow.validate(world)?;
        flow.persist_identity()?;
        flow.persist_grant()?; // Required before the first host CLI operation.
        Ok(flow)
    }

    pub(crate) fn tick(&mut self, world: &mut World) -> Result<Option<ChildConnected>, String> {
        if self.cancelled {
            return Err("Delta child bootstrap was cancelled; no retry was started".into());
        }
        if let Some(error) = &self.failure {
            return Err(error.clone());
        }
        let result = self.advance(world);
        if let Err(error) = &result {
            self.failure = Some(error.clone());
            if let Some(command) = &self.command {
                command.process.request_stop();
            }
        }
        result
    }

    pub(crate) fn cancel(&mut self) {
        self.cancelled = true;
        if let Some(command) = &self.command {
            command.process.request_stop();
        }
    }

    /// Keep this flow until the owned worker has observed its terminal result.
    /// Dropping a process alone requests cleanup but cannot observe the flush.
    pub(crate) fn drained(&mut self) -> bool {
        self.cancel();
        let Some(command) = &mut self.command else {
            return true;
        };
        for _ in 0..128 {
            if command.process.is_finished() {
                break;
            }
            match command.process.try_event() {
                Some(Event::Line(_)) => (),
                Some(Event::Exited { .. } | Event::Failed { .. }) => break,
                None => break,
            }
        }
        command.process.is_finished()
    }

    fn advance(&mut self, world: &mut World) -> Result<Option<ChildConnected>, String> {
        self.validate(world)?;
        if let Some(reply) = self.command.as_mut().and_then(Command::poll) {
            // Command::poll can report bounded-output failure before the worker
            // has terminated. Retain that handle for the controller's drain.
            if !self
                .command
                .as_ref()
                .is_some_and(|command| command.process.is_finished())
            {
                return Err(reply.error.unwrap_or_else(|| {
                    "Delta child command ended before terminal capture".into()
                }));
            }
            self.command.take();
            let value = command_json(reply)?;
            match self.stage {
                Stage::Creating => self.created(value)?,
                Stage::ReadingStatus => return self.status(world, value).map(Some),
                _ => {
                    return Err(
                        "Delta child bootstrap received an out-of-order command result".into(),
                    );
                }
            }
        }
        if self.command.is_some() {
            return Ok(None);
        }
        match self.stage {
            Stage::Create => {
                // The CLI, not a mutable local locator, establishes parentage.
                // Repeating this pair is only part of an explicitly recreated
                // bootstrap, never an automatic retry after a failed command.
                let args = vec![
                    "--json".into(),
                    "thread".into(),
                    "create".into(),
                    "--source".into(),
                    "rig".into(),
                    "--external-id".into(),
                    self.id.0.clone(),
                    "--parent".into(),
                    self.bootstrap.parent_reference.clone(),
                    "--name".into(),
                    self.title.clone(),
                    "--shared-worktrees".into(),
                ];
                self.validate(world)?;
                self.command = Some(Command::start(
                    &self.client,
                    args,
                    None,
                    world.resource::<Wake>().clone(),
                )?);
                self.stage = Stage::Creating;
            }
            Stage::Status => {
                let reference = self
                    .bootstrap
                    .reference
                    .clone()
                    .ok_or("The child reference was not persisted")?;
                let args = vec![
                    "--json".into(),
                    "thread".into(),
                    "status".into(),
                    reference,
                    "--source".into(),
                    "rig".into(),
                    "--external-id".into(),
                    self.id.0.clone(),
                ];
                self.validate(world)?;
                self.command = Some(Command::start(
                    &self.client,
                    args,
                    None,
                    world.resource::<Wake>().clone(),
                )?);
                self.stage = Stage::ReadingStatus;
            }
            Stage::Complete => (),
            Stage::Creating | Stage::ReadingStatus => {
                return Err(
                    "The owned child command disappeared; no replacement was started".into(),
                );
            }
        }
        Ok(None)
    }

    fn validate(&self, world: &World) -> Result<(), String> {
        if world.get::<AgentId>(self.agent) != Some(&self.id)
            || world.get::<AgentId>(self.parent) != Some(&self.parent_id)
        {
            return Err("The Delta child or parent identity changed during bootstrap".into());
        }
        validate_ancestry(world, self.agent, self.parent)?;
        let parent = world
            .get::<Binding>(self.parent)
            .ok_or("The live parent's Delta binding was removed")?;
        if parent.producer != self.parent_binding.producer
            || parent.thread != self.parent_binding.thread
            || parent.locator != self.parent_binding.locator
            || parent.checkout != self.parent_binding.checkout
            || parent.worktree != self.parent_binding.worktree
            || parent.options != self.parent_binding.options
        {
            return Err(
                "The parent's Delta recording scope changed; the stale child bootstrap stopped"
                    .into(),
            );
        }
        if child_directory(world, self.agent, self.parent, &self.parent_binding)? != self.client.cwd
        {
            return Err("The child working directory changed during bootstrap".into());
        }
        if let Some(binding) = world.get::<Binding>(self.agent)
            && (binding.producer != self.id.0
                || binding.joined
                || self
                    .bootstrap
                    .reference
                    .as_ref()
                    .is_some_and(|reference| binding.locator.as_ref() != Some(reference))
                || self
                    .bootstrap
                    .thread
                    .as_ref()
                    .is_some_and(|thread| &binding.thread != thread))
        {
            return Err("The child's saved Delta identity changed during bootstrap".into());
        }
        self.client.valid()
    }

    fn created(&mut self, value: Value) -> Result<(), String> {
        if value["operation"] != "created" || !value["created"].is_boolean() {
            return Err("Delta did not return a valid child creation receipt".into());
        }
        let reference = string_field(&value, "reference")?;
        argument(&reference)?;
        if self
            .bootstrap
            .reference
            .as_ref()
            .is_some_and(|old| old != &reference)
        {
            return Err("Delta returned a different opaque child reference; the saved identity was preserved".into());
        }
        self.bootstrap.reference = Some(reference);
        self.persist_identity()?; // Before status, watch, or append can start.
        self.persist_grant()?;
        self.stage = Stage::Status;
        Ok(())
    }

    fn status(&mut self, world: &mut World, value: Value) -> Result<ChildConnected, String> {
        if value["operation"] != "status"
            || value["producer"]["source"] != "rig"
            || value["producer"]["external_id"] != self.id.0
            || !matches!(
                value["access"]["state"].as_str(),
                Some("granted" | "local_only")
            )
        {
            return Err("Delta child status did not confirm this producer and access".into());
        }
        let next = value["producer"]["next_sequence"]
            .as_u64()
            .filter(|number| *number > 0 && *number <= MAX_SEQUENCE)
            .ok_or("Delta child status has no positive safe sequence frontier")?;
        let thread = string_field(&value, "thread_id")?;
        argument(&thread)?;
        if self
            .bootstrap
            .thread
            .as_ref()
            .is_some_and(|saved| saved != &thread)
        {
            return Err("Delta child canonical identity changed; no journal was rebound".into());
        }
        let reference = self
            .bootstrap
            .reference
            .clone()
            .ok_or("Missing durable opaque child reference")?;
        let selected_worktree = selected_worktree(
            world,
            self.agent,
            self.parent,
            &self.parent_binding,
            &self.client.cwd,
        );
        if value["worktrees"]["status"] != "available" {
            return Err("Delta cannot verify the child's shared worktree state".into());
        }
        if let Some(expected) = &selected_worktree {
            let items = value["worktrees"]["items"]
                .as_array()
                .ok_or("Delta cannot verify the child's shared worktrees")?;
            if !items
                .iter()
                .any(|tree| tree["reference"].as_str() == Some(expected.as_str()))
            {
                return Err(
                    "The child does not share the parent's approved active worktree".into(),
                );
            }
        }
        let mut journal = Journal::open(
            &self.client.home,
            &self.id.0,
            &thread,
            next,
            InitialMode::Joined,
        )?;
        if journal
            .options()
            .is_some_and(|options| options != self.parent_binding.options)
        {
            return Err(
                "The child's saved export policy differs from its live parent; it was not expanded"
                    .into(),
            );
        }
        let conversation = world
            .get::<Conversation>(self.agent)
            .ok_or("The child conversation disappeared")?;
        let open = journal.open_turns();
        if open.len() > 1 {
            return Err(
                "The child journal has ambiguous unfinished turns; no output was reassigned".into(),
            );
        }
        // A restored child's last retained output still belongs to its old
        // native turn. Reconcile it before ending that turn, never as history
        // under a new one. New Joined journals have no open turn.
        journal.capture(
            conversation,
            open.first().copied(),
            self.parent_binding.options,
        )?;
        if let Some(sequence) = open.first() {
            journal.finish_turn(*sequence)?;
        }
        if let Some(brief) = &self.brief {
            journal.stage(
                json!({
                    "sessionUpdate":"user_message", "messageId":"parent-assignment",
                    "content":[{"type":"text", "text":brief}],
                    "_meta":{"delta":{"sender":{"external_id":self.parent_binding.producer}}},
                }),
                None,
            )?;
        }
        let url = value["url"].as_str().map(str::to_owned);
        if let Some(url) = &url {
            safe_url(url)?;
        }
        let binding = Binding {
            producer: self.id.0.clone(),
            thread: thread.clone(),
            locator: Some(reference),
            joined: false,
            owner: self.id.0.clone(),
            checkout: self.client.cwd.clone(),
            worktree: selected_worktree,
            url,
            visibility: value["visibility"]
                .as_str()
                .map(str::to_owned)
                .or_else(|| self.parent_binding.visibility.clone()),
            organization: self.parent_binding.organization.clone(),
            options: self.parent_binding.options,
        };
        self.bootstrap.thread = Some(thread);
        self.persist_identity()?;
        self.persist_grant()?;
        let directory = self.path.parent().ok_or("Child state has no directory")?;
        write_private(
            &directory.join("binding.json"),
            &serde_json::to_vec(&binding).map_err(|error| error.to_string())?,
        )?;
        // The controller installs Binding only together with the child watch/
        // ownership gate; a bootstrap receipt alone cannot release coding.
        self.stage = Stage::Complete;
        Ok(ChildConnected {
            agent: self.agent,
            parent: self.parent,
            binding,
            client: self.client.clone(),
            journal,
            grant: serde_json::to_string(&self.grant).map_err(|error| error.to_string())?,
        })
    }

    fn persist_identity(&self) -> Result<(), String> {
        write_private(
            &self.path,
            &serde_json::to_vec(&self.bootstrap).map_err(|error| error.to_string())?,
        )
    }

    fn persist_grant(&self) -> Result<(), String> {
        let directory = self.path.parent().ok_or("Child state has no directory")?;
        write_private(
            &directory.join("child-grant.json"),
            &serde_json::to_vec(&self.grant).map_err(|error| error.to_string())?,
        )
    }
}

fn validate_parent_grant(value: &Value, client: &Client, parent: &Binding) -> Result<(), String> {
    let authority = value["authority"].as_str();
    if value["version"] != 1
        || value["producer"] != parent.producer
        || !matches!(
            authority,
            Some(
                "native_person_confirmed_delta_host_workflow"
                    | "derived_native_parent_subagent_scope"
            )
        )
        || value["client"]["executable"] != json!(client.executable)
        || value["client"]["environment"] != json!(client.environment)
        || value["options"]
            != serde_json::to_value(parent.options).map_err(|error| error.to_string())?
    {
        return Err("The supplied live parent grant does not match its producer, executable, environment or export scope".into());
    }
    let consents = value["consents"]
        .as_array()
        .ok_or("The parent grant has no retained native consent records")?;
    if consents.is_empty()
        || consents
            .iter()
            .any(|consent| consent["decided_by"] != "person")
    {
        return Err(
            "The parent's native consent provenance is absent; no child process was started".into(),
        );
    }
    Ok(())
}

fn validate_ancestry(world: &World, child: Entity, ancestor: Entity) -> Result<(), String> {
    if child == ancestor {
        return Err("A Delta subagent cannot be its own parent".into());
    }
    let mut current = child;
    for _ in 0..MAX_ANCESTRY {
        let parent = world
            .get::<SpawnedBy>(current)
            .ok_or("The Delta child is not an actual descendant of this live parent")?;
        if parent.0 == ancestor {
            return Ok(());
        }
        if parent.0 == child || parent.0 == current {
            break;
        }
        current = parent.0;
    }
    Err("The Delta child ancestry is cyclic or exceeds its bound".into())
}

fn child_directory(
    world: &World,
    child: Entity,
    parent: Entity,
    binding: &Binding,
) -> Result<PathBuf, String> {
    let directory = world
        .get::<WorkDir>(child)
        .ok_or("The Delta child has no working directory")?
        .0
        .canonicalize()
        .map_err(|error| format!("Cannot resolve the child's working directory: {error}"))?;
    let parent_id = world
        .get::<AgentId>(parent)
        .ok_or("The live parent has no identity")?;
    if let Some(scope) = world.get::<LiveWorkspace>(parent) {
        if !scope.allows(parent_id, &directory) {
            return Err("The child is outside the parent's approved joined worktrees; no new root was granted".into());
        }
    } else {
        let root = binding
            .checkout
            .canonicalize()
            .map_err(|error| format!("The parent's shared checkout disappeared: {error}"))?;
        if root != binding.checkout || !directory.starts_with(&root) {
            return Err("The child is outside the parent's confirmed shared checkout; isolated paths need their own trust flow".into());
        }
    }
    if let Some(scope) = world.get::<LiveWorkspace>(child) {
        let id = world
            .get::<AgentId>(child)
            .ok_or("The child has no identity")?;
        if !scope.allows(id, &directory) {
            return Err("The child's inherited workspace approval is stale or copied".into());
        }
    }
    Ok(directory)
}

fn selected_worktree(
    world: &World,
    child: Entity,
    parent_agent: Entity,
    parent: &Binding,
    directory: &Path,
) -> Option<String> {
    parent.worktree.clone().or_else(|| {
        world
            .get::<LiveWorkspace>(child)
            .or_else(|| world.get::<LiveWorkspace>(parent_agent))
            .and_then(|scope| {
                scope
                    .roots()
                    .iter()
                    .filter(|(_, root)| directory.starts_with(root))
                    .max_by_key(|(_, root)| root.components().count())
                    .map(|(reference, _)| reference.clone())
            })
    })
}

fn actual_assignment(world: &mut World, child: Entity) -> Option<(String, String)> {
    world
        .query::<(&SpawnedAgent, &SpawnSubagent)>()
        .iter(world)
        .find(|(spawned, _)| spawned.0 == child)
        .map(|(_, request)| (request.title.clone(), request.brief.clone()))
}
fn single_line_title(title: &str) -> String {
    let title: String = title
        .chars()
        .filter(|character| !character.is_control())
        .take(120)
        .collect();
    if title.trim().is_empty() || title.starts_with('-') {
        "Rig subagent".into()
    } else {
        title
    }
}
fn command_json(reply: CommandReply) -> Result<Value, String> {
    if reply.code != Some(0) || reply.error.is_some() {
        return Err(format!(
            "Delta child command failed ({:?}): {}; {}. No automatic retry",
            reply.code,
            reply.error.unwrap_or_default(),
            reply.diagnostics
        ));
    }
    let value: Value = serde_json::from_str(&reply.output)
        .map_err(|error| format!("Invalid Delta child JSON: {error}; {}", reply.diagnostics))?;
    if value.get("error").is_some() {
        return Err(format!(
            "Delta refused the child operation: {}",
            value["error"]
        ));
    }
    Ok(value)
}
fn string_field(value: &Value, field: &str) -> Result<String, String> {
    value[field]
        .as_str()
        .filter(|value| !value.trim().is_empty())
        .map(str::to_owned)
        .ok_or_else(|| format!("Delta child response has no nonblank {field}"))
}
fn argument(value: &str) -> Result<(), String> {
    if value.trim().is_empty()
        || value.len() > 4096
        || value.starts_with('-')
        || value.chars().any(char::is_control)
    {
        Err(
            "Delta child argument must be nonblank, bounded, control-free and not start with '-'"
                .into(),
        )
    } else {
        Ok(())
    }
}
fn safe_producer(value: &str) -> Result<(), String> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        Err("Delta child producer identity is unsafe for durable state".into())
    } else {
        Ok(())
    }
}
fn safe_url(value: &str) -> Result<(), String> {
    if value.starts_with("https://")
        && value.len() <= 8192
        && !value
            .chars()
            .any(|character| character.is_whitespace() || character.is_control())
    {
        Ok(())
    } else {
        Err("Delta returned an invalid child URL".into())
    }
}
fn same_directory(left: &Path, right: &Path) -> bool {
    left.canonicalize()
        .ok()
        .zip(right.canonicalize().ok())
        .is_some_and(|(left, right)| left == right)
}
fn producer_directory(home: &Path, producer: &str) -> Result<PathBuf, String> {
    safe_producer(producer)?;
    let root = home.join("delta");
    private_directory(&root)?;
    let directory = root.join(producer);
    private_directory(&directory)?;
    Ok(directory)
}
fn private_directory(path: &Path) -> Result<(), String> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => Ok(()),
        Ok(_) => Err("Delta child state directory must not be a link or file".into()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir(path)
                .map_err(|error| format!("Cannot create Delta child state directory: {error}"))?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(path, fs::Permissions::from_mode(0o700))
                    .map_err(|error| error.to_string())?;
            }
            Ok(())
        }
        Err(error) => Err(format!(
            "Cannot inspect Delta child state directory: {error}"
        )),
    }
}
fn read_regular(path: &Path) -> Result<Option<Vec<u8>>, String> {
    use std::os::unix::fs::OpenOptionsExt;
    let mut options = OpenOptions::new();
    options
        .read(true)
        .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32);
    let file = match options.open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(format!(
                "Cannot open saved child bootstrap without following links: {error}"
            ));
        }
    };
    let metadata = file.metadata().map_err(|error| error.to_string())?;
    if !metadata.is_file() || metadata.len() > MAX_STATE_BYTES as u64 {
        return Err("Saved Delta child state is not a bounded regular file".into());
    }
    let mut bytes = Vec::new();
    file.take(MAX_STATE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if bytes.len() > MAX_STATE_BYTES {
        return Err("Saved Delta child state exceeds 2 MiB".into());
    }
    Ok(Some(bytes))
}
fn write_private(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if bytes.len() > MAX_STATE_BYTES {
        return Err("Delta child audit/state exceeds 2 MiB".into());
    }
    match fs::symlink_metadata(path) {
        Ok(metadata) if !metadata.is_file() || metadata.file_type().is_symlink() => {
            return Err("Delta child state must not replace a link or non-file".into());
        }
        Ok(_) => (),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
        Err(error) => return Err(format!("Cannot inspect Delta child state: {error}")),
    }
    let parent = path.parent().ok_or("Delta child state has no directory")?;
    let mut temporary =
        tempfile::NamedTempFile::new_in(parent).map_err(|error| error.to_string())?;
    temporary
        .write_all(bytes)
        .and_then(|()| temporary.as_file().sync_all())
        .map_err(|error| format!("Cannot flush Delta child state: {error}"))?;
    temporary
        .persist(path)
        .map_err(|error| format!("Cannot atomically save Delta child state: {error}"))?;
    File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| format!("Cannot flush Delta child state directory: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::live_journal::ExportOptions;
    use rig_harness::rig_core::message::Message;

    fn fixture(home: &Path, root: &Path) -> (World, Entity, Entity, Client, Binding, String) {
        let mut app = App::new();
        app.add_plugins(AgentPlugin);
        let world = app.world_mut();
        let parent_id = AgentId("parent-producer".into());
        let parent_binding = Binding {
            producer: parent_id.0.clone(),
            thread: "parent-thread".into(),
            locator: None,
            joined: false,
            owner: parent_id.0.clone(),
            checkout: root.to_owned(),
            worktree: Some("tree#t=opaque".into()),
            url: None,
            visibility: Some("private".into()),
            organization: Some("organization".into()),
            options: ExportOptions::default(),
        };
        let parent = world
            .spawn((
                Agent,
                parent_id,
                WorkDir(root.to_owned()),
                parent_binding.clone(),
            ))
            .id();
        let mut conversation = Conversation::default();
        conversation.push(Message::user(
            "Inherited checkpoint text must not be re-exported",
        ));
        let child = world
            .spawn((
                Agent,
                AgentId("child-producer".into()),
                SpawnedBy(parent),
                WorkDir(root.to_owned()),
                conversation,
            ))
            .id();
        let executable = PathBuf::from("/bin/sh").canonicalize().unwrap();
        let metadata = executable.metadata().unwrap();
        let client = Client {
            executable,
            environment: std::collections::BTreeMap::new(),
            cwd: root.to_owned(),
            home: home.to_owned(),
            executable_len: metadata.len(),
            executable_modified: metadata.modified().ok(),
        };
        let grant = json!({"version":1,"producer":parent_binding.producer,"authority":"native_person_confirmed_delta_host_workflow",
            "client":client,"options":parent_binding.options,"consents":[{"decided_by":"person"}]}).to_string();
        (
            std::mem::take(app.world_mut()),
            parent,
            child,
            client,
            parent_binding,
            grant,
        )
    }

    #[test]
    fn child_scope_and_parent_identity_are_not_created_by_a_json_grant() {
        let scratch = tempfile::tempdir().unwrap();
        let home = scratch.path().join("home");
        let root = scratch.path().join("root");
        let outside = scratch.path().join("outside");
        fs::create_dir(&home).unwrap();
        fs::create_dir(&root).unwrap();
        fs::create_dir(&outside).unwrap();
        let root = root.canonicalize().unwrap();
        let (mut world, parent, child, client, binding, grant) = fixture(&home, &root);
        world
            .entity_mut(child)
            .insert(WorkDir(outside.canonicalize().unwrap()));
        assert!(
            ChildFlow::new(
                &mut world,
                child,
                parent,
                client.clone(),
                binding.clone(),
                grant.clone(),
                None
            )
            .is_err()
        );
        assert!(!home.join("delta").exists());
        world.entity_mut(child).insert(WorkDir(root.clone()));
        let unrelated = world.spawn((Agent, AgentId("unrelated".into()))).id();
        assert!(
            ChildFlow::new(
                &mut world,
                child,
                unrelated,
                client.clone(),
                binding.clone(),
                grant.clone(),
                None
            )
            .is_err()
        );
        let mut copied = binding.clone();
        copied.producer = "not-the-child".into();
        world.entity_mut(child).insert(copied);
        assert!(ChildFlow::new(&mut world, child, parent, client, binding, grant, None).is_err());
    }

    #[test]
    fn opaque_child_reference_precedes_status_and_joined_baseline_never_exports_parent_history() {
        let scratch = tempfile::tempdir().unwrap();
        let home = scratch.path().join("home");
        let root = scratch.path().join("root");
        fs::create_dir(&home).unwrap();
        fs::create_dir(&root).unwrap();
        let root = root.canonicalize().unwrap();
        let (mut world, parent, child, client, binding, grant) = fixture(&home, &root);
        let mut flow = ChildFlow::new(
            &mut world,
            child,
            parent,
            client,
            binding,
            grant,
            Some("Inspect this parser".into()),
        )
        .unwrap();
        let reference = "parent-thread#s=Opaque/Child+StillOpaque";
        flow.created(json!({"operation":"created","created":true,"reference":reference,"thread_id":"not-used-as-a-reference"})).unwrap();
        let saved: Bootstrap = serde_json::from_slice(&fs::read(&flow.path).unwrap()).unwrap();
        assert_eq!(saved.reference.as_deref(), Some(reference));
        let mut connected = flow.status(&mut world,json!({"operation":"status","thread_id":"canonical-child",
            "producer":{"source":"rig","external_id":"child-producer","next_sequence":1},
            "access":{"state":"granted"},"worktrees":{"status":"available","items":[{"reference":"tree#t=opaque"}]},"url":null,"visibility":"private"})).unwrap();
        assert_eq!(connected.binding.locator.as_deref(), Some(reference));
        let batch = connected.journal.batch().unwrap();
        assert!(!batch.input.contains("Inherited checkpoint"));
        let assignment: Value = serde_json::from_str(batch.input.trim()).unwrap();
        assert_eq!(
            assignment["update"]["content"][0]["text"],
            "Inspect this parser"
        );
        assert_eq!(
            assignment["update"]["_meta"]["delta"]["sender"]["external_id"],
            "parent-producer"
        );
        assert!(home.join("delta/child-producer/child-grant.json").is_file());
        assert!(home.join("delta/child-producer/binding.json").is_file());
    }

    #[test]
    fn conflicting_reference_and_missing_journal_do_not_reset_an_existing_child_producer() {
        let scratch = tempfile::tempdir().unwrap();
        let home = scratch.path().join("home");
        let root = scratch.path().join("root");
        fs::create_dir(&home).unwrap();
        fs::create_dir(&root).unwrap();
        let root = root.canonicalize().unwrap();
        let (mut world, parent, child, client, binding, grant) = fixture(&home, &root);
        let mut flow =
            ChildFlow::new(&mut world, child, parent, client, binding, grant, None).unwrap();
        flow.created(
            json!({"operation":"created","created":false,"reference":"parent#s=original"}),
        )
        .unwrap();
        let before = fs::read(&flow.path).unwrap();
        assert!(
            flow.created(
                json!({"operation":"created","created":false,"reference":"parent#s=different"})
            )
            .is_err()
        );
        assert_eq!(fs::read(&flow.path).unwrap(), before);
        assert!(flow.status(&mut world,json!({"operation":"status","thread_id":"canonical-child",
            "producer":{"source":"rig","external_id":"child-producer","next_sequence":2},
            "access":{"state":"granted"},"worktrees":{"status":"available","items":[{"reference":"tree#t=opaque"}]},"url":null,"visibility":"private"})).is_err());
        assert!(!home.join("delta/child-producer/live.json").exists());
    }
}
