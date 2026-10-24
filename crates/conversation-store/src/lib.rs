//! Blocking, portable SQLite owner. Call from an existing background IO owner.
//! The reducer is pinned to the same database revision, never independently writable.
pub mod codec;
pub mod gateway;
pub mod replication;
pub mod timing;
use timing::{ConnectionTiming, StatementTiming, TransactionTiming};

use codec::{encode, invalid, limit};
use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::{Component, Path};
use voxgolem_core::conversation_sync::{receipts_after_commit, Receipt};
use voxgolem_core::durable_conversation::{
    Answer, Command, ContextRequest, ContextSelection, DomainError, MutationPlan, Operation,
    OperationBody, OperationId, Provenance, Record, Store as CoreStore, StoreSnapshot,
    TerminalStage, SCHEMA_VERSION,
};

pub const MAX_CANONICAL_BYTES: usize = 4 * 1024 * 1024;
const MAX_LOCAL_BYTES: usize = 4 * 1024 * 1024;
const MAX_OUTBOX_BYTES: usize = 8 * 1024 * 1024;
const MAX_LEDGER_ROWS: usize = 65_536;
const MAX_DATABASE_BYTES: usize = 64 * 1024 * 1024;
const MAX_WAL_BYTES: usize = 16 * 1024 * 1024;

#[derive(Debug)]
pub enum Error {
    Domain(DomainError),
    Database(rusqlite::ErrorCode),
    Storage,
    Closed,
}
impl From<DomainError> for Error {
    fn from(value: DomainError) -> Self {
        Self::Domain(value)
    }
}
impl From<rusqlite::Error> for Error {
    fn from(error: rusqlite::Error) -> Self {
        match error {
            rusqlite::Error::SqliteFailure(error, _) => Self::Database(error.code),
            _ => Self::Storage,
        }
    }
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Domain(_) => f.write_str("conversation_domain"),
            Self::Database(code) => write!(f, "conversation_sqlite_{code:?}"),
            Self::Storage => f.write_str("conversation_storage"),
            Self::Closed => f.write_str("conversation_closed"),
        }
    }
}
impl std::error::Error for Error {}
pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Clone, Serialize)]
pub struct CommitOutcome {
    pub base_revision: String,
    pub revision: String,
    pub accepted: Vec<OperationId>,
    pub duplicates: Vec<OperationId>,
    pub suppressed: Vec<OperationId>,
    pub quarantined: Vec<OperationId>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Selection {
    pub conversation: Option<OperationId>,
    pub selected_head: Option<OperationId>,
}
#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Draft {
    pub text: String,
    pub interrupted: bool,
}

pub struct TurnCompletion {
    pub request: OperationId,
    pub context: ContextSelection,
    pub prompt: String,
    pub answer: Answer,
    pub sources: Vec<voxgolem_core::sources::SourceLink>,
    pub terminal_stage: TerminalStage,
    pub provenance: Provenance,
}
#[derive(Serialize)]
pub struct CompletedTurn {
    pub record: Record,
    pub saved: bool,
}

/// Final execution authority, resolved after staging and before durable publication.
pub enum CompletionDecision {
    Authorized,
    Cancelled,
    Unavailable,
}

// A single synchronous callback value, never queued/retained in a collection.
// Keep the completed record inline rather than allocate solely for empty variants.
#[allow(clippy::large_enum_variant)]
pub enum CompletionAttempt {
    Cancelled,
    Unavailable,
    Finished(Result<CompletedTurn>),
}

struct StagedCompletion<'a> {
    tx: Transaction<'a>,
    next: CoreStore,
    completed: CompletedTurn,
}
pub struct PreparedContext {
    pub context: ContextSelection,
    pub created: Option<Record>,
}

#[derive(Serialize)]
pub struct PreparedRequestContext {
    pub context: ProviderContext,
    pub request: OperationId,
}

#[derive(Serialize)]
pub struct ConversationPage {
    pub revision: String,
    pub conversation_count: usize,
    pub conversation_offset: usize,
    pub next_offset: usize,
    pub conversations: Vec<gateway::ConversationMetadata>,
}
#[derive(Serialize)]
pub struct HeadPage {
    pub revision: String,
    pub conversation: OperationId,
    pub head_count: usize,
    pub head_offset: usize,
    pub next_offset: usize,
    pub heads: Vec<OperationId>,
    pub head_previews: Vec<gateway::HeadMetadata>,
}

#[derive(Serialize)]
pub struct ProviderMessage {
    pub role: &'static str,
    pub content: String,
    #[serde(rename = "contentType", skip_serializing_if = "Option::is_none")]
    pub content_type: Option<&'static str>,
}
#[derive(Serialize)]
pub struct ProviderContext {
    #[serde(flatten)]
    pub selection: ContextSelection,
    pub provider_messages: Vec<ProviderMessage>,
}
/// Derived text only. Canonical records retain the plain answer and validated link metadata.
pub fn provider_context(selection: ContextSelection) -> ProviderContext {
    let mut messages = Vec::with_capacity(selection.turns.len() * 2);
    for record in &selection.turns {
        if let OperationBody::TurnCompleted {
            prompt,
            answer,
            sources,
            ..
        } = &record.operation.body
        {
            messages.push(ProviderMessage {
                role: "user",
                content: prompt.clone(),
                content_type: None,
            });
            let refusal = matches!(answer, Answer::Refusal { .. });
            messages.push(ProviderMessage {
                role: "assistant",
                content: voxgolem_core::sources::with_source_footer(answer.text(), sources),
                content_type: Some(if refusal { "refusal" } else { "output_text" }),
            });
        }
    }
    ProviderContext {
        selection,
        provider_messages: messages,
    }
}

pub struct Store {
    db: Connection,
    core: CoreStore,
}

