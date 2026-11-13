//! Typed native pipeline requests, independent of any public shell tool.

use std::path::PathBuf;

use crate::PendingCommand;
use rig_harness::prelude::*;
use rig_harness::rig_core::effect::EffectKind;
use rig_workspace::WorkspaceOperation;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;
use serde_json::{Map, Value};

/// Execution defaults for each independently gated native command.
///
/// An escalation flag requests review; it is never an execution permit. The
/// defaults remain sandboxed, with the configured native-command timeout.
#[derive(Clone, Debug, Default, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct NativeExecution {
    pub cwd: Option<PathBuf>,
    pub timeout_secs: Option<u64>,
    pub escalate: bool,
    pub justification: Option<String>,
}

impl NativeExecution {
    pub fn validate(&self) -> Result<(), String> {
        if self
            .timeout_secs
            .is_some_and(|timeout| !(1..=300).contains(&timeout))
        {
            return Err("timeout_secs must be between 1 and 300".to_owned());
        }
        if self
            .justification
            .as_ref()
            .is_some_and(|text| text.len() > 4096)
        {
            return Err("justification exceeds 4096 bytes".to_owned());
        }
        if self.escalate
            && self
                .justification
                .as_deref()
                .is_none_or(|text| text.trim().is_empty())
        {
            return Err("escalation requires a justification".to_owned());
        }
        Ok(())
    }
}

/// A native xonsh parent's live, immutable command capability.
///
/// Trusted frontends install this only after admitting native `run_xonsh`.
/// It binds the exact parent effect, owning agent and execution defaults. It is
/// neither reflected nor serialized, and grants no sandbox exception itself.
#[derive(Component, Debug)]
#[component(immutable)]
pub struct NativeXonshParent {
    call: Entity,
    agent: Entity,
    execution: NativeExecution,
}

impl NativeXonshParent {
    pub fn new(call: Entity, agent: Entity, execution: NativeExecution) -> Self {
        Self {
            call,
            agent,
            execution,
        }
    }

    pub fn matches(&self, call: Entity, agent: Entity, execution: &NativeExecution) -> bool {
        self.call == call && self.agent == agent && self.execution == *execution
    }
}

/// One immutable, argument-safe native pipeline owned by its calling effect.
///
/// Trusted frontends construct this from expanded argv, not Python-generated
/// shell source. It receives the same freeze, workspace, review and launch
/// checks as the optional public native-shell tool. It exposes no model tool.
#[derive(Component, Clone, Debug, PartialEq, Eq)]
#[component(immutable)]
#[require(Effect, WorkspaceOperation, Footprint)]
pub struct NativeCommandRequest {
    arguments: Map<String, Value>,
    execution: NativeExecution,
    xonsh_parent: Option<Entity>,
}

impl NativeCommandRequest {
    /// Quote argv as data into one pipefail-enabled shell pipeline.
    pub fn pipeline(
        commands: Vec<Vec<String>>,
        execution: NativeExecution,
    ) -> Result<Self, String> {
        execution.validate()?;
        if commands.is_empty() {
            return Err("a native pipeline must contain a command".to_owned());
        }
        let mut script = String::from("set -o pipefail; ");
        for (index, command) in commands.iter().enumerate() {
            if command.is_empty() || command[0].is_empty() {
                return Err("a native command must contain an executable name".to_owned());
            }
            if index != 0 {
                script.push_str(" | ");
            }
            for (index, argument) in command.iter().enumerate() {
                if argument.contains('\0') {
                    return Err("native arguments cannot contain NUL".to_owned());
                }
                if index != 0 {
                    script.push(' ');
                }
                script.push_str(&shell_words::quote(argument));
                if script.len() > 65_536 {
                    return Err("native pipeline exceeds 65536 bytes after quoting".to_owned());
                }
            }
        }
        let Value::Object(mut arguments) =
            serde_json::to_value(&execution).map_err(|error| error.to_string())?
        else {
            return Err("native execution options must be an object".to_owned());
        };
        arguments.insert("script".into(), Value::String(script));
        Ok(Self {
            arguments,
            execution,
            xonsh_parent: None,
        })
    }

