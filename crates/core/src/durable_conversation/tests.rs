use super::*;

const A: &str = "00000000-0000-4000-8000-000000000001";
const B: &str = "00000000-0000-4000-8000-000000000002";
fn id(replica: &str, sequence: u64) -> OperationId {
    OperationId {
        replica: replica.into(),
        sequence: sequence.to_string(),
    }
}
fn key(n: u64) -> String {
    format!("{n:064x}")
}
fn record(operation: Operation) -> Record {
    Record {
        body_key: key(operation.id.sequence.parse().unwrap()),
        operation,
    }
}
fn create() -> Record {
    record(Operation {
        version: 1,
        id: id(A, 1),
        body: OperationBody::ConversationCreated {
            title: "Synthetic chat".into(),
        },
    })
}
fn turn(
    replica: &str,
    sequence: u64,
    parent: Option<OperationId>,
    context: Vec<OperationId>,
) -> Record {
    record(Operation {
        version: 1,
        id: id(replica, sequence),
        body: OperationBody::TurnCompleted {
            conversation: id(A, 1),
            parent,
            request: id(replica, sequence + 1000),
            prompt: "Synthetic question".into(),
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
                context,
            },
        },
    })
}
fn delete() -> Record {
    record(Operation {
        version: 1,
        id: id(B, 99),
        body: OperationBody::ConversationDeleted {
            conversation: id(A, 1),
        },
    })
}
fn import(store: &Store, records: Vec<Record>) -> Store {
    Store::restore(
        store
            .plan(Command::Import {
                expected_revision: store.revision().into(),
                records,
            })
            .unwrap()
            .snapshot,
    )
    .unwrap()
}
fn empty() -> Store {
    Store::restore(StoreSnapshot::empty()).unwrap()
}

fn titled_create(title: &str) -> Record {
    let mut record = create();
    record.operation.body = OperationBody::ConversationCreated {
        title: title.into(),
    };
    record
}

fn prompted_root(replica: &str, sequence: u64, text: &str) -> Record {
    let mut record = turn(replica, sequence, None, vec![]);
    if let OperationBody::TurnCompleted { prompt, .. } = &mut record.operation.body {
        *prompt = text.into();
    }
    record
}

#[test]
fn title_preview_projects_stored_root_without_mutating_canonical_records() {
    let store = import(
        &empty(),
        vec![
            titled_create("Conversation"),
            prompted_root(A, 2, " \tSynthetic\u{a0} question\u{2003}\n about 🧪\r "),
            turn(A, 3, Some(id(A, 2)), vec![id(A, 2)]),
        ],
    );
    let canonical = serde_json::to_vec(&store.export()).unwrap();
    let view = store.snapshot();
    assert_eq!(
        view.conversations[0].title.as_deref(),
        Some("Synthetic question about 🧪")
    );
    assert_eq!(view.records, store.export().records);
    assert_eq!(serde_json::to_vec(&store.export()).unwrap(), canonical);
    assert_eq!(store.export().version, SCHEMA_VERSION);
    let restored = Store::restore(serde_json::from_slice(&canonical).unwrap()).unwrap();
    assert_eq!(restored.snapshot(), view);
}

#[test]
fn title_preview_is_bounded_by_unicode_scalars_including_ellipsis() {
    for character in ['x', '界', '🧪'] {
        for length in [1, 63, 64, 65, 1024] {
            let prompt = character.to_string().repeat(length);
            let store = import(
                &empty(),
                vec![titled_create("Conversation"), prompted_root(A, 2, &prompt)],
            );
            let title = store.snapshot().conversations[0].title.clone().unwrap();
            let expected = if length > 64 {
                format!("{}…", character.to_string().repeat(63))
            } else {
                prompt
            };
            assert_eq!(title, expected);
            assert!(title.chars().count() <= 64);
            assert!(title.len() <= 256);
        }
    }
}

#[test]
fn title_preview_uses_lexical_root_id_not_arrival_order_or_child_prompt() {
    let early = prompted_root(B, 2, "Synthetic higher replica");
    let lower = prompted_root(A, 10, "Synthetic lexical first root");
    let other = prompted_root(A, 2, "Synthetic numeric first root");
    let child = turn(A, 3, Some(id(B, 2)), vec![id(B, 2)]);
    let initial = import(
        &empty(),
        vec![titled_create("Conversation"), early.clone(), child.clone()],
    );
    assert_eq!(
        initial.snapshot().conversations[0].title.as_deref(),
        Some("Synthetic higher replica")
    );
    let later = import(&initial, vec![other.clone(), lower.clone()]);
    let reversed = import(
        &empty(),
        vec![lower, other, child, early, titled_create("Conversation")],
    );
    assert_eq!(
        later.snapshot().conversations[0].title.as_deref(),
        Some("Synthetic lexical first root")
    );
    assert_eq!(
        later.snapshot().conversations,
        reversed.snapshot().conversations
    );
}

#[test]
fn title_preview_keeps_first_root_even_when_its_preview_is_the_generic_word() {
    let store = import(
        &empty(),
        vec![
            titled_create("Conversation"),
            prompted_root(A, 2, " \nConversation\t "),
            prompted_root(A, 3, "Synthetic later root"),
        ],
    );
    assert_eq!(
        store.snapshot().conversations[0].title.as_deref(),
        Some("Conversation")
    );
}

