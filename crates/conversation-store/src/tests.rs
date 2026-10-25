use super::*;

#[test]
fn actual_sqlite_commit_observes_retained_final_authority() {
    use std::sync::Mutex;
    static AUTHORITY: Mutex<()> = Mutex::new(());
    static COMMIT_OBSERVED: std::sync::atomic::AtomicBool =
        std::sync::atomic::AtomicBool::new(false);
    fn commit_busy(_: i32) -> bool {
        // The reader permits BEGIN IMMEDIATE and every staged write, but prevents
        // COMMIT's exclusive lock. This callback is SQLite's real COMMIT path.
        assert!(AUTHORITY.try_lock().is_err());
        COMMIT_OBSERVED.store(true, Ordering::SeqCst);
        false
    }
    let fixture = Fixture::new();
    let mut store = fixture.open();
    let prepared = store
        .prepare_request(
            Selection {
                conversation: None,
                selected_head: None,
            },
            "Synthetic prompt",
            4096,
            true,
        )
        .unwrap();
    let context = prepared.context.selection;
    let body = turn(&mut store, &context, "Synthetic prompt")
        .operation
        .body;
    let before = store.snapshot().unwrap();
    store
        .db
        .execute_batch("PRAGMA journal_mode=DELETE")
        .unwrap();
    let reader = Connection::open(fixture.path()).unwrap();
    reader
        .execute_batch("BEGIN; SELECT * FROM history_state;")
        .unwrap();
    store.db.busy_handler(Some(commit_busy)).unwrap();
    store.complete_body_with_authority(
        body,
        || {
            assert!(!COMMIT_OBSERVED.load(Ordering::SeqCst));
            (AUTHORITY.lock().unwrap(), CompletionDecision::Authorized)
        },
        |_, attempt| {
            assert!(AUTHORITY.try_lock().is_err());
            assert!(COMMIT_OBSERVED.load(Ordering::SeqCst));
            assert!(matches!(
                attempt,
                CompletionAttempt::Finished(Err(Error::Database(
                    rusqlite::ErrorCode::DatabaseBusy
                )))
            ));
        },
    );
    reader.execute_batch("ROLLBACK").unwrap();
    drop(reader);
    assert_eq!(store.snapshot().unwrap(), before);
    assert_eq!(
        store.draft(Some(&context.conversation)).unwrap().text,
        "Synthetic prompt"
    );
}

#[test]
fn final_authority_controls_commit_and_retains_guard_through_winner() {
    use std::sync::Mutex;
    for decision in [
        CompletionDecision::Cancelled,
        CompletionDecision::Unavailable,
        CompletionDecision::Authorized,
    ] {
        let fixture = Fixture::new();
        let mut store = fixture.open();
        let prepared = store
            .prepare_request(
                Selection {
                    conversation: None,
                    selected_head: None,
                },
                "Synthetic prompt",
                4096,
                true,
            )
            .unwrap();
        let context = prepared.context.selection;
        let body = turn(&mut store, &context, "Synthetic prompt")
            .operation
            .body;
        let before = store.snapshot().unwrap();
        let authority = Mutex::new(false);
        let saved = store.complete_body_with_authority(
            body,
            || (authority.lock().unwrap(), decision),
            |guard, attempt| {
                assert!(
                    authority.try_lock().is_err(),
                    "authority retained during winner bookkeeping"
                );
                **guard = true;
                match attempt {
                    CompletionAttempt::Finished(result) => {
                        assert!(result.unwrap().saved);
                        true
                    }
                    CompletionAttempt::Cancelled | CompletionAttempt::Unavailable => false,
                }
            },
        );
        assert!(*authority.lock().unwrap());
        assert_eq!(store.snapshot().unwrap() != before, saved);
        assert_eq!(
            fixture.open().snapshot().unwrap(),
            store.snapshot().unwrap()
        );
        assert_eq!(
            store
                .draft(Some(&context.conversation))
                .unwrap()
                .text
                .is_empty(),
            saved
        );
    }
}

#[test]
fn staging_and_real_commit_errors_still_arbitrate_under_authority() {
    use std::sync::Mutex;
    for staging_failure in [false, true] {
        for cancel in [false, true] {
            let fixture = Fixture::new();
            let mut store = fixture.open();
            let prepared = store
                .prepare_request(
                    Selection {
                        conversation: None,
                        selected_head: None,
                    },
                    "Synthetic prompt",
                    4096,
                    true,
                )
                .unwrap();
            let context = prepared.context.selection;
            let mut body = turn(&mut store, &context, "Synthetic prompt")
                .operation
                .body;
            let before = store.snapshot().unwrap();
            if staging_failure {
                body = OperationBody::ConversationCreated {
                    title: "Synthetic invalid completion".into(),
                };
            } else {
                // An AFTER trigger succeeds at staging but fails the actual COMMIT:
                // the missing parent is checked only at transaction completion.
                store.db.execute_batch("CREATE TABLE synthetic_parent(id INTEGER PRIMARY KEY); CREATE TABLE synthetic_child(id INTEGER REFERENCES synthetic_parent(id) DEFERRABLE INITIALLY DEFERRED); CREATE TRIGGER synthetic_commit_failure AFTER DELETE ON history_drafts BEGIN INSERT INTO synthetic_child VALUES(1); END;").unwrap();
            }
            let authority = Mutex::new(());
            store.complete_body_with_authority(
                body,
                || {
                    (
                        authority.lock().unwrap(),
                        if cancel {
                            CompletionDecision::Cancelled
                        } else {
                            CompletionDecision::Authorized
                        },
                    )
                },
                |_, attempt| {
                    assert!(authority.try_lock().is_err());
                    if cancel {
                        assert!(matches!(attempt, CompletionAttempt::Cancelled));
                    } else if !staging_failure {
                        assert!(matches!(
                            attempt,
                            CompletionAttempt::Finished(Err(Error::Database(
                                rusqlite::ErrorCode::ConstraintViolation
                            )))
                        ));
                    } else {
                        assert!(matches!(attempt, CompletionAttempt::Finished(Err(_))));
                    }
                },
            );
            assert_eq!(store.snapshot().unwrap(), before);
            assert_eq!(fixture.open().snapshot().unwrap(), before);
            assert_eq!(
                store.draft(Some(&context.conversation)).unwrap().text,
                "Synthetic prompt"
            );
        }
    }
}

