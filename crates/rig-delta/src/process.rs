//! Owned direct CLI processes for an explicitly approved Delta connection.
//!
//! This is process mechanics, not approval policy. The caller must obtain the
//! host's explicit permission for the immutable request before calling `start`.
//! No shell, retry, escalation, or write-after-start interface is exposed here.

use std::cell::Cell;
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::os::fd::AsFd;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use rig_harness::prelude::Wake;
use rustix::fs::{OFlags, fcntl_getfl, fcntl_setfl};
use rustix::process::{
    Pid, Signal, WaitId, WaitIdOptions, kill_process, kill_process_group, waitid,
};

const MAX_STDIN_BYTES: usize = 16 * 1024 * 1024;
const MAX_ARGUMENT_BYTES: usize = 64 * 1024;
const MAX_ENVIRONMENT_BYTES: usize = 64 * 1024;
const MAX_LINE_BYTES: usize = 1024 * 1024;
const MAX_QUEUED_BYTES: usize = 8 * 1024 * 1024;
const MAX_QUEUED_LINES: usize = 128;
const STDERR_BYTES: usize = 64 * 1024;
const POLL_INTERVAL: Duration = Duration::from_millis(10);
const FINAL_DRAIN: Duration = Duration::from_millis(200);

/// A bounded one-shot action and a session-owned watch/mount are different
/// lifetimes. A session's startup deadline remains active until validated by
/// the host with `mark_ready`; receiving arbitrary output does not disable it.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Lifetime {
    Command { timeout: Duration },
    Session { startup_timeout: Duration },
}

/// Frozen by the caller before its native confirmation. `environment` is the
/// complete inherited environment, not overrides on top of ambient variables.
pub(crate) struct Request {
    pub(crate) executable: PathBuf,
    pub(crate) args: Vec<String>,
    pub(crate) cwd: PathBuf,
    pub(crate) environment: BTreeMap<String, String>,
    pub(crate) stdin: Option<Vec<u8>>,
    pub(crate) lifetime: Lifetime,
    pub(crate) shutdown_grace: Duration,
}

#[derive(Debug)]
pub(crate) enum Event {
    Line(String),
    Exited {
        code: Option<i32>,
        diagnostics: String,
        stopped: bool,
    },
    Failed {
        error: String,
        diagnostics: String,
    },
}

/// The owner polls events and retains this value until a requested stop has
/// flushed. Dropping it requests bounded cleanup on the existing worker; it
/// never disowns the child or launches a detached replacement.
pub(crate) struct Process {
    // Receiver is !Sync. Its mutex makes the ECS-owned handle Sync; polling
    // uses get_mut(), so the UI never acquires this mutex or waits on a sender.
    lines: Mutex<mpsc::Receiver<String>>,
    terminal: Arc<Mutex<Option<Event>>>,
    control: Arc<Control>,
    queued_bytes: Arc<AtomicUsize>,
    finished: bool,
}

struct Control {
    stopping: AtomicBool,
    ready: AtomicBool,
}

impl Process {
    pub(crate) fn start(request: Request, wake: Wake) -> Result<Self, String> {
        validate(&request)?;
        let captured = spawn(&request)?;
        let (sender, lines) = mpsc::sync_channel(MAX_QUEUED_LINES);
        let terminal = Arc::new(Mutex::new(None));
        let control = Arc::new(Control {
            stopping: AtomicBool::new(false),
            ready: AtomicBool::new(false),
        });
        let queued_bytes = Arc::new(AtomicUsize::new(0));
        let worker_terminal = Arc::clone(&terminal);
        let worker_control = Arc::clone(&control);
        let worker_bytes = Arc::clone(&queued_bytes);
        // The process starts now, not on the first UI poll. If thread creation
        // fails, dropping its captured ChildOwner kills and reaps the child.
        std::thread::Builder::new()
            .name("rig-delta-process".into())
            .spawn(move || {
                // Keep the channel connected until the terminal slot is visible;
                // otherwise a receiver could mistake normal worker completion
                // for a panic in the interval before that publication.
                let keep_connected = sender.clone();
                let result = collect(
                    captured,
                    request,
                    &worker_control,
                    sender,
                    worker_bytes,
                    &wake,
                );
                *worker_terminal
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(result);
                drop(keep_connected);
                wake.wake();
            })
            .map_err(|error| format!("Cannot start Delta process worker: {error}"))?;
        Ok(Self {
            lines: Mutex::new(lines),
            terminal,
            control,
            queued_bytes,
            finished: false,
        })
    }