#[test]
fn title_preview_preserves_custom_empty_missing_and_deleted_titles() {
    for title in ["Synthetic custom", "conversation", " Conversation ", ""] {
        let store = import(
            &empty(),
            vec![titled_create(title), prompted_root(A, 2, "Synthetic root")],
        );
        assert_eq!(
            store.snapshot().conversations[0].title.as_deref(),
            Some(title)
        );
    }
    let pending = import(
        &empty(),
        vec![prompted_root(A, 2, "Synthetic pending root")],
    );
    assert_eq!(pending.snapshot().conversations[0].title, None);
    assert_eq!(pending.snapshot().pending, vec![id(A, 2)]);
    let saved = import(&pending, vec![titled_create("Conversation")]);
    assert_eq!(
        saved.snapshot().conversations[0].title.as_deref(),
        Some("Synthetic pending root")
    );
    let deleted = import(&saved, vec![delete()]);
    assert!(deleted.snapshot().conversations[0].deleted);
    assert_eq!(deleted.snapshot().conversations[0].title, None);
    assert!(deleted
        .export()
        .records
        .iter()
        .all(|r| matches!(r.operation.body, OperationBody::ConversationDeleted { .. })));
}

#[test]
fn title_preview_ignores_pending_and_quarantined_turns_and_names_refused_roots() {
    let mut pending_child = turn(A, 2, Some(id(B, 8)), vec![]);
    if let OperationBody::TurnCompleted { prompt, .. } = &mut pending_child.operation.body {
        *prompt = "Synthetic ineligible child".into();
    }
    let store = import(&empty(), vec![titled_create("Conversation"), pending_child]);
    assert_eq!(
        store.snapshot().conversations[0].title.as_deref(),
        Some("Conversation")
    );
    let mut refused = prompted_root(A, 3, "Synthetic refused root");
    if let OperationBody::TurnCompleted { answer, .. } = &mut refused.operation.body {
        *answer = Answer::Refusal {
            reason: "Synthetic refusal".into(),
        };
    }
    let store = import(&store, vec![refused]);
    let wrong_parent = record(Operation {
        version: SCHEMA_VERSION,
        id: id(B, 8),
        body: OperationBody::ConversationCreated {
            title: "Synthetic unrelated".into(),
        },
    });
    let quarantined = import(&store, vec![wrong_parent]);
    assert!(quarantined
        .export()
        .quarantine
        .iter()
        .any(|entry| entry.id == id(A, 2)));
    assert_eq!(
        quarantined.snapshot().conversations[0].title.as_deref(),
        Some("Synthetic refused root")
    );
}

#[test]
fn reordered_restore_and_duplicate_import_have_the_same_graph() {
    let root = turn(A, 2, None, vec![]);
    let child = turn(A, 3, Some(id(A, 2)), vec![id(A, 2)]);
    let pending = import(&empty(), vec![child.clone()]);
    assert_eq!(pending.snapshot().pending, vec![id(A, 3)]);
    let reordered = import(&pending, vec![root.clone(), create()]);
    let ordered = import(&empty(), vec![create(), root, child.clone()]);
    assert_eq!(
        reordered.history(&id(A, 1), &id(A, 3)).unwrap(),
        ordered.history(&id(A, 1), &id(A, 3)).unwrap()
    );
    let plan = reordered
        .plan(Command::Import {
            expected_revision: reordered.revision().into(),
            records: vec![child],
        })
        .unwrap();
    assert_eq!(plan.snapshot.revision, reordered.revision());
    assert_eq!(plan.duplicates, vec![id(A, 3)]);
}

#[test]
fn conflicts_cycles_wrong_conversation_and_invalid_context_are_atomic() {
    let store = import(&empty(), vec![create(), turn(A, 2, None, vec![])]);
    let mut conflict = turn(A, 2, None, vec![]);
    if let OperationBody::TurnCompleted { prompt, .. } = &mut conflict.operation.body {
        *prompt = "Different synthetic body".into();
    }
    assert!(matches!(
        store.plan(Command::Import {
            expected_revision: store.revision().into(),
            records: vec![turn(A, 3, Some(id(A, 2)), vec![]), conflict]
        }),
        Err(DomainError::IdConflict { .. })
    ));
    assert_eq!(store.snapshot().records.len(), 2);
    let cycles = vec![
        turn(A, 3, Some(id(A, 4)), vec![]),
        turn(A, 4, Some(id(A, 3)), vec![]),
    ];
    assert!(matches!(
        store.plan(Command::Import {
            expected_revision: store.revision().into(),
            records: cycles
        }),
        Err(DomainError::Cycle { .. })
    ));
    assert!(matches!(
        store.plan(Command::Import {
            expected_revision: store.revision().into(),
            records: vec![turn(A, 3, Some(id(A, 1)), vec![])]
        }),
        Err(DomainError::InvalidReference { .. })
    ));
    assert!(matches!(
        store.plan(Command::Import {
            expected_revision: store.revision().into(),
            records: vec![turn(A, 3, Some(id(A, 2)), vec![id(B, 5)])]
        }),
        Err(DomainError::InvalidContext { .. })
    ));
}

#[test]
fn forks_require_selection_and_context_is_a_window_not_retention() {
    let store = import(
        &empty(),
        vec![
            create(),
            turn(A, 2, None, vec![]),
            turn(A, 3, Some(id(A, 2)), vec![id(A, 2)]),
            turn(B, 3, Some(id(A, 2)), vec![id(A, 2)]),
        ],
    );
    let request = ContextRequest {
        expected_revision: store.revision().into(),
        conversation: id(A, 1),
        selected_head: None,
        max_turns: 1,
        max_text_bytes: 1024,
    };
    assert!(matches!(
        store.select_context(request.clone()),
        Err(DomainError::AmbiguousHeads { .. })
    ));
    let context = store
        .select_context(ContextRequest {
            selected_head: Some(id(A, 3)),
            ..request
        })
        .unwrap();
    assert_eq!(context.ancestor_ids, vec![id(A, 3)]);
    assert_eq!(store.history(&id(A, 1), &id(A, 3)).unwrap().len(), 2);
    assert_eq!(store.snapshot().records.len(), 4);
}

