//! One bounded request queue; each interpreter request becomes a child tool effect.

use async_channel::{Receiver, Sender};
use futures_channel::oneshot;
use rig_harness::prelude::*;
use rig_harness::rig_core::message::{ToolFunction, ToolName};
use rig_harness::rig_core::tool::ToolResult;
use serde::Serialize;
use serde_json::Value;
use std::io::{self, Write};

const MAX_REPLY_BYTES: usize = 256 * 1024;

#[derive(Resource)]
pub(crate) struct Requests {
    sender: Sender<Request>,
    receiver: Receiver<Request>,
}

impl Default for Requests {
    fn default() -> Self {
        let (sender, receiver) = async_channel::bounded(32);
        Self { sender, receiver }
    }
}

struct Request {
    parent: Entity,
    agent: Entity,
    name: String,
    arguments: Value,
    answer: oneshot::Sender<Result<Value, String>>,
}

pub(crate) struct Host {
    sender: Sender<Request>,
    wake: Wake,
    parent: Entity,
    agent: Entity,
}

impl Host {
    pub(crate) fn new(requests: &Requests, wake: Wake, parent: Entity, agent: Entity) -> Self {
        Self {
            sender: requests.sender.clone(),
            wake,
            parent,
            agent,
        }
    }

    pub(crate) async fn call(&self, name: &str, arguments: Value) -> Result<Value, String> {
        // These two frontends share a runtime. Nesting them would evade a
        // per-invocation worker budget without adding a useful host capability.
        if matches!(name, "run_monty" | "run_xonsh") {
            return Err("recursive interpreter calls are unavailable".to_owned());
        }
        if !arguments.is_object() {
            return Err("tool arguments must be a JSON object".to_owned());
        }
        let (answer, receiver) = oneshot::channel();
        self.sender
            .send(Request {
                parent: self.parent,
                agent: self.agent,
                name: name.to_owned(),
                arguments,
                answer,
            })
            .await
            .map_err(|_| "the Rig tool bridge closed".to_owned())?;
        self.wake.wake();
        receiver
            .await
            .map_err(|_| "the parent call ended before its tool answered".to_owned())?
    }
}

#[derive(Component)]
pub(crate) struct HostCall {
    parent: Entity,
    answer: Option<oneshot::Sender<Result<Value, String>>>,
}

pub(crate) fn dispatch(
    requests: Res<Requests>,
    parents: Query<
        (&ForAgent, &Decision, Option<&CallOf>),
        (
            With<Started>,
            With<Running>,
            Without<Settled>,
            Without<Outcome>,
        ),
    >,
    mut commands: Commands,
) {
    // Bound main-thread work even when many programs are waiting.
    for _ in 0..32 {
        let Ok(request) = requests.receiver.try_recv() else {
            break;
        };
        if request.answer.is_canceled() {
            continue;
        }
        let parent = parents
            .get(request.parent)
            .ok()
            .filter(|(owner, decision, _)| owner.0 == request.agent && !decision.is_decided());
        let Some((_, _, turn)) = parent else {
            let _ = request
                .answer
                .send(Err("the interpreter call ended".to_owned()));
            continue;
        };
        let name = match ToolName::new(request.name) {
            Ok(name) => name,
            Err(error) => {
                let _ = request.answer.send(Err(error.to_string()));
                continue;
            }
        };
        let call = ToolCall::from_wire("", ToolFunction::new(name, request.arguments));
        let mut child = commands.spawn((
            ToolRequest { call },
            ForAgent(request.agent),
            ChildOf(request.parent),
            HostCall {
                parent: request.parent,
                answer: Some(request.answer),
            },
        ));
        if let Some(turn) = turn {
            child.insert(CallOf(turn.0));
        }
    }
}

pub(crate) fn bind_owner(
    added: On<Add<HostCall>>,
    calls: Query<&HostCall>,
    mut commands: Commands,
) {
    if let Ok(call) = calls.get(added.entity) {
        commands.entity(added.entity).insert(ChildOf(call.parent));
    }
}

pub(crate) fn answer(settled: On<Add<Settled>>, mut calls: Query<(&Outcome, &mut HostCall)>) {
    if let Ok((outcome, mut call)) = calls.get_mut(settled.entity)
        && let Some(answer) = call.answer.take()
    {
        let _ = answer.send(reply(&outcome.tool_result()));
    }
}

fn reply(result: &ToolResult) -> Result<Value, String> {
    let plain_text;
    let data = match result.output().as_json() {
        Some(value) => value,
        None => {
            let rendered = result.output().render();
            if rendered.len() > MAX_REPLY_BYTES {
                return Err("tool reply exceeds 256 KiB".to_owned());
            }
            plain_text = Value::String(rendered);
            &plain_text
        }
    };
    #[derive(Serialize)]
    struct Reply<'a> {
        status: &'a str,
        data: &'a Value,
    }
    let mut bytes = BoundedJson(Vec::new());
    serde_json::to_writer(
        &mut bytes,
        &Reply {
            status: result.status_name(),
            data,
        },
    )
    .map_err(|_| "tool reply exceeds the JSON byte bound".to_owned())?;
    serde_json::from_slice(&bytes.0).map_err(|error| error.to_string())
}

struct BoundedJson(Vec<u8>);

impl Write for BoundedJson {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > MAX_REPLY_BYTES - self.0.len() {
            return Err(io::Error::other("tool reply exceeds 256 KiB"));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
