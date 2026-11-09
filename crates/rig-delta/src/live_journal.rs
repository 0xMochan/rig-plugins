//! Durable ACP recording for one live Delta producer.
//!
//! This module owns sequence allocation, exact pending bytes and original turn
//! associations. It owns no process, permissions, retry policy or Rig effects.
//! Every mutation is flushed before records can be handed to a transport.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use rig_harness::prelude::Conversation;
use rig_harness::rig_core::message::{
    AssistantContent, DocumentData, DocumentSourceKind, Image, Message, MimeType,
    ToolResultContent, UserContent,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

const VERSION: u32 = 2;
const MAX_JOURNAL_BYTES: u64 = 64 * 1024 * 1024;
const MAX_LEGACY_BYTES: u64 = 8 * 1024 * 1024;
const MAX_RECORD_BYTES: usize = 16 * 1024 * 1024;
const MAX_BATCH_BYTES: usize = 2 * 1024 * 1024;
const MAX_BATCH_RECORDS: usize = 64;
const MAX_SEQUENCE: u64 = 9_007_199_254_740_991;

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum InitialMode {
    ShareSnapshot,
    Joined,
}

/// The host must obtain explicit export consent before enabling either option.
/// Provider-native/opaque payloads and system/plugin prompts are never exported.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct ExportOptions {
    pub reasoning: bool,
    pub media: bool,
}

#[derive(Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct State {
    version: u32,
    producer: String,
    thread: String,
    mode: InitialMode,
    initialized: bool,
    // A joined producer never exports pre-join entries, even if later amended.
    baseline_entries: BTreeSet<String>,
    baseline_tools: BTreeSet<String>,
    options: Option<ExportOptions>,
    next_sequence: u64,
    settled_sequence: u64,
    seen: BTreeMap<String, Value>,
    associations: BTreeMap<String, Option<u64>>,
    tool_turns: BTreeMap<String, Option<u64>>,
    aliases: BTreeMap<String, EntryAlias>,
    turns: BTreeMap<String, Turn>,
    lost_turns: BTreeSet<u64>,
    pending: Vec<Record>,
    // A lost receipt must replay this exact prefix before including newer rows.
    batch_end: Option<u64>,
}

#[derive(Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct EntryAlias {
    prefix: String,
    turn: Option<u64>,
    #[serde(default)]
    coalesced: bool,
}

#[derive(Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct Turn {
    sequence: u64,
    finished: bool,
}

#[derive(Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct Record {
    #[serde(rename = "schemaVersion")]
    schema_version: u32,
    sequence: u64,
    #[serde(rename = "turnSequence", skip_serializing_if = "Option::is_none")]
    turn_sequence: Option<u64>,
    update: Value,
}

/// The runtime holds the journal lock across capture and transport operations.
/// Drop releases only the lock, never pending records or checkout files.
pub(crate) struct Journal {
    path: PathBuf,
    state: State,
    _lock: Lock,
}

struct Lock(PathBuf);
impl Drop for Lock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

pub(crate) struct Batch {
    pub input: String,
    pub count: usize,
}

/// A valid receipt prefix is committed even when a later receipt is invalid.
/// `error` pauses delivery but does not hide a preceding fencing signal.
#[derive(Debug, Serialize)]
pub(crate) struct Acknowledgment {
    pub confirmed: usize,
    pub pending: usize,
    pub fenced: Vec<u64>,
    pub error: Option<String>,
}

impl Journal {
    pub(crate) fn open(
        home: &Path,
        producer: &str,
        thread: &str,
        remote_next: u64,
        mode: InitialMode,
    ) -> Result<Self, String> {
        validate_identity(producer, thread)?;
        let root = home.join("delta");
        private_directory(&root)?;
        let directory = root.join(producer);
        private_directory(&directory)?;
        let lock_path = directory.join("recording.lock");
        let mut file = OpenOptions::new().write(true).create_new(true).open(&lock_path)
            .map_err(|error| format!("Cannot lock live Delta recording at {}: {error}. Another recording may be active; inspect a stale crash lock before removing only the lock.", lock_path.display()))?;
        let lock = Lock(lock_path);
        writeln!(file, "process={}", std::process::id())
            .and_then(|()| file.sync_all())
            .map_err(|error| format!("Cannot flush live Delta recording lock: {error}"))?;
        let path = directory.join("live.json");
        let state = if let Some(bytes) = read_regular(&path, MAX_JOURNAL_BYTES)? {
            serde_json::from_slice(&bytes).map_err(|error| {
                format!("Invalid live Delta journal; preserved without sending: {error}")
            })?
        } else if let Some(bytes) = read_regular(&directory.join("outbox.json"), MAX_LEGACY_BYTES)?
        {
            if mode == InitialMode::Joined {
                return Err("Joining requires a fresh producer; this Rig session already has a historical outbox".into());
            }
            migrate_legacy(&bytes, producer, thread)?
        } else {
            if remote_next != 1 {
                return Err("Delta already has records for this producer but its local journal is missing. Restore matching stores or start a new Rig session; never reset sequence numbers.".into());
            }
            State {
                version: VERSION,
                producer: producer.into(),
                thread: thread.into(),
                mode,
                initialized: false,
                baseline_entries: BTreeSet::new(),
                baseline_tools: BTreeSet::new(),
                options: None,
                next_sequence: 1,
                settled_sequence: 0,
                seen: BTreeMap::new(),
                associations: BTreeMap::new(),
                tool_turns: BTreeMap::new(),
                aliases: BTreeMap::new(),
                turns: BTreeMap::new(),
                lost_turns: BTreeSet::new(),
                pending: Vec::new(),
                batch_end: None,
            }
        };
        validate_state(&state, producer, thread, mode, remote_next)?;
        save(&path, &state)?;
        Ok(Self {
            path,
            state,
            _lock: lock,
        })
    }

