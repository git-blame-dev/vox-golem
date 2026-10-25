//! Exact inventory and per-peer durable receipts; transient connected-session catch-up
//! and transport IO are native-owned.
//!
//! Inventory advertises retained bodies only, never purged content. Walk revision-pinned
//! pages to completion; restart if the sender revision changes. A missing ID in one page
//! is not evidence that a peer lacks it. Apply fetched tombstones before requesting bodies,
//! then restore/reconcile again. Validate fetched records against their advertised body
//! keys in the trusted bridge, independently recomputing keys as documented by the domain.
//! `reconcile_page` does not infer convergence from one page or from a sequence watermark.
//! `missing_ancestors` is bounded work, not an instruction to execute a provider request.
//! Connected-session transfer batches must account for every requested record via validated
//! descriptors or unavailable IDs, respecting phase, byte, and record count limits.
//! Generate receipts from the committed revision only. Store receipts and retry/outbox
//! changes transactionally with a revision check; publishing or GMS success is not an ACK.
//! Reciprocal receipt confirmation requires exact matching operation and body-key sets
//! before retiring per-peer delivery work.
//! Receipt `recipient_replica` names the installation that persisted data, not a node route.
//! Unknown/conflicting receipts are rejected rather than suppressing delivery.
//! Quarantined bodies remain durable evidence and exact receipt targets, but are omitted
//! from outgoing inventory. Native body-serving/outbox code must also honor the quarantine
//! ledger, including requests against a manifest published before disposition.

