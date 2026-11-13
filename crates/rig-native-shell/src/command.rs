//! Prepared command contracts and the gates that freeze and enforce them.

use crate::{
    Authorization, Executor, NATIVE_SHELL, NativeCommandRequest, ShellConfig, UserEvidence, refuse,
};
use rig_command::PreparedCommand;
use rig_harness::prelude::*;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ShellArgs {
    script: String,
    #[serde(default)]
    cwd: Option<PathBuf>,
    #[serde(default)]
    timeout_secs: Option<u64>,
    #[serde(default)]
    escalate: bool,
    #[serde(default)]
    justification: Option<String>,
}

/// The immutable action judged by a reviewer and consumed by the native runtime.
/// The runtime constructs it from the final public tool or typed pipeline request,
/// binding the owning effect, agent and any originating xonsh invocation.
#[derive(Component, Clone, Debug, PartialEq, Eq, Serialize)]
#[component(immutable)]
pub struct PendingCommand {
    #[serde(skip)]
    call: Entity,
    #[serde(skip)]
    agent: Entity,
    #[serde(skip)]
    xonsh_parent: Option<Entity>,
    command: PreparedCommand,
    arguments: serde_json::Map<String, serde_json::Value>,
    base: PathBuf,
    escalated: bool,
    justification: Option<String>,
}

impl PendingCommand {
    /// The validated native input that execution consumes after permission checks.
    pub fn command(&self) -> &PreparedCommand {
        &self.command
    }
    pub(crate) fn is_call_for(&self, call: Entity, agent: Entity) -> bool {
        self.call == call && self.agent == agent
    }
    pub(crate) fn into_command(self) -> PreparedCommand {
        self.command
    }
    /// Whether this action requests the OS user's full authority.
    pub fn is_escalation(&self) -> bool {
        self.escalated
    }
    /// The coding agent's explanation, which cannot establish user authorization.
    pub fn justification(&self) -> Option<&str> {
        self.justification.as_deref()
    }

    /// Issues a permit for this action and the user evidence the reviewer saw.
    /// Only trusted host plugins should call this after an affirmative review.
    pub fn permit(&self, evidence: UserEvidence) -> ReviewPermit {
        ReviewPermit {
            command: self.clone(),
            evidence,
        }
    }

    pub(crate) fn matches(
        &self,
        arguments: &serde_json::Map<String, serde_json::Value>,
        base: &Path,
        xonsh_parent: Option<Entity>,
    ) -> bool {
        self.arguments == *arguments && self.base == base && self.xonsh_parent == xonsh_parent
    }
}

/// A host-issued exception bound to one command snapshot and user evidence.
#[derive(Component, Debug)]
#[component(immutable)]
pub struct ReviewPermit {
    command: PendingCommand,
    evidence: UserEvidence,
}

impl ReviewPermit {
    pub(crate) fn evidence_for(&self, command: &PendingCommand) -> Option<&UserEvidence> {
        (self.command == *command).then_some(&self.evidence)
    }

    fn valid(
        &self,
        command: &PendingCommand,
        agent: Entity,
        authorization: &Authorization,
    ) -> bool {
        self.command == *command
            && authorization
                .capture(agent, self.evidence.max_bytes())
                .ok()
                .as_ref()
                == Some(&self.evidence)
    }
}

/// Read the current immutable typed request or optional public tool arguments.
/// A mixed request is invalid rather than choosing one authority path.
pub(crate) fn arguments<'a>(
    tool: Option<&'a ToolRequest>,
    native: Option<&'a NativeCommandRequest>,
) -> Result<Option<&'a serde_json::Map<String, serde_json::Value>>, String> {
    match (tool, native) {
        (Some(_), Some(_)) => Err("A native command cannot also be a tool request.".to_owned()),
        (None, Some(native)) => Ok(Some(native.arguments())),
        (Some(tool), None) if tool.call.function.name.as_str() == NATIVE_SHELL => {
            Ok(Some(&tool.call.function.arguments))
        }
        _ => Ok(None),
    }
}