    pub(crate) fn next_sequence(&self) -> u64 {
        self.state.next_sequence
    }
    /// Receipt frontier, including fenced records that advanced the producer;
    /// this is not a count of content delivered to the server.
    pub(crate) fn confirmed_sequence(&self) -> u64 {
        self.state.settled_sequence
    }
    pub(crate) fn pending_count(&self) -> usize {
        self.state.pending.len()
    }
    pub(crate) fn options(&self) -> Option<ExportOptions> {
        self.state.options
    }
    pub(crate) fn lost_turns(&self) -> &BTreeSet<u64> {
        &self.state.lost_turns
    }
    pub(crate) fn open_turns(&self) -> Vec<u64> {
        self.state
            .turns
            .values()
            .filter(|turn| !turn.finished)
            .map(|turn| turn.sequence)
            .collect()
    }

    /// Recheck after a reconnect. A server frontier ahead of pending local
    /// records can mean lost receipts; it never authorizes regenerating them.
    pub(crate) fn validate_frontier(&self, remote_next: u64) -> Result<(), String> {
        validate_state(
            &self.state,
            &self.state.producer,
            &self.state.thread,
            self.state.mode,
            remote_next,
        )
    }

    /// Allocate a running record before the caller allows this local turn to
    /// execute. Calling again with the same native turn ID is idempotent.
    pub(crate) fn start_turn(&mut self, local_id: &str) -> Result<u64, String> {
        identifier(local_id, "local turn")?;
        if !self.state.initialized || self.state.options.is_none() {
            return Err(
                "Initialize the retained/joined transcript before starting a live Delta turn"
                    .into(),
            );
        }
        if let Some(turn) = self.state.turns.get(local_id) {
            if turn.finished || self.state.lost_turns.contains(&turn.sequence) {
                return Err("This local Delta turn has already ended or lost ownership; its identity cannot start more work".into());
            }
            return Ok(turn.sequence);
        }
        if self
            .state
            .turns
            .values()
            .any(|turn| !turn.finished && !self.state.lost_turns.contains(&turn.sequence))
        {
            return Err("Another local Delta turn has not settled; recording cannot silently transfer ownership".into());
        }
        let mut next = self.state.clone();
        let sequence = next.next_sequence;
        queue(
            &mut next,
            json!({"sessionUpdate":"state_update", "state":"running"}),
            Some(sequence),
        )?;
        next.turns.insert(
            local_id.to_owned(),
            Turn {
                sequence,
                finished: false,
            },
        );
        self.commit(next)?;
        Ok(sequence)
    }

    /// Persist a terminal idle record with the turn's original Delta identity,
    /// including after takeover. It may then receive a fenced receipt normally.
    pub(crate) fn finish_turn(&mut self, sequence: u64) -> Result<(), String> {
        let Some((id, turn)) = self
            .state
            .turns
            .iter()
            .find(|(_, turn)| turn.sequence == sequence)
        else {
            return Err("Cannot finish an unknown Delta turn".into());
        };
        if turn.finished {
            return Ok(());
        }
        let id = id.clone();
        let mut next = self.state.clone();
        queue(
            &mut next,
            json!({"sessionUpdate":"state_update", "state":"idle"}),
            Some(sequence),
        )?;
        next.turns.get_mut(&id).expect("known turn").finished = true;
        self.commit(next)
    }

    /// Watch fencing is recorded independently of append receipt delivery.
    pub(crate) fn mark_lost(&mut self, sequence: u64) -> Result<(), String> {
        known_turn(&self.state, sequence)?;
        let mut next = self.state.clone();
        next.lost_turns.insert(sequence);
        self.commit(next)
    }

    /// Reconcile a completed model response with text staged under a stable
    /// stream prefix. The host identifies the actual committed assistant entry;
    /// no content matching or part-index inference happens inside the journal.
    #[cfg(test)]
    pub(crate) fn alias_entry(
        &mut self,
        entry_id: &str,
        prefix: &str,
        turn: Option<u64>,
    ) -> Result<(), String> {
        self.alias(entry_id, prefix, turn, false)
    }

    /// Reconcile whole-reply streaming text with normal or stopped Rig replies.
    /// Rig can fold several partial parts into one stopped text/thought block,
    /// so these replies use stable `:text` / `:thought` identities, not indices.
    pub(crate) fn alias_reply(
        &mut self,
        entry_id: &str,
        prefix: &str,
        turn: Option<u64>,
    ) -> Result<(), String> {
        self.alias(entry_id, prefix, turn, true)
    }

    fn alias(
        &mut self,
        entry_id: &str,
        prefix: &str,
        turn: Option<u64>,
        coalesced: bool,
    ) -> Result<(), String> {
        if !self.state.initialized || self.state.options.is_none() {
            return Err(
                "Initialize the live Delta journal before reconciling stream entries".into(),
            );
        }
        identifier(entry_id, "completed assistant entry ID")?;
        identifier(prefix, "stream prefix")?;
        if !prefix.starts_with("stream:") {
            return Err("Delta entry aliases require an explicit stream: prefix".into());
        }
        if let Some(turn) = turn {
            known_turn(&self.state, turn)?;
        }
        if self
            .state
            .baseline_entries
            .iter()
            .any(|entry| entry.split_once(':').is_some_and(|(_, id)| id == entry_id))
        {
            return Err("A joined session cannot alias or export a pre-join entry".into());
        }
        if let Some(existing) = self.state.aliases.get(entry_id) {
            return if existing.prefix == prefix
                && existing.turn == turn
                && existing.coalesced == coalesced
            {
                Ok(())
            } else {
                Err(
                    "A completed Delta entry already has a different stream identity or turn"
                        .into(),
                )
            };
        }
        if self
            .state
            .aliases
            .values()
            .any(|alias| alias.prefix == prefix)
        {
            return Err(
                "A Delta stream prefix cannot be reused for another committed entry".into(),
            );
        }
        if self
            .state
            .seen
            .keys()
            .any(|key| retained_entry(key) == Some(entry_id))
        {
            return Err("Reconcile a stream before exporting its committed entry; the existing message identity was preserved".into());
        }
        for (key, update) in &self.state.seen {
            if update["messageId"].as_str().is_some_and(|id| {
                id.strip_prefix(prefix)
                    .is_some_and(|tail| tail.starts_with(':'))
            }) && self.state.associations.get(key) != Some(&turn)
            {
                return Err(
                    "A completed stream alias changed its original partial-output turn".into(),
                );
            }
        }
        let mut next = self.state.clone();
        next.aliases.insert(
            entry_id.into(),
            EntryAlias {
                prefix: prefix.into(),
                turn,
                coalesced,
            },
        );
        self.commit(next)
    }

