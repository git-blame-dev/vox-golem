//! Portable durable conversation domain. SQLite, not this ephemeral reducer, is the authority.
//!
//! Restore a database revision, plan a command, then commit the returned snapshot and work
//! atomically only if that revision still matches. Publish/ACK only after that commit.
//! Body keys must be recomputed by the trusted native adapter using SHA-256 of normalized
//! `Operation` JSON, not `Record` JSON: parse into this type, serialize with serde_json's
//! compact UTF-8 serializer (declared field order, explicit nulls), hash those bytes, then
//! lowercase-hex encode. Never trust peer keys or hash the original unnormalized JSON.
//! The adapter must check the raw payload's `MAX_ENCODED_OPERATION_BYTES` before parsing,
//! bound snapshot decoding/staging, and authenticate replica associations separately.
//! Core deliberately adds no JSON, cryptographic, database or transport dependency.
//!
//! `Command::Import` is also the local create/completion/delete/atomic-clear command:
//! immutable records have the same validation regardless of origin. Allocate installation
//! UUID/counters transactionally in native storage, never from wall clocks or GMS node IDs.
//! `MutationPlan` is speculative; on commit failure discard it, on revision conflict restore
//! and recompute, without rerunning inference. Commit its snapshot, purge directives and
//! publication work together. Retain retired body keys and tombstones indefinitely; purge
//! deleted content, drafts and payload staging, including previously pending outbox rows.
//! `suppressed` identifies late completions that must not be presented as saved answers.
//! Context selection pins revision, parent and ordered ancestor IDs; after inference import
//! that pinned ancestry against the then-current revision, never choose a new parent.
//!
//! A dependency that proves an already committed pending record invalid still commits:
//! retain that immutable body and add a bounded local quarantine disposition. Propagate
//! disposition to previously pending descendants, exclude them from heads/context/fetches,
//! and reject known-invalid new input atomically. Quarantine is not deletion or history
//! eviction. Native storage must persist the ledger at the same revision and stop outgoing
//! staging/retries for `MutationPlan::quarantined`, without removing the evidence bodies.
//! Quarantined duplicates remain exact-body ACKable and are dispositioned again in the plan.
//! Retired duplicates appear in `suppressed` as well as `duplicates`; neither republish nor
//! advance revision. Consumer completion code must examine disposition, not just duplicates.
//! The optional v1 input `quarantine` field defaults empty; canonical exports always include
//! it, ordered by ID. Canonical snapshot hashing must serialize `restore(input)?.export()`,
//! including that ledger. Operation body-key hashing is unchanged and never includes it.
//! Known context IDs must name turns of the same conversation, including compact retired
//! identities. That evidence survives deleting a context witness in another conversation.
//! Every returned mutation snapshot passes public restoration after purge, before native IO.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub const SCHEMA_VERSION: u32 = 1;
pub const MAX_BATCH_RECORDS: usize = 128;
pub const MAX_PROMPT_BYTES: usize = 32 * 1024;
pub const MAX_TURN_TEXT_BYTES: usize = 256 * 1024;
pub const MAX_ENCODED_OPERATION_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_STORE_RECORDS: usize = 16_384;
pub const MAX_QUARANTINED_RECORDS: usize = MAX_STORE_RECORDS;
pub const MAX_RETIRED_RECORDS: usize = 65_536;
pub const MAX_STORE_TEXT_BYTES: usize = 64 * 1024 * 1024;
pub const MAX_ANCESTRY: usize = 4096;
pub const MAX_CONTEXT_TURNS: usize = 20;
pub const MAX_CONTEXT_BYTES: usize = 256 * 1024;

/// Ordering is canonical lexical ID ordering, not a causal or receipt watermark.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OperationId {
    pub replica: String,
    pub sequence: String,
}

impl OperationId {
    pub fn validate(&self) -> Result<(), DomainError> {
        validate_replica(&self.replica)?;
        decimal(&self.sequence, false)?;
        Ok(())
    }
}

pub fn validate_replica(replica: &str) -> Result<(), DomainError> {
    let bytes = replica.as_bytes();
    if bytes.len() != 36
        || bytes[14] != b'4'
        || !matches!(bytes[19], b'8' | b'9' | b'a' | b'b')
        || bytes.iter().enumerate().any(|(i, b)| {
            if matches!(i, 8 | 13 | 18 | 23) {
                *b != b'-'
            } else {
                !b.is_ascii_digit() && !matches!(*b, b'a'..=b'f')
            }
        })
    {
        return Err(DomainError::Invalid {
            field: "replica_uuid_v4".into(),
        });
    }
    Ok(())
}

