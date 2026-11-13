//! One bounded request queue; each interpreter request becomes a child tool effect.

use async_channel::{Receiver, Sender};
use futures_channel::oneshot;
use rig_harness::prelude::*;
use rig_harness::rig_core::message::{ToolFunction, ToolName};
use rig_harness::rig_core::tool::ToolResult;
use rig_native_shell::{NativeCommandRequest, NativeExecution, NativeXonshParent};
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
    operation: Operation,
    answer: oneshot::Sender<Result<Value, String>>,
}

enum Operation {
    Tool {
        name: String,
        arguments: Value,
    },
    Native {
        pipeline: Vec<Vec<String>>,
        execution: NativeExecution,
    },
}

pub(crate) struct Host {
    sender: Sender<Request>,
    wake: Wake,
    parent: Entity,
    agent: Entity,
    native: Option<NativeExecution>,
}

impl Host {
    pub(crate) fn new(
        requests: &Requests,
        wake: Wake,
        parent: Entity,
        agent: Entity,
        native: Option<NativeExecution>,
    ) -> Self {
        Self {
            sender: requests.sender.clone(),
            wake,
            parent,
            agent,
            native,
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
        self.request(Operation::Tool {
            name: name.to_owned(),
            arguments,
        })
        .await
    }

    pub(crate) async fn native(&self, pipeline: Vec<Vec<String>>) -> Result<Value, String> {
        let execution = self
            .native
            .clone()
            .ok_or("native shell forms are available only to the native xonsh frontend")?;
        self.request(Operation::Native {
            pipeline,
            execution,
        })
        .await
    }

    async fn request(&self, operation: Operation) -> Result<Value, String> {
        let (answer, receiver) = oneshot::channel();
        self.sender
            .send(Request {
                parent: self.parent,
                agent: self.agent,
                operation,
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
        (
            &ForAgent,
            &Decision,
            Option<&CallOf>,
            Option<&NativeXonshParent>,
            Option<&ToolRequest>,
        ),
        (
            With<Started>,
            With<Running>,
            Without<Settled>,
            Without<Outcome>,
        ),
    >,
    access: Query<&ToolAccess>,
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
            .filter(|(owner, decision, _, _, _)| {
                owner.0 == request.agent && !decision.is_decided()
            });
        let Some((_, _, turn, native, parent_request)) = parent else {
            let _ = request
                .answer
                .send(Err("the interpreter call ended".to_owned()));
            continue;
        };
        let prepared = match request.operation {
            Operation::Tool { name, arguments } => match ToolName::new(name) {
                Ok(name) => Ok(ChildRequest::Tool(ToolRequest {
                    call: ToolCall::from_wire("", ToolFunction::new(name, arguments)),
                })),
                Err(error) => Err(error.to_string()),
            },
            Operation::Native {
                pipeline,
                execution,
            } => {
                let allowed = native.is_some_and(|native| {
                    native.matches(request.parent, request.agent, &execution)
                }) && parent_request
                    .is_some_and(|request| request.call.function.name.as_str() == "run_xonsh")
                    && access
                        .get(request.agent)
                        .is_ok_and(|access| access.allows("run_xonsh"));
                if allowed {
                    NativeCommandRequest::for_xonsh(request.parent, pipeline, execution)
                        .map(ChildRequest::Native)
                } else {
                    Err("the native xonsh command capability is no longer current".to_owned())
                }
            }
        };
        let prepared = match prepared {
            Ok(prepared) => prepared,
            Err(error) => {
                let _ = request.answer.send(Err(error));
                continue;
            }
        };
        let mut child = commands.spawn((
            ForAgent(request.agent),
            ChildOf(request.parent),
            HostCall {
                parent: request.parent,
                answer: Some(request.answer),
            },
        ));
        match prepared {
            ChildRequest::Tool(request) => {
                child.insert(request);
            }
            ChildRequest::Native(request) => {
                child.insert(request);
            }
        }
        if let Some(turn) = turn {
            child.insert(CallOf(turn.0));
        }
    }
}

enum ChildRequest {
    Tool(ToolRequest),
    Native(NativeCommandRequest),
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