    /// Capture a borrowed retained snapshot. The first joined capture is only a
    /// baseline, so joining never exports the previous session/imported context.
    /// Existing blocks and paired tool results always retain their first turn.
    pub(crate) fn capture(
        &mut self,
        conversation: &Conversation,
        turn_sequence: Option<u64>,
        options: ExportOptions,
    ) -> Result<usize, String> {
        if let Some(sequence) = turn_sequence {
            known_turn(&self.state, sequence)?;
        }
        if self.state.options.is_some_and(|saved| saved != options) {
            return Err("Delta export policy changed; obtain a new explicit export transition rather than silently expanding a live recording".into());
        }
        let baseline = !self.state.initialized && self.state.mode == InitialMode::Joined;
        if baseline {
            let mut next = self.state.clone();
            for entry in conversation.entries() {
                next.baseline_entries
                    .insert(format!("{}:{}", entry.made, entry.id));
                match entry.message {
                    Message::Assistant(message) => {
                        for call in message.tool_calls() {
                            next.baseline_tools.insert(call.id.to_string());
                        }
                    }
                    Message::User { content } => {
                        for part in content {
                            if let UserContent::ToolResult(result) = part {
                                next.baseline_tools.insert(result.call.to_string());
                            }
                        }
                    }
                    Message::System { .. } => (),
                }
            }
            next.options = Some(options);
            next.initialized = true;
            self.commit(next)?;
            return Ok(0);
        }
        check_source_bounds(
            conversation,
            options,
            &self.state.baseline_entries,
            &self.state.baseline_tools,
        )?;
        let before = self.state.pending.len();
        let mut next = self.state.clone();
        let completed = completed_tools(conversation);
        for entry in conversation.entries() {
            let retained_prefix = format!("{}:{}", entry.made, entry.id);
            if next.baseline_entries.contains(&retained_prefix) {
                continue;
            }
            let alias = next.aliases.get(&entry.id.to_string()).cloned();
            if alias.is_some() && !matches!(entry.message, Message::Assistant(_)) {
                return Err("A Delta stream alias targets a non-assistant entry".into());
            }
            let prefix = alias
                .as_ref()
                .map(|alias| alias.prefix.clone())
                .unwrap_or(retained_prefix);
            let entry_turn = alias
                .as_ref()
                .map(|alias| alias.turn)
                .unwrap_or(turn_sequence);
            match entry.message {
                Message::System { .. } => (),
                Message::User { content } => {
                    for (index, part) in content.iter().enumerate() {
                        let key = format!("{prefix}:{index}");
                        match part {
                            UserContent::ToolResult(result)
                                if !is_transport(result.name.as_str()) =>
                            {
                                let id = result.call.to_string();
                                if next.baseline_tools.contains(&id) {
                                    continue;
                                }
                                let association = if let Some(turn) = next.tool_turns.get(&id) {
                                    *turn
                                } else if !next.initialized {
                                    next.tool_turns.insert(id.clone(), None);
                                    None
                                } else {
                                    return Err("A live tool result has no original call/turn association; recording paused rather than attributing it to the latest turn".into());
                                };
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
                                        ToolResultContent::Image(image) => {
                                            image_content(image, options)
                                        }
                                    })
                                    .collect();
                                observe(
                                    &mut next,
                                    &key,
                                    json!({"sessionUpdate":"tool_call_update", "toolCallId":id,
                                    "status":if result.is_error {"failed"} else {"completed"}, "rawOutput":output}),
                                    association,
                                )?;
                            }
                            UserContent::ToolResult(_) => (),
                            _ if entry.is_users_own() => {
                                let content = match part {
                                    UserContent::Text(text) => text_content(&text.text),
                                    UserContent::Image(image) => image_content(image, options),
                                    UserContent::Document(document) => match &document.data {
                                        DocumentData::Text(text) => text_content(text),
                                        _ => text_content(
                                            "[Binary document omitted by Rig Delta export.]",
                                        ),
                                    },
                                    _ => text_content(
                                        "[Unsupported non-text attachment omitted by Rig Delta export.]",
                                    ),
                                };
                                observe(
                                    &mut next,
                                    &key,
                                    message_update("user_message", &key, content),
                                    entry_turn,
                                )?;
                            }
                            _ => (), // Remote/plugin-authored context is never recaptured.
                        }
                    }
                }
                Message::Assistant(message) => {
                    let coalesced = alias.as_ref().is_some_and(|alias| alias.coalesced);
                    if coalesced {
                        capture_reply(&mut next, &prefix, &message.content, entry_turn, options)?;
                    }
                    for (index, part) in message.content.iter().enumerate() {
                        if coalesced
                            && matches!(
                                part,
                                AssistantContent::Text(_) | AssistantContent::Reasoning(_)
                            )
                        {
                            continue;
                        }
                        let key = format!("{prefix}:{index}");
                        let update = match part {
                            AssistantContent::Text(text) => {
                                message_update("agent_message", &key, text_content(&text.text))
                            }
                            AssistantContent::ToolCall(call)
                                if !is_transport(call.function.name.as_str()) =>
                            {
                                let id = call.id.to_string();
                                let original =
                                    *next.tool_turns.entry(id.clone()).or_insert(entry_turn);
                                let status = match completed.get(&id) {
                                    Some(true) => "failed",
                                    Some(false) => "completed",
                                    None => "in_progress",
                                };
                                observe(
                                    &mut next,
                                    &key,
                                    json!({"sessionUpdate":"tool_call_update", "toolCallId":id,
                                    "title":call.function.name.as_str(), "name":call.function.name.as_str(), "kind":"other",
                                    "status":status, "rawInput":call.function.arguments}),
                                    original,
                                )?;
                                continue;
                            }
                            AssistantContent::Reasoning(reasoning)
                                if options.reasoning && !reasoning.redacted =>
                            {
                                message_update("agent_thought", &key, text_content(&reasoning.text))
                            }
                            AssistantContent::Image(image) => {
                                message_update("agent_message", &key, image_content(image, options))
                            }
                            _ => continue,
                        };
                        let seen_key = if alias.is_some() {
                            let kind = update["sessionUpdate"]
                                .as_str()
                                .expect("canonical message kind");
                            format!("host:{kind}:{key}")
                        } else {
                            key
                        };
                        observe(&mut next, &seen_key, update, entry_turn)?;
                    }
                }
            }
        }
        next.options = Some(options);
        next.initialized = true;
        let added = next.pending.len() - before;
        self.commit(next)?;
        Ok(added)
    }

    /// Stage a canonical update authored by the host (for example a child prompt
    /// or session title). Received watch data must never be passed here. A host
    /// that stages streaming text must reconcile its IDs with final entries;
    /// this method deliberately does not guess that correspondence.
    pub(crate) fn stage(&mut self, mut update: Value, turn: Option<u64>) -> Result<(), String> {
        if !self.state.initialized || self.state.options.is_none() {
            return Err("Initialize the live Delta journal before staging host updates".into());
        }
        if let Some(turn) = turn {
            known_turn(&self.state, turn)?;
        }
        let options = self.state.options.expect("initialized export options");
        let key = validate_host_update(&update, options)?;
        check_serialized_source(&update)?;
        if update["messageId"].as_str().is_some_and(|id| {
            self.state.aliases.values().any(|alias| {
                id.strip_prefix(&alias.prefix)
                    .is_some_and(|tail| tail.starts_with(':'))
            })
        }) {
            return Ok(()); // A late partial must not replace a completed response.
        }
        let mut next = self.state.clone();
        let association = if update["sessionUpdate"] == "tool_call_update" {
            let id = update["toolCallId"]
                .as_str()
                .expect("validated tool identity")
                .to_owned();
            if next.baseline_tools.contains(&id) {
                return Ok(());
            }
            let original = *next.tool_turns.entry(id).or_insert(turn);
            if let Some(previous) = next.seen.get(&key).and_then(Value::as_object) {
                let mut merged = previous.clone();
                let fields = update.as_object().expect("validated update object");
                for (name, value) in fields {
                    let terminal = matches!(
                        merged.get("status").and_then(Value::as_str),
                        Some("completed" | "failed")
                    );
                    if name == "status"
                        && terminal
                        && matches!(value.as_str(), Some("pending" | "in_progress"))
                    {
                        continue;
                    }
                    merged.insert(name.clone(), value.clone());
                }
                update = Value::Object(merged);
            }
            original
        } else {
            turn
        };
        observe(&mut next, &key, update, association)?;
        self.commit(next)
    }

    /// Freeze the pending prefix before transport. An individual record larger
    /// than the normal 2 MiB batch is sent alone, up to Delta's 16 MiB line cap.
    pub(crate) fn batch(&mut self) -> Result<Batch, String> {
        if self.state.pending.is_empty() {
            return Ok(Batch {
                input: String::new(),
                count: 0,
            });
        }
        let mut input = String::new();
        let mut count = 0;
        for record in &self.state.pending {
            if self
                .state
                .batch_end
                .is_some_and(|end| record.sequence > end)
            {
                break;
            }
            let line = encode_record(record)?;
            if self.state.batch_end.is_none()
                && count > 0
                && (count == MAX_BATCH_RECORDS || input.len() + line.len() > MAX_BATCH_BYTES)
            {
                break;
            }
            input.push_str(&line);
            count += 1;
        }
        if self.state.batch_end.is_none() {
            let mut next = self.state.clone();
            next.batch_end = self
                .state
                .pending
                .get(count - 1)
                .map(|record| record.sequence);
            self.commit(next)?;
        }
        Ok(Batch { input, count })
    }

    /// Commit only matching prefix receipts. Invalid later lines remain an
    /// explicit error on the returned acknowledgment, alongside any lost turn.
    pub(crate) fn acknowledge(&mut self, stdout: &str) -> Result<Acknowledgment, String> {
        let Some(batch_end) = self.state.batch_end else {
            return Err("No live Delta batch was frozen for these receipts".into());
        };
        let mut count = 0;
        let mut confirmed = 0;
        let mut fenced = BTreeSet::new();
        let mut failure = None;
        for line in stdout.lines() {
            let Some(record) = self
                .state
                .pending
                .get(count)
                .filter(|record| record.sequence <= batch_end)
            else {
                failure = Some("Delta returned a receipt outside this frozen batch".into());
                break;
            };
            let receipt: Value = match serde_json::from_str(line) {
                Ok(value) => value,
                Err(error) => {
                    failure = Some(format!("Invalid Delta append receipt: {error}"));
                    break;
                }
            };
            if let Some(error) = receipt.get("error") {
                failure = Some(format!("Delta append paused: {error}"));
                break;
            }
            let disposition = receipt["disposition"].as_str();
            let matching = receipt["operation"] == "appended"
                && receipt["thread_id"] == self.state.thread
                && receipt["sequence"].as_u64() == Some(record.sequence)
                && receipt.get("turn_sequence")
                    == Some(&record.turn_sequence.map(Value::from).unwrap_or(Value::Null))
                && receipt["next_sequence"]
                    .as_u64()
                    .is_some_and(|next| next > record.sequence && next <= self.state.next_sequence)
                && matches!(disposition, Some("recorded" | "duplicate" | "fenced"))
                && receipt["duplicate"].as_bool() == Some(disposition == Some("duplicate"));
            if !matching || (disposition == Some("fenced") && record.turn_sequence.is_none()) {
                failure = Some("Delta receipt changed thread, sequence, turn association or disposition; it cannot retire this record".into());
                break;
            }
            if disposition == Some("fenced") {
                fenced.insert(record.turn_sequence.expect("fenced turn checked"));
            } else {
                confirmed += 1;
            }
            count += 1;
        }
        let mut next = self.state.clone();
        if count > 0 {
            next.settled_sequence = next.pending[count - 1].sequence;
            next.pending.drain(..count);
            next.lost_turns.extend(fenced.iter().copied());
        }
        if next.settled_sequence >= batch_end {
            next.batch_end = None;
        }
        if next.batch_end.is_some() && failure.is_none() {
            failure = Some("Delta did not acknowledge the complete frozen batch".into());
        }
        if count > 0 {
            self.commit(next)?;
        }
        Ok(Acknowledgment {
            confirmed,
            pending: self.state.pending.len(),
            fenced: fenced.into_iter().collect(),
            error: failure,
        })
    }

    fn commit(&mut self, next: State) -> Result<(), String> {
        if next == self.state {
            return Ok(());
        }
        validate_state(
            &next,
            &self.state.producer,
            &self.state.thread,
            self.state.mode,
            next.settled_sequence + 1,
        )?;
        save(&self.path, &next)?;
        self.state = next;
        Ok(())
    }
}

