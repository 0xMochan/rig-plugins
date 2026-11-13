//! Native, explicitly consented Delta collaboration sessions.
//!
//! The host-application permission here is distinct from native_shell review.
//! Only native Person answers can enable the closed Delta protocol surface;
//! ordinary model tools cannot obtain a process handle or execute remote calls.

mod children;
mod flow;
mod remote_children;

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use rig_harness::prelude::*;
use rig_harness::rig_core::message::{AssistantContent, Message};
use rig_harness::rig_core::streaming::{PartKind, StreamEvent};
use rig_harness::rig_core::tool::{Scope, ToolExecutionError, ToolOutput, ToolResult, Touches};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::live_journal::{ExportOptions, Journal};
use crate::process::{Event, Lifetime, Process, Request};
use crate::watch::{ContextChange, ContextLedger, WatchEffect, WatchPhase, WatchView};
use crate::{DeltaConfig, LiveWorkspace};
use children::{ChildConnected, ChildFlow};
use flow::{Connected, Flow, FlowProgress};
use remote_children::{RemoteChildren, RemoteLedgers};

const MAX_CONNECTIONS: usize = 32;
const MAX_COMMAND_OUTPUT: usize = 16 * 1024 * 1024;
const MAX_STREAM_EVENTS: usize = 4096;
const APPEND_INTERVAL: Duration = Duration::from_millis(150);

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Client {
    pub executable: PathBuf,
    pub environment: BTreeMap<String, String>,
    pub cwd: PathBuf,
    pub home: PathBuf,
    executable_len: u64,
    executable_modified: Option<SystemTime>,
}

impl Client {
    fn prepare(world: &World, agent: Entity) -> Result<Self, String> {
        let configured = &world.resource::<DeltaConfig>().executable;
        let selected = if Path::new(configured).is_absolute() {
            PathBuf::from(configured)
        } else if Path::new(configured).components().count() == 1 && !configured.is_empty() {
            std::env::var_os("PATH")
                .into_iter()
                .flat_map(|paths| std::env::split_paths(&paths).collect::<Vec<_>>())
                .filter(|path| path.is_absolute())
                .map(|path| path.join(configured))
                .find(|path| path.is_file())
                .ok_or(
                    "Delta was not found. Install it separately or configure [delta] executable.",
                )?
        } else {
            return Err(
                "[delta] executable must be an installed command name or absolute path".into(),
            );
        };
        let executable = selected
            .canonicalize()
            .map_err(|error| format!("Cannot resolve Delta executable: {error}"))?;
        let metadata = executable
            .metadata()
            .map_err(|error| format!("Cannot inspect Delta executable: {error}"))?;
        if !metadata.is_file() {
            return Err("Delta executable is not a regular file".into());
        }
        let requested = world
            .get::<WorkDir>(agent)
            .map(|dir| &dir.0)
            .unwrap_or(&world.resource::<ProjectRoot>().0);
        let cwd = requested
            .canonicalize()
            .or_else(|error| {
                if world
                    .get::<Binding>(agent)
                    .is_some_and(|binding| binding.joined)
                {
                    // Only the control-process cwd: coding roots require a fresh
                    // ready mount and native directory trust on reconnect.
                    world.resource::<ProjectRoot>().0.canonicalize()
                } else {
                    Err(error)
                }
            })
            .map_err(|error| format!("Cannot resolve Delta control directory: {error}"))?;
        let mut environment = BTreeMap::new();
        for name in [
            "PATH",
            "HOME",
            "USER",
            "LOGNAME",
            "LANG",
            "LC_ALL",
            "LC_CTYPE",
            "TMPDIR",
            "XDG_CONFIG_HOME",
            "XDG_CACHE_HOME",
            "XDG_DATA_HOME",
            "XDG_RUNTIME_DIR",
            "DISPLAY",
            "WAYLAND_DISPLAY",
            "DBUS_SESSION_BUS_ADDRESS",
        ] {
            if let Ok(value) = std::env::var(name) {
                environment.insert(name.to_owned(), value);
            }
        }
        let home = RigHome::of(world)
            .root()
            .canonicalize()
            .map_err(|error| format!("Cannot resolve Rig's private Delta state home: {error}"))?;
        Ok(Self {
            executable,
            environment,
            cwd,
            home,
            executable_len: metadata.len(),
            executable_modified: metadata.modified().ok(),
        })
    }

    pub(crate) fn valid(&self) -> Result<(), String> {
        let metadata = self
            .executable
            .metadata()
            .map_err(|error| format!("Delta executable disappeared: {error}"))?;
        if !metadata.is_file()
            || self.executable.canonicalize().ok().as_ref() != Some(&self.executable)
            || metadata.len() != self.executable_len
            || metadata.modified().ok() != self.executable_modified
        {
            return Err(
                "The Delta executable changed after consent. Reconnect and approve it again."
                    .into(),
            );
        }
        if self.home.canonicalize().ok().as_ref() != Some(&self.home) || !self.home.is_dir() {
            return Err("The private Rig home changed after Delta consent.".into());
        }
        if self.cwd.canonicalize().ok().as_ref() != Some(&self.cwd) || !self.cwd.is_dir() {
            return Err("The approved Delta working directory changed or disappeared.".into());
        }
        Ok(())
    }

    /// Args are exact argv, including --json. No shell or ambient env is added.
    pub(crate) fn request(
        &self,
        args: Vec<String>,
        stdin: Option<Vec<u8>>,
        session: bool,
    ) -> Request {
        Request {
            executable: self.executable.clone(),
            args,
            cwd: self.cwd.clone(),
            environment: self.environment.clone(),
            stdin,
            lifetime: if session {
                Lifetime::Session {
                    startup_timeout: Duration::from_secs(30),
                }
            } else {
                Lifetime::Command {
                    timeout: Duration::from_secs(60),
                }
            },
            shutdown_grace: Duration::from_secs(5),
        }
    }
}

/// Saved identity is not a saved execution or workspace permission. Restoring
/// this component leaves the agent paused until explicit native reconnection.
#[derive(Component, Reflect, Clone, Debug, Serialize, Deserialize)]
#[reflect(opaque, Component, Saved, Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Binding {
    pub producer: String,
    pub thread: String,
    pub locator: Option<String>,
    pub joined: bool,
    pub owner: String,
    pub checkout: PathBuf,
    pub worktree: Option<String>,
    pub url: Option<String>,
    pub visibility: Option<String>,
    pub organization: Option<String>,
    pub options: ExportOptions,
}

#[derive(Component, Reflect, Clone, Debug, Serialize, Deserialize)]
#[reflect(opaque, Component, Saved, Clone, Debug, Serialize, Deserialize)]
struct ContextCheckpoint {
    producer: String,
    thread: String,
    ledger: ContextLedger,
    #[serde(default)]
    children: RemoteLedgers,
}

#[derive(Clone)]
pub(crate) enum Action {
    Share(String),
    Join(String),
    Reconnect,
    Setup,
    Admin(Vec<String>),
}

#[derive(Resource, Default)]
struct AssignmentCache(BTreeMap<Entity, String>);

#[derive(Resource, Default)]
struct Runtime {
    flows: BTreeMap<Entity, Flow>,
    draining: Vec<Flow>,
    draining_processes: Vec<Process>,
    children: BTreeMap<Entity, ChildFlow>,
    draining_children: Vec<ChildFlow>,
    rejected_children: BTreeMap<Entity, String>,
    assignments: BTreeMap<Entity, String>,
    connections: BTreeMap<Entity, Collaboration>,
    streams: VecDeque<StreamDelta>,
    reconnect: BTreeSet<Entity>,
    auto_seen: BTreeSet<String>,
    stream_overflow: BTreeSet<Entity>,
    holds: Vec<Entity>,
    closing: bool,
    clock: Option<Entity>,
}

#[derive(Component)]
struct OwnershipHold {
    agent: Entity,
}
#[derive(Component)]
struct ExitHold;