fn sync_read(store: &mut Store, request: serde_json::Value) -> serde_json::Value {
    store
        .replication(serde_json::from_value(request).unwrap())
        .unwrap()
}

#[test]
fn native_replication_pages_have_bounded_items_bytes_and_lexical_cursors() {
    let fixture = Fixture::new();
    let mut store = fixture.open();
    for index in 0..70 {
        store
            .create_conversation(format!("Synthetic bounded {index}"))
            .unwrap();
    }
    let mut cursor = serde_json::Value::Null;
    let mut ids = Vec::new();
    loop {
        let page = sync_read(
            &mut store,
            serde_json::json!({"kind":"inventory","cursor":cursor}),
        );
        assert!(serde_json::to_vec(&page).unwrap().len() <= 32 * 1024);
        let entries = page["entries"].as_array().unwrap();
        assert!(entries.len() <= 63);
        assert!(!page.to_string().contains("Synthetic bounded"));
        ids.extend(
            entries
                .iter()
                .map(|entry| serde_json::from_value::<OperationId>(entry["id"].clone()).unwrap()),
        );
        cursor = page["next"].clone();
        if cursor.is_null() {
            break;
        }
        assert_eq!(cursor["after"], entries.last().unwrap()["id"]);
    }
    assert_eq!(ids.len(), 70);
    assert!(ids.windows(2).all(|pair| pair[0] < pair[1]));
    let revision = store.revision().unwrap();
    let oversized = ids[..9]
        .iter()
        .map(|id| serde_json::json!({"id":id,"body_key":null}))
        .collect::<Vec<_>>();
    assert!(store.replication(serde_json::from_value(serde_json::json!({"kind":"bodies","revision":revision,"entries":oversized,"tombstones":false})).unwrap()).is_err());
    let receipt_ids = vec![ids[0].clone(); 129];
    assert!(store
        .replication(
            serde_json::from_value(serde_json::json!({"kind":"receipts","ids":receipt_ids}))
                .unwrap()
        )
        .is_err());
}

/// Reusable host-only baseline. Setup and serialization are outside measured stages;
/// do not use these numbers as Android/Wear runtime latency or an optimization claim.
#[test]
#[ignore = "optimized host characterization; run explicitly with --release --ignored --nocapture"]
fn characterize_planning_vs_projection_at_short_and_near_store_budget() {
    use std::time::Instant;
    const REPETITIONS: usize = 20;
    for count in [4, 96] {
        let fixture = Fixture::new();
        let mut store = fixture.open();
        let conversation = store
            .create_conversation("Synthetic characterization".into())
            .unwrap()
            .operation
            .id;
        let mut head = None;
        for _ in 0..count {
            let context = store
                .context(conversation.clone(), head.clone(), 256 * 1024)
                .unwrap();
            let mut record = turn(&mut store, &context, &"p".repeat(16 * 1024));
            if let OperationBody::TurnCompleted { answer, .. } = &mut record.operation.body {
                *answer = Answer::Text {
                    text: "a".repeat(16 * 1024),
                };
            }
            let record = codec::record(record.operation).unwrap();
            head = Some(record.operation.id.clone());
            store.import_records(vec![record], false, None).unwrap();
        }
        let core = CoreStore::restore(store.snapshot().unwrap()).unwrap();
        let source_bytes = serde_json::to_vec(&core.export()).unwrap().len();
        assert!(source_bytes < MAX_CANONICAL_BYTES);
        let imported = codec::record(Operation {
            version: SCHEMA_VERSION,
            id: store.allocate_id().unwrap(),
            body: OperationBody::ConversationCreated {
                title: "Synthetic unrelated planning input".into(),
            },
        })
        .unwrap();
        let mut planning = Vec::new();
        let mut projection = Vec::new();
        for iteration in 0..=REPETITIONS {
            let start = Instant::now();
            let (plan, next) = std::hint::black_box(&core)
                .plan_with_store(Command::Import {
                    expected_revision: core.revision().into(),
                    records: vec![imported.clone()],
                })
                .unwrap();
            let plan_ns = start.elapsed().as_nanos();
            let start = Instant::now();
            let context = std::hint::black_box(&core)
                .select_context(ContextRequest {
                    expected_revision: core.revision().into(),
                    conversation: conversation.clone(),
                    selected_head: head.clone(),
                    max_turns: 20,
                    max_text_bytes: 256 * 1024,
                })
                .unwrap();
            let project_ns = start.elapsed().as_nanos();
            assert!(!context.turns.is_empty());
            assert_eq!(
                CoreStore::restore(plan.snapshot).unwrap().export(),
                next.export()
            );
            if iteration > 0 {
                planning.push(plan_ns);
                projection.push(project_ns);
            }
        }
        planning.sort_unstable();
        projection.sort_unstable();
        eprintln!("synthetic_host_baseline turns={count} records={} source_bytes={source_bytes} repetitions={REPETITIONS} warmup=1 planning_p50_ns={} planning_p95_ns={} projection_p50_ns={} projection_p95_ns={}", count+1, planning[10], planning[18], projection[10], projection[18]);
    }
}

