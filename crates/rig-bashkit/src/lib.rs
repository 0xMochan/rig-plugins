//! Stateless shell calculations over explicitly supplied text files.
//!
//! Each call owns a fresh Bashkit interpreter and an in-memory filesystem.
//! Nothing mounts the project, inherits the host environment, or resolves a
//! command to a native executable. Use the native shell tool for project work.

use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use bashkit::{Bash, ExecutionLimits, FileSystem, FsLimits, InMemoryFs};
use rig_harness::prelude::{
    App, AppToolsExt, Plugin, TokioRuntime, Tool, ToolContext, ToolExecutionError, ToolOptions,
    Touches, args_schema,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

const WORKSPACE: &str = "/workspace";
const MAX_SCRIPT_BYTES: usize = 64 * 1024;
const MAX_INPUT_FILES: usize = 64;
const MAX_INPUT_BYTES: usize = 1024 * 1024;
const MAX_OUTPUT_BYTES: usize = 64 * 1024;
const MAX_COMMANDS: usize = 1_000;
const TIMEOUT: Duration = Duration::from_secs(5);

/// Registers `virtual_bash`, an independent calculation with no host effects.
#[derive(Default, Debug)]
pub struct BashkitPlugin;

impl Plugin for BashkitPlugin {
    fn build(&self, app: &mut App) {
        let runtime = app.world().get_resource::<TokioRuntime>().cloned();
        app.add_tool_with(
            VirtualBash(runtime),
            ToolOptions {
                footprint: Some(Touches::Independent),
            },
        );
    }
}

/// One virtual execution. State and generated files are discarded afterward.
#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Request {
    /// Bash source, at most 64 KiB. Only Bashkit's embedded commands exist.
    pub script: String,
    /// UTF-8 files to create under /workspace; at most 64 files and 1 MiB total.
    #[serde(default)]
    pub files: Vec<InputFile>,
}

/// A text file supplied by value, never read from the host.
#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct InputFile {
    /// Relative slash-separated path, such as data/items.json; no . or .. components.
    pub path: String,
    /// UTF-8 content copied into the virtual filesystem.
    pub content: String,
}

/// Text presentation of shell output. Truncation is always reported.
#[derive(Debug, Serialize)]
pub struct Output {
    /// Standard output, decoded with replacement for invalid UTF-8 bytes.
    pub stdout: String,
    /// Standard error, decoded with replacement for invalid UTF-8 bytes.
    pub stderr: String,
    /// Shell exit status; nonzero is an ordinary command failure.
    pub exit_code: i32,
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
}

/// Executes the same isolated boundary used by the Rig tool.
///
/// The caller must provide a Tokio runtime. Files and shell state last for this
/// call only, and no process, network, or host filesystem capability is added.
/// Limits are fixed by the host; the script cannot request larger ones.
///
/// # Errors
///
/// Returns an argument error for oversized or ambiguous inputs, or an execution
/// error when Bashkit fails, including its work, memory and deadline limits.
pub async fn execute(request: &Request) -> Result<Output, ToolExecutionError> {
    validate(request)?;

    let fs = Arc::new(InMemoryFs::with_limits(
        FsLimits::new()
            .max_total_bytes(4 * 1024 * 1024)
            .max_file_size(MAX_INPUT_BYTES as u64)
            .max_file_count(256)
            .max_dir_count(256),
    ));
    fs.mkdir(Path::new(WORKSPACE), true)
        .await
        .map_err(execution_error)?;
    for file in &request.files {
        let path = Path::new(WORKSPACE).join(&file.path);
        // Validation guarantees at least one relative component, so a parent
        // exists. Fallible writes keep quota errors from silently dropping data.
        if let Some(parent) = path.parent() {
            fs.mkdir(parent, true).await.map_err(execution_error)?;
        }
        fs.write_file(&path, file.content.as_bytes())
            .await
            .map_err(execution_error)?;
    }

    let mut bash = Bash::builder()
        .fs(fs)
        .cwd(WORKSPACE)
        .username("user")
        .hostname("virtual")
        .limits(
            ExecutionLimits::new()
                .max_input_bytes(MAX_SCRIPT_BYTES)
                .max_commands(MAX_COMMANDS)
                .max_stdout_bytes(MAX_OUTPUT_BYTES)
                .max_stderr_bytes(MAX_OUTPUT_BYTES)
                .timeout(TIMEOUT),
        )
        .build();
    let result = bash.exec(&request.script).await.map_err(execution_error)?;
    Ok(Output {
        stdout: result.stdout.text_lossy().into_owned(),
        stderr: result.stderr.text_lossy().into_owned(),
        exit_code: result.exit_code,
        stdout_truncated: result.stdout_truncated,
        stderr_truncated: result.stderr_truncated,
    })
}

