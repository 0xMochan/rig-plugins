//! Native, locally confirmed Delta workflows. No model turn is used to grant
//! host-process authority, publication, or trust in a joined directory.
//!
//! Only this closed state machine constructs CLI operations. A terminal error
//! stops the flow; it never retries, widens an audience, or resets a producer.

use std::collections::BTreeSet;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};

use rig_harness::prelude::*;
use serde::Serialize;
use serde_json::{Value, json};

use super::{Action, Binding, Client};
use crate::live_journal::{ExportOptions, InitialMode, Journal};
use crate::policy::{self, Mode, Policy, PreparedPolicy};
use crate::process::{Event, Process};
use crate::workspace::LiveWorkspace;

const MAX_REPLY_BYTES: usize = 16 * 1024 * 1024;
const MAX_EVENTS_PER_TICK: usize = 64;

pub(crate) struct Connected {
    pub agent: Entity,
    pub binding: Binding,
    pub client: Client,
    pub mount: Process,
    pub journal: Journal,
    pub grant: String,
}

pub(crate) enum FlowProgress {
    Waiting,
    Connected(Box<Connected>),
    Finished,
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Audience {
    Private,
    Organization,
    Invite { emails: Vec<String> },
    Unchanged,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stage {
    Access,
    Contract,
    Auth,
    LoginChoice,
    Login,
    LoginDone,
    Organizations,
    Audience,
    Organization,
    Export,
    Confirm,
    ConfirmJoin,
    Create,
    ExistingStatus,
    BeforeShareStatus,
    Worktrees,
    CreateWorktree,
    Attach,
    MountShare,
    Publish,
    Status,
    JoinStatus,
    MountJoin,
    TrustJoin,
    SetupMode,
    SetupConfirm,
    AdminConfirm,
    Admin,
    Complete,
}

#[derive(Serialize)]
struct Consent {
    effect: String,
    question: Ask,
    answer: Answer,
    decided_by: &'static str,
}

struct Question {
    entity: Entity,
    frozen: Ask,
}

struct AutoConsent {
    policy: Policy,
    audit: String,
}

/// One user request owns its questions, transient CLI command and mount until
/// the completed connection takes ownership. It is never a coding-turn child.
pub(crate) struct Flow {
    agent: Entity,
    expected_agent: AgentId,
    original_directory: PathBuf,
    user_revision: Vec<String>,
    target_agent: Entity,
    target_id: AgentId,
    client: Client,
    action: Action,
    stage: Stage,
    question: Option<Question>,
    command: Option<Process>,
    output: String,
    mount: Option<Process>,
    journal: Option<Journal>,
    consents: Vec<Consent>,
    audience: Option<Audience>,
    organizations: Vec<(String, String)>,
    join_references: BTreeSet<String>,
    organization: Option<String>,
    binding: Option<Binding>,
    workspace: Option<LiveWorkspace>,
    options: ExportOptions,
    setup_policy: Option<Policy>,
    setup_plan: Option<PreparedPolicy>,
    approved_host: bool,
    approved_publication: bool,
    ready: Option<Box<Connected>>,
    automatic: Option<AutoConsent>,
    cancelled: bool,
}

impl Flow {
    pub(crate) fn new(
        world: &mut World,
        agent: Entity,
        mut client: Client,
        action: Action,
    ) -> Result<Self, String> {
        let expected_agent = world
            .get::<AgentId>(agent)
            .cloned()
            .ok_or("No stable Rig agent identity")?;
        if world.get::<SpawnedBy>(agent).is_some() {
            return Err(
                "Start Delta sharing or joining from a root Rig session, not a subagent".into(),
            );
        }
        if !can_answer(world, agent) {
            return Err("Live Delta requires a native interactive confirmation surface; no host access or upload was started".into());
        }
        if busy(world, agent) {
            return Err("Wait for this session and its background agents to become idle before changing Delta participation".into());
        }
        let original_directory = workdir(world, agent);
        let binding = world.get::<Binding>(agent).cloned();
        if let Some(binding) = &binding {
            if binding.producer != expected_agent.0 {
                return Err("This saved Delta binding belongs to another producer, such as a fork's source. It cannot be reused or reset".into());
            }
            argument(&binding.thread)?;
            argument(&binding.owner)?;
            if let Some(locator) = &binding.locator {
                argument(locator)?;
            }
            if let Some(reference) = &binding.worktree {
                argument(reference)?;
            }
        }
        if let Action::Join(locator) = &action {
            argument(locator)?;
        }
        if let Action::Admin(args) = &action {
            validate_admin(args)?;
        }
        if matches!(action, Action::Share(_)) && binding.as_ref().is_some_and(|value| value.joined)
        {
            return Err("This session joined an existing Delta thread. Use /delta reconnect; sharing must not attach or republish a joined checkout".into());
        }
        if matches!(action, Action::Share(_)) {
            let joined_root = world
                .query::<(&Binding, Option<&LiveWorkspace>)>()
                .iter(world)
                .filter(|(binding, _)| binding.joined)
                .any(|(binding, scope)| {
                    same_directory(&original_directory, &binding.checkout)
                        || scope.is_some_and(|scope| {
                            scope.roots().iter().any(|(_, root)| {
                                original_directory
                                    .canonicalize()
                                    .ok()
                                    .is_some_and(|directory| directory.starts_with(root))
                            })
                        })
                });
            if joined_root {
                return Err("This checkout belongs to an existing joined Delta session. Join or reconnect to that thread instead of attaching it as a new conversation".into());
            }
        }
        if matches!(action, Action::Reconnect) && binding.is_none() {
            return Err("This session has no Delta binding to reconnect".into());
        }
        let target_id = if matches!(action, Action::Join(_)) {
            AgentId::default()
        } else {
            expected_agent.clone()
        };
        let options = binding
            .as_ref()
            .map(|value| value.options)
            .unwrap_or_default();
        let organization = binding
            .as_ref()
            .and_then(|value| value.organization.clone());
        if matches!(action, Action::Share(_) | Action::Setup) {
            client.cwd = checkout_root(&original_directory)?;
        } else if matches!(action, Action::Reconnect)
            && let Some(binding) = &binding
            && !binding.joined
        {
            client.cwd = binding.checkout.clone();
        }
        client.valid()?;
        protect_executable(world, &client.executable)?;
        if matches!(
            action,
            Action::Share(_) | Action::Join(_) | Action::Reconnect
        ) {
            protect_journal_home(world, &client.home)?;
        }
        if matches!(action, Action::Share(_) | Action::Reconnect)
            && !binding.as_ref().is_some_and(|binding| binding.joined)
            && client
                .home
                .canonicalize()
                .ok()
                .is_some_and(|home| home.starts_with(&client.cwd))
        {
            return Err("This checkout contains Rig's private home. Live bidirectional file sync could publish or overwrite credentials, policy and session state; use a Rig home outside this checkout before sharing files".into());
        }
        Ok(Self {
            agent,
            expected_agent,
            original_directory,
            user_revision: user_revision(world, agent),
            target_agent: agent,
            target_id,
            client,
            action,
            stage: Stage::Access,
            question: None,
            command: None,
            output: String::new(),
            mount: None,
            journal: None,
            consents: Vec::new(),
            audience: None,
            organizations: Vec::new(),
            join_references: BTreeSet::new(),
            organization,
            binding,
            workspace: None,
            options,
            setup_policy: None,
            setup_plan: None,
            approved_host: false,
            approved_publication: false,
            ready: None,
            automatic: None,
            cancelled: false,
        })
    }

    /// A trusted local setup may preconfirm the publication scope, never the
    /// live host start. The fresh native Access question remains mandatory.
    /// This constructor is not selected by user/model argument text.
    pub(crate) fn automatic(
        world: &mut World,
        agent: Entity,
        client: Client,
        policy: Policy,
        audit: String,
    ) -> Result<Self, String> {
        if !policy.records_automatically() || world.get::<Binding>(agent).is_some() {
            return Err("Automatic sharing requires a new, unbound root session and an explicit automatic policy".into());
        }
        let mut flow = Self::new(
            world,
            agent,
            client,
            Action::Share("automatic local policy".into()),
        )?;
        let value: Value = serde_json::from_str(&audit)
            .map_err(|error| format!("Invalid native setup audit: {error}"))?;
        let expected_client =
            serde_json::to_value(&flow.client).map_err(|error| error.to_string())?;
        let expected_policy = serde_json::to_value(&policy).map_err(|error| error.to_string())?;
        let consents = value["consents"]
            .as_array()
            .ok_or("Native setup audit has no consent provenance")?;
        let person = |title: &str, choice: &str| {
            consents.iter().any(|consent| {
                consent["decided_by"] == "person"
                    && consent["question"]["title"] == title
                    && consent["answer"]["choices"]
                        .as_array()
                        .is_some_and(|choices| choices.iter().any(|value| value == choice))
            })
        };
        if value["version"] != 1
            || value["authority"] != "native_person_confirmed_delta_host_workflow"
            || value["request"] != action_label(&Action::Setup)
            || value["client"] != expected_client
            || value["policy"] != expected_policy
            || !person("Allow this native Delta host workflow?", "allow")
            || !person("Confirm this local Delta setup policy?", "setup")
        {
            return Err("The native setup audit does not match this exact executable, environment, checkout and publication policy".into());
        }
        flow.organization = policy.organization.clone();
        flow.options = ExportOptions {
            reasoning: policy.reasoning,
            media: policy.media,
        };
        flow.audience = Some(match policy.mode {
            Mode::Private => Audience::Private,
            Mode::Organization => Audience::Organization,
            Mode::Manual => return Err("Manual mode cannot activate automatic sharing".into()),
        });
        flow.automatic = Some(AutoConsent { policy, audit });
        flow.validate_automatic(world)?;
        Ok(flow)
    }

    pub(crate) fn waiting_on_user(&self) -> bool {
        self.question.is_some()
    }

    pub(crate) fn cancel(&mut self, world: &mut World) {
        self.cancelled = true;
        if let Some(question) = self.question.take()
            && world.get_entity(question.entity).is_ok()
        {
            world.entity_mut(question.entity).despawn();
        }
        if let Some(command) = &self.command {
            command.request_stop();
        }
        if let Some(mount) = &self.mount {
            mount.request_stop();
        }
        if let Some(ready) = &self.ready {
            ready.mount.request_stop();
        }
    }

    /// Keep the cancelled owner alive until all process workers have reported
    /// their terminal event. This observes the mount's bounded flush before app
    /// exit; it never dispatches another workflow step or starts a process.
    pub(crate) fn drained(&mut self) -> bool {
        self.cancelled = true;
        let command = self.command.as_mut().is_none_or(stop_and_drain);
        let mount = self.mount.as_mut().is_none_or(stop_and_drain);
        let ready = self
            .ready
            .as_mut()
            .is_none_or(|ready| stop_and_drain(&mut ready.mount));
        command && mount && ready
    }

    pub(crate) fn tick(&mut self, world: &mut World) -> Result<FlowProgress, String> {
        if self.cancelled {
            return Err("The Delta request was cancelled; it cannot start another step".into());
        }
        self.validate(world)?;
        // A mount remains owned and monitored while publication/status/trust
        // completes. Readiness once observed is not proof it stayed alive.
        if let Some(mount) = &mut self.mount
            && let Some(event) = mount.try_event()
        {
            return Err(match event {
                Event::Line(_) => "Delta mount returned unexpected activity after readiness".into(),
                Event::Exited {
                    code, diagnostics, ..
                } => format!("Delta mount ended during setup ({code:?}): {diagnostics}"),
                Event::Failed { error, diagnostics } => {
                    format!("Delta mount failed during setup: {error}; {diagnostics}")
                }
            });
        }
        if let Some(question) = &self.question {
            if world.get::<Settled>(question.entity).is_none() {
                if world.get_entity(question.entity).is_err() {
                    return Err(
                        "Delta confirmation was cancelled; nothing further was started".into(),
                    );
                }
                return Ok(FlowProgress::Waiting);
            }
            let question = self.question.take().expect("known pending question");
            let unchanged = world.get::<Ask>(question.entity) == Some(&question.frozen)
                && world
                    .get::<ForAgent>(question.entity)
                    .is_some_and(|owner| owner.0 == self.agent);
            let decision = world.get::<Decision>(question.entity);
            let person =
                decision.is_some_and(|decision| matches!(decision.by(), Some(Decider::Person)));
            let original_answer = decision.and_then(Decision::get).and_then(Answer::of);
            let answer = world.get::<Outcome>(question.entity).and_then(Answer::of);
            if !unchanged || !person || answer != original_answer {
                return Err("Delta requires the local user's unchanged answer to the unchanged native question; no permission was inferred from a rewritten outcome".into());
            }
            let answer = answer.ok_or("Delta confirmation ended without a local answer")?;
            if answer.is("cancel")
                || answer.is("no")
                || answer.text.as_deref().is_some_and(|text| {
                    matches!(
                        text.trim().to_ascii_lowercase().as_str(),
                        "cancel" | "stop" | "no"
                    )
                })
            {
                self.cancel(world);
                notice(
                    world,
                    self.agent,
                    "Delta request cancelled; any completed earlier operations and saved producer state were preserved.",
                );
                return Ok(FlowProgress::Finished);
            }
            let effect = world
                .get::<Effect>(question.entity)
                .map(|effect| format!("{:?}", effect.id()))
                .unwrap_or_default();
            self.consents.push(Consent {
                effect,
                question: question.frozen,
                answer: answer.clone(),
                decided_by: "person",
            });
            self.answer(world, answer)?;
        }
        if self.command.is_some() {
            for _ in 0..MAX_EVENTS_PER_TICK {
                let event = self.command.as_mut().and_then(Process::try_event);
                let Some(event) = event else {
                    return Ok(FlowProgress::Waiting);
                };
                match event {
                    Event::Line(line)
                        if matches!(self.stage, Stage::MountShare | Stage::MountJoin) =>
                    {
                        let value: Value = serde_json::from_str(&line)
                            .map_err(|error| format!("Invalid Delta mount readiness: {error}"))?;
                        self.mounted(world, value)?;
                        let command = self.command.take().expect("mount is still owned");
                        command.mark_ready();
                        self.mount = Some(command);
                        break;
                    }
                    Event::Line(line) => {
                        if line.len() + 1 > MAX_REPLY_BYTES.saturating_sub(self.output.len()) {
                            return Err("Delta CLI reply exceeds the 16 MiB setup bound".into());
                        }
                        self.output.push_str(&line);
                        self.output.push('\n');
                    }
                    Event::Exited {
                        code,
                        diagnostics,
                        stopped,
                    } => {
                        self.command.take();
                        if stopped {
                            return Err(format!("Delta setup command was stopped: {diagnostics}"));
                        }
                        let value: Value = serde_json::from_str(&self.output).map_err(|error| {
                            format!("Delta returned invalid JSON: {error}; {diagnostics}")
                        })?;
                        self.output.clear();
                        if value.get("error").is_some() {
                            return Err(format!(
                                "Delta refused the operation: {}; {diagnostics}. No automatic retry was started",
                                value["error"]
                            ));
                        }
                        let auth_outcome = matches!(self.stage, Stage::Auth | Stage::Login)
                            && code == Some(1)
                            && value["status"].is_string();
                        if code != Some(0) && !auth_outcome {
                            return Err(format!(
                                "Delta command exited {code:?}: {value}; {diagnostics}. No automatic retry was started"
                            ));
                        }
                        self.result(world, value)?;
                        break;
                    }
                    Event::Failed { error, diagnostics } => {
                        return Err(format!(
                            "Delta setup process failed: {error}; {diagnostics}. Saved state was preserved; no automatic retry"
                        ));
                    }
                }
            }
            if self.command.is_some() {
                return Ok(FlowProgress::Waiting);
            }
        }
        self.dispatch(world)
    }

    fn validate(&self, world: &mut World) -> Result<(), String> {
        self.client.valid()?;
        protect_executable(world, &self.client.executable)?;
        if matches!(
            self.action,
            Action::Share(_) | Action::Join(_) | Action::Reconnect
        ) {
            protect_journal_home(world, &self.client.home)?;
        }
        self.validate_automatic(world)?;
        if world.get::<AgentId>(self.agent) != Some(&self.expected_agent)
            || workdir(world, self.agent) != self.original_directory
        {
            return Err("The Rig session or directory changed while the Delta request was pending; the stale request stopped".into());
        }
        if user_revision(world, self.agent) != self.user_revision {
            return Err("New local user intent superseded the pending Delta workflow, even though its input effect has already settled. No next command was started.".into());
        }
        if busy(world, self.agent) {
            return Err("A coding turn or descendant began while Delta setup was pending; the old request stopped".into());
        }
        let pending_text_answer = self.question.as_ref().is_some_and(|question| {
            question.frozen.free_text
                && !question.frozen.inline
                && world.get::<Outcome>(question.entity).is_none()
                && world.get::<Settled>(question.entity).is_none()
                && world
                    .get::<Decision>(question.entity)
                    .is_none_or(|decision| !decision.is_decided())
        });
        if pending_local_intent(world, self.agent, pending_text_answer) {
            return Err("New local input superseded the pending Delta workflow. No next command was started; explicitly request the workflow again if still wanted".into());
        }
        Ok(())
    }

    fn dispatch(&mut self, world: &mut World) -> Result<FlowProgress, String> {
        match self.stage {
            Stage::Access => {
                let body = format!(
                    "Installed executable: {}\nControl directory: {}\nFrozen environment (complete, no ambient overrides): {}\n\nRequest: {}\n\nThis is a distinct host Delta integration, not a native_shell escalation. It uses the OS user's network, Delta account/app storage and filesystem authority outside the command sandbox. Only fixed Delta CLI operations are constructed: compatibility/auth/organization inspection, the requested publication or join, bounded immutable ACP appends, and session-owned watch/mount processes with closed stdin. Watches/mounts live until disconnect or exit, with a 30-second readiness deadline and 5-second TERM/flush grace before kill/reap; one-shot operations have a 60-second deadline. Stdout lines/queues and stderr are bounded. No generic shell or remote tool execution is enabled; failures never retry or broaden permissions.\n\nThis approval permits the control workflow, not publication. Audience and ongoing transcript/file upload need a separate confirmation. Joining also needs a separate mounted-folder trust decision.",
                    self.client.executable.display(),
                    self.client.cwd.display(),
                    serde_json::to_string(&self.client.environment)
                        .map_err(|error| error.to_string())?,
                    action_label(&self.action),
                );
                let body = if let Some(automatic) = &self.automatic {
                    format!(
                        "{body}\n\nThis new session also has a protected, previously native-confirmed automatic publication policy: {}. After you approve this fresh host start, that exact prior audience/transcript/file scope will be applied without another publication question. Cancel starts nothing. A project policy or a self-asserted JSON audit alone cannot grant this host start.",
                        serde_json::to_string(&automatic.policy)
                            .map_err(|error| error.to_string())?
                    )
                } else {
                    body
                };
                self.ask(
                    world,
                    Ask::new("Allow this native Delta host workflow?")
                        .body(body)
                        .choice("cancel", "Cancel")
                        .choice("allow", "Allow this Delta workflow"),
                )?;
            }
            Stage::Contract => self.run(world, words(&["--json", "thread", "contract"]), false)?,
            Stage::Auth => self.run(world, words(&["--json", "auth", "status"]), false)?,
            Stage::LoginChoice => {
                self.ask(world, Ask::new("Sign in to Delta?").body("Delta is not signed in or needs account access. The next step starts its device/browser sign-in attempt; it does not upload this conversation. You will see the returned URL and can complete it yourself.").choice("cancel", "Not now").choice("login", "Start Delta sign-in"))?;
            }
            Stage::Login => self.run(
                world,
                words(&["--json", "auth", "login", "--no-wait"]),
                false,
            )?,
            Stage::LoginDone => return Err("Delta sign-in link was not prepared".into()),
            Stage::Organizations => self.run(world, words(&["--json", "org", "list"]), false)?,
            Stage::Audience => {
                let hint = match &self.action {
                    Action::Share(hint) if !hint.is_empty() => {
                        format!("Audience hint you supplied: {hint}\n\n")
                    }
                    _ => String::new(),
                };
                let mut question = Ask::new("Who should see this live Delta session?")
                    .body(format!("{hint}Sharing includes ongoing public messages/tool data and the checkout's non-ignored files, including uncommitted and untracked changes. The selected organization is not assumed to be the audience. To invite specific people, type their email addresses. Invitations never narrow existing broader access or remove existing invitees. Current visibility: {}. Keeping current sharing does not mean 'just me'.", self.binding.as_ref().and_then(|binding| binding.visibility.as_deref()).unwrap_or("new thread")))
                    .choice("cancel", "Cancel");
                question = if self.binding.is_some() {
                    question.choice("unchanged", "Keep the thread's current sharing")
                } else {
                    question.choice("private", "Just me — sync a new thread privately")
                };
                self.ask(
                    world,
                    question
                        .choice("organization", "Everyone in the chosen organization")
                        .free_text(),
                )?;
            }
            Stage::Organization => {
                let body = self.organization_body();
                let mut question = Ask::new("Which Delta organization owns this session?")
                    .body(body)
                    .choice("cancel", "Cancel")
                    .free_text();
                for (index, (_, name)) in self.organizations.iter().enumerate() {
                    question = question.choice(format!("org-{index}"), name);
                }
                self.ask(world, question)?;
            }
            Stage::Export => {
                self.ask(world, Ask::new("Choose the transcript export scope")
                    .body("Public messages and tool inputs/results can already contain secrets. System/plugin prompts and opaque provider payloads are never exported. Reasoning and supported binary images are excluded unless you explicitly include them here. Checkout files are a separate ongoing upload scope in the next confirmation.")
                    .choice("cancel", "Cancel").choice("public", "Public text and tools only")
                    .choice("reasoning", "Also include reasoning")
                    .choice("media", "Also include supported images")
                    .choice("all", "Include reasoning and supported images"))?;
            }
            Stage::Confirm => {
                let audience =
                    serde_json::to_string(&self.audience).map_err(|error| error.to_string())?;
                self.ask(world, Ask::new("Enable ongoing Delta sharing for this session?")
                    .body(format!("Checkout: {}\nOrganization: {}\nAudience: {audience}\nProducer: rig / {}\nReasoning: {}\nImages: {}\n\nDelta will keep recording this session's messages and tool inputs/results, including future activity and its subagents, and synchronize this checkout's non-ignored files, including staged, unstaged and untracked changes. This may contain secrets. It is not merely a transcript snapshot. Delta owns upload/storage and ignores; its local receipt is not proof of server delivery. Organization sharing exposes attached files to every member; invitations do not revoke existing access.\n\nCancel leaves existing thread state intact. Continue explicitly confirms this audience and ongoing transcript/file scope; it does not authorize unrelated commands.",
                        self.client.cwd.display(), self.organization.as_deref().unwrap_or("not selected"),
                        self.target_id.0, self.options.reasoning, self.options.media))
                    .choice("cancel", "Cancel").choice("share", "Confirm ongoing sharing and file sync"))?;
            }
            Stage::ConfirmJoin => {
                let binding = self.binding.as_ref().ok_or("Missing join scope")?;
                self.ask(world, Ask::new("Confirm live participation in this shared Delta thread?")
                    .body(format!("Thread: {}\nLink: {}\nExisting visibility: {} (existing invitations also remain)\nWorktree references: {}\n\nThis mounts the shared files and connects a fresh Rig session, or reconnects the existing joined producer. It does not upload the old session's transcript or attach its checkout. Future local messages, tool inputs/results, subagents and non-ignored file changes in the joined session will synchronize to the thread's existing audience. Reasoning: {}; images: {}. Shared content can contain secrets and remote context cannot grant local execution permission. The mounted directories need a separate trust decision before coding.\n\nThis confirms the ongoing joined-session transcript/file scope, not any broader publication or access change.",
                        binding.thread, binding.url.as_deref().unwrap_or("not returned"), binding.visibility.as_deref().unwrap_or("unknown"),
                        self.join_references.iter().cloned().collect::<Vec<_>>().join(", "), self.options.reasoning, self.options.media))
                    .choice("cancel", "Cancel join").choice("join", "Confirm ongoing participation and file sync"))?;
            }
            Stage::Create => {
                self.persist_audit()?;
                let organization = self
                    .organization
                    .as_deref()
                    .ok_or("Choose an organization before creating a private upload")?;
                self.run(
                    world,
                    vec![
                        "--json".into(),
                        "thread".into(),
                        "create".into(),
                        "--source".into(),
                        "rig".into(),
                        "--external-id".into(),
                        self.target_id.0.clone(),
                        "--organization".into(),
                        organization.into(),
                        "--visibility".into(),
                        "private".into(),
                    ],
                    false,
                )?;
            }
            Stage::ExistingStatus | Stage::BeforeShareStatus => {
                let binding = self.binding.as_ref().ok_or("Missing producer identity")?;
                self.run(
                    world,
                    status_args(&binding.thread, &self.target_id.0),
                    false,
                )?;
            }
            Stage::Worktrees => self.run(world, words(&["--json", "worktree", "list"]), false)?,
            Stage::CreateWorktree => self.run(
                world,
                vec![
                    "--json".into(),
                    "worktree".into(),
                    "create".into(),
                    path_text(&self.client.cwd)?,
                ],
                false,
            )?,
            Stage::Attach => {
                let binding = self
                    .binding
                    .as_ref()
                    .ok_or("Missing persisted Delta binding")?;
                self.run(
                    world,
                    vec![
                        "--json".into(),
                        "worktree".into(),
                        "attach".into(),
                        binding
                            .worktree
                            .clone()
                            .ok_or("Missing saved worktree reference")?,
                        "--thread".into(),
                        binding.thread.clone(),
                    ],
                    false,
                )?;
            }
            Stage::MountShare => {
                let binding = self
                    .binding
                    .as_ref()
                    .ok_or("Missing persisted Delta binding")?;
                self.run(
                    world,
                    vec![
                        "--json".into(),
                        "worktree".into(),
                        "mount".into(),
                        binding
                            .worktree
                            .clone()
                            .ok_or("Missing worktree reference")?,
                        path_text(&binding.checkout)?,
                        "--existing-checkout".into(),
                    ],
                    true,
                )?;
            }
            Stage::Publish => {
                let binding = self.binding.as_ref().ok_or("Missing Delta binding")?;
                match &self.audience {
                    Some(Audience::Organization) => self.run(
                        world,
                        vec![
                            "--json".into(),
                            "thread".into(),
                            "share".into(),
                            binding.thread.clone(),
                            "--visibility".into(),
                            "organization".into(),
                        ],
                        false,
                    )?,
                    Some(Audience::Invite { emails }) => {
                        let mut args = vec![
                            "--json".into(),
                            "thread".into(),
                            "invite".into(),
                            binding.thread.clone(),
                        ];
                        args.extend(emails.clone());
                        self.run(world, args, false)?;
                    }
                    Some(Audience::Private | Audience::Unchanged) => {
                        self.stage = Stage::Status;
                        return self.dispatch(world);
                    }
                    None => return Err("No confirmed publication audience".into()),
                }
            }
            Stage::Status => {
                let binding = self.binding.as_ref().ok_or("Missing Delta binding")?;
                self.run(
                    world,
                    status_args(&binding.thread, &self.target_id.0),
                    false,
                )?;
            }
            Stage::JoinStatus => {
                let locator = match &self.action {
                    Action::Join(locator) => locator.clone(),
                    Action::Reconnect => self
                        .binding
                        .as_ref()
                        .and_then(|binding| binding.locator.clone())
                        .or_else(|| self.binding.as_ref().map(|binding| binding.thread.clone()))
                        .ok_or("Missing joined-thread locator")?,
                    _ => return Err("Unexpected join request".into()),
                };
                self.run(world, status_args(&locator, &self.target_id.0), false)?;
            }
            Stage::MountJoin => {
                let binding = self.binding.as_ref().ok_or("Missing joined binding")?;
                self.persist_audit()?;
                self.run(
                    world,
                    vec![
                        "--json".into(),
                        "thread".into(),
                        "mount".into(),
                        binding
                            .locator
                            .clone()
                            .unwrap_or_else(|| binding.thread.clone()),
                        "--owner".into(),
                        binding.owner.clone(),
                    ],
                    true,
                )?;
            }
            Stage::TrustJoin => {
                let scope = self
                    .workspace
                    .as_ref()
                    .ok_or("Missing mounted workspace trust plan")?;
                let roots = scope
                    .roots()
                    .iter()
                    .map(|(reference, directory)| format!("{reference}\n  {}", directory.display()))
                    .collect::<Vec<_>>()
                    .join("\n");
                self.ask(world, Ask::new("Trust these Delta-mounted worktrees for a fresh Rig session?")
                    .body(format!("Primary directory: {}\n\nExact approved roots (no common-parent grant):\n{roots}\n\nProject instructions to apply:\n{}\n\nRemote conversation will enter as non-user context and cannot authorize local commands. No old conversation is exported or copied into a fresh join. Coding tools remain confined to the active worktree; switching is limited to these approved roots. Existing sessions remain resumable. Declining stops this mount workflow. A reconnect preserves its existing producer/session instead of creating another.", scope.primary().display(), scope.instructions()))
                    .choice("cancel", "Do not trust / cancel join").choice("trust", "Trust these roots and join"))?;
            }
            Stage::SetupMode => {
                self.ask(world, Ask::new("How should new sessions in this checkout sync?")
                    .body("This is a local opt-in, not permission inferred from a repository file. Manual leaves new sessions disconnected. Private/organization mode can automatically record future root sessions in this exact checkout under a separate local native-consent record; the next confirmation discloses ongoing data and host access. Editing or importing a project file cannot create that authority.")
                    .choice("cancel", "Cancel").choice("manual", "Manual only").choice("private", "Private automatic sync").choice("organization", "Organization automatic sharing"))?;
            }
            Stage::SetupConfirm => {
                let policy = self.setup_policy.as_ref().ok_or("Missing setup policy")?;
                if policy.records_automatically() {
                    protect_journal_home(world, &self.client.home)?;
                    if self
                        .client
                        .home
                        .canonicalize()
                        .ok()
                        .is_some_and(|home| home.starts_with(&self.client.cwd))
                    {
                        return Err("Automatic file sync cannot be enabled while this checkout contains Rig's private home; move the home outside the shared checkout first".into());
                    }
                }
                let plan = policy::prepare(&self.client.cwd, policy)?;
                let body = format!(
                    "Checkout: {}\nExact new policy:\n{}\nReasoning: {}\nImages: {}\n\nFor automatic mode this explicitly opts future root sessions in this checkout into the named organization's ongoing transcript, subagent and non-ignored checkout-file synchronization and the fixed native Delta host workflow previously shown (including app storage/network, watch and mounts until disconnect/exit). It grants no generic shell access or automatic command retry. A separate local grant binds this exact document and directory identity; changed policy disables automation. Manual mode does not authorize automatic uploading.\n\nThe project policy file is .delta/settings.json; keep it out of Git. Unknown document keys are preserved.",
                    plan.root().display(),
                    serde_json::to_string_pretty(plan.document())
                        .map_err(|error| error.to_string())?,
                    policy.reasoning,
                    policy.media
                );
                self.setup_plan = Some(plan);
                self.ask(
                    world,
                    Ask::new("Confirm this local Delta setup policy?")
                        .body(body)
                        .choice("cancel", "Cancel")
                        .choice("setup", "Save this exact local policy and consent"),
                )?;
            }
            Stage::AdminConfirm => {
                let Action::Admin(args) = &self.action else {
                    return Err("Unexpected administrative action".into());
                };
                validate_admin(args)?;
                self.ask(world, Ask::new("Run this explicit Delta account/administrative operation?")
                    .body(format!("Executable: {}\nExact argument data: {}\n\nThis is a bounded one-shot host operation, not an ongoing collaboration grant. Organization invitations can expose every organization-shared thread, unsharing retains explicit invitations, and updates can restart Delta/disconnect active clients. Device login may return a browser URL. No other administrative action will run automatically.", self.client.executable.display(), serde_json::to_string(args).map_err(|error| error.to_string())?))
                    .choice("cancel", "Cancel").choice("run", "Run this exact operation"))?;
            }
            Stage::Admin => {
                self.persist_audit()?;
                let Action::Admin(args) = &self.action else {
                    return Err("Unexpected administrative action".into());
                };
                validate_admin(args)?;
                self.run(world, args.clone(), false)?;
            }
            Stage::Complete => return self.complete(world),
        }
        Ok(FlowProgress::Waiting)
    }

    fn ask(&mut self, world: &mut World, question: Ask) -> Result<(), String> {
        if !can_answer(world, self.agent) {
            return Err("No native confirmation surface is available; Delta request paused without approval".into());
        }
        let entity = world.spawn((question.clone(), ForAgent(self.agent))).id();
        self.question = Some(Question {
            entity,
            frozen: question,
        });
        Ok(())
    }

    fn run(&mut self, world: &mut World, args: Vec<String>, session: bool) -> Result<(), String> {
        if !self.approved_host {
            return Err("Delta host access has not been approved by the local user".into());
        }
        if matches!(
            self.stage,
            Stage::Create
                | Stage::CreateWorktree
                | Stage::Attach
                | Stage::MountShare
                | Stage::MountJoin
                | Stage::Publish
        ) && !self.approved_publication
        {
            return Err(
                "The ongoing transcript/file publication scope has not been confirmed".into(),
            );
        }
        // Recheck immediately before the spawn, including queued local stop or
        // new intent that has not reached AgentLoop yet.
        self.validate(world)?;
        self.output.clear();
        self.command = Some(Process::start(
            self.client.request(args, None, session),
            world.resource::<Wake>().clone(),
        )?);
        Ok(())
    }

    fn answer(&mut self, world: &mut World, answer: Answer) -> Result<(), String> {
        match self.stage {
            Stage::Access if answer.is("allow") => {
                self.approved_host = true;
                self.persist_audit()?;
                self.stage = Stage::Contract;
            }
            Stage::LoginChoice if answer.is("login") => { self.stage = Stage::Login; }
            Stage::LoginDone if answer.is("continue") => { self.stage = Stage::Auth; }
            Stage::Audience => {
                self.audience = if answer.is("private") && self.binding.is_none() { Some(Audience::Private) }
                    else if answer.is("unchanged") && self.binding.is_some() { Some(Audience::Unchanged) }
                    else if answer.is("organization") { Some(Audience::Organization) }
                    else if let Some(text) = &answer.text { Some(Audience::Invite { emails: emails(text)? }) }
                    else { return Err("Choose private/organization access or enter specific invitation email addresses".into()); };
                self.stage = Stage::Organization;
            }
            Stage::Organization => {
                let organization = answer.choices.iter().find_map(|choice| choice.strip_prefix("org-").and_then(|index| index.parse::<usize>().ok()).and_then(|index| self.organizations.get(index)).map(|(id, _)| id.clone()))
                    .or_else(|| answer.text.as_deref().map(str::trim).filter(|text| !text.is_empty()).map(str::to_owned))
                    .ok_or("Choose or enter an organization ID")?;
                argument(&organization)?;
                if organization.chars().any(char::is_whitespace) { return Err("Enter an organization ID, not a team name".into()); }
                self.organization = Some(organization.clone());
                if let Some(policy) = &mut self.setup_policy { policy.organization = Some(organization); }
                self.stage = if matches!(self.action, Action::Setup) { Stage::Export }
                    else if self.binding.is_some() { Stage::Confirm } else { Stage::Export };
            }
            Stage::Export => {
                self.options = if answer.is("public") { ExportOptions::default() }
                    else if answer.is("reasoning") { ExportOptions { reasoning: true, media: false } }
                    else if answer.is("media") { ExportOptions { reasoning: false, media: true } }
                    else if answer.is("all") { ExportOptions { reasoning: true, media: true } }
                    else { return Err("Choose an explicit transcript export scope".into()); };
                if let Some(policy) = &mut self.setup_policy {
                    policy.reasoning = self.options.reasoning;
                    policy.media = self.options.media;
                }
                self.stage = if matches!(self.action, Action::Setup) { Stage::SetupConfirm }
                    else if self.joining() { Stage::ConfirmJoin } else { Stage::Confirm };
            }
            Stage::Confirm if answer.is("share") => {
                self.approved_publication = true;
                self.stage = Stage::Create;
            }
            Stage::ConfirmJoin if answer.is("join") => {
                self.approved_publication = true;
                if let Some(binding) = &mut self.binding { binding.options = self.options; }
                self.persist_audit()?;
                self.stage = Stage::MountJoin;
            }
            Stage::TrustJoin if answer.is("trust") => {
                self.install_join(world)?;
                self.stage = Stage::Status;
            }
            Stage::SetupMode => {
                let mode = if answer.is("manual") { Mode::Manual } else if answer.is("private") { Mode::Private }
                    else if answer.is("organization") { Mode::Organization } else { return Err("Choose a setup mode".into()); };
                self.setup_policy = Some(Policy { mode, organization: None, reasoning: false, media: false });
                self.stage = if mode == Mode::Manual { Stage::SetupConfirm } else { Stage::Organization };
            }
            Stage::SetupConfirm if answer.is("setup") => {
                let plan = self.setup_plan.as_ref().ok_or("Missing immutable setup plan")?;
                if plan.root() != self.client.cwd || self.setup_policy.as_ref() != Some(plan.policy()) {
                    return Err("The setup policy changed after its native confirmation plan was prepared".into());
                }
                let grant = self.grant()?;
                policy::save_confirmed(&self.client.home, plan, &grant)?;
                notice(world, self.agent, "Saved the exact local Delta policy and native consent record. Keep .delta/settings.json excluded from Git; a repository policy alone cannot enable uploads.");
                self.stage = Stage::Complete;
            }
            Stage::AdminConfirm if answer.is("run") => { self.stage = Stage::Admin; }
            _ => return Err("The native answer did not approve the pending Delta step; no further action started".into()),
        }
        Ok(())
    }

    fn result(&mut self, world: &mut World, value: Value) -> Result<(), String> {
        match self.stage {
            Stage::Contract => {
                crate::cli::check_contract(&value)?;
                self.stage = if matches!(self.action, Action::Admin(_)) {
                    Stage::AdminConfirm
                } else {
                    Stage::Auth
                };
            }
            Stage::Auth => {
                match value["status"].as_str() {
                    Some("signed_in") => {
                        self.stage = match &self.action {
                            Action::Join(_) => Stage::JoinStatus,
                            Action::Reconnect
                                if self.binding.as_ref().is_some_and(|binding| binding.joined) =>
                            {
                                Stage::JoinStatus
                            }
                            Action::Reconnect => {
                                self.audience = Some(Audience::Unchanged);
                                // Reconnection still asks for ongoing file/transcript scope;
                                // it never republishes a private thread to repair an error.
                                self.stage = Stage::Confirm;
                                return Ok(());
                            }
                            _ if self.automatic.is_some() => {
                                self.approved_publication = true;
                                Stage::Create
                            }
                            _ => Stage::Organizations,
                        };
                    }
                    Some("signed_out" | "signing_in") => self.stage = Stage::LoginChoice,
                    Some("access_denied") => {
                        return Err(format!(
                            "Delta account access is required: {value}. Resolve it explicitly; no sharing was started"
                        ));
                    }
                    _ => return Err(format!("Unsupported Delta authentication status: {value}")),
                }
            }
            Stage::Login => {
                if value["status"] == "signed_in" {
                    self.stage = Stage::Auth;
                } else if value["status"] == "pending" {
                    let url = value["url"]
                        .as_str()
                        .ok_or("Delta sign-in returned no URL")?;
                    safe_url(url)?;
                    self.stage = Stage::LoginDone;
                    self.ask(world, Ask::new("Complete Delta sign-in in your browser")
                        .body(format!("Delta returned this device sign-in URL:\n{url}\n\nExpiry: {}\nNo browser was opened automatically. Complete sign-in yourself, then choose Continue to perform a fresh status check. Cancelling stops this waiter, not another client's shared sign-in attempt.", value["expires_at"]))
                        .choice("cancel", "Stop waiting").choice("continue", "I completed sign-in — check status"))?;
                } else {
                    return Err(format!("Delta sign-in did not complete: {value}"));
                }
            }
            Stage::Organizations => {
                self.organizations = organizations(&value)?;
                self.stage = if matches!(self.action, Action::Setup) {
                    Stage::SetupMode
                } else {
                    Stage::Audience
                };
            }
            Stage::Create => {
                let thread = string_field(&value, "thread_id")?;
                argument(&thread)?;
                if let Some(binding) = &self.binding
                    && binding.thread != thread
                {
                    return Err("Delta returned a different thread for this producer; old binding preserved".into());
                }
                let previous = self.binding.take();
                let inspect_existing = previous.is_none() && match value["created"].as_bool() {
                    Some(created) => !created,
                    None => return Err("Delta creation receipt does not say whether this producer was newly created; existing access must be inspected before uploads".into()),
                };
                self.binding = Some(Binding {
                    producer: self.target_id.0.clone(),
                    thread,
                    locator: None,
                    joined: false,
                    owner: self.target_id.0.clone(),
                    checkout: self.client.cwd.clone(),
                    worktree: previous
                        .as_ref()
                        .and_then(|binding| binding.worktree.clone()),
                    url: previous.as_ref().and_then(|binding| binding.url.clone()),
                    visibility: previous
                        .as_ref()
                        .and_then(|binding| binding.visibility.clone()),
                    organization: self.organization.clone(),
                    options: self.options,
                });
                self.save_binding(world)?;
                self.stage = if inspect_existing {
                    Stage::ExistingStatus
                } else {
                    Stage::BeforeShareStatus
                };
            }
            Stage::ExistingStatus => {
                let thread = status_thread(&value, &self.target_id.0)?;
                let binding = self.binding.as_mut().ok_or("Missing reused binding")?;
                if thread != binding.thread {
                    return Err("Reused producer resolved a different thread".into());
                }
                let next = value["producer"]["next_sequence"]
                    .as_u64()
                    .ok_or("Missing producer frontier")?;
                let directory = producer_directory(&self.client.home, &self.target_id.0)?;
                if next != 1
                    && !directory.join("live.json").is_file()
                    && !directory.join("outbox.json").is_file()
                {
                    return Err("This producer has remote records but no matching local journal. Restore the matching store; no files were mounted and no sequence was reset".into());
                }
                binding.visibility = value["visibility"].as_str().map(str::to_owned);
                binding.url = value["url"].as_str().map(str::to_owned);
                if let Some(organization) = value["upload"]["organization_id"].as_str() {
                    binding.organization = Some(organization.to_owned());
                    self.organization = Some(organization.to_owned());
                }
                self.save_binding(world)?;
                self.approved_publication = false;
                self.automatic = None; // Existing access must be chosen afresh, not inferred from an automatic new-session policy.
                self.stage = Stage::Audience;
                notice(
                    world,
                    self.agent,
                    "Delta reused an existing producer. Its current audience must be explicitly confirmed before any checkout or new transcript is uploaded.",
                );
            }
            Stage::BeforeShareStatus => {
                let thread = status_thread(&value, &self.target_id.0)?;
                let binding = self.binding.as_ref().ok_or("Missing share binding")?;
                if thread != binding.thread {
                    return Err("Delta changed thread identity before file sharing".into());
                }
                let next = value["producer"]["next_sequence"]
                    .as_u64()
                    .ok_or("Missing producer sequence frontier")?;
                let journal = Journal::open(
                    &self.client.home,
                    &binding.producer,
                    &binding.thread,
                    next,
                    InitialMode::ShareSnapshot,
                )?;
                if journal
                    .options()
                    .is_some_and(|options| options != self.options)
                {
                    return Err("The confirmed export scope differs from the saved live journal; restore the matching scope rather than silently changing it".into());
                }
                self.journal = Some(journal);
                self.stage = if binding.worktree.is_some() {
                    Stage::Attach
                } else {
                    Stage::Worktrees
                };
            }
            Stage::Worktrees => {
                let trees = value["worktrees"]
                    .as_array()
                    .ok_or("Delta worktree inventory has no worktrees array")?;
                let mut matches = Vec::new();
                for tree in trees {
                    let reference = tree["reference"]
                        .as_str()
                        .ok_or("Delta worktree inventory has no reference")?;
                    let directories = tree["directories"]
                        .as_array()
                        .ok_or("Delta worktree inventory has no directories array")?;
                    if directories.iter().any(|entry| {
                        entry["directory"].as_str().is_some_and(|directory| {
                            same_directory(Path::new(directory), &self.client.cwd)
                        })
                    }) {
                        argument(reference)?;
                        matches.push(reference.to_owned());
                    }
                }
                if matches.len() > 1 {
                    return Err("Multiple Delta worktrees match this checkout; choose the intended binding rather than creating another".into());
                }
                if let Some(reference) = matches.pop() {
                    self.binding.as_mut().ok_or("Missing binding")?.worktree = Some(reference);
                    self.save_binding(world)?;
                    self.stage = Stage::Attach;
                } else {
                    self.stage = Stage::CreateWorktree;
                }
            }
            Stage::CreateWorktree => {
                let reference = string_field(&value, "reference")?;
                argument(&reference)?;
                self.binding.as_mut().ok_or("Missing binding")?.worktree = Some(reference);
                self.save_binding(world)?; // Persist before a fallible attach.
                self.stage = Stage::Attach;
            }
            Stage::Attach => {
                self.stage = Stage::MountShare;
            }
            Stage::Publish => {
                let binding = self.binding.as_mut().ok_or("Missing binding")?;
                if value["thread_id"]
                    .as_str()
                    .is_some_and(|id| id != binding.thread)
                {
                    return Err("Delta publication replied for a different thread".into());
                }
                let url = string_field(&value, "url")?;
                safe_url(&url)?;
                binding.url = Some(url.clone());
                if let Some(Audience::Invite { emails }) = &self.audience {
                    let results = value["results"]
                        .as_array()
                        .ok_or("Delta invitations returned no recipient results")?;
                    let mut returned = BTreeSet::new();
                    for result in results {
                        let email = string_field(result, "email")?;
                        if !emails.contains(&email)
                            || !returned.insert(email.clone())
                            || !result["invited"].is_boolean()
                        {
                            return Err(
                                "Delta invitation results do not match the requested recipients"
                                    .into(),
                            );
                        }
                        notice(
                            world,
                            self.agent,
                            &format!("Delta invitation {email}: {result}"),
                        );
                    }
                    if returned.len() != emails.len() {
                        return Err("Delta did not report every invitation result; do not assume the missing recipients were invited".into());
                    }
                }
                self.save_binding(world)?;
                self.stage = Stage::Status;
            }
            Stage::JoinStatus => {
                let thread = status_thread(&value, &self.target_id.0)?;
                if value["access"]["state"] != "granted" {
                    return Err(
                        "Join requires authenticated access to an existing shared thread".into(),
                    );
                }
                let worktrees = value["worktrees"]["items"]
                    .as_array()
                    .ok_or("Delta cannot enumerate this thread's worktrees")?;
                if worktrees.is_empty() || worktrees.len() > 32 {
                    return Err("The shared thread must have 1 to 32 worktrees to join".into());
                }
                self.join_references.clear();
                for worktree in worktrees {
                    let reference = string_field(worktree, "reference")?;
                    argument(&reference)?;
                    if !self.join_references.insert(reference) {
                        return Err("Delta status returned duplicate worktree references".into());
                    }
                }
                let locator = match &self.action {
                    Action::Join(locator) => Some(locator.clone()),
                    _ => self
                        .binding
                        .as_ref()
                        .and_then(|binding| binding.locator.clone()),
                };
                if let Some(binding) = &self.binding
                    && matches!(self.action, Action::Reconnect)
                    && binding.thread != thread
                {
                    return Err("Reconnect resolved a different canonical thread; the original binding was preserved".into());
                }
                let previous = self.binding.take();
                self.binding = Some(Binding {
                    producer: self.target_id.0.clone(),
                    thread,
                    locator,
                    joined: true,
                    owner: previous
                        .as_ref()
                        .filter(|_| matches!(self.action, Action::Reconnect))
                        .map(|binding| binding.owner.clone())
                        .unwrap_or_else(|| self.target_id.0.clone()),
                    checkout: previous
                        .as_ref()
                        .map(|binding| binding.checkout.clone())
                        .unwrap_or_else(|| self.client.cwd.clone()),
                    worktree: None,
                    url: value["url"].as_str().map(str::to_owned),
                    visibility: value["visibility"].as_str().map(str::to_owned),
                    organization: previous
                        .as_ref()
                        .and_then(|binding| binding.organization.clone()),
                    options: self.options,
                });
                let binding = self.binding.as_ref().ok_or("Missing joined binding")?;
                let next = value["producer"]["next_sequence"]
                    .as_u64()
                    .ok_or("Missing joined producer frontier")?;
                let journal = Journal::open(
                    &self.client.home,
                    &binding.producer,
                    &binding.thread,
                    next,
                    InitialMode::Joined,
                )?;
                if journal
                    .options()
                    .is_some_and(|options| options != self.options)
                {
                    return Err(
                        "The joined journal's export scope does not match this session binding"
                            .into(),
                    );
                }
                self.journal = Some(journal);
                self.stage = if matches!(self.action, Action::Join(_)) {
                    Stage::Export
                } else {
                    Stage::ConfirmJoin
                };
            }
            Stage::Status => {
                let thread = status_thread(&value, &self.target_id.0)?;
                let binding = self.binding.as_mut().ok_or("Missing binding")?;
                if thread != binding.thread {
                    return Err("Delta status changed canonical thread identity".into());
                }
                if value["visibility"].is_null() && value["upload"].is_null() {
                    return Err("Delta has no confirmed upload intent; no implicit publication repair was attempted".into());
                }
                binding.visibility = value["visibility"].as_str().map(str::to_owned);
                if let Some(url) = value["url"].as_str() {
                    safe_url(url)?;
                    binding.url = Some(url.to_owned());
                }
                self.save_binding(world)?;
                let remote_next = value["producer"]["next_sequence"]
                    .as_u64()
                    .ok_or("Delta producer has no sequence frontier")?;
                // Open only after mount/access are checked; this does not erase
                // older records or replace a producer on missing receipts.
                let binding = self.binding.as_ref().ok_or("Missing binding")?;
                let journal = self
                    .journal
                    .take()
                    .ok_or("The validated durable journal was lost before connection")?;
                journal.validate_frontier(remote_next)?;
                let mount = self
                    .mount
                    .take()
                    .ok_or("Delta mount ownership was lost before connection")?;
                let mut client = self.client.clone();
                if binding.joined {
                    client.cwd = binding.checkout.clone();
                }
                client.valid()?;
                let grant = self.grant()?;
                let connected = Connected {
                    agent: self.target_agent,
                    binding: binding.clone(),
                    client,
                    mount,
                    journal,
                    grant,
                };
                // The pending return is kept in a dedicated holder so result()
                // remains one-shot while dispatch owns the public transition.
                self.ready = Some(Box::new(connected));
                self.stage = Stage::Complete;
            }
            Stage::Admin => {
                if let Action::Admin(args) = &self.action
                    && args.get(1).map(String::as_str) == Some("org")
                    && args.get(2).map(String::as_str) == Some("invite")
                {
                    let results = value["results"]
                        .as_array()
                        .ok_or("Delta organization invitation returned no per-address results")?;
                    let expected: BTreeSet<_> = args.iter().skip(3).cloned().collect();
                    let mut reported = BTreeSet::new();
                    for result in results {
                        let email = string_field(result, "email")?;
                        if !expected.contains(&email)
                            || !reported.insert(email.clone())
                            || !result["invited"].is_boolean()
                        {
                            return Err("Delta organization invitation results did not match the requested addresses".into());
                        }
                        notice(
                            world,
                            self.agent,
                            &format!("Delta organization invitation {email}: {result}"),
                        );
                    }
                    if reported.len() != expected.len() {
                        return Err(
                            "Delta did not report every organization invitation result".into()
                        );
                    }
                }
                notice(world, self.agent, &format!("Delta result: {value}"));
                self.stage = Stage::Complete;
            }
            _ => return Err(format!("Unexpected CLI result during {:?}", self.stage)),
        }
        Ok(())
    }

    fn mounted(&mut self, _world: &mut World, value: Value) -> Result<(), String> {
        if value.get("error").is_some() {
            return Err(format!("Delta mount refused readiness: {}", value["error"]));
        }
        if value["operation"] != "mounted" {
            return Err("Delta did not return a mount readiness frame".into());
        }
        let binding = self.binding.as_mut().ok_or("Missing mount binding")?;
        if self.stage == Stage::MountShare {
            if value["reference"] != binding.worktree.clone().unwrap_or_default() {
                return Err("Delta mounted a different worktree reference".into());
            }
            let directory = string_field(&value, "directory")?;
            if !same_directory(Path::new(&directory), &binding.checkout) {
                return Err("Delta mounted a different checkout directory".into());
            }
            self.stage = Stage::Publish;
        } else {
            if value["thread_id"] != binding.thread || value["owner"] != binding.owner {
                return Err("Delta thread mount returned a different thread or owner".into());
            }
            let worktrees = value["worktrees"]
                .as_array()
                .ok_or("Delta thread mount returned no worktree readiness list")?;
            if worktrees.is_empty() {
                return Err("Delta thread mount returned no checkouts".into());
            }
            let mut roots = Vec::new();
            for worktree in worktrees {
                if worktree["state"] != "mounted" {
                    return Err("Not every joined worktree is ready; coding remains blocked".into());
                }
                roots.push((
                    string_field(worktree, "reference")?,
                    PathBuf::from(string_field(worktree, "directory")?),
                ));
            }
            let returned: BTreeSet<_> = roots
                .iter()
                .map(|(reference, _)| reference.clone())
                .collect();
            if returned != self.join_references || returned.len() != roots.len() {
                return Err("Delta mounted a changed or duplicate worktree set. No coding roots were trusted; recheck attachments and explicitly start join again".into());
            }
            let primary = roots.first().ok_or("No primary worktree")?.1.clone();
            self.workspace = Some(LiveWorkspace::prepare(
                &self.target_id,
                &self.client.home,
                &primary,
                &roots,
            )?);
            binding.checkout = primary;
            self.stage = Stage::TrustJoin;
        }
        Ok(())
    }

    fn install_join(&mut self, world: &mut World) -> Result<(), String> {
        let scope = self
            .workspace
            .take()
            .ok_or("Missing immutable trust plan")?;
        // A native answer does not permit a changed mount/instruction plan.
        let current = LiveWorkspace::prepare(
            &self.target_id,
            &self.client.home,
            scope.primary(),
            scope.roots(),
        )?;
        if current != scope {
            return Err("Joined directories or project instructions changed while trust was pending; no coding scope was installed".into());
        }
        self.persist_audit()?;
        if matches!(self.action, Action::Join(_)) {
            let model = world.get::<ModelChoice>(self.agent).cloned();
            let reasoning = world.get::<ReasoningSetting>(self.agent).cloned();
            let access = world.get::<ToolAccess>(self.agent).cloned();
            let prompt = world.get::<SystemPrompt>(self.agent).cloned();
            let binding = self
                .binding
                .as_ref()
                .ok_or("Missing joined binding")?
                .clone();
            let entity = world
                .spawn((
                    Agent,
                    self.target_id.clone(),
                    WorkDir(scope.primary().to_owned()),
                    Conversation::default(),
                    scope,
                    binding,
                ))
                .id();
            if let Some(model) = model {
                world.entity_mut(entity).insert(model);
            }
            if let Some(reasoning) = reasoning {
                world.entity_mut(entity).insert(reasoning);
            }
            if let Some(access) = access {
                world.entity_mut(entity).insert(access);
            }
            if let Some(prompt) = prompt {
                world.entity_mut(entity).insert(prompt);
            }
            self.target_agent = entity;
        } else {
            let primary = scope.primary().to_owned();
            world
                .entity_mut(self.agent)
                .insert((WorkDir(primary.clone()), scope));
            self.original_directory = primary;
        }
        self.save_binding(world)?;
        Ok(())
    }

    fn complete(&mut self, world: &mut World) -> Result<FlowProgress, String> {
        if let Some(connected) = self.ready.take() {
            #[cfg(feature = "tui")]
            if connected.binding.joined {
                world.trigger(rig_tui::ShowAgent {
                    entity: connected.agent,
                });
            }
            notice(
                world,
                self.agent,
                &format!(
                    "Delta mount and access verified for producer {}. The connected runtime will start the watch and validate its live snapshot before coding is released. Link: {}. Upload state must still be checked; local receipts are not server delivery.",
                    connected.binding.producer,
                    connected
                        .binding
                        .url
                        .as_deref()
                        .unwrap_or("not returned yet")
                ),
            );
            return Ok(FlowProgress::Connected(connected));
        }
        Ok(FlowProgress::Finished)
    }

    fn validate_automatic(&self, world: &mut World) -> Result<(), String> {
        let Some(automatic) = &self.automatic else {
            return Ok(());
        };
        let store = self
            .client
            .home
            .join("delta/policies.json")
            .canonicalize()
            .map_err(|error| format!("The trusted setup store is unavailable: {error}"))?;
        let project = world
            .resource::<ProjectRoot>()
            .0
            .canonicalize()
            .map_err(|error| error.to_string())?;
        if store.starts_with(&project) || store.starts_with(&self.client.cwd) {
            return Err("The setup consent store is writable or synchronizable from this project. Automatic publication cannot trust it; use a Rig home outside the checkout".into());
        }
        let workdirs: Vec<_> = world
            .query::<&WorkDir>()
            .iter(world)
            .map(|directory| directory.0.clone())
            .collect();
        if workdirs
            .iter()
            .filter_map(|directory| directory.canonicalize().ok())
            .any(|directory| store.starts_with(directory))
            || world.query::<&LiveWorkspace>().iter(world).any(|scope| {
                scope
                    .roots()
                    .iter()
                    .any(|(_, root)| store.starts_with(root))
            })
        {
            return Err("A model-accessible workspace can write the setup consent store; automatic publication is disabled".into());
        }
        let effective = policy::effective(&self.client.home, &self.client.cwd);
        if !effective.trusted
            || effective.policy != automatic.policy
            || effective.consent_audit.as_deref() != Some(&automatic.audit)
        {
            return Err("The automatic Delta policy, directory identity or native consent record changed; no automatic publication was started".into());
        }
        Ok(())
    }

    fn joining(&self) -> bool {
        matches!(self.action, Action::Join(_))
            || (matches!(self.action, Action::Reconnect)
                && self.binding.as_ref().is_some_and(|binding| binding.joined))
    }

    fn organization_body(&self) -> String {
        let mut text = "The organization owns the cloud record; choosing it does not itself select organization-wide visibility. Choose one explicitly or enter its exact ID.\n\n".to_owned();
        for (id, name) in &self.organizations {
            text.push_str(&format!("{name}: {id}\n"));
        }
        if let Some(id) = &self.organization {
            text.push_str(&format!("Previously saved organization: {id}\n"));
        }
        text
    }

    fn grant(&self) -> Result<String, String> {
        serde_json::to_string(&json!({
            "version":1, "client":self.client, "producer":self.target_id.0,
            "request":action_label(&self.action), "audience":self.audience,
            "organization":self.organization, "options":self.options,
            "policy":self.setup_policy, "consents":self.consents,
            "automatic_policy":self.automatic.as_ref().map(|grant| &grant.policy),
            "prior_setup_audit":self.automatic.as_ref().map(|grant| &grant.audit),
            "authority":"native_person_confirmed_delta_host_workflow",
            "retry_policy":"pause_on_failure",
        }))
        .map_err(|error| error.to_string())
    }

    fn persist_audit(&self) -> Result<(), String> {
        if !self.approved_host {
            return Err("No native Delta host consent to persist".into());
        }
        let directory = producer_directory(&self.client.home, &self.target_id.0)?;
        write_private(&directory.join("grant.json"), self.grant()?.as_bytes())
    }

    fn save_binding(&self, world: &mut World) -> Result<(), String> {
        let binding = self.binding.as_ref().ok_or("Missing binding to persist")?;
        let directory = producer_directory(&self.client.home, &binding.producer)?;
        write_private(
            &directory.join("binding.json"),
            &serde_json::to_vec(binding).map_err(|error| error.to_string())?,
        )?;
        if (self.target_agent != self.agent || !matches!(self.action, Action::Join(_)))
            && let Ok(mut entity) = world.get_entity_mut(self.target_agent)
        {
            entity.insert(binding.clone());
        }
        Ok(())
    }
}

/// Continuous upload cannot trust a journal or binding another model-accessible
/// workspace can rewrite. A fresh Person answer authorizes the connection, not
/// treating mutable project data as trusted future protocol input.
pub(crate) fn protect_journal_home(world: &mut World, home: &Path) -> Result<(), String> {
    let home = home
        .canonicalize()
        .map_err(|error| format!("Cannot resolve the private Delta journal home: {error}"))?;
    if model_writable_roots(world)?
        .iter()
        .any(|root| home.starts_with(root))
    {
        return Err("Rig's private home is inside a model-writable project/workdir. Continuous Delta sharing or joining cannot trust journals, bindings or consent there. Use a private Rig home outside every agent's writable roots; no live process was started".into());
    }
    Ok(())
}

/// A closed CLI surface is not a boundary if the model can replace the binary
/// behind it. File size/mtime checks alone cannot establish executable trust.
/// This applies even to explicitly confirmed one-shot administrative actions.
pub(crate) fn protect_executable(world: &mut World, executable: &Path) -> Result<(), String> {
    let executable = executable
        .canonicalize()
        .map_err(|error| format!("Cannot resolve the approved Delta executable: {error}"))?;
    if model_writable_roots(world)?
        .iter()
        .any(|root| executable.starts_with(root))
    {
        return Err("The Delta executable is inside a model-writable project/workdir. Native host execution requires an installed executable outside every agent's writable roots; no process was started".into());
    }
    Ok(())
}

fn model_writable_roots(world: &mut World) -> Result<Vec<PathBuf>, String> {
    let project = world
        .resource::<ProjectRoot>()
        .0
        .canonicalize()
        .map_err(|error| format!("Cannot establish the launcher's workspace boundary: {error}"))?;
    let mut writable = vec![project];
    writable.extend(world.query::<&WorkDir>().iter(world).map(|directory| {
        directory
            .0
            .canonicalize()
            .unwrap_or_else(|_| directory.0.clone())
    }));
    for scope in world.query::<&LiveWorkspace>().iter(world) {
        writable.extend(
            scope
                .roots()
                .iter()
                .map(|(_, root)| root.canonicalize().unwrap_or_else(|_| root.clone())),
        );
    }
    Ok(writable)
}

fn stop_and_drain(process: &mut Process) -> bool {
    process.request_stop();
    for _ in 0..MAX_EVENTS_PER_TICK {
        match process.try_event() {
            Some(Event::Line(_)) => (),
            Some(Event::Exited { .. } | Event::Failed { .. }) => break,
            None => break,
        }
    }
    process.is_finished()
}

fn action_label(action: &Action) -> String {
    match action {
        Action::Share(hint) => format!("Live share this session; audience hint: {hint}"),
        Action::Join(locator) => format!("Join shared thread {locator} into a fresh Rig session"),
        Action::Reconnect => "Reconnect the saved producer, mounts and exact outbox".into(),
        Action::Setup => {
            "Configure trusted local policy for future sessions in this checkout".into()
        }
        Action::Admin(args) => format!("One explicit administrative operation: {args:?}"),
    }
}

fn status_args(thread: &str, producer: &str) -> Vec<String> {
    vec![
        "--json".into(),
        "thread".into(),
        "status".into(),
        thread.into(),
        "--source".into(),
        "rig".into(),
        "--external-id".into(),
        producer.into(),
    ]
}

fn status_thread(value: &Value, producer: &str) -> Result<String, String> {
    if value["operation"] != "status" {
        return Err("Delta did not return thread status".into());
    }
    let thread = string_field(value, "thread_id")?;
    argument(&thread)?;
    if value["producer"]["source"] != "rig"
        || value["producer"]["external_id"] != producer
        || value["producer"]["next_sequence"]
            .as_u64()
            .is_none_or(|sequence| sequence == 0)
    {
        return Err(
            "Delta returned an unexpected producer identity or invalid sequence frontier".into(),
        );
    }
    if !matches!(
        value["access"]["state"].as_str(),
        Some("granted" | "local_only")
    ) {
        return Err("Delta thread access is unavailable".into());
    }
    Ok(thread)
}

fn organizations(value: &Value) -> Result<Vec<(String, String)>, String> {
    let entries = value
        .as_array()
        .or_else(|| value["organizations"].as_array())
        .ok_or("Delta org list returned no organizations array")?;
    if entries.len() > 128 {
        return Err("Delta organization directory exceeds 128 entries".into());
    }
    entries
        .iter()
        .map(|entry| {
            let id = string_field(entry, "id")?;
            argument(&id)?;
            let name = entry["name"].as_str().unwrap_or(&id).to_owned();
            Ok((id, name))
        })
        .collect()
}

fn words(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}
fn string_field(value: &Value, field: &str) -> Result<String, String> {
    value[field]
        .as_str()
        .filter(|value| !value.trim().is_empty())
        .map(str::to_owned)
        .ok_or_else(|| format!("Delta response has no nonblank {field}"))
}
fn argument(value: &str) -> Result<(), String> {
    if value.trim().is_empty()
        || value.len() > 4096
        || value.starts_with('-')
        || value.chars().any(char::is_control)
    {
        Err("Delta argument must be nonblank, bounded, control-free and not start with '-'".into())
    } else {
        Ok(())
    }
}
fn path_text(path: &Path) -> Result<String, String> {
    path.to_str()
        .map(str::to_owned)
        .ok_or_else(|| "Delta requires UTF-8 directory paths".into())
}
fn same_directory(left: &Path, right: &Path) -> bool {
    left.canonicalize()
        .ok()
        .zip(right.canonicalize().ok())
        .is_some_and(|(left, right)| left == right)
}
fn safe_url(url: &str) -> Result<(), String> {
    if !url.starts_with("https://")
        || url.len() > 8192
        || url
            .chars()
            .any(|character| character.is_control() || character.is_whitespace())
    {
        Err("Delta returned an invalid HTTPS URL; no link was fabricated".into())
    } else {
        Ok(())
    }
}
fn emails(text: &str) -> Result<Vec<String>, String> {
    let emails: BTreeSet<String> = text
        .split(|character: char| character.is_whitespace() || character == ',')
        .filter(|email| !email.is_empty())
        .map(str::to_owned)
        .collect();
    if emails.is_empty()
        || emails.len() > 20
        || emails
            .iter()
            .any(|email| !email.contains('@') || argument(email).is_err())
    {
        return Err("Enter 1 to 20 explicit email addresses, separated by spaces or commas".into());
    }
    Ok(emails.into_iter().collect())
}
fn user_revision(world: &World, agent: Entity) -> Vec<String> {
    world.get::<Conversation>(agent).map(|conversation| conversation.entries().filter(|entry| {
        entry.is_users_own() && matches!(entry.message, rig_harness::rig_core::message::Message::User { content } if content.iter().any(|part| !matches!(part, rig_harness::rig_core::message::UserContent::ToolResult(_))))
    }).map(|entry| entry.id.to_string()).collect()).unwrap_or_default()
}

fn workdir(world: &World, agent: Entity) -> PathBuf {
    world
        .get::<WorkDir>(agent)
        .map(|directory| directory.0.clone())
        .unwrap_or_else(|| world.resource::<ProjectRoot>().0.clone())
}
fn can_answer(world: &mut World, agent: Entity) -> bool {
    world
        .query::<&Answerer>()
        .iter(world)
        .any(|answerer| answerer.agent.is_none() || answerer.agent == Some(agent))
}
fn busy(world: &mut World, agent: Entity) -> bool {
    if world.get::<ActiveTurn>(agent).is_some() {
        return true;
    }
    let active: Vec<_> = world
        .query_filtered::<Entity, With<ActiveTurn>>()
        .iter(world)
        .collect();
    active.into_iter().any(|mut child| {
        for _ in 0..64 {
            let Some(parent) = world.get::<SpawnedBy>(child) else {
                return false;
            };
            if parent.0 == agent {
                return true;
            }
            child = parent.0;
        }
        true
    })
}
fn pending_local_intent(world: &mut World, agent: Entity, pending_text_answer: bool) -> bool {
    world
        .query::<(
            &Incoming,
            &ForAgent,
            Option<&Started>,
            Option<&Outcome>,
            Option<&Settled>,
            Option<&Decision>,
        )>()
        .iter(world)
        .any(|(message, owner, started, outcome, settled, decision)| {
            if owner.0 != agent
                || message.author.kind != AuthorKind::User
                || started.is_some()
                || outcome.is_some()
                || settled.is_some()
                || decision.is_some_and(Decision::is_decided)
            {
                return false;
            }
            let text = message.text().trim();
            let cancel = matches!(text.to_ascii_lowercase().as_str(), "cancel" | "stop" | "no");
            // Every new native command is intent, not a free-text answer to an
            // organization/email question. Remote/plugin messages do not count.
            cancel || message.command().is_some() || !pending_text_answer
        })
}

fn checkout_root(directory: &Path) -> Result<PathBuf, String> {
    let directory = directory
        .canonicalize()
        .map_err(|error| format!("Cannot resolve this checkout: {error}"))?;
    directory.ancestors().find(|parent| parent.join(".git").exists()).map(Path::to_owned)
        .ok_or_else(|| "Delta file sharing requires a Git checkout with a remote; no mount or upload was started".into())
}
fn notice(world: &mut World, agent: Entity, text: &str) {
    world.write_message(Notice::info(agent, text));
}

fn producer_directory(home: &Path, producer: &str) -> Result<PathBuf, String> {
    if producer.is_empty()
        || producer.len() > 128
        || !producer
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err("Invalid durable Delta producer identity".into());
    }
    let root = home.join("delta");
    private_directory(&root)?;
    let directory = root.join(producer);
    private_directory(&directory)?;
    Ok(directory)
}
fn private_directory(directory: &Path) -> Result<(), String> {
    match fs::symlink_metadata(directory) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => Ok(()),
        Ok(_) => Err("Delta durable state path is not a regular directory".into()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir(directory)
                .map_err(|error| format!("Cannot create Delta state directory: {error}"))?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(directory, fs::Permissions::from_mode(0o700))
                    .map_err(|error| error.to_string())?;
            }
            Ok(())
        }
        Err(error) => Err(format!("Cannot inspect Delta durable directory: {error}")),
    }
}
fn write_private(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if bytes.len() > 2 * 1024 * 1024 {
        return Err("Delta native audit/binding exceeds 2 MiB".into());
    }
    if fs::symlink_metadata(path)
        .is_ok_and(|metadata| !metadata.is_file() || metadata.file_type().is_symlink())
    {
        return Err("Delta durable state must not replace a link or non-file".into());
    }
    let parent = path.parent().ok_or("Delta state has no parent directory")?;
    let mut temporary =
        tempfile::NamedTempFile::new_in(parent).map_err(|error| error.to_string())?;
    temporary
        .write_all(bytes)
        .and_then(|()| temporary.as_file().sync_all())
        .map_err(|error| format!("Cannot flush Delta native state: {error}"))?;
    temporary
        .persist(path)
        .map_err(|error| format!("Cannot save Delta native state: {error}"))?;
    File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| format!("Cannot flush Delta state directory: {error}"))
}

