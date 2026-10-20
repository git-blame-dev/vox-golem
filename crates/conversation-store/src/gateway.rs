//! Pure JSON gateway: `kind` selects record/view/projection/plan/context/history/continuation/inventory/reconcile/receipts.
//! Request and output are capped at 16 MiB; raw operations at 2 MiB and snapshots at 4 MiB,
//! including every post-import snapshot. No database, credentials, provider or cached authority.
//! Retained records are rehashed from typed Operation JSON. Retired entries have no remaining
//! body to rehash: accept those only from the app's trusted committed SQLite ledger, never peers.

use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;
use std::io::{self, Write};
use std::panic::{catch_unwind, AssertUnwindSafe};
use voxgolem_core::conversation_sync::{
    inventory_page, receipts_after_commit, reconcile_page, InventoryPage, InventoryRequest,
};
use voxgolem_core::durable_conversation::{
    Command, ContextRequest, ContinuationDecision, ContinuationRequest, ConversationView,
    DomainError, OperationId, QuarantinedRecord, Record, RetiredRecord, Store, StoreSnapshot,
    StoreSummary, StoreView, MAX_BATCH_RECORDS,
};

pub const MAX_GATEWAY_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_NATIVE_SNAPSHOT_BYTES: usize = crate::MAX_CANONICAL_BYTES;
pub const MAX_HISTORY_TURNS: usize = 128;
pub const MAX_HISTORY_TEXT_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_HISTORY_CURSOR_BYTES: usize = 512;

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum RequestKind {
    Record,
    View,
    Projection,
    Plan,
    Context,
    History,
    Continuation,
    Inventory,
    Reconcile,
    Receipts,
}

#[derive(Default)]
struct RawField(Option<Box<RawValue>>);

impl<'de> Deserialize<'de> for RawField {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Box::<RawValue>::deserialize(deserializer).map(|raw| Self(Some(raw)))
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    kind: RequestKind,
    #[serde(default)]
    snapshot: RawField,
    #[serde(default)]
    operation: RawField,
    #[serde(default)]
    command: RawField,
    #[serde(default)]
    request: RawField,
    #[serde(default)]
    page: RawField,
    #[serde(default)]
    recipient: RawField,
    #[serde(default)]
    operation_ids: RawField,
}

impl Request {
    fn validate_shape(&self) -> Result<(), DomainError> {
        let present = [
            &self.snapshot,
            &self.operation,
            &self.command,
            &self.request,
            &self.page,
            &self.recipient,
            &self.operation_ids,
        ]
        .map(|field| field.0.is_some());
        let expected = match self.kind {
            RequestKind::Record => [false, true, false, false, false, false, false],
            RequestKind::View => [true, false, false, false, false, false, false],
            RequestKind::Plan => [true, false, true, false, false, false, false],
            RequestKind::Context
            | RequestKind::Projection
            | RequestKind::History
            | RequestKind::Continuation
            | RequestKind::Inventory => [true, false, false, true, false, false, false],
            RequestKind::Reconcile => [true, false, false, false, true, false, false],
            RequestKind::Receipts => [true, false, false, false, false, true, true],
        };
        if present != expected {
            return Err(invalid("native_request_json"));
        }
        Ok(())
    }

    fn validate_owned_shape(&self) -> Result<(), DomainError> {
        let actual = [
            &self.snapshot,
            &self.operation,
            &self.command,
            &self.request,
            &self.page,
            &self.recipient,
            &self.operation_ids,
        ]
        .map(|field| field.0.is_some());
        let expected = match self.kind {
            RequestKind::View => [false; 7],
            RequestKind::Projection
            | RequestKind::Context
            | RequestKind::History
            | RequestKind::Continuation
            | RequestKind::Inventory => [false, false, false, true, false, false, false],
            RequestKind::Reconcile => [false, false, false, false, true, false, false],
            RequestKind::Receipts => [false, false, false, false, false, true, true],
            _ => return Err(invalid("owned_read_kind")),
        };
        if actual != expected {
            return Err(invalid("owned_read_shape"));
        }
        Ok(())
    }
}