#[test]
fn delete_before_create_and_late_completion_never_resurrect_and_purge_text() {
    let store = import(&empty(), vec![delete()]);
    let plan = store
        .plan(Command::Import {
            expected_revision: store.revision().into(),
            records: vec![create(), turn(A, 2, None, vec![])],
        })
        .unwrap();
    assert_eq!(plan.snapshot.records.len(), 1);
    assert_eq!(plan.snapshot.retired.len(), 2);
    assert!(plan.purge_conversations.contains(&id(A, 1)));
    let restored = Store::restore(plan.snapshot).unwrap();
    assert!(restored.snapshot().conversations[0].deleted);
    assert!(matches!(
        restored.history(&id(A, 1), &id(A, 2)),
        Err(DomainError::Deleted { .. })
    ));
    let duplicate = restored
        .plan(Command::Import {
            expected_revision: restored.revision().into(),
            records: vec![turn(A, 2, None, vec![])],
        })
        .unwrap();
    assert_eq!(duplicate.duplicates, vec![id(A, 2)]);
    let mut conflict = turn(A, 2, None, vec![]);
    conflict.body_key = key(999);
    assert!(matches!(
        restored.plan(Command::Import {
            expected_revision: restored.revision().into(),
            records: vec![conflict]
        }),
        Err(DomainError::IdConflict { .. })
    ));
}

#[test]
fn revision_plans_do_not_mutate_and_clear_is_atomic() {
    let store = import(&empty(), vec![create(), turn(A, 2, None, vec![])]);
    let new = record(Operation {
        version: 1,
        id: id(B, 100),
        body: OperationBody::ConversationCreated {
            title: "Synthetic fresh".into(),
        },
    });
    let plan = store
        .plan(Command::Import {
            expected_revision: store.revision().into(),
            records: vec![delete(), new],
        })
        .unwrap();
    assert_eq!(store.snapshot().records.len(), 2);
    assert_eq!(plan.base_revision, store.revision());
    assert_eq!(plan.snapshot.revision, "2");
    assert_eq!(plan.publish.len(), 2);
    assert_eq!(plan.purge_conversations, vec![id(A, 1)]);
    let committed = Store::restore(plan.snapshot).unwrap();
    assert!(matches!(
        committed.plan(Command::Import {
            expected_revision: "1".into(),
            records: vec![]
        }),
        Err(DomainError::RevisionConflict { .. })
    ));
}

#[test]
fn serde_rejects_unknown_fields_and_decimal_identifiers_are_strings() {
    let json = serde_json::to_string(&create()).unwrap();
    assert!(json.contains("\"sequence\":\"1\""));
    assert_eq!(serde_json::from_str::<Record>(&json).unwrap(), create());
    let value = serde_json::json!({"replica": A, "sequence": 1});
    assert!(serde_json::from_value::<OperationId>(value).is_err());
    assert!(serde_json::from_value::<OperationId>(
        serde_json::json!({"replica": A, "sequence": "1", "unknown": true})
    )
    .is_err());
}

#[test]
fn wrong_conversation_and_disordered_context_are_rejected() {
    let other = record(Operation {
        version: 1,
        id: id(B, 1),
        body: OperationBody::ConversationCreated {
            title: "Synthetic other".into(),
        },
    });
    let mut other_root = turn(B, 2, None, vec![]);
    if let OperationBody::TurnCompleted { conversation, .. } = &mut other_root.operation.body {
        *conversation = id(B, 1);
    }
    let store = import(
        &empty(),
        vec![
            create(),
            other,
            other_root,
            turn(A, 2, None, vec![]),
            turn(A, 3, Some(id(A, 2)), vec![]),
        ],
    );
    assert!(matches!(
        store.plan(Command::Import {
            expected_revision: store.revision().into(),
            records: vec![turn(A, 4, Some(id(B, 2)), vec![])]
        }),
        Err(DomainError::InvalidReference { .. })
    ));
    assert!(matches!(
        store.plan(Command::Import {
            expected_revision: store.revision().into(),
            records: vec![turn(A, 4, Some(id(A, 3)), vec![id(A, 3), id(A, 2)])]
        }),
        Err(DomainError::InvalidContext { .. })
    ));
}

#[test]
fn context_selection_fast_forwards_only_without_displacing_local_work() {
    let store = import(
        &empty(),
        vec![
            create(),
            turn(A, 2, None, vec![]),
            turn(A, 3, Some(id(A, 2)), vec![]),
        ],
    );
    let request = ContinuationRequest {
        expected_revision: store.revision().into(),
        conversation: id(A, 1),
        selected_head: Some(id(A, 2)),
        local_work_in_flight: false,
    };
    assert_eq!(
        store.continuation(request.clone()).unwrap(),
        ContinuationDecision::FastForward { head: id(A, 3) }
    );
    assert_eq!(
        store
            .continuation(ContinuationRequest {
                local_work_in_flight: true,
                ..request.clone()
            })
            .unwrap(),
        ContinuationDecision::Keep {
            selected_head: Some(id(A, 2))
        }
    );
    let forked = import(&store, vec![turn(B, 3, Some(id(A, 2)), vec![])]);
    assert!(
        matches!(forked.continuation(ContinuationRequest { expected_revision: forked.revision().into(), ..request }), Ok(ContinuationDecision::Choose { heads }) if heads.len() == 2)
    );
}

