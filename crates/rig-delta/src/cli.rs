//! Typed, finite CLI surface and argument-safe transport through native_shell.

use rig_harness::rig_core::tool::ToolResult;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub(crate) const SOURCE: &str = "rig";
pub(crate) const CONTRACT: u64 = 8;
const MAX_ARGUMENT_BYTES: usize = 8 * 1024;

#[derive(Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct DeltaArgs {
    pub command: Command,
    #[serde(default)]
    pub execution: Execution,
}

#[derive(Clone, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct Execution {
    /// Request separate review of this invocation's exact CLI commands outside the sandbox.
    #[serde(default)]
    pub escalate: bool,
    /// Required for escalation. A failed call never retries or escalates automatically.
    pub justification: Option<String>,
    /// Per CLI command deadline, from 1 to 300 seconds (default 60).
    pub timeout_secs: Option<u64>,
}

impl Execution {
    pub fn validate(&self) -> Result<(), String> {
        if self
            .timeout_secs
            .is_some_and(|seconds| !(1..=300).contains(&seconds))
        {
            return Err("timeout_secs must be between 1 and 300".into());
        }
        if self.escalate
            && self
                .justification
                .as_ref()
                .is_none_or(|text| text.trim().is_empty())
        {
            return Err("escalation requires a justification".into());
        }
        Ok(())
    }
}

/// No arbitrary arguments, installation, deletion, watch, or background mounts.
#[derive(Clone, Deserialize, JsonSchema)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum Command {
    Contract,
    AuthStatus,
    OrgList,
    OrgCurrent,
    ThreadList,
    ThreadSearch {
        #[serde(default)]
        terms: Vec<String>,
        scope: Option<SearchScope>,
        edited_path: Option<String>,
        limit: Option<u16>,
    },
    ThreadShow {
        thread: String,
        #[serde(default)]
        format: ShowFormat,
        /// Inclusive 1-based range, at most 20 messages; required for messages format.
        messages: Option<MessageRange>,
    },
    ThreadStatus {
        thread: String,
    },
    ThreadCreate {
        /// If present, queues a PRIVATE upload to this organization. Requires confirm_upload.
        organization: Option<String>,
        #[serde(default)]
        confirm_upload: bool,
    },
    ThreadShare {
        thread: String,
        /// Explicit consent to publish the thread AND attached files to the whole organization.
        confirm_upload: bool,
    },
    ThreadInvite {
        thread: String,
        emails: Vec<String>,
        /// Uploads privately if needed; existing broader access is not narrowed.
        confirm_upload: bool,
    },
    ThreadUnshare {
        thread: String,
    },
    ThreadFork {
        thread: String,
        name: Option<String>,
    },
    WorktreeList {
        thread: Option<String>,
    },
    /// Records the current checkout only; does not upload or start file syncing.
    WorktreeCreate,
    /// Attaches an existing worktree only; does not mount or sync files.
    WorktreeAttach {
        reference: String,
        thread: String,
    },
    McpSearch {
        query: String,
    },
}

#[derive(Clone, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SearchScope {
    Messages,
}

#[derive(Clone, Default, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ShowFormat {
    #[default]
    Full,
    Summarize,
    Messages,
}

#[derive(Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct MessageRange {
    start: u64,
    end: u64,
}

