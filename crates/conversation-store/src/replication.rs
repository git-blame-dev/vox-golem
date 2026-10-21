//! Bounded replication reads against the SQLite owner's current canonical revision.
use crate::{codec, Store};
use serde::{Deserialize, Serialize};
use voxgolem_core::conversation_sync::{
    self as sync, InventoryCursor, InventoryEntry, InventoryPage, InventoryRequest,
};
use voxgolem_core::durable_conversation::{OperationId, Record};

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    Revision,
    RetainedDescriptors {
        revision: Option<String>,
        entries: Vec<InventoryEntry>,
    },
    Inventory {
        cursor: Option<InventoryCursor>,
    },
    Reconcile {
        page: InventoryPage,
    },
    Receipts {
        ids: Vec<OperationId>,
    },
    Import {
        records: Vec<Record>,
        entries: Vec<InventoryEntry>,
    },
    Bodies {
        revision: String,
        entries: Vec<BodyRequest>,
        tombstones: bool,
    },
}

#[derive(Deserialize)]
#[serde(untagged)]
pub enum BodyRequest {
    Exact(InventoryEntry),
    Ancestor(AncestorRequest),
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AncestorRequest {
    id: OperationId,
    body_key: Option<String>,
}

#[derive(Serialize)]
struct Body {
    entry: InventoryEntry,
    // UTF-8 JSON string, not a parsed JSON object: Kotlin encodes these exact canonical
    // bytes once for the Asset without reparsing/reformatting the record.
    body: String,
}

fn entry(record: &Record) -> InventoryEntry {
    InventoryEntry {
        id: record.operation.id.clone(),
        conversation: record.operation.conversation().clone(),
        body_key: record.body_key.clone(),
        tombstone: matches!(
            record.operation.body,
            voxgolem_core::durable_conversation::OperationBody::ConversationDeleted { .. }
        ),
    }
}

impl Store {
    pub fn replication(&mut self, request: Request) -> crate::Result<serde_json::Value> {
        if let Request::Import { records, entries } = request {
            if records.is_empty() || records.len() > 8 || records.len() != entries.len() {
                return Err(codec::limit("replication_import").into());
            }
            let mut bytes = 0usize;
            for (record, expected) in records.iter().zip(entries) {
                if entry(record) != expected {
                    return Err(codec::invalid("replication_body_descriptor").into());
                }
                bytes += codec::encode(record, 2 * 1024 * 1024, "replication_record_bytes")?.len();
                if bytes > 4 * 1024 * 1024 {
                    return Err(codec::limit("replication_asset_bytes").into());
                }
            }
            // The canonical import owner independently recomputes body keys and commits
            // durable receipts; advertised metadata never becomes identity authority.
            return serde_json::to_value(self.import_records(records, true, None)?)
                .map_err(|_| crate::Error::Storage);
        }
        let replica = if matches!(
            &request,
            Request::Reconcile { .. } | Request::Receipts { .. }
        ) {
            self.replica()?
        } else {
            String::new()
        };
        self.read(|core| {
            match request {
                Request::Revision => encode(core.revision()),
                Request::RetainedDescriptors { revision, entries } => {
                    if entries.len() > sync::MAX_CONTROL_ITEMS {
                        return Err(codec::limit("retained_descriptor_items"));
                    }
                    // Bound the encoded control request independently of the larger
                    // JNI envelope limit; never serialize retained bodies here.
                    codec::encode(&entries, sync::MAX_CONTROL_BYTES, "retained_descriptor_bytes")?;
                    encode(sync::retained_descriptors(core, revision, entries)?)
                }
                Request::Import { .. } => unreachable!("handled before read transaction"),
                Request::Inventory { cursor } => {
                    if cursor.as_ref().is_some_and(|cursor| cursor.revision != core.revision()) {
                        return Ok(serde_json::json!({"restart": true}));
                    }
                    encode(sync::inventory_page(core, InventoryRequest {
                    cursor: cursor.unwrap_or_else(|| InventoryCursor { revision: core.revision().into(), after: None }),
                    max_items: sync::MAX_CONTROL_ITEMS,
                })?)
                },
                Request::Reconcile { page } => {
                    let plan = sync::reconcile_page(core, page.clone())?;
                    let receipts = sync::known_receipts(core, &replica, &page)?;
                    Ok(serde_json::json!({"plan": plan, "receipts": receipts}))
                }
                Request::Receipts { ids } => encode(sync::receipts_after_commit(core, &replica, &ids)?),
                Request::Bodies { revision, entries, tombstones } => {
                    if revision != core.revision() { return Ok(serde_json::json!({"restart": true})); }
                    if entries.is_empty() || entries.len() > 8 { return Err(codec::limit("replication_bodies")); }
                    let mut seen = std::collections::BTreeSet::new();
                    let mut bodies = Vec::new();
                    let mut unavailable = Vec::new();
                    let mut bytes = 0usize;
                    for requested in entries {
                        let id = match &requested { BodyRequest::Exact(e) => &e.id, BodyRequest::Ancestor(e) => {
                            if e.body_key.is_some() { return Err(codec::invalid("replication_body_descriptor")); }
                            &e.id
                        }};
                        id.validate()?;
                        if !seen.insert(id.clone()) { return Err(codec::invalid("replication_body_duplicate")); }
                        let record = core.retained_records().find(|r| &r.operation.id == id && !core.is_quarantined(id));
                        let Some(record) = record else { unavailable.push(id.clone()); continue; };
                        let actual = entry(record);
                        if let BodyRequest::Exact(expected) = requested {
                            if expected != actual { return Err(codec::invalid("replication_body_descriptor")); }
                        }
                        if tombstones && !actual.tombstone { return Err(codec::invalid("replication_body_phase")); }
                        let body = codec::encode(record, 2 * 1024 * 1024, "replication_record_bytes")?;
                        if body.len() > 2 * 1024 * 1024 { return Err(codec::limit("replication_record_bytes")); }
                        if bytes + body.len() > 4 * 1024 * 1024 { unavailable.push(actual.id); continue; }
                        bytes += body.len();
                        bodies.push(Body { entry: actual, body });
                    }
                    Ok(serde_json::json!({"restart": false, "bodies": bodies, "unavailable": unavailable}))
                }
            }
        })
    }
}

fn encode(
    value: impl Serialize,
) -> Result<serde_json::Value, voxgolem_core::durable_conversation::DomainError> {
    serde_json::to_value(value).map_err(|_| codec::invalid("replication_encoding"))
}