fn validate_host_update(update: &Value, options: ExportOptions) -> Result<String, String> {
    let fields = update
        .as_object()
        .ok_or("A host-authored Delta update must be an object")?;
    let kind = update["sessionUpdate"]
        .as_str()
        .ok_or("Delta host update lacks sessionUpdate")?;
    let (identity, allowed): (String, &[&str]) = match kind {
        "user_message" | "agent_message" | "agent_thought" => {
            if kind == "agent_thought" && !options.reasoning {
                return Err("Reasoning export was not enabled for this live producer".into());
            }
            let id = update["messageId"]
                .as_str()
                .ok_or("Delta message update lacks messageId")?;
            identifier(id, "host message ID")?;
            let content = update["content"]
                .as_array()
                .ok_or("Delta message content must be an array")?;
            if content.is_empty() {
                return Err("Delta message update has no content".into());
            }
            for part in content {
                match part["type"].as_str() {
                    Some("text") if part["text"].is_string() => (),
                    Some("image") if options.media && part["data"].is_string() && part["mimeType"].is_string() => (),
                    _ => return Err("Host update contains unsupported content or media outside its export policy".into()),
                }
            }
            (
                format!("host:{kind}:{id}"),
                &["sessionUpdate", "messageId", "content", "_meta"],
            )
        }
        "tool_call_update" => {
            let id = update["toolCallId"]
                .as_str()
                .ok_or("Delta tool update lacks toolCallId")?;
            identifier(id, "host tool ID")?;
            if let Some(status) = update.get("status")
                && !matches!(
                    status.as_str(),
                    Some("pending" | "in_progress" | "completed" | "failed")
                )
            {
                return Err("Delta host tool update has an invalid status".into());
            }
            (
                format!("host:tool:{id}"),
                &[
                    "sessionUpdate",
                    "toolCallId",
                    "title",
                    "name",
                    "kind",
                    "status",
                    "rawInput",
                    "rawOutput",
                    "_meta",
                ],
            )
        }
        "session_info_update" => {
            if !update["title"].is_string() {
                return Err("Delta title update lacks a string title".into());
            }
            (
                "host:session_info".into(),
                &["sessionUpdate", "title", "_meta"],
            )
        }
        "state_update" => {
            return Err("Use start_turn/finish_turn to change Delta turn ownership".into());
        }
        _ => return Err("Unsupported host-authored Delta update".into()),
    };
    if fields.keys().any(|name| !allowed.contains(&name.as_str())) {
        return Err("Host-authored Delta update has unsupported fields; opaque/provider payloads cannot be staged".into());
    }
    Ok(identity)
}

