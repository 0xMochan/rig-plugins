//! Native execution for trusted hosts. Tool authorization belongs to the calling plugin.
//!
//! A [`PreparedCommand`] captures the command before review. Execution consumes that
//! value, never reparsing tool arguments or retrying with broader permissions.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Serialize;
use thiserror::Error;

mod policy;
mod runner;
pub use runner::RunningCommand;

const CODEX_VERSION: &str = "codex-cli 0.142.0";
const MAX_SCRIPT_BYTES: usize = 64 * 1024;
const MAX_TIMEOUT_SECS: u64 = 3600;

// Shell initialization, loader hooks, credentials and agent configuration are
// deliberately absent. Values are captured once, not reread after review.
const ENVIRONMENT_KEYS: &[&str] = &[
    "PATH",
    "HOME",
    "USER",
    "LOGNAME",
    "TMPDIR",
    "LANG",
    "LC_ALL",
    "LC_CTYPE",
    "CARGO_HOME",
    "RUSTUP_HOME",
    "VIRTUAL_ENV",
    "UV_CACHE_DIR",
    "XDG_CACHE_HOME",
];

/// Which authority the caller has granted for this particular prepared command.
///
/// This is an API for trusted plugins, not an authorization token. A tool plugin
/// must obtain and bind its review decision before selecting `Escalated`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionMode {
    /// Run through the pinned Codex workspace sandbox, with networking disabled.
    Sandboxed,
    /// Run with the current OS user's authority, after the caller's approval.
    Escalated,
}

/// The complete, immutable execution input supplied to the permission reviewer.
///
/// There is no deserializer or public field mutation: tool JSON must pass through
/// [`NativeExecutor::prepare`]. Clone only when a caller needs the same value for
/// review bookkeeping and execution; borrowed getters avoid incidental copies.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PreparedCommand {
    script: String,
    workspace: PathBuf,
    cwd: PathBuf,
    environment: BTreeMap<String, String>,
    timeout_secs: u64,
    sandbox_executable: PathBuf,
}

impl PreparedCommand {
    pub fn script(&self) -> &str {
        &self.script
    }

    pub fn workspace(&self) -> &Path {
        &self.workspace
    }

    pub fn cwd(&self) -> &Path {
        &self.cwd
    }

    pub fn environment(&self) -> &BTreeMap<String, String> {
        &self.environment
    }

    pub fn timeout_secs(&self) -> u64 {
        self.timeout_secs
    }

    pub fn sandbox_executable(&self) -> &Path {
        &self.sandbox_executable
    }
}

/// A pinned executable, shared cheaply with the native-shell observer.
#[derive(Clone, Debug)]
pub struct NativeExecutor {
    codex: PathBuf,
}

#[derive(Debug, Error)]
pub enum CommandError {
    #[error("invalid native command: {0}")]
    Invalid(String),
    #[error("could not prepare or launch native command: {0}")]
    Io(#[from] std::io::Error),
    #[error("could not {operation}: {source}")]
    Process {
        operation: &'static str,
        #[source]
        source: std::io::Error,
    },
    #[error("native execution failed: {0}")]
    Execution(String),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandStatus {
    Exited(i32),
    Signaled,
    TimedOut,
    Stopped,
}

/// Combined stdout/stderr, limited to 64 KiB while the process is still running.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct CommandOutput {
    pub output: String,
    pub status: CommandStatus,
    pub truncated: bool,
}

impl NativeExecutor {
    /// Validate an explicitly configured absolute executable. PATH lookup is not
    /// used for the sandbox launcher, and unsupported versions fail closed.
    pub fn new(codex: impl AsRef<Path>) -> Result<Self, CommandError> {
        if !cfg!(any(target_os = "macos", target_os = "linux")) {
            return Err(CommandError::Invalid(
                "this adapter currently supports macOS and Linux".to_owned(),
            ));
        }
        let codex = codex.as_ref();
        if !codex.is_absolute() {
            return Err(CommandError::Invalid(
                "codex must be an absolute executable path".to_owned(),
            ));
        }
        let codex = codex.canonicalize()?;
        let version = Command::new(&codex).arg("--version").env_clear().output()?;
        if !version.status.success()
            || String::from_utf8_lossy(&version.stdout).trim() != CODEX_VERSION
        {
            return Err(CommandError::Invalid(format!(
                "sandbox adapter requires {CODEX_VERSION}; {} exited with {}. stdout: {:?}; stderr: {:?}. Configure a trusted native Codex executable; scripts that require PATH runtimes cannot start with the cleared environment",
                codex.display(),
                version.status,
                String::from_utf8_lossy(&version.stdout).trim(),
                String::from_utf8_lossy(&version.stderr).trim(),
            )));
        }
        Ok(Self { codex })
    }

    /// The canonical executable whose compatibility was checked at construction.
    pub fn executable(&self) -> &Path {
        &self.codex
    }