#[test]
fn a_pinned_inflight_context_remains_valid_after_sync_and_stale_revision_does_not() {
    let store = import(&empty(), vec![create(), turn(A, 2, None, vec![])]);
    let selected = store
        .select_context(ContextRequest {
            expected_revision: store.revision().into(),
            conversation: id(A, 1),
            selected_head: None,
            max_turns: 20,
            max_text_bytes: 1024,
        })
        .unwrap();
    let synced = import(&store, vec![turn(B, 3, Some(id(A, 2)), vec![])]);
    assert!(matches!(
        synced.select_context(ContextRequest {
            expected_revision: selected.revision,
            conversation: id(A, 1),
            selected_head: Some(id(A, 2)),
            max_turns: 20,
            max_text_bytes: 1024
        }),
        Err(DomainError::RevisionConflict { .. })
    ));
    let saved = import(
        &synced,
        vec![turn(A, 3, selected.parent, selected.ancestor_ids)],
    );
    assert_eq!(saved.history(&id(A, 1), &id(A, 3)).unwrap().len(), 2);
    assert_eq!(saved.snapshot().conversations[0].heads.len(), 2);
}

#[test]
fn retired_parent_in_another_conversation_cannot_become_an_eternal_pending_record() {
    let deleted = import(&empty(), vec![create(), turn(A, 2, None, vec![]), delete()]);
    let other = record(Operation {
        version: 1,
        id: id(B, 1),
        body: OperationBody::ConversationCreated {
            title: "Synthetic other".into(),
        },
    });
    let mut bad = turn(B, 2, Some(id(A, 2)), vec![]);
    if let OperationBody::TurnCompleted { conversation, .. } = &mut bad.operation.body {
        *conversation = id(B, 1);
    }
    assert!(matches!(
        deleted.plan(Command::Import {
            expected_revision: deleted.revision().into(),
            records: vec![other, bad]
        }),
        Err(DomainError::InvalidReference { .. })
    ));
}

#[test]
fn bounds_invalid_versions_identifiers_and_restore_corruption_are_rejected() {
    let mut oversized = turn(A, 2, None, vec![]);
    if let OperationBody::TurnCompleted { prompt, .. } = &mut oversized.operation.body {
        *prompt = "x".repeat(MAX_PROMPT_BYTES + 1);
    }
    assert!(matches!(
        oversized.validate(),
        Err(DomainError::LimitExceeded { .. })
    ));
    let mut version = create();
    version.operation.version = 2;
    assert_eq!(
        version.validate(),
        Err(DomainError::UnsupportedVersion { version: 2 })
    );
    for sequence in ["0", "01", "-1", "18446744073709551616"] {
        assert!(OperationId {
            replica: A.into(),
            sequence: sequence.into()
        }
        .validate()
        .is_err());
    }
    assert!(OperationId {
        replica: A.into(),
        sequence: "18446744073709551615".into()
    }
    .validate()
    .is_ok());
    let mut snapshot = StoreSnapshot::empty();
    snapshot.retired.push(RetiredRecord {
        id: id(A, 2),
        conversation: id(A, 1),
        body_key: key(2),
    });
    assert!(matches!(
        Store::restore(snapshot),
        Err(DomainError::Invalid { .. })
    ));
    assert!(matches!(
        empty().plan(Command::Import {
            expected_revision: "0".into(),
            records: vec![create(); MAX_BATCH_RECORDS + 1]
        }),
        Err(DomainError::LimitExceeded { .. })
    ));
}

#[test]
fn every_small_import_order_converges_even_with_deletion_and_restarts() {
    let records = [
        create(),
        turn(A, 2, None, vec![]),
        turn(B, 3, Some(id(A, 2)), vec![id(A, 2)]),
        delete(),
    ];
    let expected = import(&empty(), records.to_vec()).export();
    for a in 0..4 {
        for b in 0..4 {
            for c in 0..4 {
                for d in 0..4 {
                    let order = [a, b, c, d];
                    if order
                        .into_iter()
                        .collect::<std::collections::BTreeSet<_>>()
                        .len()
                        != 4
                    {
                        continue;
                    }
                    let mut store = empty();
                    for i in order {
                        store = import(&store, vec![records[i].clone()]);
                        store = Store::restore(
                            serde_json::from_str(&serde_json::to_string(&store.export()).unwrap())
                                .unwrap(),
                        )
                        .unwrap();
                    }
                    let mut actual = store.export();
                    actual.revision = expected.revision.clone();
                    assert_eq!(actual, expected, "synthetic permutation {order:?}");
                }
            }
        }
    }
}

#[test]
fn canonical_operation_json_is_a_stable_body_key_input() {
    let json = serde_json::to_string(&create().operation).unwrap();
    assert_eq!(json, concat!("{\"version\":1,\"id\":{\"replica\":\"00000000-0000-4000-8000-000000000001\",\"sequence\":\"1\"},",
        "\"body\":{\"kind\":\"conversation_created\",\"title\":\"Synthetic chat\"}}"));
    let mut value = serde_json::to_value(turn(A, 2, None, vec![]).operation).unwrap();
    value["body"]["provenance"]["system_instruction"] =
        serde_json::json!("Synthetic untrusted instruction");
    assert!(serde_json::from_value::<Operation>(value).is_err());
    let refused = Record {
        body_key: key(5),
        operation: Operation {
            version: 1,
            id: id(A, 5),
            body: OperationBody::TurnCompleted {
                conversation: id(A, 1),
                parent: None,
                request: id(A, 1005),
                prompt: "Synthetic question".into(),
                answer: Answer::Refusal {
                    reason: "Synthetic refusal".into(),
                },
                terminal_stage: TerminalStage::Review,
                provenance: match turn(A, 2, None, vec![]).operation.body {
                    OperationBody::TurnCompleted { provenance, .. } => provenance,
                    _ => unreachable!(),
                },
            },
        },
    };
    let stored = import(&empty(), vec![create(), refused.clone()]);
    assert_eq!(stored.history(&id(A, 1), &id(A, 5)).unwrap(), vec![refused]);
}