fn retained_entry(key: &str) -> Option<&str> {
    let (made, rest) = key.split_once(':')?;
    made.parse::<u64>().ok()?;
    let (entry, part) = rest.rsplit_once(':')?;
    part.parse::<usize>().ok()?;
    Some(entry)
}

fn capture_reply(
    state: &mut State,
    prefix: &str,
    content: &[AssistantContent],
    turn: Option<u64>,
    options: ExportOptions,
) -> Result<(), String> {
    fn append(target: &mut String, value: &str) -> Result<(), String> {
        if value.len() > MAX_RECORD_BYTES - target.len() {
            return Err(
                "Combined Delta reply exceeds 16 MiB; recording paused without truncation".into(),
            );
        }
        target.push_str(value);
        Ok(())
    }
    let (mut text, mut thought) = (String::new(), String::new());
    let (mut has_text, mut has_thought) = (false, false);
    for part in content {
        match part {
            AssistantContent::Text(part) => {
                has_text = true;
                append(&mut text, &part.text)?;
            }
            AssistantContent::Reasoning(part) if options.reasoning && !part.redacted => {
                has_thought = true;
                append(&mut thought, &part.text)?;
            }
            _ => (),
        }
    }
    let text_id = format!("{prefix}:text");
    let text_key = format!("host:agent_message:{text_id}");
    if has_text || state.seen.contains_key(&text_key) {
        observe(
            state,
            &text_key,
            message_update("agent_message", &text_id, text_content(&text)),
            turn,
        )?;
    }
    let thought_id = format!("{prefix}:thought");
    let thought_key = format!("host:agent_thought:{thought_id}");
    if options.reasoning && (has_thought || state.seen.contains_key(&thought_key)) {
        observe(
            state,
            &thought_key,
            message_update("agent_thought", &thought_id, text_content(&thought)),
            turn,
        )?;
    }
    Ok(())
}