impl Store {
    /// Path is supplied by the trusted shell, never provider or peer input. Parent exists.
    pub fn open(path: &Path) -> Result<Self> {
        if !path.is_absolute()
            || path.components().any(|c| c == Component::ParentDir)
            || path.file_name().is_none()
            || std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink())
        {
            return Err(invalid("database_path").into());
        }
        let db = Connection::open(path)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
                .map_err(|_| Error::Storage)?;
        }
        db.busy_timeout(std::time::Duration::from_secs(2))?;
        db.timed_execute_batch(&format!("PRAGMA page_size=4096; PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA secure_delete=ON; PRAGMA foreign_keys=ON; PRAGMA journal_size_limit={MAX_WAL_BYTES}; PRAGMA wal_autocheckpoint=512;"))?;
        let page_size: usize =
            db.timed_query_row("PRAGMA page_size", [], |r| r.get::<_, u32>(0))? as usize;
        let pages: u32 = db.timed_query_row("PRAGMA page_count", [], |r| r.get(0))?;
        if (pages as usize).saturating_mul(page_size) > MAX_DATABASE_BYTES {
            return Err(limit("database_bytes").into());
        }
        timing::measure(timing::Phase::sqlite_write, || {
            db.pragma_update(
                None,
                "max_page_count",
                (MAX_DATABASE_BYTES / page_size) as u32,
            )
        })?;
        let mut db = db;
        bootstrap(&mut db)?;
        let tx = db.timed_transaction()?;
        identity(&tx)?;
        let core = load(&tx)?;
        tx.timed_commit()?;
        Ok(Self { db, core })
    }

    /// Revalidate only when another handle committed a different database revision.
    pub fn read<T>(
        &mut self,
        read: impl FnOnce(&CoreStore) -> std::result::Result<T, DomainError>,
    ) -> Result<T> {
        let tx = self.db.timed_transaction()?;
        refresh(&tx, &mut self.core)?;
        let result = timing::measure(timing::Phase::projection, || read(&self.core))?;
        tx.timed_commit()?;
        Ok(result)
    }
    pub fn snapshot(&mut self) -> Result<StoreSnapshot> {
        self.read(|core| Ok(core.export()))
    }
    /// Owned gateway times parsing, pure projection and encoding independently.
    /// Serialization remains inside the read transaction, before COMMIT.
    pub fn read_owned(&mut self, input: &str) -> Result<String> {
        let tx = self.db.timed_transaction()?;
        refresh(&tx, &mut self.core)?;
        let result = gateway::read_owned(&self.core, input)?;
        tx.timed_commit()?;
        Ok(result)
    }
    pub fn revision(&mut self) -> Result<String> {
        self.read(|core| Ok(core.revision().into()))
    }
    pub fn replica(&self) -> Result<String> {
        Ok(self.db.timed_query_row(
            "SELECT replica FROM history_state WHERE singleton=1",
            [],
            |r| bounded_text(r, 0, 36),
        )?)
    }

    pub fn allocate_id(&mut self) -> Result<OperationId> {
        let tx = self
            .db
            .timed_transaction_with_behavior(TransactionBehavior::Immediate)?;
        let id = allocate_id(&tx)?;
        tx.timed_commit()?;
        Ok(id)
    }

    pub fn create_conversation(&mut self, title: String) -> Result<Record> {
        let tx = self
            .db
            .timed_transaction_with_behavior(TransactionBehavior::Immediate)?;
        let id = allocate_id(&tx)?;
        let record = codec::record(Operation {
            version: SCHEMA_VERSION,
            id,
            body: OperationBody::ConversationCreated { title },
        })?;
        let (_, next) = Self::import_within_tx_with_hook(
            &mut self.core,
            &tx,
            vec![record.clone()],
            false,
            None,
            |_| Ok(()),
        )?;
        tx.timed_commit()?;
        self.core = next;
        Ok(record)
    }

    pub fn context(
        &mut self,
        conversation: OperationId,
        selected_head: Option<OperationId>,
        max_text_bytes: usize,
    ) -> Result<ContextSelection> {
        self.read(|core| {
            core.select_context(ContextRequest {
                expected_revision: core.revision().into(),
                conversation,
                selected_head,
                max_turns: 20,
                max_text_bytes,
            })
        })
    }

    /// Admission fence against the accepted local owner, not a stale global revision.
    /// Unrelated imports are allowed. Never fast-forward/reparent behind the caller's back.
    /// Fresh create, selected owner and admitted draft commit atomically. Call under the
    /// shell's admission/cancellation gate; no provider work is performed or replayed here.
    pub fn prepare_context(
        &mut self,
        expected: Selection,
        prompt: &str,
        max_text_bytes: usize,
    ) -> Result<PreparedContext> {
        self.prepare_context_inner(expected, prompt, max_text_bytes, false, false)
            .map(|(prepared, _)| prepared)
    }

    /// Atomically pin context, allocate request identity and persist the admitted draft.
    /// SQLite runs outside the shell admission monitor; its final ticket check decides
    /// whether to execute. A successful late-cancel preparation remains durable.
    /// `total_text_bytes` includes the prompt exactly once, not a remaining budget.
    pub fn prepare_request(
        &mut self,
        expected: Selection,
        prompt: &str,
        total_text_bytes: usize,
        strict_parent: bool,
    ) -> Result<PreparedRequestContext> {
        if total_text_bytes > 256 * 1024 || prompt.len() > total_text_bytes {
            return Err(limit("provider_context").into());
        }
        let (prepared, request) = self.prepare_context_inner(
            expected,
            prompt,
            total_text_bytes - prompt.len(),
            strict_parent,
            true,
        )?;
        Ok(PreparedRequestContext {
            context: provider_context(prepared.context),
            request: request.ok_or(Error::Storage)?,
        })
    }

    fn prepare_context_inner(
        &mut self,
        expected: Selection,
        prompt: &str,
        max_text_bytes: usize,
        strict_parent: bool,
        allocate_request: bool,
    ) -> Result<(PreparedContext, Option<OperationId>)> {
        if prompt.trim().is_empty() || prompt.len() > 32 * 1024 {
            return Err(limit("prompt").into());
        }
        if max_text_bytes > 256 * 1024 {
            return Err(limit("provider_context").into());
        }
        let max_text_bytes = max_text_bytes.min(256 * 1024 - prompt.len());
        let tx = self
            .db
            .timed_transaction_with_behavior(TransactionBehavior::Immediate)?;
        refresh(&tx, &mut self.core)?;
        let (conversation, head): (Option<String>, Option<String>) = tx.timed_query_row(
            "SELECT conversation,selected_head FROM history_local WHERE singleton=1",
            [],
            |r| Ok((optional_text(r, 0, 256)?, optional_text(r, 1, 256)?)),
        )?;
        if conversation != expected.conversation.as_ref().map(json_id).transpose()?
            || head != expected.selected_head.as_ref().map(json_id).transpose()?
        {
            return Err(invalid("selection_changed").into());
        }
        let mut created = None;
        let mut next = None;
        let conversation = if let Some(conversation) = expected.conversation {
            let continuation = self.core.continuation(
                voxgolem_core::durable_conversation::ContinuationRequest {
                    expected_revision: self.core.revision().into(),
                    conversation: conversation.clone(),
                    selected_head: expected.selected_head.clone(),
                    local_work_in_flight: false,
                },
            )?;
            match continuation {
                voxgolem_core::durable_conversation::ContinuationDecision::FastForward {
                    ..
                } => return Err(invalid("selection_changed").into()),
                voxgolem_core::durable_conversation::ContinuationDecision::Choose { heads } => {
                    return Err(DomainError::AmbiguousHeads { heads }.into())
                }
                _ => {}
            }
            conversation
        } else {
            if expected.selected_head.is_some() {
                return Err(invalid("selection_head").into());
            }
            let record = codec::record(Operation {
                version: SCHEMA_VERSION,
                id: allocate_id(&tx)?,
                body: OperationBody::ConversationCreated {
                    title: "Conversation".into(),
                },
            })?;
            let (plan, next_store) = timing::measure(timing::Phase::reducer, || {
                self.core.plan_with_store(Command::Import {
                    expected_revision: self.core.revision().into(),
                    records: vec![record.clone()],
                })
            })?;
            next = Some(next_store);
            apply_plan(&tx, &self.core, &plan)?;
            tx.timed_execute(
                "UPDATE history_local SET conversation=?,selected_head=NULL WHERE singleton=1",
                [json_id(&record.operation.id)?],
            )?;
            tx.timed_execute(
                "DELETE FROM history_drafts WHERE conversation_key='new' AND text=?",
                [prompt],
            )?;
            let id = record.operation.id.clone();
            created = Some(record);
            id
        };
        let core = next.as_ref().unwrap_or(&self.core);
        let expected_parent = expected.selected_head.clone();
        let context = timing::measure(timing::Phase::projection, || {
            core.select_context(ContextRequest {
                expected_revision: core.revision().into(),
                conversation: conversation.clone(),
                selected_head: expected.selected_head,
                max_turns: 20,
                max_text_bytes,
            })
        })?;
        if strict_parent && context.parent != expected_parent {
            return Err(invalid("selection_changed").into());
        }
        let request = if allocate_request {
            Some(allocate_id(&tx)?)
        } else {
            None
        };
        tx.timed_execute("INSERT INTO history_drafts(conversation_key,text,interrupted) VALUES(?,?,1) ON CONFLICT(conversation_key) DO UPDATE SET text=excluded.text,interrupted=1 WHERE text!=excluded.text OR interrupted!=1",params![key(&conversation),prompt])?;
        quotas(&tx)?;
        check_quota(
            &tx,
            "history_drafts",
            "length(CAST(text AS BLOB))",
            MAX_LOCAL_BYTES,
            MAX_LEDGER_ROWS,
        )?;
        tx.timed_commit()?;
        if let Some(next) = next {
            self.core = next;
        }
        Ok((PreparedContext { context, created }, request))
    }

    pub fn project(&mut self, request: gateway::ProjectionRequest) -> Result<gateway::Projection> {
        self.read(|core| gateway::project(core, request))
    }
    pub fn conversations(&mut self, offset: usize, count: usize) -> Result<ConversationPage> {
        self.read(|core| gateway::conversation_page(core.summary(), offset, count, None))
    }
    pub fn conversations_at(
        &mut self,
        expected_revision: &str,
        offset: usize,
        count: usize,
    ) -> Result<ConversationPage> {
        self.read(|core| {
            core.check_revision(expected_revision)?;
            gateway::conversation_page(core.summary(), offset, count, None)
        })
    }
    pub fn heads(
        &mut self,
        conversation: OperationId,
        offset: usize,
        count: usize,
    ) -> Result<HeadPage> {
        if count == 0 || count > 128 || offset > 16_384 {
            return Err(limit("head_page").into());
        }
        conversation.validate()?;
        self.read(|core| {
            let view = core.summary();
            let owner = view
                .conversations
                .into_iter()
                .find(|c| c.id == conversation)
                .ok_or_else(|| DomainError::Missing {
                    id: conversation.clone(),
                })?;
            if owner.deleted {
                return Err(DomainError::Deleted { conversation });
            }
            gateway::head_page_from_heads(
                core,
                view.revision,
                conversation,
                owner.heads,
                offset,
                count,
            )
        })
    }
    pub fn branches_at(
        &mut self,
        expected_revision: &str,
        conversation: OperationId,
        selected_head: Option<OperationId>,
        offset: usize,
        count: usize,
    ) -> Result<HeadPage> {
        if count == 0 || count > 128 || offset > 16_384 {
            return Err(limit("head_page").into());
        }
        self.read(|core| {
            core.check_revision(expected_revision)?;
            let decision =
                core.continuation(voxgolem_core::durable_conversation::ContinuationRequest {
                    expected_revision: expected_revision.into(),
                    conversation: conversation.clone(),
                    selected_head,
                    local_work_in_flight: false,
                })?;
            let all_heads = match decision {
                voxgolem_core::durable_conversation::ContinuationDecision::Choose { heads } => {
                    heads
                }
                _ => vec![],
            };
            gateway::head_page_from_heads(
                core,
                core.revision().into(),
                conversation,
                all_heads,
                offset,
                count,
            )
        })
    }
    pub fn history(&mut self, request: gateway::HistoryRequest) -> Result<gateway::HistoryPage> {
        self.read(|core| gateway::history_page(core, request))
    }
    pub fn delete_conversation(&mut self, conversation: OperationId) -> Result<CommitOutcome> {
        let tx = self
            .db
            .timed_transaction_with_behavior(TransactionBehavior::Immediate)?;
        let id = allocate_id(&tx)?;
        let record = codec::record(Operation {
            version: SCHEMA_VERSION,
            id,
            body: OperationBody::ConversationDeleted { conversation },
        })?;
        let (outcome, next) = Self::import_within_tx_with_hook(
            &mut self.core,
            &tx,
            vec![record],
            false,
            None,
            |_| Ok(()),
        )?;
        tx.timed_commit()?;
        self.core = next;
        Ok(outcome)
    }
    pub fn complete_turn(&mut self, mut completion: TurnCompletion) -> Result<CompletedTurn> {
        completion.provenance.context = completion.context.ancestor_ids;
        self.complete_body(OperationBody::TurnCompleted {
            conversation: completion.context.conversation,
            parent: completion.context.parent,
            request: completion.request,
            prompt: completion.prompt,
            answer: completion.answer,
            sources: completion.sources,
            terminal_stage: completion.terminal_stage,
            provenance: completion.provenance,
        })
    }

    /// Trusted thin adapters may supply the same immutable pinned body directly, avoiding
    /// an extra hash/record JNI round-trip. Only the shared codec/reducer validate it.
    pub fn complete_body(&mut self, body: OperationBody) -> Result<CompletedTurn> {
        self.complete_body_with_authority(
            body,
            || ((), CompletionDecision::Authorized),
            |_, attempt| match attempt {
                CompletionAttempt::Finished(result) => result,
                _ => unreachable!("unit authority always authorizes"),
            },
        )
    }

    /// Stage first, then retain final authority across SQLite COMMIT, core publication
    /// and terminal winner bookkeeping, all on this synchronous worker. Neither callback
    /// may call the store or acquire a lock whose owner waits for this store. Do not move
    /// guards across async boundaries. Unavailable authority never permits a commit.
    pub fn complete_body_with_authority<G, R>(
        &mut self,
        body: OperationBody,
        acquire: impl FnOnce() -> (G, CompletionDecision),
        finish: impl FnOnce(&mut G, CompletionAttempt) -> R,
    ) -> R {
        self.complete_body_guarded(body, false, acquire, finish)
    }

    /// Desktop local completion also advances its still-matching selected parent,
    /// atomically with the turn. Other adapters retain explicit selection ownership.
    /// A changed selection is never overwritten or used to reparent the completion.
    pub fn complete_selected_body_with_authority<G, R>(
        &mut self,
        body: OperationBody,
        acquire: impl FnOnce() -> (G, CompletionDecision),
        finish: impl FnOnce(&mut G, CompletionAttempt) -> R,
    ) -> R {
        self.complete_body_guarded(body, true, acquire, finish)
    }

    fn complete_body_guarded<G, R>(
        &mut self,
        body: OperationBody,
        advance_selection: bool,
        acquire: impl FnOnce() -> (G, CompletionDecision),
        finish: impl FnOnce(&mut G, CompletionAttempt) -> R,
    ) -> R {
        let staged = Self::stage_completion(&mut self.db, &mut self.core, body, advance_selection);
        let (mut guard, decision) = acquire();
        let attempt = match decision {
            CompletionDecision::Cancelled => {
                drop(staged);
                CompletionAttempt::Cancelled
            }
            CompletionDecision::Unavailable => {
                drop(staged);
                CompletionAttempt::Unavailable
            }
            CompletionDecision::Authorized => {
                CompletionAttempt::Finished(staged.and_then(|staged| {
                    staged.tx.timed_commit()?;
                    self.core = staged.next;
                    Ok(staged.completed)
                }))
            }
        };
        finish(&mut guard, attempt)
    }

    fn stage_completion<'a>(
        db: &'a mut Connection,
        core: &mut CoreStore,
        body: OperationBody,
        advance_selection: bool,
    ) -> Result<StagedCompletion<'a>> {
        if !matches!(body, OperationBody::TurnCompleted { .. }) {
            return Err(invalid("completion_body").into());
        }
        let tx = db.timed_transaction_with_behavior(TransactionBehavior::Immediate)?;
        let id = allocate_id(&tx)?;
        let record = codec::record(Operation {
            version: SCHEMA_VERSION,
            id,
            body,
        })?;
        let (outcome, next) = Self::import_within_tx_with_hook(
            core,
            &tx,
            vec![record.clone()],
            false,
            Some(&record.operation.id),
            |_| Ok(()),
        )?;
        let saved = !outcome.suppressed.contains(&record.operation.id)
            && !outcome.quarantined.contains(&record.operation.id);
        if advance_selection && saved {
            if let OperationBody::TurnCompleted {
                conversation,
                parent,
                ..
            } = &record.operation.body
            {
                tx.timed_execute("UPDATE history_local SET selected_head=? WHERE singleton=1 AND conversation=? AND selected_head IS ?", params![json_id(&record.operation.id)?, json_id(conversation)?, parent.as_ref().map(json_id).transpose()?])?;
            }
        }
        Ok(StagedCompletion {
            tx,
            next,
            completed: CompletedTurn { record, saved },
        })
    }
    /// Commit an immutable completion constructed from its pre-inference ContextSelection.
    /// The caller's cancellation/admission lock must cover this entire call, including COMMIT.
    /// A revision change reloads the DB and replans the same records; inference is never rerun.
    pub fn import_records(
        &mut self,
        records: Vec<Record>,
        receive: bool,
        completion: Option<&OperationId>,
    ) -> Result<CommitOutcome> {
        self.import_with_hook(records, receive, completion, || Ok(()))
    }

    fn import_with_hook(
        &mut self,
        records: Vec<Record>,
        receive: bool,
        completion: Option<&OperationId>,
        before_commit: impl FnOnce() -> Result<()>,
    ) -> Result<CommitOutcome> {
        let tx = self
            .db
            .timed_transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (outcome, next) = Self::import_within_tx_with_hook(
            &mut self.core,
            &tx,
            records,
            receive,
            completion,
            |_| before_commit(),
        )?;
        tx.timed_commit()?;
        self.core = next; // Only advance the pinned reducer after SQLite COMMIT succeeds.
        Ok(outcome)
    }

    pub(crate) fn import_within_tx_with_hook(
        core: &mut CoreStore,
        tx: &Transaction<'_>,
        records: Vec<Record>,
        receive: bool,
        completion: Option<&OperationId>,
        before_commit: impl FnOnce(&Transaction<'_>) -> Result<()>,
    ) -> Result<(CommitOutcome, CoreStore)> {
        if records.len() > 128 {
            return Err(limit("batch_records").into());
        }
        for record in &records {
            if codec::record(record.operation.clone())? != *record {
                return Err(invalid("body_key_sha256_mismatch").into());
            }
        }
        refresh(tx, core)?;
        if let Some(completion_id) = completion {
            let completed = records
                .iter()
                .find(|r| &r.operation.id == completion_id)
                .ok_or_else(|| invalid("completion_record"))?;
            let conversation = completed.operation.conversation();
            if !core
                .retained_records()
                .any(|r| r.operation.conversation() == conversation)
                && !core
                    .retired_records()
                    .any(|r| &r.conversation == conversation)
            {
                return Err(DomainError::Missing {
                    id: conversation.clone(),
                }
                .into());
            }
        }
        let ids: Vec<_> = records.iter().map(|r| r.operation.id.clone()).collect();
        let completion_body = completion
            .and_then(|id| records.iter().find(|r| &r.operation.id == id))
            .map(|r| &r.operation.body);
        let (plan, next) = timing::measure(timing::Phase::reducer, || {
            core.plan_with_store(Command::Import {
                expected_revision: core.revision().into(),
                records: records.clone(),
            })
        })?;
        apply_plan(tx, core, &plan)?;
        if receive {
            let replica: String = tx.timed_query_row(
                "SELECT replica FROM history_state WHERE singleton=1",
                [],
                |r| bounded_text(r, 0, 36),
            )?;
            for receipt in receipts_after_commit(&next, &replica, &ids)? {
                tx.timed_prepare_cached("INSERT OR IGNORE INTO history_receipt_outbox(operation_key,body_key,payload) VALUES(?,?,?)")?.timed_execute(params![key(&receipt.operation),receipt.body_key,encode(&receipt, 32*1024,"receipt_bytes")?])?;
            }
        }
        if let (
            Some(id),
            Some(OperationBody::TurnCompleted {
                conversation,
                prompt,
                ..
            }),
        ) = (completion, completion_body)
        {
            if !plan.suppressed.contains(id) && !plan.quarantined.contains(id) {
                tx.timed_execute(
                    "DELETE FROM history_drafts WHERE conversation_key=? AND text=?",
                    params![key(conversation), prompt],
                )?;
            }
        }
        quotas(tx)?;
        before_commit(tx)?;
        Ok((
            CommitOutcome {
                base_revision: plan.base_revision,
                revision: plan.snapshot.revision,
                accepted: plan.accepted,
                duplicates: plan.duplicates,
                suppressed: plan.suppressed,
                quarantined: plan.quarantined,
            },
            next,
        ))
    }

    pub fn select(&mut self, selection: Selection) -> Result<()> {
        let tx = self
            .db
            .timed_transaction_with_behavior(TransactionBehavior::Immediate)?;
        refresh(&tx, &mut self.core)?;
        if let Some(conversation) = &selection.conversation {
            self.core
                .continuation(voxgolem_core::durable_conversation::ContinuationRequest {
                    expected_revision: self.core.revision().into(),
                    conversation: conversation.clone(),
                    selected_head: selection.selected_head.clone(),
                    local_work_in_flight: true,
                })?;
        } else if selection.selected_head.is_some() {
            return Err(invalid("selection_head").into());
        }
        tx.timed_execute(
            "UPDATE history_local SET conversation=?,selected_head=? WHERE singleton=1",
            params![
                selection.conversation.as_ref().map(json_id).transpose()?,
                selection.selected_head.as_ref().map(json_id).transpose()?
            ],
        )?;
        tx.timed_commit()?;
        Ok(())
    }
    pub fn selection(&self) -> Result<Selection> {
        let (conversation, selected_head): (Option<String>, Option<String>) =
            self.db.timed_query_row(
                "SELECT conversation,selected_head FROM history_local WHERE singleton=1",
                [],
                |r| Ok((optional_text(r, 0, 256)?, optional_text(r, 1, 256)?)),
            )?;
        Ok(Selection {
            conversation: conversation
                .map(|s| serde_json::from_str(&s).map_err(|_| Error::Storage))
                .transpose()?,
            selected_head: selected_head
                .map(|s| serde_json::from_str(&s).map_err(|_| Error::Storage))
                .transpose()?,
        })
    }
    pub fn projection_still_current(
        &mut self,
        revision: &str,
        selection: &Selection,
    ) -> Result<bool> {
        let tx = self.db.timed_transaction()?;
        let actual = revision_of(&tx)?;
        let (conversation, head): (Option<String>, Option<String>) = tx.timed_query_row(
            "SELECT conversation,selected_head FROM history_local WHERE singleton=1",
            [],
            |r| Ok((optional_text(r, 0, 256)?, optional_text(r, 1, 256)?)),
        )?;
        let same = actual == revision
            && conversation == selection.conversation.as_ref().map(json_id).transpose()?
            && head == selection.selected_head.as_ref().map(json_id).transpose()?;
        tx.timed_commit()?;
        Ok(same)
    }

    pub fn save_draft(
        &mut self,
        conversation: Option<&OperationId>,
        text: &str,
        interrupted: bool,
    ) -> Result<()> {
        if text.len() > 32 * 1024 {
            return Err(limit("draft_bytes").into());
        }
        let tx = self
            .db
            .timed_transaction_with_behavior(TransactionBehavior::Immediate)?;
        refresh(&tx, &mut self.core)?;
        if let Some(conversation) = conversation {
            conversation.validate()?;
            // Branch ambiguity must not prevent a pinned conversation draft.
            let deleted: bool = tx.timed_query_row("SELECT EXISTS(SELECT 1 FROM history_records WHERE conversation_key=? AND tombstone=1 AND operation_key NOT IN (SELECT operation_key FROM history_quarantine))", [key(conversation)], |r| r.get(0))?;
            if deleted {
                return Err(DomainError::Deleted {
                    conversation: conversation.clone(),
                }
                .into());
            }
        }
        tx.timed_prepare_cached("INSERT INTO history_drafts(conversation_key,text,interrupted) VALUES(?,?,?) ON CONFLICT(conversation_key) DO UPDATE SET text=excluded.text,interrupted=excluded.interrupted WHERE text!=excluded.text OR interrupted!=excluded.interrupted")?.timed_execute(params![draft_key(conversation),text,interrupted])?;
        check_quota(
            &tx,
            "history_drafts",
            "length(CAST(text AS BLOB))",
            MAX_LOCAL_BYTES,
            MAX_LEDGER_ROWS,
        )?;
        tx.timed_commit()?;
        Ok(())
    }
    pub fn draft(&self, conversation: Option<&OperationId>) -> Result<Draft> {
        Ok(self
            .db
            .timed_query_row(
                "SELECT text,interrupted FROM history_drafts WHERE conversation_key=?",
                [draft_key(conversation)],
                |r| {
                    Ok(Draft {
                        text: bounded_text(r, 0, 32 * 1024)?,
                        interrupted: r.get(1)?,
                    })
                },
            )
            .optional()?
            .unwrap_or(Draft {
                text: String::new(),
                interrupted: false,
            }))
    }

    pub fn publications(
        &self,
        peer: Option<&str>,
        count: usize,
        max_bytes: usize,
        after: Option<&OperationId>,
    ) -> Result<Vec<Record>> {
        if count == 0 || count > 128 || max_bytes == 0 || max_bytes > MAX_CANONICAL_BYTES {
            return Err(limit("publication_page").into());
        }
        if let Some(peer) = peer {
            voxgolem_core::durable_conversation::validate_replica(peer)?;
        }
        if let Some(after) = after {
            after.validate()?;
        }
        let mut stmt = self.db.timed_prepare_cached("SELECT o.payload FROM history_outbox o WHERE o.operation_key>? AND (? IS NULL OR NOT EXISTS(SELECT 1 FROM history_peer_receipts r WHERE r.peer=? AND r.operation_key=o.operation_key AND r.body_key=o.body_key)) ORDER BY o.operation_key LIMIT ?")?;
        let rows = stmt.timed_query_map(
            params![after.map(key).unwrap_or_default(), peer, peer, count as i64],
            |r| bounded_text(r, 0, MAX_CANONICAL_BYTES),
        )?;
        bounded_rows(rows, max_bytes, |s| {
            codec::parse_record(&s).map_err(Error::from)
        })
    }
    pub fn receipts_to_publish(
        &self,
        count: usize,
        after: Option<&OperationId>,
    ) -> Result<Vec<Receipt>> {
        if count == 0 || count > 128 {
            return Err(limit("receipt_page").into());
        }
        if let Some(after) = after {
            after.validate()?;
        }
        let mut stmt = self.db.timed_prepare_cached("SELECT payload FROM history_receipt_outbox WHERE operation_key>? ORDER BY operation_key LIMIT ?")?;
        let rows = stmt.timed_query_map(
            params![after.map(key).unwrap_or_default(), count as i64],
            |r| bounded_text(r, 0, 32 * 1024),
        )?;
        bounded_rows(rows, 32 * 1024, |s| {
            serde_json::from_str(&s).map_err(|_| Error::Storage)
        })
    }
    pub fn acknowledge(&mut self, peer: &str, receipts: Vec<Receipt>) -> Result<()> {
        if receipts.len() > 128 || encode(&receipts, 32 * 1024, "receipt_bytes").is_err() {
            return Err(limit("receipt_bytes").into());
        }
        let tx = self
            .db
            .timed_transaction_with_behavior(TransactionBehavior::Immediate)?;
        refresh(&tx, &mut self.core)?;
        voxgolem_core::conversation_sync::merge_receipts(
            &self.core,
            peer,
            vec![],
            receipts.clone(),
        )?;
        for receipt in receipts {
            tx.timed_prepare_cached("INSERT OR IGNORE INTO history_peer_receipts(peer,operation_key,body_key) VALUES(?,?,?)")?.timed_execute(params![peer,key(&receipt.operation),receipt.body_key])?;
        }
        check_quota(&tx, "history_peer_receipts", "0", 0, MAX_LEDGER_ROWS)?;
        tx.timed_commit()?;
        Ok(())
    }

    /// Explicit idle/close maintenance, never called by admission, streaming or cancellation.
    pub fn checkpoint(&self) -> Result<()> {
        let busy: i64 = self
            .db
            .timed_query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| row.get(0))?;
        if busy != 0 {
            return Err(Error::Database(rusqlite::ErrorCode::DatabaseBusy));
        }
        Ok(())
    }

    pub fn close(self) -> Result<()> {
        self.db.close().map_err(|(_, error)| error.into())
    }
}

