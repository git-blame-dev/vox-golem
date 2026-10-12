use voxgolem_core::conversation_sync::{
    inventory_page, receipts_after_commit, reconcile_page, InventoryCursor, InventoryPage,
    InventoryRequest, Receipt,
};
use voxgolem_core::durable_conversation::{
    Command, ContextRequest, ContextSelection, MutationPlan, Operation, Record, Store,
    StoreSnapshot,
};

// Synthetic keys represent keys independently computed by the trusted native adapter.
const CREATE_JSON: &str = r#"{"version":1,"id":{"replica":"00000000-0000-4000-8000-000000000001","sequence":"1"},"body":{"kind":"conversation_created","title":"Synthetic bridge chat"}}"#;
const TURN_JSON: &str = r#"{"version":1,"id":{"replica":"00000000-0000-4000-8000-000000000001","sequence":"3"},"body":{"kind":"turn_completed","conversation":{"replica":"00000000-0000-4000-8000-000000000001","sequence":"1"},"parent":null,"request":{"replica":"00000000-0000-4000-8000-000000000001","sequence":"2"},"prompt":"Synthetic bridge question","answer":{"kind":"text","text":"Synthetic bridge answer"},"terminal_stage":"instant","provenance":{"requested_model":"synthetic-model","returned_model":null,"policy_id":"synthetic-policy","policy_version":"1","instruction_profile_id":"synthetic-profile","instruction_profile_version":"1","context":[]}}}"#;

fn normalized_record(json: &str, n: u64) -> Record {
    let operation: Operation = serde_json::from_str(json).unwrap();
    assert_eq!(serde_json::to_string(&operation).unwrap(), json);
    Record {
        body_key: format!("{n:064x}"),
        operation,
    }
}

#[test]
fn public_json_command_plan_restore_context_and_peer_receipt_roundtrip() {
    let sender = Store::restore(StoreSnapshot::empty()).unwrap();
    let records = vec![
        normalized_record(CREATE_JSON, 1),
        normalized_record(TURN_JSON, 3),
    ];
    let command_json = serde_json::to_string(&Command::Import {
        expected_revision: "0".into(),
        records,
    })
    .unwrap();
    let command: Command = serde_json::from_str(&command_json).unwrap();
    let plan_json = serde_json::to_string(&sender.plan(command).unwrap()).unwrap();
    let plan: MutationPlan = serde_json::from_str(&plan_json).unwrap();
    assert_eq!(plan.base_revision, "0");
    assert_eq!(sender.revision(), "0");
    let committed_snapshot_json = serde_json::to_string(&plan.snapshot).unwrap();
    let sender = Store::restore(serde_json::from_str(&committed_snapshot_json).unwrap()).unwrap();
    let page = inventory_page(
        &sender,
        InventoryRequest {
            cursor: InventoryCursor {
                revision: "1".into(),
                after: None,
            },
            max_items: 128,
        },
    )
    .unwrap();
    let page_json = serde_json::to_string(&page).unwrap();
    let received_page: InventoryPage = serde_json::from_str(&page_json).unwrap();
    let receiver = Store::restore(StoreSnapshot::empty()).unwrap();
    let requests = reconcile_page(&receiver, received_page).unwrap();
    assert_eq!(requests.fetch_records.len(), 2);
    let received_records: Vec<Record> =
        serde_json::from_str(&serde_json::to_string(&plan.publish).unwrap()).unwrap();
    let imported = receiver
        .plan(Command::Import {
            expected_revision: requests.expected_revision,
            records: received_records,
        })
        .unwrap();
    let receiver = Store::restore(imported.snapshot).unwrap();
    let ids: Vec<_> = page.entries.iter().map(|entry| entry.id.clone()).collect();
    let receipts: Vec<Receipt> = serde_json::from_str(
        &serde_json::to_string(
            &receipts_after_commit(&receiver, "00000000-0000-4000-8000-000000000002", &ids)
                .unwrap(),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(receipts.len(), 2);
    let context: ContextSelection = serde_json::from_str(
        &serde_json::to_string(
            &receiver
                .select_context(ContextRequest {
                    expected_revision: "1".into(),
                    conversation: ids[0].clone(),
                    selected_head: None,
                    max_turns: 20,
                    max_text_bytes: 256 * 1024,
                })
                .unwrap(),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(context.parent, Some(ids[1].clone()));
    assert_eq!(context.ancestor_ids, vec![ids[1].clone()]);
    assert_eq!(
        context.turns[0].operation,
        serde_json::from_str::<Operation>(TURN_JSON).unwrap()
    );
}

#[test]
fn public_json_quarantine_roundtrip_retains_body_and_normalizes_the_ledger() {
    let create = normalized_record(CREATE_JSON, 1);
    let parent = normalized_record(TURN_JSON, 3);
    let mut child = parent.clone();
    child.operation.id.sequence = "4".into();
    child.body_key = format!("{:064x}", 4);
    let missing_ancestor = voxgolem_core::durable_conversation::OperationId {
        replica: parent.operation.id.replica.clone(),
        sequence: "777".into(),
    };
    if let voxgolem_core::durable_conversation::OperationBody::TurnCompleted {
        parent: pinned_parent,
        provenance,
        ..
    } = &mut child.operation.body
    {
        *pinned_parent = Some(parent.operation.id.clone());
        provenance.context = vec![missing_ancestor, parent.operation.id.clone()];
    }
    let canonical_child = serde_json::to_vec(&child.operation).unwrap();
    let empty = Store::restore(StoreSnapshot::empty()).unwrap();
    let pending = Store::restore(
        empty
            .plan(Command::Import {
                expected_revision: "0".into(),
                records: vec![create.clone(), child.clone()],
            })
            .unwrap()
            .snapshot,
    )
    .unwrap();
    let plan_json = serde_json::to_string(
        &pending
            .plan(Command::Import {
                expected_revision: "1".into(),
                records: vec![parent],
            })
            .unwrap(),
    )
    .unwrap();
    let plan: MutationPlan = serde_json::from_str(&plan_json).unwrap();
    assert_eq!(plan.quarantined, vec![child.operation.id.clone()]);
    let canonical_snapshot = serde_json::to_vec(&plan.snapshot).unwrap();
    let restored = Store::restore(serde_json::from_slice(&canonical_snapshot).unwrap()).unwrap();
    assert_eq!(
        serde_json::to_vec(&restored.export()).unwrap(),
        canonical_snapshot
    );
    assert_eq!(
        serde_json::to_value(restored.export()).unwrap()["quarantine"],
        serde_json::json!([{"id": child.operation.id, "reason": "invalid_context"}])
    );
    let retained = restored
        .export()
        .records
        .into_iter()
        .find(|r| r.operation.id == child.operation.id)
        .unwrap();
    assert_eq!(
        serde_json::to_vec(&retained.operation).unwrap(),
        canonical_child
    );
    assert_eq!(retained.body_key, child.body_key);
    let error = restored
        .history(&create.operation.id, &child.operation.id)
        .unwrap_err();
    assert_eq!(
        serde_json::to_value(error).unwrap(),
        serde_json::json!({"kind": "quarantined", "id": child.operation.id})
    );
}