#[test]
fn byte_limited_context_can_be_empty_without_erasing_history_or_parent() {
    let store = import(&empty(), vec![create(), turn(A, 2, None, vec![])]);
    let context = store
        .select_context(ContextRequest {
            expected_revision: store.revision().into(),
            conversation: id(A, 1),
            selected_head: None,
            max_turns: 20,
            max_text_bytes: 1,
        })
        .unwrap();
    assert!(context.turns.is_empty());
    assert_eq!(context.parent, Some(id(A, 2)));
    assert_eq!(store.history(&id(A, 1), &id(A, 2)).unwrap().len(), 1);
    let continued = import(
        &store,
        vec![turn(A, 3, context.parent, context.ancestor_ids)],
    );
    assert_eq!(continued.history(&id(A, 1), &id(A, 3)).unwrap().len(), 2);
}

#[test]
fn text_quota_is_checked_without_overflow_on_arm32() {
    assert!(check_text_quota([MAX_STORE_TEXT_BYTES].into_iter()).is_ok());
    assert!(matches!(
        check_text_quota([MAX_STORE_TEXT_BYTES, 1].into_iter()),
        Err(DomainError::LimitExceeded { .. })
    ));
    assert!(matches!(
        check_text_quota([usize::MAX, 1].into_iter()),
        Err(DomainError::LimitExceeded { .. })
    ));
}

#[test]
fn formerly_pending_invalid_children_do_not_block_authentic_dependencies() {
    let parent_id = id(B, 2);
    let mut other_parent = turn(B, 2, None, vec![]);
    if let OperationBody::TurnCompleted { conversation, .. } = &mut other_parent.operation.body {
        *conversation = id(B, 1);
    }
    let other_create = record(Operation {
        version: 1,
        id: id(B, 1),
        body: OperationBody::ConversationCreated {
            title: "Synthetic other".into(),
        },
    });
    let parent_types = [
        (
            record(Operation {
                version: 1,
                id: parent_id.clone(),
                body: OperationBody::ConversationCreated {
                    title: "Synthetic parent identity".into(),
                },
            }),
            vec![],
        ),
        (other_parent, vec![]),
        (turn(B, 2, None, vec![]), vec![id(A, 88), parent_id.clone()]),
    ];
    for (case, (parent, context)) in parent_types.into_iter().enumerate() {
        let baseline = import(
            &empty(),
            vec![create(), other_create.clone(), turn(A, 2, None, vec![])],
        );
        let child = turn(A, 3, Some(parent_id.clone()), context);
        let pending = import(&baseline, vec![child.clone()]);
        assert!(pending.snapshot().pending.contains(&id(A, 3)));
        let resolved = pending.plan(Command::Import {
            expected_revision: pending.revision().into(),
            records: vec![parent.clone()],
        });
        assert!(
            resolved.is_ok(),
            "authentic dependency must commit rather than be poisoned: {resolved:?}"
        );
        let plan = resolved.unwrap();
        assert_eq!(plan.quarantined, vec![id(A, 3)]);
        assert!(plan.purge_conversations.is_empty());
        let snapshot_json = serde_json::to_string(&plan.snapshot).unwrap();
        let restored = Store::restore(serde_json::from_str(&snapshot_json).unwrap()).unwrap();
        assert_eq!(
            restored.export().quarantine,
            vec![QuarantinedRecord {
                id: id(A, 3),
                reason: if case == 2 {
                    QuarantineReason::InvalidContext
                } else {
                    QuarantineReason::InvalidReference
                }
            }]
        );
        assert!(restored.export().records.contains(&parent));
        assert!(
            restored.export().records.contains(&child),
            "immutable invalid evidence must not be purged"
        );
        assert!(!restored.snapshot().pending.contains(&id(A, 3)));
        assert!(!restored
            .snapshot()
            .conversations
            .iter()
            .flat_map(|c| &c.heads)
            .any(|head| head == &id(A, 3)));
        assert!(restored.history(&id(A, 1), &id(A, 3)).is_err());
        assert!(!restored.missing_dependencies().contains(&parent_id));
        assert!(!restored.missing_dependencies().contains(&id(A, 88)));
        let page = crate::conversation_sync::inventory_page(
            &restored,
            crate::conversation_sync::InventoryRequest {
                cursor: crate::conversation_sync::InventoryCursor {
                    revision: restored.revision().into(),
                    after: None,
                },
                max_items: 128,
            },
        )
        .unwrap();
        assert!(!page.entries.iter().any(|e| e.id == id(A, 3)));
        let old_page = crate::conversation_sync::inventory_page(
            &pending,
            crate::conversation_sync::InventoryRequest {
                cursor: crate::conversation_sync::InventoryCursor {
                    revision: pending.revision().into(),
                    after: None,
                },
                max_items: 128,
            },
        )
        .unwrap();
        let reconciliation = crate::conversation_sync::reconcile_page(&restored, old_page).unwrap();
        assert!(reconciliation.fetch_records.is_empty());
        assert!(reconciliation.missing_ancestors.is_empty());
        let duplicate = restored
            .plan(Command::Import {
                expected_revision: restored.revision().into(),
                records: vec![child.clone()],
            })
            .unwrap();
        assert_eq!(duplicate.quarantined, vec![id(A, 3)]);
        assert_eq!(duplicate.duplicates, vec![id(A, 3)]);
        assert_eq!(duplicate.snapshot.revision, restored.revision());
        assert!(duplicate.publish.is_empty());
        assert_eq!(
            crate::conversation_sync::receipts_after_commit(&restored, B, &[id(A, 3)]).unwrap()[0]
                .body_key,
            child.body_key
        );
        assert_eq!(
            serde_json::to_vec(
                &restored
                    .export()
                    .records
                    .iter()
                    .find(|r| r.operation.id == id(A, 3))
                    .unwrap()
                    .operation
            )
            .unwrap(),
            serde_json::to_vec(&child.operation).unwrap(),
            "quarantine must not change the canonical operation hash input"
        );
        assert!(
            baseline
                .plan(Command::Import {
                    expected_revision: baseline.revision().into(),
                    records: vec![parent.clone(), child.clone()]
                })
                .is_err(),
            "same-batch known-invalid input must reject atomically"
        );
        let mut known_good = Store::restore(baseline.export()).unwrap();
        known_good = import(&known_good, vec![parent]);
        assert!(
            known_good
                .plan(Command::Import {
                    expected_revision: known_good.revision().into(),
                    records: vec![child]
                })
                .is_err(),
            "known-invalid new child must still reject"
        );
    }
}