fn key(id: &OperationId) -> String {
    format!("{}/{}", id.replica, id.sequence)
}

fn bounded_text(row: &rusqlite::Row<'_>, column: usize, max: usize) -> rusqlite::Result<String> {
    let rusqlite::types::ValueRef::Text(bytes) = row.get_ref(column)? else {
        return Err(rusqlite::Error::InvalidQuery);
    };
    if bytes.len() > max {
        return Err(rusqlite::Error::InvalidQuery);
    }
    std::str::from_utf8(bytes)
        .map(str::to_owned)
        .map_err(|_| rusqlite::Error::InvalidQuery)
}
fn optional_text(
    row: &rusqlite::Row<'_>,
    column: usize,
    max: usize,
) -> rusqlite::Result<Option<String>> {
    if matches!(row.get_ref(column)?, rusqlite::types::ValueRef::Null) {
        Ok(None)
    } else {
        bounded_text(row, column, max).map(Some)
    }
}
fn json_id(id: &OperationId) -> std::result::Result<String, DomainError> {
    id.validate()?;
    encode(id, 256, "operation_id_bytes")
}
fn draft_key(id: Option<&OperationId>) -> String {
    id.map(key).unwrap_or_else(|| "new".into())
}
fn revision_of(tx: &Transaction<'_>) -> Result<String> {
    Ok(tx.timed_query_row(
        "SELECT revision FROM history_state WHERE singleton=1",
        [],
        |r| bounded_text(r, 0, 20),
    )?)
}
fn allocate_id(tx: &Transaction<'_>) -> Result<OperationId> {
    let (replica, sequence) = identity(tx)?;
    let next = sequence
        .checked_add(1)
        .ok_or_else(|| limit("sequence_u64"))?;
    tx.timed_execute(
        "UPDATE history_state SET last_sequence=? WHERE singleton=1",
        [next.to_string()],
    )?;
    Ok(OperationId {
        replica,
        sequence: next.to_string(),
    })
}
fn identity(tx: &Transaction<'_>) -> Result<(String, u64)> {
    let (replica, sequence): (String, String) = tx.timed_query_row(
        "SELECT replica,last_sequence FROM history_state WHERE singleton=1",
        [],
        |r| Ok((bounded_text(r, 0, 36)?, bounded_text(r, 1, 20)?)),
    )?;
    voxgolem_core::durable_conversation::validate_replica(&replica)?;
    let value = sequence
        .parse::<u64>()
        .ok()
        .filter(|n| n.to_string() == sequence)
        .ok_or_else(|| limit("sequence_u64"))?;
    Ok((replica, value))
}
fn refresh(tx: &Transaction<'_>, core: &mut CoreStore) -> Result<()> {
    if revision_of(tx)? != core.revision() {
        let _reload = timing::span(timing::Phase::refresh_reload);
        *core = load(tx)?;
    }
    Ok(())
}