fn observe(state: &mut State, key: &str, update: Value, turn: Option<u64>) -> Result<(), String> {
    if state.seen.get(key) == Some(&update) {
        return Ok(());
    }
    let original = *state.associations.entry(key.into()).or_insert(turn);
    queue(state, update, original)?;
    let queued = state
        .pending
        .last()
        .expect("queued observed record")
        .update
        .clone();
    state.seen.insert(key.into(), queued);
    Ok(())
}

fn queue(state: &mut State, update: Value, turn_sequence: Option<u64>) -> Result<(), String> {
    if state.next_sequence >= MAX_SEQUENCE {
        return Err("Delta live sequence exhausted; start a new Rig session".into());
    }
    let record = Record {
        schema_version: 1,
        sequence: state.next_sequence,
        turn_sequence,
        update,
    };
    encode_record(&record)?;
    state.pending.push(record);
    state.next_sequence += 1;
    Ok(())
}

fn encode_record(record: &Record) -> Result<String, String> {
    struct Encoded(Vec<u8>);
    impl Write for Encoded {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if bytes.len() > MAX_RECORD_BYTES - 1 - self.0.len() {
                return Err(std::io::Error::other("record exceeds 16 MiB JSONL limit"));
            }
            self.0.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut encoded = Encoded(Vec::new());
    serde_json::to_writer(&mut encoded, record).map_err(|error| {
        format!("Cannot encode bounded Delta record; recording paused without truncation: {error}")
    })?;
    encoded.0.push(b'\n');
    String::from_utf8(encoded.0)
        .map_err(|error| format!("Delta record encoder produced invalid UTF-8: {error}"))
}

fn known_turn(state: &State, sequence: u64) -> Result<(), String> {
    if state.turns.values().any(|turn| turn.sequence == sequence) {
        Ok(())
    } else {
        Err("This output has no known original local Delta turn".into())
    }
}

fn message_update(kind: &str, key: &str, content: Value) -> Value {
    json!({"sessionUpdate":kind, "messageId":key, "content":[content]})
}
fn text_content(text: &str) -> Value {
    json!({"type":"text", "text":text})
}
fn image_content(image: &Image, options: ExportOptions) -> Value {
    if !options.media {
        return text_content("[Image omitted by Rig Delta export policy.]");
    }
    let media = image.media_type.clone().or_else(|| match &image.data {
        DocumentSourceKind::Raw(bytes) => {
            rig_harness::rig_core::message::ImageMediaType::sniff(bytes)
        }
        _ => None,
    });
    let Some(media) = media else {
        return text_content("[Image omitted: its media type is unknown.]");
    };
    let data = match &image.data {
        DocumentSourceKind::Base64(data) => data.clone(),
        DocumentSourceKind::Raw(data) => base64(data),
        _ => {
            return text_content(
                "[External/provider image source omitted; Rig does not fetch it for export.]",
            );
        }
    };
    json!({"type":"image", "mimeType":media.to_mime_type(), "data":data})
}

fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut text = String::with_capacity(bytes.len().div_ceil(3).saturating_mul(4));
    for chunk in bytes.chunks(3) {
        let value = (u32::from(chunk[0]) << 16)
            | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8)
            | u32::from(*chunk.get(2).unwrap_or(&0));
        text.push(ALPHABET[((value >> 18) & 63) as usize] as char);
        text.push(ALPHABET[((value >> 12) & 63) as usize] as char);
        text.push(if chunk.len() > 1 {
            ALPHABET[((value >> 6) & 63) as usize] as char
        } else {
            '='
        });
        text.push(if chunk.len() > 2 {
            ALPHABET[(value & 63) as usize] as char
        } else {
            '='
        });
    }
    text
}
// Reject huge selected inputs while still borrowed, before JSON projection or
// base64 encoding can duplicate them. The wire writer also budgets escaping.
fn check_source_bounds(
    conversation: &Conversation,
    options: ExportOptions,
    baseline_entries: &BTreeSet<String>,
    baseline_tools: &BTreeSet<String>,
) -> Result<(), String> {
    fn text(value: &str) -> Result<(), String> {
        if value.len() >= MAX_RECORD_BYTES {
            Err("A selected Delta transcript part exceeds 16 MiB; recording paused without truncation".into())
        } else {
            Ok(())
        }
    }
    fn image(image: &Image, options: ExportOptions) -> Result<(), String> {
        if options.media {
            match &image.data {
                DocumentSourceKind::Base64(data) => text(data)?,
                DocumentSourceKind::Raw(data) if data.len() >= MAX_RECORD_BYTES => {
                    return Err("Selected Delta image exceeds its input limit".into());
                }
                _ => (),
            }
        }
        Ok(())
    }
    for entry in conversation.entries() {
        if baseline_entries.contains(&format!("{}:{}", entry.made, entry.id)) {
            continue;
        }
        match entry.message {
            Message::User { content } => {
                for part in content {
                    match part {
                        UserContent::ToolResult(result)
                            if !is_transport(result.name.as_str())
                                && !baseline_tools.contains(&result.call.to_string()) =>
                        {
                            for part in &result.content {
                                match part {
                                    ToolResultContent::Text(value) => text(&value.text)?,
                                    ToolResultContent::Json { value } => {
                                        check_serialized_source(value)?
                                    }
                                    ToolResultContent::Image(value) => image(value, options)?,
                                }
                            }
                        }
                        UserContent::Text(value) if entry.is_users_own() => text(&value.text)?,
                        UserContent::Image(value) if entry.is_users_own() => image(value, options)?,
                        UserContent::Document(document) if entry.is_users_own() => {
                            if let DocumentData::Text(value) = &document.data {
                                text(value)?;
                            }
                        }
                        _ => (),
                    }
                }
            }
            Message::Assistant(message) => {
                for part in &message.content {
                    match part {
                        AssistantContent::Text(value) => text(&value.text)?,
                        AssistantContent::ToolCall(call)
                            if !is_transport(call.function.name.as_str()) =>
                        {
                            check_serialized_source(&call.function.arguments)?
                        }
                        AssistantContent::Reasoning(value)
                            if options.reasoning && !value.redacted =>
                        {
                            text(&value.text)?
                        }
                        AssistantContent::Image(value) => image(value, options)?,
                        _ => (),
                    }
                }
            }
            Message::System { .. } => (),
        }
    }
    Ok(())
}