impl Command {
    pub fn argv(&self, external_id: &str) -> Result<Vec<String>, String> {
        let mut args: Vec<String> = match self {
            Self::Contract => words(&["thread", "contract"]),
            Self::AuthStatus => words(&["auth", "status"]),
            Self::OrgList => words(&["org", "list"]),
            Self::OrgCurrent => words(&["org", "current"]),
            Self::ThreadList => words(&["thread", "list"]),
            Self::ThreadSearch {
                terms,
                scope,
                edited_path,
                limit,
            } => {
                if terms.is_empty() && edited_path.is_none() {
                    return Err("search needs terms or edited_path".into());
                }
                let mut args = words(&["thread", "search"]);
                if scope.is_some() {
                    args.extend(words(&["--scope", "messages"]));
                }
                if let Some(path) = edited_path {
                    option(&mut args, "--edited-path", path)?;
                }
                if let Some(limit) = limit {
                    if !(1..=100).contains(limit) {
                        return Err("limit must be between 1 and 100".into());
                    }
                    args.extend(["--limit".into(), limit.to_string()]);
                }
                for term in terms {
                    positional(&mut args, term)?;
                }
                args
            }
            Self::ThreadShow {
                thread,
                format,
                messages,
            } => {
                let mut args = words(&["thread", "show"]);
                positional(&mut args, thread)?;
                match format {
                    ShowFormat::Full if messages.is_none() => (),
                    ShowFormat::Summarize if messages.is_none() => {
                        args.extend(words(&["--format", "summarize"]))
                    }
                    ShowFormat::Messages => {
                        let range = messages
                            .as_ref()
                            .ok_or("messages format requires a range")?;
                        if range.start == 0
                            || range.end < range.start
                            || range.end - range.start >= 20
                        {
                            return Err(
                                "message range must be 1-based, inclusive, and at most 20 messages"
                                    .into(),
                            );
                        }
                        args.extend(words(&["--format", "messages", "--messages"]));
                        args.push(format!("{}-{}", range.start, range.end));
                    }
                    _ => return Err("a range is only valid with messages format".into()),
                }
                args
            }
            Self::ThreadStatus { thread } => {
                producer(words(&["thread", "status"]), thread, external_id)?
            }
            Self::ThreadCreate {
                organization,
                confirm_upload,
            } => {
                let mut args = words(&["thread", "create", "--source", SOURCE, "--external-id"]);
                option_value(&mut args, external_id)?;
                if let Some(org) = organization {
                    consent(*confirm_upload)?;
                    option(&mut args, "--organization", org)?;
                    args.extend(words(&["--visibility", "private"]));
                }
                args
            }
            Self::ThreadShare {
                thread,
                confirm_upload,
            } => {
                consent(*confirm_upload)?;
                let mut args = words(&["thread", "share"]);
                positional(&mut args, thread)?;
                args.extend(words(&["--visibility", "organization"]));
                args
            }
            Self::ThreadInvite {
                thread,
                emails,
                confirm_upload,
            } => {
                consent(*confirm_upload)?;
                if emails.is_empty() || emails.len() > 20 {
                    return Err("invite needs 1 to 20 email addresses".into());
                }
                let mut args = words(&["thread", "invite"]);
                positional(&mut args, thread)?;
                for email in emails {
                    if !email.contains('@') || email.chars().any(char::is_whitespace) {
                        return Err("invite requires email addresses".into());
                    }
                    positional(&mut args, email)?;
                }
                args
            }
            Self::ThreadUnshare { thread } => {
                let mut args = words(&["thread", "unshare"]);
                positional(&mut args, thread)?;
                args
            }
            Self::ThreadFork { thread, name } => {
                let mut args = words(&["thread", "fork"]);
                positional(&mut args, thread)?;
                if let Some(name) = name {
                    option(&mut args, "--name", name)?;
                }
                args
            }
            Self::WorktreeList { thread } => {
                let mut args = words(&["worktree", "list"]);
                if let Some(thread) = thread {
                    option(&mut args, "--thread", thread)?;
                }
                args
            }
            Self::WorktreeCreate => words(&["worktree", "create", "."]),
            Self::WorktreeAttach { reference, thread } => {
                let mut args = words(&["worktree", "attach"]);
                positional(&mut args, reference)?;
                option(&mut args, "--thread", thread)?;
                args
            }
            Self::McpSearch { query } => {
                let mut args = words(&["mcp", "search"]);
                positional(&mut args, query)?;
                args
            }
        };
        if args.iter().map(String::len).sum::<usize>() > MAX_ARGUMENT_BYTES {
            return Err("Delta arguments exceed 8 KiB".into());
        }
        args.insert(0, "--json".into());
        Ok(args)
    }
}

pub(crate) fn producer(
    mut args: Vec<String>,
    thread: &str,
    external_id: &str,
) -> Result<Vec<String>, String> {
    positional(&mut args, thread)?;
    args.extend(words(&["--source", SOURCE, "--external-id"]));
    option_value(&mut args, external_id)?;
    Ok(args)
}

pub(crate) fn words(args: &[&str]) -> Vec<String> {
    args.iter().map(|arg| (*arg).to_owned()).collect()
}

fn consent(confirmed: bool) -> Result<(), String> {
    if confirmed {
        Ok(())
    } else {
        Err(
            "ask the user before uploading, then set confirm_upload=true for that explicit request"
                .into(),
        )
    }
}
fn option(args: &mut Vec<String>, name: &str, value: &str) -> Result<(), String> {
    args.push(name.into());
    option_value(args, value)
}
fn option_value(args: &mut Vec<String>, value: &str) -> Result<(), String> {
    // Shell quoting alone does not prevent CLI flag injection.
    if value.trim().is_empty()
        || value.starts_with('-')
        || value.contains('\0')
        || value.len() > MAX_ARGUMENT_BYTES
    {
        return Err(
            "Delta values must be nonblank, at most 8 KiB, contain no NUL, and not start with '-'"
                .into(),
        );
    }
    args.push(value.to_owned());
    Ok(())
}
fn positional(args: &mut Vec<String>, value: &str) -> Result<(), String> {
    option_value(args, value)
}

