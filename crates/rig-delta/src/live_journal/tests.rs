use super::*;
use rig_harness::rig_core::conversation::{Author, Entry};
use rig_harness::rig_core::message::{
    AssistantMessage, ImageMediaType, Reasoning, Text, ToolCall, ToolFunction, ToolName,
};
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;

fn records(batch: &Batch) -> Vec<Value> {
    batch
        .input
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn receipt(record: &Value, disposition: &str) -> String {
    json!({
        "operation":"appended", "thread_id":"thread_test",
        "sequence":record["sequence"], "next_sequence":record["sequence"].as_u64().unwrap() + 1,
        "duplicate":disposition == "duplicate", "disposition":disposition,
        "turn_sequence":record.get("turnSequence").cloned().unwrap_or(Value::Null), "version":"actor.42",
    }).to_string() + "\n"
}

fn call(name: &str, id: &str) -> ToolCall {
    ToolCall::from_wire(
        id,
        ToolFunction::new(ToolName::new(name).unwrap(), json!({"path":"src/lib.rs"})),
    )
}

#[test]
fn lost_receipts_replay_the_frozen_batch_before_later_captured_rows() {
    let home = tempfile::tempdir().unwrap();
    let mut conversation = Conversation::default();
    conversation.push(Message::user("first"));
    conversation.push(Message::assistant("second"));
    let mut journal = Journal::open(
        home.path(),
        "agent-1",
        "thread_test",
        1,
        InitialMode::ShareSnapshot,
    )
    .unwrap();
    assert_eq!(
        journal
            .capture(&conversation, None, ExportOptions::default())
            .unwrap(),
        2
    );
    let original = journal.batch().unwrap();
    assert_eq!(original.count, 2);
    let saved = fs::read_to_string(home.path().join("delta/agent-1/live.json")).unwrap();
    assert!(saved.contains("first"));
    assert!(saved.contains("batch_end"));
    drop(journal); // Process/caller cancellation preserves bytes before any receipt.

    conversation.push(Message::user("third"));
    let mut resumed = Journal::open(
        home.path(),
        "agent-1",
        "thread_test",
        3,
        InitialMode::ShareSnapshot,
    )
    .unwrap();
    assert_eq!(
        resumed
            .capture(&conversation, None, ExportOptions::default())
            .unwrap(),
        1
    );
    assert_eq!(resumed.batch().unwrap().input, original.input);
    let receipts: String = records(&original)
        .iter()
        .map(|record| receipt(record, "duplicate"))
        .collect();
    let ack = resumed.acknowledge(&receipts).unwrap();
    assert_eq!(ack.confirmed, 2);
    assert_eq!(ack.pending, 1);
    assert!(ack.error.is_none());
    let next = resumed.batch().unwrap();
    assert_eq!(next.count, 1);
    assert_eq!(records(&next)[0]["sequence"], 3);
    assert!(resumed.validate_frontier(3).is_ok());
    assert!(resumed.validate_frontier(1).is_err());
}

#[test]
fn fenced_prefix_and_late_tool_output_keep_the_original_turn() {
    let home = tempfile::tempdir().unwrap();
    let mut conversation = Conversation::default();
    let mut journal = Journal::open(
        home.path(),
        "agent-1",
        "thread_test",
        1,
        InitialMode::ShareSnapshot,
    )
    .unwrap();
    journal
        .capture(&conversation, None, ExportOptions::default())
        .unwrap();
    let old_turn = journal.start_turn("native-turn-one").unwrap();
    assert_eq!(journal.start_turn("native-turn-one").unwrap(), old_turn);
    assert!(journal.start_turn("overlapping-native-turn").is_err());
    let tool = call("read", "read-one");
    conversation.push(Message::Assistant(AssistantMessage::new(vec![
        AssistantContent::ToolCall(tool.clone()),
    ])));
    journal
        .capture(&conversation, Some(old_turn), ExportOptions::default())
        .unwrap();
    journal.mark_lost(old_turn).unwrap();
    let new_turn = journal.start_turn("native-turn-two").unwrap();
    conversation.push(Message::User {
        content: vec![UserContent::ToolResult(
            tool.result(vec![ToolResultContent::text("late old result")]),
        )],
    });
    conversation.push(Message::user("new prompt"));
    journal
        .capture(&conversation, Some(new_turn), ExportOptions::default())
        .unwrap();
    journal.finish_turn(old_turn).unwrap();
    let next = journal.next_sequence();
    journal.finish_turn(old_turn).unwrap();
    assert_eq!(journal.next_sequence(), next);
    assert!(journal.start_turn("native-turn-one").is_err());

    let batch = journal.batch().unwrap();
    let lines = records(&batch);
    for record in &lines {
        if record["update"]["toolCallId"] == "read-one" {
            assert_eq!(record["turnSequence"], old_turn);
        }
    }
    assert!(
        lines
            .iter()
            .any(|record| record["update"]["rawOutput"][0]["text"] == "late old result")
    );
    let ack = journal
        .acknowledge(&(receipt(&lines[0], "fenced") + "{\"error\":{\"code\":\"offline\"}}\n"))
        .unwrap();
    assert_eq!(ack.confirmed, 0);
    assert_eq!(journal.confirmed_sequence(), 1);
    assert_eq!(ack.fenced, vec![old_turn]);
    assert!(ack.error.is_some());
    assert_eq!(ack.pending, lines.len() - 1);
    drop(journal);

    let mut resumed = Journal::open(
        home.path(),
        "agent-1",
        "thread_test",
        2,
        InitialMode::ShareSnapshot,
    )
    .unwrap();
    assert!(resumed.lost_turns().contains(&old_turn));
    let rest = resumed.batch().unwrap();
    let mut changed_turn: Value =
        serde_json::from_str(&receipt(&records(&rest)[0], "recorded")).unwrap();
    changed_turn["turn_sequence"] = json!(new_turn);
    let refused = resumed.acknowledge(&changed_turn.to_string()).unwrap();
    assert_eq!(refused.confirmed, 0);
    assert!(refused.error.is_some());
    assert_eq!(resumed.batch().unwrap().input, rest.input);
    let receipts: String = records(&rest)
        .iter()
        .map(|record| {
            receipt(
                record,
                if record["turnSequence"] == old_turn {
                    "fenced"
                } else {
                    "recorded"
                },
            )
        })
        .collect();
    let ack = resumed.acknowledge(&receipts).unwrap();
    assert_eq!(ack.fenced, vec![old_turn]);
    assert_eq!(ack.pending, 0);
    assert!(ack.error.is_none());
}

#[test]
fn joining_baselines_prior_history_and_never_recaptures_remote_plugin_context() {
    let home = tempfile::tempdir().unwrap();
    let mut conversation = Conversation::default();
    conversation.push(Message::user("old session prompt"));
    conversation.push(Message::assistant("old session answer"));
    let old_tool = call("read", "pre-join-read");
    conversation.push(Message::Assistant(AssistantMessage::new(vec![
        AssistantContent::ToolCall(old_tool.clone()),
    ])));
    let old_entry_id = conversation.entry(0).unwrap().id.unwrap().to_string();
    let mut journal = Journal::open(
        home.path(),
        "fresh-agent",
        "thread_test",
        1,
        InitialMode::Joined,
    )
    .unwrap();
    assert!(journal.start_turn("too-early").is_err());
    assert_eq!(
        journal
            .capture(&conversation, None, ExportOptions::default())
            .unwrap(),
        0
    );
    assert_eq!(journal.pending_count(), 0);
    assert!(
        journal
            .alias_entry(&old_entry_id, "stream:old", None)
            .is_err()
    );
    let mut prior: Vec<_> = (0..conversation.messages().len())
        .map(|index| conversation.entry(index).unwrap())
        .collect();
    prior[0].message = std::sync::Arc::new(Message::user("amended old session prompt"));
    conversation.set_entries(prior, Author::plugin("edit-prior-history"));
    conversation.push(Message::User {
        content: vec![UserContent::ToolResult(
            old_tool.result(vec![ToolResultContent::text("late old session result")]),
        )],
    });
    conversation
        .push(Entry::new(Message::user("remote collaborator context")).by(Author::plugin("delta")));
    let turn = journal.start_turn("joined-turn").unwrap();
    conversation.push(Message::user("new local prompt"));
    conversation.push(Message::assistant("new local answer"));
    assert_eq!(
        journal
            .capture(&conversation, Some(turn), ExportOptions::default())
            .unwrap(),
        2
    );
    assert_eq!(
        journal
            .capture(&conversation, Some(turn), ExportOptions::default())
            .unwrap(),
        0
    );
    let batch = journal.batch().unwrap();
    assert!(!batch.input.contains("old session"));
    assert!(!batch.input.contains("remote collaborator"));
    assert!(batch.input.contains("new local"));
    for record in records(&batch) {
        assert_eq!(record["turnSequence"], turn);
    }
    // Keeping retained entry identities through compaction never allocates their
    // content a second time or changes its original turn association.
    let last = conversation
        .entry(conversation.messages().len() - 1)
        .unwrap();
    conversation.set_entries(vec![last], Author::plugin("compaction-test"));
    assert_eq!(
        journal
            .capture(&conversation, Some(turn), ExportOptions::default())
            .unwrap(),
        0
    );
}

#[test]
fn projection_requires_explicit_reasoning_media_and_omits_native_or_recursive_data() {
    let home = tempfile::tempdir().unwrap();
    let mut conversation = Conversation::default();
    conversation.push(Message::system("secret system"));
    conversation.push(Entry::new(Message::user("secret plugin prompt")).by(Author::plugin("test")));
    conversation.push(Message::user("public question"));
    conversation.push(Message::Assistant(AssistantMessage::new(vec![
        AssistantContent::Text(Text::new("public answer"))
            .with_native(json!({"provider":"secret provider payload"})),
        AssistantContent::Reasoning(Reasoning::new("secret thought")),
        AssistantContent::ToolCall(call("delta_live", "recursive-transport")),
        AssistantContent::Image(Image {
            data: DocumentSourceKind::Raw(b"hi".to_vec()),
            media_type: Some(ImageMediaType::PNG),
            ..Image::default()
        }),
    ])));
    let mut public = Journal::open(
        home.path(),
        "public-agent",
        "thread_test",
        1,
        InitialMode::ShareSnapshot,
    )
    .unwrap();
    public
        .capture(&conversation, None, ExportOptions::default())
        .unwrap();
    let batch = public.batch().unwrap();
    assert!(!batch.input.contains("secret"));
    assert!(!batch.input.contains("recursive-transport"));
    assert!(batch.input.contains("Image omitted"));
    assert_eq!(public.options(), Some(ExportOptions::default()));
    let before = fs::read(home.path().join("delta/public-agent/live.json")).unwrap();
    assert!(
        public
            .capture(
                &conversation,
                None,
                ExportOptions {
                    reasoning: true,
                    media: true
                }
            )
            .is_err()
    );
    assert_eq!(
        fs::read(home.path().join("delta/public-agent/live.json")).unwrap(),
        before
    );

    let mut explicit = Journal::open(
        home.path(),
        "explicit-agent",
        "thread_test",
        1,
        InitialMode::ShareSnapshot,
    )
    .unwrap();
    explicit
        .capture(
            &conversation,
            None,
            ExportOptions {
                reasoning: true,
                media: true,
            },
        )
        .unwrap();
    let batch = explicit.batch().unwrap();
    let records = records(&batch);
    assert!(
        records
            .iter()
            .any(|record| record["update"]["sessionUpdate"] == "agent_thought")
    );
    assert!(
        records
            .iter()
            .any(|record| record["update"]["content"][0]["type"] == "image"
                && record["update"]["content"][0]["data"] == "aGk=")
    );
    assert!(!batch.input.contains("secret system"));
    assert!(!batch.input.contains("secret provider"));
    assert!(!batch.input.contains("secret plugin"));
}

#[test]
fn legacy_migration_preserves_wire_bytes_and_frontier_without_rewriting_old_state() {
    let home = tempfile::tempdir().unwrap();
    let mut conversation = Conversation::default();
    conversation.push(Message::user("legacy first"));
    conversation.push(Message::assistant("legacy second"));
    let old =
        crate::recording::prepare(home.path(), "agent-1", "thread_test", &conversation, 1).unwrap();
    let input = old.input().to_owned();
    drop(old);
    let path = home.path().join("delta/agent-1/outbox.json");
    let original = fs::read(&path).unwrap();
    let mut live = Journal::open(
        home.path(),
        "agent-1",
        "thread_test",
        3,
        InitialMode::ShareSnapshot,
    )
    .unwrap();
    assert_eq!(live.next_sequence(), 3);
    assert_eq!(live.batch().unwrap().input, input);
    assert_eq!(fs::read(&path).unwrap(), original);
    assert!(
        crate::recording::prepare(home.path(), "agent-1", "thread_test", &conversation, 1).is_err()
    );
    let batch = live.batch().unwrap();
    let receipts: String = records(&batch)
        .iter()
        .map(|record| receipt(record, "duplicate"))
        .collect();
    live.acknowledge(&receipts).unwrap();
    assert_eq!(
        live.capture(&conversation, None, ExportOptions::default())
            .unwrap(),
        0
    );
    assert_eq!(live.start_turn("new-live-turn").unwrap(), 3);
    drop(live);
    let refused =
        crate::recording::prepare(home.path(), "agent-1", "thread_test", &conversation, 3)
            .err()
            .unwrap();
    assert!(refused.contains("migrated"));
    assert_eq!(fs::read(&path).unwrap(), original);
    let saved = fs::read(home.path().join("delta/agent-1/live.json")).unwrap();
    assert!(
        Journal::open(
            home.path(),
            "agent-1",
            "different-thread",
            3,
            InitialMode::ShareSnapshot
        )
        .is_err()
    );
    assert_eq!(
        fs::read(home.path().join("delta/agent-1/live.json")).unwrap(),
        saved
    );
    assert!(
        Journal::open(
            home.path(),
            "missing-agent",
            "thread_test",
            2,
            InitialMode::ShareSnapshot
        )
        .is_err()
    );
}

#[test]
fn staged_stream_reconciles_to_final_entry_and_late_partials_cannot_overwrite_it() {
    let home = tempfile::tempdir().unwrap();
    let mut conversation = Conversation::default();
    let mut journal = Journal::open(
        home.path(),
        "agent-1",
        "thread_test",
        1,
        InitialMode::ShareSnapshot,
    )
    .unwrap();
    journal
        .capture(&conversation, None, ExportOptions::default())
        .unwrap();
    let turn = journal.start_turn("stream-turn").unwrap();
    let partial = |text: &str| json!({"sessionUpdate":"agent_message", "messageId":"stream:effect-one:0", "content":[{"type":"text", "text":text}]});
    journal.stage(partial("Hel"), Some(turn)).unwrap();
    journal.stage(partial("Hello"), Some(turn)).unwrap();
    let next = journal.next_sequence();
    #[cfg(unix)]
    let unchanged_stage_inode = fs::metadata(&journal.path).unwrap().ino();
    journal.stage(partial("Hello"), Some(turn)).unwrap();
    assert_eq!(journal.next_sequence(), next);
    #[cfg(unix)]
    assert_eq!(
        fs::metadata(&journal.path).unwrap().ino(),
        unchanged_stage_inode,
        "an unchanged stage must not atomically replace/fsync the journal"
    );
    conversation.push(Message::assistant("Hello world"));
    let entry = conversation.entry(0).unwrap();
    journal
        .alias_entry(
            &entry.id.unwrap().to_string(),
            "stream:effect-one",
            Some(turn),
        )
        .unwrap();
    journal.mark_lost(turn).unwrap();
    let new_turn = journal.start_turn("subsequent-turn").unwrap();
    assert_eq!(
        journal
            .capture(&conversation, Some(new_turn), ExportOptions::default())
            .unwrap(),
        1
    );
    let next = journal.next_sequence();
    journal.stage(partial("Hell"), Some(turn)).unwrap();
    assert_eq!(journal.next_sequence(), next);
    #[cfg(unix)]
    let unchanged_capture_inode = fs::metadata(&journal.path).unwrap().ino();
    assert_eq!(
        journal
            .capture(&conversation, None, ExportOptions::default())
            .unwrap(),
        0
    );
    #[cfg(unix)]
    assert_eq!(
        fs::metadata(&journal.path).unwrap().ino(),
        unchanged_capture_inode,
        "an unchanged capture must not atomically replace/fsync the journal"
    );
    assert!(
        journal
            .alias_entry(&entry.id.unwrap().to_string(), "stream:other", Some(turn))
            .is_err()
    );
    assert_eq!(journal.open_turns().len(), 2);
    journal
        .stage(
            json!({"sessionUpdate":"session_info_update", "title":"Session title"}),
            None,
        )
        .unwrap();
    let next = journal.next_sequence();
    journal
        .stage(
            json!({"sessionUpdate":"session_info_update", "title":"Session title"}),
            None,
        )
        .unwrap();
    assert_eq!(journal.next_sequence(), next);
    assert!(
        journal
            .stage(
                json!({"sessionUpdate":"state_update", "state":"running"}),
                Some(turn)
            )
            .is_err()
    );
    assert!(journal.stage(json!({"sessionUpdate":"agent_thought", "messageId":"private", "content":[{"type":"text", "text":"not consented"}]}), Some(turn)).is_err());
    assert!(journal.stage(json!({"sessionUpdate":"agent_message", "messageId":"opaque", "content":[{"type":"text", "text":"public"}], "native":{"secret":"provider"}}), Some(turn)).is_err());
    let batch = journal.batch().unwrap();
    let messages: Vec<_> = records(&batch)
        .into_iter()
        .filter(|record| record["update"]["sessionUpdate"] == "agent_message")
        .collect();
    assert_eq!(messages.len(), 3);
    assert_eq!(
        messages.last().unwrap()["update"]["content"][0]["text"],
        "Hello world"
    );
    for record in messages {
        assert_eq!(record["update"]["messageId"], "stream:effect-one:0");
        assert_eq!(record["turnSequence"], turn);
    }
    drop(journal);
    // Old indexed alias metadata remains readable before the coalesced-reply
    // field existed; its canonical message IDs must not silently change.
    let path = home.path().join("delta/agent-1/live.json");
    let mut older: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    older["aliases"][entry.id.unwrap().to_string()]
        .as_object_mut()
        .unwrap()
        .remove("coalesced");
    fs::write(&path, serde_json::to_vec(&older).unwrap()).unwrap();
    let mut resumed = Journal::open(
        home.path(),
        "agent-1",
        "thread_test",
        1,
        InitialMode::ShareSnapshot,
    )
    .unwrap();
    assert_eq!(
        resumed
            .capture(&conversation, None, ExportOptions::default())
            .unwrap(),
        0
    );
    let next = resumed.next_sequence();
    resumed
        .stage(partial("late after resume"), Some(turn))
        .unwrap();
    assert_eq!(resumed.next_sequence(), next);
}

#[test]
fn multipart_normal_and_folded_stopped_replies_keep_coalesced_stream_identities() {
    let home = tempfile::tempdir().unwrap();
    let options = ExportOptions {
        reasoning: true,
        media: false,
    };
    for stopped in [false, true] {
        let producer = if stopped {
            "stopped-agent"
        } else {
            "normal-agent"
        };
        let mut conversation = Conversation::default();
        let mut journal = Journal::open(
            home.path(),
            producer,
            "thread_test",
            1,
            InitialMode::ShareSnapshot,
        )
        .unwrap();
        journal.capture(&conversation, None, options).unwrap();
        let turn = journal.start_turn("multipart-turn").unwrap();
        let partial = |kind: &str, suffix: &str, text: &str| json!({"sessionUpdate":kind, "messageId":format!("stream:multipart:{suffix}"), "content":[{"type":"text", "text":text}]});
        journal
            .stage(partial("agent_thought", "thought", "one "), Some(turn))
            .unwrap();
        journal
            .stage(partial("agent_message", "text", "Hello "), Some(turn))
            .unwrap();
        journal
            .stage(partial("agent_thought", "thought", "one two"), Some(turn))
            .unwrap();
        journal
            .stage(partial("agent_message", "text", "Hello world"), Some(turn))
            .unwrap();
        let message = if stopped {
            // Rig 0.46 StopTurn concatenates all partial reasoning/text and
            // commits just one reasoning block followed by one text block.
            AssistantMessage::aborted(
                None,
                vec![
                    AssistantContent::Reasoning(Reasoning::new("one two")),
                    AssistantContent::Text(Text::new("Hello world")),
                ],
                "stopped: superseded",
            )
        } else {
            AssistantMessage::new(vec![
                AssistantContent::Reasoning(Reasoning::new("one ")),
                AssistantContent::Text(Text::new("Hello ")),
                AssistantContent::Reasoning(Reasoning::new("two")),
                AssistantContent::Text(Text::new("world")),
                AssistantContent::ToolCall(call("read", "multipart-read")),
                AssistantContent::Image(Image {
                    data: DocumentSourceKind::Unknown,
                    ..Image::default()
                }),
            ])
        };
        conversation.push(Message::Assistant(message));
        let entry_id = conversation.entry(0).unwrap().id.unwrap().to_string();
        journal
            .alias_reply(&entry_id, "stream:multipart", Some(turn))
            .unwrap();
        assert!(
            journal
                .alias_entry(&entry_id, "stream:multipart", Some(turn))
                .is_err()
        );
        journal.mark_lost(turn).unwrap();
        let subsequent = journal.start_turn("subsequent-turn").unwrap();
        assert_eq!(
            journal
                .capture(&conversation, Some(subsequent), options)
                .unwrap(),
            if stopped { 0 } else { 2 }
        );
        let next = journal.next_sequence();
        journal
            .stage(partial("agent_message", "text", "late partial"), Some(turn))
            .unwrap();
        journal
            .stage(
                partial("agent_thought", "thought", "late thought"),
                Some(turn),
            )
            .unwrap();
        assert_eq!(journal.next_sequence(), next);
        let batch = journal.batch().unwrap();
        let mut latest = BTreeMap::new();
        for record in records(&batch) {
            if let Some(id) = record["update"]["messageId"].as_str() {
                assert!(!matches!(id, "stream:multipart:0" | "stream:multipart:1"));
                assert_eq!(record["turnSequence"], turn);
                latest.insert(id.to_owned(), record["update"].clone());
            }
            if record["update"]["toolCallId"] == "multipart-read" {
                assert_eq!(record["turnSequence"], turn);
            }
        }
        assert_eq!(
            latest["stream:multipart:text"]["content"][0]["text"],
            "Hello world"
        );
        assert_eq!(
            latest["stream:multipart:thought"]["content"][0]["text"],
            "one two"
        );
        if !stopped {
            assert!(latest.contains_key("stream:multipart:5"));
        }
        drop(journal);
        let mut resumed = Journal::open(
            home.path(),
            producer,
            "thread_test",
            1,
            InitialMode::ShareSnapshot,
        )
        .unwrap();
        assert_eq!(
            resumed
                .capture(&conversation, Some(subsequent), options)
                .unwrap(),
            0
        );
        assert_eq!(resumed.next_sequence(), next);
    }
}

#[test]
fn bounds_and_changed_associations_pause_without_truncation_or_sequence_reset() {
    let home = tempfile::tempdir().unwrap();
    let mut conversation = Conversation::default();
    conversation.push(Message::user("x".repeat(MAX_BATCH_BYTES + 128)));
    conversation.push(Message::assistant("small trailing record"));
    let mut journal = Journal::open(
        home.path(),
        "agent-1",
        "thread_test",
        1,
        InitialMode::ShareSnapshot,
    )
    .unwrap();
    journal
        .capture(&conversation, None, ExportOptions::default())
        .unwrap();
    let batch = journal.batch().unwrap();
    assert_eq!(batch.count, 1);
    assert!(batch.input.len() > MAX_BATCH_BYTES);
    let ack = journal
        .acknowledge(&receipt(&records(&batch)[0], "recorded"))
        .unwrap();
    assert_eq!(ack.pending, 1);
    assert_eq!(journal.batch().unwrap().count, 1);
    let path = home.path().join("delta/agent-1/live.json");
    let before = fs::read(&path).unwrap();
    let mut oversized = Conversation::default();
    oversized.push(Message::user("x".repeat(MAX_RECORD_BYTES)));
    assert!(
        journal
            .capture(&oversized, None, ExportOptions::default())
            .is_err()
    );
    assert_eq!(fs::read(&path).unwrap(), before);
    drop(journal);

    let mut state: Value = serde_json::from_slice(&before).unwrap();
    state["associations"] = json!({});
    let changed = serde_json::to_vec(&state).unwrap();
    fs::write(&path, &changed).unwrap();
    assert!(
        Journal::open(
            home.path(),
            "agent-1",
            "thread_test",
            2,
            InitialMode::ShareSnapshot
        )
        .is_err()
    );
    assert_eq!(fs::read(path).unwrap(), changed);
}