fn check_serialized_source(value: &impl Serialize) -> Result<(), String> {
    struct Count(usize);
    impl Write for Count {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if bytes.len() > MAX_RECORD_BYTES - self.0 {
                return Err(std::io::Error::other("selected Delta data exceeds 16 MiB"));
            }
            self.0 += bytes.len();
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    serde_json::to_writer(&mut Count(0), value)
        .map_err(|error| format!("Cannot project bounded Delta data: {error}"))
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
fn is_transport(name: &str) -> bool {
    matches!(name, "delta" | "delta_sync" | "delta_live")
}

fn validate_identity(producer: &str, thread: &str) -> Result<(), String> {
    if producer.is_empty()
        || producer.len() > 128
        || !producer
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err("Delta live recording requires a safe stable Rig producer ID".into());
    }
    identifier(thread, "thread")
}
fn identifier(value: &str, kind: &str) -> Result<(), String> {
    if value.trim().is_empty() || value.len() > 4096 || value.chars().any(char::is_control) {
        return Err(format!(
            "Delta {kind} must be nonblank, bounded and contain no controls"
        ));
    }
    Ok(())
}

fn validate_state(
    state: &State,
    producer: &str,
    thread: &str,
    mode: InitialMode,
    remote_next: u64,
) -> Result<(), String> {
    if state.version != VERSION
        || state.producer != producer
        || state.thread != thread
        || state.mode != mode
    {
        return Err("Live Delta journal belongs to another version, producer, thread or join mode; preserved without sending".into());
    }
    if mode == InitialMode::ShareSnapshot
        && (!state.baseline_entries.is_empty() || !state.baseline_tools.is_empty())
    {
        return Err("A sharing journal contains unexpected join-only baseline state".into());
    }
    for id in state
        .baseline_entries
        .iter()
        .chain(state.baseline_tools.iter())
    {
        identifier(id, "joined baseline ID")?;
    }
    if state.next_sequence == 0
        || state.next_sequence > MAX_SEQUENCE
        || state.settled_sequence >= state.next_sequence
        || state.pending.len() as u64 != state.next_sequence - state.settled_sequence - 1
    {
        return Err("Invalid live Delta sequence frontier; preserved without sending".into());
    }
    for (index, record) in state.pending.iter().enumerate() {
        if record.schema_version != 1
            || record.sequence != state.settled_sequence + index as u64 + 1
        {
            return Err(
                "Live Delta pending sequence was changed; preserved without sending".into(),
            );
        }
        let kind = record.update["sessionUpdate"].as_str();
        if !matches!(
            kind,
            Some(
                "user_message"
                    | "agent_message"
                    | "agent_thought"
                    | "tool_call_update"
                    | "state_update"
                    | "session_info_update"
            )
        ) {
            return Err("Unknown live Delta update in durable journal".into());
        }
        if let Some(turn) = record.turn_sequence
            && (turn == 0
                || turn > record.sequence
                || (turn == record.sequence
                    && !(kind == Some("state_update") && record.update["state"] == "running")))
        {
            return Err("Live Delta record has an invalid original turn association".into());
        }
        encode_record(record)?;
    }
    let mut turn_sequences = BTreeSet::new();
    for (id, turn) in &state.turns {
        identifier(id, "saved local turn")?;
        if turn.sequence == 0
            || turn.sequence >= state.next_sequence
            || !turn_sequences.insert(turn.sequence)
        {
            return Err("Invalid or reused Delta local turn identity".into());
        }
    }
    let mut stream_prefixes = BTreeSet::new();
    for (entry, alias) in &state.aliases {
        identifier(entry, "saved assistant entry alias")?;
        identifier(&alias.prefix, "saved stream alias")?;
        if !alias.prefix.starts_with("stream:")
            || !stream_prefixes.insert(&alias.prefix)
            || alias
                .turn
                .is_some_and(|turn| !turn_sequences.contains(&turn))
            || state.baseline_entries.iter().any(|baseline| {
                baseline
                    .split_once(':')
                    .is_some_and(|(_, id)| id == entry.as_str())
            })
        {
            return Err("Delta journal has conflicting stream or pre-join entry aliases".into());
        }
    }
    if state.seen.keys().ne(state.associations.keys()) {
        return Err(
            "Delta live snapshot lost its original block associations; preserved without sending"
                .into(),
        );
    }
    for (key, update) in &state.seen {
        if let Some(id) = update["toolCallId"].as_str()
            && state.tool_turns.get(id) != state.associations.get(key)
        {
            return Err(
                "Delta live tool input/result turn association changed; preserved without sending"
                    .into(),
            );
        }
    }
    if state
        .lost_turns
        .iter()
        .any(|turn| !turn_sequences.contains(turn))
        || state
            .pending
            .iter()
            .filter_map(|record| record.turn_sequence)
            .any(|turn| !turn_sequences.contains(&turn))
        || state
            .associations
            .values()
            .chain(state.tool_turns.values())
            .flatten()
            .any(|turn| !turn_sequences.contains(turn))
    {
        return Err("Delta live journal refers to an unknown original turn".into());
    }
    if state
        .batch_end
        .is_some_and(|end| end <= state.settled_sequence || end >= state.next_sequence)
    {
        return Err("Invalid frozen Delta batch frontier".into());
    }
    if remote_next < state.settled_sequence + 1 || remote_next > state.next_sequence {
        return Err("Delta's producer frontier does not match this durable live journal. Restore matching stores or start a new session; do not reset sequences.".into());
    }
    Ok(())
}

fn migrate_legacy(bytes: &[u8], producer: &str, thread: &str) -> Result<State, String> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Legacy {
        version: u32,
        agent_id: String,
        thread: String,
        next_sequence: u64,
        confirmed_sequence: u64,
        seen: BTreeMap<String, Value>,
        pending: Vec<Record>,
    }
    let old: Legacy = serde_json::from_slice(bytes).map_err(|error| {
        format!("Invalid historical Delta outbox; migration preserved it without sending: {error}")
    })?;
    if old.version != 1
        || old.agent_id != producer
        || old.thread != thread
        || old
            .pending
            .iter()
            .any(|record| record.turn_sequence.is_some())
    {
        return Err(
            "Historical Delta outbox identity/version changed; cannot migrate or reset it".into(),
        );
    }
    let associations = old.seen.keys().map(|key| (key.clone(), None)).collect();
    let tool_turns = old
        .seen
        .values()
        .filter_map(|update| {
            update["toolCallId"]
                .as_str()
                .map(|id| (id.to_owned(), None))
        })
        .collect();
    let batch_end = old.pending.last().map(|record| record.sequence);
    Ok(State {
        version: VERSION,
        producer: old.agent_id,
        thread: old.thread,
        mode: InitialMode::ShareSnapshot,
        initialized: true,
        baseline_entries: BTreeSet::new(),
        baseline_tools: BTreeSet::new(),
        options: None,
        next_sequence: old.next_sequence,
        settled_sequence: old.confirmed_sequence,
        seen: old.seen,
        associations,
        tool_turns,
        aliases: BTreeMap::new(),
        turns: BTreeMap::new(),
        lost_turns: BTreeSet::new(),
        pending: old.pending,
        batch_end,
    })
}

