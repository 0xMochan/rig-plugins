//! Bounded Delta watch snapshots and inert remote-context changes.
//!
//! This module implements wire-data handling only. It neither starts a process
//! nor schedules a Rig turn or tool. A host must separately own the watch,
//! authorize its lifetime, and act on a matching [`WatchEffect::StopLocalTurn`].

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::io::{self, Write};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub const MAX_LINE_BYTES: usize = 1024 * 1024;
pub const MAX_STATE_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_ITEMS: usize = 4096;
pub const MAX_COMMENTS: usize = 2048;
pub const MAX_DIRECTORY_ENTRIES: usize = 256;
const MAX_SEQUENCE: u64 = 9_007_199_254_740_991;

/// A known malformed frame or a bounded-state failure. Hosts must pause rather
/// than treat these as permission to reconnect or continue a local shared turn.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WatchError(String);

impl fmt::Display for WatchError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for WatchError {}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct WatchUser {
    pub user_id: Option<String>,
    pub name: Option<String>,
    pub actor_id: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct Producer {
    pub source: String,
    pub external_id: String,
    pub actor_id: Option<String>,
    pub next_sequence: u64,
    pub turn: Option<ProducerTurn>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct ProducerTurn {
    pub sequence: u64,
    pub status: ProducerTurnStatus,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProducerTurnStatus {
    Running,
    Finished,
    Superseded,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct WatchItem {
    pub id: String,
    pub kind: String,
    pub after_id: Option<String>,
    pub turn_id: Option<String>,
    pub author: Option<WatchUser>,
    pub created_at: Option<u64>,
    pub open: bool,
    pub content: serde_json::Map<String, Value>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Worktree {
    /// This is an opaque CLI reference, not a thread ID to reconstruct.
    pub reference: String,
    pub name: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Child {
    pub thread_id: String,
    /// Preserve this reference verbatim for a later child watch or append.
    pub reference: String,
    pub kind: String,
    pub name: String,
    pub status: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct Comment {
    pub id: String,
    pub chain: String,
    pub after_id: Option<String>,
    pub delivered_with: String,
    pub routed_to: Option<String>,
    pub author: WatchUser,
    pub co_authors: Vec<WatchUser>,
    pub agent: bool,
    pub created_at: Option<u64>,
    pub text: String,
    pub target: serde_json::Map<String, Value>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct CommandError {
    pub code: String,
    pub message: String,
    pub reason: Option<String>,
    pub retryable: Option<bool>,
    pub retry_after_ms: Option<u64>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SharedTurnState {
    Idle,
    Running,
}

/// Contract-8 watch frames. Additional object fields are deliberately ignored.
/// Unknown frame types are skipped by [`WatchEvent::parse`], not interpreted.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WatchEvent {
    Ready {
        thread_id: String,
        producer: Option<Producer>,
    },
    Item {
        item: WatchItem,
    },
    #[serde(rename = "item.append")]
    ItemAppend {
        id: String,
        text: String,
    },
    Remove {
        id: String,
    },
    Live,
    Turn {
        state: SharedTurnState,
        turn_id: Option<String>,
        user: Option<WatchUser>,
    },
    #[serde(rename = "producer.turn")]
    ProducerTurn {
        sequence: u64,
        status: ProducerTurnStatus,
    },
    Presence {
        users: Vec<WatchUser>,
    },
    Worktrees {
        worktrees: Vec<Worktree>,
    },
    Children {
        children: Vec<Child>,
    },
    Child {
        child: Child,
    },
    #[serde(rename = "child.remove")]
    ChildRemove {
        thread_id: String,
    },
    Comment {
        comment: Comment,
    },
    #[serde(rename = "comment.remove")]
    CommentRemove {
        id: String,
    },
    Error {
        error: CommandError,
    },
}

impl WatchEvent {
    /// Decode one UTF-8 JSONL line. A typed error envelope may have no `type`.
    /// This does not execute retry hints or interpret diagnostic `reason` values.
    pub fn parse(line: &str) -> Result<Option<Self>, WatchError> {
        if line.len() > MAX_LINE_BYTES {
            return Err(error("Delta watch line exceeds 1 MiB"));
        }
        let mut value: Value = serde_json::from_str(line)
            .map_err(|cause| error(format!("Invalid Delta watch JSON: {cause}")))?;
        let object = value
            .as_object_mut()
            .ok_or_else(|| error("Delta watch frame must be an object"))?;
        if object.contains_key("error") {
            object.insert("type".into(), Value::String("error".into()));
        }
        let kind = object
            .get("type")
            .and_then(Value::as_str)
            .ok_or_else(|| error("Delta watch frame has no string type"))?;
        let fields: &[&str] = match kind {
            "ready" => &["thread_id", "producer"],
            "item" => &["item"],
            "item.append" => &["id", "text"],
            "remove" | "comment.remove" => &["id"],
            "live" => &[],
            "turn" => &["state", "turn_id", "user"],
            "producer.turn" => &["sequence", "status"],
            "presence" => &["users"],
            "worktrees" => &["worktrees"],
            "children" => &["children"],
            "child" => &["child"],
            "child.remove" => &["thread_id"],
            "comment" => &["comment"],
            "error" => &["error"],
            _ => return Ok(None),
        };
        required(&value, fields)?;
        validate_nested(&value)?;
        let event: Self = serde_json::from_value(value)
            .map_err(|cause| error(format!("Malformed Delta watch frame: {cause}")))?;
        event.validate()?;
        Ok(Some(event))
    }

    fn validate(&self) -> Result<(), WatchError> {
        match self {
            Self::Ready {
                thread_id,
                producer,
            } => {
                identifier(thread_id)?;
                if let Some(producer) = producer {
                    identifier(&producer.source)?;
                    identifier(&producer.external_id)?;
                    sequence(producer.next_sequence)?;
                    if let Some(turn) = &producer.turn {
                        sequence(turn.sequence)?;
                    }
                }
            }
            Self::Item { item } => {
                identifier(&item.id)?;
                identifier(&item.kind)?;
                if let Some(previous) = &item.after_id {
                    identifier(previous)?;
                }
            }
            Self::ItemAppend { id, .. } | Self::Remove { id } | Self::CommentRemove { id } => {
                identifier(id)?
            }
            Self::ProducerTurn {
                sequence: number, ..
            } => sequence(*number)?,
            Self::Presence { users } => {
                bounded_count(users.len(), MAX_DIRECTORY_ENTRIES, "presence")?
            }
            Self::Worktrees { worktrees } => {
                bounded_count(worktrees.len(), MAX_DIRECTORY_ENTRIES, "worktrees")?;
                for tree in worktrees {
                    identifier(&tree.reference)?;
                }
            }
            Self::Children { children } => {
                bounded_count(children.len(), MAX_DIRECTORY_ENTRIES, "children")?;
                for child in children {
                    validate_child(child)?;
                }
            }
            Self::Child { child } => validate_child(child)?,
            Self::ChildRemove { thread_id } => identifier(thread_id)?,
            Self::Comment { comment } => {
                identifier(&comment.id)?;
                identifier(&comment.chain)?;
                identifier(&comment.delivered_with)?;
                if let Some(previous) = &comment.after_id {
                    identifier(previous)?;
                }
                bounded_count(
                    comment.co_authors.len(),
                    MAX_DIRECTORY_ENTRIES,
                    "comment co-authors",
                )?;
            }
            Self::Error { error: failure } => {
                identifier(&failure.code)?;
                if failure.message.is_empty() {
                    return Err(error("Delta error has no message"));
                }
            }
            Self::Turn { .. } | Self::Live => (),
        }
        Ok(())
    }
}

fn validate_nested(value: &Value) -> Result<(), WatchError> {
    match value["type"].as_str() {
        Some("ready") if !value["producer"].is_null() => {
            let producer = &value["producer"];
            required(
                producer,
                &["source", "external_id", "next_sequence", "turn"],
            )?;
            if !producer["turn"].is_null() {
                required(&producer["turn"], &["sequence", "status"])?;
            }
        }
        Some("item") => {
            required(
                &value["item"],
                &[
                    "id",
                    "kind",
                    "after_id",
                    "turn_id",
                    "author",
                    "created_at",
                    "open",
                    "content",
                ],
            )?;
            if !value["item"]["author"].is_null() {
                validate_user(&value["item"]["author"])?;
            }
        }
        Some("turn") if !value["user"].is_null() => validate_user(&value["user"])?,
        Some("presence") => {
            if let Some(users) = value["users"].as_array() {
                for user in users {
                    validate_user(user)?;
                }
            }
        }
        Some("comment") => {
            let comment = &value["comment"];
            required(
                comment,
                &[
                    "id",
                    "chain",
                    "after_id",
                    "delivered_with",
                    "routed_to",
                    "author",
                    "co_authors",
                    "agent",
                    "created_at",
                    "text",
                    "target",
                ],
            )?;
            validate_user(&comment["author"])?;
            if let Some(users) = comment["co_authors"].as_array() {
                for user in users {
                    validate_user(user)?;
                }
            }
        }
        _ => (),
    }
    Ok(())
}

fn validate_user(value: &Value) -> Result<(), WatchError> {
    required(value, &["user_id", "name"])
}
fn validate_child(child: &Child) -> Result<(), WatchError> {
    identifier(&child.thread_id)?;
    identifier(&child.reference)?;
    identifier(&child.kind)?;
    identifier(&child.status)
}
fn required(value: &Value, fields: &[&str]) -> Result<(), WatchError> {
    let object = value
        .as_object()
        .ok_or_else(|| error("Expected a Delta watch object"))?;
    for field in fields {
        if !object.contains_key(*field) {
            return Err(error(format!("Delta watch object lacks {field}")));
        }
    }
    Ok(())
}
fn identifier(value: &str) -> Result<(), WatchError> {
    if value.trim().is_empty() || value.len() > 4096 || value.chars().any(char::is_control) {
        return Err(error(
            "Delta watch identifier must be nonblank, bounded, and contain no controls",
        ));
    }
    Ok(())
}
fn sequence(value: u64) -> Result<(), WatchError> {
    if value == 0 || value > MAX_SEQUENCE {
        return Err(error("Delta watch sequence is not a positive safe integer"));
    }
    Ok(())
}
fn bounded_count(count: usize, maximum: usize, kind: &str) -> Result<(), WatchError> {
    if count > maximum {
        return Err(error(format!(
            "Delta watch {kind} exceeds its {maximum}-entry limit"
        )));
    }
    Ok(())
}
fn error(message: impl Into<String>) -> WatchError {
    WatchError(message.into())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WatchPhase {
    AwaitingReady,
    Snapshot,
    Live,
    Paused,
}

/// Advice for the host. There is intentionally no "run remote tool/turn" case.
#[derive(Clone, Debug, PartialEq)]
pub enum WatchEffect {
    None,
    StopLocalTurn { sequence: u64 },
    Pause { error: CommandError },
}

#[derive(Clone, Serialize)]
struct StoredItem {
    item: WatchItem,
    arrived: u64,
}

/// One watched thread's replaceable snapshot. Start a reconnect with
/// [`WatchView::reconnect`]; keep the independent [`ContextLedger`] across it.
pub struct WatchView {
    thread_id: String,
    phase: WatchPhase,
    producer: Option<Producer>,
    owned_turn: Option<u64>,
    next_arrival: u64,
    items: BTreeMap<String, StoredItem>,
    comments: BTreeMap<String, Comment>,
    children: BTreeMap<String, Child>,
    presence: Vec<WatchUser>,
    worktrees: Vec<Worktree>,
    shared_turn: Option<(SharedTurnState, Option<String>, Option<WatchUser>)>,
}

impl WatchView {
    pub fn new(thread_id: impl Into<String>) -> Result<Self, WatchError> {
        let thread_id = thread_id.into();
        identifier(&thread_id)?;
        Ok(Self {
            thread_id,
            phase: WatchPhase::AwaitingReady,
            producer: None,
            owned_turn: None,
            next_arrival: 0,
            items: BTreeMap::new(),
            comments: BTreeMap::new(),
            children: BTreeMap::new(),
            presence: Vec::new(),
            worktrees: Vec::new(),
            shared_turn: None,
        })
    }

    pub fn phase(&self) -> WatchPhase {
        self.phase
    }
    pub fn producer(&self) -> Option<&Producer> {
        self.producer.as_ref()
    }
    pub fn presence(&self) -> &[WatchUser] {
        &self.presence
    }
    pub fn worktrees(&self) -> &[Worktree] {
        &self.worktrees
    }
    pub fn children(&self) -> &BTreeMap<String, Child> {
        &self.children
    }
    pub fn comments(&self) -> &BTreeMap<String, Comment> {
        &self.comments
    }
    pub fn shared_turn(&self) -> Option<&(SharedTurnState, Option<String>, Option<WatchUser>)> {
        self.shared_turn.as_ref()
    }

    /// Remember only the host's already-authorized local Delta turn. This is not
    /// a claim of ownership made by a received remote frame.
    pub fn set_owned_turn(&mut self, turn: Option<u64>) -> Result<(), WatchError> {
        if let Some(turn) = turn {
            sequence(turn)?;
        }
        self.owned_turn = turn;
        Ok(())
    }

    /// Clear the remote snapshot; do not reset the host's local turn identity or
    /// discard its delivered-content ledger. This method does not reconnect IO.
    pub fn reconnect(&mut self) {
        self.phase = WatchPhase::AwaitingReady;
        self.producer = None;
        self.next_arrival = 0;
        self.items.clear();
        self.comments.clear();
        self.children.clear();
        self.presence.clear();
        self.worktrees.clear();
        self.shared_turn = None;
    }

    pub fn apply_line(&mut self, line: &str) -> Result<WatchEffect, WatchError> {
        match WatchEvent::parse(line) {
            Ok(Some(event)) => self.apply(event),
            Ok(None) => Ok(WatchEffect::None),
            Err(cause) => {
                self.phase = WatchPhase::Paused;
                Err(cause)
            }
        }
    }

    pub fn apply(&mut self, event: WatchEvent) -> Result<WatchEffect, WatchError> {
        if self.phase == WatchPhase::Paused {
            return Err(error("Delta watch is paused; a fresh snapshot is required"));
        }
        let answer = self.apply_inner(event);
        if answer.is_err() {
            self.phase = WatchPhase::Paused;
        }
        answer
    }

    fn apply_inner(&mut self, event: WatchEvent) -> Result<WatchEffect, WatchError> {
        event.validate()?;
        if let WatchEvent::Error { error: failure } = event {
            self.phase = WatchPhase::Paused;
            return Ok(WatchEffect::Pause { error: failure });
        }
        if self.phase == WatchPhase::AwaitingReady && !matches!(event, WatchEvent::Ready { .. }) {
            return Err(error("Delta watch snapshot did not begin with ready"));
        }
        let mut effect = WatchEffect::None;
        match event {
            WatchEvent::Ready {
                thread_id,
                producer,
            } => {
                if self.phase != WatchPhase::AwaitingReady || thread_id != self.thread_id {
                    return Err(error(
                        "Delta watch ready changed thread or restarted without a reconnect",
                    ));
                }
                if let Some(turn) = producer
                    .as_ref()
                    .and_then(|producer| producer.turn.as_ref())
                {
                    effect = self.turn_effect(turn.sequence, turn.status);
                }
                self.producer = producer;
                self.phase = WatchPhase::Snapshot;
            }
            WatchEvent::Live => {
                if self.phase != WatchPhase::Snapshot {
                    return Err(error("Delta watch live did not finish a snapshot"));
                }
                self.phase = WatchPhase::Live;
            }
            WatchEvent::Item { item } => {
                let arrived = match self.items.get(&item.id) {
                    Some(previous) => previous.arrived,
                    None => {
                        self.next_arrival = self
                            .next_arrival
                            .checked_add(1)
                            .ok_or_else(|| error("Delta watch arrival counter exhausted"))?;
                        self.next_arrival
                    }
                };
                self.items
                    .insert(item.id.clone(), StoredItem { item, arrived });
                bounded_count(self.items.len(), MAX_ITEMS, "items")?;
                self.ordered_items()?;
            }
            WatchEvent::ItemAppend { id, text } => {
                let item = self
                    .items
                    .get_mut(&id)
                    .ok_or_else(|| error("Delta appended text to an unknown item"))?;
                let text_slot = item
                    .item
                    .content
                    .get("text")
                    .and_then(Value::as_str)
                    .ok_or_else(|| error("Delta text append targets a non-text item"))?;
                let mut combined =
                    String::with_capacity(text_slot.len().saturating_add(text.len()));
                combined.push_str(text_slot);
                combined.push_str(&text);
                item.item
                    .content
                    .insert("text".into(), Value::String(combined));
            }
            WatchEvent::Remove { id } => {
                self.items.remove(&id);
            }
            WatchEvent::Turn {
                state,
                turn_id,
                user,
            } => {
                self.shared_turn = Some((state, turn_id, user));
            }
            WatchEvent::ProducerTurn { sequence, status } => {
                effect = self.turn_effect(sequence, status);
                if let Some(producer) = &mut self.producer {
                    producer.turn = Some(ProducerTurn { sequence, status });
                }
            }
            WatchEvent::Presence { users } => {
                self.presence = users;
            }
            WatchEvent::Worktrees { worktrees } => {
                self.worktrees = worktrees;
            }
            WatchEvent::Children { children } => {
                self.children = children
                    .into_iter()
                    .map(|child| (child.thread_id.clone(), child))
                    .collect();
            }
            WatchEvent::Child { child } => {
                self.children.insert(child.thread_id.clone(), child);
                bounded_count(self.children.len(), MAX_DIRECTORY_ENTRIES, "children")?;
            }
            WatchEvent::ChildRemove { thread_id } => {
                self.children.remove(&thread_id);
            }
            WatchEvent::Comment { comment } => {
                self.comments.insert(comment.id.clone(), comment);
                bounded_count(self.comments.len(), MAX_COMMENTS, "comments")?;
            }
            WatchEvent::CommentRemove { id } => {
                self.comments.remove(&id);
            }
            WatchEvent::Error { .. } => unreachable!("error envelope handled above"),
        }
        self.check_budget()?;
        Ok(effect)
    }

    fn turn_effect(&self, sequence: u64, status: ProducerTurnStatus) -> WatchEffect {
        if self.owned_turn == Some(sequence) && status == ProducerTurnStatus::Superseded {
            WatchEffect::StopLocalTurn { sequence }
        } else {
            WatchEffect::None
        }
    }

    /// Ordered visible IDs. A missing predecessor remains unresolved rather
    /// than promoting its children to the beginning of the transcript.
    pub fn ordered_item_ids(&self) -> Result<Vec<&str>, WatchError> {
        Ok(self
            .ordered_items()?
            .into_iter()
            .map(|stored| stored.item.id.as_str())
            .collect())
    }

    fn ordered_items(&self) -> Result<Vec<&StoredItem>, WatchError> {
        let mut checked = BTreeSet::new();
        for id in self.items.keys() {
            let mut path = BTreeSet::new();
            let mut next = Some(id.as_str());
            while let Some(id) = next {
                if checked.contains(id) {
                    break;
                }
                let Some(stored) = self.items.get(id) else {
                    break;
                };
                if !path.insert(id) {
                    return Err(error("Delta item predecessor chain contains a cycle"));
                }
                next = stored.item.after_id.as_deref();
            }
            checked.extend(path);
        }
        let mut successors: BTreeMap<Option<&str>, Vec<&StoredItem>> = BTreeMap::new();
        for stored in self.items.values() {
            successors
                .entry(stored.item.after_id.as_deref())
                .or_default()
                .push(stored);
        }
        for siblings in successors.values_mut() {
            siblings.sort_by_key(|stored| stored.arrived);
        }
        let mut queue: Vec<&StoredItem> = successors
            .get(&None)
            .into_iter()
            .flatten()
            .rev()
            .copied()
            .collect();
        let mut ordered = Vec::new();
        while let Some(stored) = queue.pop() {
            ordered.push(stored);
            if let Some(children) = successors.get(&Some(stored.item.id.as_str())) {
                queue.extend(children.iter().rev().copied());
            }
        }
        Ok(ordered)
    }

    fn check_budget(&self) -> Result<(), WatchError> {
        #[derive(Serialize)]
        struct Retained<'a> {
            producer: &'a Option<Producer>,
            items: &'a BTreeMap<String, StoredItem>,
            comments: &'a BTreeMap<String, Comment>,
            children: &'a BTreeMap<String, Child>,
            presence: &'a [WatchUser],
            worktrees: &'a [Worktree],
            shared_turn: &'a Option<(SharedTurnState, Option<String>, Option<WatchUser>)>,
        }
        bounded_json(&Retained {
            producer: &self.producer,
            items: &self.items,
            comments: &self.comments,
            children: &self.children,
            presence: &self.presence,
            worktrees: &self.worktrees,
            shared_turn: &self.shared_turn,
        })
    }

    /// Project only public, inert data after `live`. Remote thoughts and unknown
    /// item kinds stay in the UI snapshot but are never included in model context.
    /// A ledger is updated only after the whole bounded change plan validates.
    pub fn context_changes(
        &mut self,
        ledger: &mut ContextLedger,
    ) -> Result<Vec<ContextChange>, WatchError> {
        if self.phase != WatchPhase::Live {
            return Err(error(
                "Delta context is unavailable until a complete live snapshot",
            ));
        }
        let changes = self.plan_context_changes(ledger);
        if changes.is_err() {
            self.phase = WatchPhase::Paused;
        }
        changes
    }

    fn plan_context_changes(
        &self,
        ledger: &mut ContextLedger,
    ) -> Result<Vec<ContextChange>, WatchError> {
        bounded_count(ledger.items.len(), MAX_ITEMS, "delivered items")?;
        bounded_count(ledger.comments.len(), MAX_COMMENTS, "delivered comments")?;
        bounded_json(ledger)?;
        let mut next = ledger.clone();
        let mut changes = Vec::new();
        for id in ledger.items.keys() {
            if self
                .items
                .get(id)
                .is_none_or(|stored| project_item(&stored.item).is_none())
            {
                next.items.remove(id);
                changes.push(ContextChange::RemoveItem { id: id.clone() });
            }
        }
        for stored in self.ordered_items()? {
            if let Some(item) = project_item(&stored.item)
                && next.items.get(&item.id) != Some(&item)
            {
                next.items.insert(item.id.clone(), item.clone());
                changes.push(ContextChange::UpsertItem { item });
            }
        }
        for id in ledger.comments.keys() {
            if !self.comments.contains_key(id) {
                next.comments.remove(id);
                changes.push(ContextChange::RemoveComment { id: id.clone() });
            }
        }
        for comment in self.comments.values() {
            if next.comments.get(&comment.id) != Some(comment) {
                next.comments.insert(comment.id.clone(), comment.clone());
                changes.push(ContextChange::UpsertComment {
                    comment: comment.clone(),
                });
            }
        }
        bounded_json(&next)?;
        *ledger = next;
        Ok(changes)
    }
}

/// Last public values delivered by a host. Keep this across reconnects; callers
/// choose their own durable storage before injecting new context into an agent.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct ContextLedger {
    items: BTreeMap<String, ContextItem>,
    comments: BTreeMap<String, Comment>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct ContextItem {
    pub id: String,
    pub after_id: Option<String>,
    pub kind: String,
    pub author: Option<WatchUser>,
    pub content: Value,
}

/// These values are data for plugin-authored context, never tool requests or
/// user-authored approvals. The caller must preserve that provenance in Rig.
#[derive(Clone, Debug, PartialEq)]
pub enum ContextChange {
    UpsertItem { item: ContextItem },
    RemoveItem { id: String },
    UpsertComment { comment: Comment },
    RemoveComment { id: String },
}

fn project_item(item: &WatchItem) -> Option<ContextItem> {
    let content = match item.kind.as_str() {
        "user_message" | "agent_message" => {
            let text = item.content.get("text").and_then(Value::as_str)?;
            json!({"text":text})
        }
        "tool_call" | "tool_call_update" => {
            let mut fields = serde_json::Map::new();
            for name in [
                "toolCallId",
                "name",
                "title",
                "kind",
                "status",
                "rawInput",
                "rawOutput",
            ] {
                if let Some(value) = item.content.get(name) {
                    fields.insert(name.into(), value.clone());
                }
            }
            Value::Object(fields)
        }
        _ => return None,
    };
    Some(ContextItem {
        id: item.id.clone(),
        after_id: item.after_id.clone(),
        kind: item.kind.clone(),
        author: item.author.clone(),
        content,
    })
}

fn bounded_json(value: &impl Serialize) -> Result<(), WatchError> {
    struct Count(usize);
    impl Write for Count {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if bytes.len() > MAX_STATE_BYTES - self.0 {
                return Err(io::Error::other("Delta retained state exceeds 8 MiB"));
            }
            self.0 += bytes.len();
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    serde_json::to_writer(&mut Count(0), value)
        .map_err(|cause| error(format!("Delta watch state limit: {cause}")))
}

#[cfg(test)]
mod tests;