#[test]
fn quarantine_propagates_to_old_pending_descendants_but_not_valid_history() {
    let baseline = import(&empty(), vec![create(), turn(A, 2, None, vec![])]);
    let child = turn(A, 4, Some(id(A, 3)), vec![id(B, 88), id(A, 3)]);
    let descendant = turn(A, 5, Some(id(A, 4)), vec![id(A, 4)]);
    let pending = import(&baseline, vec![child.clone(), descendant.clone()]);
    let plan = pending
        .plan(Command::Import {
            expected_revision: pending.revision().into(),
            records: vec![turn(A, 3, None, vec![])],
        })
        .unwrap();
    assert_eq!(plan.quarantined, vec![id(A, 4), id(A, 5)]);
    assert!(plan.purge_conversations.is_empty());
    let restored = Store::restore(plan.snapshot).unwrap();
    assert_eq!(
        restored.export().quarantine,
        vec![
            QuarantinedRecord {
                id: id(A, 4),
                reason: QuarantineReason::InvalidContext
            },
            QuarantinedRecord {
                id: id(A, 5),
                reason: QuarantineReason::InvalidReference
            },
        ]
    );
    let mut unordered = restored.export();
    unordered.quarantine.reverse();
    assert_eq!(
        serde_json::to_vec(&Store::restore(unordered).unwrap().export()).unwrap(),
        serde_json::to_vec(&restored.export()).unwrap()
    );
    let mut wrong_reason = restored.export();
    wrong_reason.quarantine[0].reason = QuarantineReason::InvalidReference;
    assert!(Store::restore(wrong_reason).is_err());
    assert!(restored.snapshot().pending.is_empty());
    assert!(restored.missing_dependencies().is_empty());
    assert_eq!(
        restored.history(&id(A, 1), &id(A, 2)).unwrap(),
        baseline.history(&id(A, 1), &id(A, 2)).unwrap()
    );
    assert_eq!(
        restored.history(&id(A, 1), &id(A, 5)),
        Err(DomainError::Quarantined { id: id(A, 5) })
    );
    assert!(restored
        .plan(Command::Import {
            expected_revision: restored.revision().into(),
            records: vec![turn(A, 6, Some(id(A, 5)), vec![id(A, 5)])]
        })
        .is_err());
    let mut conflicting = child;
    if let OperationBody::TurnCompleted { prompt, .. } = &mut conflicting.operation.body {
        *prompt = "Synthetic conflicting evidence".into();
    }
    assert!(matches!(
        restored.plan(Command::Import {
            expected_revision: restored.revision().into(),
            records: vec![conflicting]
        }),
        Err(DomainError::IdConflict { .. })
    ));
    let deleted = import(&restored, vec![delete()]);
    assert!(deleted.export().quarantine.is_empty());
    assert!(deleted.export().retired.iter().any(|r| r.id == id(A, 4)));
}

#[test]
fn quarantine_restore_is_bounded_validated_and_old_v1_snapshots_default_to_empty() {
    let mut old_json = serde_json::to_value(StoreSnapshot::empty()).unwrap();
    old_json.as_object_mut().unwrap().remove("quarantine");
    assert!(Store::restore(serde_json::from_value(old_json).unwrap())
        .unwrap()
        .export()
        .quarantine
        .is_empty());
    let valid = import(&empty(), vec![create(), turn(A, 2, None, vec![])]);
    let mut invalid_ledger = valid.export();
    invalid_ledger.quarantine.push(QuarantinedRecord {
        id: id(A, 2),
        reason: QuarantineReason::InvalidReference,
    });
    assert!(
        Store::restore(invalid_ledger).is_err(),
        "valid user history cannot be hidden by arbitrary quarantine"
    );
    let mut dangling = valid.export();
    dangling.quarantine.push(QuarantinedRecord {
        id: id(A, 999),
        reason: QuarantineReason::InvalidContext,
    });
    assert!(Store::restore(dangling).is_err());
    let mut oversized = valid.export();
    oversized.quarantine = vec![
        QuarantinedRecord {
            id: id(A, 2),
            reason: QuarantineReason::InvalidContext
        };
        MAX_QUARANTINED_RECORDS + 1
    ];
    assert!(matches!(
        Store::restore(oversized),
        Err(DomainError::LimitExceeded { .. })
    ));
}