    /// At most one event, without blocking the ECS thread. A terminal result
    /// follows every already-queued line, including an acknowledged prefix.
    pub(crate) fn try_event(&mut self) -> Option<Event> {
        if self.finished {
            return None;
        }
        let disconnected = match self
            .lines
            .get_mut()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .try_recv()
        {
            Ok(line) => {
                self.queued_bytes.fetch_sub(line.len(), Ordering::AcqRel);
                return Some(Event::Line(line));
            }
            // A worker may enqueue its final line and publish the terminal
            // result immediately after this empty observation. Only channel
            // disconnection proves every queued line has been drained.
            Err(mpsc::TryRecvError::Empty) => return None,
            Err(mpsc::TryRecvError::Disconnected) => true,
        };
        let terminal = self
            .terminal
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        if let Some(terminal) = terminal {
            self.finished = true;
            return Some(terminal);
        }
        if disconnected {
            self.finished = true;
            Some(Event::Failed {
                error: "Delta process worker ended without a terminal result".into(),
                diagnostics: String::new(),
            })
        } else {
            None
        }
    }

    /// This is a protocol-host decision, not a first-byte heuristic. It cannot
    /// revive a cancelled or failed worker.
    pub(crate) fn mark_ready(&self) {
        self.control.ready.store(true, Ordering::Release);
    }

    pub(crate) fn request_stop(&self) {
        self.control.stopping.store(true, Ordering::Release);
    }

    pub(crate) fn is_finished(&self) -> bool {
        self.finished
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        self.control.stopping.store(true, Ordering::Release);
    }
}

fn validate(request: &Request) -> Result<(), String> {
    if !request.executable.is_absolute() || !request.executable.is_file() {
        return Err("Delta executable must be an existing absolute file".into());
    }
    if request
        .executable
        .canonicalize()
        .map_err(|error| format!("Cannot resolve Delta executable: {error}"))?
        != request.executable
    {
        return Err("Delta executable must be the frozen canonical path".into());
    }
    if !request.cwd.is_absolute()
        || !request.cwd.is_dir()
        || request
            .cwd
            .canonicalize()
            .map_err(|error| format!("Cannot resolve Delta directory: {error}"))?
            != request.cwd
    {
        return Err("Delta directory must be the frozen canonical directory".into());
    }
    if request.args.len() > 128
        || request.args.iter().any(|arg| arg.contains('\0'))
        || request.args.iter().map(String::len).sum::<usize>() > MAX_ARGUMENT_BYTES
    {
        return Err("Delta arguments exceed the fixed argv bound or contain NUL".into());
    }
    let environment_bytes = request
        .environment
        .iter()
        .map(|(key, value)| key.len() + value.len())
        .sum::<usize>();
    if environment_bytes > MAX_ENVIRONMENT_BYTES
        || request
            .environment
            .iter()
            .any(|(key, value)| key.is_empty() || key.contains(['\0', '=']) || value.contains('\0'))
    {
        return Err("Delta environment exceeds 64 KiB or contains an invalid entry".into());
    }
    if request
        .stdin
        .as_ref()
        .is_some_and(|input| input.len() > MAX_STDIN_BYTES)
    {
        return Err("Delta one-shot stdin exceeds 16 MiB".into());
    }
    let timeout = match request.lifetime {
        Lifetime::Command { timeout } => timeout,
        Lifetime::Session { startup_timeout } => {
            if request.stdin.is_some() {
                return Err("A Delta watch or mount cannot have stdin".into());
            }
            startup_timeout
        }
    };
    if !(Duration::from_secs(1)..=Duration::from_secs(300)).contains(&timeout) {
        return Err("Delta command/startup deadline must be 1 to 300 seconds".into());
    }
    if request.shutdown_grace.is_zero() || request.shutdown_grace > Duration::from_secs(30) {
        return Err("Delta shutdown grace must be positive and at most 30 seconds".into());
    }
    Ok(())
}