fn bounded_rows<T>(
    rows: impl Iterator<Item = rusqlite::Result<String>>,
    max_bytes: usize,
    parse: impl Fn(String) -> Result<T>,
) -> Result<Vec<T>> {
    let mut result = Vec::new();
    let mut bytes = 2;
    for raw in rows {
        let raw = raw?;
        let size = raw.len() + usize::from(!result.is_empty());
        if size > max_bytes.saturating_sub(bytes) {
            if result.is_empty() {
                return Err(limit("publication_bytes").into());
            }
            break;
        }
        bytes += size;
        result.push(parse(raw)?);
    }
    Ok(result)
}

fn apply_plan(tx: &Transaction<'_>, old: &CoreStore, plan: &MutationPlan) -> Result<()> {
    for record in old.retained_records() {
        if plan
            .snapshot
            .retired
            .binary_search_by(|r| r.id.cmp(&record.operation.id))
            .is_ok()
        {
            tx.timed_prepare_cached("DELETE FROM history_records WHERE operation_key=?")?
                .timed_execute([key(&record.operation.id)])?;
        }
    }
    for record in &plan.snapshot.records {
        if !plan.accepted.contains(&record.operation.id) {
            continue;
        }
        let payload = encode(record, MAX_CANONICAL_BYTES, "record_bytes")?;
        tx.timed_prepare_cached("INSERT OR IGNORE INTO history_records(operation_key,conversation_key,body_key,tombstone,payload) VALUES(?,?,?,?,?)")?.timed_execute(params![key(&record.operation.id),key(record.operation.conversation()),record.body_key,matches!(record.operation.body,OperationBody::ConversationDeleted{..}),payload])?;
    }
    let previously_retired: BTreeSet<_> = old.retired_records().map(|r| &r.id).collect();
    for retired in plan
        .snapshot
        .retired
        .iter()
        .filter(|r| !previously_retired.contains(&r.id))
    {
        tx.timed_prepare_cached(
            "INSERT OR IGNORE INTO history_retired(operation_key,payload) VALUES(?,?)",
        )?
        .timed_execute(params![
            key(&retired.id),
            encode(retired, 1024, "retired_bytes")?
        ])?;
    }
    // Bounded disposition ledger; only changed entries are written, never clear/reinsert.
    tx.timed_execute("DELETE FROM history_quarantine WHERE operation_key NOT IN (SELECT operation_key FROM history_records)",[])?;
    for entry in &plan.snapshot.quarantine {
        tx.timed_prepare_cached("INSERT INTO history_quarantine(operation_key,payload) VALUES(?,?) ON CONFLICT(operation_key) DO UPDATE SET payload=excluded.payload WHERE payload!=excluded.payload")?.timed_execute(params![key(&entry.id),encode(entry,1024,"quarantine_bytes")?])?;
    }
    for conversation in &plan.purge_conversations {
        let key = key(conversation);
        tx.timed_execute(
            "DELETE FROM history_outbox WHERE conversation_key=? AND tombstone=0",
            [&key],
        )?;
        tx.timed_execute(
            "DELETE FROM history_drafts WHERE conversation_key=?",
            [&key],
        )?;
        tx.timed_execute("UPDATE history_local SET conversation=NULL,selected_head=NULL WHERE singleton=1 AND conversation=?",[json_id(conversation)?])?;
    }
    for publication in &plan.publish {
        if plan.quarantined.contains(&publication.operation.id) {
            continue;
        }
        tx.timed_prepare_cached("INSERT OR IGNORE INTO history_outbox(operation_key,conversation_key,body_key,tombstone,payload) VALUES(?,?,?,?,?)")?.timed_execute(params![key(&publication.operation.id),key(publication.operation.conversation()),publication.body_key,matches!(publication.operation.body,OperationBody::ConversationDeleted{..}),encode(publication,MAX_CANONICAL_BYTES,"record_bytes")?])?;
    }
    for id in &plan.quarantined {
        tx.timed_execute(
            "DELETE FROM history_outbox WHERE operation_key=?",
            [key(id)],
        )?;
    }
    tx.timed_execute(
        "UPDATE history_state SET revision=? WHERE singleton=1",
        [&plan.snapshot.revision],
    )?;
    Ok(())
}

