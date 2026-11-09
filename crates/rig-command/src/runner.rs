//! Concrete native process ownership and bounded pipe draining.

use std::io::Read;
use std::os::fd::AsFd;
use std::os::unix::process::CommandExt;
use std::process::{Child, ChildStderr, ChildStdout, Command, ExitStatus, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use rustix::fs::{OFlags, fcntl_getfl, fcntl_setfl};
use rustix::process::{
    Pid, Signal, WaitId, WaitIdOptions, kill_process, kill_process_group, waitid,
};

use crate::{CommandError, CommandOutput, CommandStatus};

const OUTPUT_BYTES: usize = 64 * 1024;
const PROCESS_POLL: Duration = Duration::from_millis(10);

/// The worker owns the leader/pipes; this value owns cancellation and its result.
/// Dropping it, including before `wait` is polled, asks the worker to stop.
#[must_use = "dropping the command cancels its process; call wait to observe completion"]
#[derive(Debug)]
pub struct RunningCommand {
    result: futures_channel::oneshot::Receiver<Result<CommandOutput, CommandError>>,
    cancellation: CancelOnDrop,
}

impl RunningCommand {
    pub(crate) fn start(
        command: Command,
        home: tempfile::TempDir,
        timeout_secs: u64,
    ) -> Result<Self, CommandError> {
        let process = spawn(command)?;
        let (sender, result) = futures_channel::oneshot::channel();
        let cancelled = Arc::new(AtomicBool::new(false));
        let cancellation = CancelOnDrop(Arc::clone(&cancelled));
        // Spawn immediately: the deadline and cancellation cannot depend on an
        // async executor polling `wait` after the OS process has already started.
        std::thread::Builder::new()
            .name("rig-native-command".to_owned())
            .spawn(move || {
                let _home = home;
                let _ = sender.send(collect_output(process, timeout_secs, &cancelled));
            })?;
        Ok(Self {
            result,
            cancellation,
        })
    }

    /// Wait for completion; dropping this future requests process-group cleanup.
    ///
    /// # Errors
    /// Returns a process error if the worker cannot drain, signal, or reap its child.
    pub async fn wait(self) -> Result<CommandOutput, CommandError> {
        let Self {
            result,
            cancellation,
        } = self;
        let _cancel = cancellation;
        result.await.map_err(|_| {
            CommandError::Execution("native command worker stopped without a result".to_owned())
        })?
    }
}

#[derive(Debug)]
struct CancelOnDrop(Arc<AtomicBool>);

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

/// No code reaps this leader until its group has been killed. `waitid(NOWAIT)`
/// observes exit while the zombie still reserves the process/group identifier.
struct ChildOwner {
    child: Option<Child>,
    pid: Pid,
}

impl ChildOwner {
    fn exited(&self) -> Result<bool, CommandError> {
        Ok(waitid(
            WaitId::Pid(self.pid),
            WaitIdOptions::EXITED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT,
        )
        .map_err(|source| CommandError::Process {
            operation: "observe the native command's exit",
            source: source.into(),
        })?
        .is_some())
    }

    fn kill_and_reap(&mut self) -> Result<ExitStatus, CommandError> {
        if let Err(error) = kill_process_group(self.pid, Signal::KILL)
            && error != rustix::io::Errno::SRCH
            // Darwin returns EPERM when the group contains only an unreaped
            // zombie. A live same-user descendant remains signalable. The
            // leader is still reserved until wait below, so no reused ID is hit.
            && !(cfg!(target_os = "macos")
                && error == rustix::io::Errno::PERM
                && self.exited()?)
        {
            return Err(CommandError::Process {
                operation: "stop the native command's process group",
                source: error.into(),
            });
        }
        // If the leader deliberately changed groups, kill its PID as well. It
        // remains ours and unreaped, so neither signal targets a recycled PID.
        if let Err(error) = kill_process(self.pid, Signal::KILL)
            && error != rustix::io::Errno::SRCH
        {
            return Err(CommandError::Process {
                operation: "stop the native command's leader",
                source: error.into(),
            });
        }
        let status = self
            .child
            .as_mut()
            .expect("child is reaped exactly once")
            .wait()?;
        self.child = None;
        Ok(status)
    }
}

impl Drop for ChildOwner {
    fn drop(&mut self) {
        if self.child.is_some() {
            let _ = self.kill_and_reap();
        }
    }
}

struct CapturedProcess {
    owner: ChildOwner,
    stdout: ChildStdout,
    stderr: ChildStderr,
    started: Instant,
}

fn spawn(mut command: Command) -> Result<CapturedProcess, CommandError> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    let started = Instant::now();
    let child = command.spawn()?;
    let pid = Pid::from_raw(child.id() as i32).expect("a spawned child has a nonzero PID");
    let mut owner = ChildOwner {
        child: Some(child),
        pid,
    };
    let child = owner.child.as_mut().expect("child was just spawned");
    let stdout = child
        .stdout
        .take()
        .expect("stdout was configured as a pipe");
    let stderr = child
        .stderr
        .take()
        .expect("stderr was configured as a pipe");
    make_nonblocking(&stdout)?;
    make_nonblocking(&stderr)?;
    Ok(CapturedProcess {
        owner,
        stdout,
        stderr,
        started,
    })
}