pub(crate) fn install(app: &mut App) {
    if !app.is_plugin_added::<rig_harness::ask::AskPlugin>() {
        app.add_plugins(rig_harness::ask::AskPlugin);
    }
    app.init_resource::<Runtime>()
        .init_resource::<AssignmentCache>()
        .keeps_saved::<Binding>()
        .keeps_saved::<ContextCheckpoint>()
        .add_observer(capture_assignment)
        .add_systems(Update, tick.after(PollTasks).before(AgentLoop))
        .add_systems(PostUpdate, collect_changes)
        .add_systems(AgentPass, (ownership_gate.in_set(crate::DeltaSystems::Ownership), hold_exit).chain().in_set(Stage::Gate).before(rig_native_shell::NativeShellSystems::Freeze))
        .add_open_tool("delta_session", "Inspect this agent's live Delta connection, audience, returned URL, participants, worktrees and paused state. Metadata only; never starts a process, uploads, reconnects, or grants permissions.", ToolOptions::default(), session_status)
        .add_open_tool("delta_worktree", "Select one already native-user-approved worktree in a joined live Delta session. Does not add roots, mount files, or approve execution. File tools and native commands remain confined to the selected workdir; other approved roots require explicit selection.", ToolOptions { footprint: Some(Touches::Writes(Scope::Anything)) }, select_worktree);
}

pub(crate) fn start(world: &mut World, agent: Entity, action: Action) {
    let can_answer = world
        .query::<&Answerer>()
        .iter(world)
        .any(|answerer| answerer.agent.is_none() || answerer.agent == Some(agent));
    if !can_answer {
        notice(
            world,
            agent,
            "Live Delta commands require an interactive native confirmation surface; no process was started.",
        );
        return;
    }
    if world.get::<SpawnedBy>(agent).is_some() {
        notice(
            world,
            agent,
            "Start sharing/join from the main Rig agent, not a subagent.",
        );
        return;
    }
    let allowed = world.get::<ToolAccess>(agent).is_none_or(|access| {
        ["delta", "delta_sync", "native_shell"]
            .iter()
            .all(|tool| access.allows(tool))
    });
    if !allowed {
        notice(
            world,
            agent,
            "Delta sharing requires the configured Delta/native tools; this command does not widen tool access.",
        );
        return;
    }
    let Some(id) = world.get::<AgentId>(agent) else {
        notice(world, agent, "The selected agent has no stable identity.");
        return;
    };
    if let Some(binding) = world.get::<Binding>(agent)
        && binding.producer != id.0
    {
        notice(
            world,
            agent,
            "This is a copied/forked Delta binding. It cannot reuse another session's producer or outbox. Start a fresh unbound Rig session (/new) to share independently; the source producer and journal were preserved.",
        );
        return;
    }
    if world.resource::<Runtime>().closing {
        notice(
            world,
            agent,
            "Rig is shutting down; no Delta session was started.",
        );
        return;
    }
    if world.resource::<Runtime>().flows.contains_key(&agent) {
        notice(
            world,
            agent,
            "A Delta request is already open for this agent. Cancel or finish it first.",
        );
        return;
    }
    if world.resource::<Runtime>().connections.contains_key(&agent)
        && !matches!(action, Action::Admin(_) | Action::Setup)
    {
        if matches!(action, Action::Reconnect) {
            world.resource_mut::<Runtime>().reconnect.insert(agent);
            disconnect(world, agent);
        } else {
            notice(
                world,
                agent,
                "This agent already has an active or paused connection. Use /delta reconnect or disconnect before changing its sharing.",
            );
        }
        return;
    }
    if world.resource::<Runtime>().flows.len() + world.resource::<Runtime>().connections.len()
        >= MAX_CONNECTIONS
    {
        notice(world, agent, "Delta connection limit reached.");
        return;
    }
    let result =
        Client::prepare(world, agent).and_then(|client| Flow::new(world, agent, client, action));
    match result {
        Ok(flow) => {
            world.resource_mut::<Runtime>().flows.insert(agent, flow);
            world.resource::<Wake>().wake();
        }
        Err(error) => notice(world, agent, error),
    }
}

pub(crate) fn disconnect(world: &mut World, agent: Entity) {
    let mut runtime = world.remove_resource::<Runtime>().unwrap_or_default();
    if let Some(mut flow) = runtime.flows.remove(&agent) {
        flow.cancel(world);
        runtime.draining.push(flow);
    }
    if let Some(connection) = runtime.connections.get_mut(&agent) {
        connection.stop(world, "Disconnected by the local user");
    } else {
        notice(
            world,
            agent,
            "There is no running Delta connection. Saved producer/outbox state was preserved.",
        );
    }
    world.insert_resource(runtime);
    world.resource::<Wake>().wake();
}

pub(crate) fn show_status(world: &mut World, agent: Entity) {
    let mut value = state(world, agent);
    let home = RigHome::of(world).root().to_owned();
    let root = world
        .get::<WorkDir>(agent)
        .map(|dir| dir.0.clone())
        .unwrap_or_else(|| world.resource::<ProjectRoot>().0.clone());
    let configured = crate::policy::read(&root);
    let policy = crate::policy::effective(&home, &root);
    value["automatic_policy"] = json!({"policy":policy.policy,"configured":configured.configured,"trusted_local_scope":policy.trusted,"problem":policy.problem.or(configured.problem),"host_access":"fresh native confirmation is still required"});
    notice(
        world,
        agent,
        serde_json::to_string_pretty(&value).unwrap_or_else(|_| "Delta status unavailable".into()),
    );
}

pub(crate) fn disable_auto(world: &mut World, agent: Entity) {
    let home = RigHome::of(world).root().to_owned();
    let current = world
        .get::<WorkDir>(agent)
        .map(|dir| dir.0.clone())
        .unwrap_or_else(|| world.resource::<ProjectRoot>().0.clone());
    let root = current
        .ancestors()
        .find(|dir| dir.join(".git").exists())
        .unwrap_or(&current);
    match crate::policy::revoke(&home, root) {
        Ok(()) => notice(
            world,
            agent,
            "Revoked automatic Delta policy consent for this checkout. Existing connections are unchanged; /delta disconnect stops one.",
        ),
        Err(error) => notice(
            world,
            agent,
            format!("Automatic policy revocation failed: {error}"),
        ),
    }
}

pub(crate) fn switch_worktree(world: &mut World, agent: Entity, reference: &str) {
    if world.get::<ActiveTurn>(agent).is_some() {
        notice(
            world,
            agent,
            "Wait for the current turn to finish or stop it before changing worktrees from the UI. The model's delta_worktree tool reports its own in-turn transition explicitly.",
        );
        return;
    }
    match selected_root(world, agent, reference) {
        Ok(value) => notice(world, agent, format!("Selected Delta worktree: {value}")),
        Err(error) => notice(world, agent, error),
    }
}

fn selected_root(world: &mut World, agent: Entity, reference: &str) -> Result<Value, String> {
    let id = world.get::<AgentId>(agent).ok_or("No agent identity")?;
    let scope = world
        .get::<LiveWorkspace>(agent)
        .ok_or("This agent has no approved joined worktrees")?;
    let directory = scope
        .find(reference)
        .ok_or("That reference is not an approved worktree")?
        .to_owned();
    if !scope.allows(id, &directory) {
        return Err("The workspace approval is no longer valid".into());
    }
    let connection = world
        .resource::<Runtime>()
        .connections
        .get(&agent)
        .ok_or("The joined connection is not running")?;
    if !connection.ready() {
        return Err("The joined connection is paused or not ready".into());
    }
    world.entity_mut(agent).insert(WorkDir(directory.clone()));
    Ok(json!({"reference":reference,"working_directory":directory}))
}

