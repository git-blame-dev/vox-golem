//! Exact inventory and per-peer durable receipts; transport and retry IO are native-owned.
//!
//! Inventory advertises retained bodies only, never purged content. Walk revision-pinned
//! pages to completion; restart if the sender revision changes. A missing ID in one page
//! is not evidence that a peer lacks it. Apply fetched tombstones before requesting bodies,
//! then restore/reconcile again. Validate fetched records against their advertised body
//! keys in the trusted bridge, independently recomputing keys as documented by the domain.
//! `reconcile_page` does not infer convergence from one page or from a sequence watermark.
//! `missing_ancestors` is bounded work, not an instruction to execute a provider request.
//! Generate receipts from the committed revision only. Store receipts and retry/outbox
//! changes transactionally with a revision check; publishing or GMS success is not an ACK.
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

fn inventory(store: &Store) -> BTreeMap<OperationId, InventoryEntry> {
    let snapshot = store.export();
    let quarantined: BTreeSet<_> = snapshot.quarantine.into_iter().map(|q| q.id).collect();
    snapshot
        .records
        .into_iter()
        .filter(|r| !quarantined.contains(&r.operation.id))
        .map(|r| {
            let entry = InventoryEntry {
                id: r.operation.id.clone(),
                conversation: r.operation.conversation().clone(),
                body_key: r.body_key,
                tombstone: matches!(r.operation.body, OperationBody::ConversationDeleted { .. }),
            };
            (entry.id.clone(), entry)
        })
        .collect()
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
    let remaining: Vec<_> = inventory(store)
        .into_values()
        .filter(|entry| {
            request
                .cursor
                .after
                .as_ref()
                .is_none_or(|after| &entry.id > after)
        })
        .collect();
    let entries: Vec<_> = remaining.iter().take(max).cloned().collect();
    let next = if remaining.len() > entries.len() {
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
    let snapshot = store.export();
    let retained_metadata: BTreeMap<_, _> = snapshot
        .records
        .iter()
        .map(|r| {
            (
                r.operation.id.clone(),
                (
                    r.operation.conversation().clone(),
                    matches!(r.operation.body, OperationBody::ConversationDeleted { .. }),
                ),
            )
        })
        .collect();
    let retired: BTreeMap<_, _> = snapshot
        .retired
        .into_iter()
        .map(|r| (r.id.clone(), r))
        .collect();
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
            if retained_metadata
                .get(&entry.id)
                .is_some_and(|(conversation, tombstone)| {
                    conversation != &entry.conversation || tombstone != &entry.tombstone
                })
            {
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
    let snapshot = store.export();
    snapshot
        .records
        .into_iter()
        .map(|r| (r.operation.id, r.body_key))
        .chain(snapshot.retired.into_iter().map(|r| (r.id, r.body_key)))
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
        entry.id.validate()?;
        entry.conversation.validate()?;
        validate_body_key(&entry.body_key)?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::durable_conversation::{Command, Operation, Record, StoreSnapshot};
    const A: &str = "00000000-0000-4000-8000-000000000001";
    const B: &str = "00000000-0000-4000-8000-000000000002";
    const C: &str = "00000000-0000-4000-8000-000000000003";
    fn id(n: u64) -> OperationId {
        OperationId {
            replica: A.into(),
            sequence: n.to_string(),
        }
    }
    fn store(count: u64) -> Store {
        let records = (1..=count)
            .map(|n| Record {
                body_key: format!("{n:064x}"),
                operation: Operation {
                    version: 1,
                    id: id(n),
                    body: OperationBody::ConversationCreated {
                        title: "Synthetic".into(),
                    },
                },
            })
            .collect();
        Store::restore(StoreSnapshot {
            version: 1,
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
                version: 1,
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