fn raw(field: &RawField) -> Result<&RawValue, DomainError> {
    field
        .0
        .as_deref()
        .ok_or_else(|| invalid("native_request_json"))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireRecord {
    body_key: String,
    operation: Box<RawValue>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireSnapshot {
    version: u32,
    revision: String,
    records: Vec<WireRecord>,
    retired: Vec<RetiredRecord>,
    #[serde(default)]
    quarantine: Vec<QuarantinedRecord>,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum CommandKind {
    Import,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireCommand {
    kind: CommandKind,
    expected_revision: String,
    records: Vec<WireRecord>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HistoryRequest {
    pub expected_revision: String,
    pub conversation: OperationId,
    pub selected_head: Option<OperationId>,
    pub before: Option<String>,
    pub max_turns: usize,
    pub max_text_bytes: usize,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct HistoryCursor {
    version: u32,
    revision: String,
    conversation: OperationId,
    head: OperationId,
    before: OperationId,
}

#[derive(Serialize)]
pub struct HistoryPage {
    pub revision: String,
    pub conversation: OperationId,
    pub head: Option<OperationId>,
    pub turns: Vec<Record>,
    pub older_before: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectionRequest {
    pub conversation: Option<OperationId>,
    pub selected_head: Option<OperationId>,
    pub local_work_in_flight: bool,
    pub read: Option<ProjectionRead>,
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ProjectionRead {
    Context {
        max_turns: usize,
        max_text_bytes: usize,
    },
    History {
        before: Option<String>,
        max_turns: usize,
        max_text_bytes: usize,
    },
}

#[derive(Serialize)]
#[serde(untagged)]
pub enum ProjectionReadValue {
    Context(crate::ProviderContext),
    History(HistoryPage),
}

#[derive(Serialize)]
#[serde(untagged)]
pub enum ProjectionReadOutcome {
    Success(Success<ProjectionReadValue>),
    Failure(Failure),
}

pub const MAX_METADATA_BYTES: usize = 32 * 1024;
pub const MAX_METADATA_ITEMS: usize = 128;
const CONVERSATION_PAGE_BYTES: usize = MAX_METADATA_BYTES - 12 * 1024;

#[derive(Clone, Debug, Serialize)]
pub struct ConversationMetadata {
    pub id: OperationId,
    pub title: String,
    pub head_count: usize,
    pub pending_count: usize,
}
#[derive(Clone, Debug, Serialize)]
pub struct HeadMetadata {
    pub id: OperationId,
    pub prompt_preview: String,
}
#[derive(Serialize)]
pub struct CompactView {
    pub revision: String,
    pub conversations: Vec<ConversationMetadata>,
    pub conversation_count: usize,
    pub conversation_offset: usize,
    pub branch_count: usize,
    pub branch_offset: usize,
    pub branch_previews: Vec<HeadMetadata>,
    pub pending_count: usize,
    pub quarantine_count: usize,
}

#[derive(Serialize)]
pub struct Projection<V = CompactView> {
    pub view: V,
    pub continuation: Option<ContinuationDecision>,
    pub read: Option<ProjectionReadOutcome>,
}

fn projection(
    store: &Store,
    request: ProjectionRequest,
) -> Result<Projection<StoreView>, DomainError> {
    let view = store.snapshot();
    let exists = selected_exists(&view.conversations, request.conversation.as_ref());
    let revision = view.revision.clone();
    projection_with_view(store, request, view, exists, revision)
}

fn selected_exists(conversations: &[ConversationView], selected: Option<&OperationId>) -> bool {
    selected.is_some_and(|id| {
        conversations
            .iter()
            .any(|c| &c.id == id && !c.deleted && c.title.is_some())
    })
}

pub fn head_previews(store: &Store, heads: &[OperationId]) -> Vec<HeadMetadata> {
    heads
        .iter()
        .map(|id| HeadMetadata {
            id: id.clone(),
            prompt_preview: store.prompt_preview(id).unwrap_or_default(),
        })
        .collect()
}

pub fn head_page_from_heads(
    store: &Store,
    revision: String,
    conversation: OperationId,
    all_heads: Vec<OperationId>,
    offset: usize,
    count: usize,
) -> Result<crate::HeadPage, DomainError> {
    if count == 0 || count > MAX_METADATA_ITEMS || offset > 16_384 {
        return Err(limit("head_page"));
    }
    let total = all_heads.len();
    let candidates: Vec<_> = all_heads.into_iter().skip(offset).take(count).collect();
    let candidate_previews = head_previews(store, &candidates);
    let mut heads = Vec::new();
    let mut head_previews = Vec::new();
    let mut bytes = 512;
    for (head, preview) in candidates.into_iter().zip(candidate_previews) {
        let step = encode(&head, MAX_METADATA_BYTES, "metadata_bytes")?.len()
            + 1
            + encode(&preview, MAX_METADATA_BYTES, "metadata_bytes")?.len()
            + 1;
        if bytes + step > MAX_METADATA_BYTES {
            break;
        }
        bytes += step;
        heads.push(head);
        head_previews.push(preview);
    }
    let next_offset = (offset + heads.len()).min(total);
    let page = crate::HeadPage {
        revision,
        conversation,
        head_count: total,
        head_offset: offset,
        next_offset,
        heads,
        head_previews,
    };
    encode(&page, MAX_METADATA_BYTES, "metadata_bytes")?;
    Ok(page)
}

pub fn conversation_page(
    summary: StoreSummary,
    offset: usize,
    count: usize,
    selected: Option<&OperationId>,
) -> Result<crate::ConversationPage, DomainError> {
    if count == 0 || count > MAX_METADATA_ITEMS || offset > 16_384 {
        return Err(limit("conversation_page"));
    }
    let conversations: Vec<_> = summary
        .conversations
        .into_iter()
        .filter_map(|c| {
            if c.deleted {
                return None;
            }
            c.title.map(|title| ConversationMetadata {
                id: c.id,
                title,
                head_count: c.heads.len(),
                pending_count: c.pending.len(),
            })
        })
        .collect();
    let total = conversations.len();
    let selected = selected
        .and_then(|id| conversations.iter().find(|c| &c.id == id))
        .cloned();
    let mut rows = Vec::new();
    let mut bytes = 512;
    for conversation in conversations.into_iter().skip(offset).take(count) {
        let size = encode(&conversation, MAX_METADATA_BYTES, "metadata_bytes")?.len() + 1;
        if bytes + size > CONVERSATION_PAGE_BYTES {
            break;
        }
        bytes += size;
        rows.push(conversation);
    }
    let mut next = (offset + rows.len()).min(total);
    if let Some(selected) =
        selected.filter(|selected| !rows.iter().any(|row| row.id == selected.id))
    {
        let size = encode(&selected, MAX_METADATA_BYTES, "metadata_bytes")?.len() + 1;
        while rows.len() >= count || bytes + size > CONVERSATION_PAGE_BYTES {
            let Some(last) = rows.pop() else {
                return Err(limit("metadata_bytes"));
            };
            bytes -= encode(&last, MAX_METADATA_BYTES, "metadata_bytes")?.len() + 1;
            next = next.saturating_sub(1);
        }
        rows.push(selected);
    }
    let page = crate::ConversationPage {
        revision: summary.revision,
        conversation_count: total,
        conversation_offset: offset,
        next_offset: next,
        conversations: rows,
    };
    encode(&page, MAX_METADATA_BYTES, "metadata_bytes")?;
    Ok(page)
}

fn compact_from_summary(
    summary: StoreSummary,
    selected: Option<&OperationId>,
) -> Result<CompactView, DomainError> {
    let pending_count = summary.pending.len();
    let quarantine_count = summary.quarantine.len();
    let page = conversation_page(summary, 0, MAX_METADATA_ITEMS, selected)?;
    let view = CompactView {
        revision: page.revision,
        conversations: page.conversations,
        conversation_count: page.conversation_count,
        conversation_offset: page.next_offset,
        branch_count: 0,
        branch_offset: 0,
        branch_previews: vec![],
        pending_count,
        quarantine_count,
    };
    encode(&view, MAX_METADATA_BYTES, "metadata_bytes")?;
    Ok(view)
}
/// No snapshot, retained record or answer clone occurs in this path.
pub fn compact_view(store: &Store) -> Result<CompactView, DomainError> {
    compact_from_summary(store.summary(), None)
}

pub fn project(store: &Store, request: ProjectionRequest) -> Result<Projection, DomainError> {
    let summary = store.summary();
    let exists = selected_exists(&summary.conversations, request.conversation.as_ref());
    let revision = summary.revision.clone();
    let view = compact_from_summary(summary, request.conversation.as_ref())?;
    let mut projection = projection_with_view(store, request, view, exists, revision)?;
    if let Some(ContinuationDecision::Choose { heads }) = &mut projection.continuation {
        let total = heads.len();
        projection.view.branch_count = total;
        heads.truncate(MAX_METADATA_ITEMS);
        let candidates = std::mem::take(heads);
        let candidate_previews = head_previews(store, &candidates);
        let base_view_bytes = encode(&projection.view, MAX_METADATA_BYTES, "metadata_bytes")?.len();
        // Reserve 64 bytes for potential digit growth in branch_offset.
        let max_preview_bytes = MAX_METADATA_BYTES.saturating_sub(base_view_bytes + 64);
        let mut preview_bytes = 0usize;
        for (head, preview) in candidates.into_iter().zip(candidate_previews) {
            let size = encode(&preview, MAX_METADATA_BYTES, "metadata_bytes")?.len() + 1;
            if preview_bytes + size > max_preview_bytes {
                break;
            }
            preview_bytes += size;
            heads.push(head);
            projection.view.branch_previews.push(preview);
        }
        projection.view.branch_offset = heads.len();
        while encode(&projection.view, MAX_METADATA_BYTES, "metadata_bytes").is_err() {
            if heads.pop().is_none() {
                return Err(limit("metadata_bytes"));
            }
            projection.view.branch_previews.pop();
            projection.view.branch_offset = heads.len();
        }
    }
    Ok(projection)
}

fn projection_with_view<V>(
    store: &Store,
    request: ProjectionRequest,
    view: V,
    exists: bool,
    revision: String,
) -> Result<Projection<V>, DomainError> {
    let mut projection = Projection {
        view,
        continuation: None,
        read: None,
    };
    let Some(conversation) = request.conversation else {
        return Ok(projection);
    };
    conversation.validate()?;
    if !exists {
        return Ok(projection);
    }
    let decision = store.continuation(ContinuationRequest {
        expected_revision: revision.clone(),
        conversation: conversation.clone(),
        selected_head: request.selected_head.clone(),
        local_work_in_flight: request.local_work_in_flight,
    })?;
    let unresolved =
        matches!(decision, ContinuationDecision::Choose { .. }) && request.selected_head.is_none();
    let head = match &decision {
        ContinuationDecision::FastForward { head } => Some(head.clone()),
        _ => request.selected_head,
    };
    projection.continuation = Some(decision);
    if let Some(read) = request.read.filter(|_| !unresolved) {
        let result = match read {
            ProjectionRead::Context {
                max_turns,
                max_text_bytes,
            } => store
                .select_context(ContextRequest {
                    expected_revision: revision,
                    conversation,
                    selected_head: head,
                    max_turns,
                    max_text_bytes,
                })
                .map(|selection| ProjectionReadValue::Context(crate::provider_context(selection))),
            ProjectionRead::History {
                before,
                max_turns,
                max_text_bytes,
            } => history_page(
                store,
                HistoryRequest {
                    expected_revision: revision,
                    conversation,
                    selected_head: head,
                    before,
                    max_turns,
                    max_text_bytes,
                },
            )
            .map(ProjectionReadValue::History),
        };
        // A cursor/read failure must not discard the fresh canonical selection projection.
        projection.read = Some(match result {
            Ok(value) => ProjectionReadOutcome::Success(Success { ok: true, value }),
            Err(error) => ProjectionReadOutcome::Failure(Failure { ok: false, error }),
        });
    }
    Ok(projection)
}

pub fn history_page(store: &Store, request: HistoryRequest) -> Result<HistoryPage, DomainError> {
    if request.max_turns == 0
        || request.max_turns > MAX_HISTORY_TURNS
        || request.max_text_bytes == 0
        || request.max_text_bytes > MAX_HISTORY_TEXT_BYTES
    {
        return Err(limit("history_page"));
    }
    let cursor = request
        .before
        .as_deref()
        .map(|before| {
            if before.len() > MAX_HISTORY_CURSOR_BYTES {
                return Err(limit("history_cursor_bytes"));
            }
            let cursor: HistoryCursor =
                serde_json::from_str(before).map_err(|_| invalid("history_cursor"))?;
            if cursor.version != 2
                || cursor.conversation.validate().is_err()
                || cursor.head.validate().is_err()
                || cursor.before.validate().is_err()
            {
                return Err(invalid("history_cursor"));
            }
            Ok(cursor)
        })
        .transpose()?;
    // Zero context budget reuses the core's selection rules, not its provider turn window.
    let selection = store.select_context(ContextRequest {
        expected_revision: request.expected_revision,
        conversation: request.conversation,
        selected_head: request.selected_head,
        max_turns: 0,
        max_text_bytes: 0,
    })?;
    // Core validates the entire ancestry before exposing any records. Keep that
    // authority, but do not clone text outside the visible page.
    let mut history = selection
        .parent
        .as_ref()
        .map(|head| store.history_borrowed(&selection.conversation, head))
        .transpose()?
        .into_iter()
        .flatten()
        .rev();
    if let Some(cursor) = &cursor {
        if cursor.conversation != selection.conversation
            || Some(&cursor.head) != selection.parent.as_ref()
        {
            return Err(invalid("history_cursor_branch"));
        }
        if cursor.revision != selection.revision {
            return Err(invalid("history_cursor_stale"));
        }
        if !history.any(|record| record.operation.id == cursor.before) {
            return Err(invalid("history_cursor_before"));
        }
    }
    let mut turns = Vec::new();
    let mut bytes = 0;
    let mut has_older = false;
    for record in history {
        if turns.len() == request.max_turns {
            has_older = true;
            break;
        }
        let size = record.operation.text_bytes();
        if size > request.max_text_bytes - bytes {
            if turns.is_empty() {
                return Err(limit("history_turn_text_bytes"));
            }
            has_older = true;
            break;
        }
        bytes += size;
        turns.push(record.clone());
    }
    turns.reverse();
    let older_before = if has_older {
        Some(encode(
            &HistoryCursor {
                version: 2,
                revision: selection.revision.clone(),
                conversation: selection.conversation.clone(),
                head: selection
                    .parent
                    .clone()
                    .ok_or_else(|| invalid("native_internal"))?,
                before: turns[0].operation.id.clone(),
            },
            MAX_HISTORY_CURSOR_BYTES,
            "history_cursor_bytes",
        )?)
    } else {
        None
    };
    Ok(HistoryPage {
        revision: selection.revision,
        conversation: selection.conversation,
        head: selection.parent,
        turns,
        older_before,
    })
}

#[derive(Serialize)]
pub struct Success<T> {
    pub ok: bool,
    pub value: T,
}

#[derive(Serialize)]
pub struct Failure {
    pub ok: bool,
    pub error: DomainError,
}

pub fn invalid(field: &str) -> DomainError {
    DomainError::Invalid {
        field: field.into(),
    }
}

pub fn limit(name: &str) -> DomainError {
    DomainError::LimitExceeded { limit: name.into() }
}

fn parse<T: serde::de::DeserializeOwned>(raw: &str) -> Result<T, DomainError> {
    let _decode = crate::timing::span(crate::timing::Phase::native_request_decode);
    serde_json::from_str(raw).map_err(|_| invalid("native_request_json"))
}

fn canonical_record(raw: &RawValue) -> Result<Record, DomainError> {
    crate::codec::canonical_record(raw.get())
}

fn verify_record(wire: WireRecord) -> Result<Record, DomainError> {
    let record = canonical_record(&wire.operation)?;
    if wire.body_key != record.body_key {
        return Err(invalid("body_key_sha256_mismatch"));
    }
    Ok(record)
}

#[cfg(debug_assertions)]
thread_local! {
    pub static RESTORE_CALLS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

fn restore(raw: &RawValue) -> Result<Store, DomainError> {
    #[cfg(debug_assertions)]
    RESTORE_CALLS.with(|calls| calls.set(calls.get() + 1));
    if raw.get().len() > MAX_NATIVE_SNAPSHOT_BYTES {
        return Err(limit("native_snapshot_bytes"));
    }
    let wire: WireSnapshot = parse(raw.get())?;
    let snapshot = StoreSnapshot {
        version: wire.version,
        revision: wire.revision,
        records: wire
            .records
            .into_iter()
            .map(verify_record)
            .collect::<Result<_, _>>()?,
        // Retired bodies have been purged. Their keys are a trusted SQLite ledger, not peer input.
        retired: wire.retired,
        quarantine: wire.quarantine,
    };
    check_snapshot_quota(&snapshot)?;
    let store = Store::restore(snapshot)?;
    check_snapshot_quota(&store.export())?;
    Ok(store)
}

struct BoundedWriter {
    bytes: Vec<u8>,
    max: usize,
}

impl Write for BoundedWriter {
    fn write(&mut self, input: &[u8]) -> io::Result<usize> {
        if input.len() > self.max.saturating_sub(self.bytes.len()) {
            return Err(io::Error::other("native JSON byte limit"));
        }
        self.bytes.extend_from_slice(input);
        Ok(input.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn encode(value: &impl Serialize, max: usize, bound: &str) -> Result<String, DomainError> {
    let mut writer = BoundedWriter { bytes: vec![], max };
    serde_json::to_writer(&mut writer, value).map_err(|_| limit(bound))?;
    String::from_utf8(writer.bytes).map_err(|_| invalid("native_internal"))
}

fn check_snapshot_quota(snapshot: &StoreSnapshot) -> Result<(), DomainError> {
    encode(snapshot, MAX_NATIVE_SNAPSHOT_BYTES, "native_snapshot_bytes").map(|_| ())
}

pub fn success(value: impl Serialize) -> Result<String, DomainError> {
    let _encode = crate::timing::span(crate::timing::Phase::native_response_encode);
    encode(
        &Success { ok: true, value },
        MAX_GATEWAY_BYTES,
        "native_output_bytes",
    )
}

pub fn failure(error: DomainError) -> String {
    let _encode = crate::timing::span(crate::timing::Phase::native_response_encode);
    encode(
        &Failure { ok: false, error },
        MAX_GATEWAY_BYTES,
        "native_output_bytes",
    )
    .unwrap_or_else(|_| {
        r#"{"ok":false,"error":{"kind":"invalid","field":"native_internal"}}"#.into()
    })
}

fn evaluate(input: &str) -> Result<String, DomainError> {
    if input.len() > MAX_GATEWAY_BYTES {
        return Err(limit("native_request_bytes"));
    }
    let input: Request = parse(input)?;
    input.validate_shape()?;
    match input.kind {
        RequestKind::Record => success(canonical_record(raw(&input.operation)?)?),
        RequestKind::View => success(restore(raw(&input.snapshot)?)?.snapshot()),
        RequestKind::Projection => {
            let request: ProjectionRequest = parse(raw(&input.request)?.get())?;
            success(projection(&restore(raw(&input.snapshot)?)?, request)?)
        }
        RequestKind::Plan => {
            let store = restore(raw(&input.snapshot)?)?;
            let WireCommand {
                kind: CommandKind::Import,
                expected_revision,
                records,
            } = parse(raw(&input.command)?.get())?;
            if records.len() > MAX_BATCH_RECORDS {
                return Err(limit("batch_records"));
            }
            let records = records
                .into_iter()
                .map(verify_record)
                .collect::<Result<_, _>>()?;
            let plan = store.plan(Command::Import {
                expected_revision,
                records,
            })?;
            check_snapshot_quota(&plan.snapshot)?;
            success(plan)
        }
        RequestKind::Context => {
            let request: ContextRequest = parse(raw(&input.request)?.get())?;
            success(crate::provider_context(
                restore(raw(&input.snapshot)?)?.select_context(request)?,
            ))
        }
        RequestKind::History => {
            let request: HistoryRequest = parse(raw(&input.request)?.get())?;
            success(history_page(&restore(raw(&input.snapshot)?)?, request)?)
        }
        RequestKind::Continuation => {
            let request: ContinuationRequest = parse(raw(&input.request)?.get())?;
            success(restore(raw(&input.snapshot)?)?.continuation(request)?)
        }
        RequestKind::Inventory => {
            let request: InventoryRequest = parse(raw(&input.request)?.get())?;
            success(inventory_page(&restore(raw(&input.snapshot)?)?, request)?)
        }
        RequestKind::Reconcile => {
            let page: InventoryPage = parse(raw(&input.page)?.get())?;
            success(reconcile_page(&restore(raw(&input.snapshot)?)?, page)?)
        }
        RequestKind::Receipts => {
            let recipient: String = parse(raw(&input.recipient)?.get())?;
            let operation_ids: Vec<OperationId> = parse(raw(&input.operation_ids)?.get())?;
            success(receipts_after_commit(
                &restore(raw(&input.snapshot)?)?,
                &recipient,
                &operation_ids,
            )?)
        }
    }
}

/// Stateful owner path: no snapshot is accepted, restored or returned for ordinary reads.
pub fn read_owned(store: &Store, input: &str) -> Result<String, DomainError> {
    if input.len() > MAX_GATEWAY_BYTES {
        return Err(limit("native_request_bytes"));
    }
    let input: Request = parse(input)?;
    input.validate_owned_shape()?;
    match input.kind {
        RequestKind::View => success(crate::timing::measure(
            crate::timing::Phase::projection,
            || compact_view(store),
        )?),
        RequestKind::Projection => {
            let request = parse(raw(&input.request)?.get())?;
            success(crate::timing::measure(
                crate::timing::Phase::projection,
                || project(store, request),
            )?)
        }
        RequestKind::Context => {
            let request = parse(raw(&input.request)?.get())?;
            success(crate::timing::measure(
                crate::timing::Phase::projection,
                || store.select_context(request).map(crate::provider_context),
            )?)
        }
        RequestKind::History => {
            let request = parse(raw(&input.request)?.get())?;
            success(crate::timing::measure(
                crate::timing::Phase::projection,
                || history_page(store, request),
            )?)
        }
        RequestKind::Continuation => {
            let request = parse(raw(&input.request)?.get())?;
            success(crate::timing::measure(
                crate::timing::Phase::projection,
                || store.continuation(request),
            )?)
        }
        RequestKind::Inventory => {
            let request = parse(raw(&input.request)?.get())?;
            success(crate::timing::measure(
                crate::timing::Phase::projection,
                || inventory_page(store, request),
            )?)
        }
        RequestKind::Reconcile => {
            let page = parse(raw(&input.page)?.get())?;
            success(crate::timing::measure(
                crate::timing::Phase::projection,
                || reconcile_page(store, page),
            )?)
        }
        RequestKind::Receipts => {
            let recipient: String = parse(raw(&input.recipient)?.get())?;
            let operation_ids: Vec<OperationId> = parse(raw(&input.operation_ids)?.get())?;
            success(crate::timing::measure(
                crate::timing::Phase::projection,
                || receipts_after_commit(store, &recipient, &operation_ids),
            )?)
        }
        _ => Err(invalid("owned_read_kind")),
    }
}

/// Pure, bounded core evaluation. SQLite remains the only persisted authority.
pub fn apply_json(input: &str) -> String {
    catch_unwind(AssertUnwindSafe(|| evaluate(input)))
        .unwrap_or_else(|_| Err(invalid("native_internal")))
        .unwrap_or_else(failure)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_writer_accepts_exact_boundary_without_retaining_overflow() {
        assert_eq!(encode(&"x", 3, "native_output_bytes").unwrap(), "\"x\"");
        assert_eq!(
            encode(&"xx", 3, "native_output_bytes"),
            Err(limit("native_output_bytes"))
        );
        let mut writer = BoundedWriter {
            bytes: vec![],
            max: 2,
        };
        writer.write_all(b"ab").unwrap();
        assert!(writer.write_all(b"c").is_err());
        assert_eq!(writer.bytes, b"ab");
    }
}