fn state(world: &World, agent: Entity) -> Value {
    if let Some(connection) = world.resource::<Runtime>().connections.get(&agent) {
        json!({"connected": connection.ready(), "phase": connection.phase(), "thread_id": connection.binding.thread,
            "url":connection.binding.url, "visibility":connection.binding.visibility, "joined":connection.binding.joined,
            "pending":connection.journal.pending_count(), "next_sequence":connection.journal.next_sequence(), "lost_turns":connection.journal.lost_turns(),
            "execution":"native_user_confirmed_delta_host_workflow", "consent_audit_retained":!connection.grant.is_empty(), "participants":connection.view.presence(), "worktrees":connection.view.worktrees(),
            "children":connection.view.children(), "error":connection.failure})
    } else if world.resource::<Runtime>().flows.contains_key(&agent) {
        json!({"connected":false,"phase":"connecting_or_awaiting_native_consent"})
    } else if let Some(binding) = world.get::<Binding>(agent) {
        json!({"connected":false,"phase":"disconnected_or_restored","thread_id":binding.thread,"url":binding.url,
            "notice":"Saved metadata is not process/workspace permission. Use /delta reconnect."})
    } else {
        json!({"connected":false,"phase":"not_shared"})
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct StatusArgs {}

fn session_status(called: On<ToolCalled<StatusArgs>>, mut commands: Commands) {
    let (agent, call) = (called.agent, called.call);
    commands.queue(move |world: &mut World| answer(world, call, Ok(state(world, agent))));
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct WorktreeArgs {
    reference: String,
}

fn select_worktree(called: On<ToolCalled<WorktreeArgs>>, mut commands: Commands) {
    let (agent, call, reference) = (called.agent, called.call, called.args.reference.clone());
    commands.queue(move |world: &mut World| {
        let result = (|| {
            let id = world.get::<AgentId>(agent).ok_or("No agent identity")?;
            let scope = world
                .get::<LiveWorkspace>(agent)
                .ok_or("This agent has no approved joined worktrees")?;
            let directory = scope
                .find(&reference)
                .ok_or("That reference is not an approved worktree")?
                .to_owned();
            if !scope.allows(id, &directory) {
                return Err("The workspace approval is no longer valid".into());
            }
            let connection = world
                .resource::<Runtime>()
                .connections
                .get(&agent)
                .ok_or("The joined connection is not running")?;
            if !connection.ready() {
                return Err("The joined connection is paused or not ready".into());
            }
            world.entity_mut(agent).insert(WorkDir(directory.clone()));
            Ok(json!({"reference":reference,"working_directory":directory}))
        })();
        answer(world, call, result);
    });
}

fn answer(world: &mut World, call: Entity, result: Result<Value, String>) {
    let result = match result {
        Ok(value) => ToolResult::success(ToolOutput::json(value)),
        Err(error) => ToolResult::failed(ToolExecutionError::other(error)),
    };
    if let Some(mut decision) = world.get_mut::<Decision>(call) {
        decision.decide(Decider::plugin("delta-live"), result);
    }
}

fn notice(world: &mut World, agent: Entity, text: impl Into<String>) {
    world.write_message(Notice::info(agent, text));
}

/// One bounded native command. A failed exit can still carry append receipts;
/// its caller decides which prefix was durably confirmed before pausing.
struct Command {
    process: Process,
    output: String,
}
struct CommandReply {
    code: Option<i32>,
    output: String,
    diagnostics: String,
    error: Option<String>,
}
impl Command {
    fn start(
        client: &Client,
        args: Vec<String>,
        input: Option<Vec<u8>>,
        wake: Wake,
    ) -> Result<Self, String> {
        client.valid()?;
        Ok(Self {
            process: Process::start(client.request(args, input, false), wake)?,
            output: String::new(),
        })
    }
    fn poll(&mut self) -> Option<CommandReply> {
        for _ in 0..128 {
            match self.process.try_event()? {
                Event::Line(line) => {
                    if self.output.len().saturating_add(line.len() + 1) > MAX_COMMAND_OUTPUT {
                        self.process.request_stop();
                        return Some(CommandReply {
                            code: None,
                            output: std::mem::take(&mut self.output),
                            diagnostics: String::new(),
                            error: Some("Delta command output exceeded 16 MiB".into()),
                        });
                    }
                    self.output.push_str(&line);
                    self.output.push('\n');
                }
                Event::Exited {
                    code,
                    diagnostics,
                    stopped,
                } => {
                    return Some(CommandReply {
                        code,
                        output: std::mem::take(&mut self.output),
                        diagnostics,
                        error: stopped.then(|| "Delta command was stopped".into()),
                    });
                }
                Event::Failed { error, diagnostics } => {
                    return Some(CommandReply {
                        code: None,
                        output: std::mem::take(&mut self.output),
                        diagnostics,
                        error: Some(error),
                    });
                }
            }
        }
        None
    }
}

struct LocalTurn {
    entity: Entity,
    sequence: u64,
}
struct StreamPart {
    kind: PartKind,
    text: String,
}
struct StreamedReply {
    effect: Entity,
    prefix: String,
    turn: Option<u64>,
    parts: BTreeMap<usize, StreamPart>,
    before_entries: BTreeSet<String>,
    dirty_text: bool,
    dirty_thought: bool,
}

struct FailedStart {
    error: String,
    mount: Option<Process>,
}
impl std::fmt::Debug for FailedStart {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        out.debug_tuple("FailedDeltaStart")
            .field(&self.error)
            .finish()
    }
}

struct Collaboration {
    agent: Entity,
    parent: Option<Entity>,
    binding: Binding,
    client: Client,
    mount: Option<Process>,
    watch: Process,
    view: WatchView,
    ledger: ContextLedger,
    remote_children: RemoteChildren,
    children_directory_dirty: bool,
    defer_child_directory: bool,
    local_child_refs: BTreeSet<String>,
    journal: Journal,
    append: Option<Command>,
    retiring: Vec<Process>,
    defer_mount_stop: bool,
    turn: Option<LocalTurn>,
    streams: BTreeMap<Entity, StreamedReply>,
    failure: Option<String>,
    stopping: bool,
    last_append: Instant,
    last_stream_flush: Instant,
    dirty_conversation: bool,
    grant: String,
}

impl Collaboration {
    fn new(connected: Connected, world: &mut World) -> Result<Self, FailedStart> {
        let Connected {
            agent,
            binding,
            client,
            mount,
            journal,
            grant,
        } = connected;
        Self::build(
            agent,
            None,
            binding,
            client,
            Some(mount),
            journal,
            grant,
            world,
        )
    }
    fn new_child(connected: ChildConnected, world: &mut World) -> Result<Self, FailedStart> {
        let ChildConnected {
            agent,
            parent,
            binding,
            client,
            journal,
            grant,
        } = connected;
        Self::build(
            agent,
            Some(parent),
            binding,
            client,
            None,
            journal,
            grant,
            world,
        )
    }
    fn build(
        agent: Entity,
        parent: Option<Entity>,
        binding: Binding,
        client: Client,
        mount: Option<Process>,
        mut journal: Journal,
        grant: String,
        world: &mut World,
    ) -> Result<Self, FailedStart> {
        let prepared = (|| {
            client.valid()?;
            flow::protect_journal_home(world, &client.home)?;
            flow::protect_executable(world, &client.executable)?;
            if grant.is_empty() {
                return Err("No native consent audit accompanies this connection".into());
            }
            let id = world
                .get::<AgentId>(agent)
                .ok_or("The connected agent disappeared")?;
            if id.0 != binding.producer {
                return Err("Connected producer does not match the agent".into());
            }
            let open = journal.open_turns();
            if open.len() > 1 {
                return Err("Several interrupted Delta turns need explicit journal recovery; their lineage was not guessed".into());
            }
            let conversation = world
                .get::<Conversation>(agent)
                .ok_or("The agent has no conversation")?;
            // Recover retained late output under the one known interrupted
            // sequence BEFORE ending it. Never turn a failed final capture into
            // an unassociated historical response on resume.
            journal.capture(conversation, open.first().copied(), binding.options)?;
            for sequence in open {
                journal.finish_turn(sequence)?;
            }
            let view = WatchView::new(binding.thread.clone()).map_err(|error| error.to_string())?;
            let (ledger, children) = match world.get::<ContextCheckpoint>(agent) {
                Some(saved)
                    if saved.producer == binding.producer && saved.thread == binding.thread =>
                {
                    (saved.ledger.clone(), saved.children.clone())
                }
                Some(_) => {
                    return Err(
                        "Remote context checkpoint belongs to a different producer/thread".into(),
                    );
                }
                None => (ContextLedger::default(), RemoteLedgers::default()),
            };
            let remote_children = RemoteChildren::new(children)?;
            Ok((view, ledger, remote_children))
        })();
        let (view, ledger, remote_children) = match prepared {
            Ok(prepared) => prepared,
            Err(error) => return Err(FailedStart { error, mount }),
        };
        let args = vec![
            "--json".into(),
            "thread".into(),
            "watch".into(),
            binding
                .locator
                .clone()
                .unwrap_or_else(|| binding.thread.clone()),
            "--source".into(),
            "rig".into(),
            "--external-id".into(),
            binding.producer.clone(),
            "--participate".into(),
            "Rig".into(),
            "--owner-pid".into(),
            std::process::id().to_string(),
        ];
        let watch = match Process::start(
            client.request(args, None, true),
            world.resource::<Wake>().clone(),
        ) {
            Ok(watch) => watch,
            Err(error) => return Err(FailedStart { error, mount }),
        };
        world.entity_mut(agent).insert(binding.clone());
        Ok(Self {
            agent,
            parent,
            binding,
            client,
            mount,
            watch,
            view,
            ledger,
            remote_children,
            children_directory_dirty: false,
            defer_child_directory: false,
            local_child_refs: BTreeSet::new(),
            journal,
            append: None,
            retiring: Vec::new(),
            defer_mount_stop: false,
            turn: None,
            streams: BTreeMap::new(),
            failure: None,
            stopping: false,
            last_append: Instant::now() - APPEND_INTERVAL,
            last_stream_flush: Instant::now() - APPEND_INTERVAL,
            dirty_conversation: false,
            grant,
        })
    }
    fn stop_mount(&self) {
        if !self.defer_mount_stop
            && let Some(mount) = &self.mount
        {
            mount.request_stop();
        }
    }
    fn phase(&self) -> &str {
        if self.stopping {
            "disconnecting_and_flushing"
        } else if self.failure.is_some() {
            "paused"
        } else if self.view.phase() != WatchPhase::Live {
            "watch_snapshot"
        } else if self.journal.pending_count() != 0 {
            "syncing"
        } else {
            "live"
        }
    }
    fn ready(&self) -> bool {
        !self.stopping
            && self.failure.is_none()
            && self.view.phase() == WatchPhase::Live
            && self.turn.as_ref().is_none_or(|turn| {
                self.journal.confirmed_sequence() >= turn.sequence
                    && !self.journal.lost_turns().contains(&turn.sequence)
            })
    }
    fn pause(&mut self, world: &mut World, error: impl Into<String>) {
        let error = error.into();
        if self.failure.is_none() {
            notice(
                world,
                self.agent,
                format!(
                    "Delta paused: {error}. No automatic retry. Use /delta disconnect, then /delta reconnect."
                ),
            );
        }
        self.failure = Some(error.clone());
        self.watch.request_stop();
        self.remote_children.request_stop();
        self.stop_mount();
        if let Some(turn) = &self.turn {
            let _ = self.journal.mark_lost(turn.sequence);
        }
        if world.get::<Agent>(self.agent).is_some() {
            world.trigger(StopTurn {
                entity: self.agent,
                reason: Some(format!("Delta connection paused: {error}")),
            });
        }
    }
    fn stop(&mut self, world: &mut World, why: &str) {
        self.stop_with_flush(world, why, false);
    }
    fn stop_with_flush(&mut self, world: &mut World, why: &str, flush: bool) {
        if self.stopping {
            return;
        }
        self.stopping = true;
        if world.get::<Agent>(self.agent).is_some() {
            world.trigger(StopTurn {
                entity: self.agent,
                reason: Some(why.into()),
            });
        }
        if let Err(error) = self
            .flush_streams(true)
            .and_then(|()| self.reconcile_streams(world))
        {
            self.failure = Some(error);
        }
        if let Some(turn) = self.turn.take() {
            if let Some(conversation) = world.get::<Conversation>(self.agent)
                && let Err(error) =
                    self.journal
                        .capture(conversation, Some(turn.sequence), self.binding.options)
            {
                self.failure = Some(format!(
                    "Final capture for turn {} failed; its open lineage was preserved: {error}",
                    turn.sequence
                ));
            }
            if self.failure.is_none()
                && let Err(error) = self.journal.finish_turn(turn.sequence)
            {
                self.failure = Some(error);
            }
        }
        self.watch.request_stop();
        self.remote_children.request_stop();
        // Disconnect revokes new starts. Normal exit can flush immutable
        // records under its previously confirmed shutdown scope before mount
        // teardown; errors keep the records durable and are reported.
        if flush
            && self.append.is_none()
            && self.failure.is_none()
            && self.journal.pending_count() > 0
            && let Err(error) = self.start_append(world)
        {
            self.failure = Some(error);
        }
        if self.append.is_none() {
            self.stop_mount();
        }
        if let Some(error) = &self.failure {
            notice(
                world,
                self.agent,
                format!("Delta stopped with an incomplete capture: {error}"),
            );
        }
    }
    fn start_append(&mut self, world: &World) -> Result<(), String> {
        let result = (|| {
            let batch = self.journal.batch()?;
            if batch.count == 0 {
                return Ok(None);
            }
            let args = vec![
                "--json".into(),
                "thread".into(),
                "append".into(),
                self.binding
                    .locator
                    .clone()
                    .unwrap_or_else(|| self.binding.thread.clone()),
                "--source".into(),
                "rig".into(),
                "--external-id".into(),
                self.binding.producer.clone(),
                "--producer-name".into(),
                "Rig".into(),
                "--acknowledge".into(),
                "local".into(),
            ];
            Command::start(
                &self.client,
                args,
                Some(batch.input.into_bytes()),
                world.resource::<Wake>().clone(),
            )
            .map(Some)
        })();
        self.append = result?;
        self.last_append = Instant::now();
        Ok(())
    }
    fn poll(&mut self, world: &mut World) {
        if !self.stopping
            && self.failure.is_none()
            && let Err(error) = flow::protect_journal_home(world, &self.client.home)
                .and_then(|()| flow::protect_executable(world, &self.client.executable))
        {
            self.pause(world, error);
        }
        if world
            .get::<AgentId>(self.agent)
            .is_none_or(|id| id.0 != self.binding.producer)
        {
            self.stop(world, "Delta owner ended or changed");
        }
        for _ in 0..64 {
            let Some(event) = self.watch.try_event() else {
                break;
            };
            match event {
                Event::Line(line) if !self.stopping && self.failure.is_none() => {
                    let result = (|| {
                        let event =
                            crate::watch::WatchEvent::parse(&line).map_err(|e| e.to_string())?;
                        if let Some(crate::watch::WatchEvent::Worktrees { worktrees }) = &event {
                            let references: BTreeSet<_> = worktrees
                                .iter()
                                .map(|tree| tree.reference.as_str())
                                .collect();
                            if self.binding.joined
                                || self.parent.is_some()
                                    && world.get::<LiveWorkspace>(self.agent).is_some()
                            {
                                let scope = world
                                    .get::<LiveWorkspace>(self.agent)
                                    .ok_or("Joined workspace trust is absent")?;
                                let approved: BTreeSet<_> = scope
                                    .roots()
                                    .iter()
                                    .map(|(reference, _)| reference.as_str())
                                    .collect();
                                if references != approved {
                                    return Err("Thread worktree attachments changed; reconnect and confirm the new ready mount roots before coding".into());
                                }
                            } else if self
                                .binding
                                .worktree
                                .as_ref()
                                .is_none_or(|reference| !references.contains(reference.as_str()))
                            {
                                return Err(
                                    "This shared checkout is no longer attached to the thread"
                                        .into(),
                                );
                            }
                        }
                        if let Some(crate::watch::WatchEvent::Ready { producer, .. }) = &event {
                            let producer = producer
                                .as_ref()
                                .ok_or("Delta watch returned no producer")?;
                            if producer.source != "rig"
                                || producer.external_id != self.binding.producer
                            {
                                return Err("Delta watch returned a different producer".into());
                            }
                            self.journal.validate_frontier(producer.next_sequence)?;
                        }
                        if let Some(event) = event {
                            if matches!(
                                event,
                                crate::watch::WatchEvent::Children { .. }
                                    | crate::watch::WatchEvent::Child { .. }
                                    | crate::watch::WatchEvent::ChildRemove { .. }
                            ) {
                                self.children_directory_dirty = true;
                            }
                            self.view.apply(event).map_err(|e| e.to_string())
                        } else {
                            Ok(WatchEffect::None)
                        }
                    })();
                    match result {
                        Ok(WatchEffect::StopLocalTurn { sequence }) => {
                            if let Err(error) = self.journal.mark_lost(sequence) {
                                self.pause(world, error);
                                break;
                            }
                            world.trigger(StopTurn {
                                entity: self.agent,
                                reason: Some(
                                    "Another Delta participant took over this turn".into(),
                                ),
                            });
                            notice(
                                world,
                                self.agent,
                                "Delta turn taken over by another participant; local model and nested tools were stopped.",
                            );
                        }
                        Ok(WatchEffect::Pause { error }) => {
                            self.pause(world, format!("{}: {}", error.code, error.message));
                            break;
                        }
                        Ok(WatchEffect::None) => {
                            if self.view.phase() == WatchPhase::Live {
                                self.watch.mark_ready();
                            }
                        }
                        Err(error) => {
                            self.pause(world, error);
                            break;
                        }
                    }
                }
                Event::Line(_) => (),
                Event::Exited {
                    code, diagnostics, ..
                } if !self.stopping && self.failure.is_none() => {
                    self.pause(world, format!("Delta watch exited {code:?}: {diagnostics}"));
                    break;
                }
                Event::Failed { error, diagnostics }
                    if !self.stopping && self.failure.is_none() =>
                {
                    self.pause(world, format!("{error}: {diagnostics}"));
                    break;
                }
                _ => (),
            }
        }
        for _ in 0..8 {
            let Some(event) = self.mount.as_mut().and_then(Process::try_event) else {
                break;
            };
            match event {
                Event::Line(line) if !self.stopping => {
                    self.pause(
                        world,
                        format!("Unexpected mount activity after readiness: {line}"),
                    );
                    break;
                }
                Event::Line(_) => (),
                Event::Exited {
                    code, diagnostics, ..
                } if !self.stopping => {
                    self.pause(world, format!("Delta mount exited {code:?}: {diagnostics}"));
                    break;
                }
                Event::Failed { error, diagnostics } if !self.stopping => {
                    self.pause(world, format!("{error}: {diagnostics}"));
                    break;
                }
                _ => (),
            }
        }
        self.retiring.retain_mut(|process| {
            process.request_stop();
            for _ in 0..64 {
                if process.try_event().is_none() {
                    break;
                }
            }
            !process.is_finished()
        });
        if let Some(reply) = self.append.as_mut().and_then(Command::poll) {
            if let Some(command) = self.append.take()
                && !command.process.is_finished()
            {
                command.process.request_stop();
                self.retiring.push(command.process);
            }
            match self.journal.acknowledge(&reply.output) {
                Ok(ack) => {
                    for sequence in ack.fenced {
                        if self
                            .turn
                            .as_ref()
                            .is_some_and(|turn| turn.sequence == sequence)
                        {
                            world.trigger(StopTurn {
                                entity: self.agent,
                                reason: Some("Delta fenced this local turn".into()),
                            });
                        }
                    }
                    if let Some(error) = ack.error {
                        self.pause(world, error);
                    } else if reply.code != Some(0) || reply.error.is_some() {
                        self.pause(
                            world,
                            format!(
                                "Delta append failed: {:?}; {}",
                                reply.error, reply.diagnostics
                            ),
                        );
                    }
                }
                Err(error) => self.pause(world, error),
            }
            if self.stopping {
                self.stop_mount();
            }
        }
        if self.stopping || self.failure.is_some() {
            let _ = self.remote_children.drain();
            return;
        }
        if let Err(error) = self.client.valid() {
            self.pause(world, error);
            return;
        }
        if let Err(error) = self.flush_streams(false) {
            self.pause(world, error);
            return;
        }
        let active = world.get::<ActiveTurn>(self.agent).map(ActiveTurn::turn);
        if self.turn.as_ref().map(|turn| turn.entity) != active {
            if let Some(turn) = self.turn.take() {
                if let Err(error) = self.reconcile_streams(world) {
                    self.pause(world, error);
                    return;
                }
                if let Some(conversation) = world.get::<Conversation>(self.agent)
                    && let Err(error) = self.journal.capture(
                        conversation,
                        Some(turn.sequence),
                        self.binding.options,
                    )
                {
                    self.pause(world, error);
                    return;
                }
                if let Err(error) = self.journal.finish_turn(turn.sequence) {
                    self.pause(world, error);
                    return;
                }
                let _ = self.view.set_owned_turn(None);
            }
            if let Some(entity) = active {
                let local_id = world
                    .get::<Effect>(entity)
                    .map(|effect| effect.id().to_string())
                    .unwrap_or_else(|| format!("turn-{}", entity.to_bits()));
                match self.journal.start_turn(&local_id) {
                    Ok(sequence) => {
                        self.turn = Some(LocalTurn { entity, sequence });
                        self.dirty_conversation = true;
                        let _ = self.view.set_owned_turn(Some(sequence));
                    }
                    Err(error) => {
                        self.pause(world, error);
                        return;
                    }
                }
            }
        }
        if let Err(error) = self.reconcile_streams(world) {
            self.pause(world, error);
            return;
        }
        let sequence = self.turn.as_ref().map(|turn| turn.sequence);
        if self.dirty_conversation
            && let Some(conversation) = world.get::<Conversation>(self.agent)
        {
            if let Err(error) = self
                .journal
                .capture(conversation, sequence, self.binding.options)
            {
                self.pause(world, error);
                return;
            }
            self.dirty_conversation = false;
        }
        if self.view.phase() == WatchPhase::Live {
            if self.children_directory_dirty && !self.defer_child_directory {
                let children = self
                    .view
                    .children()
                    .iter()
                    .filter(|(_, child)| !self.local_child_refs.contains(&child.reference))
                    .map(|(key, child)| (key.clone(), child.clone()))
                    .collect();
                if let Err(error) = self.remote_children.sync(
                    &children,
                    &self.client,
                    world.resource::<Wake>().clone(),
                ) {
                    self.pause(world, error);
                    return;
                }
                self.children_directory_dirty = false;
            }
            match self.remote_children.poll() {
                Ok(changes) => {
                    if !changes.is_empty() {
                        world.entity_mut(self.agent).insert(ContextCheckpoint {
                            producer: self.binding.producer.clone(),
                            thread: self.binding.thread.clone(),
                            ledger: self.ledger.clone(),
                            children: self.remote_children.ledgers().clone(),
                        });
                    }
                    for change in changes {
                        let text = format!(
                            "Delta remote child {:?} ({}) context update: {:?}",
                            change.name, change.reference, change.change
                        );
                        world.spawn((
                            Incoming::new(text, DeliveryMode::Inform)
                                .with_author(Author::plugin("delta-remote")),
                            ForAgent(self.agent),
                        ));
                    }
                }
                Err(error) => {
                    self.pause(world, error);
                    return;
                }
            }
            match self.view.context_changes(&mut self.ledger) {
                Ok(changes) => {
                    if !changes.is_empty() {
                        world.entity_mut(self.agent).insert(ContextCheckpoint {
                            producer: self.binding.producer.clone(),
                            thread: self.binding.thread.clone(),
                            ledger: self.ledger.clone(),
                            children: self.remote_children.ledgers().clone(),
                        });
                    }
                    for change in changes {
                        let text = match change {
                            ContextChange::UpsertItem { item } => format!(
                                "Delta remote transcript item {} ({}; author {:?}): {}",
                                item.id, item.kind, item.author, item.content
                            ),
                            ContextChange::RemoveItem { id } => format!(
                                "Delta remote transcript item {id} was removed; do not rely on its previous content."
                            ),
                            ContextChange::UpsertComment { comment } => format!(
                                "Delta comment {} by {:?}: {:?}",
                                comment.id, comment.author, comment.text
                            ),
                            ContextChange::RemoveComment { id } => {
                                format!("Delta comment {id} was removed.")
                            }
                        };
                        world.spawn((
                            Incoming::new(text, DeliveryMode::Inform)
                                .with_author(Author::plugin("delta-remote")),
                            ForAgent(self.agent),
                        ));
                    }
                }
                Err(error) => {
                    self.pause(world, error.to_string());
                    return;
                }
            }
        }
        if self.append.is_none()
            && self.journal.pending_count() > 0
            && self.view.phase() == WatchPhase::Live
            && self.last_append.elapsed() >= APPEND_INTERVAL
            && let Err(error) = self.start_append(world)
        {
            self.pause(world, error);
        }
    }
    fn stream(&mut self, delta: StreamDelta, world: &mut World) {
        if self.failure.is_some() || self.stopping {
            return;
        }
        let prefix = world
            .get::<Effect>(delta.effect)
            .map(|effect| format!("stream:{}", effect.id()))
            .unwrap_or_else(|| format!("stream-entity:{}", delta.effect.to_bits()));
        let original_turn = self.turn.as_ref().map(|turn| turn.sequence);
        let before_entries = world
            .get::<Conversation>(self.agent)
            .map(|conversation| {
                conversation
                    .entries()
                    .map(|entry| entry.id.to_string())
                    .collect()
            })
            .unwrap_or_default();
        let reply = self
            .streams
            .entry(delta.effect)
            .or_insert_with(|| StreamedReply {
                effect: delta.effect,
                prefix,
                turn: original_turn,
                parts: BTreeMap::new(),
                before_entries,
                dirty_text: false,
                dirty_thought: false,
            });
        let index = delta.event.part().index();
        let incoming_kind = match &delta.event {
            StreamEvent::Text { .. } => Some(PartKind::Text),
            StreamEvent::Reasoning { .. } => Some(PartKind::Reasoning),
            _ => None,
        };
        match delta.event {
            StreamEvent::Start { kind, .. } => {
                if reply.parts.len() >= MAX_STREAM_EVENTS && !reply.parts.contains_key(&index) {
                    self.pause(world, "Too many live streamed parts");
                    return;
                }
                reply.parts.entry(index).or_insert(StreamPart {
                    kind,
                    text: String::new(),
                });
            }
            StreamEvent::Text { text, .. } | StreamEvent::Reasoning { text, .. } => {
                let Some(part) = reply.parts.get_mut(&index) else {
                    return;
                };
                let kind = part.kind;
                if Some(kind) != incoming_kind {
                    self.pause(world, "A live stream changed its declared part kind");
                    return;
                }
                if part.text.len().saturating_add(text.len()) > 8 * 1024 * 1024 {
                    self.pause(world, "A live streamed part exceeded 8 MiB");
                    return;
                }
                part.text.push_str(&text);
                let bytes = reply
                    .parts
                    .values()
                    .try_fold(0usize, |bytes, part| bytes.checked_add(part.text.len()));
                if bytes.is_none_or(|bytes| bytes > 8 * 1024 * 1024) {
                    self.pause(world, "A live streamed reply exceeded 8 MiB");
                    return;
                }
                if kind == PartKind::Text {
                    reply.dirty_text = true;
                }
                if kind == PartKind::Reasoning && self.binding.options.reasoning {
                    reply.dirty_thought = true;
                }
            }
            // Keep the actual fragments for StopTurn's folded partial. The
            // successful canonical response replaces them during reconciliation.
            StreamEvent::End { .. } => (),
            StreamEvent::Arguments { .. } => (),
        }
    }
    fn flush_streams(&mut self, force: bool) -> Result<(), String> {
        if !force && self.last_stream_flush.elapsed() < APPEND_INTERVAL {
            return Ok(());
        }
        for stream in self.streams.values_mut() {
            for (kind, suffix, update_kind, dirty) in [
                (
                    PartKind::Text,
                    "text",
                    "agent_message",
                    &mut stream.dirty_text,
                ),
                (
                    PartKind::Reasoning,
                    "thought",
                    "agent_thought",
                    &mut stream.dirty_thought,
                ),
            ] {
                if !*dirty {
                    continue;
                }
                let combined: String = stream
                    .parts
                    .values()
                    .filter(|part| part.kind == kind)
                    .map(|part| part.text.as_str())
                    .collect();
                self.journal.stage(json!({"sessionUpdate":update_kind,"messageId":format!("{}:{suffix}",stream.prefix),"content":[{"type":"text","text":combined}]}),stream.turn)?;
                *dirty = false;
            }
        }
        self.last_stream_flush = Instant::now();
        Ok(())
    }
    fn reconcile_streams(&mut self, world: &World) -> Result<(), String> {
        let Some(conversation) = world.get::<Conversation>(self.agent) else {
            return Ok(());
        };
        let mut finished = Vec::new();
        for (&entity, stream) in &self.streams {
            let outcome = world.get::<Outcome>(stream.effect);
            let response = outcome.and_then(Outcome::completion);
            let text: String = stream
                .parts
                .values()
                .filter(|part| part.kind == PartKind::Text)
                .map(|part| part.text.as_str())
                .collect();
            let thought: String = stream
                .parts
                .values()
                .filter(|part| part.kind == PartKind::Reasoning)
                .map(|part| part.text.as_str())
                .collect();
            let candidates: Vec<_> = conversation
                .entries()
                .filter(|entry| {
                    if stream.before_entries.contains(&entry.id.to_string()) {
                        return false;
                    }
                    let Message::Assistant(message) = entry.message else {
                        return false;
                    };
                    match response {
                        Some(response) => message.content == response.choice,
                        None => {
                            matches!(
                                message.stop.as_ref(),
                                Some(rig_harness::rig_core::message::StopReason::Aborted(_))
                            ) && message.text() == text
                                && message
                                    .content
                                    .iter()
                                    .filter_map(|part| match part {
                                        AssistantContent::Reasoning(reasoning) => {
                                            Some(reasoning.text.as_str())
                                        }
                                        _ => None,
                                    })
                                    .collect::<String>()
                                    == thought
                        }
                    }
                })
                .collect();
            if candidates.len() > 1 {
                return Err(
                    "Ambiguous completed stream entries; no final lineage was guessed".into(),
                );
            }
            if let Some(entry) = candidates.first() {
                self.journal
                    .alias_reply(&entry.id.to_string(), &stream.prefix, stream.turn)
                    .map_err(|error| format!("Stream reconciliation paused: {error}"))?;
                finished.push(entity);
            } else if world.get_entity(stream.effect).is_err()
                || outcome.is_some() && text.is_empty() && thought.is_empty()
            {
                // An uncommitted/failed response can leave a valid stopped
                // partial in Delta; it must not retain an unbounded live handle.
                finished.push(entity);
            }
        }
        for entity in finished {
            self.streams.remove(&entity);
        }
        Ok(())
    }
    fn finished(&self) -> bool {
        self.stopping
            && self.mount.as_ref().is_none_or(Process::is_finished)
            && self.watch.is_finished()
            && self.remote_children.is_finished()
            && self.append.is_none()
            && self.retiring.is_empty()
    }
}

/// One serialized, bounded host ingress owns UI transitions and process handles.
/// It never awaits on the ECS thread. World access is confined to this controller
/// because join atomically installs a new root agent + its approved scope.
fn tick(world: &mut World) {
    let mut runtime = world.remove_resource::<Runtime>().unwrap_or_default();
    if let Some(mut assignments) = world.get_resource_mut::<AssignmentCache>() {
        runtime
            .assignments
            .extend(std::mem::take(&mut assignments.0));
    }
    runtime
        .assignments
        .retain(|agent, _| world.get::<Agent>(*agent).is_some());
    for connection in runtime.connections.values_mut() {
        if !connection.stopping
            && connection.failure.is_none()
            && let Err(error) = flow::protect_executable(world, &connection.client.executable)
                .and_then(|()| flow::protect_journal_home(world, &connection.client.home))
        {
            connection.pause(world, error);
        }
    }
    for delta in std::mem::take(&mut runtime.streams) {
        if let Some(agent) = delta.agent
            && let Some(connection) = runtime.connections.get_mut(&agent)
        {
            connection.stream(delta, world);
        }
    }
    let revocations:BTreeSet<_>=world.query_filtered::<(&Incoming,&ForAgent,Option<&Decision>),(Without<Started>,Without<Outcome>)>().iter(world)
        .filter(|(incoming,_,decision)|incoming.author.kind==AuthorKind::User && decision.is_none_or(|decision|!decision.is_decided()) && incoming.command().is_some_and(|line|line.trim()=="delta disconnect"))
        .map(|(_,owner,_)|owner.0).collect();
    for agent in revocations {
        if let Some(mut flow) = runtime.flows.remove(&agent) {
            flow.cancel(world);
            runtime.draining.push(flow);
        }
        if let Some(connection) = runtime.connections.get_mut(&agent) {
            connection.stop(world, "Local Delta disconnect is queued");
        }
    }
    if world.contains_resource::<ExitReason>() && !runtime.closing {
        runtime.closing = true;
        for (_, mut child) in std::mem::take(&mut runtime.children) {
            child.cancel();
            runtime.draining_children.push(child);
        }
        for (_, mut flow) in std::mem::take(&mut runtime.flows) {
            flow.cancel(world);
            runtime.draining.push(flow);
        }
        for connection in runtime.connections.values_mut() {
            if !connection.stopping {
                connection.stop_with_flush(world, "Rig session is exiting", true);
            }
        }
    }
    if !runtime.closing {
        let roots: Vec<_> = world
            .query_filtered::<(Entity, &AgentId), (
                With<Agent>,
                Without<SpawnedBy>,
                Without<Binding>,
                Without<Restored>,
            )>()
            .iter(world)
            .map(|(agent, id)| (agent, id.0.clone()))
            .collect();
        for (agent, id) in roots {
            if !runtime.auto_seen.insert(id)
                || runtime.flows.contains_key(&agent)
                || runtime.connections.len() + runtime.flows.len() >= MAX_CONNECTIONS
            {
                continue;
            }
            let home = RigHome::of(world).root().to_owned();
            let dir = world
                .get::<WorkDir>(agent)
                .map(|dir| dir.0.clone())
                .unwrap_or_else(|| world.resource::<ProjectRoot>().0.clone());
            let root = dir
                .ancestors()
                .find(|dir| dir.join(".git").exists())
                .unwrap_or(&dir)
                .to_owned();
            let policy = crate::policy::effective(&home, &root);
            if policy.trusted
                && policy.policy.records_automatically()
                && let Some(audit) = policy.consent_audit
            {
                match Client::prepare(world, agent)
                    .and_then(|client| Flow::automatic(world, agent, client, policy.policy, audit))
                {
                    Ok(flow) => {
                        runtime.flows.insert(agent, flow);
                        notice(
                            world,
                            agent,
                            "A confirmed automatic Delta publication policy applies. Native host-access confirmation is still required before this connection can start.",
                        );
                    }
                    Err(error) => notice(
                        world,
                        agent,
                        format!("Automatic Delta activation did not start: {error}"),
                    ),
                }
            }
        }
    }
    let mut completed = Vec::new();
    for (&agent, flow) in &mut runtime.flows {
        match flow.tick(world) {
            Ok(FlowProgress::Waiting) => (),
            Ok(FlowProgress::Connected(connected)) => {
                match Collaboration::new(*connected, world) {
                    Ok(connection) => {
                        let target = connection.agent;
                        notice(
                            world,
                            target,
                            "Delta connection starting. Coding remains blocked until the watch is live and the local turn is acknowledged.",
                        );
                        runtime.connections.insert(target, connection);
                    }
                    Err(failure) => {
                        notice(
                            world,
                            agent,
                            format!("Delta connection did not activate: {}", failure.error),
                        );
                        if let Some(process) = failure.mount {
                            process.request_stop();
                            runtime.draining_processes.push(process);
                        }
                    }
                }
                completed.push(agent);
            }
            Ok(FlowProgress::Finished) => completed.push(agent),
            Err(error) => {
                notice(
                    world,
                    agent,
                    format!("Delta request paused: {error}. No automatic retry."),
                );
                flow.cancel(world);
                completed.push(agent);
            }
        }
    }
    for agent in completed {
        if let Some(mut flow) = runtime.flows.remove(&agent) {
            flow.cancel(world);
            runtime.draining.push(flow);
        }
    }
    for connection in runtime.connections.values_mut() {
        if !connection.stopping
            && connection.failure.is_none()
            && let Err(error) = flow::protect_executable(world, &connection.client.executable)
                .and_then(|()| flow::protect_journal_home(world, &connection.client.home))
        {
            connection.pause(world, error);
        }
    }
    if !runtime.closing {
        let descendants: Vec<_> = world
            .query::<(Entity, &SpawnedBy)>()
            .iter(world)
            .map(|(child, _)| child)
            .collect();
        for child in descendants {
            if runtime.connections.contains_key(&child)
                || runtime.children.contains_key(&child)
                || runtime.rejected_children.contains_key(&child)
            {
                continue;
            }
            let mut ancestor = child;
            let mut parent = None;
            for _ in 0..64 {
                let Some(spawned) = world.get::<SpawnedBy>(ancestor) else {
                    break;
                };
                ancestor = spawned.0;
                if runtime.connections.contains_key(&ancestor) {
                    parent = Some(ancestor);
                    break;
                }
            }
            let Some(parent) = parent else {
                continue;
            };
            let connected = runtime
                .connections
                .get(&parent)
                .expect("known live ancestor");
            if !connected.ready() {
                continue;
            }
            if runtime.children.len() + runtime.connections.len() >= MAX_CONNECTIONS {
                runtime
                    .rejected_children
                    .insert(child, "Delta child connection limit reached".into());
                world.trigger(StopTurn {
                    entity: child,
                    reason: Some("Delta child connection limit reached".into()),
                });
                continue;
            }
            let result = ChildFlow::new(
                world,
                child,
                parent,
                connected.client.clone(),
                connected.binding.clone(),
                connected.grant.clone(),
                runtime.assignments.get(&child).cloned(),
            );
            match result {
                Ok(flow) => {
                    runtime.children.insert(child, flow);
                }
                Err(error) => {
                    runtime.rejected_children.insert(child, error.clone());
                    notice(
                        world,
                        child,
                        format!("Delta subagent recording paused: {error}"),
                    );
                    world.trigger(StopTurn {
                        entity: child,
                        reason: Some(error),
                    });
                }
            }
        }
    }
    let cancel_children: Vec<_> = runtime
        .children
        .keys()
        .copied()
        .filter(|child| {
            let mut current = *child;
            for _ in 0..64 {
                let Some(parent) = world.get::<SpawnedBy>(current) else {
                    return true;
                };
                current = parent.0;
                if let Some(connection) = runtime.connections.get(&current) {
                    return connection.failure.is_some() || connection.stopping;
                }
            }
            true
        })
        .collect();
    for child in cancel_children {
        if let Some(mut flow) = runtime.children.remove(&child) {
            flow.cancel();
            runtime.draining_children.push(flow);
        }
    }
    let mut finished_children = Vec::new();
    for (&agent, child) in &mut runtime.children {
        match child.tick(world) {
            Ok(Some(connected)) => {
                match Collaboration::new_child(connected, world) {
                    Ok(connection) => {
                        runtime.connections.insert(agent, connection);
                    }
                    Err(failure) => {
                        runtime
                            .rejected_children
                            .insert(agent, failure.error.clone());
                        notice(world, agent, failure.error);
                        if let Some(process) = failure.mount {
                            process.request_stop();
                            runtime.draining_processes.push(process);
                        }
                        world.trigger(StopTurn::new(agent));
                    }
                }
                finished_children.push(agent);
            }
            Ok(None) => (),
            Err(error) => {
                runtime.rejected_children.insert(agent, error.clone());
                notice(world, agent, format!("Delta child paused: {error}"));
                world.trigger(StopTurn {
                    entity: agent,
                    reason: Some(error),
                });
                finished_children.push(agent);
            }
        }
    }
    for agent in finished_children {
        if let Some(mut child) = runtime.children.remove(&agent) {
            child.cancel();
            runtime.draining_children.push(child);
        }
    }
    runtime
        .draining_children
        .retain_mut(|child| !child.drained());
    runtime.draining.retain_mut(|flow| !flow.drained());
    runtime.draining_processes.retain_mut(|process| {
        process.request_stop();
        for _ in 0..64 {
            if process.try_event().is_none() {
                break;
            }
        }
        !process.is_finished()
    });
    for agent in std::mem::take(&mut runtime.stream_overflow) {
        if let Some(connection) = runtime.connections.get_mut(&agent) {
            connection.pause(world, "Live stream ingress overflowed");
        }
    }
    let children_to_stop: Vec<_> = runtime
        .connections
        .iter()
        .filter_map(|(&agent, connection)| {
            connection
                .parent
                .filter(|parent| {
                    runtime
                        .connections
                        .get(parent)
                        .is_none_or(|parent| parent.failure.is_some() || parent.stopping)
                })
                .map(|_| agent)
        })
        .collect();
    for child in children_to_stop {
        if let Some(connection) = runtime.connections.get_mut(&child) {
            connection.stop(world, "The parent Delta connection ended or paused");
        }
    }
    let borrowed: Vec<_> = runtime
        .connections
        .values()
        .filter(|connection| connection.parent.is_some())
        .map(|connection| connection.agent)
        .collect();
    for (&agent, connection) in &mut runtime.connections {
        connection.defer_mount_stop = connection.mount.is_some()
            && borrowed
                .iter()
                .any(|child| is_descendant(world, *child, agent));
        connection.defer_child_directory = runtime
            .children
            .keys()
            .any(|child| is_descendant(world, *child, agent));
        let local_refs: BTreeSet<_> = world
            .query::<(Entity, &Binding)>()
            .iter(world)
            .filter(|(child, _)| is_descendant(world, *child, agent))
            .filter_map(|(_, binding)| binding.locator.clone())
            .collect();
        if connection.local_child_refs != local_refs {
            connection.local_child_refs = local_refs;
            if !connection.view.children().is_empty() {
                connection.children_directory_dirty = true;
            }
        }
        if connection.failure.is_some() || connection.stopping {
            connection.stop_mount();
        }
        connection.poll(world);
    }
    let finished: Vec<_> = runtime
        .connections
        .iter()
        .filter(|(_, connection)| connection.finished())
        .map(|(&agent, _)| agent)
        .collect();
    for agent in finished {
        if let Some(connection) = runtime.connections.remove(&agent) {
            notice(
                world,
                agent,
                format!(
                    "Delta disconnected; {} records remain durable. Producer identity was preserved. {}",
                    connection.journal.pending_count(),
                    connection
                        .failure
                        .as_deref()
                        .unwrap_or("Joined checkout cleanup belongs to Delta.")
                ),
            );
        }
        if runtime.reconnect.remove(&agent) && !runtime.closing {
            match Client::prepare(world, agent)
                .and_then(|client| Flow::new(world, agent, client, Action::Reconnect))
            {
                Ok(flow) => {
                    runtime.flows.insert(agent, flow);
                }
                Err(error) => notice(world, agent, format!("Reconnect did not start: {error}")),
            }
        }
    }
    let release: Vec<_> = world
        .query::<(Entity, &OwnershipHold)>()
        .iter(world)
        .filter(|(_, hold)| {
            runtime
                .connections
                .get(&hold.agent)
                .is_some_and(|connection| {
                    connection.ready()
                        && world.get::<ActiveTurn>(hold.agent).is_some_and(|active| {
                            connection
                                .turn
                                .as_ref()
                                .is_some_and(|turn| turn.entity == active.turn())
                        })
                })
        })
        .map(|(entity, _)| entity)
        .collect();
    for entity in release {
        world.entity_mut(entity).despawn();
    }
    if runtime.closing
        && runtime.connections.is_empty()
        && runtime.flows.is_empty()
        && runtime.draining.is_empty()
        && runtime.draining_processes.is_empty()
        && runtime.children.is_empty()
        && runtime.draining_children.is_empty()
    {
        for entity in runtime.holds.drain(..) {
            if let Ok(entity) = world.get_entity_mut(entity) {
                entity.despawn();
            }
        }
    }
    let busy = runtime.flows.values().any(|flow| !flow.waiting_on_user())
        || !runtime.children.is_empty()
        || !runtime.draining_children.is_empty()
        || !runtime.draining.is_empty()
        || !runtime.draining_processes.is_empty()
        || runtime.connections.values().any(|connection| {
            connection.stopping
                || !connection.retiring.is_empty()
                || connection.append.is_some()
                || connection.journal.pending_count() > 0
        });
    if busy && runtime.clock.is_none() {
        runtime.clock = Some(world.spawn(KeepAwake(Duration::from_millis(100))).id());
    }
    if !busy
        && let Some(clock) = runtime.clock.take()
        && let Ok(clock) = world.get_entity_mut(clock)
    {
        clock.despawn();
    }
    world.insert_resource(runtime);
}

fn is_descendant(world: &World, mut child: Entity, parent: Entity) -> bool {
    for _ in 0..64 {
        match world.get::<SpawnedBy>(child) {
            Some(owner) if owner.0 == parent => return true,
            Some(owner) => child = owner.0,
            None => return false,
        }
    }
    false
}

fn capture_assignment(
    added: On<Add<rig_basics::subagents::SpawnedAgent>>,
    requests: Query<(
        &rig_basics::subagents::SpawnedAgent,
        &rig_basics::subagents::SpawnSubagent,
    )>,
    mut assignments: ResMut<AssignmentCache>,
) {
    if let Ok((child, request)) = requests.get(added.entity) {
        assignments.0.insert(child.0, request.brief.clone());
    }
}

fn collect_changes(
    mut streams: MessageReader<StreamDelta>,
    mut changed: MessageReader<ConversationChanged>,
    turn_calls: Query<(), With<TurnCall>>,
    mut runtime: ResMut<Runtime>,
    wake: Res<Wake>,
) {
    let mut dirty = false;
    for stream in streams.read() {
        if turn_calls.contains(stream.effect)
            && let Some(agent) = stream.agent
            && runtime.connections.contains_key(&agent)
        {
            if runtime.streams.len() == MAX_STREAM_EVENTS {
                runtime.stream_overflow.insert(agent);
            } else {
                runtime.streams.push_back(stream.clone());
            }
            dirty = true;
        }
    }
    for change in changed.read() {
        if let Some(connection) = runtime.connections.get_mut(&change.agent) {
            connection.dirty_conversation = true;
            dirty = true;
        }
    }
    if dirty {
        wake.wake();
    }
}

fn ownership_gate(
    mut calls: Query<
        (
            Entity,
            &Effect,
            &ForAgent,
            &mut Decision,
            Option<&ToolRequest>,
            Has<TurnCall>,
        ),
        (
            ReadyToStart,
            Or<(
                With<ModelRequest>,
                With<ToolRequest>,
                With<rig_workspace::WorkspaceOperation>,
            )>,
        ),
    >,
    bindings: Query<&Binding>,
    parents: Query<&SpawnedBy>,
    turns: Query<&ActiveTurn>,
    conversations: Query<&Conversation>,
    mut runtime: ResMut<Runtime>,
    mut commands: Commands,
    wake: Res<Wake>,
) {
    for (call, effect, &ForAgent(agent), mut decision, tool, is_model) in &mut calls {
        if decision.is_decided() {
            continue;
        }
        if let Some(error) = runtime.rejected_children.get(&agent) {
            decision.decide(
                Decider::plugin("delta-child"),
                Outcome::refused(error.clone()),
            );
            continue;
        }
        let mut chain = Vec::new();
        let mut cursor = agent;
        let mut seen = BTreeSet::new();
        for _ in 0..64 {
            if !seen.insert(cursor) {
                break;
            }
            if bindings.get(cursor).is_ok() {
                chain.push(cursor);
            }
            match parents.get(cursor) {
                Ok(parent) => cursor = parent.0,
                Err(_) => break,
            }
        }
        let Some(owner) = chain.first().copied() else {
            continue;
        };
        if tool.is_some_and(|request| {
            matches!(
                request.call.function.name.as_str(),
                "agent_status" | "auto_review_status" | "delta_session"
            )
        }) {
            continue;
        }
        if chain.iter().skip(1).any(|ancestor| {
            runtime.connections.get(ancestor).is_none_or(|connection| {
                connection.failure.is_some()
                    || connection.stopping
                    || connection.view.phase() != WatchPhase::Live
            })
        }) {
            decision.decide(Decider::plugin("delta-ownership"),Outcome::refused("An ancestor Delta connection is paused or disconnected; descendant work cannot bypass it."));
            continue;
        }
        // Until a child producer is connected, do not let its first call run
        // under a parent turn or a copied checkpoint. Bootstrap owns this hold.
        if owner != agent {
            commands.spawn((OwnershipHold { agent }, HoldOf(call), ChildOf(call)));
            wake.wake();
            continue;
        }
        match runtime.connections.get_mut(&owner) {
            Some(connection) if connection.failure.is_some() || connection.stopping => {
                decision.decide(Decider::plugin("delta-ownership"),Outcome::refused("Delta collaboration is paused/disconnecting; coding cannot continue as if the shared turn were owned."));
            }
            Some(connection)
                if connection.ready()
                    && turns.get(agent).ok().is_some_and(|active| {
                        connection
                            .turn
                            .as_ref()
                            .is_some_and(|turn| turn.entity == active.turn())
                    }) =>
            {
                if is_model {
                    let before_entries = conversations
                        .get(agent)
                        .map(|conversation| {
                            conversation
                                .entries()
                                .map(|entry| entry.id.to_string())
                                .collect()
                        })
                        .unwrap_or_default();
                    let turn = connection.turn.as_ref().map(|turn| turn.sequence);
                    connection
                        .streams
                        .entry(call)
                        .or_insert_with(|| StreamedReply {
                            effect: call,
                            prefix: format!("stream:{}", effect.id()),
                            turn,
                            parts: BTreeMap::new(),
                            before_entries,
                            dirty_text: false,
                            dirty_thought: false,
                        });
                }
            }
            Some(_) if turns.get(agent).is_ok() => {
                commands.spawn((OwnershipHold { agent }, HoldOf(call), ChildOf(call)));
                wake.wake();
            }
            Some(_) => {
                decision.decide(
                    Decider::plugin("delta-ownership"),
                    Outcome::refused(
                        "Coding effects in a live Delta session require an owned active turn.",
                    ),
                );
            }
            None => {
                decision.decide(Decider::plugin("delta-ownership"),Outcome::refused("This saved Delta session is not connected. Use /delta reconnect; no workspace or execution authority was restored implicitly."));
            }
        }
    }
}

fn hold_exit(
    exits: Query<Entity, (With<ExitRequest>, ReadyToStart)>,
    mut runtime: ResMut<Runtime>,
    mut commands: Commands,
    wake: Res<Wake>,
) {
    if runtime.connections.is_empty()
        && runtime.flows.is_empty()
        && runtime.draining.is_empty()
        && runtime.draining_processes.is_empty()
        && runtime.children.is_empty()
        && runtime.draining_children.is_empty()
    {
        return;
    }
    for exit in &exits {
        let hold = commands.spawn((ExitHold, HoldOf(exit), ChildOf(exit))).id();
        runtime.holds.push(hold);
        wake.wake();
    }
}

#[cfg(test)]
mod integration_tests;
#[cfg(test)]
mod service_tests;
#[cfg(test)]
mod tests;