fn check_quota(
    tx: &Transaction<'_>,
    table: &str,
    bytes: &str,
    max_bytes: usize,
    max_rows: usize,
) -> Result<()> {
    let _quota = timing::span(timing::Phase::quota_check);
    let (rows, size): (i64, i64) = tx.timed_query_row(
        &format!("SELECT COUNT(*),COALESCE(SUM({bytes}),0) FROM {table}"),
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    if rows > max_rows as i64 || size > max_bytes as i64 {
        return Err(limit("storage_quota").into());
    }
    Ok(())
}
fn quotas(tx: &Transaction<'_>) -> Result<()> {
    let _quota = timing::span(timing::Phase::quota_check);
    // Exact compact StoreSnapshot encoded size, including commas and fixed field overhead.
    let revision = revision_of(tx)?;
    let empty = StoreSnapshot {
        revision,
        ..StoreSnapshot::empty()
    };
    let mut size = encode(&empty, 1024, "snapshot_bytes")?.len();
    for (table, max_rows) in [
        ("history_records", 16_384),
        ("history_retired", 65_536),
        ("history_quarantine", 16_384),
    ] {
        let (rows, bytes): (i64, i64) = tx.timed_query_row(
            &format!("SELECT COUNT(*),COALESCE(SUM(length(CAST(payload AS BLOB))),0) FROM {table}"),
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let (rows, bytes) = (
            usize::try_from(rows).map_err(|_| Error::Storage)?,
            usize::try_from(bytes).map_err(|_| Error::Storage)?,
        );
        if rows > max_rows {
            return Err(limit("storage_rows").into());
        }
        size = size
            .checked_add(bytes + rows.saturating_sub(1))
            .ok_or_else(|| limit("snapshot_bytes"))?;
    }
    if size > MAX_CANONICAL_BYTES {
        return Err(limit("native_snapshot_bytes").into());
    }
    check_quota(
        tx,
        "history_outbox",
        "length(CAST(payload AS BLOB))",
        MAX_OUTBOX_BYTES,
        MAX_LEDGER_ROWS,
    )?;
    check_quota(
        tx,
        "history_receipt_outbox",
        "length(CAST(payload AS BLOB))",
        MAX_OUTBOX_BYTES,
        MAX_LEDGER_ROWS,
    )?;
    Ok(())
}

fn load(tx: &Transaction<'_>) -> Result<CoreStore> {
    quotas(tx)?;
    let mut snapshot = StoreSnapshot {
        revision: revision_of(tx)?,
        ..StoreSnapshot::empty()
    };
    for raw in tx
        .timed_prepare("SELECT payload FROM history_records ORDER BY operation_key")?
        .timed_query_map([], |r| bounded_text(r, 0, MAX_CANONICAL_BYTES))?
    {
        snapshot.records.push(codec::parse_record(&raw?)?);
    }
    for raw in tx
        .timed_prepare("SELECT payload FROM history_retired ORDER BY operation_key")?
        .timed_query_map([], |r| bounded_text(r, 0, 1024))?
    {
        snapshot
            .retired
            .push(timing::measure(timing::Phase::record_decode, || {
                serde_json::from_str(&raw?).map_err(|_| Error::Storage)
            })?);
    }
    for raw in tx
        .timed_prepare("SELECT payload FROM history_quarantine ORDER BY operation_key")?
        .timed_query_map([], |r| bounded_text(r, 0, 1024))?
    {
        snapshot
            .quarantine
            .push(timing::measure(timing::Phase::record_decode, || {
                serde_json::from_str(&raw?).map_err(|_| Error::Storage)
            })?);
    }
    Ok(timing::measure(timing::Phase::reducer, || {
        CoreStore::restore(snapshot)
    })?)
}

fn bootstrap(db: &mut Connection) -> Result<()> {
    let tx = db.timed_transaction_with_behavior(TransactionBehavior::Immediate)?;
    let epoch: u32 = tx.timed_query_row("PRAGMA user_version", [], |r| r.get(0))?;
    let has_new: bool = tx.timed_query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='history_state')",
        [],
        |r| r.get(0),
    )?;
    if has_new {
        if epoch != SCHEMA_VERSION {
            return Err(DomainError::UnsupportedVersion { version: epoch }.into());
        }
        tx.timed_commit()?;
        return Ok(());
    }
    if epoch > 1 {
        return Err(DomainError::UnsupportedVersion { version: epoch }.into());
    }
    let has_legacy: bool = tx.timed_query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='state')",
        [],
        |r| r.get(0),
    )?;
    // Preserve administrative identity/counter without parsing or importing any legacy body.
    let identity: Option<(String, String)> = if has_legacy {
        Some(tx.timed_query_row(
            "SELECT replica,last_sequence FROM state WHERE singleton=1",
            [],
            |r| Ok((bounded_text(r, 0, 36)?, bounded_text(r, 1, 20)?)),
        )?)
    } else {
        None
    };
    let (replica, sequence) = match identity {
        Some(identity) => identity,
        None => {
            let mut bytes: Vec<u8> =
                tx.timed_query_row("SELECT randomblob(16)", [], |r| r.get(0))?;
            bytes[6] = (bytes[6] & 0x0f) | 0x40;
            bytes[8] = (bytes[8] & 0x3f) | 0x80;
            let hex = bytes.iter().map(|b| format!("{b:02x}")).collect::<String>();
            (
                format!(
                    "{}-{}-{}-{}-{}",
                    &hex[..8],
                    &hex[8..12],
                    &hex[12..16],
                    &hex[16..20],
                    &hex[20..]
                ),
                "0".into(),
            )
        }
    };
    voxgolem_core::durable_conversation::validate_replica(&replica)?;
    if sequence
        .parse::<u64>()
        .ok()
        .is_none_or(|n| n.to_string() != sequence)
    {
        return Err(invalid("sequence_u64").into());
    }
    // History-only alpha reset. Credentials/settings/pairing/capture data are not owned here.
    if has_legacy {
        for table in [
            "state",
            "operations",
            "outbox",
            "receipt_outbox",
            "peer_receipts",
            "local_state",
            "drafts",
        ] {
            tx.timed_execute_batch(&format!("DROP TABLE IF EXISTS {table}"))?;
        }
    }
    tx.timed_execute_batch("CREATE TABLE history_state(singleton INTEGER PRIMARY KEY CHECK(singleton=1),replica TEXT NOT NULL,last_sequence TEXT NOT NULL,revision TEXT NOT NULL);
        CREATE TABLE history_records(operation_key TEXT PRIMARY KEY,conversation_key TEXT NOT NULL,body_key TEXT NOT NULL,tombstone INTEGER NOT NULL,payload TEXT NOT NULL);
        CREATE INDEX history_records_conversation ON history_records(conversation_key,tombstone);
        CREATE TABLE history_retired(operation_key TEXT PRIMARY KEY,payload TEXT NOT NULL);
        CREATE TABLE history_quarantine(operation_key TEXT PRIMARY KEY,payload TEXT NOT NULL);
        CREATE TABLE history_outbox(operation_key TEXT PRIMARY KEY,conversation_key TEXT NOT NULL,body_key TEXT NOT NULL,tombstone INTEGER NOT NULL,payload TEXT NOT NULL);
        CREATE INDEX history_outbox_conversation ON history_outbox(conversation_key);
        CREATE TABLE history_receipt_outbox(operation_key TEXT PRIMARY KEY,body_key TEXT NOT NULL,payload TEXT NOT NULL);
        CREATE TABLE history_peer_receipts(peer TEXT NOT NULL,operation_key TEXT NOT NULL,body_key TEXT NOT NULL,PRIMARY KEY(peer,operation_key));
        CREATE TABLE history_local(singleton INTEGER PRIMARY KEY CHECK(singleton=1),conversation TEXT,selected_head TEXT);
        CREATE TABLE history_drafts(conversation_key TEXT PRIMARY KEY,text TEXT NOT NULL,interrupted INTEGER NOT NULL);
        INSERT INTO history_local VALUES(1,NULL,NULL);
        PRAGMA user_version=2;")?;
    tx.timed_execute(
        "INSERT INTO history_state VALUES(1,?,?,'0')",
        params![replica, sequence],
    )?;
    tx.timed_commit()?;
    Ok(())
}

#[cfg(test)]
mod tests;
