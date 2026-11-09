//! Durable historical transcript batches, not a live Delta turn or process owner.
//!
//! The caller checks the CLI contract/access and runs append through ordinary Rig
//! tool gates. This module owns only the local journal and immutable record bytes.

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use rig_harness::prelude::Conversation;
use rig_harness::rig_core::message::{AssistantContent, Message, ToolResultContent, UserContent};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

const JOURNAL_VERSION: u32 = 1;
const MAX_JOURNAL_BYTES: u64 = 8 * 1024 * 1024;
const MAX_BATCH_BYTES: usize = 32 * 1024;
const MAX_BATCH_RECORDS: usize = 64;
const MAX_SEQUENCE: u64 = 9_007_199_254_740_991;

/// One batch owns the cross-process lock until its gated append ends or is cancelled.
/// Dropping it leaves all unacknowledged records on disk.
pub(crate) struct Batch {
    path: PathBuf,
    journal: Journal,
    input: String,
    more: bool,
    _lock: JournalLock,
}

#[derive(Serialize)]
pub(crate) struct Acknowledgment {
    pub(crate) confirmed: usize,
    pub(crate) pending: usize,
    pub(crate) more: bool,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Journal {
    version: u32,
    agent_id: String,
    thread: String,
    next_sequence: u64,
    confirmed_sequence: u64,
    // The last immutable update queued for each stable transcript block. Keeping
    // these after acknowledgment prevents compaction or restore resetting IDs.
    seen: BTreeMap<String, Value>,
    pending: Vec<Record>,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Record {
    #[serde(rename = "schemaVersion")]
    schema_version: u32,
    sequence: u64,
    update: Value,
}

struct JournalLock(PathBuf);

impl Drop for JournalLock {
    fn drop(&mut self) {
        // Never recurse or remove journal data. A process crash deliberately
        // leaves this lock for a person to inspect, rather than stealing it.
        let _ = fs::remove_file(&self.0);
    }
}

/// Prepare and save an exact append batch before it can reach a process.
///
/// `thread` must be the canonical ID returned by status, not a share locator.
/// `remote_next` is that status's producer frontier for source `rig` and this
/// stable agent ID. Missing local state never restarts an existing producer.
pub(crate) fn prepare(
    home: &Path,
    agent_id: &str,
    thread: &str,
    conversation: &Conversation,
    remote_next: u64,
) -> Result<Batch, String> {
    if agent_id.is_empty()
        || agent_id.len() > 128
        || !agent_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err("Delta recording requires a safe, stable Rig agent ID".into());
    }
    if thread.is_empty() || thread.len() > 512 || thread.chars().any(char::is_control) {
        return Err("Delta status returned an invalid canonical thread ID".into());
    }
    let root = home.join("delta");
    private_directory(&root)?;
    let directory = root.join(agent_id);
    private_directory(&directory)?;
    let lock_path = directory.join("recording.lock");
    let mut lock_file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&lock_path)
        .map_err(|error| {
            format!(
                "Cannot lock Delta recording at {}: {error}. Another sync may be active; \
                 after a crash, inspect the lock and confirm no sync is running before removing it.",
                lock_path.display()
            )
        })?;
    let lock = JournalLock(lock_path);
    match fs::symlink_metadata(directory.join("live.json")) {
        Ok(_) => return Err("This producer has migrated to a live Delta journal. Historical delta_sync must not reopen its old sequence frontier; reconnect the live session instead.".into()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
        Err(error) => return Err(format!("Cannot verify the live Delta journal boundary: {error}")),
    }
    writeln!(lock_file, "process={}", std::process::id())
        .and_then(|()| lock_file.sync_all())
        .map_err(|error| format!("Cannot save Delta recording lock: {error}"))?;
    let path = directory.join("outbox.json");
    let mut journal = match fs::symlink_metadata(&path) {
        Ok(metadata) => {
            if !metadata.is_file() || metadata.file_type().is_symlink() {
                return Err("Delta outbox must be a regular file, not a link".into());
            }
            if metadata.len() > MAX_JOURNAL_BYTES {
                return Err(
                    "Delta outbox exceeds its 8 MiB journal limit; recording paused".into(),
                );
            }
            let mut bytes = Vec::new();
            File::open(&path)
                .and_then(|file| file.take(MAX_JOURNAL_BYTES + 1).read_to_end(&mut bytes))
                .map_err(|error| format!("Cannot read Delta outbox: {error}"))?;
            if bytes.len() as u64 > MAX_JOURNAL_BYTES {
                return Err(
                    "Delta outbox exceeds its 8 MiB journal limit; recording paused".into(),
                );
            }
            let journal: Journal = serde_json::from_slice(&bytes).map_err(|error| {
                format!("Invalid Delta outbox; preserved without sending: {error}")
            })?;
            validate_journal(&journal, agent_id, thread, remote_next)?;
            journal
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            if remote_next != 1 {
                return Err(
                    "Delta already has records for this Rig session, but its local outbox is missing. \
                     Restore the matching Rig home or start a new session; do not reset its sequence."
                        .into(),
                );
            }
            Journal {
                version: JOURNAL_VERSION,
                agent_id: agent_id.into(),
                thread: thread.into(),
                next_sequence: 1,
                confirmed_sequence: 0,
                seen: BTreeMap::new(),
                pending: Vec::new(),
            }
        }
        Err(error) => return Err(format!("Cannot inspect Delta outbox: {error}")),
    };

    let replaying = !journal.pending.is_empty();
    let mut input = encode_pending(&journal.pending)?;
    let mut more = false;
    let completed = completed_tools(conversation);
    // Inspect borrowed transcript entries. Only updates admitted into this batch
    // become owned journal records; a later explicit sync picks up the remainder.
    'entries: for entry in conversation.entries() {
        let prefix = format!("{}:{}", entry.made, entry.id);
        match entry.message {
            Message::System { .. } => continue,
            Message::User { content } => {
                for (index, part) in content.iter().enumerate() {
                    let key = format!("{prefix}:{index}");
                    let update = match part {
                        UserContent::ToolResult(result) if !is_delta_tool(result.name.as_str()) => {
                            let output: Vec<Value> = result
                                .content
                                .iter()
                                .map(|part| match part {
                                    ToolResultContent::Text(text) => {
                                        json!({"type":"text", "text":text.text})
                                    }
                                    ToolResultContent::Json { value } => {
                                        json!({"type":"json", "value":value})
                                    }
                                    ToolResultContent::Image(_) => {
                                        json!({"type":"text", "text":"[Image omitted by Rig Delta export.]"})
                                    }
                                })
                                .collect();
                            json!({
                                "sessionUpdate":"tool_call_update",
                                "toolCallId":result.call.to_string(),
                                "status":if result.is_error {"failed"} else {"completed"},
                                "rawOutput":output,
                            })
                        }
                        UserContent::ToolResult(_) => continue,
                        UserContent::Text(text) if entry.is_users_own() => {
                            text_update("user_message", &key, &text.text)
                        }
                        _ if entry.is_users_own() => text_update(
                            "user_message",
                            &key,
                            "[Non-text attachment omitted by Rig Delta export.]",
                        ),
                        _ => continue, // Plugin prompts are not the user's transcript.
                    };
                    if !queue_update(&mut journal, &mut input, &key, update, replaying)? {
                        more = true;
                        break 'entries;
                    }
                }
            }
            Message::Assistant(message) => {
                for (index, part) in message.content.iter().enumerate() {
                    let key = format!("{prefix}:{index}");
                    let update = match part {
                        AssistantContent::Text(text) => {
                            text_update("agent_message", &key, &text.text)
                        }
                        AssistantContent::ToolCall(call)
                            if !is_delta_tool(call.function.name.as_str()) =>
                        {
                            let id = call.id.to_string();
                            let status = match completed.get(&id) {
                                Some(true) => "failed",
                                Some(false) => "completed",
                                None => "in_progress",
                            };
                            json!({
                                "sessionUpdate":"tool_call_update",
                                "toolCallId":id,
                                "name":call.function.name.as_str(),
                                "title":call.function.name.as_str(),
                                "kind":"other",
                                "status":status,
                                "rawInput":call.function.arguments,
                            })
                        }
                        AssistantContent::Image(_) => text_update(
                            "agent_message",
                            &key,
                            "[Image omitted by Rig Delta export.]",
                        ),
                        // Reasoning, opaque provider data and our own transport
                        // calls are never included in this historical export.
                        _ => continue,
                    };
                    if !queue_update(&mut journal, &mut input, &key, update, replaying)? {
                        more = true;
                        break 'entries;
                    }
                }
            }
        }
    }
    save(&path, &journal)?;
    Ok(Batch {
        path,
        journal,
        input,
        more,
        _lock: lock,
    })
}