pub(crate) fn decimal(value: &str, allow_zero: bool) -> Result<u64, DomainError> {
    if value.is_empty()
        || value.len() > 20
        || (value.len() > 1 && value.starts_with('0'))
        || !value.bytes().all(|b| b.is_ascii_digit())
    {
        return Err(DomainError::Invalid {
            field: "decimal_u64".into(),
        });
    }
    value
        .parse::<u64>()
        .ok()
        .filter(|n| allow_zero || *n != 0)
        .ok_or_else(|| DomainError::Invalid {
            field: "decimal_u64".into(),
        })
}

pub(crate) fn validate_body_key(key: &str) -> Result<(), DomainError> {
    if key.len() != 64
        || !key
            .bytes()
            .all(|b| b.is_ascii_digit() || matches!(b, b'a'..=b'f'))
    {
        return Err(DomainError::Invalid {
            field: "body_key_sha256".into(),
        });
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Operation {
    pub version: u32,
    pub id: OperationId,
    pub body: OperationBody,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
#[allow(clippy::large_enum_variant)]
pub enum OperationBody {
    ConversationCreated {
        title: String,
    },
    TurnCompleted {
        conversation: OperationId,
        parent: Option<OperationId>,
        request: OperationId,
        prompt: String,
        answer: Answer,
        terminal_stage: TerminalStage,
        provenance: Provenance,
    },
    ConversationDeleted {
        conversation: OperationId,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Answer {
    Text { text: String },
    Refusal { reason: String },
}

impl Answer {
    pub fn text(&self) -> &str {
        match self {
            Self::Text { text } => text,
            Self::Refusal { reason } => reason,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TerminalStage {
    Instant,
    Deep,
    Review,
}

/// Profile identities are inert provenance, never synced executable system instructions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Provenance {
    pub requested_model: String,
    pub returned_model: Option<String>,
    pub policy_id: String,
    pub policy_version: String,
    pub instruction_profile_id: String,
    pub instruction_profile_version: String,
    pub context: Vec<OperationId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Record {
    pub body_key: String,
    pub operation: Operation,
}

/// Keep the immutable identity, but remove content and outgoing payloads after deletion.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetiredRecord {
    pub id: OperationId,
    pub conversation: OperationId,
    pub body_key: String,
}

/// Local disposition only: immutable bodies remain in `records`, never become context.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QuarantinedRecord {
    pub id: OperationId,
    pub reason: QuarantineReason,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuarantineReason {
    InvalidReference,
    InvalidContext,
    Cycle,
    AncestryLimit,
}

/// Only this input is restored from SQLite; projections are always recomputed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoreSnapshot {
    pub version: u32,
    pub revision: String,
    pub records: Vec<Record>,
    pub retired: Vec<RetiredRecord>,
    #[serde(default)]
    pub quarantine: Vec<QuarantinedRecord>,
}

impl StoreSnapshot {
    pub fn empty() -> Self {
        Self {
            version: SCHEMA_VERSION,
            revision: "0".into(),
            records: vec![],
            retired: vec![],
            quarantine: vec![],
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConversationView {
    pub id: OperationId,
    pub title: Option<String>,
    pub deleted: bool,
    pub heads: Vec<OperationId>,
    pub pending: Vec<OperationId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoreView {
    pub revision: String,
    pub records: Vec<Record>,
    pub retired: Vec<RetiredRecord>,
    pub conversations: Vec<ConversationView>,
    pub pending: Vec<OperationId>,
    #[serde(default)]
    pub quarantine: Vec<QuarantinedRecord>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Command {
    Import {
        expected_revision: String,
        records: Vec<Record>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MutationPlan {
    pub base_revision: String,
    pub snapshot: StoreSnapshot,
    pub accepted: Vec<OperationId>,
    pub duplicates: Vec<OperationId>,
    pub suppressed: Vec<OperationId>,
    pub purge_conversations: Vec<OperationId>,
    /// Eligible newly admitted records, including remote imports for multi-peer forwarding.
    pub publish: Vec<Record>,
    /// Stop publication/retry work for these IDs, without deleting their retained bodies.
    #[serde(default)]
    pub quarantined: Vec<OperationId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextRequest {
    pub expected_revision: String,
    pub conversation: OperationId,
    pub selected_head: Option<OperationId>,
    pub max_turns: usize,
    pub max_text_bytes: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextSelection {
    pub revision: String,
    pub conversation: OperationId,
    pub parent: Option<OperationId>,
    pub ancestor_ids: Vec<OperationId>,
    pub turns: Vec<Record>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContinuationRequest {
    pub expected_revision: String,
    pub conversation: OperationId,
    pub selected_head: Option<OperationId>,
    pub local_work_in_flight: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ContinuationDecision {
    Keep { selected_head: Option<OperationId> },
    FastForward { head: OperationId },
    Choose { heads: Vec<OperationId> },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum DomainError {
    Invalid { field: String },
    UnsupportedVersion { version: u32 },
    LimitExceeded { limit: String },
    RevisionConflict { expected: String, actual: String },
    IdConflict { id: OperationId },
    InvalidReference { id: OperationId },
    InvalidContext { id: OperationId },
    Cycle { id: OperationId },
    Missing { id: OperationId },
    Quarantined { id: OperationId },
    Deleted { conversation: OperationId },
    AmbiguousHeads { heads: Vec<OperationId> },
}

impl std::fmt::Display for DomainError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for DomainError {}

#[derive(Debug, Clone)]
pub struct Store {
    revision: String,
    records: BTreeMap<OperationId, Record>,
    retired: BTreeMap<OperationId, RetiredRecord>,
    quarantine: BTreeMap<OperationId, QuarantineReason>,
}

impl Store {
    pub fn restore(snapshot: StoreSnapshot) -> Result<Self, DomainError> {
        if snapshot.version != SCHEMA_VERSION {
            return Err(DomainError::UnsupportedVersion {
                version: snapshot.version,
            });
        }
        decimal(&snapshot.revision, true)?;
        if snapshot.records.len() > MAX_STORE_RECORDS
            || snapshot.retired.len() > MAX_RETIRED_RECORDS
            || snapshot.quarantine.len() > MAX_QUARANTINED_RECORDS
        {
            return Err(limit("restore_records"));
        }
        let mut store = Self {
            revision: snapshot.revision,
            records: BTreeMap::new(),
            retired: BTreeMap::new(),
            quarantine: BTreeMap::new(),
        };
        for record in snapshot.records {
            record.validate()?;
            store.insert(record)?;
        }
        for record in snapshot.retired {
            record.id.validate()?;
            record.conversation.validate()?;
            validate_body_key(&record.body_key)?;
            if store.records.contains_key(&record.id)
                || store
                    .retired
                    .insert(record.id.clone(), record.clone())
                    .is_some()
            {
                return Err(DomainError::IdConflict { id: record.id });
            }
        }
        for entry in snapshot.quarantine {
            entry.id.validate()?;
            if !store.records.contains_key(&entry.id)
                || store
                    .quarantine
                    .insert(entry.id.clone(), entry.reason)
                    .is_some()
            {
                return Err(DomainError::Invalid {
                    field: "quarantine_identity".into(),
                });
            }
        }
        // A quarantined delete may have retired only records of an impossible conversation ID.
        // Keep those compact identities; quarantine itself never purges retained content.
        let tombstoned: BTreeSet<_> = store
            .records
            .values()
            .filter_map(|r| match &r.operation.body {
                OperationBody::ConversationDeleted { conversation } => Some(conversation.clone()),
                _ => None,
            })
            .collect();
        if store
            .retired
            .values()
            .any(|r| !tombstoned.contains(&r.conversation))
        {
            return Err(DomainError::Invalid {
                field: "retired_without_tombstone".into(),
            });
        }
        store.validate_graph()?;
        store.validate_quarantine()?;
        let deleted = store.deleted();
        store.purge(&deleted);
        store.validate_limits()?;
        Ok(store)
    }

    pub fn revision(&self) -> &str {
        &self.revision
    }

    pub fn export(&self) -> StoreSnapshot {
        StoreSnapshot {
            version: SCHEMA_VERSION,
            revision: self.revision.clone(),
            records: self.records.values().cloned().collect(),
            retired: self.retired.values().cloned().collect(),
            quarantine: self.quarantine_entries(),
        }
    }

    pub fn snapshot(&self) -> StoreView {
        let deleted = self.deleted();
        let mut conversations = BTreeMap::new();
        for record in self.records.values() {
            let id = record.operation.conversation().clone();
            conversations
                .entry(id.clone())
                .or_insert_with(|| ConversationView {
                    id: id.clone(),
                    title: None,
                    deleted: deleted.contains(&id),
                    heads: vec![],
                    pending: vec![],
                });
            if let OperationBody::ConversationCreated { title } = &record.operation.body {
                conversations
                    .get_mut(&id)
                    .expect("inserted conversation")
                    .title = Some(title.clone());
            }
        }
        let mut pending = vec![];
        let mut children = BTreeSet::new();
        let mut title_roots = BTreeMap::new();
        for record in self.records.values() {
            if self.quarantine.contains_key(&record.operation.id) {
                continue;
            }
            if let OperationBody::TurnCompleted {
                conversation,
                parent,
                prompt,
                ..
            } = &record.operation.body
            {
                let view = conversations
                    .get_mut(conversation)
                    .expect("inserted conversation");
                if self.ready(&record.operation.id) {
                    view.heads.push(record.operation.id.clone());
                    if let Some(parent) = parent {
                        children.insert(parent.clone());
                    } else if !view.deleted && view.title.as_deref() == Some("Conversation") {
                        // Records are scanned in lexical ID order, not global submission order.
                        title_roots.entry(conversation.clone()).or_insert(prompt);
                    }
                } else {
                    view.pending.push(record.operation.id.clone());
                    pending.push(record.operation.id.clone());
                }
            }
        }
        for view in conversations.values_mut() {
            view.heads.retain(|id| !children.contains(id));
            if let Some(prompt) = title_roots.get(&view.id) {
                view.title = Some(title_preview(prompt));
            }
        }
        StoreView {
            revision: self.revision.clone(),
            records: self.records.values().cloned().collect(),
            retired: self.retired.values().cloned().collect(),
            conversations: conversations.into_values().collect(),
            pending,
            quarantine: self.quarantine_entries(),
        }
    }

    pub fn plan(&self, command: Command) -> Result<MutationPlan, DomainError> {
        let Command::Import {
            expected_revision,
            records,
        } = command;
        self.check_revision(&expected_revision)?;
        if records.len() > MAX_BATCH_RECORDS {
            return Err(limit("batch_records"));
        }
        let mut next = self.clone();
        let mut accepted = BTreeSet::new();
        let mut duplicates = BTreeSet::new();
        let formerly_pending = self.unresolved_ids();
        for record in records {
            record.validate()?;
            let id = record.operation.id.clone();
            if next.insert(record)? {
                accepted.insert(id);
            } else {
                duplicates.insert(id);
            }
        }
        next.resolve_graph(&accepted, &formerly_pending)?;
        let deleted = next.deleted();
        next.purge(&deleted);
        next.validate_limits()?;
        if !accepted.is_empty() {
            next.revision = decimal(&self.revision, true)?
                .checked_add(1)
                .ok_or_else(|| limit("revision_u64"))?
                .to_string();
        }
        let suppressed = accepted
            .iter()
            .chain(duplicates.iter())
            .filter(|id| next.retired.contains_key(*id))
            .cloned()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let publish = accepted
            .iter()
            .filter_map(|id| next.records.get(id).cloned())
            .collect();
        let previously_deleted = self.deleted();
        let newly_retired: BTreeSet<_> = next
            .retired
            .values()
            .filter(|r| accepted.contains(&r.id))
            .map(|r| r.conversation.clone())
            .collect();
        let purge_conversations = deleted
            .into_iter()
            .filter(|id| !previously_deleted.contains(id) || newly_retired.contains(id))
            .collect();
        let quarantined = next
            .quarantine
            .iter()
            .filter(|(id, reason)| {
                self.quarantine.get(*id) != Some(*reason) || duplicates.contains(*id)
            })
            .map(|(id, _)| id.clone())
            .collect();
        let snapshot = next.export();
        // Validate the post-purge state through the same boundary used after a native commit.
        Self::restore(snapshot.clone())?;
        Ok(MutationPlan {
            base_revision: self.revision.clone(),
            snapshot,
            accepted: accepted.into_iter().collect(),
            duplicates: duplicates.into_iter().collect(),
            suppressed,
            purge_conversations,
            publish,
            quarantined,
        })
    }

    fn insert(&mut self, record: Record) -> Result<bool, DomainError> {
        let id = &record.operation.id;
        if let Some(existing) = self.records.get(id) {
            if existing != &record {
                return Err(DomainError::IdConflict { id: id.clone() });
            }
            return Ok(false);
        }
        if let Some(existing) = self.retired.get(id) {
            if existing.body_key != record.body_key
                || &existing.conversation != record.operation.conversation()
            {
                return Err(DomainError::IdConflict { id: id.clone() });
            }
            return Ok(false);
        }
        self.records.insert(id.clone(), record);
        Ok(true)
    }

    fn check_revision(&self, expected: &str) -> Result<(), DomainError> {
        decimal(expected, true)?;
        if expected != self.revision {
            return Err(DomainError::RevisionConflict {
                expected: expected.into(),
                actual: self.revision.clone(),
            });
        }
        Ok(())
    }

    fn deleted(&self) -> BTreeSet<OperationId> {
        self.records
            .values()
            .filter(|r| !self.quarantine.contains_key(&r.operation.id))
            .filter_map(|r| match &r.operation.body {
                OperationBody::ConversationDeleted { conversation } => Some(conversation.clone()),
                _ => None,
            })
            .collect()
    }

    fn purge(&mut self, deleted: &BTreeSet<OperationId>) {
        let ids: Vec<_> = self
            .records
            .values()
            .filter(|r| {
                deleted.contains(r.operation.conversation())
                    && !matches!(r.operation.body, OperationBody::ConversationDeleted { .. })
            })
            .map(|r| r.operation.id.clone())
            .collect();
        for id in ids {
            self.quarantine.remove(&id);
            let record = self.records.remove(&id).expect("collected record");
            self.retired.insert(
                id.clone(),
                RetiredRecord {
                    id,
                    conversation: record.operation.conversation().clone(),
                    body_key: record.body_key,
                },
            );
        }
    }

    fn validate_limits(&self) -> Result<(), DomainError> {
        if self.records.len() > MAX_STORE_RECORDS {
            return Err(limit("store_records"));
        }
        if self.retired.len() > MAX_RETIRED_RECORDS {
            return Err(limit("retired_records"));
        }
        if self.quarantine.len() > MAX_QUARANTINED_RECORDS {
            return Err(limit("quarantine_records"));
        }
        check_text_quota(self.records.values().map(|r| r.operation.text_bytes()))?;
        Ok(())
    }

    fn validate_graph(&self) -> Result<(), DomainError> {
        for record in self.records.values() {
            if !self.quarantine.contains_key(&record.operation.id) {
                self.validate_record_graph(record)?;
            }
        }
        Ok(())
    }

    fn validate_record_graph(&self, record: &Record) -> Result<(), DomainError> {
        match &record.operation.body {
            OperationBody::ConversationCreated { .. } => {}
            OperationBody::ConversationDeleted { conversation } => {
                if !self.valid_conversation_reference(conversation) {
                    return Err(DomainError::InvalidReference {
                        id: record.operation.id.clone(),
                    });
                }
            }
            OperationBody::TurnCompleted {
                conversation,
                parent,
                provenance,
                ..
            } => {
                if !self.valid_conversation_reference(conversation) {
                    return Err(DomainError::InvalidReference {
                        id: record.operation.id.clone(),
                    });
                }
                let path = self.path(&record.operation.id)?;
                let ancestors: Vec<_> = path.iter().skip(1).rev().cloned().collect();
                // Even when parents are missing, provenance must end at the pinned parent.
                if (!provenance.context.is_empty() && provenance.context.last() != parent.as_ref())
                    || provenance
                        .context
                        .iter()
                        .any(|id| id == &record.operation.id)
                    || provenance.context.windows(2).any(|pair| pair[0] == pair[1])
                {
                    return Err(DomainError::InvalidContext {
                        id: record.operation.id.clone(),
                    });
                }
                let known_suffix = provenance.context.len().min(ancestors.len());
                if provenance.context[provenance.context.len() - known_suffix..]
                    != ancestors[ancestors.len() - known_suffix..]
                {
                    return Err(DomainError::InvalidContext {
                        id: record.operation.id.clone(),
                    });
                }
                if self.path_complete(&path) && provenance.context.len() > ancestors.len() {
                    return Err(DomainError::InvalidContext {
                        id: record.operation.id.clone(),
                    });
                }
                if provenance
                    .context
                    .iter()
                    .any(|id| !self.valid_context_reference(id, conversation))
                    || provenance.context.iter().collect::<BTreeSet<_>>().len()
                        != provenance.context.len()
                {
                    return Err(DomainError::InvalidContext {
                        id: record.operation.id.clone(),
                    });
                }
            }
        }
        Ok(())
    }

    fn quarantine_entries(&self) -> Vec<QuarantinedRecord> {
        self.quarantine
            .iter()
            .map(|(id, reason)| QuarantinedRecord {
                id: id.clone(),
                reason: *reason,
            })
            .collect()
    }

    fn unresolved_ids(&self) -> BTreeSet<OperationId> {
        self.records
            .values()
            .filter(|r| {
                !self.quarantine.contains_key(&r.operation.id)
                    && match &r.operation.body {
                        OperationBody::ConversationCreated { .. } => false,
                        OperationBody::TurnCompleted { .. } => !self.ready(&r.operation.id),
                        OperationBody::ConversationDeleted { conversation } => {
                            !self.records.contains_key(conversation)
                                && !self.retired.contains_key(conversation)
                        }
                    }
            })
            .map(|r| r.operation.id.clone())
            .collect()
    }

    fn resolve_graph(
        &mut self,
        incoming: &BTreeSet<OperationId>,
        formerly_pending: &BTreeSet<OperationId>,
    ) -> Result<(), DomainError> {
        loop {
            // New invalid input never gets a quarantine exception, even if another record
            // in this batch would expose an older invalid ancestor.
            for id in incoming {
                self.validate_record_graph(&self.records[id])?;
            }
            let mut dispositions = vec![];
            for record in self
                .records
                .values()
                .filter(|r| !self.quarantine.contains_key(&r.operation.id))
            {
                if let Err(error) = self.validate_record_graph(record) {
                    if !formerly_pending.contains(&record.operation.id) {
                        return Err(error);
                    }
                    dispositions.push((record.operation.id.clone(), quarantine_reason(&error)?));
                }
            }
            if dispositions.is_empty() {
                break;
            }
            self.quarantine.extend(dispositions);
        }
        // Dependency disposition can change a descendant's reason; normalize the entire
        // bounded ledger so restore and canonical serialization do not depend on scan order.
        let normalized: Result<BTreeMap<_, _>, _> = self
            .quarantine
            .keys()
            .map(|id| {
                let error = self
                    .validate_record_graph(&self.records[id])
                    .err()
                    .ok_or_else(|| DomainError::Invalid {
                        field: "quarantine_valid_record".into(),
                    })?;
                Ok((id.clone(), quarantine_reason(&error)?))
            })
            .collect();
        self.quarantine = normalized?;
        Ok(())
    }

    fn validate_quarantine(&self) -> Result<(), DomainError> {
        for (id, reason) in &self.quarantine {
            let error = self
                .validate_record_graph(&self.records[id])
                .err()
                .ok_or_else(|| DomainError::Invalid {
                    field: "quarantine_valid_record".into(),
                })?;
            if quarantine_reason(&error)? != *reason {
                return Err(DomainError::Invalid {
                    field: "quarantine_reason".into(),
                });
            }
        }
        Ok(())
    }

    fn valid_conversation_reference(&self, id: &OperationId) -> bool {
        !self
            .records
            .get(id)
            .is_some_and(|r| !matches!(r.operation.body, OperationBody::ConversationCreated { .. }))
            && self.retired.get(id).is_none_or(|r| r.id == r.conversation)
    }

    fn valid_context_reference(&self, id: &OperationId, conversation: &OperationId) -> bool {
        !self.quarantine.contains_key(id)
            && self.records.get(id).is_none_or(|record| {
                matches!(&record.operation.body,
                OperationBody::TurnCompleted { conversation: owner, .. } if owner == conversation)
            })
            && self.retired.get(id).is_none_or(|record| {
                &record.conversation == conversation && record.id != record.conversation
            })
    }

    /// Newest first. Missing tail remains in the result for deterministic pending projection.
    fn path(&self, head: &OperationId) -> Result<Vec<OperationId>, DomainError> {
        let mut result = vec![];
        let mut seen = BTreeSet::new();
        let mut current = Some(head.clone());
        let mut conversation = None;
        while let Some(id) = current {
            if &id != head && self.quarantine.contains_key(&id) {
                return Err(DomainError::InvalidReference { id });
            }
            if !seen.insert(id.clone()) {
                return Err(DomainError::Cycle { id });
            }
            if result.len() == MAX_ANCESTRY {
                return Err(limit("ancestry"));
            }
            result.push(id.clone());
            let Some(record) = self.records.get(&id) else {
                if self.retired.get(&id).is_some_and(|r| {
                    conversation
                        .as_ref()
                        .is_some_and(|owner| owner != &r.conversation)
                        || r.id == r.conversation
                }) {
                    return Err(DomainError::InvalidReference { id });
                }
                break;
            };
            let OperationBody::TurnCompleted {
                conversation: owner,
                parent,
                ..
            } = &record.operation.body
            else {
                return Err(DomainError::InvalidReference { id });
            };
            if conversation.as_ref().is_some_and(|c| c != owner) {
                return Err(DomainError::InvalidReference { id });
            }
            conversation = Some(owner.clone());
            current = parent.clone();
        }
        Ok(result)
    }

    fn path_complete(&self, path: &[OperationId]) -> bool {
        path.last().is_some_and(|id| {
            self.records.get(id).is_some_and(|r| {
                matches!(
                    r.operation.body,
                    OperationBody::TurnCompleted { parent: None, .. }
                )
            })
        })
    }

    fn ready(&self, id: &OperationId) -> bool {
        if self.quarantine.contains_key(id) {
            return false;
        }
        let Some(record) = self.records.get(id) else {
            return false;
        };
        if !self
            .records
            .get(record.operation.conversation())
            .is_some_and(|r| matches!(r.operation.body, OperationBody::ConversationCreated { .. }))
        {
            return false;
        }
        self.path(id).is_ok_and(|path| self.path_complete(&path))
    }

    pub fn history(
        &self,
        conversation: &OperationId,
        head: &OperationId,
    ) -> Result<Vec<Record>, DomainError> {
        conversation.validate()?;
        head.validate()?;
        if self.deleted().contains(conversation) {
            return Err(DomainError::Deleted {
                conversation: conversation.clone(),
            });
        }
        if self.quarantine.contains_key(head) {
            return Err(DomainError::Quarantined { id: head.clone() });
        }
        let record = self
            .records
            .get(head)
            .ok_or_else(|| DomainError::Missing { id: head.clone() })?;
        if record.operation.conversation() != conversation {
            return Err(DomainError::InvalidReference { id: head.clone() });
        }
        if !self.ready(head) {
            return Err(DomainError::Missing { id: head.clone() });
        }
        Ok(self
            .path(head)?
            .into_iter()
            .rev()
            .map(|id| self.records[&id].clone())
            .collect())
    }

    pub fn select_context(&self, request: ContextRequest) -> Result<ContextSelection, DomainError> {
        self.check_revision(&request.expected_revision)?;
        request.conversation.validate()?;
        if request.max_turns > MAX_CONTEXT_TURNS || request.max_text_bytes > MAX_CONTEXT_BYTES {
            return Err(limit("provider_context"));
        }
        if self.deleted().contains(&request.conversation) {
            return Err(DomainError::Deleted {
                conversation: request.conversation,
            });
        }
        let view = self
            .snapshot()
            .conversations
            .into_iter()
            .find(|c| c.id == request.conversation)
            .ok_or_else(|| DomainError::Missing {
                id: request.conversation.clone(),
            })?;
        if view.title.is_none() {
            return Err(DomainError::Missing {
                id: request.conversation,
            });
        }
        let parent = if let Some(head) = request.selected_head {
            head.validate()?;
            Some(head)
        } else {
            match view.heads.as_slice() {
                [] if view.pending.is_empty() => {
                    if let Some(id) = self.quarantine.keys().find(|id| {
                        self.records[*id].operation.conversation() == &request.conversation
                    }) {
                        return Err(DomainError::Quarantined { id: id.clone() });
                    }
                    None
                }
                [] => {
                    return Err(DomainError::Missing {
                        id: view.pending[0].clone(),
                    })
                }
                [head] => Some(head.clone()),
                _ => return Err(DomainError::AmbiguousHeads { heads: view.heads }),
            }
        };
        let history = match &parent {
            Some(head) => self.history(&request.conversation, head)?,
            None => vec![],
        };
        let mut turns = vec![];
        let mut bytes = 0;
        for record in history.into_iter().rev().take(request.max_turns) {
            let size = record.operation.text_bytes();
            if size > request.max_text_bytes - bytes {
                break;
            }
            bytes += size;
            turns.push(record);
        }
        turns.reverse();
        Ok(ContextSelection {
            revision: self.revision.clone(),
            conversation: request.conversation,
            parent,
            ancestor_ids: turns.iter().map(|r| r.operation.id.clone()).collect(),
            turns,
        })
    }

    pub fn continuation(
        &self,
        request: ContinuationRequest,
    ) -> Result<ContinuationDecision, DomainError> {
        self.check_revision(&request.expected_revision)?;
        request.conversation.validate()?;
        if self.deleted().contains(&request.conversation) {
            return Err(DomainError::Deleted {
                conversation: request.conversation,
            });
        }
        if let Some(head) = &request.selected_head {
            self.history(&request.conversation, head)?;
        }
        let view = self
            .snapshot()
            .conversations
            .into_iter()
            .find(|c| c.id == request.conversation)
            .ok_or_else(|| DomainError::Missing {
                id: request.conversation.clone(),
            })?;
        if view.title.is_none() {
            return Err(DomainError::Missing {
                id: request.conversation,
            });
        }
        if request.local_work_in_flight {
            return Ok(ContinuationDecision::Keep {
                selected_head: request.selected_head,
            });
        }
        let candidates: Vec<_> = view
            .heads
            .into_iter()
            .filter(|head| {
                request.selected_head.as_ref().is_none_or(|selected| {
                    self.path(head).is_ok_and(|path| path.contains(selected))
                })
            })
            .collect();
        match candidates.as_slice() {
            [head] if request.selected_head.as_ref() != Some(head) => {
                Ok(ContinuationDecision::FastForward { head: head.clone() })
            }
            [] | [_] => Ok(ContinuationDecision::Keep {
                selected_head: request.selected_head,
            }),
            _ => Ok(ContinuationDecision::Choose { heads: candidates }),
        }
    }

    pub fn missing_dependencies(&self) -> Vec<OperationId> {
        let mut missing = BTreeSet::new();
        for record in self.records.values() {
            if self.quarantine.contains_key(&record.operation.id) {
                continue;
            }
            if let OperationBody::TurnCompleted {
                conversation,
                parent,
                provenance,
                ..
            } = &record.operation.body
            {
                for id in std::iter::once(conversation)
                    .chain(parent.iter())
                    .chain(provenance.context.iter())
                {
                    if !self.records.contains_key(id) && !self.retired.contains_key(id) {
                        missing.insert(id.clone());
                    }
                }
            }
        }
        missing.into_iter().collect()
    }
}

fn title_preview(prompt: &str) -> String {
    const MAX_SCALARS: usize = 64;
    let mut characters = prompt
        .split_whitespace()
        .flat_map(|word| std::iter::once(' ').chain(word.chars()))
        .skip(1);
    let mut preview: String = characters.by_ref().take(MAX_SCALARS).collect();
    if characters.next().is_some() {
        preview.pop();
        preview.push('…');
    }
    // At most 64 Unicode scalars, hence at most 256 UTF-8 bytes including the ellipsis.
    preview
}

impl Operation {
    pub fn conversation(&self) -> &OperationId {
        match &self.body {
            OperationBody::ConversationCreated { .. } => &self.id,
            OperationBody::TurnCompleted { conversation, .. }
            | OperationBody::ConversationDeleted { conversation } => conversation,
        }
    }
    pub fn text_bytes(&self) -> usize {
        match &self.body {
            OperationBody::ConversationCreated { title } => title.len(),
            OperationBody::TurnCompleted { prompt, answer, .. } => {
                prompt.len() + answer.text().len()
            }
            OperationBody::ConversationDeleted { .. } => 0,
        }
    }
}

impl Record {
    pub fn validate(&self) -> Result<(), DomainError> {
        validate_body_key(&self.body_key)?;
        let op = &self.operation;
        if op.version != SCHEMA_VERSION {
            return Err(DomainError::UnsupportedVersion {
                version: op.version,
            });
        }
        op.id.validate()?;
        op.conversation().validate()?;
        match &op.body {
            OperationBody::ConversationCreated { title } => {
                bounded_text(title, "title", 256, false)?;
            }
            OperationBody::ConversationDeleted { conversation } => {
                if conversation == &op.id {
                    return Err(DomainError::InvalidReference { id: op.id.clone() });
                }
            }
            OperationBody::TurnCompleted {
                conversation,
                parent,
                request,
                prompt,
                answer,
                provenance,
                ..
            } => {
                if conversation == &op.id {
                    return Err(DomainError::InvalidReference { id: op.id.clone() });
                }
                if let Some(parent) = parent {
                    parent.validate()?;
                }
                request.validate()?;
                bounded_text(prompt, "prompt", MAX_PROMPT_BYTES, true)?;
                bounded_text(answer.text(), "answer", MAX_TURN_TEXT_BYTES, true)?;
                if op.text_bytes() > MAX_TURN_TEXT_BYTES {
                    return Err(limit("turn_text_bytes"));
                }
                for (field, text) in [
                    ("requested_model", &provenance.requested_model),
                    ("policy_id", &provenance.policy_id),
                    ("policy_version", &provenance.policy_version),
                    ("instruction_profile_id", &provenance.instruction_profile_id),
                    (
                        "instruction_profile_version",
                        &provenance.instruction_profile_version,
                    ),
                ] {
                    bounded_text(text, field, 256, true)?;
                }
                if let Some(model) = &provenance.returned_model {
                    bounded_text(model, "returned_model", 256, true)?;
                }
                if provenance.context.len() > MAX_CONTEXT_TURNS {
                    return Err(limit("provenance_context"));
                }
                for id in &provenance.context {
                    id.validate()?;
                }
            }
        }
        Ok(())
    }
}

fn bounded_text(text: &str, field: &str, max: usize, nonempty: bool) -> Result<(), DomainError> {
    if text.len() > max {
        return Err(limit(field));
    }
    if nonempty && text.trim().is_empty() {
        return Err(DomainError::Invalid {
            field: field.into(),
        });
    }
    Ok(())
}
fn limit(name: &str) -> DomainError {
    DomainError::LimitExceeded { limit: name.into() }
}

fn quarantine_reason(error: &DomainError) -> Result<QuarantineReason, DomainError> {
    match error {
        DomainError::InvalidReference { .. } => Ok(QuarantineReason::InvalidReference),
        DomainError::InvalidContext { .. } => Ok(QuarantineReason::InvalidContext),
        DomainError::Cycle { .. } => Ok(QuarantineReason::Cycle),
        DomainError::LimitExceeded { limit } if limit == "ancestry" => {
            Ok(QuarantineReason::AncestryLimit)
        }
        _ => Err(error.clone()),
    }
}

fn check_text_quota(mut sizes: impl Iterator<Item = usize>) -> Result<(), DomainError> {
    sizes
        .try_fold(0usize, |sum, size| {
            sum.checked_add(size)
                .filter(|total| *total <= MAX_STORE_TEXT_BYTES)
                .ok_or_else(|| limit("store_text_bytes"))
        })
        .map(|_| ())
}

#[cfg(test)]
mod tests;