fn validate(request: &Request) -> Result<(), ToolExecutionError> {
    if request.script.len() > MAX_SCRIPT_BYTES {
        return Err(ToolExecutionError::invalid_args("script exceeds 64 KiB"));
    }
    if request.files.len() > MAX_INPUT_FILES {
        return Err(ToolExecutionError::invalid_args("more than 64 input files"));
    }
    let mut total_bytes = 0usize;
    let mut paths = HashSet::with_capacity(request.files.len());
    for file in &request.files {
        // Slash is the virtual separator on every host. Refuse Windows path
        // syntax and ambiguous spellings before constructing a host Path.
        if file.path.is_empty()
            || file.path.len() > 1024
            || file.path.contains('\\')
            || file.path.contains(':')
            || file
                .path
                .split('/')
                .any(|part| matches!(part, "" | "." | ".."))
        {
            return Err(ToolExecutionError::invalid_args(
                "input paths must be at most 1024 bytes, relative, slash-separated, and contain \
                 no empty, . or .. components, backslashes, or colons",
            ));
        }
        if !paths.insert(file.path.as_str()) {
            return Err(ToolExecutionError::invalid_args(
                "duplicate input file path",
            ));
        }
        total_bytes = total_bytes
            .checked_add(file.content.len())
            .filter(|&total| total <= MAX_INPUT_BYTES)
            .ok_or_else(|| ToolExecutionError::invalid_args("input files exceed 1 MiB"))?;
    }
    Ok(())
}

fn execution_error(error: bashkit::Error) -> ToolExecutionError {
    ToolExecutionError::other(format!("virtual bash failed: {error}"))
}

struct VirtualBash(Option<TokioRuntime>);

impl Tool for VirtualBash {
    const NAME: &'static str = "virtual_bash";
    type Args = Request;
    type Output = Output;
    type Error = ToolExecutionError;

    fn description(&self) -> String {
        "Run a short Bash calculation in a fresh in-memory filesystem. Supply text files \
         explicitly; they appear under /workspace. This cannot read or change project files, \
         run native executables (cargo, git, uv, npm), access the network, or inherit host \
         environment variables. Use the native shell tool for project work. State and files \
         are discarded after each call; print results to stdout. Limits: 64 KiB script, \
         64 input files totaling 1 MiB, 1,000 commands, 5 seconds, 64 KiB captured raw bytes \
         per output stream."
            .to_owned()
    }

    fn parameters(&self) -> Value {
        args_schema::<Request>()
    }

    async fn call(
        &self,
        _: &mut ToolContext,
        request: Request,
    ) -> Result<Output, ToolExecutionError> {
        let runtime = self.0.as_ref().ok_or_else(|| {
            ToolExecutionError::other("virtual_bash requires Rig's TokioRuntime service")
        })?;
        // Rig polls tools on Bevy's pool. Bashkit needs Tokio's timer and
        // blocking parser pool, so run on the host's existing runtime.
        let mut execution = ExecutionTask(runtime.0.spawn(async move { execute(&request).await }));
        (&mut execution.0).await.map_err(|error| {
            ToolExecutionError::other(format!("virtual bash task failed: {error}"))
        })?
    }
}

/// A dropped Rig tool future must cancel its Tokio task, not detach it.
struct ExecutionTask(tokio::task::JoinHandle<Result<Output, ToolExecutionError>>);

impl Drop for ExecutionTask {
    fn drop(&mut self) {
        self.0.abort();
    }
}

#[cfg(test)]
mod tests;
