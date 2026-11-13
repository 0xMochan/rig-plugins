//! One owned, crash-isolated worker execution with no implicit host capabilities.

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use monty_pool::{Pool, PoolConfig, ReplConfig, ResumeValue, TurnEvent, on_print_sync};
use monty_types::{CallArgs, NameLookupResult, OsPolicy, PrintStream, ResourceLimits, SleepMode};
use serde_json::Value;
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tokio::time::timeout;

use crate::host::Host;
use crate::{Output, Program, values};

const MAX_SOURCE_BYTES: usize = 64 * 1024;
const MAX_INPUT_BYTES: usize = 64 * 1024;
const MAX_JSON_BYTES: usize = 256 * 1024;
const MAX_PRINT_BYTES: usize = 64 * 1024;

pub(crate) async fn run(worker: PathBuf, program: Program, host: Host) -> Result<Output, String> {
    if program.code.len() > MAX_SOURCE_BYTES {
        return Err("Python source exceeds 64 KiB".to_owned());
    }
    let inputs = values::inputs(&program.inputs, MAX_INPUT_BYTES)?;
    verify_worker(&worker).await?;
    let mut config = PoolConfig::subprocess(worker);
    config.min_processes = 0;
    config.max_processes = 1;
    config.checkout_timeout = Some(Duration::from_secs(2));
    config.request_timeout = Some(Duration::from_secs(6));
    // Keep the completed worker idle until close can explicitly reap it.
    config.max_checkouts_per_worker = None;
    config.auto_resume = false;
    let pool = Pool::new(config).await.map_err(|error| error.to_string())?;
    let limits = ResourceLimits {
        max_feed_duration: Some(Duration::from_secs(5)),
        max_turn_duration: Some(Duration::from_secs(5)),
        max_memory: Some(32 * 1024 * 1024),
        max_recursion_depth: 128,
        max_suspensions: 64,
        max_total_sleep: Some(Duration::ZERO),
        ..ResourceLimits::default()
    };
    let repl = ReplConfig {
        limits: Some(limits),
        os_policy: OsPolicy {
            sleep: SleepMode::CallHost,
            ..OsPolicy::default()
        },
        ..ReplConfig::default()
    };
    let mut captured = CapturedOutput::default();
    let result = execute(&pool, &repl, program.code, inputs, &host, &mut captured).await;
    // On failure execute has already dropped its checkout, requesting worker
    // cancellation. Normal finish returns it idle so this call can reap it.
    pool.close().await;
    Ok(Output {
        value: result?,
        stdout: captured.stdout,
        stderr: captured.stderr,
        output_truncated: captured.truncated,
    })
}

async fn verify_worker(worker: &std::path::Path) -> Result<(), String> {
    if !worker.is_absolute() {
        return Err("Monty requires a trusted absolute worker path".to_owned());
    }
    let mut child = Command::new(worker)
        .arg("--version")
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| format!("could not start Monty's version check: {error}"))?;
    timeout(Duration::from_secs(2), async {
        let stdout = child
            .stdout
            .take()
            .ok_or("Monty's version output pipe is unavailable")?;
        let mut output = Vec::with_capacity(257);
        stdout
            .take(257)
            .read_to_end(&mut output)
            .await
            .map_err(|error| format!("could not read Monty's version: {error}"))?;
        if output.len() > 256 {
            return Err("Monty's version output exceeds 256 bytes".to_owned());
        }
        let status = child
            .wait()
            .await
            .map_err(|error| format!("could not finish Monty's version check: {error}"))?;
        let version = std::str::from_utf8(&output)
            .map_err(|_| "Monty's version output is not UTF-8")?
            .trim_end_matches(['\r', '\n']);
        if !status.success() || version != "monty-runtime 1.4.0" {
            return Err("this plugin requires the trusted monty 1.4.0 worker".to_owned());
        }
        Ok(())
    })
    .await
    .map_err(|_| "Monty's version check exceeded two seconds".to_owned())?
}