/// Keep the unreaped leader reserving its identity until its process group has
/// been stopped. No `try_wait`/other reaper may release this PID early.
struct ChildOwner {
    child: Option<Child>,
    pid: Pid,
    reserved: Cell<bool>,
}

impl ChildOwner {
    fn exited(&self) -> Result<bool, String> {
        match waitid(
            WaitId::Pid(self.pid),
            WaitIdOptions::EXITED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT,
        ) {
            Ok(status) => Ok(status.is_some()),
            Err(error) => {
                // Another reaper must not normally touch this child. If it did,
                // the PID is no longer reserved and must never be signalled.
                if error == rustix::io::Errno::CHILD {
                    self.reserved.set(false);
                }
                Err(format!("Cannot observe Delta process exit: {error}"))
            }
        }
    }

    fn signal(&self, signal: Signal) -> Result<(), String> {
        if !self.reserved.get() {
            return Err("Delta process identity is no longer reserved".into());
        }
        let group = kill_process_group(self.pid, signal);
        let leader = kill_process(self.pid, signal);
        for (target, result) in [("group", group), ("leader", leader)] {
            if let Err(error) = result {
                let harmless = error == rustix::io::Errno::SRCH
                    || (cfg!(target_os = "macos")
                        && error == rustix::io::Errno::PERM
                        && self.exited()?);
                if !harmless {
                    return Err(format!("Cannot signal Delta {target}: {error}"));
                }
            }
        }
        Ok(())
    }

    fn reap(&mut self) -> Result<ExitStatus, String> {
        if !self.reserved.get() {
            self.child = None;
            return Err("Delta process was reaped outside its owner".into());
        }
        self.signal(Signal::KILL)?;
        let status = self
            .child
            .as_mut()
            .expect("child is reaped once")
            .wait()
            .map_err(|error| format!("Cannot reap Delta process: {error}"))?;
        self.child = None;
        Ok(status)
    }
}

impl Drop for ChildOwner {
    fn drop(&mut self) {
        if self.child.is_some() {
            let _ = self.reap();
        }
    }
}

struct Captured {
    owner: ChildOwner,
    stdout: ChildStdout,
    stderr: ChildStderr,
    stdin: Option<ChildStdin>,
    started: Instant,
}