#[test]
fn native_replication_is_revision_pinned_and_serves_acked_canonical_bodies_and_retired_receipts() {
    use voxgolem_core::conversation_sync::{inventory_page, InventoryCursor, InventoryRequest};
    let fixture = Fixture::new();
    let mut store = fixture.open();
    let record = store
        .create_conversation("Synthetic 😀 quote \" newline\n".into())
        .unwrap();
    let snapshot = store.snapshot().unwrap();
    let expected = inventory_page(
        &CoreStore::restore(snapshot.clone()).unwrap(),
        InventoryRequest {
            cursor: InventoryCursor {
                revision: snapshot.revision.clone(),
                after: None,
            },
            max_items: 128,
        },
    )
    .unwrap();
    let page = sync_read(
        &mut store,
        serde_json::json!({"kind":"inventory","cursor":null}),
    );
    assert_eq!(page, serde_json::to_value(&expected).unwrap());
    let peer = "00000000-0000-4000-8000-000000000002";
    let receipts = store
        .read(|core| receipts_after_commit(core, peer, std::slice::from_ref(&record.operation.id)))
        .unwrap();
    store.acknowledge(peer, receipts).unwrap();
    assert!(store
        .publications(Some(peer), 128, MAX_CANONICAL_BYTES, None)
        .unwrap()
        .is_empty());
    let bodies = sync_read(
        &mut store,
        serde_json::json!({"kind":"bodies","revision":snapshot.revision,"entries":page["entries"],"tombstones":false}),
    );
    let raw = bodies["bodies"][0]["body"].as_str().unwrap();
    assert_eq!(
        raw.as_bytes(),
        codec::encode(&record, MAX_CANONICAL_BYTES, "test")
            .unwrap()
            .as_bytes()
    );
    assert_eq!(codec::parse_record(raw).unwrap(), record);
    let reconciled = sync_read(
        &mut store,
        serde_json::json!({"kind":"reconcile","page":page}),
    );
    assert_eq!(reconciled["receipts"].as_array().unwrap().len(), 1);
    let mut bad = page.clone();
    bad["entries"][0]["body_key"] = "0".repeat(64).into();
    assert!(store
        .replication(
            serde_json::from_value(serde_json::json!({"kind":"reconcile","page":bad})).unwrap()
        )
        .is_err());
    let mut cursor =
        serde_json::json!({"revision":snapshot.revision,"after":{"replica":peer,"sequence":"01"}});
    assert!(store
        .replication(
            serde_json::from_value(serde_json::json!({"kind":"inventory","cursor":cursor}))
                .unwrap()
        )
        .is_err());
    store
        .delete_conversation(record.operation.id.clone())
        .unwrap();
    cursor["after"] = serde_json::Value::Null;
    assert_eq!(
        sync_read(
            &mut store,
            serde_json::json!({"kind":"inventory","cursor":cursor})
        )["restart"],
        true
    );
    assert_eq!(
        sync_read(
            &mut store,
            serde_json::json!({"kind":"bodies","revision":snapshot.revision,"entries":page["entries"],"tombstones":false})
        )["restart"],
        true
    );
    let exact = sync_read(
        &mut store,
        serde_json::json!({"kind":"receipts","ids":[record.operation.id]}),
    );
    assert_eq!(exact[0]["body_key"], record.body_key);
}

#[test]
fn native_replication_rejects_body_metadata_and_hash_conflicts_without_committing() {
    let source = Fixture::new();
    let mut source = source.open();
    let record = source
        .create_conversation("Synthetic source".into())
        .unwrap();
    let page = sync_read(
        &mut source,
        serde_json::json!({"kind":"inventory","cursor":null}),
    );
    let mut entries = page["entries"].clone();
    entries[0]["tombstone"] = true.into();
    assert!(source.replication(serde_json::from_value(serde_json::json!({"kind":"bodies","revision":page["revision"],"entries":entries,"tombstones":false})).unwrap()).is_err());
    let target = Fixture::new();
    let mut target = target.open();
    assert!(target
        .replication(
            serde_json::from_value(
                serde_json::json!({"kind":"import","records":[record],"entries":entries})
            )
            .unwrap()
        )
        .is_err());
    let mut forged = serde_json::to_value(&record).unwrap();
    forged["operation"]["body"]["title"] = "Synthetic changed body".into();
    assert!(target
        .replication(
            serde_json::from_value(
                serde_json::json!({"kind":"import","records":[forged],"entries":page["entries"]})
            )
            .unwrap()
        )
        .is_err());
    assert_eq!(target.snapshot().unwrap(), StoreSnapshot::empty());
    assert!(target.receipts_to_publish(128, None).unwrap().is_empty());
    let imported = sync_read(
        &mut target,
        serde_json::json!({"kind":"import","records":[record.clone()],"entries":page["entries"]}),
    );
    let target_replica = target.replica().unwrap();
    let expected_receipts = target
        .read(|core| {
            receipts_after_commit(
                core,
                &target_replica,
                std::slice::from_ref(&record.operation.id),
            )
        })
        .unwrap();
    assert_eq!(imported["base_revision"], "0");
    assert_eq!(imported["revision"], "1");
    assert_eq!(
        imported["accepted"],
        serde_json::json!([record.operation.id.clone()])
    );
    assert_eq!(imported["duplicates"], serde_json::json!([]));
    assert_eq!(imported["suppressed"], serde_json::json!([]));
    assert_eq!(imported["quarantined"], serde_json::json!([]));
    assert_eq!(
        imported["receipts"],
        serde_json::to_value(&expected_receipts).unwrap()
    );
    assert_eq!(target.snapshot().unwrap().records, vec![record]);
}

