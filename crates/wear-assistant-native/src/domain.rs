//! Pure JSON gateway: `kind` selects record/view/projection/plan/context/history/continuation/inventory/reconcile/receipts.
//! Request and output are capped at 16 MiB; raw operations at 2 MiB and snapshots at 4 MiB,
//! including every post-import snapshot. No database, credentials, provider or cached authority.
//! Retained records are rehashed from typed Operation JSON. Retired entries have no remaining
//! body to rehash: accept those only from the app's trusted committed SQLite ledger, never peers.

use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;
use sha2::{Digest, Sha256};
use std::io::{self, Write};
use std::panic::{catch_unwind, AssertUnwindSafe};
use voxgolem_core::conversation_sync::{
    inventory_page, receipts_after_commit, reconcile_page, InventoryPage, InventoryRequest,
};
use voxgolem_core::durable_conversation::{
    Command, ContextRequest, ContextSelection, ContinuationDecision, ContinuationRequest,
    DomainError, Operation, OperationId, QuarantinedRecord, Record, RetiredRecord, Store,
    StoreSnapshot, StoreView, MAX_BATCH_RECORDS, MAX_ENCODED_OPERATION_BYTES,
};

pub const MAX_GATEWAY_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_NATIVE_SNAPSHOT_BYTES: usize = 4 * 1024 * 1024;
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
struct HistoryRequest {
    expected_revision: String,
    conversation: OperationId,
    selected_head: Option<OperationId>,
    before: Option<String>,
    max_turns: usize,
    max_text_bytes: usize,
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
struct HistoryPage {
    revision: String,
    conversation: OperationId,
    head: Option<OperationId>,
    turns: Vec<Record>,
    older_before: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProjectionRequest {
    conversation: Option<OperationId>,
    selected_head: Option<OperationId>,
    local_work_in_flight: bool,
    read: Option<ProjectionRead>,
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum ProjectionRead {
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
enum ProjectionReadValue {
    Context(ContextSelection),
    History(HistoryPage),
}

#[derive(Serialize)]
#[serde(untagged)]
enum ProjectionReadOutcome {
    Success(Success<ProjectionReadValue>),
    Failure(Failure),
}

#[derive(Serialize)]
struct Projection {
    view: StoreView,
    continuation: Option<ContinuationDecision>,
    read: Option<ProjectionReadOutcome>,
}

fn projection(store: &Store, request: ProjectionRequest) -> Result<Projection, DomainError> {
    let view = store.snapshot();
    let mut projection = Projection {
        view,
        continuation: None,
        read: None,
    };
    let Some(conversation) = request.conversation else {
        return Ok(projection);
    };
    conversation.validate()?;
    if !projection
        .view
        .conversations
        .iter()
        .any(|item| item.id == conversation && !item.deleted && item.title.is_some())
    {
        return Ok(projection);
    }
    let revision = projection.view.revision.clone();
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
                .map(ProjectionReadValue::Context),
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

fn history_page(store: &Store, request: HistoryRequest) -> Result<HistoryPage, DomainError> {
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
            if cursor.version != 1
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
    let history = match &selection.parent {
        Some(head) => store.history(&selection.conversation, head)?,
        None => vec![],
    };
    let end = if let Some(cursor) = cursor {
        if cursor.conversation != selection.conversation
            || Some(&cursor.head) != selection.parent.as_ref()
        {
            return Err(invalid("history_cursor_branch"));
        }
        if cursor.revision != selection.revision {
            return Err(invalid("history_cursor_stale"));
        }
        history
            .iter()
            .position(|record| record.operation.id == cursor.before)
            .ok_or_else(|| invalid("history_cursor_before"))?
    } else {
        history.len()
    };
    let mut start = end;
    let mut bytes = 0;
    for record in history[..end].iter().rev().take(request.max_turns) {
        let size = record.operation.text_bytes();
        if size > request.max_text_bytes - bytes {
            if start == end {
                return Err(limit("history_turn_text_bytes"));
            }
            break;
        }
        bytes += size;
        start -= 1;
    }
    let older_before = if start > 0 {
        Some(encode(
            &HistoryCursor {
                version: 1,
                revision: selection.revision.clone(),
                conversation: selection.conversation.clone(),
                head: selection
                    .parent
                    .clone()
                    .ok_or_else(|| invalid("native_internal"))?,
                before: history[start].operation.id.clone(),
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
        turns: history[start..end].to_vec(),
        older_before,
    })
}

#[derive(Serialize)]
struct Success<T> {
    ok: bool,
    value: T,
}

#[derive(Serialize)]
struct Failure {
    ok: bool,
    error: DomainError,
}

pub(crate) fn invalid(field: &str) -> DomainError {
    DomainError::Invalid {
        field: field.into(),
    }
}

pub(crate) fn limit(name: &str) -> DomainError {
    DomainError::LimitExceeded { limit: name.into() }
}

fn parse<T: serde::de::DeserializeOwned>(raw: &str) -> Result<T, DomainError> {
    serde_json::from_str(raw).map_err(|_| invalid("native_request_json"))
}

fn canonical_record(raw: &RawValue) -> Result<Record, DomainError> {
    if raw.get().len() > MAX_ENCODED_OPERATION_BYTES {
        return Err(limit("encoded_operation_bytes"));
    }
    let operation: Operation = parse(raw.get())?;
    let encoded = serde_json::to_vec(&operation).map_err(|_| invalid("native_internal"))?;
    if encoded.len() > MAX_ENCODED_OPERATION_BYTES {
        return Err(limit("encoded_operation_bytes"));
    }
    let body_key = format!("{:x}", Sha256::digest(encoded));
    let record = Record {
        body_key,
        operation,
    };
    record.validate()?;
    Ok(record)
}

fn verify_record(wire: WireRecord) -> Result<Record, DomainError> {
    let record = canonical_record(&wire.operation)?;
    if wire.body_key != record.body_key {
        return Err(invalid("body_key_sha256_mismatch"));
    }
    Ok(record)
}

#[cfg(test)]
thread_local! {
    pub(crate) static RESTORE_CALLS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

fn restore(raw: &RawValue) -> Result<Store, DomainError> {
    #[cfg(test)]
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

fn success(value: impl Serialize) -> Result<String, DomainError> {
    encode(
        &Success { ok: true, value },
        MAX_GATEWAY_BYTES,
        "native_output_bytes",
    )
}

pub(crate) fn failure(error: DomainError) -> String {
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
            success(restore(raw(&input.snapshot)?)?.select_context(request)?)
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