    /// Resolve directory symlinks, confine the starting directory, and freeze the
    /// script/environment/timeout before a reviewer sees the command.
    ///
    /// `cwd` may be absolute or relative to `workspace`. Confinement describes the
    /// starting directory; only the OS sandbox constrains script filesystem writes.
    pub fn prepare(
        &self,
        workspace: &Path,
        cwd: &Path,
        script: String,
        timeout_secs: u64,
    ) -> Result<PreparedCommand, CommandError> {
        if script.is_empty() || script.len() > MAX_SCRIPT_BYTES || script.contains('\0') {
            return Err(CommandError::Invalid(
                "script must contain 1..=65536 bytes and no NUL".to_owned(),
            ));
        }
        if !(1..=MAX_TIMEOUT_SECS).contains(&timeout_secs) {
            return Err(CommandError::Invalid(
                "timeout_secs must be between 1 and 3600".to_owned(),
            ));
        }
        let workspace = workspace.canonicalize()?;
        let cwd = workspace.join(cwd).canonicalize()?;
        if !workspace.is_dir() || !cwd.is_dir() || !cwd.starts_with(&workspace) {
            return Err(CommandError::Invalid(
                "cwd must resolve to a directory inside the workspace".to_owned(),
            ));
        }
        let mut environment: BTreeMap<String, String> = ENVIRONMENT_KEYS
            .iter()
            .filter_map(|&key| std::env::var(key).ok().map(|value| (key.to_owned(), value)))
            .collect();
        // Codex's empty include_only means "no filter", so keep one explicit
        // key even when the host starts with an empty environment.
        environment
            .entry("PATH".to_owned())
            .or_insert_with(|| "/usr/bin:/bin:/usr/sbin:/sbin".to_owned());
        // Native compilers still need temporary files. Use an existing writable
        // directory instead of granting the host's shared temporary directory.
        let temporary = workspace
            .to_str()
            .ok_or_else(|| CommandError::Invalid("workspace must be a UTF-8 path".to_owned()))?;
        environment.insert("TMPDIR".to_owned(), temporary.to_owned());
        Ok(PreparedCommand {
            script,
            workspace,
            cwd,
            environment,
            timeout_secs,
            sandbox_executable: self.codex.clone(),
        })
    }

    /// Run exactly this prepared value once. A failed sandbox command remains a
    /// failure; permission escalation is a separate tool call and review.
    ///
    /// Dropping this future asks its bounded pipe-draining worker to kill and reap
    /// the process group. No global `Processes` resource is modified.
    pub async fn execute(
        &self,
        command: PreparedCommand,
        mode: ExecutionMode,
    ) -> Result<CommandOutput, CommandError> {
        self.start(command, mode)?.wait().await
    }

    /// Validate and spawn without yielding. A tool observer can consume its
    /// action-bound approval immediately before this call, then schedule `wait`.
    pub fn start(
        &self,
        command: PreparedCommand,
        mode: ExecutionMode,
    ) -> Result<RunningCommand, CommandError> {
        if command.sandbox_executable != self.codex {
            return Err(CommandError::Invalid(
                "prepared command belongs to a different sandbox executable".to_owned(),
            ));
        }
        if command.cwd.canonicalize()? != command.cwd
            || command.workspace.canonicalize()? != command.workspace
        {
            return Err(CommandError::Invalid(
                "prepared directory changed during review".to_owned(),
            ));
        }
        // Codex must not read the user's own account/configuration. No login or
        // model call is needed to use its sandbox launcher.
        let home = tempfile::Builder::new().prefix("rig-command-").tempdir()?;
        RunningCommand::start(
            launch_command(&command, mode, home.path())?,
            home,
            command.timeout_secs,
        )
    }
}

/// Clear the environment on the first exec: an `env -i` child would be too late
/// to prevent inherited loader hooks from executing inside the wrapper itself.
fn launch_command(
    command: &PreparedCommand,
    mode: ExecutionMode,
    home: &Path,
) -> Result<Command, CommandError> {
    let program = match mode {
        ExecutionMode::Sandboxed => command.sandbox_executable.as_path(),
        ExecutionMode::Escalated => Path::new("/bin/bash"),
    };
    let mut process = Command::new(program);
    process
        .env_clear()
        .envs(&command.environment)
        .current_dir(&command.cwd);
    let mut args: Vec<OsString> = Vec::new();
    if mode == ExecutionMode::Sandboxed {
        process.env("CODEX_HOME", home);
        args.extend(policy::sandbox_args(command)?);
        args.push("--".into());
        args.push("/bin/bash".into());
    }
    // Codex uses workspace as its policy/execution cwd. The fixed bash wrapper
    // restores the separately reviewed starting directory via a positional arg.
    // No user text is interpolated into this wrapper.
    args.extend([
        "--noprofile".into(),
        "--norc".into(),
        "-c".into(),
        "cd -- \"$1\" && exec /bin/bash --noprofile --norc -c \"$2\"".into(),
        "rig-command".into(),
        command.cwd.as_os_str().to_owned(),
        command.script.clone().into(),
    ]);
    process.args(args);
    Ok(process)
}

#[cfg(test)]
mod tests;