fn make_nonblocking(pipe: &impl AsFd) -> Result<(), CommandError> {
    let flags = fcntl_getfl(pipe).map_err(std::io::Error::from)?;
    fcntl_setfl(pipe, flags | OFlags::NONBLOCK).map_err(std::io::Error::from)?;
    Ok(())
}

#[derive(Default)]
struct CapturedOutput {
    bytes: Vec<u8>,
    truncated: bool,
}

impl CapturedOutput {
    fn push(&mut self, bytes: &[u8]) {
        let keep = bytes.len().min(OUTPUT_BYTES - self.bytes.len());
        self.bytes.extend_from_slice(&bytes[..keep]);
        self.truncated |= keep < bytes.len();
    }

    fn finish(self, status: CommandStatus) -> CommandOutput {
        let mut output = String::from_utf8_lossy(&self.bytes).into_owned();
        let mut keep = output.len().min(OUTPUT_BYTES);
        while !output.is_char_boundary(keep) {
            keep -= 1;
        }
        let truncated = self.truncated || keep < output.len();
        output.truncate(keep);
        CommandOutput {
            output,
            status,
            truncated,
        }
    }
}

/// Drain at most 128 KiB per pipe per turn so an output flood cannot starve the
/// deadline/cancellation check or the other stream. Discard excess immediately;
/// there are no channels or producer-side output queues.
fn drain(pipe: &mut impl Read, output: &mut CapturedOutput) -> Result<bool, CommandError> {
    let mut buffer = [0; 8192];
    for _ in 0..16 {
        match pipe.read(&mut buffer) {
            Ok(0) => return Ok(true),
            Ok(count) => output.push(&buffer[..count]),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => return Ok(false),
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error.into()),
        }
    }
    Ok(false)
}

fn collect_output(
    mut process: CapturedProcess,
    timeout_secs: u64,
    cancelled: &AtomicBool,
) -> Result<CommandOutput, CommandError> {
    let deadline = process.started + Duration::from_secs(timeout_secs);
    let mut output = CapturedOutput::default();
    let reason = loop {
        if cancelled.load(Ordering::Acquire) {
            break Some(CommandStatus::Stopped);
        }
        if Instant::now() >= deadline {
            break Some(CommandStatus::TimedOut);
        }
        if process.owner.exited()? {
            break None;
        }
        drain(&mut process.stdout, &mut output)?;
        drain(&mut process.stderr, &mut output)?;
        std::thread::sleep(PROCESS_POLL);
    };
    let status = process.owner.kill_and_reap()?;
    // Same-group descendants are now stopped. A child that deliberately escaped
    // the group may still hold a pipe; never let it keep this worker alive.
    let drain_deadline = Instant::now() + Duration::from_millis(100);
    loop {
        let stdout_closed = drain(&mut process.stdout, &mut output)?;
        let stderr_closed = drain(&mut process.stderr, &mut output)?;
        if (stdout_closed && stderr_closed) || Instant::now() >= drain_deadline {
            break;
        }
        std::thread::sleep(PROCESS_POLL);
    }
    let status = reason.unwrap_or_else(|| {
        status
            .code()
            .map_or(CommandStatus::Signaled, CommandStatus::Exited)
    });
    Ok(output.finish(status))
}

#[cfg(test)]
mod tests;