#[test]
fn unknown_conversation_reference_cannot_poison_a_real_turn_of_that_identity() {
    let other_create = record(Operation {
        version: 1,
        id: id(B, 1),
        body: OperationBody::ConversationCreated {
            title: "Synthetic other".into(),
        },
    });
    let mut child = turn(A, 3, None, vec![]);
    if let OperationBody::TurnCompleted { conversation, .. } = &mut child.operation.body {
        *conversation = id(B, 2);
    }
    let baseline = import(&empty(), vec![create(), other_create]);
    let pending = import(&baseline, vec![child]);
    let mut authentic = turn(B, 2, None, vec![]);
    if let OperationBody::TurnCompleted { conversation, .. } = &mut authentic.operation.body {
        *conversation = id(B, 1);
    }
    let restored = import(&pending, vec![authentic.clone()]);
    assert_eq!(
        restored.history(&id(B, 1), &id(B, 2)).unwrap(),
        vec![authentic]
    );
    assert_eq!(
        restored.export().quarantine,
        vec![QuarantinedRecord {
            id: id(A, 3),
            reason: QuarantineReason::InvalidReference
        }]
    );
}

#[test]
fn invalid_unknown_tombstone_is_dispositioned_without_retiring_the_authentic_turn() {
    let other = record(Operation {
        version: 1,
        id: id(B, 1),
        body: OperationBody::ConversationCreated {
            title: "Synthetic real conversation".into(),
        },
    });
    let pending = import(&empty(), vec![other, turn(A, 2, None, vec![]), delete()]);
    assert!(pending.export().retired.iter().any(|r| r.id == id(A, 2)));
    let mut authentic = turn(A, 1, None, vec![]);
    if let OperationBody::TurnCompleted { conversation, .. } = &mut authentic.operation.body {
        *conversation = id(B, 1);
    }
    let plan = pending
        .plan(Command::Import {
            expected_revision: pending.revision().into(),
            records: vec![authentic.clone()],
        })
        .unwrap();
    assert_eq!(plan.quarantined, vec![id(B, 99)]);
    assert!(plan.purge_conversations.is_empty());
    let restored = Store::restore(plan.snapshot).unwrap();
    assert_eq!(
        restored.history(&id(B, 1), &id(A, 1)).unwrap(),
        vec![authentic]
    );
    assert!(restored.export().retired.iter().any(|r| r.id == id(A, 2)));
    assert!(restored.snapshot().conversations.iter().all(|c| !c.deleted));
}

#[test]
fn a_quarantined_only_continuation_cannot_silently_become_empty_provider_context() {
    let pending = import(&empty(), vec![create(), turn(A, 3, Some(id(B, 2)), vec![])]);
    let authentic = record(Operation {
        version: 1,
        id: id(B, 2),
        body: OperationBody::ConversationCreated {
            title: "Synthetic unrelated create".into(),
        },
    });
    let resolved = import(&pending, vec![authentic]);
    assert_eq!(
        resolved.select_context(ContextRequest {
            expected_revision: resolved.revision().into(),
            conversation: id(A, 1),
            selected_head: None,
            max_turns: 20,
            max_text_bytes: 1024
        }),
        Err(DomainError::Quarantined { id: id(A, 3) })
    );
}

#[test]
fn exact_two_conversation_quarantine_survives_deletion_of_its_context_witness() {
    let other_create = record(Operation {
        version: 1,
        id: id(B, 1),
        body: OperationBody::ConversationCreated {
            title: "Synthetic D".into(),
        },
    });
    let q = turn(A, 3, Some(id(A, 2)), vec![id(A, 88), id(A, 2)]);
    let mut t = turn(B, 3, Some(id(B, 2)), vec![id(A, 3), id(B, 2)]);
    if let OperationBody::TurnCompleted { conversation, .. } = &mut t.operation.body {
        *conversation = id(B, 1);
    }
    // Exact persisted state after authentic A2 resolved Q: B2 and A88 are still absent.
    let before_delete = Store::restore(StoreSnapshot {
        version: 1,
        revision: "3".into(),
        records: vec![
            create(),
            other_create,
            q.clone(),
            t.clone(),
            turn(A, 2, None, vec![]),
        ],
        retired: vec![],
        quarantine: vec![
            QuarantinedRecord {
                id: id(A, 3),
                reason: QuarantineReason::InvalidContext,
            },
            QuarantinedRecord {
                id: id(B, 3),
                reason: QuarantineReason::InvalidContext,
            },
        ],
    })
    .unwrap();
    let plan = before_delete
        .plan(Command::Import {
            expected_revision: before_delete.revision().into(),
            records: vec![delete()],
        })
        .unwrap();
    assert!(plan
        .snapshot
        .retired
        .iter()
        .any(|r| r.id == id(A, 3) && r.body_key == q.body_key));
    assert!(plan.snapshot.records.contains(&t));
    assert_eq!(
        plan.snapshot.quarantine,
        vec![QuarantinedRecord {
            id: id(B, 3),
            reason: QuarantineReason::InvalidContext
        }]
    );
    let plan_json = serde_json::to_vec(&plan).unwrap();
    let restored_plan: MutationPlan = serde_json::from_slice(&plan_json).unwrap();
    let restored = Store::restore(restored_plan.snapshot);
    assert!(
        restored.is_ok(),
        "deleting C must not invalidate D's quarantine justification: {restored:?}"
    );
    let restored = restored.unwrap();
    assert_eq!(
        restored.history(&id(B, 1), &id(B, 3)),
        Err(DomainError::Quarantined { id: id(B, 3) })
    );
    assert!(restored.missing_dependencies().is_empty());
}