async fn execute(
    pool: &Pool,
    repl: &ReplConfig,
    code: String,
    inputs: monty_types::NamedValues,
    host: &Host,
    captured: &mut CapturedOutput,
) -> Result<Value, String> {
    let mut session = pool
        .checkout(repl)
        .await
        .map_err(|error| error.to_string())?;
    let mut on_print = on_print_sync(|stream, text| captured.push(stream, text));
    let mut event = session
        .feed(code, inputs, Vec::new(), false, &mut on_print)
        .await
        .map_err(|error| error.to_string())?;
    loop {
        event = match event {
            TurnEvent::FunctionCall {
                function_name,
                args,
                object_id,
                allow_eager_await,
                ..
            } => {
                if object_id.is_some() {
                    return Err("host methods are not supported".to_owned());
                }
                if allow_eager_await {
                    return Err("asynchronous host calls are not supported".to_owned());
                }
                let answer = match function_name.as_str() {
                    "tool" => {
                        let (name, arguments) = tool_arguments(&args)?;
                        host.call(&name, arguments).await?
                    }
                    "__rig_xonsh_native" => {
                        let pipeline = native_arguments(&args)?;
                        host.native(pipeline).await?
                    }
                    _ => return Err("unsupported interpreter host function".to_owned()),
                };
                let answer = values::from_json(&answer, MAX_JSON_BYTES)?;
                session
                    .resume(ResumeValue::Return(answer), &mut on_print)
                    .await
            }
            TurnEvent::OsCall { .. } => {
                // Do not service mounts, imports, environment or filesystem
                // operations. Native work must be a separately gated command effect.
                session.resume(ResumeValue::NotHandled, &mut on_print).await
            }
            TurnEvent::NameLookup { .. } => {
                session
                    .resume_name_lookup(NameLookupResult::Undefined, &mut on_print)
                    .await
            }
            TurnEvent::ResolveFutures { .. } => {
                return Err("asynchronous Python host futures are not supported".to_owned());
            }
            TurnEvent::Complete(value) => {
                let value = values::to_json(value.as_ref(), MAX_JSON_BYTES)?;
                session.finish().await.map_err(|error| error.to_string())?;
                return Ok(value);
            }
        }
        .map_err(|error| error.to_string())?;
    }
}

fn native_arguments(args: &CallArgs) -> Result<Vec<Vec<String>>, String> {
    if args.args().len() != 1 || args.kwargs().len() != 0 {
        return Err("the native xonsh host operation requires one argv pipeline".to_owned());
    }
    let pipeline = args.arg(0).ok_or("native argv pipeline is missing")?;
    let pipeline = values::to_json(pipeline, MAX_JSON_BYTES)?;
    serde_json::from_value(pipeline)
        .map_err(|_| "native pipeline must contain lists of string arguments".to_owned())
}

fn tool_arguments(args: &CallArgs) -> Result<(String, Value), String> {
    if args.args().len() != 2 || args.kwargs().len() != 0 {
        return Err(
            "tool requires exactly two positional arguments: name and argument object".to_owned(),
        );
    }
    let name = args
        .arg(0)
        .and_then(|value| value.as_str())
        .ok_or("tool name must be a string")?
        .to_owned();
    let arguments = args.arg(1).ok_or("tool arguments are missing")?;
    if arguments.type_name() != "dict" {
        return Err("tool arguments must be a JSON object".to_owned());
    }
    let arguments = values::to_json(arguments, MAX_JSON_BYTES)?;
    // The owned name and JSON outlive the borrowed Monty call across host await.
    Ok((name, arguments))
}

#[derive(Default)]
struct CapturedOutput {
    stdout: String,
    stderr: String,
    bytes: usize,
    truncated: bool,
}

impl CapturedOutput {
    fn push(&mut self, stream: PrintStream, text: &str) {
        let mut keep = text.len().min(MAX_PRINT_BYTES - self.bytes);
        while !text.is_char_boundary(keep) {
            keep -= 1;
        }
        match stream {
            PrintStream::Stdout => self.stdout.push_str(&text[..keep]),
            PrintStream::Stderr => self.stderr.push_str(&text[..keep]),
        }
        self.bytes += keep;
        self.truncated |= keep != text.len();
    }
}