fn private_directory(path: &Path) -> Result<(), String> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => Ok(()),
        Ok(_) => Err(format!(
            "Delta journal directory cannot be a file or symlink: {}",
            path.display()
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                let mut builder = fs::DirBuilder::new();
                builder.mode(0o700);
                builder.create(path).map_err(|error| {
                    format!("Cannot create private Delta journal directory: {error}")
                })?;
            }
            #[cfg(not(unix))]
            fs::create_dir(path)
                .map_err(|error| format!("Cannot create Delta journal directory: {error}"))?;
            Ok(())
        }
        Err(error) => Err(format!("Cannot inspect Delta journal directory: {error}")),
    }
}

fn read_regular(path: &Path, maximum: u64) -> Result<Option<Vec<u8>>, String> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(format!(
                "Cannot inspect Delta journal {}: {error}",
                path.display()
            ));
        }
    };
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > maximum {
        return Err(format!(
            "Delta journal must be a bounded regular file, not a symlink: {}",
            path.display()
        ));
    }
    let mut bytes = Vec::new();
    File::open(path)
        .and_then(|file| file.take(maximum + 1).read_to_end(&mut bytes))
        .map_err(|error| format!("Cannot read Delta journal: {error}"))?;
    if bytes.len() as u64 > maximum {
        return Err("Delta journal exceeded its byte limit during read".into());
    }
    Ok(Some(bytes))
}

fn save(path: &Path, state: &State) -> Result<(), String> {
    struct Bounded<'a> {
        file: &'a mut File,
        written: u64,
    }
    impl Write for Bounded<'_> {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if bytes.len() as u64 > MAX_JOURNAL_BYTES - self.written {
                return Err(std::io::Error::other("Delta live journal exceeds 64 MiB"));
            }
            let count = self.file.write(bytes)?;
            self.written += count as u64;
            Ok(count)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            self.file.flush()
        }
    }
    let directory = path
        .parent()
        .ok_or("Delta journal has no parent directory")?;
    let mut temporary = tempfile::NamedTempFile::new_in(directory)
        .map_err(|error| format!("Cannot create Delta journal transaction: {error}"))?;
    serde_json::to_writer(
        &mut Bounded {
            file: temporary.as_file_mut(),
            written: 0,
        },
        state,
    )
    .map_err(|error| format!("Cannot save bounded Delta live journal: {error}"))?;
    temporary
        .as_file_mut()
        .sync_all()
        .map_err(|error| format!("Cannot flush Delta live journal: {error}"))?;
    temporary
        .persist(path)
        .map_err(|error| format!("Cannot commit Delta live journal: {error}"))?;
    #[cfg(unix)]
    File::open(directory)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| format!("Cannot flush Delta journal directory: {error}"))?;
    Ok(())
}

#[cfg(test)]
mod tests;