pub(crate) fn freeze(
    mut calls: Query<
        (
            Entity,
            Option<&ToolRequest>,
            Option<&NativeCommandRequest>,
            Option<&ForAgent>,
            Option<&PendingCommand>,
            &mut Decision,
        ),
        (
            ReadyToStart,
            Or<(With<ToolRequest>, With<NativeCommandRequest>)>,
        ),
    >,
    agents: Query<(&AgentId, Option<&WorkDir>), With<Agent>>,
    root: Res<ProjectRoot>,
    executor: Res<Executor>,
    config: Res<ShellConfig>,
    mut commands: Commands,
) {
    for (call, tool, native, owner, frozen, mut decision) in &mut calls {
        if decision.is_decided() {
            continue;
        }
        let arguments = match arguments(tool, native) {
            Ok(Some(arguments)) => arguments,
            Ok(None) => continue,
            Err(error) => {
                refuse(&mut decision, error);
                continue;
            }
        };
        let Some(&ForAgent(agent)) = owner else {
            refuse(&mut decision, "A native command requires an owning agent.");
            continue;
        };
        let Ok((_, directory)) = agents.get(agent) else {
            refuse(&mut decision, "A native command requires a live Rig agent.");
            continue;
        };
        let base = directory.map_or(root.0.as_path(), |work| work.0.as_path());
        let xonsh_parent = native.and_then(NativeCommandRequest::xonsh_parent);
        if let Some(frozen) = frozen {
            if !frozen.is_call_for(call, agent) || !frozen.matches(arguments, base, xonsh_parent) {
                refuse(
                    &mut decision,
                    "The command changed after preparation. Submit a new call.",
                );
            }
            continue;
        }
        let prepared = prepare(
            call,
            agent,
            xonsh_parent,
            arguments,
            base,
            &executor,
            &config,
        );
        match prepared {
            Ok(prepared) => {
                commands.entity(call).insert(prepared);
            }
            Err(error) => refuse(&mut decision, error),
        }
    }
}

fn prepare(
    call: Entity,
    agent: Entity,
    xonsh_parent: Option<Entity>,
    arguments: &serde_json::Map<String, serde_json::Value>,
    base: &Path,
    executor: &Executor,
    config: &ShellConfig,
) -> Result<PendingCommand, String> {
    let args: ShellArgs = serde_json::from_value(serde_json::Value::Object(arguments.clone()))
        .map_err(|error| error.to_string())?;
    if args.script.trim().is_empty() || args.script.len() > 65_536 {
        return Err("script must contain 1 to 65536 bytes".to_owned());
    }
    let timeout = args.timeout_secs.unwrap_or(config.timeout_secs);
    if !(1..=300).contains(&timeout) {
        return Err("timeout_secs must be between 1 and 300".to_owned());
    }
    if args
        .justification
        .as_ref()
        .is_some_and(|text| text.len() > 4096)
    {
        return Err("justification exceeds 4096 bytes".to_owned());
    }
    if args.escalate
        && args
            .justification
            .as_deref()
            .is_none_or(|text| text.trim().is_empty())
    {
        return Err("escalation requires a justification".to_owned());
    }
    let executor = executor.0.as_ref().map_err(Clone::clone)?;
    let cwd = args
        .cwd
        .as_ref()
        .map_or_else(|| base.to_owned(), |cwd| base.join(cwd));
    let command = executor
        .prepare(base, &cwd, args.script, timeout)
        .map_err(|error| error.to_string())?;
    Ok(PendingCommand {
        call,
        agent,
        xonsh_parent,
        command,
        arguments: arguments.clone(),
        base: base.to_owned(),
        escalated: args.escalate,
        justification: args.justification,
    })
}

pub(crate) fn require_permit(
    mut calls: Query<
        (
            &PendingCommand,
            Option<&ReviewPermit>,
            &ForAgent,
            &mut Decision,
        ),
        ReadyToStart,
    >,
    authorization: Authorization,
) {
    for (command, permit, &ForAgent(agent), mut decision) in &mut calls {
        if command.escalated
            && !permit.is_some_and(|permit| permit.valid(command, agent, &authorization))
        {
            refuse(
                &mut decision,
                "Escalation has no current approval for this exact command.",
            );
        }
    }
}