fn spawn(request: &Request) -> Result<Captured, String> {
    let started = Instant::now();
    let child = Command::new(&request.executable)
        .args(&request.args)
        .current_dir(&request.cwd)
        .env_clear()
        .envs(&request.environment)
        .stdin(if request.stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .map_err(|error| format!("Cannot start Delta CLI: {error}"))?;
    let pid = Pid::from_raw(child.id() as i32).expect("spawned child has a nonzero PID");
    let mut owner = ChildOwner {
        child: Some(child),
        pid,
        reserved: Cell::new(true),
    };
    let child = owner.child.as_mut().expect("child was just spawned");
    let stdout = child.stdout.take().expect("configured stdout pipe");
    let stderr = child.stderr.take().expect("configured stderr pipe");
    let stdin = child.stdin.take();
    nonblocking(&stdout)?;
    nonblocking(&stderr)?;
    if let Some(stdin) = &stdin {
        nonblocking(stdin)?;
    }
    Ok(Captured {
        owner,
        stdout,
        stderr,
        stdin,
        started,
    })
}

fn nonblocking(pipe: &impl AsFd) -> Result<(), String> {
    let flags = fcntl_getfl(pipe).map_err(|error| format!("Cannot inspect Delta pipe: {error}"))?;
    fcntl_setfl(pipe, flags | OFlags::NONBLOCK)
        .map_err(|error| format!("Cannot make Delta pipe nonblocking: {error}"))
}

struct Output {
    lines: mpsc::SyncSender<String>,
    queued_bytes: Arc<AtomicUsize>,
    pending: Vec<u8>,
    stderr: Vec<u8>,
    wake: Wake,
}

impl Output {
    fn stdout(&mut self, bytes: &[u8]) -> Result<(), String> {
        for part in bytes.split_inclusive(|byte| *byte == b'\n') {
            let terminated = part.last() == Some(&b'\n');
            let part = if terminated {
                &part[..part.len() - 1]
            } else {
                part
            };
            if part.len() > MAX_LINE_BYTES - self.pending.len() {
                return Err("Delta stdout line exceeds 1 MiB; connection paused".into());
            }
            self.pending.extend_from_slice(part);
            if terminated {
                self.line()?;
            }
        }
        Ok(())
    }

    fn line(&mut self) -> Result<(), String> {
        if self.pending.last() == Some(&b'\r') {
            self.pending.pop();
        }
        let line = String::from_utf8(std::mem::take(&mut self.pending))
            .map_err(|_| "Delta stdout is not valid UTF-8".to_owned())?;
        let count = line.len();
        // There is one producer. Consumers only subtract after a successful
        // receive, so this reservation bounds actual retained queue bytes.
        let previous = self.queued_bytes.fetch_add(count, Ordering::AcqRel);
        if count > MAX_QUEUED_BYTES.saturating_sub(previous) {
            self.queued_bytes.fetch_sub(count, Ordering::AcqRel);
            return Err("Delta stdout queue exceeds 8 MiB; connection paused".into());
        }
        match self.lines.try_send(line) {
            Ok(()) => {
                self.wake.wake();
                Ok(())
            }
            Err(error) => {
                self.queued_bytes.fetch_sub(count, Ordering::AcqRel);
                match error {
                    mpsc::TrySendError::Full(_) => {
                        Err("Delta stdout queue exceeds 128 lines; connection paused".into())
                    }
                    mpsc::TrySendError::Disconnected(_) => {
                        Err("Delta process owner no longer receives output".into())
                    }
                }
            }
        }
    }

    fn stderr(&mut self, bytes: &[u8]) {
        if bytes.len() >= STDERR_BYTES {
            self.stderr.clear();
            self.stderr
                .extend_from_slice(&bytes[bytes.len() - STDERR_BYTES..]);
        } else {
            let discard = (self.stderr.len() + bytes.len()).saturating_sub(STDERR_BYTES);
            self.stderr.drain(..discard);
            self.stderr.extend_from_slice(bytes);
        }
    }

    fn diagnostics(&self) -> String {
        let mut diagnostics = String::from_utf8_lossy(&self.stderr).into_owned();
        if diagnostics.len() > STDERR_BYTES {
            let mut start = diagnostics.len() - STDERR_BYTES;
            while !diagnostics.is_char_boundary(start) {
                start += 1;
            }
            diagnostics.drain(..start);
        }
        diagnostics
    }
}

/// A pipe gets at most 128 KiB per pass, so floods cannot hide cancellation,
/// stdin progress, the other pipe, or a deadline.
fn drain(
    pipe: &mut impl Read,
    mut bytes: impl FnMut(&[u8]) -> Result<(), String>,
) -> Result<bool, String> {
    let mut buffer = [0; 8192];
    for _ in 0..16 {
        match pipe.read(&mut buffer) {
            Ok(0) => return Ok(true),
            Ok(count) => bytes(&buffer[..count])?,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => return Ok(false),
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => (),
            Err(error) => return Err(format!("Cannot read Delta pipe: {error}")),
        }
    }
    Ok(false)
}

fn write_input(
    pipe: &mut Option<ChildStdin>,
    input: Option<&[u8]>,
    offset: &mut usize,
) -> Result<(), String> {
    let Some(input) = input else {
        return Ok(());
    };
    let Some(stdin) = pipe else {
        return Ok(());
    };
    let end = input.len().min(offset.saturating_add(128 * 1024));
    while *offset < end {
        match stdin.write(&input[*offset..end]) {
            Ok(0) => return Err("Delta stdin closed before its approved input was sent".into()),
            Ok(written) => *offset += written,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => return Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => (),
            Err(error) => return Err(format!("Cannot write approved Delta stdin: {error}")),
        }
    }
    if *offset == input.len() {
        *pipe = None;
    }
    Ok(())
}

fn collect(
    mut process: Captured,
    request: Request,
    control: &Control,
    sender: mpsc::SyncSender<String>,
    queued_bytes: Arc<AtomicUsize>,
    wake: &Wake,
) -> Event {
    let mut output = Output {
        lines: sender,
        queued_bytes,
        pending: Vec::new(),
        stderr: Vec::new(),
        wake: wake.clone(),
    };
    let initial_timeout = match request.lifetime {
        Lifetime::Command { timeout } => timeout,
        Lifetime::Session { startup_timeout } => startup_timeout,
    };
    let initial_deadline = process.started + initial_timeout;
    let mut stopping_at = None;
    let mut requested_stop = false;
    let mut failure = None;
    let mut input_offset = 0;
    loop {
        let now = Instant::now();
        if control.stopping.load(Ordering::Acquire) {
            requested_stop = true;
        }
        let deadline_active = matches!(request.lifetime, Lifetime::Command { .. })
            || !control.ready.load(Ordering::Acquire);
        if failure.is_none() && deadline_active && now >= initial_deadline {
            failure = Some(if matches!(request.lifetime, Lifetime::Command { .. }) {
                "Delta command deadline expired".into()
            } else {
                "Delta startup deadline expired before validated readiness".into()
            });
        }
        if (requested_stop || failure.is_some()) && stopping_at.is_none() {
            if let Err(error) = process.owner.signal(Signal::TERM) {
                failure.get_or_insert(error);
            }
            process.stdin = None;
            stopping_at = Some(now + request.shutdown_grace);
        }
        match process.owner.exited() {
            Ok(true) => break,
            Ok(false) => (),
            Err(error) => {
                failure.get_or_insert(error);
                break;
            }
        }
        if stopping_at.is_some_and(|deadline| now >= deadline) {
            break;
        }
        if let Err(error) = drain(&mut process.stdout, |bytes| {
            if failure.is_some() {
                Ok(())
            } else {
                output.stdout(bytes)
            }
        }) {
            failure.get_or_insert(error);
        }
        if let Err(error) = drain(&mut process.stderr, |bytes| {
            output.stderr(bytes);
            Ok(())
        }) {
            failure.get_or_insert(error);
        }
        if stopping_at.is_none()
            && let Err(error) = write_input(
                &mut process.stdin,
                request.stdin.as_deref(),
                &mut input_offset,
            )
        {
            failure.get_or_insert(error);
        }
        std::thread::sleep(POLL_INTERVAL);
    }
    let status = match process.owner.reap() {
        Ok(status) => Some(status),
        Err(error) => {
            failure.get_or_insert(error);
            None
        }
    };
    // A descendant that escaped the owned group may hold a pipe open. It must
    // not keep this worker alive or turn incomplete protocol into success.
    let drain_deadline = Instant::now() + FINAL_DRAIN;
    loop {
        let stdout_closed = match drain(&mut process.stdout, |bytes| {
            if failure.is_some() {
                Ok(())
            } else {
                output.stdout(bytes)
            }
        }) {
            Ok(closed) => closed,
            Err(error) => {
                failure.get_or_insert(error);
                true
            }
        };
        let stderr_closed = match drain(&mut process.stderr, |bytes| {
            output.stderr(bytes);
            Ok(())
        }) {
            Ok(closed) => closed,
            Err(error) => {
                failure.get_or_insert(error);
                true
            }
        };
        if stdout_closed && stderr_closed {
            break;
        }
        if Instant::now() >= drain_deadline {
            failure.get_or_insert("Delta pipe remained open after the owned group stopped".into());
            break;
        }
        std::thread::sleep(POLL_INTERVAL);
    }
    if failure.is_none()
        && !output.pending.is_empty()
        && let Err(error) = output.line()
    {
        failure = Some(error);
    }
    if !requested_stop
        && failure.is_none()
        && request
            .stdin
            .as_ref()
            .is_some_and(|input| input_offset < input.len())
    {
        failure = Some("Delta exited before receiving its complete approved input".into());
    }
    match failure {
        Some(error) => Event::Failed {
            error,
            diagnostics: output.diagnostics(),
        },
        None => Event::Exited {
            code: status.and_then(|status| status.code()),
            diagnostics: output.diagnostics(),
            stopped: requested_stop,
        },
    }
}

#[cfg(test)]
mod tests;