impl Batch {
    pub(crate) fn input(&self) -> &str {
        &self.input
    }

    pub(crate) fn pending_count(&self) -> usize {
        self.journal.pending.len()
    }

    /// Commit only a strictly matching receipt prefix, even when append exits
    /// nonzero after recording that prefix. No receipt is inferred from exit code.
    pub(crate) fn acknowledge(&mut self, stdout: &str) -> Result<Acknowledgment, String> {
        let mut confirmed = 0;
        let mut failure = None;
        for line in stdout.lines() {
            let Some(expected) = self.journal.pending.get(confirmed) else {
                failure = Some("Delta returned an unexpected extra append receipt".to_owned());
                break;
            };
            let receipt: Value = match serde_json::from_str(line) {
                Ok(receipt) => receipt,
                Err(error) => {
                    failure = Some(format!("Delta returned an invalid append receipt: {error}"));
                    break;
                }
            };
            if let Some(error) = receipt.get("error") {
                failure = Some(format!("Delta append paused: {error}"));
                break;
            }
            let disposition = receipt["disposition"].as_str();
            if receipt["operation"] != "appended"
                || receipt["thread_id"] != self.journal.thread
                || receipt["sequence"].as_u64() != Some(expected.sequence)
                || receipt.get("turn_sequence") != Some(&Value::Null)
                || !receipt["next_sequence"].as_u64().is_some_and(|next| {
                    next > expected.sequence && next <= self.journal.next_sequence
                })
                || !matches!(disposition, Some("recorded" | "duplicate"))
                || receipt["duplicate"].as_bool() != Some(disposition == Some("duplicate"))
            {
                failure = Some(
                    "Delta receipt did not match this historical record; \
                     fenced, changed-thread or malformed receipts cannot acknowledge it"
                        .into(),
                );
                break;
            }
            confirmed += 1;
        }
        if confirmed > 0 {
            self.journal.confirmed_sequence += confirmed as u64;
            self.journal.pending.drain(..confirmed);
            save(&self.path, &self.journal)?;
            self.input = encode_pending(&self.journal.pending)?;
        }
        if !self.journal.pending.is_empty() && failure.is_none() {
            failure = Some("Delta did not acknowledge the complete batch".into());
        }
        if let Some(error) = failure {
            return Err(format!(
                "{error}. Confirmed {confirmed} records; {} remain durable. \
                 No automatic retry was started.",
                self.journal.pending.len()
            ));
        }
        Ok(Acknowledgment {
            confirmed,
            pending: self.journal.pending.len(),
            more: self.more,
        })
    }
}

