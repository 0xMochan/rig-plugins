use super::*;

fn item(id: &str, previous: Option<&str>, kind: &str, text: &str) -> Value {
    json!({"type":"item", "item":{
        "id":id, "kind":kind, "after_id":previous, "turn_id":null,
        "author":null, "created_at":null, "open":false, "content":{"text":text},
    }})
}

fn ready() -> Value {
    json!({"type":"ready", "thread_id":"thread-parent", "producer":null})
}

fn apply(view: &mut WatchView, value: Value) -> WatchEffect {
    view.apply_line(&value.to_string()).unwrap()
}

fn live_view() -> WatchView {
    let mut view = WatchView::new("thread-parent").unwrap();
    apply(&mut view, ready());
    apply(&mut view, json!({"type":"live"}));
    view
}

#[test]
fn known_wire_shapes_allow_extensions_but_reject_malformed_required_fields() {
    // These independently written values exercise the contract-8 frame shapes
    // documented by Delta, including both ready and error-envelope forms.
    let user = json!({"user_id":"person-1", "name":"Ada", "actor_id":"person-actor"});
    let child = json!({"thread_id":"child-1", "reference":"thread-parent#s=Opaque/Child", "kind":"subagent", "name":"Scout", "status":"running"});
    let comment = json!({"id":"note-1", "chain":"note-1", "after_id":null,
        "delivered_with":"answer", "routed_to":null, "author":user, "co_authors":[],
        "agent":false, "created_at":null, "text":"Why this?", "target":{"kind":"transcript", "item_id":"answer", "quote":"Hello"}});
    for value in [
        ready(),
        json!({"type":"ready", "thread_id":"thread-parent", "producer":{
            "source":"rig", "external_id":"session-1", "next_sequence":2,
            "turn":{"sequence":1,"status":"running"}, "future":true}}),
        item("answer", None, "agent_message", "Hello"),
        json!({"type":"item.append", "id":"answer", "text":" world"}),
        json!({"type":"remove", "id":"answer"}),
        json!({"type":"live"}),
        json!({"type":"turn", "state":"idle", "turn_id":null, "user":null}),
        json!({"type":"turn", "state":"running", "turn_id":"turn-1", "user":user}),
        json!({"type":"producer.turn", "sequence":1, "status":"finished"}),
        json!({"type":"presence", "users":[user]}),
        json!({"type":"worktrees", "worktrees":[{"reference":"history#t=opaque", "name":"repo"}]}),
        json!({"error":{"code":"watch_reset", "message":"Request a fresh snapshot", "reason":"diagnostic-only", "retryable":true}}),
        json!({"type":"comment", "comment":comment}),
        json!({"type":"comment.remove", "id":"note-1"}),
        json!({"type":"children", "children":[child]}),
        json!({"type":"child", "child":child}),
        json!({"type":"child.remove", "thread_id":"child-1"}),
    ] {
        let mut extended = value;
        extended["new_optional_field"] = json!({"future":true});
        assert!(WatchEvent::parse(&extended.to_string()).unwrap().is_some());
    }
    assert_eq!(
        WatchEvent::parse(r#"{"type":"new.event","anything":true}"#).unwrap(),
        None
    );
    for malformed in [
        json!([]),
        json!({"type":"item", "item":{"id":"missing-fields"}}),
        json!({"type":"ready", "thread_id":"thread-parent"}),
        json!({"type":"producer.turn", "sequence":0, "status":"running"}),
        json!({"type":"producer.turn", "sequence":1, "status":"made-up"}),
        json!({"type":"turn", "state":"running", "turn_id":null}),
        json!({"type":"item.append", "id":"answer", "text":42}),
        json!({"error":{"code":"watch_reset"}}),
    ] {
        assert!(
            WatchEvent::parse(&malformed.to_string()).is_err(),
            "{malformed}"
        );
    }
}

#[test]
fn snapshot_orders_predecessors_and_reconnect_dedupes_without_hiding_edits_or_removals() {
    let mut view = WatchView::new("thread-parent").unwrap();
    let mut ledger = ContextLedger::default();
    assert!(view.context_changes(&mut ledger).is_err());
    apply(&mut view, ready());
    apply(
        &mut view,
        item("first-child", Some("root"), "agent_message", "one"),
    );
    apply(
        &mut view,
        item("second-child", Some("root"), "agent_message", "two"),
    );
    assert!(view.ordered_item_ids().unwrap().is_empty());
    assert!(view.context_changes(&mut ledger).is_err());
    apply(&mut view, json!({"type":"live"}));
    assert!(view.context_changes(&mut ledger).unwrap().is_empty());
    apply(&mut view, item("root", None, "user_message", "question"));
    assert_eq!(
        view.ordered_item_ids().unwrap(),
        ["root", "first-child", "second-child"]
    );
    let changes = view.context_changes(&mut ledger).unwrap();
    assert_eq!(changes.len(), 3);
    assert!(matches!(&changes[0], ContextChange::UpsertItem { item } if item.id == "root"));
    assert!(view.context_changes(&mut ledger).unwrap().is_empty());

    apply(
        &mut view,
        json!({"type":"item.append", "id":"first-child", "text":"!"}),
    );
    let changes = view.context_changes(&mut ledger).unwrap();
    assert!(
        matches!(&changes[..], [ContextChange::UpsertItem { item }] if item.content["text"] == "one!")
    );
    view.reconnect();
    assert!(view.context_changes(&mut ledger).is_err());
    apply(&mut view, ready());
    apply(&mut view, item("root", None, "user_message", "question"));
    apply(
        &mut view,
        item("first-child", Some("root"), "agent_message", "one!"),
    );
    apply(
        &mut view,
        item("second-child", Some("root"), "agent_message", "two"),
    );
    apply(&mut view, json!({"type":"live"}));
    assert!(view.context_changes(&mut ledger).unwrap().is_empty());

    // Same text with a changed predecessor is still a meaningful item edit.
    apply(
        &mut view,
        item("second-child", Some("first-child"), "agent_message", "two"),
    );
    assert!(
        matches!(&view.context_changes(&mut ledger).unwrap()[..], [ContextChange::UpsertItem { item }] if item.after_id.as_deref() == Some("first-child"))
    );
    apply(&mut view, json!({"type":"remove", "id":"second-child"}));
    assert!(
        matches!(&view.context_changes(&mut ledger).unwrap()[..], [ContextChange::RemoveItem { id }] if id == "second-child")
    );
}

#[test]
fn public_context_is_inert_and_metadata_updates_replace_their_own_state() {
    let mut view = live_view();
    let mut ledger = ContextLedger::default();
    apply(
        &mut view,
        item("thought", None, "agent_thought", "private reasoning"),
    );
    let mut tool = item(
        "tool",
        Some("thought"),
        "tool_call",
        "unused tool display text",
    );
    tool["item"]["content"] = json!({"toolCallId":"remote-call", "name":"native_shell",
        "status":"completed", "rawInput":{"script":"touch never-execute"}, "rawOutput":{"output":"ok"},
        "native":{"private":"provider blob"}});
    apply(&mut view, tool);
    apply(
        &mut view,
        item("answer", Some("tool"), "agent_message", "public answer"),
    );
    let changes = view.context_changes(&mut ledger).unwrap();
    assert_eq!(changes.len(), 2);
    assert!(
        matches!(&changes[0], ContextChange::UpsertItem { item } if item.id == "tool" && item.content["rawInput"]["script"] == "touch never-execute" && item.content.get("native").is_none())
    );
    assert!(
        changes
            .iter()
            .all(|change| !format!("{change:?}").contains("private reasoning"))
    );

    apply(
        &mut view,
        json!({"type":"presence", "users":[{"user_id":"person-1", "name":"Ada"}]}),
    );
    assert_eq!(view.presence().len(), 1);
    apply(&mut view, json!({"type":"presence", "users":[]}));
    assert!(view.presence().is_empty());
    apply(
        &mut view,
        json!({"type":"worktrees", "worktrees":[{"reference":"history#t=opaque", "name":"repo"}]}),
    );
    assert_eq!(view.worktrees()[0].reference, "history#t=opaque");
    let child = json!({"thread_id":"child-1", "reference":"thread-parent#s=Opaque/Child", "kind":"future-child-kind", "name":"Scout", "status":"future-status"});
    apply(&mut view, json!({"type":"children", "children":[child]}));
    assert_eq!(
        view.children()["child-1"].reference,
        "thread-parent#s=Opaque/Child"
    );
    let mut updated = child;
    updated["status"] = json!("completed");
    apply(&mut view, json!({"type":"child", "child":updated}));
    assert_eq!(view.children()["child-1"].status, "completed");
    apply(
        &mut view,
        json!({"type":"child.remove", "thread_id":"child-1"}),
    );
    assert!(view.children().is_empty());

    let comment = json!({"id":"note-1", "chain":"note-1", "after_id":null, "delivered_with":"answer", "routed_to":null,
        "author":{"user_id":"person-1", "name":"Ada"}, "co_authors":[], "agent":false, "created_at":null,
        "text":"Please explain", "target":{"kind":"transcript", "item_id":"answer", "quote":"public answer"}});
    apply(&mut view, json!({"type":"comment", "comment":comment}));
    assert!(
        matches!(&view.context_changes(&mut ledger).unwrap()[..], [ContextChange::UpsertComment { comment }] if comment.id == "note-1")
    );
    apply(&mut view, json!({"type":"comment.remove", "id":"note-1"}));
    assert!(
        matches!(&view.context_changes(&mut ledger).unwrap()[..], [ContextChange::RemoveComment { id }] if id == "note-1")
    );
}

#[test]
fn supersession_only_signals_the_exact_local_turn_including_a_ready_snapshot() {
    let mut view = live_view();
    view.set_owned_turn(Some(7)).unwrap();
    assert_eq!(
        apply(
            &mut view,
            json!({"type":"producer.turn", "sequence":6, "status":"superseded"})
        ),
        WatchEffect::None
    );
    assert_eq!(
        apply(
            &mut view,
            json!({"type":"producer.turn", "sequence":7, "status":"finished"})
        ),
        WatchEffect::None
    );
    assert_eq!(
        apply(
            &mut view,
            json!({"type":"producer.turn", "sequence":7, "status":"superseded"})
        ),
        WatchEffect::StopLocalTurn { sequence: 7 }
    );
    view.set_owned_turn(None).unwrap();
    assert_eq!(
        apply(
            &mut view,
            json!({"type":"producer.turn", "sequence":7, "status":"superseded"})
        ),
        WatchEffect::None
    );
    view.set_owned_turn(Some(7)).unwrap();
    view.reconnect();
    assert_eq!(
        apply(
            &mut view,
            json!({"type":"ready", "thread_id":"thread-parent", "producer":{
        "source":"rig", "external_id":"session-1", "next_sequence":10,
        "turn":{"sequence":7, "status":"superseded"}}})
        ),
        WatchEffect::StopLocalTurn { sequence: 7 }
    );
    assert_eq!(view.phase(), WatchPhase::Snapshot);
    view.set_owned_turn(None).unwrap();
    assert_eq!(
        apply(
            &mut view,
            json!({"type":"turn", "state":"running", "turn_id":"other-turn", "user":null})
        ),
        WatchEffect::None
    );
}

#[test]
fn malformed_frames_cycles_and_bounds_pause_without_changing_delivered_context() {
    let mut view = live_view();
    let mut ledger = ContextLedger::default();
    apply(
        &mut view,
        item("answer", None, "agent_message", "last good answer"),
    );
    view.context_changes(&mut ledger).unwrap();
    let before = serde_json::to_value(&ledger).unwrap();
    assert!(
        view.apply_line(r#"{"type":"item.append","id":"answer","text":42}"#)
            .is_err()
    );
    assert_eq!(view.phase(), WatchPhase::Paused);
    assert!(view.context_changes(&mut ledger).is_err());
    assert_eq!(serde_json::to_value(&ledger).unwrap(), before);
    assert!(view.apply(WatchEvent::Live).is_err());

    view.reconnect();
    apply(&mut view, ready());
    apply(&mut view, item("a", Some("b"), "agent_message", "a"));
    assert!(
        view.apply_line(&item("b", Some("a"), "agent_message", "b").to_string())
            .is_err()
    );
    assert_eq!(view.phase(), WatchPhase::Paused);
    assert_eq!(serde_json::to_value(&ledger).unwrap(), before);

    let mut oversize = live_view();
    assert!(
        oversize
            .apply_line(&" ".repeat(MAX_LINE_BYTES + 1))
            .is_err()
    );
    assert_eq!(oversize.phase(), WatchPhase::Paused);
    let mut directory = live_view();
    assert!(directory.apply_line(&json!({"type":"presence", "users":vec![json!({"user_id":null, "name":null}); MAX_DIRECTORY_ENTRIES + 1]}).to_string()).is_err());
    assert_eq!(directory.phase(), WatchPhase::Paused);

    let mut retained = live_view();
    let large = "x".repeat(900_000);
    let mut failed = false;
    for number in 0..10 {
        if retained
            .apply_line(
                &item(&format!("large-{number}"), None, "agent_message", &large).to_string(),
            )
            .is_err()
        {
            failed = true;
            break;
        }
    }
    assert!(
        failed,
        "retained byte bound must reject repeated individually valid lines"
    );
    assert_eq!(retained.phase(), WatchPhase::Paused);
    assert!(retained.context_changes(&mut ledger).is_err());
    assert_eq!(serde_json::to_value(&ledger).unwrap(), before);

    let mut remote_error = live_view();
    let effect = apply(
        &mut remote_error,
        json!({"error":{"code":"watch_reset", "message":"fresh snapshot required", "retryable":true, "retry_after_ms":100}}),
    );
    assert!(matches!(effect, WatchEffect::Pause { error } if error.code == "watch_reset"));
    assert_eq!(remote_error.phase(), WatchPhase::Paused);
    assert!(remote_error.context_changes(&mut ledger).is_err());
    assert_eq!(serde_json::to_value(&ledger).unwrap(), before);
}
