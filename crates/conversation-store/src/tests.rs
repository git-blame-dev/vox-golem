use super::*;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
static NEXT: AtomicUsize = AtomicUsize::new(0);
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let root = PathBuf::from("/tmp/opencode").join(format!(
            "synthetic-conversation-store-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&root).unwrap();
        Self(root)
    }
    fn path(&self) -> PathBuf {
        self.0.join("history.db")
    }
    fn open(&self) -> Store {
        Store::open(&self.path()).unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn real_database_commit_restart_and_failed_commit_are_atomic() {
    let fixture = Fixture::new();
    let mut store = fixture.open();
    let id = store.allocate_id().unwrap();
    let record = codec::record(Operation {
        version: 2,
        id,
        body: OperationBody::ConversationCreated {
            title: "Synthetic 🧪".into(),
        },
    })
    .unwrap();
    assert!(store
        .import_with_hook(vec![record.clone()], true, None, || Err(Error::Storage))
        .is_err());
    assert_eq!(store.revision().unwrap(), "0");
    assert!(store
        .publications(None, 128, MAX_CANONICAL_BYTES, None)
        .unwrap()
        .is_empty());
    assert!(store.receipts_to_publish(128, None).unwrap().is_empty());
    assert_eq!(fixture.open().revision().unwrap(), "0");
    store
        .import_records(vec![record.clone()], true, None)
        .unwrap();
    drop(store);
    let mut cold = fixture.open();
    assert_eq!(cold.snapshot().unwrap().records, vec![record.clone()]);
    assert_eq!(
        cold.publications(None, 128, MAX_CANONICAL_BYTES, None)
            .unwrap(),
        vec![record.clone()]
    );
    assert_eq!(
        cold.receipts_to_publish(128, None).unwrap()[0].body_key,
        record.body_key
    );
}

#[test]
fn two_handles_reload_revision_and_never_reuse_sequence() {
    let fixture = Fixture::new();
    let mut left = fixture.open();
    let mut right = fixture.open();
    let first = left.allocate_id().unwrap();
    let second = right.allocate_id().unwrap();
    assert_eq!(first.replica, second.replica);
    assert_eq!(first.sequence, "1");
    assert_eq!(second.sequence, "2");
    left.create_conversation("Synthetic left".into()).unwrap();
    right.create_conversation("Synthetic right".into()).unwrap();
    assert_eq!(left.snapshot().unwrap().records.len(), 2);
    assert_eq!(right.snapshot().unwrap(), left.snapshot().unwrap());
}

#[test]
fn allocated_sequence_reaches_u64_max_and_never_wraps_after_reopen() {
    let fixture = Fixture::new();
    let mut store = fixture.open();
    store
        .db
        .execute(
            "UPDATE history_state SET last_sequence='18446744073709551614'",
            [],
        )
        .unwrap();
    assert_eq!(
        store.allocate_id().unwrap().sequence,
        "18446744073709551615"
    );
    assert!(matches!(
        store.allocate_id(),
        Err(Error::Domain(DomainError::LimitExceeded { .. }))
    ));
    drop(store);
    assert!(matches!(
        fixture.open().allocate_id(),
        Err(Error::Domain(DomainError::LimitExceeded { .. }))
    ));
}

#[test]
fn alpha_reset_preserves_identity_counter_and_unowned_tables_without_reading_old_bodies() {
    let fixture = Fixture::new();
    let db = Connection::open(fixture.path()).unwrap();
    db.execute_batch("CREATE TABLE state(singleton INTEGER PRIMARY KEY,replica TEXT,last_sequence TEXT,snapshot TEXT);
        INSERT INTO state VALUES(1,'00000000-0000-4000-8000-000000000001','901','deliberately invalid legacy JSON');
        CREATE TABLE drafts(text TEXT); INSERT INTO drafts VALUES('Synthetic old history');
        CREATE TABLE credentials(value TEXT); INSERT INTO credentials VALUES('Synthetic preserved credential');
        CREATE TABLE preferences(value TEXT); INSERT INTO preferences VALUES('Synthetic preserved preference');
        CREATE TABLE pairing(value TEXT); INSERT INTO pairing VALUES('Synthetic preserved association');
        CREATE TABLE capture_notes(value TEXT); INSERT INTO capture_notes VALUES('Synthetic preserved note');
        PRAGMA user_version=1;").unwrap();
    drop(db);
    let mut store = fixture.open();
    assert!(store.snapshot().unwrap().records.is_empty());
    assert_eq!(store.allocate_id().unwrap().sequence, "902");
    for table in ["credentials", "preferences", "pairing", "capture_notes"] {
        let count: i64 = store
            .db
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1);
    }
    drop(store);
    assert_eq!(fixture.open().allocate_id().unwrap().sequence, "903");
}

#[test]
fn raw_and_normalized_records_require_v2_strict_fields_and_real_hashes() {
    let valid = r#"{"version":2,"id":{"replica":"00000000-0000-4000-8000-000000000001","sequence":"1"},"body":{"kind":"conversation_created","title":"Synthetic"}}"#;
    let record = codec::canonical_record(valid).unwrap();
    let raw = serde_json::to_string(&record).unwrap();
    assert_eq!(codec::parse_record(&raw).unwrap(), record);
    for bad in [
        valid.replace("\"version\":2", "\"version\":1"),
        valid.replace("\"version\":2", "\"version\":999"),
        valid.replace("\"version\":2", "\"version\":2,\"unexpected\":true"),
        valid.replace("\"version\":2", "\"version\":2,\"version\":2"),
    ] {
        assert!(codec::canonical_record(&bad).is_err());
    }
    assert!(codec::parse_record(&raw.replace(&record.body_key, &"0".repeat(64))).is_err());
}

fn turn(store: &mut Store, context: &ContextSelection, prompt: &str) -> Record {
    use voxgolem_core::durable_conversation::{Answer, Provenance, TerminalStage};
    let request = store.allocate_id().unwrap();
    let id = store.allocate_id().unwrap();
    codec::record(Operation {
        version: 2,
        id,
        body: OperationBody::TurnCompleted {
            sources: vec![voxgolem_core::sources::SourceLink {
                url: "https://example.test/synthetic".into(),
                title: "Synthetic source".into(),
            }],
            conversation: context.conversation.clone(),
            parent: context.parent.clone(),
            request,
            prompt: prompt.into(),
            answer: Answer::Text {
                text: "Synthetic answer".into(),
            },
            terminal_stage: TerminalStage::Instant,
            provenance: Provenance {
                requested_model: "synthetic-model".into(),
                returned_model: None,
                policy_id: "synthetic-policy".into(),
                policy_version: "1".into(),
                instruction_profile_id: "synthetic-instructions".into(),
                instruction_profile_version: "1".into(),
                context: context.ancestor_ids.clone(),
            },
        },
    })
    .unwrap()
}

#[test]
fn pinned_context_creates_branches_sources_survive_and_delete_suppresses_late_completion() {
    let fixture = Fixture::new();
    let mut store = fixture.open();
    let creation = store.create_conversation("Conversation".into()).unwrap();
    let conversation = creation.operation.id;
    let pinned = store
        .context(conversation.clone(), None, 256 * 1024)
        .unwrap();
    let first = turn(&mut store, &pinned, "Synthetic first prompt");
    let late = turn(&mut store, &pinned, "Synthetic concurrent prompt");
    store
        .import_records(vec![first.clone()], false, Some(&first.operation.id))
        .unwrap();
    store
        .import_records(vec![late.clone()], false, Some(&late.operation.id))
        .unwrap();
    let selected = store
        .context(
            conversation.clone(),
            Some(first.operation.id.clone()),
            256 * 1024,
        )
        .unwrap();
    assert_eq!(selected.turns.len(), 1);
    assert_eq!(selected.turns[0], first);
    assert!(store
        .context(conversation.clone(), None, 256 * 1024)
        .is_err());
    store
        .save_draft(Some(&conversation), "Synthetic sent", true)
        .unwrap();
    store
        .select(Selection {
            conversation: Some(conversation.clone()),
            selected_head: Some(first.operation.id.clone()),
        })
        .unwrap();
    let late_after_delete = turn(&mut store, &selected, "Synthetic sent");
    let deletion = codec::record(Operation {
        version: 2,
        id: store.allocate_id().unwrap(),
        body: OperationBody::ConversationDeleted {
            conversation: conversation.clone(),
        },
    })
    .unwrap();
    store.import_records(vec![deletion], false, None).unwrap();
    let result = store
        .import_records(
            vec![late_after_delete.clone()],
            false,
            Some(&late_after_delete.operation.id),
        )
        .unwrap();
    assert_eq!(result.suppressed, vec![late_after_delete.operation.id]);
    assert_eq!(store.draft(Some(&conversation)).unwrap().text, "");
    assert_eq!(store.selection().unwrap().conversation, None);
    let snapshot = store.snapshot().unwrap();
    assert_eq!(snapshot.records.len(), 1);
    assert_eq!(snapshot.retired.len(), 4);
    assert_eq!(
        store
            .publications(None, 128, MAX_CANONICAL_BYTES, None)
            .unwrap()
            .len(),
        1
    );
    assert_eq!(fixture.open().snapshot().unwrap(), snapshot);
}

#[test]
fn saved_completion_consumes_only_exact_pinned_draft_and_receipts_are_exact() {
    let fixture = Fixture::new();
    let mut store = fixture.open();
    let conversation = store
        .create_conversation("Synthetic".into())
        .unwrap()
        .operation
        .id;
    let pinned = store
        .context(conversation.clone(), None, 256 * 1024)
        .unwrap();
    let completed = turn(&mut store, &pinned, "Synthetic admitted");
    store
        .save_draft(Some(&conversation), "Synthetic newer unsent draft", true)
        .unwrap();
    store
        .import_records(
            vec![completed.clone()],
            false,
            Some(&completed.operation.id),
        )
        .unwrap();
    assert_eq!(
        store.draft(Some(&conversation)).unwrap().text,
        "Synthetic newer unsent draft"
    );
    let peer = "00000000-0000-4000-8000-000000000002";
    let mut receipts = store
        .read(|core| {
            receipts_after_commit(core, peer, std::slice::from_ref(&completed.operation.id))
        })
        .unwrap();
    let mut wrong = receipts[0].clone();
    wrong.body_key = "0".repeat(64);
    assert!(store.acknowledge(peer, vec![wrong]).is_err());
    assert_eq!(
        store
            .publications(Some(peer), 128, MAX_CANONICAL_BYTES, None)
            .unwrap()
            .len(),
        2
    );
    store
        .acknowledge(peer, std::mem::take(&mut receipts))
        .unwrap();
    assert_eq!(
        store
            .publications(Some(peer), 128, MAX_CANONICAL_BYTES, None)
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        fixture
            .open()
            .publications(Some(peer), 128, MAX_CANONICAL_BYTES, None)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn actual_sqlite_failure_after_record_writes_rolls_back_the_whole_revision_and_cached_reducer() {
    let fixture = Fixture::new();
    let mut store = fixture.open();
    let conversation = store
        .create_conversation("Synthetic retained".into())
        .unwrap()
        .operation
        .id;
    let context = store
        .context(conversation.clone(), None, 256 * 1024)
        .unwrap();
    store
        .save_draft(Some(&conversation), "Synthetic retained draft", true)
        .unwrap();
    let before = store.snapshot().unwrap();
    let record = turn(&mut store, &context, "Synthetic retained draft");
    store.db.execute_batch("CREATE TRIGGER synthetic_outbox_failure BEFORE INSERT ON history_outbox BEGIN SELECT RAISE(ABORT,'synthetic transaction failure'); END;").unwrap();
    assert!(store
        .import_records(vec![record.clone()], true, Some(&record.operation.id))
        .is_err());
    assert_eq!(store.snapshot().unwrap(), before);
    assert_eq!(
        store.draft(Some(&conversation)).unwrap().text,
        "Synthetic retained draft"
    );
    assert!(store.receipts_to_publish(128, None).unwrap().is_empty());
    assert_eq!(fixture.open().snapshot().unwrap(), before);
    store
        .db
        .execute_batch("DROP TRIGGER synthetic_outbox_failure;")
        .unwrap();
    store
        .import_records(vec![record.clone()], true, Some(&record.operation.id))
        .unwrap();
    assert_eq!(store.draft(Some(&conversation)).unwrap().text, "");
    assert_eq!(fixture.open().snapshot().unwrap().records.len(), 2);
}

#[test]
fn prepare_fences_exact_local_owner_but_allows_unrelated_imports_and_never_replays_generation() {
    let fixture = Fixture::new();
    let mut store = fixture.open();
    store
        .save_draft(None, "Synthetic admitted fresh draft", false)
        .unwrap();
    let fresh = Selection {
        conversation: None,
        selected_head: None,
    };
    let prepared = store
        .prepare_context(fresh.clone(), "Synthetic admitted fresh draft", 256 * 1024)
        .unwrap();
    let conversation = prepared.context.conversation.clone();
    assert!(prepared.created.is_some());
    assert_eq!(
        store.selection().unwrap().conversation,
        Some(conversation.clone())
    );
    assert_eq!(
        store.draft(Some(&conversation)).unwrap().text,
        "Synthetic admitted fresh draft"
    );
    assert_eq!(store.draft(None).unwrap().text, "");
    assert!(store
        .prepare_context(fresh, "Synthetic stale fresh draft", 256 * 1024)
        .is_err());
    assert_eq!(store.snapshot().unwrap().records.len(), 1);
    let expected = store.selection().unwrap();
    fixture
        .open()
        .create_conversation("Synthetic unrelated import".into())
        .unwrap();
    let pinned = store
        .prepare_context(expected.clone(), "Synthetic retained owner", 256 * 1024)
        .unwrap();
    assert_eq!(pinned.context.conversation, conversation);
    assert!(pinned.created.is_none());
    let completed = turn(&mut store, &pinned.context, "Synthetic retained owner");
    store
        .import_records(
            vec![completed.clone()],
            false,
            Some(&completed.operation.id),
        )
        .unwrap();
    assert!(
        matches!(store.prepare_context(expected,"Synthetic stale head",256*1024),Err(Error::Domain(DomainError::Invalid { field })) if field=="selection_changed")
    );
    assert_eq!(store.snapshot().unwrap().records.len(), 3);
}

#[test]
fn failed_fresh_prepare_does_not_leave_a_conversation_selection_publication_or_draft() {
    let fixture = Fixture::new();
    let mut store = fixture.open();
    store
        .save_draft(None, "Synthetic fresh retry", true)
        .unwrap();
    store.db.execute_batch("CREATE TRIGGER synthetic_draft_failure BEFORE INSERT ON history_drafts BEGIN SELECT RAISE(ABORT,'synthetic draft failure'); END;").unwrap();
    assert!(store
        .prepare_context(
            Selection {
                conversation: None,
                selected_head: None
            },
            "Synthetic fresh retry",
            256 * 1024
        )
        .is_err());
    assert_eq!(store.snapshot().unwrap(), StoreSnapshot::empty());
    assert_eq!(store.selection().unwrap().conversation, None);
    assert_eq!(store.draft(None).unwrap().text, "Synthetic fresh retry");
    assert!(store
        .publications(None, 128, MAX_CANONICAL_BYTES, None)
        .unwrap()
        .is_empty());
    assert_eq!(fixture.open().snapshot().unwrap(), StoreSnapshot::empty());
}

#[test]
fn stale_context_cannot_create_a_pending_graph_in_a_new_epoch_database() {
    let old = Fixture::new();
    let mut old_store = old.open();
    let conversation = old_store
        .create_conversation("Synthetic old epoch".into())
        .unwrap()
        .operation
        .id;
    let old_context = old_store.context(conversation, None, 256 * 1024).unwrap();
    let fresh = Fixture::new();
    let mut fresh_store = fresh.open();
    let completed = turn(&mut fresh_store, &old_context, "Synthetic stale completion");
    assert!(matches!(
        fresh_store.import_records(
            vec![completed.clone()],
            false,
            Some(&completed.operation.id)
        ),
        Err(Error::Domain(DomainError::Missing { .. }))
    ));
    assert_eq!(fresh_store.snapshot().unwrap(), StoreSnapshot::empty());
    assert_eq!(fresh.open().snapshot().unwrap(), StoreSnapshot::empty());
}

#[test]
fn encoded_quota_failure_preserves_prior_rows_publications_receipts_and_reopen_state() {
    let fixture = Fixture::new();
    let mut store = fixture.open();
    let conversation = store
        .create_conversation("Synthetic quota owner".into())
        .unwrap()
        .operation
        .id;
    let context = store.context(conversation, None, 256 * 1024).unwrap();
    let before = store.snapshot().unwrap();
    let mut oversized = Vec::new();
    for _ in 0..20 {
        let mut operation = turn(&mut store, &context, "Synthetic quota prompt").operation;
        if let OperationBody::TurnCompleted { answer, .. } = &mut operation.body {
            *answer = Answer::Text {
                text: "s".repeat(230 * 1024),
            };
        }
        oversized.push(codec::record(operation).unwrap());
    }
    assert!(
        matches!(store.import_records(oversized,true,None),Err(Error::Domain(DomainError::LimitExceeded { limit })) if limit=="native_snapshot_bytes")
    );
    assert_eq!(store.snapshot().unwrap(), before);
    assert_eq!(fixture.open().snapshot().unwrap(), before);
    assert_eq!(
        store
            .publications(None, 128, MAX_CANONICAL_BYTES, None)
            .unwrap()
            .len(),
        1
    );
    assert!(store.receipts_to_publish(128, None).unwrap().is_empty());
}

#[test]
fn drafts_are_deduplicated_and_the_aggregate_local_byte_budget_is_transactional() {
    let fixture = Fixture::new();
    let mut store = fixture.open();
    store
        .save_draft(None, "Synthetic identical", false)
        .unwrap();
    let writes = store.db.total_changes();
    store
        .save_draft(None, "Synthetic identical", false)
        .unwrap();
    assert_eq!(store.db.total_changes(), writes);
    store.save_draft(None, "", false).unwrap();
    let text = "d".repeat(32 * 1024);
    for sequence in 1..=128 {
        let id = OperationId {
            replica: "00000000-0000-4000-8000-000000000001".into(),
            sequence: sequence.to_string(),
        };
        store.save_draft(Some(&id), &text, true).unwrap();
    }
    let overflow = OperationId {
        replica: "00000000-0000-4000-8000-000000000001".into(),
        sequence: "129".into(),
    };
    assert!(store.save_draft(Some(&overflow), "x", true).is_err());
    assert_eq!(store.draft(Some(&overflow)).unwrap().text, "");
    assert_eq!(fixture.open().draft(Some(&overflow)).unwrap().text, "");
}

#[test]
fn unknown_database_schema_is_rejected_without_resetting_any_owned_or_unowned_row() {
    let fixture = Fixture::new();
    let store = fixture.open();
    store.db.execute_batch("CREATE TABLE synthetic_settings(value TEXT); INSERT INTO synthetic_settings VALUES('Synthetic preserve'); PRAGMA user_version=999;").unwrap();
    drop(store);
    assert!(matches!(
        Store::open(&fixture.path()),
        Err(Error::Domain(DomainError::UnsupportedVersion {
            version: 999
        }))
    ));
    let db = Connection::open(fixture.path()).unwrap();
    assert_eq!(
        db.query_row("SELECT value FROM synthetic_settings", [], |r| r
            .get::<_, String>(0))
            .unwrap(),
        "Synthetic preserve"
    );
    assert_eq!(
        db.query_row("SELECT COUNT(*) FROM history_state", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        1
    );
}

#[test]
fn typed_completion_pins_ancestry_keeps_sources_plain_and_reports_saved_false_after_delete() {
    let fixture = Fixture::new();
    let mut store = fixture.open();
    let conversation = store
        .create_conversation("Synthetic typed completion".into())
        .unwrap()
        .operation
        .id;
    let context = store
        .context(conversation.clone(), None, 256 * 1024)
        .unwrap();
    fn completion(store: &mut Store, context: ContextSelection) -> TurnCompletion {
        let OperationBody::TurnCompleted {
            request,
            prompt,
            answer,
            sources,
            terminal_stage,
            provenance,
            ..
        } = turn(store, &context, "Synthetic typed prompt")
            .operation
            .body
        else {
            unreachable!()
        };
        TurnCompletion {
            request,
            context,
            prompt,
            answer,
            sources,
            terminal_stage,
            provenance,
        }
    }
    let input = completion(&mut store, context);
    let first = store.complete_turn(input).unwrap();
    assert!(first.saved);
    let pinned = store
        .context(
            conversation.clone(),
            Some(first.record.operation.id.clone()),
            256 * 1024,
        )
        .unwrap();
    let derived = provider_context(pinned.clone());
    assert_eq!(
        derived.provider_messages[1].content,
        "Synthetic answer\n\nSources:\n- https://example.test/synthetic"
    );
    let mut second = completion(&mut store, pinned.clone());
    second.provenance.context.clear();
    let second = store.complete_turn(second).unwrap();
    assert!(second.saved);
    let OperationBody::TurnCompleted {
        provenance,
        answer,
        sources,
        parent,
        ..
    } = &second.record.operation.body
    else {
        unreachable!()
    };
    assert_eq!(provenance.context, pinned.ancestor_ids);
    assert_eq!(parent.as_ref(), Some(&first.record.operation.id));
    assert_eq!(answer.text(), "Synthetic answer");
    assert_eq!(sources.len(), 1);
    let late = completion(&mut store, pinned);
    store.delete_conversation(conversation).unwrap();
    assert!(!store.complete_turn(late).unwrap().saved);
    store.close().unwrap();
    let snapshot = fixture.open().snapshot().unwrap();
    assert_eq!(snapshot.records.len(), 1);
    assert!(!serde_json::to_string(&snapshot)
        .unwrap()
        .contains("Synthetic answer"));
}

#[test]
fn conversation_and_head_pages_are_bounded_without_truncating_the_persisted_graph() {
    let fixture = Fixture::new();
    let mut store = fixture.open();
    for _ in 0..150 {
        store.create_conversation("Synthetic page".into()).unwrap();
    }
    let first = store.conversations(0, 128).unwrap();
    let next = store.conversations(128, 128).unwrap();
    assert_eq!(first.conversation_count, 150);
    assert_eq!(first.conversations.len(), 128);
    assert_eq!(next.conversation_offset, 128);
    assert_eq!(next.conversations.len(), 22);
    let conversation = first.conversations[0].id.clone();
    let context = store
        .context(conversation.clone(), None, 256 * 1024)
        .unwrap();
    let mut turns = Vec::new();
    for _ in 0..150 {
        turns.push(turn(&mut store, &context, "Synthetic sibling"));
    }
    for batch in turns.chunks(128) {
        store.import_records(batch.to_vec(), false, None).unwrap();
    }
    let heads = store.heads(conversation.clone(), 0, 128).unwrap();
    let older_heads = store.heads(conversation.clone(), 128, 128).unwrap();
    assert_eq!(heads.head_count, 150);
    assert_eq!(heads.heads.len(), 128);
    assert_eq!(older_heads.heads.len(), 22);
    store
        .select(Selection {
            conversation: Some(conversation.clone()),
            selected_head: None,
        })
        .unwrap();
    assert!(matches!(
        store.prepare_context(
            store.selection().unwrap(),
            "Synthetic ambiguous request",
            256 * 1024
        ),
        Err(Error::Domain(DomainError::AmbiguousHeads { .. }))
    ));
    assert_eq!(fixture.open().snapshot().unwrap().records.len(), 300);
    assert!(store.conversations(0, 129).is_err());
    assert!(store.heads(conversation, 0, 129).is_err());
}

#[test]
fn idle_checkpoint_reports_busy_reader_instead_of_claiming_truncation_completed() {
    let fixture = Fixture::new();
    let mut writer = fixture.open();
    let mut reader = fixture.open();
    writer
        .create_conversation("Synthetic checkpoint base".into())
        .unwrap();
    let read = reader.db.transaction().unwrap();
    let _: i64 = read
        .query_row("SELECT COUNT(*) FROM history_records", [], |row| row.get(0))
        .unwrap();
    writer
        .create_conversation("Synthetic checkpoint tail".into())
        .unwrap();
    writer.db.busy_timeout(std::time::Duration::ZERO).unwrap();
    assert!(matches!(
        writer.checkpoint(),
        Err(Error::Database(rusqlite::ErrorCode::DatabaseBusy))
    ));
    read.commit().unwrap();
    writer.checkpoint().unwrap();
    assert_eq!(
        std::fs::metadata(fixture.path().with_extension("db-wal"))
            .unwrap()
            .len(),
        0
    );
}

#[test]
fn compact_projection_excludes_large_turn_bodies_and_preserves_bounded_previews_and_selected_pages()
{
    let fixture = Fixture::new();
    let mut store = fixture.open();
    let mut conversations = Vec::new();
    for index in 0..129 {
        let created = store
            .create_conversation(format!("Synthetic conversation {index}"))
            .unwrap();
        conversations.push(created.operation.id);
    }
    let selected_conversation = store.conversations(128, 1).unwrap().conversations[0]
        .id
        .clone();
    let item127_conversation = store.conversations(127, 1).unwrap().conversations[0]
        .id
        .clone();
    let context = store
        .context(selected_conversation.clone(), None, 256 * 1024)
        .unwrap();
    let mut turns = Vec::new();
    for index in 0..129 {
        let mut record = turn(
            &mut store,
            &context,
            &format!(
                "   Synthetic   branch\n\tprompt {index}   {}",
                "🧪".repeat(40)
            ),
        );
        if let OperationBody::TurnCompleted { answer, .. } = &mut record.operation.body {
            *answer = Answer::Text {
                text: format!("LARGE_BODY_SENTINEL_{index}_{}", "x".repeat(16 * 1024)),
            };
        }
        turns.push(codec::record(record.operation).unwrap());
    }
    for batch in turns.chunks(64) {
        store.import_records(batch.to_vec(), false, None).unwrap();
    }

    let projection = store
        .read(|core| {
            gateway::project(
                core,
                gateway::ProjectionRequest {
                    conversation: Some(selected_conversation.clone()),
                    selected_head: None,
                    local_work_in_flight: false,
                    read: None,
                },
            )
        })
        .unwrap();
    let encoded_view = serde_json::to_string(&projection.view).unwrap();
    assert!(!encoded_view.contains("LARGE_BODY_SENTINEL_"));
    assert!(!encoded_view.contains("\"records\""));
    assert!(!encoded_view.contains("\"quarantine\""));
    assert!(encoded_view.len() <= 32 * 1024);
    assert_eq!(projection.view.conversation_count, 129);
    assert_eq!(projection.view.conversations.len(), 128);
    assert_eq!(projection.view.conversation_offset, 127);
    assert_eq!(projection.view.conversations[127].id, selected_conversation);
    assert_eq!(projection.view.branch_count, 129);
    let fitted_branches = projection.view.branch_offset;
    assert!((1..=128).contains(&fitted_branches));
    assert_eq!(projection.view.branch_previews.len(), fitted_branches);
    if let Some(voxgolem_core::durable_conversation::ContinuationDecision::Choose { heads }) =
        &projection.continuation
    {
        assert_eq!(heads.len(), fitted_branches);
    } else {
        panic!("expected Choose continuation");
    }
    assert!(projection.view.branch_previews[0]
        .prompt_preview
        .starts_with("Synthetic branch prompt "));
    assert!(projection.view.branch_previews[0].prompt_preview.len() <= 128);

    let rev = projection.view.revision.clone();
    let second_conv_page = store.conversations_at(&rev, 127, 128).unwrap();
    assert!(serde_json::to_string(&second_conv_page).unwrap().len() <= 32 * 1024);
    assert_eq!(second_conv_page.conversations.len(), 2);
    assert_eq!(second_conv_page.conversations[0].id, item127_conversation);
    assert_eq!(second_conv_page.conversations[1].id, selected_conversation);
    assert_eq!(second_conv_page.next_offset, 129);
    assert!(matches!(
        store.conversations_at("0", 0, 128),
        Err(Error::Domain(DomainError::RevisionConflict { .. }))
    ));

    let first_branch_page = store
        .branches_at(&rev, selected_conversation.clone(), None, 0, 128)
        .unwrap();
    assert!(serde_json::to_string(&first_branch_page).unwrap().len() <= 32 * 1024);
    assert!((1..=128).contains(&first_branch_page.heads.len()));
    assert_eq!(first_branch_page.next_offset, first_branch_page.heads.len());
    assert_eq!(
        first_branch_page.head_previews.len(),
        first_branch_page.heads.len()
    );

    let second_branch_page = store
        .branches_at(
            &rev,
            selected_conversation.clone(),
            None,
            first_branch_page.next_offset,
            128,
        )
        .unwrap();
    assert!(serde_json::to_string(&second_branch_page).unwrap().len() <= 32 * 1024);
    assert_eq!(
        first_branch_page.heads.len() + second_branch_page.heads.len(),
        129
    );
    assert_eq!(second_branch_page.next_offset, 129);
    assert_eq!(
        second_branch_page.head_previews.len(),
        second_branch_page.heads.len()
    );
    assert!(matches!(
        store.branches_at("0", selected_conversation, None, 0, 128),
        Err(Error::Domain(DomainError::RevisionConflict { .. }))
    ));
}