fn private_directory(path: &Path) -> Result<(), String> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => Ok(()),
        Ok(_) => Err(format!(
            "Delta journal directory must not be a file or link: {}",
            path.display()
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir(path).map_err(|error| {
                format!(
                    "Cannot create Delta journal directory {}: {error}",
                    path.display()
                )
            })?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(|error| {
                    format!(
                        "Cannot protect Delta journal directory {}: {error}",
                        path.display()
                    )
                })?;
            }
            Ok(())
        }
        Err(error) => Err(format!("Cannot inspect Delta journal directory: {error}")),
    }
}

fn validate_journal(
    journal: &Journal,
    agent_id: &str,
    thread: &str,
    remote_next: u64,
) -> Result<(), String> {
    if journal.version != JOURNAL_VERSION
        || journal.agent_id != agent_id
        || journal.thread != thread
    {
        return Err(
            "Delta outbox belongs to a different version, session or thread; \
             preserved without sending. Start a new Rig session to use another thread."
                .into(),
        );
    }
    let valid = journal.next_sequence > 0
        && journal.next_sequence <= MAX_SEQUENCE
        && journal.confirmed_sequence < journal.next_sequence
        && journal.pending.len() <= MAX_BATCH_RECORDS
        && journal.pending.len() as u64 == journal.next_sequence - journal.confirmed_sequence - 1
        && journal.pending.iter().enumerate().all(|(index, record)| {
            record.schema_version == 1
                && record.sequence == journal.confirmed_sequence + index as u64 + 1
                && matches!(
                    record.update["sessionUpdate"].as_str(),
                    Some("user_message" | "agent_message" | "tool_call_update")
                )
        });
    if !valid {
        return Err("Invalid Delta outbox sequence or records; preserved without sending".into());
    }
    if remote_next < journal.confirmed_sequence + 1 || remote_next > journal.next_sequence {
        return Err(
            "Delta's producer frontier does not match the durable Rig outbox. \
             Restore the matching stores or start a new session; do not reset its sequence."
                .into(),
        );
    }
    Ok(())
}