#[test]
fn native_replication_omits_quarantine_bodies_but_keeps_exact_known_receipts() {
    let fixture = Fixture::new();
    let mut store = fixture.open();
    let creation = store
        .create_conversation("Synthetic quarantine".into())
        .unwrap();
    let conversation = creation.operation.id.clone();
    let context = store
        .context(conversation.clone(), None, 256 * 1024)
        .unwrap();
    let authentic = turn(&mut store, &context, "Synthetic authentic parent");
    let mut poison = turn(&mut store, &context, "Synthetic pending poison");
    if let OperationBody::TurnCompleted {
        parent, provenance, ..
    } = &mut poison.operation.body
    {
        *parent = Some(authentic.operation.id.clone());
        provenance.context = vec![
            OperationId {
                replica: "00000000-0000-4000-8000-000000000088".into(),
                sequence: "88".into(),
            },
            authentic.operation.id.clone(),
        ];
    }
    let poison = codec::record(poison.operation).unwrap();
    store
        .import_records(vec![poison.clone()], true, None)
        .unwrap();
    let advertised = sync_read(
        &mut store,
        serde_json::json!({"kind":"inventory","cursor":null}),
    );
    let poison_entry = advertised["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["body_key"] == poison.body_key)
        .unwrap()
        .clone();
    store.import_records(vec![authentic], true, None).unwrap();
    assert_eq!(store.snapshot().unwrap().quarantine.len(), 1);
    let current = sync_read(
        &mut store,
        serde_json::json!({"kind":"inventory","cursor":null}),
    );
    assert!(!current["entries"]
        .as_array()
        .unwrap()
        .iter()
        .any(|entry| entry["body_key"] == poison.body_key));
    let served = sync_read(
        &mut store,
        serde_json::json!({"kind":"bodies","revision":current["revision"],"entries":[poison_entry],"tombstones":false}),
    );
    assert!(served["bodies"].as_array().unwrap().is_empty());
    assert_eq!(
        served["unavailable"][0],
        serde_json::to_value(&poison.operation.id).unwrap()
    );
    let reconciled = sync_read(
        &mut store,
        serde_json::json!({"kind":"reconcile","page":advertised}),
    );
    assert!(reconciled["receipts"]
        .as_array()
        .unwrap()
        .iter()
        .any(|receipt| receipt["body_key"] == poison.body_key));
    store.delete_conversation(conversation).unwrap();
    let receipts = sync_read(
        &mut store,
        serde_json::json!({"kind":"receipts","ids":[poison.operation.id]}),
    );
    assert_eq!(receipts[0]["body_key"], poison.body_key);
}

#[test]
fn mobile_preparation_allocates_atomically_keeps_late_cancel_state_and_obeys_total_budget() {
    let fixture = Fixture::new();
    let mut store = fixture.open();
    let empty = Selection {
        conversation: None,
        selected_head: None,
    };
    store.save_draft(None, "Synthetic prompt", true).unwrap();
    let prepared = store
        .prepare_request(empty.clone(), "Synthetic prompt", 256 * 1024, false)
        .unwrap();
    let conversation = prepared.context.selection.conversation.clone();
    assert_eq!(prepared.request.sequence, "2");
    // No execute/settle follows: successful preparation survives late cancellation/reopen.
    assert_eq!(
        fixture.open().selection().unwrap().conversation,
        Some(conversation.clone())
    );
    assert_eq!(
        fixture.open().draft(Some(&conversation)).unwrap().text,
        "Synthetic prompt"
    );
    assert_eq!(store.draft(None).unwrap().text, "");
    let selected = store.selection().unwrap();
    fixture
        .open()
        .create_conversation("Synthetic unrelated".into())
        .unwrap();
    let first = turn(
        &mut store,
        &prepared.context.selection,
        "Synthetic history prompt",
    );
    store
        .import_records(vec![first.clone()], false, Some(&first.operation.id))
        .unwrap();
    // The local owner is still explicitly null. A sole retained head must not
    // silently become the voice parent, nor overwrite its pre-existing draft.
    let null_parent_owner = store.selection().unwrap();
    assert!(null_parent_owner.selected_head.is_none());
    assert!(store
        .prepare_request(null_parent_owner, "Synthetic stale voice", 256 * 1024, true)
        .is_err());
    assert_eq!(
        store.draft(Some(&conversation)).unwrap().text,
        "Synthetic prompt"
    );
    // Actor reconciliation explicitly accepts the new head before typed admission.
    store
        .select(Selection {
            conversation: Some(conversation),
            selected_head: Some(first.operation.id.clone()),
        })
        .unwrap();
    let selected_head = store.selection().unwrap();
    let prompt = "Synthetic next prompt";
    let history_bytes = first.operation.context_bytes();
    let exact = store
        .prepare_request(selected_head, prompt, history_bytes + prompt.len(), true)
        .unwrap();
    assert_eq!(exact.context.selection.turns.len(), 1);
    assert_eq!(
        exact.context.selection.parent,
        Some(first.operation.id.clone())
    );
    let pinned_request = exact.request.clone();
    // Later owner changes cannot rewrite the admitted context or request identity.
    store
        .select(Selection {
            conversation: None,
            selected_head: None,
        })
        .unwrap();
    let later_selection = store.selection().unwrap();
    let later = store
        .prepare_request(later_selection, "Synthetic later prompt", 4096, true)
        .unwrap();
    assert_ne!(later.request, pinned_request);
    assert_eq!(exact.request, pinned_request);
    assert_eq!(exact.context.selection.parent, Some(first.operation.id));
    assert_eq!(exact.context.selection.turns.len(), 1);
    let before = store.snapshot().unwrap();
    assert!(store
        .prepare_request(selected, prompt, 256 * 1024, true)
        .is_err());
    assert_eq!(store.snapshot().unwrap(), before);
    assert!(store
        .prepare_request(empty, prompt, 256 * 1024, false)
        .is_err());
}

#[test]
fn failed_mobile_preparation_rolls_back_identity_selection_and_preserves_input_draft() {
    let fixture = Fixture::new();
    let mut store = fixture.open();
    store.save_draft(None, "Synthetic rollback", true).unwrap();
    store.db.execute_batch("CREATE TRIGGER synthetic_mobile_failure BEFORE INSERT ON history_drafts BEGIN SELECT RAISE(ABORT,'synthetic'); END;").unwrap();
    let empty = Selection {
        conversation: None,
        selected_head: None,
    };
    assert!(store
        .prepare_request(empty.clone(), "Synthetic rollback", 256 * 1024, true)
        .is_err());
    assert_eq!(store.selection().unwrap(), empty);
    assert_eq!(store.snapshot().unwrap(), StoreSnapshot::empty());
    assert_eq!(store.draft(None).unwrap().text, "Synthetic rollback");
    assert_eq!(store.allocate_id().unwrap().sequence, "1");
}

#[test]
fn timing_projects_context_at_admission_not_after_inference() {
    let fixture = Fixture::new();
    let mut store = fixture.open();
    let (prepared, summary) = timing::capture(true, || {
        store.prepare_request(
            Selection {
                conversation: None,
                selected_head: None,
            },
            "Synthetic prompt",
            4096,
            true,
        )
    });
    let prepared = prepared.unwrap();
    assert_eq!(summary.unwrap().metrics.projection.count, 1);
    let mut body = turn(&mut store, &prepared.context.selection, "Synthetic prompt")
        .operation
        .body;
    if let OperationBody::TurnCompleted { request, .. } = &mut body {
        *request = prepared.request;
    }
    let revision = store.revision().unwrap();
    let selection = store.selection().unwrap();
    let (failed, summary) = timing::capture(true, || {
        store.prepare_request(selection.clone(), "Synthetic prompt", usize::MAX, true)
    });
    assert!(matches!(
        failed,
        Err(Error::Domain(DomainError::LimitExceeded { .. }))
    ));
    let metrics = summary.unwrap().metrics;
    assert_eq!(metrics.projection.count, 0);
    assert_eq!(metrics.sqlite_commit.count, 0);
    assert_eq!(store.revision().unwrap(), revision);
    assert_eq!(store.selection().unwrap(), selection);
    let (settled, summary) = timing::capture(true, || {
        store.complete_selected_body_with_authority(
            body,
            || ((), CompletionDecision::Authorized),
            |_, attempt| match attempt {
                CompletionAttempt::Finished(result) => result,
                _ => unreachable!(),
            },
        )
    });
    assert!(settled.unwrap().saved);
    assert_eq!(summary.unwrap().metrics.projection.count, 0);
}

#[test]
fn timing_codec_counts_individual_serializations_and_json_parses() {
    let operation = Operation {
        version: SCHEMA_VERSION,
        id: OperationId {
            replica: "00000000-0000-4000-8000-000000000001".into(),
            sequence: "1".into(),
        },
        body: OperationBody::ConversationCreated {
            title: "Synthetic codec timing".into(),
        },
    };
    let (record, summary) = timing::capture(true, || codec::record(operation).unwrap());
    let metrics = summary.unwrap().metrics;
    assert_eq!(metrics.record_encode.count, 1);
    assert_eq!(metrics.record_decode.count, 0);
    let raw = serde_json::to_string(&record).unwrap();
    let (parsed, summary) = timing::capture(true, || codec::parse_record(&raw).unwrap());
    assert_eq!(parsed, record);
    let metrics = summary.unwrap().metrics;
    assert_eq!(metrics.record_decode.count, 2);
    assert_eq!(metrics.record_encode.count, 1);
}

#[test]
fn timing_capture_is_scoped_bounded_and_content_free() {
    use timing::{capture, measure, Phase};
    assert_eq!(capture(false, || 42).0, 42);
    assert!(capture(false, || measure(Phase::sqlite_read, || 42))
        .1
        .is_none());
    let (_, outer) = capture(true, || {
        measure(Phase::sqlite_read, || ());
        let (_, nested) = capture(true, || measure(Phase::sqlite_write, || ()));
        assert_eq!(nested.unwrap().metrics.sqlite_write.count, 1);
        capture(false, || measure(Phase::sqlite_read, || ()));
        assert!(std::panic::catch_unwind(|| capture(true, || panic!("synthetic"))).is_err());
        measure(Phase::sqlite_read, || ());
    });
    let outer = outer.unwrap();
    assert_eq!(outer.metrics.sqlite_read.count, 2);
    assert_eq!(outer.metrics.sqlite_write.count, 0);
    let (_, repeated) = capture(true, || {
        for _ in 0..10_000 {
            measure(Phase::sqlite_read, || ());
        }
    });
    assert_eq!(repeated.unwrap().metrics.sqlite_read.count, 10_000);
    let json = serde_json::to_value(outer).unwrap();
    assert_eq!(json["schema"], 1);
    assert_eq!(json["metrics"].as_object().unwrap().len(), 13);
    for metric in json["metrics"].as_object().unwrap().values() {
        assert_eq!(metric.as_object().unwrap().len(), 3);
        for field in ["count", "total_ns", "max_ns"] {
            assert!(metric[field].is_u64());
        }
    }
    assert!(capture(false, || ()).1.is_none());
}

#[test]
fn timing_actual_storage_counts_refresh_and_failures() {
    let fixture = Fixture::new();
    let mut store = fixture.open();
    let (result, disabled) = timing::capture(false, || store.revision());
    assert_eq!(result.unwrap(), "0");
    assert!(disabled.is_none());
    let (result, summary) = timing::capture(true, || store.revision());
    assert_eq!(result.unwrap(), "0");
    let m = summary.unwrap().metrics;
    assert_eq!(
        (
            m.sqlite_begin.count,
            m.sqlite_read.count,
            m.sqlite_commit.count
        ),
        (1, 1, 1)
    );
    assert_eq!(m.projection.count, 1);
    assert_eq!(m.refresh_reload.count, 0);
    let (_, summary) = timing::capture(true, || {
        store.save_draft(None, "Synthetic draft", false).unwrap()
    });
    let m = summary.unwrap().metrics;
    assert_eq!(
        (
            m.sqlite_write.count,
            m.sqlite_read.count,
            m.quota_check.count
        ),
        (1, 2, 1)
    );
    let (_, summary) = timing::capture(true, || {
        store
            .select(Selection {
                conversation: None,
                selected_head: None,
            })
            .unwrap()
    });
    assert_eq!(summary.unwrap().metrics.sqlite_write.count, 1);
    let mut other = fixture.open();
    other.create_conversation("Synthetic title".into()).unwrap();
    let (_, summary) = timing::capture(true, || store.revision().unwrap());
    assert_eq!(summary.unwrap().metrics.refresh_reload.count, 1);
    let (_, summary) = timing::capture(true, || store.revision().unwrap());
    assert_eq!(summary.unwrap().metrics.refresh_reload.count, 0);
    let conversation = store
        .create_conversation("Synthetic completion".into())
        .unwrap();
    let context = store
        .context(conversation.operation.id, None, 4096)
        .unwrap();
    let body = turn(&mut store, &context, "Synthetic prompt")
        .operation
        .body;
    let (result, summary) = timing::capture(true, || store.complete_body(body));
    assert!(result.unwrap().saved);
    let m = summary.unwrap().metrics;
    assert_eq!((m.sqlite_begin.count, m.sqlite_commit.count), (1, 1));
    assert_eq!((m.sqlite_write.count, m.sqlite_read.count), (6, 8));
    assert_eq!(m.reducer.count, 1);
    assert!(m.record_encode.count > 0);
    // Deferred foreign-key violation fails the real COMMIT, not a simulated timer.
    store.db.execute_batch("CREATE TABLE synthetic_parent(id INTEGER PRIMARY KEY); CREATE TABLE synthetic_child(id INTEGER REFERENCES synthetic_parent(id) DEFERRABLE INITIALLY DEFERRED);").unwrap();
    let (result, summary) = timing::capture(true, || {
        let tx = store.db.timed_transaction()?;
        tx.timed_execute("INSERT INTO synthetic_child VALUES(1)", [])?;
        tx.timed_commit()
    });
    assert!(result.is_err());
    assert_eq!(summary.unwrap().metrics.sqlite_commit.count, 1);
    store.db.busy_timeout(std::time::Duration::ZERO).unwrap();
    let _lock = other
        .db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    let (result, summary) = timing::capture(true, || store.allocate_id());
    assert!(result.is_err());
    assert_eq!(summary.unwrap().metrics.sqlite_begin.count, 1);
}
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

#[test]
fn selected_completion_advances_only_its_matching_owner_atomically() {
    let fixture = Fixture::new();
    let mut store = fixture.open();
    let prepared = store
        .prepare_request(
            Selection {
                conversation: None,
                selected_head: None,
            },
            "Synthetic prompt",
            4096,
            true,
        )
        .unwrap();
    let mut body = turn(&mut store, &prepared.context.selection, "Synthetic prompt")
        .operation
        .body;
    if let OperationBody::TurnCompleted { request, .. } = &mut body {
        *request = prepared.request;
    }
    let selected = store.selection().unwrap();
    store.complete_selected_body_with_authority(
        body.clone(),
        || ((), CompletionDecision::Cancelled),
        |_, attempt| assert!(matches!(attempt, CompletionAttempt::Cancelled)),
    );
    assert_eq!(store.selection().unwrap(), selected);
    assert_eq!(
        store.draft(selected.conversation.as_ref()).unwrap().text,
        "Synthetic prompt"
    );
    let saved = store.complete_selected_body_with_authority(
        body.clone(),
        || ((), CompletionDecision::Authorized),
        |_, attempt| match attempt {
            CompletionAttempt::Finished(result) => result.unwrap(),
            _ => unreachable!(),
        },
    );
    assert!(saved.saved);
    assert_eq!(
        store.selection().unwrap().selected_head,
        Some(saved.record.operation.id)
    );
    let other = store
        .create_conversation("Synthetic other owner".into())
        .unwrap()
        .operation
        .id;
    let other_selected = Selection {
        conversation: Some(other),
        selected_head: None,
    };
    store.select(other_selected.clone()).unwrap();
    store.complete_selected_body_with_authority(
        body,
        || ((), CompletionDecision::Authorized),
        |_, attempt| {
            assert!(matches!(attempt, CompletionAttempt::Finished(Ok(_))));
        },
    );
    assert_eq!(store.selection().unwrap(), other_selected);
}

#[test]
fn coarse_replication_import_returns_commit_outcome_and_exact_receipts_atomically() {
    let source_fixture = Fixture::new();
    let mut source = source_fixture.open();
    let created = source
        .create_conversation("Synthetic coarse import".into())
        .unwrap();
    let context = source
        .context(created.operation.id.clone(), None, 256 * 1024)
        .unwrap();
    let turn_record = turn(&mut source, &context, "Synthetic coarse turn");
    source
        .import_records(vec![turn_record.clone()], false, None)
        .unwrap();
    let page = sync_read(
        &mut source,
        serde_json::json!({"kind":"inventory","cursor":null}),
    );
    let entries = page["entries"].as_array().unwrap().clone();
    assert_eq!(entries.len(), 2);

    let target_fixture = Fixture::new();
    let mut target = target_fixture.open();
    let ids = vec![
        created.operation.id.clone(),
        turn_record.operation.id.clone(),
    ];

    // Conflicting/rejected imports do not commit or produce receipts.
    let mut bad_entries = entries.clone();
    bad_entries[1]["body_key"] = "0".repeat(64).into();
    assert!(target
        .replication(
            serde_json::from_value(serde_json::json!({
                "kind": "import",
                "records": [created.clone(), turn_record.clone()],
                "entries": bad_entries,
            }))
            .unwrap()
        )
        .is_err());
    assert!(target
        .replication(
            serde_json::from_value(serde_json::json!({
                "kind": "import",
                "records": vec![created.clone(); 9],
                "entries": vec![entries[0].clone(); 9],
            }))
            .unwrap()
        )
        .is_err());
    assert_eq!(target.revision().unwrap(), "0");
    assert!(target.receipts_to_publish(128, None).unwrap().is_empty());

    let committed = sync_read(
        &mut target,
        serde_json::json!({
            "kind": "import",
            "records": [created.clone(), turn_record.clone()],
            "entries": entries,
        }),
    );
    let target_replica = target.replica().unwrap();
    let expected_receipts = target
        .read(|core| receipts_after_commit(core, &target_replica, &ids))
        .unwrap();
    assert_eq!(committed["base_revision"], "0");
    assert_eq!(committed["revision"], target.revision().unwrap());
    assert_eq!(committed["accepted"], serde_json::to_value(&ids).unwrap());
    assert_eq!(committed["duplicates"], serde_json::json!([]));
    assert_eq!(committed["suppressed"], serde_json::json!([]));
    assert_eq!(committed["quarantined"], serde_json::json!([]));
    assert_eq!(
        committed["receipts"],
        serde_json::to_value(&expected_receipts).unwrap()
    );
    assert_eq!(
        target.receipts_to_publish(128, None).unwrap(),
        expected_receipts
    );

    // Duplicate import preserves revision and still returns exact durable receipts in one call.
    let duplicate = sync_read(
        &mut target,
        serde_json::json!({
            "kind": "import",
            "records": [created, turn_record],
            "entries": entries,
        }),
    );
    assert_eq!(duplicate["base_revision"], committed["revision"]);
    assert_eq!(duplicate["revision"], committed["revision"]);
    assert_eq!(duplicate["accepted"], serde_json::json!([]));
    assert_eq!(duplicate["duplicates"], serde_json::to_value(&ids).unwrap());
    assert_eq!(
        duplicate["receipts"],
        serde_json::to_value(&expected_receipts).unwrap()
    );
}

#[test]
fn prepared_replication_bodies_enforce_batch_and_gateway_caps_and_release_read_lock() {
    use voxgolem_core::conversation_sync::{MAX_BATCH_BYTES, MAX_BATCH_RECORDS, MAX_RECORD_BYTES};
    let fixture = Fixture::new();
    let mut store = fixture.open();

    let created = store.create_conversation("Synthetic batch".into()).unwrap();
    let conversation = created.operation.id.clone();
    let context = store
        .context(conversation.clone(), None, 256 * 1024)
        .unwrap();

    // Heavy JSON-escaping characters (quotes, backslashes, newlines, control chars) stay within
    // the 2 MiB record cap, 4 MiB batch cap, and 16 MiB gateway envelope.
    // Control chars like \x01 serialize as 6-byte "\u0001" in JSON, so ~200 KiB of "\x01"
    // plus quotes/backslashes/newlines produces ~1.3 MiB per encoded record while keeping raw
    // turn text <= 256 KiB. Three such records fit in a 3.9 MiB store snapshot, and requesting
    // additional large records exercises the 4 MiB aggregate batch cap (`unavailable`).
    let mut heavy_records = Vec::new();
    for _ in 0..3 {
        let mut r = turn(&mut store, &context, &"\\\"\n\x01".repeat(6000));
        if let OperationBody::TurnCompleted {
            answer, sources, ..
        } = &mut r.operation.body
        {
            sources.clear();
            *answer = Answer::Text {
                text: format!("{}{}", "\\\"\n\r\t".repeat(4000), "\x01".repeat(200 * 1024)),
            };
        }
        let rec = codec::record(r.operation).unwrap();
        let encoded = codec::encode(&rec, MAX_RECORD_BYTES, "replication_record_bytes").unwrap();
        assert!(encoded.len() > 1200 * 1024 && encoded.len() <= MAX_RECORD_BYTES);
        heavy_records.push(rec);
    }
    store
        .import_records(heavy_records.clone(), false, None)
        .unwrap();

    let page = sync_read(
        &mut store,
        serde_json::json!({"kind":"inventory","cursor":null}),
    );
    let all_entries = page["entries"].as_array().unwrap().clone();
    assert_eq!(all_entries.len(), 4);
    assert!(all_entries.len() <= MAX_BATCH_RECORDS);

    let prepared = sync_read(
        &mut store,
        serde_json::json!({
            "kind": "bodies",
            "revision": page["revision"],
            "entries": all_entries,
            "tombstones": false,
        }),
    );
    assert_eq!(prepared["restart"], false);
    assert_eq!(prepared["bodies"].as_array().unwrap().len(), 4);
    assert_eq!(prepared["unavailable"], serde_json::json!([]));
    assert!(codec::encode(
        &prepared,
        16 * 1024 * 1024 - 4096,
        "replication_gateway_bytes"
    )
    .is_ok());

    // Verify that the SQLite read lock is released upon return so another Store connection on
    // the same database can immediately commit a write without DatabaseBusy even in DELETE mode.
    store
        .db
        .execute_batch("PRAGMA journal_mode=DELETE")
        .unwrap();
    let rev = store.revision().unwrap();
    let delete_mode_batch = sync_read(
        &mut store,
        serde_json::json!({
            "kind": "bodies",
            "revision": rev,
            "entries": &all_entries[..2],
            "tombstones": false,
        }),
    );
    assert_eq!(delete_mode_batch["restart"], false);
    let mut writer = fixture.open();
    writer.db.busy_timeout(std::time::Duration::ZERO).unwrap();
    writer
        .db
        .execute_batch("PRAGMA journal_mode=DELETE")
        .unwrap();
    writer
        .create_conversation("Synthetic immediate writer commit".into())
        .unwrap();
    drop(writer);
    store.db.execute_batch("PRAGMA journal_mode=WAL").unwrap();
    let _ = store.revision().unwrap();

    // Add a 4th heavy record into the store core so the 5 records exceed the
    // 4 MiB aggregate batch cap (`MAX_BATCH_BYTES`) while each record is <= 2 MiB (`MAX_RECORD_BYTES`).
    let mut fourth_op = turn(&mut store, &context, &"\\\"\n\x01".repeat(6000)).operation;
    if let OperationBody::TurnCompleted {
        answer, sources, ..
    } = &mut fourth_op.body
    {
        sources.clear();
        *answer = Answer::Text {
            text: format!("{}{}", "\\\"\n\r\t".repeat(4000), "\x01".repeat(200 * 1024)),
        };
    }
    let fourth_rec = codec::record(fourth_op).unwrap();
    let fourth_entry = serde_json::json!({
        "id": fourth_rec.operation.id,
        "conversation": fourth_rec.operation.conversation(),
        "body_key": fourth_rec.body_key,
        "tombstone": false,
    });
    let mut snap = store.core.export();
    snap.records.push(fourth_rec.clone());
    snap.records
        .sort_by(|a, b| a.operation.id.cmp(&b.operation.id));
    store.core = CoreStore::restore(snap).unwrap();

    let mut five_entries = all_entries.clone();
    five_entries.push(fourth_entry);
    assert_eq!(five_entries.len(), 5);
    assert!(five_entries.len() <= MAX_BATCH_RECORDS);

    let current_rev = store.core.revision().to_string();
    let capped = sync_read(
        &mut store,
        serde_json::json!({
            "kind": "bodies",
            "revision": current_rev,
            "entries": five_entries,
            "tombstones": false,
        }),
    );
    assert_eq!(capped["restart"], false);
    let bodies = capped["bodies"].as_array().unwrap();
    assert_eq!(bodies.len(), 4);
    assert!(bodies.len() <= MAX_BATCH_RECORDS);
    let mut aggregate_bytes = 0usize;
    for body in bodies {
        let raw = body["body"].as_str().unwrap();
        assert!(raw.len() <= MAX_RECORD_BYTES);
        aggregate_bytes += raw.len();
        let parsed = codec::parse_record(raw).unwrap();
        assert_eq!(
            body["entry"]["id"],
            serde_json::to_value(&parsed.operation.id).unwrap()
        );
    }
    assert!(aggregate_bytes <= MAX_BATCH_BYTES);
    let fourth_raw =
        codec::encode(&fourth_rec, MAX_RECORD_BYTES, "replication_record_bytes").unwrap();
    assert!(aggregate_bytes + fourth_raw.len() > MAX_BATCH_BYTES);
    assert_eq!(
        capped["unavailable"],
        serde_json::json!([fourth_rec.operation.id])
    );
    assert!(codec::encode(
        &capped,
        16 * 1024 * 1024 - 4096,
        "replication_gateway_bytes"
    )
    .is_ok());
}