#[test]
fn known_context_conversation_and_type_are_validated_even_with_a_missing_parent() {
    let mut known_turn = turn(A, 3, None, vec![]);
    let other_create = record(Operation {
        version: 1,
        id: id(B, 1),
        body: OperationBody::ConversationCreated {
            title: "Synthetic D".into(),
        },
    });
    let store = import(&empty(), vec![create(), other_create, known_turn.clone()]);
    let mut incoming = turn(B, 3, Some(id(B, 2)), vec![id(A, 3), id(B, 2)]);
    if let OperationBody::TurnCompleted { conversation, .. } = &mut incoming.operation.body {
        *conversation = id(B, 1);
    }
    assert!(
        matches!(
            store.plan(Command::Import {
                expected_revision: store.revision().into(),
                records: vec![incoming.clone()]
            }),
            Err(DomainError::InvalidContext { .. })
        ),
        "known cross-conversation context must reject before its parent arrives"
    );
    let retired = import(&store, vec![delete()]);
    assert!(
        matches!(
            retired.plan(Command::Import {
                expected_revision: retired.revision().into(),
                records: vec![incoming]
            }),
            Err(DomainError::InvalidContext { .. })
        ),
        "retired conversation metadata must enforce the same rejection"
    );
    if let OperationBody::TurnCompleted {
        parent, provenance, ..
    } = &mut known_turn.operation.body
    {
        *parent = Some(id(A, 77));
        provenance.context = vec![id(A, 1), id(A, 77)];
    }
    known_turn.operation.id = id(A, 4);
    assert!(
        matches!(
            store.plan(Command::Import {
                expected_revision: store.revision().into(),
                records: vec![known_turn]
            }),
            Err(DomainError::InvalidContext { .. })
        ),
        "a known create is not a context turn"
    );
}

#[test]
fn two_conversation_witness_deletion_is_restorable_at_every_public_plan_boundary() {
    let other_create = record(Operation {
        version: 1,
        id: id(B, 1),
        body: OperationBody::ConversationCreated {
            title: "Synthetic D".into(),
        },
    });
    let baseline = import(&empty(), vec![create(), other_create]);
    let q = turn(A, 3, Some(id(A, 2)), vec![id(A, 88), id(A, 2)]);
    let mut t = turn(B, 3, Some(id(B, 2)), vec![id(A, 3), id(B, 2)]);
    if let OperationBody::TurnCompleted { conversation, .. } = &mut t.operation.body {
        *conversation = id(B, 1);
    }
    let q_first = import(&baseline, vec![q.clone()]);
    assert!(
        matches!(
            q_first.plan(Command::Import {
                expected_revision: q_first.revision().into(),
                records: vec![t.clone()]
            }),
            Err(DomainError::InvalidContext { .. })
        ),
        "a known pending Q already proves the new T's cross-conversation context invalid"
    );
    let t_first = import(&baseline, vec![t.clone()]);
    let q_arrived = import(&t_first, vec![q.clone()]);
    assert_eq!(
        q_arrived.export().quarantine,
        vec![QuarantinedRecord {
            id: id(B, 3),
            reason: QuarantineReason::InvalidContext
        }]
    );
    let parent_arrived = import(&q_arrived, vec![turn(A, 2, None, vec![])]);
    assert_eq!(parent_arrived.export().quarantine.len(), 2);
    let deleted_c = import(&parent_arrived, vec![delete()]);
    assert_eq!(
        deleted_c.export().quarantine,
        vec![QuarantinedRecord {
            id: id(B, 3),
            reason: QuarantineReason::InvalidContext
        }]
    );
    assert!(deleted_c
        .export()
        .retired
        .iter()
        .any(|r| r.id == id(A, 3) && r.body_key == q.body_key));
    assert!(deleted_c.export().records.contains(&t));
    let mut b2 = turn(B, 2, None, vec![]);
    if let OperationBody::TurnCompleted { conversation, .. } = &mut b2.operation.body {
        *conversation = id(B, 1);
    }
    let late_parent = import(&deleted_c, vec![b2]);
    assert_eq!(
        late_parent.export().quarantine,
        deleted_c.export().quarantine
    );
    assert_eq!(
        late_parent
            .snapshot()
            .conversations
            .iter()
            .find(|c| c.id == id(B, 1))
            .unwrap()
            .heads,
        vec![id(B, 2)]
    );
    assert_eq!(
        late_parent.history(&id(B, 1), &id(B, 3)),
        Err(DomainError::Quarantined { id: id(B, 3) })
    );
    assert!(late_parent.missing_dependencies().is_empty());
}

#[test]
fn retired_duplicate_completion_is_suppressed_without_republication_or_revision_change() {
    let completion = turn(A, 2, None, vec![]);
    let completed = import(&empty(), vec![create(), completion.clone()]);
    let deleted = import(&completed, vec![delete()]);
    let plan = deleted
        .plan(Command::Import {
            expected_revision: deleted.revision().into(),
            records: vec![completion],
        })
        .unwrap();
    assert_eq!(plan.duplicates, vec![id(A, 2)]);
    assert_eq!(plan.suppressed, vec![id(A, 2)]);
    assert!(plan.accepted.is_empty());
    assert!(plan.publish.is_empty());
    assert_eq!(plan.snapshot.revision, deleted.revision());
    assert_eq!(
        crate::conversation_sync::receipts_after_commit(
            &Store::restore(plan.snapshot).unwrap(),
            B,
            &[id(A, 2)]
        )
        .unwrap()[0]
            .body_key,
        key(2)
    );
}