fn completed_tools(conversation: &Conversation) -> BTreeMap<String, bool> {
    let mut completed = BTreeMap::new();
    for entry in conversation.entries() {
        if let Message::User { content } = entry.message {
            for part in content {
                if let UserContent::ToolResult(result) = part {
                    completed.insert(result.call.to_string(), result.is_error);
                }
            }
        }
    }
    completed
}

fn is_delta_tool(name: &str) -> bool {
    matches!(name, "delta" | "delta_sync")
}

fn text_update(kind: &str, id: &str, text: &str) -> Value {
    json!({
        "sessionUpdate":kind,
        "messageId":id,
        "content":[{"type":"text", "text":text}],
    })
}

// `false` means this unchanged prefix is queued but later snapshot activity must
// wait for another explicit call. No unqueued block is marked as seen.
fn queue_update(
    journal: &mut Journal,
    input: &mut String,
    key: &str,
    update: Value,
    replaying: bool,
) -> Result<bool, String> {
    if journal.seen.get(key) == Some(&update) {
        return Ok(true);
    }
    if replaying || journal.pending.len() == MAX_BATCH_RECORDS {
        return Ok(false);
    }
    if journal.next_sequence >= MAX_SEQUENCE {
        return Err("Delta recording sequence exhausted; start a new Rig session".into());
    }
    let record = Record {
        schema_version: 1,
        sequence: journal.next_sequence,
        update,
    };
    let line = serde_json::to_string(&record)
        .map_err(|error| format!("Cannot encode Delta record: {error}"))?
        + "\n";
    if line.len() > MAX_BATCH_BYTES {
        return Err(
            "A Delta transcript record exceeds this plugin's 32 KiB batch limit; \
             recording paused without truncating its content"
                .into(),
        );
    }
    if input.len() + line.len() > MAX_BATCH_BYTES {
        return Ok(false);
    }
    input.push_str(&line);
    journal.seen.insert(key.to_owned(), record.update.clone());
    journal.pending.push(record);
    journal.next_sequence += 1;
    Ok(true)
}

fn encode_pending(records: &[Record]) -> Result<String, String> {
    let mut input = String::new();
    for record in records {
        let line = serde_json::to_string(record)
            .map_err(|error| format!("Cannot encode saved Delta record: {error}"))?;
        if line.len() + 1 > MAX_BATCH_BYTES - input.len() {
            return Err(
                "Saved Delta records exceed the 32 KiB batch limit; preserved without sending"
                    .into(),
            );
        }
        input.push_str(&line);
        input.push('\n');
    }
    Ok(input)
}

fn save(path: &Path, journal: &Journal) -> Result<(), String> {
    let directory = path.parent().ok_or("Delta outbox has no directory")?;
    let mut temporary = tempfile::NamedTempFile::new_in(directory)
        .map_err(|error| format!("Cannot create Delta outbox transaction: {error}"))?;
    let mut writer = JournalWriter {
        file: temporary.as_file_mut(),
        bytes: 0,
    };
    serde_json::to_writer(&mut writer, journal)
        .map_err(|error| format!("Cannot save bounded Delta outbox: {error}"))?;
    writer
        .file
        .sync_all()
        .map_err(|error| format!("Cannot flush Delta outbox transaction: {error}"))?;
    temporary
        .persist(path)
        .map_err(|error| format!("Cannot commit Delta outbox transaction: {error}"))?;
    #[cfg(unix)]
    File::open(directory)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| format!("Cannot flush Delta outbox directory: {error}"))?;
    Ok(())
}

struct JournalWriter<'a> {
    file: &'a mut File,
    bytes: u64,
}

impl Write for JournalWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() as u64 > MAX_JOURNAL_BYTES - self.bytes {
            return Err(std::io::Error::other(
                "Delta outbox exceeds its 8 MiB journal limit",
            ));
        }
        let written = self.file.write(bytes)?;
        self.bytes += written as u64;
        Ok(written)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.file.flush()
    }
}

#[cfg(test)]
mod tests;