/// Defense in depth: the controller never accepts a generic CLI tail, even if a
/// future caller bypasses the command parser. Mutation still needs native consent.
fn validate_admin(args: &[String]) -> Result<(), String> {
    if args.first().map(String::as_str) != Some("--json") {
        return Err("Administrative Delta commands must request JSON output".into());
    }
    let tail = &args[1..];
    let allowed = match tail {
        [group, action] => {
            (group == "org" && matches!(action.as_str(), "list" | "current"))
                || (group == "auth" && action == "status")
                || (group == "update" && action == "--check")
        }
        [group] => group == "update",
        [group, action, value] => {
            (group == "org"
                && matches!(action.as_str(), "switch" | "create")
                && argument(value).is_ok())
                || (group == "org"
                    && action == "invite"
                    && argument(value).is_ok()
                    && value.contains('@'))
                || (group == "auth" && action == "login" && value == "--no-wait")
                || (group == "thread"
                    && matches!(action.as_str(), "unshare" | "fork")
                    && argument(value).is_ok())
        }
        [group, action, values @ ..] => {
            group == "org"
                && action == "invite"
                && !values.is_empty()
                && values.len() <= 20
                && values
                    .iter()
                    .all(|value| argument(value).is_ok() && value.contains('@'))
        }
        _ => false,
    };
    if allowed {
        Ok(())
    } else {
        Err("Unsupported administrative Delta operation; no arbitrary arguments or shell are allowed".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(answerer: bool) -> (tempfile::TempDir, World, Entity, Client) {
        let scratch = tempfile::tempdir().unwrap();
        let root = scratch.path().join("project");
        let home = scratch.path().join("rig");
        fs::create_dir(&root).unwrap();
        fs::create_dir(root.join(".git")).unwrap();
        fs::create_dir(&home).unwrap();
        let root = root.canonicalize().unwrap();
        let home = home.canonicalize().unwrap();
        let executable = scratch.path().join("fake-delta");
        fs::write(&executable, "#!/bin/sh\nexit 1\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
        let executable = executable.canonicalize().unwrap();
        let metadata = executable.metadata().unwrap();
        let client = Client {
            executable,
            environment: std::collections::BTreeMap::new(),
            cwd: root.clone(),
            home,
            executable_len: metadata.len(),
            executable_modified: metadata.modified().ok(),
        };
        let mut world = World::new();
        world.insert_resource(ProjectRoot(root.clone()));
        world.insert_resource(Wake::default());
        let agent = world
            .spawn((Agent, AgentId("native-flow-test".into()), WorkDir(root)))
            .id();
        if answerer {
            world.spawn(Answerer { agent: Some(agent) });
        }
        (scratch, world, agent, client)
    }

    fn with_executable(mut client: Client, executable: &Path) -> Client {
        client.executable = executable.canonicalize().unwrap();
        let metadata = client.executable.metadata().unwrap();
        client.executable_len = metadata.len();
        client.executable_modified = metadata.modified().ok();
        client
    }

    #[test]
    fn no_native_surface_or_closed_operation_never_obtains_a_process() {
        let (_scratch, mut world, agent, client) = fixture(false);
        assert!(
            Flow::new(
                &mut world,
                agent,
                client.clone(),
                Action::Share(String::new())
            )
            .is_err()
        );
        assert!(!client.home.join("delta").exists());
        assert!(validate_admin(&words(&["--json", "org", "invite", "ada@example.com"])).is_ok());
        assert!(
            validate_admin(&words(&[
                "--json",
                "org",
                "invite",
                "ada@example.com",
                "sam@example.com"
            ]))
            .is_ok()
        );
        for args in [
            words(&["--json", "thread", "delete", "thread_1"]),
            words(&["--json", "org", "switch", "--force"]),
            words(&["--json", "auth", "logout"]),
            words(&["-c", "touch escaped"]),
        ] {
            assert!(validate_admin(&args).is_err());
        }
    }

    #[test]
    fn a_writable_private_home_blocks_join_and_invalidates_pending_host_consent() {
        let (scratch, mut world, agent, client) = fixture(true);
        let client = with_executable(client, Path::new("/bin/sh"));
        let mut flow = Flow::new(
            &mut world,
            agent,
            client.clone(),
            Action::Join("https://delta.dev/join/thread-example".into()),
        )
        .unwrap();
        flow.tick(&mut world).unwrap();
        world.flush();
        // A different agent's writable root can include the journal even when
        // neither the current checkout nor the new mount contains it.
        world.spawn((
            Agent,
            AgentId("other-agent".into()),
            WorkDir(scratch.path().canonicalize().unwrap()),
        ));
        assert!(flow.tick(&mut world).is_err());
        assert!(flow.command.is_none());
        assert!(!client.home.join("delta").exists());
        assert!(
            Flow::new(
                &mut world,
                agent,
                client.clone(),
                Action::Join("https://delta.dev/join/thread-example".into())
            )
            .is_err()
        );
        assert!(
            Flow::new(
                &mut world,
                agent,
                client.clone(),
                Action::Share(String::new())
            )
            .is_err()
        );
        // An explicit, bounded administrative request has no continuous
        // outbox authority and still needs its own native Person approval.
        assert!(
            Flow::new(
                &mut world,
                agent,
                client.clone(),
                Action::Admin(words(&["--json", "auth", "status"]))
            )
            .is_ok()
        );

        let (scratch, mut world, agent, client) = fixture(true);
        let client = with_executable(client, Path::new("/bin/sh"));
        world.insert_resource(ProjectRoot(scratch.path().canonicalize().unwrap()));
        assert!(
            Flow::new(
                &mut world,
                agent,
                client,
                Action::Join("https://delta.dev/join/thread-example".into())
            )
            .is_err(),
            "the launcher's broad root matters even when the active WorkDir is narrower"
        );
    }

    #[test]
    fn model_writable_executables_never_obtain_native_host_authority() {
        let (_scratch, mut world, agent, client) = fixture(true);
        let writable_binary = client.cwd.join("project-delta");
        fs::copy(&client.executable, &writable_binary).unwrap();
        let writable = with_executable(client.clone(), &writable_binary);
        assert!(
            writable.valid().is_ok(),
            "the fingerprint alone would accept this executable"
        );
        assert!(
            Flow::new(
                &mut world,
                agent,
                writable,
                Action::Admin(words(&["--json", "auth", "status"]))
            )
            .is_err()
        );
        assert!(!client.home.join("delta").exists());

        let (scratch, mut world, agent, client) = fixture(true);
        let tooling = scratch.path().join("tooling");
        fs::create_dir(&tooling).unwrap();
        let binary = tooling.join("installed-for-test");
        fs::copy(&client.executable, &binary).unwrap();
        let client = with_executable(client, &binary);
        let mut flow = Flow::new(
            &mut world,
            agent,
            client.clone(),
            Action::Admin(words(&["--json", "auth", "status"])),
        )
        .unwrap();
        flow.tick(&mut world).unwrap();
        world.flush();
        world.spawn((
            Agent,
            AgentId("other-agent".into()),
            WorkDir(tooling.canonicalize().unwrap()),
        ));
        assert!(
            client.valid().is_ok(),
            "the file need not change for new write authority to invalidate approval"
        );
        let failure = flow
            .tick(&mut world)
            .err()
            .expect("new writable root must invalidate the pending host question");
        assert!(failure.contains("executable"));
        assert!(flow.command.is_none());
        assert!(!client.home.join("delta").exists());
    }

    #[test]
    fn cancelling_a_flow_retains_the_mount_until_its_flush_is_observed() {
        use std::time::{Duration, Instant};
        let (_scratch, mut world, agent, client) = fixture(true);
        let mut flow = Flow::new(
            &mut world,
            agent,
            client.clone(),
            Action::Share(String::new()),
        )
        .unwrap();
        let mut request = client.request(vec!["-c".into(), "trap 'printf flushed > flow-flush; exit 0' TERM; printf 'ready\\n'; while :; do sleep 0.05; done".into()], None, true);
        request.executable = PathBuf::from("/bin/sh").canonicalize().unwrap();
        request
            .environment
            .insert("PATH".into(), "/usr/bin:/bin".into());
        request.shutdown_grace = Duration::from_secs(1);
        let mut process = Process::start(request, Wake::default()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            if matches!(process.try_event(), Some(Event::Line(line)) if line == "ready") {
                break;
            }
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
        process.mark_ready();
        flow.mount = Some(process);
        flow.cancel(&mut world);
        assert!(flow.tick(&mut world).is_err());
        while !flow.drained() {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(
            fs::read_to_string(client.cwd.join("flow-flush")).unwrap(),
            "flushed"
        );
    }

    #[test]
    fn pending_local_intent_invalidates_a_decision_but_unanswered_text_can_answer_its_question() {
        let (_scratch, mut world, agent, client) = fixture(true);
        let mut flow = Flow::new(
            &mut world,
            agent,
            client.clone(),
            Action::Share(String::new()),
        )
        .unwrap();
        flow.tick(&mut world).unwrap();
        world.flush();
        let question = flow.question.as_ref().unwrap().entity;
        let approved = Answer::choice("allow").outcome();
        let mut decision = Decision::default();
        decision.decide(Decider::Person, approved.clone());
        world
            .entity_mut(question)
            .insert((decision, approved, Settled));
        world.spawn((
            Incoming::new("/delta disconnect", DeliveryMode::Steer),
            ForAgent(agent),
        ));
        world.flush();
        assert!(flow.tick(&mut world).is_err());
        assert!(flow.command.is_none());
        assert!(!client.home.join("delta").exists());

        let (_scratch, mut world, agent, client) = fixture(true);
        let mut flow = Flow::new(&mut world, agent, client, Action::Share(String::new())).unwrap();
        flow.stage = Stage::Organization;
        flow.tick(&mut world).unwrap();
        world.flush();
        world.spawn((
            Incoming::new("organization-1", DeliveryMode::Steer),
            ForAgent(agent),
        ));
        world.flush();
        assert!(
            flow.validate(&mut world).is_ok(),
            "an undecided free-text answer is not stale intent"
        );
        world.spawn((
            Incoming::new("remote note", DeliveryMode::Inform).with_author(Author::plugin("delta")),
            ForAgent(agent),
        ));
        world.flush();
        assert!(
            flow.validate(&mut world).is_ok(),
            "remote context is not local user authorization or cancellation"
        );
        world.spawn((Incoming::new("/new", DeliveryMode::Steer), ForAgent(agent)));
        world.flush();
        assert!(flow.validate(&mut world).is_err());

        let (_scratch, mut world, agent, client) = fixture(true);
        let flow = Flow::new(&mut world, agent, client, Action::Share(String::new())).unwrap();
        world.get_mut::<Conversation>(agent).unwrap().push(
            rig_harness::rig_core::message::Message::user(
                "A new request already processed by the inbox",
            ),
        );
        assert!(
            flow.validate(&mut world).is_err(),
            "settled input must still revoke stale workflow consent"
        );
    }

    #[test]
    fn changed_questions_nonperson_and_rewritten_answers_cannot_grant_host_access() {
        for case in 0..3 {
            let (_scratch, mut world, agent, client) = fixture(true);
            let mut flow = Flow::new(
                &mut world,
                agent,
                client.clone(),
                Action::Share(String::new()),
            )
            .unwrap();
            assert!(matches!(
                flow.tick(&mut world).unwrap(),
                FlowProgress::Waiting
            ));
            world.flush();
            let question = flow.question.as_ref().unwrap().entity;
            let approved = Answer::choice("allow").outcome();
            let mut decision = Decision::default();
            if case == 1 {
                decision.decide(Decider::plugin("not-the-person"), approved.clone());
            } else if case == 2 {
                decision.decide(Decider::Person, Answer::choice("cancel").outcome());
            } else {
                decision.decide(Decider::Person, approved.clone());
                world
                    .get_mut::<Ask>(question)
                    .unwrap()
                    .body
                    .push_str(" changed after display");
            }
            world
                .entity_mut(question)
                .insert((decision, approved, Settled));
            assert!(flow.tick(&mut world).is_err());
            assert!(flow.command.is_none());
            assert!(!client.home.join("delta").exists());
        }
    }
}