use crate::durable_conversation::{
    decimal, validate_body_key, validate_replica, DomainError, OperationBody, OperationId, Store,
    SCHEMA_VERSION,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub const MAX_CONTROL_ITEMS: usize = 128;
pub const MAX_CONTROL_BYTES: usize = 32 * 1024;
pub const MAX_HINT_BYTES: usize = 256;
pub const MAX_CHUNK_BYTES: usize = 64 * 1024;
pub const MAX_BATCH_RECORDS: usize = 8;
pub const MAX_RECORD_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_BATCH_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_SESSION_PAGES: usize = 4096;
pub const MAX_RECEIPTS: usize = 65_536;
// IDs/keys are validated ASCII, so 512 covers each encoded entry including field names.
const ENTRY_BYTES_UPPER_BOUND: usize = 512;
const PAGE_BYTES_OVERHEAD: usize = 512;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InventoryEntry {
    pub id: OperationId,
    pub conversation: OperationId,
    pub body_key: String,
    pub tombstone: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InventoryCursor {
    pub revision: String,
    pub after: Option<OperationId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InventoryRequest {
    pub cursor: InventoryCursor,
    pub max_items: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InventoryPage {
    pub version: u32,
    pub revision: String,
    pub entries: Vec<InventoryEntry>,
    pub next: Option<InventoryCursor>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReconcilePlan {
    pub expected_revision: String,
    pub fetch_tombstones: Vec<InventoryEntry>,
    pub fetch_records: Vec<InventoryEntry>,
    /// Return local tombstones to a peer still advertising deleted content.
    pub offer_tombstones: Vec<InventoryEntry>,
    pub missing_ancestors: Vec<OperationId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Receipt {
    pub version: u32,
    /// Installation replica that durably persisted this exact operation/body.
    pub recipient_replica: String,
    pub operation: OperationId,
    pub body_key: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SyncPhase {
    Tombstones,
    Records,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequestedRecord {
    pub id: OperationId,
    pub expected: Option<InventoryEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TransferDescriptor {
    pub entry: InventoryEntry,
    pub bytes: usize,
}

pub fn validate_transfer_batch(
    requested: &[RequestedRecord],
    phase: SyncPhase,
    descriptors: &[TransferDescriptor],
    unavailable: &[OperationId],
) -> Result<(), DomainError> {
    if requested.is_empty() || requested.len() > MAX_BATCH_RECORDS {
        return Err(DomainError::LimitExceeded {
            limit: "transfer_batch_records".into(),
        });
    }
    let mut requested_by_id = BTreeMap::new();
    for req in requested {
        req.id.validate()?;
        if let Some(expected) = &req.expected {
            validate_entry(expected)?;
            if expected.id != req.id {
                return Err(DomainError::Invalid {
                    field: "requested_record_id".into(),
                });
            }
        }
        if requested_by_id
            .insert(&req.id, req.expected.as_ref())
            .is_some()
        {
            return Err(DomainError::Invalid {
                field: "requested_record_duplicate".into(),
            });
        }
    }
    if descriptors.len() > MAX_BATCH_RECORDS {
        return Err(DomainError::LimitExceeded {
            limit: "transfer_batch_records".into(),
        });
    }
    let mut answered = BTreeSet::new();
    let mut total_bytes = 0usize;
    for descriptor in descriptors {
        validate_entry(&descriptor.entry)?;
        if !(2..=MAX_RECORD_BYTES).contains(&descriptor.bytes) {
            return Err(DomainError::LimitExceeded {
                limit: "transfer_record_bytes".into(),
            });
        }
        total_bytes = total_bytes.checked_add(descriptor.bytes).ok_or_else(|| {
            DomainError::LimitExceeded {
                limit: "transfer_batch_bytes".into(),
            }
        })?;
        if total_bytes > MAX_BATCH_BYTES {
            return Err(DomainError::LimitExceeded {
                limit: "transfer_batch_bytes".into(),
            });
        }
        if phase == SyncPhase::Tombstones && !descriptor.entry.tombstone {
            return Err(DomainError::Invalid {
                field: "transfer_phase_tombstone".into(),
            });
        }
        let expected =
            requested_by_id
                .get(&descriptor.entry.id)
                .ok_or_else(|| DomainError::Invalid {
                    field: "transfer_unrequested_id".into(),
                })?;
        if !answered.insert(&descriptor.entry.id) {
            return Err(DomainError::Invalid {
                field: "transfer_duplicate_id".into(),
            });
        }
        if let Some(exp) = expected {
            if &descriptor.entry != *exp {
                return Err(DomainError::Invalid {
                    field: "transfer_entry_mismatch".into(),
                });
            }
        }
    }
    for id in unavailable {
        id.validate()?;
        if !requested_by_id.contains_key(id) {
            return Err(DomainError::Invalid {
                field: "transfer_unrequested_id".into(),
            });
        }
        if !answered.insert(id) {
            return Err(DomainError::Invalid {
                field: "transfer_duplicate_id".into(),
            });
        }
    }
    if descriptors.len() + unavailable.len() != requested.len() {
        return Err(DomainError::Invalid {
            field: "transfer_unanswered_request".into(),
        });
    }
    Ok(())
}

pub fn validate_confirmed_receipts(
    peer_replica: &str,
    expected: &[Receipt],
    confirmed: &[Receipt],
) -> Result<(), DomainError> {
    validate_replica(peer_replica)?;
    if expected.is_empty() || expected.len() > MAX_CONTROL_ITEMS {
        return Err(DomainError::LimitExceeded {
            limit: "confirmed_receipt_items".into(),
        });
    }
    if confirmed.len() != expected.len() {
        return Err(DomainError::Invalid {
            field: "confirmed_receipt_count".into(),
        });
    }
    let mut expected_by_op = BTreeMap::new();
    for receipt in expected {
        if receipt.version != SCHEMA_VERSION {
            return Err(DomainError::UnsupportedVersion {
                version: receipt.version,
            });
        }
        validate_replica(&receipt.recipient_replica)?;
        receipt.operation.validate()?;
        validate_body_key(&receipt.body_key)?;
        if expected_by_op
            .insert(&receipt.operation, &receipt.body_key)
            .is_some()
        {
            return Err(DomainError::Invalid {
                field: "expected_receipt_duplicate".into(),
            });
        }
    }
    let mut confirmed_ops = BTreeSet::new();
    for receipt in confirmed {
        if receipt.version != SCHEMA_VERSION {
            return Err(DomainError::UnsupportedVersion {
                version: receipt.version,
            });
        }
        if receipt.recipient_replica != peer_replica {
            return Err(DomainError::Invalid {
                field: "receipt_peer".into(),
            });
        }
        receipt.operation.validate()?;
        validate_body_key(&receipt.body_key)?;
        if !confirmed_ops.insert(&receipt.operation) {
            return Err(DomainError::Invalid {
                field: "confirmed_receipt_duplicate".into(),
            });
        }
        let expected_key =
            expected_by_op
                .get(&receipt.operation)
                .ok_or_else(|| DomainError::Missing {
                    id: receipt.operation.clone(),
                })?;
        if *expected_key != &receipt.body_key {
            return Err(DomainError::IdConflict {
                id: receipt.operation.clone(),
            });
        }
    }
    Ok(())
}

fn inventory(store: &Store) -> BTreeMap<OperationId, InventoryEntry> {
    inventory_entries(store)
        .map(|entry| (entry.id.clone(), entry))
        .collect()
}

fn inventory_entries(store: &Store) -> impl Iterator<Item = InventoryEntry> + '_ {
    store
        .retained_records()
        .filter(|r| !store.is_quarantined(&r.operation.id))
        .map(inventory_entry)
}

fn inventory_entry(r: &crate::durable_conversation::Record) -> InventoryEntry {
    InventoryEntry {
        id: r.operation.id.clone(),
        conversation: r.operation.conversation().clone(),
        body_key: r.body_key.clone(),
        tombstone: matches!(r.operation.body, OperationBody::ConversationDeleted { .. }),
    }
}

pub fn inventory_page(
    store: &Store,
    request: InventoryRequest,
) -> Result<InventoryPage, DomainError> {
    decimal(&request.cursor.revision, true)?;
    if request.cursor.revision != store.revision() {
        return Err(DomainError::RevisionConflict {
            expected: request.cursor.revision,
            actual: store.revision().into(),
        });
    }
    if request.max_items == 0 || request.max_items > MAX_CONTROL_ITEMS {
        return Err(DomainError::LimitExceeded {
            limit: "inventory_items".into(),
        });
    }
    if let Some(after) = &request.cursor.after {
        after.validate()?;
    }
    let max = request
        .max_items
        .min((MAX_CONTROL_BYTES - PAGE_BYTES_OVERHEAD) / ENTRY_BYTES_UPPER_BOUND);
    let mut remaining = store
        .retained_records()
        .filter(|record| {
            !store.is_quarantined(&record.operation.id)
                && request
                    .cursor
                    .after
                    .as_ref()
                    .is_none_or(|after| &record.operation.id > after)
        })
        .map(inventory_entry)
        .peekable();
    let entries: Vec<_> = remaining.by_ref().take(max).collect();
    let next = if remaining.peek().is_some() {
        Some(InventoryCursor {
            revision: store.revision().into(),
            after: entries.last().map(|e| e.id.clone()),
        })
    } else {
        None
    };
    Ok(InventoryPage {
        version: SCHEMA_VERSION,
        revision: store.revision().into(),
        entries,
        next,
    })
}

pub fn reconcile_page(store: &Store, page: InventoryPage) -> Result<ReconcilePlan, DomainError> {
    validate_page(&page)?;
    let local = inventory(store);
    let keys = body_keys(store);
    let retired: BTreeMap<_, _> = store.retired_records().map(|r| (r.id.clone(), r)).collect();
    let locally_deleted: BTreeSet<_> = local
        .values()
        .filter(|e| e.tombstone)
        .map(|e| e.conversation.clone())
        .collect();
    let remotely_deleted: BTreeSet<_> = page
        .entries
        .iter()
        .filter(|e| e.tombstone)
        .map(|e| e.conversation.clone())
        .collect();
    let mut fetch_tombstones = vec![];
    let mut fetch_records = vec![];
    let mut offer_conversations = BTreeSet::new();
    for entry in page.entries {
        if let Some(key) = keys.get(&entry.id) {
            if key != &entry.body_key {
                return Err(DomainError::IdConflict { id: entry.id });
            }
            if store.retained_record(&entry.id).is_some_and(|record| {
                record.operation.conversation() != &entry.conversation
                    || matches!(
                        record.operation.body,
                        OperationBody::ConversationDeleted { .. }
                    ) != entry.tombstone
            }) {
                return Err(DomainError::Invalid {
                    field: "inventory_metadata".into(),
                });
            }
            if retired
                .get(&entry.id)
                .is_some_and(|known| known.conversation != entry.conversation || entry.tombstone)
            {
                return Err(DomainError::Invalid {
                    field: "inventory_metadata".into(),
                });
            }
        }
        if !entry.tombstone && locally_deleted.contains(&entry.conversation) {
            offer_conversations.insert(entry.conversation.clone());
            continue;
        }
        if keys.contains_key(&entry.id) {
            continue;
        }
        if entry.tombstone {
            fetch_tombstones.push(entry);
        } else if !remotely_deleted.contains(&entry.conversation) {
            fetch_records.push(entry);
        }
    }
    let offer_tombstones = local
        .into_values()
        .filter(|e| e.tombstone && offer_conversations.contains(&e.conversation))
        .take(MAX_CONTROL_ITEMS)
        .collect();
    Ok(ReconcilePlan {
        expected_revision: store.revision().into(),
        fetch_tombstones,
        fetch_records,
        offer_tombstones,
        missing_ancestors: store
            .missing_dependencies()
            .into_iter()
            .take(MAX_CONTROL_ITEMS)
            .collect(),
    })
}

pub fn receipts_after_commit(
    store: &Store,
    recipient_replica: &str,
    ids: &[OperationId],
) -> Result<Vec<Receipt>, DomainError> {
    validate_replica(recipient_replica)?;
    if ids.len() > MAX_CONTROL_ITEMS {
        return Err(DomainError::LimitExceeded {
            limit: "receipt_items".into(),
        });
    }
    let keys = body_keys(store);
    ids.iter()
        .map(|id| {
            id.validate()?;
            let key = keys
                .get(id)
                .ok_or_else(|| DomainError::Missing { id: id.clone() })?;
            Ok(Receipt {
                version: SCHEMA_VERSION,
                recipient_replica: recipient_replica.into(),
                operation: id.clone(),
                body_key: key.clone(),
            })
        })
        .collect()
}

/// Exact known evidence for a validated inventory page, including retired/quarantined
/// records. Build the key index once, not once per offered ID.
pub fn known_receipts(
    store: &Store,
    recipient_replica: &str,
    page: &InventoryPage,
) -> Result<Vec<Receipt>, DomainError> {
    validate_replica(recipient_replica)?;
    validate_page(page)?;
    let keys = body_keys(store);
    Ok(page
        .entries
        .iter()
        .filter_map(|entry| {
            keys.get(&entry.id)
                .filter(|key| *key == &entry.body_key)
                .map(|key| Receipt {
                    version: SCHEMA_VERSION,
                    recipient_replica: recipient_replica.into(),
                    operation: entry.id.clone(),
                    body_key: key.clone(),
                })
        })
        .collect())
}

/// Native transaction owns persisting the returned ledger and revision, not a second cache.
pub fn merge_receipts(
    store: &Store,
    peer_replica: &str,
    existing: Vec<Receipt>,
    incoming: Vec<Receipt>,
) -> Result<Vec<Receipt>, DomainError> {
    validate_replica(peer_replica)?;
    if existing.len() > MAX_RECEIPTS || incoming.len() > MAX_CONTROL_ITEMS {
        return Err(DomainError::LimitExceeded {
            limit: "receipts".into(),
        });
    }
    let keys = body_keys(store);
    let mut ledger = BTreeMap::new();
    for receipt in existing {
        validate_receipt(&receipt, &keys)?;
        ledger.insert(
            (receipt.recipient_replica.clone(), receipt.operation.clone()),
            receipt,
        );
    }
    for receipt in incoming {
        if receipt.recipient_replica != peer_replica {
            return Err(DomainError::Invalid {
                field: "receipt_peer".into(),
            });
        }
        validate_receipt(&receipt, &keys)?;
        ledger.insert(
            (receipt.recipient_replica.clone(), receipt.operation.clone()),
            receipt,
        );
    }
    if ledger.len() > MAX_RECEIPTS {
        return Err(DomainError::LimitExceeded {
            limit: "receipts".into(),
        });
    }
    Ok(ledger.into_values().collect())
}

pub fn unacknowledged(
    store: &Store,
    peer_replica: &str,
    receipts: &[Receipt],
) -> Result<Vec<InventoryEntry>, DomainError> {
    validate_replica(peer_replica)?;
    if receipts.len() > MAX_RECEIPTS {
        return Err(DomainError::LimitExceeded {
            limit: "receipts".into(),
        });
    }
    let keys = body_keys(store);
    let mut acknowledged = BTreeSet::new();
    for receipt in receipts {
        validate_receipt(receipt, &keys)?;
        if receipt.recipient_replica == peer_replica {
            acknowledged.insert(receipt.operation.clone());
        }
    }
    Ok(inventory(store)
        .into_values()
        .filter(|entry| !acknowledged.contains(&entry.id))
        .collect())
}

fn body_keys(store: &Store) -> BTreeMap<OperationId, String> {
    store
        .retained_records()
        .map(|r| (r.operation.id.clone(), r.body_key.clone()))
        .chain(
            store
                .retired_records()
                .map(|r| (r.id.clone(), r.body_key.clone())),
        )
        .collect()
}

fn validate_receipt(
    receipt: &Receipt,
    keys: &BTreeMap<OperationId, String>,
) -> Result<(), DomainError> {
    if receipt.version != SCHEMA_VERSION {
        return Err(DomainError::UnsupportedVersion {
            version: receipt.version,
        });
    }
    validate_replica(&receipt.recipient_replica)?;
    receipt.operation.validate()?;
    validate_body_key(&receipt.body_key)?;
    let key = keys
        .get(&receipt.operation)
        .ok_or_else(|| DomainError::Missing {
            id: receipt.operation.clone(),
        })?;
    if key != &receipt.body_key {
        return Err(DomainError::IdConflict {
            id: receipt.operation.clone(),
        });
    }
    Ok(())
}

fn validate_page(page: &InventoryPage) -> Result<(), DomainError> {
    if page.version != SCHEMA_VERSION {
        return Err(DomainError::UnsupportedVersion {
            version: page.version,
        });
    }
    decimal(&page.revision, true)?;
    if page.entries.len() > (MAX_CONTROL_BYTES - PAGE_BYTES_OVERHEAD) / ENTRY_BYTES_UPPER_BOUND {
        return Err(DomainError::LimitExceeded {
            limit: "inventory_page_bytes".into(),
        });
    }
    for entry in &page.entries {
        validate_entry(entry)?;
    }
    if page.entries.windows(2).any(|pair| pair[0].id >= pair[1].id) {
        return Err(DomainError::Invalid {
            field: "inventory_order".into(),
        });
    }
    if let Some(next) = &page.next {
        if next.revision != page.revision
            || next.after.as_ref() != page.entries.last().map(|e| &e.id)
            || next.after.is_none()
        {
            return Err(DomainError::Invalid {
                field: "inventory_cursor".into(),
            });
        }
    }
    Ok(())
}

fn validate_entry(entry: &InventoryEntry) -> Result<(), DomainError> {
    entry.id.validate()?;
    entry.conversation.validate()?;
    validate_body_key(&entry.body_key)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::durable_conversation::{Command, Operation, Record, StoreSnapshot};
    use std::slice::from_ref;
    const A: &str = "00000000-0000-4000-8000-000000000001";
    const B: &str = "00000000-0000-4000-8000-000000000002";
    const C: &str = "00000000-0000-4000-8000-000000000003";
    fn id(n: u64) -> OperationId {
        OperationId {
            replica: A.into(),
            sequence: n.to_string(),
        }
    }
    fn entry(n: u64, tombstone: bool) -> InventoryEntry {
        InventoryEntry {
            id: id(n),
            conversation: id(1),
            body_key: format!("{n:064x}"),
            tombstone,
        }
    }
    fn receipt(recipient: &str, n: u64) -> Receipt {
        Receipt {
            version: SCHEMA_VERSION,
            recipient_replica: recipient.into(),
            operation: id(n),
            body_key: format!("{n:064x}"),
        }
    }
    #[test]
    fn session_bounds_constants_match_specification() {
        assert_eq!(MAX_CONTROL_ITEMS, 128);
        assert_eq!(MAX_CONTROL_BYTES, 32 * 1024);
        assert_eq!(MAX_HINT_BYTES, 256);
        assert_eq!(MAX_CHUNK_BYTES, 64 * 1024);
        assert_eq!(MAX_BATCH_RECORDS, 8);
        assert_eq!(MAX_RECORD_BYTES, 2 * 1024 * 1024);
        assert_eq!(MAX_BATCH_BYTES, 4 * 1024 * 1024);
        assert_eq!(MAX_SESSION_PAGES, 4096);
        assert_eq!(MAX_RECEIPTS, 65_536);
    }
    #[test]
    fn validate_transfer_batch_accepts_exact_and_ancestor_requests_and_enforces_invariants() {
        let req_exact = RequestedRecord {
            id: id(1),
            expected: Some(entry(1, false)),
        };
        let req_ancestor = RequestedRecord {
            id: id(2),
            expected: None,
        };
        let req_unavailable = RequestedRecord {
            id: id(3),
            expected: Some(entry(3, false)),
        };
        let desc1 = TransferDescriptor {
            entry: entry(1, false),
            bytes: 128,
        };
        let desc2 = TransferDescriptor {
            entry: entry(2, false),
            bytes: 256,
        };
        assert_eq!(
            validate_transfer_batch(
                &[req_exact.clone(), req_ancestor.clone(), req_unavailable],
                SyncPhase::Records,
                &[desc1.clone(), desc2.clone()],
                &[id(3)],
            ),
            Ok(())
        );

        let tomb_req = RequestedRecord {
            id: id(10),
            expected: Some(entry(10, true)),
        };
        let tomb_desc = TransferDescriptor {
            entry: entry(10, true),
            bytes: 64,
        };
        assert_eq!(
            validate_transfer_batch(
                from_ref(&tomb_req),
                SyncPhase::Tombstones,
                &[tomb_desc],
                &[]
            ),
            Ok(())
        );
        assert!(matches!(
            validate_transfer_batch(
                from_ref(&req_exact),
                SyncPhase::Tombstones,
                from_ref(&desc1),
                &[]
            ),
            Err(DomainError::Invalid { .. })
        ));

        assert!(matches!(
            validate_transfer_batch(&[], SyncPhase::Records, &[], &[]),
            Err(DomainError::LimitExceeded { .. })
        ));
        let too_many_reqs: Vec<_> = (1..=9)
            .map(|n| RequestedRecord {
                id: id(n),
                expected: None,
            })
            .collect();
        assert!(matches!(
            validate_transfer_batch(&too_many_reqs, SyncPhase::Records, &[], &[]),
            Err(DomainError::LimitExceeded { .. })
        ));

        let mismatched_expected = RequestedRecord {
            id: id(1),
            expected: Some(entry(2, false)),
        };
        assert!(matches!(
            validate_transfer_batch(
                &[mismatched_expected],
                SyncPhase::Records,
                from_ref(&desc1),
                &[]
            ),
            Err(DomainError::Invalid { .. })
        ));

        assert!(matches!(
            validate_transfer_batch(
                &[req_exact.clone(), req_exact.clone()],
                SyncPhase::Records,
                from_ref(&desc1),
                &[id(1)]
            ),
            Err(DomainError::Invalid { .. })
        ));

        let too_small = TransferDescriptor {
            entry: entry(1, false),
            bytes: 1,
        };
        assert!(matches!(
            validate_transfer_batch(from_ref(&req_exact), SyncPhase::Records, &[too_small], &[]),
            Err(DomainError::LimitExceeded { .. })
        ));
        let too_large_record = TransferDescriptor {
            entry: entry(1, false),
            bytes: MAX_RECORD_BYTES + 1,
        };
        assert!(matches!(
            validate_transfer_batch(
                from_ref(&req_exact),
                SyncPhase::Records,
                &[too_large_record],
                &[]
            ),
            Err(DomainError::LimitExceeded { .. })
        ));

        let batch_reqs: Vec<_> = (1..=3)
            .map(|n| RequestedRecord {
                id: id(n),
                expected: None,
            })
            .collect();
        let batch_descs = vec![
            TransferDescriptor {
                entry: entry(1, false),
                bytes: MAX_RECORD_BYTES,
            },
            TransferDescriptor {
                entry: entry(2, false),
                bytes: MAX_RECORD_BYTES,
            },
            TransferDescriptor {
                entry: entry(3, false),
                bytes: 2,
            },
        ];
        assert!(matches!(
            validate_transfer_batch(&batch_reqs, SyncPhase::Records, &batch_descs, &[]),
            Err(DomainError::LimitExceeded { .. })
        ));

        let mut wrong_meta = desc1.clone();
        wrong_meta.entry.body_key = format!("{:064x}", 999);
        assert!(matches!(
            validate_transfer_batch(from_ref(&req_exact), SyncPhase::Records, &[wrong_meta], &[]),
            Err(DomainError::Invalid { .. })
        ));

        assert!(matches!(
            validate_transfer_batch(
                from_ref(&req_exact),
                SyncPhase::Records,
                from_ref(&desc2),
                &[]
            ),
            Err(DomainError::Invalid { .. })
        ));
        assert!(matches!(
            validate_transfer_batch(from_ref(&req_exact), SyncPhase::Records, &[], &[id(2)]),
            Err(DomainError::Invalid { .. })
        ));
        assert!(matches!(
            validate_transfer_batch(
                &[req_exact.clone(), req_ancestor.clone()],
                SyncPhase::Records,
                &[desc1.clone(), desc1.clone()],
                &[]
            ),
            Err(DomainError::Invalid { .. })
        ));
        assert!(matches!(
            validate_transfer_batch(
                &[req_exact.clone(), req_ancestor.clone()],
                SyncPhase::Records,
                from_ref(&desc1),
                &[id(1)]
            ),
            Err(DomainError::Invalid { .. })
        ));
        assert!(matches!(
            validate_transfer_batch(
                &[req_exact.clone(), req_ancestor.clone()],
                SyncPhase::Records,
                &[],
                &[id(1), id(1)]
            ),
            Err(DomainError::Invalid { .. })
        ));
        assert!(matches!(
            validate_transfer_batch(
                &[req_exact, req_ancestor],
                SyncPhase::Records,
                &[desc1],
                &[]
            ),
            Err(DomainError::Invalid { .. })
        ));
    }
    #[test]
    fn validate_confirmed_receipts_enforces_exact_reciprocal_set() {
        let r1 = receipt(B, 1);
        let r2 = receipt(B, 2);
        assert_eq!(
            validate_confirmed_receipts(B, &[r1.clone(), r2.clone()], &[r2.clone(), r1.clone()]),
            Ok(())
        );

        assert!(matches!(
            validate_confirmed_receipts("invalid", from_ref(&r1), from_ref(&r1)),
            Err(DomainError::Invalid { .. })
        ));
        assert!(matches!(
            validate_confirmed_receipts(B, &[], &[]),
            Err(DomainError::LimitExceeded { .. })
        ));
        let too_many: Vec<_> = (1..=129).map(|n| receipt(B, n)).collect();
        assert!(matches!(
            validate_confirmed_receipts(B, &too_many, &too_many),
            Err(DomainError::LimitExceeded { .. })
        ));
        assert!(matches!(
            validate_confirmed_receipts(B, &[r1.clone(), r2.clone()], from_ref(&r1)),
            Err(DomainError::Invalid { .. })
        ));

        let mut wrong_version = r1.clone();
        wrong_version.version = 1;
        assert!(matches!(
            validate_confirmed_receipts(B, from_ref(&wrong_version), from_ref(&r1)),
            Err(DomainError::UnsupportedVersion { version: 1 })
        ));
        assert!(matches!(
            validate_confirmed_receipts(B, from_ref(&r1), &[wrong_version]),
            Err(DomainError::UnsupportedVersion { version: 1 })
        ));

        let wrong_peer = receipt(C, 1);
        assert!(matches!(
            validate_confirmed_receipts(B, from_ref(&r1), &[wrong_peer]),
            Err(DomainError::Invalid { .. })
        ));

        let mut wrong_key = r1.clone();
        wrong_key.body_key = format!("{:064x}", 999);
        assert!(matches!(
            validate_confirmed_receipts(B, from_ref(&r1), &[wrong_key]),
            Err(DomainError::IdConflict { .. })
        ));

        assert!(matches!(
            validate_confirmed_receipts(B, &[r1.clone(), r1.clone()], &[r1.clone(), r2.clone()]),
            Err(DomainError::Invalid { .. })
        ));
        assert!(matches!(
            validate_confirmed_receipts(B, &[r1.clone(), r2.clone()], &[r1.clone(), r1.clone()]),
            Err(DomainError::Invalid { .. })
        ));
        assert!(matches!(
            validate_confirmed_receipts(B, &[r1], &[r2]),
            Err(DomainError::Missing { .. })
        ));
    }
    fn store(count: u64) -> Store {
        let records = (1..=count)
            .map(|n| Record {
                body_key: format!("{n:064x}"),
                operation: Operation {
                    version: 2,
                    id: id(n),
                    body: OperationBody::ConversationCreated {
                        title: "Synthetic".into(),
                    },
                },
            })
            .collect();
        Store::restore(StoreSnapshot {
            version: 2,
            revision: "1".into(),
            records,
            retired: vec![],
            quarantine: vec![],
        })
        .unwrap()
    }
    fn page(store: &Store) -> InventoryPage {
        inventory_page(
            store,
            InventoryRequest {
                cursor: InventoryCursor {
                    revision: store.revision().into(),
                    after: None,
                },
                max_items: 128,
            },
        )
        .unwrap()
    }

    #[test]
    fn legacy_v1_sync_envelopes_are_rejected() {
        let store = store(1);
        let mut legacy = page(&store);
        legacy.version = 1;
        assert!(matches!(
            reconcile_page(&store, legacy),
            Err(DomainError::UnsupportedVersion { version: 1 })
        ));
        let mut receipts = receipts_after_commit(&store, B, &[id(1)]).unwrap();
        receipts[0].version = 1;
        assert!(matches!(
            merge_receipts(&store, B, vec![], receipts),
            Err(DomainError::UnsupportedVersion { version: 1 })
        ));
    }

    #[test]
    fn exact_receipts_do_not_acknowledge_sequence_gaps_or_other_peers() {
        let store = store(3);
        let receipt = receipts_after_commit(&store, B, &[id(3)]).unwrap();
        let ledger = merge_receipts(&store, B, vec![], receipt.clone()).unwrap();
        assert_eq!(
            merge_receipts(&store, B, ledger.clone(), receipt).unwrap(),
            ledger
        );
        assert_eq!(
            unacknowledged(&store, B, &ledger)
                .unwrap()
                .iter()
                .map(|e| e.id.clone())
                .collect::<Vec<_>>(),
            vec![id(1), id(2)]
        );
        assert_eq!(unacknowledged(&store, C, &ledger).unwrap().len(), 3);
        let mut stale = ledger[0].clone();
        stale.body_key = format!("{:064x}", 999);
        assert!(matches!(
            merge_receipts(&store, B, ledger.clone(), vec![stale]),
            Err(DomainError::IdConflict { .. })
        ));
        assert!(matches!(
            merge_receipts(&store, C, ledger.clone(), vec![ledger[0].clone()]),
            Err(DomainError::Invalid { .. })
        ));
    }

    #[test]
    fn inventory_is_bounded_exact_and_pinned_to_revision() {
        let store = store(128);
        let first = page(&store);
        assert!(serde_json::to_vec(&first).unwrap().len() < MAX_CONTROL_BYTES);
        let second = inventory_page(
            &store,
            InventoryRequest {
                cursor: first.next.clone().unwrap(),
                max_items: 128,
            },
        )
        .unwrap();
        let third = inventory_page(
            &store,
            InventoryRequest {
                cursor: second.next.clone().unwrap(),
                max_items: 128,
            },
        )
        .unwrap();
        let ids: BTreeSet<_> = first
            .entries
            .iter()
            .chain(&second.entries)
            .chain(&third.entries)
            .map(|e| e.id.clone())
            .collect();
        assert_eq!(ids.len(), 128);
        assert!(third.next.is_none());
        assert!(matches!(
            inventory_page(
                &store,
                InventoryRequest {
                    cursor: InventoryCursor {
                        revision: "0".into(),
                        after: None
                    },
                    max_items: 128
                }
            ),
            Err(DomainError::RevisionConflict { .. })
        ));
    }

    #[test]
    fn reconciliation_fetches_tombstones_before_missing_records_and_detects_body_conflicts() {
        let local = store(1);
        let remote = store(2);
        let plan = reconcile_page(&local, page(&remote)).unwrap();
        assert_eq!(plan.fetch_records.len(), 1);
        assert_eq!(plan.fetch_records[0].id, id(2));
        let deletion = Record {
            body_key: format!("{:064x}", 100),
            operation: Operation {
                version: 2,
                id: id(100),
                body: OperationBody::ConversationDeleted {
                    conversation: id(1),
                },
            },
        };
        let deleted = Store::restore(
            local
                .plan(Command::Import {
                    expected_revision: "1".into(),
                    records: vec![deletion],
                })
                .unwrap()
                .snapshot,
        )
        .unwrap();
        assert_eq!(
            reconcile_page(&local, page(&deleted))
                .unwrap()
                .fetch_tombstones
                .len(),
            1
        );
        let response = reconcile_page(&deleted, page(&remote)).unwrap();
        assert_eq!(response.offer_tombstones.len(), 1);
        assert_eq!(response.fetch_records.len(), 1);
        let mut conflicting = page(&remote);
        conflicting.entries[0].body_key = format!("{:064x}", 999);
        assert!(matches!(
            reconcile_page(&local, conflicting),
            Err(DomainError::IdConflict { .. })
        ));
        assert_eq!(
            receipts_after_commit(&deleted, B, &[id(1)]).unwrap()[0].body_key,
            format!("{:064x}", 1)
        );
    }
}
