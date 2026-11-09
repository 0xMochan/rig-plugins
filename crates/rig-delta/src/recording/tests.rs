use super::*;
use rig_harness::rig_core::conversation::{Author, Entry};
use rig_harness::rig_core::message::{
    AssistantMessage, DocumentSourceKind, Image, Reasoning, Text, ToolCall, ToolFunction, ToolName,
};

fn receipt(sequence: u64, duplicate: bool) -> String {
    json!({
        "operation":"appended", "thread_id":"thread_test", "sequence":sequence,
        "next_sequence":sequence + 1, "duplicate":duplicate,
        "disposition":if duplicate {"duplicate"} else {"recorded"},
        "turn_sequence":null, "version":"actor.42",
    })
    .to_string()
        + "\n"
}

fn records(batch: &Batch) -> Vec<Value> {
    batch
        .input()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn call(name: &str, id: &str) -> ToolCall {
    ToolCall::from_wire(
        id,
        ToolFunction::new(ToolName::new(name).unwrap(), json!({"path":"src/lib.rs"})),
    )
}

#[test]
fn snapshot_preserves_tools_and_excludes_private_and_recursive_transport_content() {
    let home = tempfile::tempdir().unwrap();
    let mut conversation = Conversation::default();
    conversation.push(Message::system("private system instructions"));
    conversation
        .push(Entry::new(Message::user("private plugin context")).by(Author::plugin("test")));
    conversation.push(Message::user("Inspect the change"));
    let inspect = call("read", "inspect-1");
    let transport = call("delta_sync", "transport-1");
    conversation.push(Message::Assistant(AssistantMessage::new(vec![
        AssistantContent::Reasoning(Reasoning::new("private reasoning")),
        AssistantContent::Text(Text::new("I will read it")),
        AssistantContent::ToolCall(inspect.clone()),
        AssistantContent::ToolCall(transport.clone()),
    ])));
    conversation.push(Message::User {
        content: vec![
            UserContent::ToolResult(inspect.result(vec![ToolResultContent::Json {
                value: json!({"lines":["fn main() {}"]}),
            }])),
            UserContent::ToolResult(
                transport.result(vec![ToolResultContent::text("private transport payload")]),
            ),
        ],
    });
    conversation.push(Message::User {
        content: vec![UserContent::Image(Image {
            data: DocumentSourceKind::Base64("private image bytes".into()),
            ..Image::default()
        })],
    });
    let batch = prepare(home.path(), "agent-1", "thread_test", &conversation, 1).unwrap();
    let records = records(&batch);
    assert_eq!(records.len(), 5);
    assert_eq!(records[0]["update"]["sessionUpdate"], "user_message");
    assert_eq!(records[1]["update"]["sessionUpdate"], "agent_message");
    assert_eq!(records[2]["update"]["toolCallId"], "inspect-1");
    assert_eq!(records[2]["update"]["status"], "completed");
    assert_eq!(records[2]["update"]["rawInput"]["path"], "src/lib.rs");
    assert_eq!(records[3]["update"]["toolCallId"], "inspect-1");
    assert_eq!(
        records[3]["update"]["rawOutput"][0]["value"]["lines"][0],
        "fn main() {}"
    );
    assert_eq!(
        records[4]["update"]["content"][0]["text"],
        "[Non-text attachment omitted by Rig Delta export.]"
    );
    assert!(!batch.input().contains("private"));
    assert!(!batch.input().contains("transport-1"));
    assert!(
        records
            .iter()
            .all(|record| record.get("turnSequence").is_none())
    );
    // Records exist before the caller has any opportunity to start append.
    let saved = fs::read_to_string(home.path().join("delta/agent-1/outbox.json")).unwrap();
    assert!(saved.contains("Inspect the change"));
}

#[test]
fn cancellation_and_lost_receipts_replay_identical_records_before_new_activity() {
    let home = tempfile::tempdir().unwrap();
    let mut conversation = Conversation::default();
    conversation.push(Message::user("first"));
    conversation.push(Message::assistant("second"));
    let batch = prepare(home.path(), "agent-1", "thread_test", &conversation, 1).unwrap();
    let original = batch.input().to_owned();
    drop(batch); // Cancellation or no receipt does not advance the journal.
    conversation.push(Message::user("third"));
    // The remote side recorded both messages, but their receipts were lost.
    let mut replay = prepare(home.path(), "agent-1", "thread_test", &conversation, 3).unwrap();
    assert_eq!(replay.input(), original);
    let confirmed = replay
        .acknowledge(&(receipt(1, true) + &receipt(2, true)))
        .unwrap();
    assert_eq!(confirmed.confirmed, 2);
    assert_eq!(confirmed.pending, 0);
    assert!(confirmed.more);
    drop(replay);
    let mut next = prepare(home.path(), "agent-1", "thread_test", &conversation, 3).unwrap();
    assert_eq!(records(&next)[0]["sequence"], 3);
    assert_eq!(next.pending_count(), 1);
    next.acknowledge(&receipt(3, false)).unwrap();
    drop(next);
    // Compaction removes old entries without resetting the recording sequence.
    let last = conversation.entry(2).unwrap();
    conversation.set_entries(vec![last], Author::plugin("test-compaction"));
    conversation.push(Message::assistant("fourth"));
    let next = prepare(home.path(), "agent-1", "thread_test", &conversation, 4).unwrap();
    assert_eq!(next.pending_count(), 1);
    assert_eq!(records(&next)[0]["sequence"], 4);
}

#[test]
fn only_matching_receipt_prefix_is_removed_and_fenced_records_never_confirm() {
    let home = tempfile::tempdir().unwrap();
    let mut conversation = Conversation::default();
    conversation.push(Message::user("first"));
    conversation.push(Message::assistant("second"));
    let mut batch = prepare(home.path(), "agent-1", "thread_test", &conversation, 1).unwrap();
    // An append may record a prefix, then fail with a structured error.
    let error = batch
        .acknowledge(&(receipt(1, false) + "{\"error\":{\"code\":\"offline\"}}\n"))
        .err()
        .unwrap();
    assert!(error.contains("Confirmed 1 records; 1 remain durable"));
    drop(batch);
    let mut resumed = prepare(home.path(), "agent-1", "thread_test", &conversation, 2).unwrap();
    assert_eq!(records(&resumed)[0]["sequence"], 2);
    let mut fenced: Value = serde_json::from_str(&receipt(2, false)).unwrap();
    fenced["disposition"] = json!("fenced");
    assert!(resumed.acknowledge(&fenced.to_string()).is_err());
    assert_eq!(resumed.pending_count(), 1);
    let mut wrong_thread: Value = serde_json::from_str(&receipt(2, false)).unwrap();
    wrong_thread["thread_id"] = json!("different_thread");
    assert!(resumed.acknowledge(&wrong_thread.to_string()).is_err());
    assert_eq!(resumed.pending_count(), 1);
    resumed.acknowledge(&receipt(2, false)).unwrap();
    assert_eq!(resumed.pending_count(), 0);
}

#[test]
fn missing_corrupt_mismatched_or_locked_state_refuses_without_resetting() {
    let home = tempfile::tempdir().unwrap();
    let mut conversation = Conversation::default();
    conversation.push(Message::user("first"));
    assert!(prepare(home.path(), "../escape", "thread_test", &conversation, 1).is_err());
    assert!(prepare(home.path(), "agent-1", "thread_test", &conversation, 2).is_err());
    let path = home.path().join("delta/agent-1/outbox.json");
    assert!(!path.exists());
    let batch = prepare(home.path(), "agent-1", "thread_test", &conversation, 1).unwrap();
    assert!(prepare(home.path(), "agent-1", "thread_test", &conversation, 1).is_err());
    drop(batch);
    let original = fs::read(&path).unwrap();
    assert!(prepare(home.path(), "agent-1", "other_thread", &conversation, 1).is_err());
    assert_eq!(fs::read(&path).unwrap(), original);
    assert!(prepare(home.path(), "agent-1", "thread_test", &conversation, 3).is_err());
    assert_eq!(fs::read(&path).unwrap(), original);
    fs::write(&path, b"not valid journal data").unwrap();
    assert!(prepare(home.path(), "agent-1", "thread_test", &conversation, 1).is_err());
    assert_eq!(fs::read(&path).unwrap(), b"not valid journal data");
}

#[test]
fn bounded_batches_defer_whole_records_without_truncating_or_marking_them_seen() {
    let home = tempfile::tempdir().unwrap();
    let mut conversation = Conversation::default();
    for number in 0..65 {
        conversation.push(Message::user(format!("message {number}")));
    }
    let mut batch = prepare(home.path(), "agent-1", "thread_test", &conversation, 1).unwrap();
    assert_eq!(batch.pending_count(), MAX_BATCH_RECORDS);
    let receipts: String = (1..=64).map(|sequence| receipt(sequence, false)).collect();
    assert!(batch.acknowledge(&receipts).unwrap().more);
    drop(batch);
    let next = prepare(home.path(), "agent-1", "thread_test", &conversation, 65).unwrap();
    assert_eq!(records(&next)[0]["sequence"], 65);
    assert_eq!(next.pending_count(), 1);
    drop(next);
    let mut too_large = Conversation::default();
    too_large.push(Message::user("x".repeat(MAX_BATCH_BYTES)));
    assert!(prepare(home.path(), "large-agent", "thread_test", &too_large, 1).is_err());
    assert!(!home.path().join("delta/large-agent/outbox.json").exists());
}