    /// Bind a pipeline to its live native xonsh invocation. The runtime checks
    /// that exact parent's capability again at Gate and immediately at launch.
    pub fn for_xonsh(
        parent: Entity,
        commands: Vec<Vec<String>>,
        execution: NativeExecution,
    ) -> Result<Self, String> {
        let mut request = Self::pipeline(commands, execution)?;
        request.xonsh_parent = Some(parent);
        Ok(request)
    }

    pub(crate) fn xonsh_parent(&self) -> Option<Entity> {
        self.xonsh_parent
    }

    pub(crate) fn arguments(&self) -> &Map<String, Value> {
        &self.arguments
    }
}

pub(crate) fn bind_request(
    inserted: On<Insert<NativeCommandRequest>>,
    requests: Query<(&NativeCommandRequest, Has<PendingCommand>, Has<Started>)>,
    parents: Query<&Effect>,
    mut commands: Commands,
) {
    if let Ok((request, frozen, started)) = requests.get(inserted.entity) {
        if frozen || started {
            return; // The record remains bound to the frozen/executing input.
        }
        let mut payload = request.arguments.clone();
        if let Some(parent) = request.xonsh_parent {
            payload.insert(
                "origin".into(),
                json!({
                    "frontend": "run_xonsh",
                    "parent_effect": parents.get(parent).ok().map(|effect| effect.id().as_u64()),
                }),
            );
        }
        commands.entity(inserted.entity).insert((
            Name::new("native command"),
            EffectRequest {
                kind: EffectKind::Custom {
                    kind: "native-command".into(),
                    payload: Value::Object(payload),
                },
                handler: None,
            },
        ));
    }
}

pub(crate) fn validate_origins(
    mut calls: Query<
        (&NativeCommandRequest, &ForAgent, &mut Decision),
        (ReadyToStart, With<NativeCommandRequest>),
    >,
    parents: Query<
        (&ForAgent, &ToolRequest, &Decision, &NativeXonshParent),
        (
            With<Admitted>,
            With<Started>,
            With<Running>,
            Without<Outcome>,
            Without<Settled>,
            Without<NativeCommandRequest>,
        ),
    >,
    access: Query<&ToolAccess, With<Agent>>,
) {
    for (request, &ForAgent(agent), mut decision) in &mut calls {
        let Some(parent) = request.xonsh_parent else {
            continue;
        };
        let allowed = parents
            .get(parent)
            .is_ok_and(|(owner, tool, status, capability)| {
                owner.0 == agent
                    && tool.call.function.name.as_str() == "run_xonsh"
                    && !status.is_decided()
                    && capability.matches(parent, agent, &request.execution)
            })
            && access
                .get(agent)
                .is_ok_and(|access| access.allows("run_xonsh"));
        if !allowed {
            crate::refuse(
                &mut decision,
                "The native xonsh parent capability is no longer current.",
            );
        }
    }
}

pub(crate) fn origin_is_current(
    world: &World,
    request: &NativeCommandRequest,
    agent: Entity,
) -> bool {
    let Some(parent) = request.xonsh_parent else {
        return true;
    };
    world.get::<Admitted>(parent).is_some()
        && world.get::<Started>(parent).is_some()
        && world.get::<Running>(parent).is_some()
        && world.get::<Outcome>(parent).is_none()
        && world.get::<Settled>(parent).is_none()
        && world.get::<NativeCommandRequest>(parent).is_none()
        && world
            .get::<ForAgent>(parent)
            .is_some_and(|owner| owner.0 == agent)
        && world
            .get::<ToolRequest>(parent)
            .is_some_and(|tool| tool.call.function.name.as_str() == "run_xonsh")
        && world
            .get::<Decision>(parent)
            .is_some_and(|decision| !decision.is_decided())
        && world
            .get::<NativeXonshParent>(parent)
            .is_some_and(|capability| capability.matches(parent, agent, &request.execution))
        && world
            .get::<ToolAccess>(agent)
            .is_some_and(|access| access.allows("run_xonsh"))
}

pub(crate) fn start_request(
    started: On<Add<Started>>,
    requests: Query<&ForAgent, With<NativeCommandRequest>>,
    mut commands: Commands,
) {
    if let Ok(&ForAgent(agent)) = requests.get(started.entity) {
        let call = started.entity;
        commands.queue(move |world: &mut World| crate::launch::start(world, call, agent));
    }
}