/// Split pipes before the shared runner combines them. Length framing cannot be
/// forged by a string inside a thread, unlike sentinel-line splitting.
pub(crate) fn script(
    executable: &str,
    argv: &[String],
    input: Option<&str>,
    spool: &str,
) -> String {
    let spool = shell_words::quote(spool);
    let command = std::iter::once(executable)
        .chain(argv.iter().map(String::as_str))
        .map(shell_words::quote)
        .collect::<Vec<_>>()
        .join(" ");
    let input = input
        .map(|input| {
            let mut delimiter = "RIG_DELTA_RECORDS".to_owned();
            while input.lines().any(|line| line == delimiter) {
                delimiter.push('_');
            }
            let newline = if input.ends_with('\n') { "" } else { "\n" };
            format!(" <<'{delimiter}'\n{input}{newline}{delimiter}")
        })
        .unwrap_or_default();
    format!(
        "dir={spool}\n\
         capture() {{ head -c 65537 >\"$1\"; saved=$?; cat >/dev/null || return 1; return \"$saved\"; }}\n\
         mkfifo \"$dir/out.pipe\" \"$dir/err.pipe\" || exit 125\n\
         capture \"$dir/out\" <\"$dir/out.pipe\" &\nout_pid=$!\n\
         capture \"$dir/err\" <\"$dir/err.pipe\" &\nerr_pid=$!\n\
         {command} >\"$dir/out.pipe\" 2>\"$dir/err.pipe\"{input}\ncode=$?\n\
         wait \"$out_pid\"; out_saved=$?\nwait \"$err_pid\"; err_saved=$?\n\
         rm -f -- \"$dir/out.pipe\" \"$dir/err.pipe\" || exit 125\n\
         if [ \"$out_saved\" -ne 0 ] || [ \"$err_saved\" -ne 0 ]; then printf 'Delta output capture failed\\n' >&2; exit 125; fi\n\
         printf 'RIG_DELTA_V1\\n%s\\n%s\\n' \"$(wc -c <\"$dir/out\")\" \"$(wc -c <\"$dir/err\")\"\n\
         cat -- \"$dir/out\" \"$dir/err\"\nexit \"$code\""
    )
}

#[derive(Serialize)]
pub(crate) struct Reply {
    pub exit_code: i64,
    pub stdout: String,
    pub diagnostics: String,
}

impl Reply {
    pub fn from_tool(result: &ToolResult) -> Result<Self, String> {
        if !result.is_success() {
            return Err(result.output().render());
        }
        let native = result
            .output()
            .as_json()
            .ok_or("native_shell did not return JSON")?;
        if native["truncated"] != false {
            return Err(
                "native_shell output was truncated or malformed; no complete Delta result".into(),
            );
        }
        let exit_code = native["status"]["exited"]
            .as_i64()
            .ok_or_else(|| format!("Delta command did not exit normally: {}", native["status"]))?;
        let framed = native["output"]
            .as_str()
            .ok_or("native_shell output is missing")?;
        let mut fields = framed.splitn(4, '\n');
        if fields.next() != Some("RIG_DELTA_V1") {
            return Err(format!("Delta transport failed: {framed}"));
        }
        let out_len = fields
            .next()
            .and_then(|n| n.trim().parse::<usize>().ok())
            .ok_or("invalid Delta stdout length")?;
        let err_len = fields
            .next()
            .and_then(|n| n.trim().parse::<usize>().ok())
            .ok_or("invalid Delta stderr length")?;
        let body = fields.next().ok_or("missing Delta transport body")?;
        if out_len > 65_536 || err_len > 65_536 {
            return Err(
                "Delta exceeded the 64 KiB per-stream spool bound; no complete result".into(),
            );
        }
        if out_len.checked_add(err_len) != Some(body.len()) || !body.is_char_boundary(out_len) {
            return Err("incomplete Delta transport body".into());
        }
        Ok(Self {
            exit_code,
            stdout: body[..out_len].into(),
            diagnostics: body[out_len..].into(),
        })
    }

    pub fn json(&self) -> Result<Value, String> {
        let value: Value = serde_json::from_str(&self.stdout)
            .map_err(|error| format!("invalid Delta JSON: {error}; {}", self.diagnostics))?;
        if self.exit_code != 0 || value.get("error").is_some() {
            return Err(format!(
                "Delta command failed (exit {}): {}; {}",
                self.exit_code, value, self.diagnostics
            ));
        }
        Ok(value)
    }

    pub fn result(&self) -> Value {
        json!({"result": self.stdout, "diagnostics": self.diagnostics, "exit_code": self.exit_code})
    }
}

pub(crate) fn check_contract(value: &Value) -> Result<(), String> {
    let min = value["min"]
        .as_u64()
        .ok_or("Delta contract has no valid min")?;
    let max = value["max"]
        .as_u64()
        .ok_or("Delta contract has no valid max")?;
    if value["operation"] != "contract" || min > max {
        return Err("malformed Delta contract".into());
    }
    if max < CONTRACT {
        return Err("Delta is too old: update Delta (contract 8 required, Delta 0.22.0+)".into());
    }
    if min > CONTRACT {
        return Err(
            "Delta is newer than this plugin: update rig-delta (contract 8 required)".into(),
        );
    }
    Ok(())
}
