use crate::assistant::{
    resolve_with_stage, AssistantPolicy, Content, DeepOutcome, InstantOutcome, Role, Stage,
    StageResult,
};
use crate::sources::{merge_sources, source_bytes, SourceLink, SourceValidationError};
use serde::{Deserialize, Serialize};

pub const MAX_REQUEST_ID_BYTES: usize = 128;
pub const MAX_PROMPT_BYTES: usize = 32 * 1024;
pub const MAX_UPDATE_BYTES: usize = 256 * 1024;
pub const MAX_COMMITTED_MESSAGES: usize = 40;
pub const MAX_COMMITTED_TEXT_BYTES: usize = 256 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SessionId(pub u64);
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RequestId(pub String);
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct GenerationId(pub u64);
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ExecutionId {
    pub session_id: SessionId,
    pub generation: GenerationId,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct MessageId(pub u64);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Message {
    pub id: MessageId,
    pub role: Role,
    pub content: Content,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Turn {
    pub sources: Vec<SourceLink>,
    pub request_id: RequestId,
    pub execution_id: ExecutionId,
    pub user: Message,
    pub answer: Message,
    pub final_stage: Stage,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdmissionError {
    EmptyRequestId,
    EmptyPrompt,
    RequestIdTooLong,
    PromptTooLong,
    Busy,
    InvalidContext,
    UnresolvedSave,
}

/// Validate renderer/native request correlation before any admission work. Keep
/// accepted identifiers unchanged so every admitted request remains representable
/// in both saved and unsaved terminal projections.
pub fn validate_request_id(request_id: &str) -> Result<(), AdmissionError> {
    if request_id.len() > MAX_REQUEST_ID_BYTES {
        return Err(AdmissionError::RequestIdTooLong);
    }
    if request_id.trim().is_empty() {
        return Err(AdmissionError::EmptyRequestId);
    }
    Ok(())
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateResult {
    Pending,
    Provisional(Content),
    Resolved {
        content: Content,
        final_stage: Stage,
    },
    IgnoredStale,
    WrongStage,
    DuplicateStage,
    RejectedOversized,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SettlementIntent {
    Success,
    Failed,
    Cancelled,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppliedOutcome {
    Completed(Turn),
    Refused(Turn),
    Failed,
    Cancelled,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SettlementResult {
    Applied(AppliedOutcome),
    IgnoredStale,
    Rejected(SettlementError),
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SettlementError {
    NotResolved,
    TranscriptTooLarge,
}

/// Admission-blocking metadata only. The native owner retains one validated payload;
/// this execution reducer never stores an unsaved answer as conversation history.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnresolvedSave {
    pub execution_id: ExecutionId,
    pub request_id: RequestId,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResetResult {
    pub old_session_id: SessionId,
    pub new_session_id: SessionId,
    pub invalidated: Option<ExecutionId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdmissionSnapshot {
    pub execution_id: ExecutionId,
    pub request_id: RequestId,
    pub prompt: String,
    pub history: Vec<crate::assistant::ConversationTurn>,
    pub policy: AssistantPolicy,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActiveExecutionSummary {
    pub execution_id: ExecutionId,
    pub request_id: RequestId,
    pub prompt: String,
}

#[derive(Debug, Clone)]
struct Active {
    sources: Vec<SourceLink>,
    snapshot: ActiveAdmission,
    instant: Option<InstantOutcome>,
    deep: Option<DeepOutcome>,
    review: Option<crate::assistant::ReviewOutcome>,
    resolved: Option<(Content, Stage)>,
    finalizing: bool,
}

/// One bounded completion capture for durable planning. Once captured, only final
/// settlement/cancellation may change the execution; late provider updates are stale.
pub struct ResolvedCompletion {
    pub prompt: String,
    pub answer: crate::durable_conversation::Answer,
    pub sources: Vec<SourceLink>,
    pub final_stage: Stage,
}

/// Provider context belongs to the caller's pinned request, not the execution reducer.
#[derive(Debug, Clone)]
struct ActiveAdmission {
    execution_id: ExecutionId,
    request_id: RequestId,
    prompt: String,
    policy: AssistantPolicy,
}
#[derive(Debug, Clone)]
pub struct ConversationSession {
    session_id: SessionId,
    policy: AssistantPolicy,
    active: Option<Active>,
    next_session: u64,
    next_generation: u64,
    next_message: u64,
    unresolved_save: Option<UnresolvedSave>,
}

impl ConversationSession {
    /// Coordinate one execution while a durable store exclusively owns history.
    /// Admission context must come from that store; settled turns are returned,
    /// never retained as a second independently mutable transcript.
    pub fn execution_only(policy: AssistantPolicy) -> Self {
        Self {
            session_id: SessionId(1),
            policy,
            active: None,
            next_session: 2,
            next_generation: 1,
            next_message: 1,
            unresolved_save: None,
        }
    }
    pub fn session_id(&self) -> SessionId {
        self.session_id
    }
    pub fn policy(&self) -> &AssistantPolicy {
        &self.policy
    }
    pub fn has_active_execution(&self) -> bool {
        self.active.is_some()
    }
    pub fn unresolved_save(&self) -> Option<&UnresolvedSave> {
        self.unresolved_save.as_ref()
    }
    pub fn active_execution(&self) -> Option<ExecutionId> {
        self.active.as_ref().map(|a| a.snapshot.execution_id)
    }
    pub fn active_summary(&self) -> Option<ActiveExecutionSummary> {
        self.active.as_ref().map(|a| ActiveExecutionSummary {
            execution_id: a.snapshot.execution_id,
            request_id: a.snapshot.request_id.clone(),
            prompt: a.snapshot.prompt.clone(),
        })
    }
    pub fn active_prompt(&self) -> Option<&str> {
        self.active.as_ref().map(|a| a.snapshot.prompt.as_str())
    }
    pub fn provisional_instant(&self) -> Option<&InstantOutcome> {
        self.active
            .as_ref()
            .and_then(|active| active.instant.as_ref())
    }
    pub fn resolved(&self) -> Option<(&Content, Stage)> {
        self.active.as_ref().and_then(|active| {
            active
                .resolved
                .as_ref()
                .map(|(content, stage)| (content, *stage))
        })
    }
    pub fn replace_policy(&mut self, policy: AssistantPolicy) -> Result<(), AdmissionError> {
        if self.active.is_some() {
            return Err(AdmissionError::Busy);
        }
        self.policy = policy;
        Ok(())
    }

    /// Admit an immutable provider context owned by the durable history authority.
    pub fn admit_with_context(
        &mut self,
        request_id: &str,
        prompt: &str,
        history: Vec<crate::assistant::ConversationTurn>,
    ) -> Result<AdmissionSnapshot, AdmissionError> {
        if history.len() > MAX_COMMITTED_MESSAGES
            || !history.len().is_multiple_of(2)
            || history.iter().enumerate().any(|(index, turn)| {
                turn.role
                    != if index % 2 == 0 {
                        Role::User
                    } else {
                        Role::Assistant
                    }
                    || (index % 2 == 0 && !matches!(turn.content, Content::Text(_)))
            })
            || history
                .iter()
                .try_fold(prompt.len(), |bytes, turn| {
                    bytes.checked_add(content_bytes(&turn.content))
                })
                .is_none_or(|bytes| bytes > MAX_COMMITTED_TEXT_BYTES)
        {
            return Err(AdmissionError::InvalidContext);
        }
        if self.unresolved_save.is_some() {
            return Err(AdmissionError::UnresolvedSave);
        }
        validate_request_id(request_id)?;
        if prompt.trim().is_empty() {
            return Err(AdmissionError::EmptyPrompt);
        }
        if prompt.len() > MAX_PROMPT_BYTES {
            return Err(AdmissionError::PromptTooLong);
        }
        if self.active.is_some() {
            return Err(AdmissionError::Busy);
        }
        let execution_id = ExecutionId {
            session_id: self.session_id,
            generation: GenerationId(self.next_generation),
        };
        self.next_generation += 1;
        let snapshot = AdmissionSnapshot {
            execution_id,
            request_id: RequestId(request_id.into()),
            prompt: prompt.into(),
            history,
            policy: self.policy.clone(),
        };
        self.active = Some(Active {
            sources: Vec::new(),
            instant: None,
            deep: None,
            review: None,
            snapshot: ActiveAdmission {
                execution_id: snapshot.execution_id,
                request_id: snapshot.request_id.clone(),
                prompt: snapshot.prompt.clone(),
                policy: snapshot.policy.clone(),
            },
            resolved: None,
            finalizing: false,
        });
        Ok(snapshot)
    }

    pub fn observe_sources(
        &mut self,
        execution_id: ExecutionId,
        sources: &[SourceLink],
    ) -> Result<bool, SourceValidationError> {
        let Some(active) = self
            .active
            .as_mut()
            .filter(|active| active.snapshot.execution_id == execution_id && !active.finalizing)
        else {
            return Ok(false);
        };
        let merged = merge_sources(&active.sources, sources)?;
        active.sources = merged;
        Ok(true)
    }
    pub fn update(
        &mut self,
        execution_id: ExecutionId,
        stage: Stage,
        result: StageResult,
    ) -> UpdateResult {
        let Some(active) = self.active.as_mut() else {
            return UpdateResult::IgnoredStale;
        };
        if active.snapshot.execution_id != execution_id || active.finalizing {
            return UpdateResult::IgnoredStale;
        }
        let seen = match (&stage, &result) {
            (Stage::Instant, StageResult::Instant(_)) => active.instant.is_some(),
            (Stage::Deep, StageResult::Deep(_)) => active.deep.is_some(),
            (Stage::Review, StageResult::Review(_)) => active.review.is_some(),
            _ => return UpdateResult::WrongStage,
        };
        if seen {
            return UpdateResult::DuplicateStage;
        }
        if (stage == Stage::Deep && !active.snapshot.policy.deep_enabled)
            || (stage == Stage::Review && !active.snapshot.policy.review_enabled)
        {
            return UpdateResult::WrongStage;
        }
        if stage_result_bytes(&result) > MAX_UPDATE_BYTES {
            return UpdateResult::RejectedOversized;
        }
        match (&stage, &result) {
            (Stage::Instant, StageResult::Instant(v)) => active.instant = Some(v.clone()),
            (Stage::Deep, StageResult::Deep(v)) => active.deep = Some(v.clone()),
            (Stage::Review, StageResult::Review(v)) => active.review = Some(v.clone()),
            _ => unreachable!("stage/result pairing validated above"),
        }
        if let Some((content, final_stage)) = resolve_with_stage(
            active.instant.as_ref(),
            active.deep.as_ref(),
            active.review.as_ref(),
            &active.snapshot.policy,
        ) {
            active.resolved = Some((content.clone(), final_stage));
            return UpdateResult::Resolved {
                content,
                final_stage,
            };
        }
        match result {
            StageResult::Instant(InstantOutcome::Complete(c) | InstantOutcome::NeedsDeep(c)) => {
                UpdateResult::Provisional(c)
            }
            _ => UpdateResult::Pending,
        }
    }
    pub fn begin_settlement(
        &mut self,
        execution_id: ExecutionId,
    ) -> Result<ResolvedCompletion, SettlementError> {
        let active = self
            .active
            .as_mut()
            .filter(|a| a.snapshot.execution_id == execution_id && !a.finalizing)
            .ok_or(SettlementError::NotResolved)?;
        let (content, final_stage) = active
            .resolved
            .as_ref()
            .ok_or(SettlementError::NotResolved)?;
        let answer = match content {
            Content::Text(text) => crate::durable_conversation::Answer::Text { text: text.clone() },
            Content::Refusal(reason) => crate::durable_conversation::Answer::Refusal {
                reason: reason.clone(),
            },
        };
        let sources = if matches!(content, Content::Refusal(_)) {
            &[][..]
        } else {
            active.sources.as_slice()
        };
        crate::durable_conversation::validate_completed_content(
            &active.snapshot.prompt,
            &answer,
            sources,
        )
        .map_err(|_| SettlementError::TranscriptTooLarge)?;
        let rendered_bytes = active
            .snapshot
            .prompt
            .len()
            .saturating_add(answer.text().len())
            .saturating_add(
                if matches!(answer, crate::durable_conversation::Answer::Text { .. }) {
                    crate::sources::source_footer_bytes(sources)
                } else {
                    0
                },
            );
        if rendered_bytes > MAX_COMMITTED_TEXT_BYTES {
            return Err(SettlementError::TranscriptTooLarge);
        }
        let completion = ResolvedCompletion {
            prompt: active.snapshot.prompt.clone(),
            answer,
            sources: sources.to_vec(),
            final_stage: *final_stage,
        };
        active.finalizing = true;
        Ok(completion)
    }

    pub fn settle(
        &mut self,
        execution_id: ExecutionId,
        intent: SettlementIntent,
    ) -> SettlementResult {
        let Some(active) = self.active.as_ref() else {
            return SettlementResult::IgnoredStale;
        };
        if active.snapshot.execution_id != execution_id {
            return SettlementResult::IgnoredStale;
        }
        if !matches!(intent, SettlementIntent::Success) {
            self.active = None;
            return SettlementResult::Applied(if matches!(intent, SettlementIntent::Failed) {
                AppliedOutcome::Failed
            } else {
                AppliedOutcome::Cancelled
            });
        }
        let Some((content, final_stage)) = self.active.as_ref().and_then(|a| a.resolved.clone())
        else {
            return SettlementResult::Rejected(SettlementError::NotResolved);
        };
        let sources = if matches!(content, Content::Refusal(_)) {
            Vec::new()
        } else {
            active.sources.clone()
        };
        let text = active_text_bytes(&active.snapshot.prompt, &content) + source_bytes(&sources);
        let context_text = match &content {
            Content::Text(text) => text.len() + crate::sources::source_footer_bytes(&sources),
            Content::Refusal(text) => text.len(),
        } + active.snapshot.prompt.len();
        if text > MAX_COMMITTED_TEXT_BYTES || context_text > MAX_COMMITTED_TEXT_BYTES {
            return SettlementResult::Rejected(SettlementError::TranscriptTooLarge);
        }
        let active = self.active.take().expect("active checked above");
        let user = Message {
            id: self.new_message(),
            role: Role::User,
            content: Content::Text(active.snapshot.prompt),
        };
        let answer = Message {
            id: self.new_message(),
            role: Role::Assistant,
            content: content.clone(),
        };
        let turn = Turn {
            sources,
            request_id: active.snapshot.request_id,
            execution_id,
            user,
            answer,
            final_stage,
        };
        SettlementResult::Applied(if matches!(content, Content::Refusal(_)) {
            AppliedOutcome::Refused(turn)
        } else {
            AppliedOutcome::Completed(turn)
        })
    }

    /// Finish an execution whose validated answer could not become durable history.
    /// Invoke under the same final authority as the failed/suppressed store completion.
    /// The returned payload stays with the native caller, never in retained history.
    pub fn settle_unsaved(&mut self, execution_id: ExecutionId) -> SettlementResult {
        let result = self.settle(execution_id, SettlementIntent::Success);
        if let SettlementResult::Applied(
            AppliedOutcome::Completed(turn) | AppliedOutcome::Refused(turn),
        ) = &result
        {
            self.unresolved_save = Some(UnresolvedSave {
                execution_id: turn.execution_id,
                request_id: turn.request_id.clone(),
            });
        }
        result
    }

    /// Resolve only after the durable owner has selected a fresh conversation.
    /// A failed selection changes neither execution state nor the unsaved block.
    pub fn reset_after_selection<E>(&mut self, selection: Result<(), E>) -> Result<ResetResult, E> {
        selection?;
        self.unresolved_save = None;
        Ok(self.reset())
    }

    /// Invalidate execution state. This alone does not discard an unsaved result;
    /// that requires successful durable selection via `reset_after_selection`.
    pub fn reset(&mut self) -> ResetResult {
        let old = self.session_id;
        let invalidated = self.active.take().map(|a| a.snapshot.execution_id);
        self.session_id = SessionId(self.next_session);
        self.next_session += 1;
        ResetResult {
            old_session_id: old,
            new_session_id: self.session_id,
            invalidated,
        }
    }
    fn new_message(&mut self) -> MessageId {
        let id = MessageId(self.next_message);
        self.next_message += 1;
        id
    }
}
fn active_text_bytes(prompt: &str, answer: &Content) -> usize {
    prompt.len()
        + match answer {
            Content::Text(v) | Content::Refusal(v) => v.len(),
        }
}
fn stage_result_bytes(result: &StageResult) -> usize {
    match result {
        StageResult::Instant(InstantOutcome::Complete(c) | InstantOutcome::NeedsDeep(c)) => {
            content_bytes(c)
        }
        StageResult::Instant(InstantOutcome::Failure(v))
        | StageResult::Deep(crate::assistant::DeepOutcome::Failure(v))
        | StageResult::Review(crate::assistant::ReviewOutcome::Failure(v)) => v.len(),
        StageResult::Deep(crate::assistant::DeepOutcome::Success(v)) => content_bytes(&v.answer),
        StageResult::Review(crate::assistant::ReviewOutcome::Success(
            crate::assistant::ReviewDecision::Rewrite(c),
        )) => content_bytes(c),
        StageResult::Review(crate::assistant::ReviewOutcome::Success(
            crate::assistant::ReviewDecision::Keep,
        )) => 0,
    }
}
fn content_bytes(content: &Content) -> usize {
    match content {
        Content::Text(v) | Content::Refusal(v) => v.len(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::durable_conversation::{
        Answer, Command, ContextRequest, Operation, OperationBody, OperationId, Provenance, Record,
        Store, StoreSnapshot, TerminalStage, SCHEMA_VERSION,
    };
    use crate::sources::with_source_footer;

    const REPLICA_A: &str = "00000000-0000-4000-8000-000000000001";

    struct DurableTestHistory {
        store: Store,
        conversation: OperationId,
        head: Option<OperationId>,
        next_seq: u64,
    }

    impl DurableTestHistory {
        fn new() -> Self {
            let conversation = OperationId {
                replica: REPLICA_A.into(),
                sequence: "1".into(),
            };
            let mut snapshot = StoreSnapshot::empty();
            snapshot.records.push(Record {
                body_key: format!("{:064x}", 1_u64),
                operation: Operation {
                    version: SCHEMA_VERSION,
                    id: conversation.clone(),
                    body: OperationBody::ConversationCreated {
                        title: "Synthetic conversation".into(),
                    },
                },
            });
            Self {
                store: Store::restore(snapshot).unwrap(),
                conversation,
                head: None,
                next_seq: 2,
            }
        }

        fn context_for(&self, prompt: &str) -> Vec<crate::assistant::ConversationTurn> {
            let selection = self
                .store
                .select_context(ContextRequest {
                    expected_revision: self.store.revision().to_owned(),
                    conversation: self.conversation.clone(),
                    selected_head: self.head.clone(),
                    max_turns: MAX_COMMITTED_MESSAGES / 2,
                    max_text_bytes: MAX_COMMITTED_TEXT_BYTES.saturating_sub(prompt.len()),
                })
                .unwrap();
            selection
                .turns
                .into_iter()
                .flat_map(|record| {
                    let OperationBody::TurnCompleted {
                        prompt,
                        answer,
                        sources,
                        ..
                    } = record.operation.body
                    else {
                        unreachable!()
                    };
                    [
                        crate::assistant::ConversationTurn {
                            role: Role::User,
                            content: Content::Text(prompt),
                        },
                        crate::assistant::ConversationTurn {
                            role: Role::Assistant,
                            content: match answer {
                                Answer::Text { text } => {
                                    Content::Text(with_source_footer(&text, &sources))
                                }
                                Answer::Refusal { reason } => Content::Refusal(reason),
                            },
                        },
                    ]
                })
                .collect()
        }

        fn commit_turn(&mut self, turn: &Turn) {
            let seq = self.next_seq;
            self.next_seq += 2;
            let op_id = OperationId {
                replica: REPLICA_A.into(),
                sequence: seq.to_string(),
            };
            let req_id = OperationId {
                replica: REPLICA_A.into(),
                sequence: (seq + 1).to_string(),
            };
            let Content::Text(prompt) = &turn.user.content else {
                unreachable!()
            };
            let answer = match &turn.answer.content {
                Content::Text(text) => Answer::Text { text: text.clone() },
                Content::Refusal(reason) => Answer::Refusal {
                    reason: reason.clone(),
                },
            };
            let terminal_stage = match turn.final_stage {
                Stage::Instant => TerminalStage::Instant,
                Stage::Deep => TerminalStage::Deep,
                Stage::Review => TerminalStage::Review,
            };
            let selection = self
                .store
                .select_context(ContextRequest {
                    expected_revision: self.store.revision().to_owned(),
                    conversation: self.conversation.clone(),
                    selected_head: self.head.clone(),
                    max_turns: MAX_COMMITTED_MESSAGES / 2,
                    max_text_bytes: MAX_COMMITTED_TEXT_BYTES.saturating_sub(prompt.len()),
                })
                .unwrap();
            let record = Record {
                body_key: format!("{seq:064x}"),
                operation: Operation {
                    version: SCHEMA_VERSION,
                    id: op_id.clone(),
                    body: OperationBody::TurnCompleted {
                        conversation: self.conversation.clone(),
                        parent: self.head.clone(),
                        request: req_id,
                        prompt: prompt.clone(),
                        answer,
                        sources: turn.sources.clone(),
                        terminal_stage,
                        provenance: Provenance {
                            requested_model: "synthetic-model".into(),
                            returned_model: None,
                            policy_id: "synthetic-policy".into(),
                            policy_version: "1".into(),
                            instruction_profile_id: "synthetic-profile".into(),
                            instruction_profile_version: "1".into(),
                            context: selection.ancestor_ids,
                        },
                    },
                },
            };
            let (_, next_store) = self
                .store
                .plan_with_store(Command::Import {
                    expected_revision: self.store.revision().to_owned(),
                    records: vec![record],
                })
                .unwrap();
            self.store = next_store;
            self.head = Some(op_id);
        }
    }

    #[test]
    fn request_correlation_validation_rejects_blank_and_counts_utf8_before_mutation() {
        for invalid in ["", " ", "\t\r\n", "\u{2003}"] {
            let mut session = ConversationSession::execution_only(policy());
            assert_eq!(
                validate_request_id(invalid),
                Err(AdmissionError::EmptyRequestId)
            );
            assert_eq!(
                session.admit_with_context(invalid, "Synthetic prompt", Vec::new()),
                Err(AdmissionError::EmptyRequestId)
            );
            assert_eq!((session.next_generation, session.next_message), (1, 1));
            assert!(!session.has_active_execution());
        }
        for too_long in [
            "x".repeat(MAX_REQUEST_ID_BYTES + 1),
            "😀".repeat(MAX_REQUEST_ID_BYTES / 4 + 1),
        ] {
            assert_eq!(
                validate_request_id(&too_long),
                Err(AdmissionError::RequestIdTooLong)
            );
        }
        for accepted in [
            "synthetic-correlation".to_owned(),
            " synthetic preserved correlation ".to_owned(),
            "😀".repeat(MAX_REQUEST_ID_BYTES / 4),
        ] {
            let mut session = ConversationSession::execution_only(policy());
            assert_eq!(validate_request_id(&accepted), Ok(()));
            assert_eq!(
                session
                    .admit_with_context(&accepted, "Synthetic prompt", Vec::new())
                    .unwrap()
                    .request_id
                    .0,
                accepted
            );
        }
    }
    #[test]
    fn durable_completion_capture_freezes_stage_and_sources_but_not_cancellation() {
        let mut session = ConversationSession::execution_only(policy());
        let admitted = session
            .admit_with_context("synthetic-finalizing", "Synthetic prompt", Vec::new())
            .unwrap();
        complete(
            &mut session,
            admitted.execution_id,
            Content::Text("Synthetic captured answer".into()),
        );
        let completion = session.begin_settlement(admitted.execution_id).unwrap();
        assert_eq!(completion.answer.text(), "Synthetic captured answer");
        assert!(completion.sources.is_empty());
        assert!(!session
            .observe_sources(
                admitted.execution_id,
                &[SourceLink {
                    url: "https://example.test/synthetic-late".into(),
                    title: "Synthetic late".into()
                }]
            )
            .unwrap());
        assert_eq!(
            session.update(
                admitted.execution_id,
                Stage::Instant,
                StageResult::Instant(InstantOutcome::Complete(Content::Text(
                    "Synthetic late answer".into()
                )))
            ),
            UpdateResult::IgnoredStale
        );
        assert!(session.begin_settlement(admitted.execution_id).is_err());
        assert_eq!(
            session.settle(admitted.execution_id, SettlementIntent::Cancelled),
            SettlementResult::Applied(AppliedOutcome::Cancelled)
        );
        assert!(session.unresolved_save().is_none());
        assert!(session.active_summary().is_none());
    }
    #[test]
    fn unsaved_settlement_blocks_admission_until_successful_fresh_selection() {
        let mut session = ConversationSession::execution_only(policy());
        let admitted = session
            .admit_with_context("synthetic-unsaved", "Synthetic prompt", Vec::new())
            .unwrap();
        complete(
            &mut session,
            admitted.execution_id,
            Content::Text("Synthetic unsaved answer".into()),
        );
        assert!(matches!(
            session.settle_unsaved(admitted.execution_id),
            SettlementResult::Applied(AppliedOutcome::Completed(_))
        ));
        assert_eq!(
            session.unresolved_save().unwrap().execution_id,
            admitted.execution_id
        );
        assert_eq!(
            session.unresolved_save().unwrap().request_id,
            admitted.request_id
        );
        let counters = (session.next_generation, session.next_message);
        assert_eq!(
            session.admit_with_context("synthetic-next", "Synthetic follow-up", vec![]),
            Err(AdmissionError::UnresolvedSave)
        );
        assert_eq!((session.next_generation, session.next_message), counters);
        let before_session_id = session.session_id();
        assert_eq!(
            session.reset_after_selection(Err("synthetic selection failure")),
            Err("synthetic selection failure")
        );
        assert_eq!(session.session_id(), before_session_id);
        assert!(session.unresolved_save().is_some());
        session.reset();
        assert_eq!(
            session.admit_with_context("synthetic-next", "Synthetic follow-up", vec![]),
            Err(AdmissionError::UnresolvedSave)
        );
        session.reset_after_selection(Ok::<_, ()>(())).unwrap();
        assert!(session.unresolved_save().is_none());
        assert!(session
            .admit_with_context("synthetic-next", "Synthetic follow-up", vec![])
            .is_ok());
    }

    #[test]
    fn unsaved_settlement_rejects_unresolved_or_stale_work() {
        let mut session = ConversationSession::execution_only(policy());
        let admitted = session
            .admit_with_context("synthetic-unsaved", "Synthetic prompt", Vec::new())
            .unwrap();
        assert_eq!(
            session.settle_unsaved(admitted.execution_id),
            SettlementResult::Rejected(SettlementError::NotResolved)
        );
        assert!(session.unresolved_save().is_none());
        session.settle(admitted.execution_id, SettlementIntent::Cancelled);
        assert_eq!(
            session.settle_unsaved(admitted.execution_id),
            SettlementResult::IgnoredStale
        );
        assert!(session.unresolved_save().is_none());
    }
    #[test]
    fn external_context_is_validated_and_pinned() {
        use crate::assistant::ConversationTurn;
        let pair = vec![
            ConversationTurn {
                role: Role::User,
                content: Content::Text("Synthetic prior".into()),
            },
            ConversationTurn {
                role: Role::Assistant,
                content: Content::Refusal("Synthetic refusal".into()),
            },
        ];
        let mut session = ConversationSession::execution_only(policy());
        let mut oversized = pair.clone();
        oversized[0].content = Content::Text("😀".repeat(MAX_COMMITTED_TEXT_BYTES / 4));
        let mut reversed = pair.clone();
        reversed.swap(0, 1);
        let mut refused_user = pair.clone();
        refused_user[0].content = Content::Refusal("Synthetic invalid user".into());
        for invalid in [
            vec![pair[0].clone()],
            vec![pair.clone(); 21].concat(),
            oversized,
            reversed,
            refused_user,
        ] {
            assert_eq!(
                session.admit_with_context("synthetic", "prompt", invalid),
                Err(AdmissionError::InvalidContext)
            );
            assert_eq!((session.next_generation, session.next_message), (1, 1));
            assert!(!session.has_active_execution());
        }
        let pinned = pair.clone();
        let pinned_allocation = pinned.as_ptr();
        let admitted = session
            .admit_with_context("synthetic", "prompt", pinned)
            .unwrap();
        assert_eq!(admitted.history.as_ptr(), pinned_allocation);
        assert_eq!(admitted.history, pair);
        assert_eq!(admitted.execution_id.generation, GenerationId(1));
        complete(
            &mut session,
            admitted.execution_id,
            Content::Text("Synthetic answer".into()),
        );
        assert!(matches!(
            session.settle(admitted.execution_id, SettlementIntent::Success),
            SettlementResult::Applied(AppliedOutcome::Completed(_))
        ));
        let next = session
            .admit_with_context("synthetic-next", "prompt", Vec::new())
            .unwrap();
        assert!(next.history.is_empty());
    }

    #[test]
    fn external_context_budget_counts_utf8_prompt_exactly_once() {
        use crate::assistant::ConversationTurn;
        let mut session = ConversationSession::execution_only(policy());
        let history = vec![
            ConversationTurn {
                role: Role::User,
                content: Content::Text("😀".into()),
            },
            ConversationTurn {
                role: Role::Assistant,
                content: Content::Text("a".repeat(MAX_COMMITTED_TEXT_BYTES - 5)),
            },
        ];
        assert_eq!(
            session.admit_with_context("synthetic", "ab", history.clone()),
            Err(AdmissionError::InvalidContext)
        );
        assert_eq!(session.next_generation, 1);
        assert!(session
            .admit_with_context("synthetic", "a", history)
            .is_ok());
    }
    fn policy() -> AssistantPolicy {
        AssistantPolicy::default()
    }
    #[test]
    fn externally_owned_history_is_never_cached_by_the_execution_coordinator() {
        let mut session = ConversationSession::execution_only(policy());
        let first = session
            .admit_with_context("synthetic-first", "Synthetic prompt", Vec::new())
            .unwrap();
        complete(
            &mut session,
            first.execution_id,
            Content::Text("Synthetic answer".into()),
        );
        assert!(matches!(
            session.settle(first.execution_id, SettlementIntent::Success),
            SettlementResult::Applied(AppliedOutcome::Completed(_))
        ));
        let next = session
            .admit_with_context("synthetic-next", "Synthetic follow-up", Vec::new())
            .unwrap();
        assert!(next.history.is_empty());
        assert_ne!(first.execution_id, next.execution_id);
        assert_eq!(session.active_execution(), Some(next.execution_id));
    }
    #[test]
    fn source_footer_overhead_cannot_oversize_admission_context() {
        let mut session = ConversationSession::execution_only(policy());
        let execution = session
            .admit_with_context("synthetic-budget", "p", Vec::new())
            .unwrap()
            .execution_id;
        let sources = vec![SourceLink {
            url: "https://example.test/e".into(),
            title: "S".into(),
        }];
        session.observe_sources(execution, &sources).unwrap();
        let answer = "a".repeat(MAX_COMMITTED_TEXT_BYTES - 1 - source_bytes(&sources));
        complete(&mut session, execution, Content::Text(answer));
        assert_eq!(
            session.settle(execution, SettlementIntent::Success),
            SettlementResult::Rejected(SettlementError::TranscriptTooLarge)
        );
        assert_eq!(session.active_execution(), Some(execution));
    }

    #[test]
    fn observed_sources_are_canonical_but_context_is_derived_and_stale_is_inert() {
        let mut session = ConversationSession::execution_only(policy());
        let mut durable = DurableTestHistory::new();
        let source = SourceLink {
            url: "https://example.test/evidence".into(),
            title: "Synthetic evidence".into(),
        };
        let first = session
            .admit_with_context(
                "synthetic-first",
                "question",
                durable.context_for("question"),
            )
            .unwrap()
            .execution_id;
        assert_eq!(
            session.observe_sources(first, std::slice::from_ref(&source)),
            Ok(true)
        );
        assert!(session
            .observe_sources(
                first,
                &[
                    SourceLink {
                        url: "https://example.test/new".into(),
                        title: "New".into()
                    },
                    SourceLink {
                        url: "invalid".into(),
                        title: "Invalid".into()
                    }
                ]
            )
            .is_err());
        complete(&mut session, first, Content::Text("plain answer".into()));
        let SettlementResult::Applied(AppliedOutcome::Completed(first_turn)) =
            session.settle(first, SettlementIntent::Success)
        else {
            panic!("expected completed first turn")
        };
        assert_eq!(
            first_turn.answer.content,
            Content::Text("plain answer".into())
        );
        assert_eq!(first_turn.sources, vec![source.clone()]);
        durable.commit_turn(&first_turn);

        let provider = durable.context_for("follow up");
        let next = session
            .admit_with_context("synthetic-next", "follow up", provider.clone())
            .unwrap();
        assert_eq!(next.history[1].content, provider[1].content);
        assert_eq!(
            provider[1].content,
            Content::Text(with_source_footer(
                "plain answer",
                std::slice::from_ref(&source)
            ))
        );
        assert_eq!(
            session.observe_sources(
                first,
                &[SourceLink {
                    url: "bad".into(),
                    title: "bad".into()
                }]
            ),
            Ok(false)
        );
        assert!(session
            .observe_sources(next.execution_id, &vec![source.clone(); 33])
            .is_err());
        assert_eq!(
            session.observe_sources(next.execution_id, &[source]),
            Ok(true)
        );
        complete(
            &mut session,
            next.execution_id,
            Content::Refusal("no".into()),
        );
        let SettlementResult::Applied(AppliedOutcome::Refused(second_turn)) =
            session.settle(next.execution_id, SettlementIntent::Success)
        else {
            panic!("expected refused second turn")
        };
        assert!(second_turn.sources.is_empty());
        durable.commit_turn(&second_turn);

        let cancelled = session
            .admit_with_context(
                "synthetic-cancel",
                "question",
                durable.context_for("question"),
            )
            .unwrap()
            .execution_id;
        session.settle(cancelled, SettlementIntent::Cancelled);
        assert_eq!(session.observe_sources(cancelled, &[]), Ok(false));
        assert_eq!(durable.context_for("question").len(), 4);
    }
    fn complete(s: &mut ConversationSession, e: ExecutionId, content: Content) {
        assert!(matches!(
            s.update(
                e,
                Stage::Instant,
                StageResult::Instant(InstantOutcome::Complete(content))
            ),
            UpdateResult::Resolved { .. }
        ));
    }

    #[test]
    fn admission_snapshot_preserves_identity_history_and_policy() {
        let mut s = ConversationSession::execution_only(policy());
        let mut durable = DurableTestHistory::new();
        assert_eq!(
            s.admit_with_context("", "hello", Vec::new()),
            Err(AdmissionError::EmptyRequestId)
        );
        let snapshot = s
            .admit_with_context("frontend/id/1", "hello", durable.context_for("hello"))
            .unwrap();
        assert_eq!(snapshot.request_id, RequestId("frontend/id/1".into()));
        assert_eq!(snapshot.execution_id.session_id, s.session_id());
        assert!(snapshot.history.is_empty());
        assert_eq!(
            s.replace_policy(AssistantPolicy {
                deep_enabled: true,
                ..policy()
            }),
            Err(AdmissionError::Busy)
        );
        complete(
            &mut s,
            snapshot.execution_id,
            Content::Text("answer".into()),
        );
        let SettlementResult::Applied(AppliedOutcome::Completed(turn)) =
            s.settle(snapshot.execution_id, SettlementIntent::Success)
        else {
            panic!("expected completed turn")
        };
        durable.commit_turn(&turn);
        s.replace_policy(AssistantPolicy {
            deep_enabled: true,
            ..policy()
        })
        .unwrap();
        let next = s
            .admit_with_context("next", "question", durable.context_for("question"))
            .unwrap();
        assert_eq!(next.history.len(), 2);
        assert!(next.policy.deep_enabled);
    }

    #[test]
    fn cancel_fail_and_rejected_success_have_typed_lifecycle() {
        let mut s = ConversationSession::execution_only(policy());
        let a = s.admit_with_context("a", "a", Vec::new()).unwrap();
        assert_eq!(
            s.update(
                a.execution_id,
                Stage::Instant,
                StageResult::Deep(crate::assistant::DeepOutcome::Failure("wrong".into()))
            ),
            UpdateResult::WrongStage
        );
        assert_eq!(
            s.settle(a.execution_id, SettlementIntent::Success),
            SettlementResult::Rejected(SettlementError::NotResolved)
        );
        assert_eq!(s.active_execution(), Some(a.execution_id));
        assert_eq!(
            s.settle(a.execution_id, SettlementIntent::Cancelled),
            SettlementResult::Applied(AppliedOutcome::Cancelled)
        );
        assert_eq!(
            s.settle(a.execution_id, SettlementIntent::Cancelled),
            SettlementResult::IgnoredStale
        );
        let b = s.admit_with_context("b", "b", Vec::new()).unwrap();
        assert_eq!(
            s.settle(b.execution_id, SettlementIntent::Failed),
            SettlementResult::Applied(AppliedOutcome::Failed)
        );
        assert!(!s.has_active_execution());
    }

    #[test]
    fn refusal_derives_terminal_type_and_review_metadata_is_order_independent() {
        let mut s = ConversationSession::execution_only(AssistantPolicy {
            review_enabled: true,
            ..policy()
        });
        let e = s.admit_with_context("r", "p", Vec::new()).unwrap();
        assert!(matches!(
            s.update(
                e.execution_id,
                Stage::Review,
                StageResult::Review(crate::assistant::ReviewOutcome::Success(
                    crate::assistant::ReviewDecision::Rewrite(Content::Refusal("no".into()))
                ))
            ),
            UpdateResult::Pending
        ));
        assert!(matches!(
            s.update(
                e.execution_id,
                Stage::Instant,
                StageResult::Instant(InstantOutcome::Complete(Content::Text("i".into())))
            ),
            UpdateResult::Resolved {
                final_stage: Stage::Review,
                ..
            }
        ));
        assert!(matches!(
            s.settle(e.execution_id, SettlementIntent::Success),
            SettlementResult::Applied(AppliedOutcome::Refused(_))
        ));
    }

    #[test]
    fn deep_and_review_matrix_is_preserved() {
        use crate::assistant::{DeepReport, ReviewDecision, ReviewOutcome};

        for deep_enabled in [false, true] {
            for review_enabled in [false, true] {
                let mut session = ConversationSession::execution_only(AssistantPolicy {
                    deep_enabled,
                    review_enabled,
                    ..policy()
                });
                let execution = session
                    .admit_with_context("request", "prompt", Vec::new())
                    .unwrap()
                    .execution_id;
                let instant = session.update(
                    execution,
                    Stage::Instant,
                    StageResult::Instant(InstantOutcome::NeedsDeep(Content::Text(
                        "instant".into(),
                    ))),
                );
                assert_eq!(
                    instant,
                    if deep_enabled || review_enabled {
                        UpdateResult::Provisional(Content::Text("instant".into()))
                    } else {
                        UpdateResult::Resolved {
                            content: Content::Text("instant".into()),
                            final_stage: Stage::Instant,
                        }
                    }
                );

                if deep_enabled {
                    assert_eq!(
                        session.update(
                            execution,
                            Stage::Deep,
                            StageResult::Deep(DeepOutcome::Success(DeepReport {
                                answer: Content::Text("deep".into()),
                            })),
                        ),
                        if review_enabled {
                            UpdateResult::Pending
                        } else {
                            UpdateResult::Resolved {
                                content: Content::Text("deep".into()),
                                final_stage: Stage::Deep,
                            }
                        }
                    );
                } else {
                    assert_eq!(
                        session.update(
                            execution,
                            Stage::Deep,
                            StageResult::Deep(DeepOutcome::Failure("disabled".into())),
                        ),
                        UpdateResult::WrongStage
                    );
                }

                if review_enabled {
                    assert_eq!(
                        session.update(
                            execution,
                            Stage::Review,
                            StageResult::Review(ReviewOutcome::Success(ReviewDecision::Keep)),
                        ),
                        UpdateResult::Resolved {
                            content: Content::Text(if deep_enabled {
                                "deep".into()
                            } else {
                                "instant".into()
                            }),
                            final_stage: Stage::Review,
                        }
                    );
                } else {
                    assert_eq!(
                        session.update(
                            execution,
                            Stage::Review,
                            StageResult::Review(ReviewOutcome::Failure("disabled".into())),
                        ),
                        UpdateResult::WrongStage
                    );
                }

                assert!(matches!(
                    session.settle(execution, SettlementIntent::Success),
                    SettlementResult::Applied(AppliedOutcome::Completed(_))
                ));
            }
        }
    }

    #[test]
    fn failures_and_review_fallbacks_are_preserved() {
        use crate::assistant::{DeepReport, ReviewOutcome};

        for (deep_enabled, review_enabled) in
            [(false, false), (true, false), (false, true), (true, true)]
        {
            let mut session = ConversationSession::execution_only(AssistantPolicy {
                deep_enabled,
                review_enabled,
                ..policy()
            });
            let execution = session
                .admit_with_context("request", "prompt", Vec::new())
                .unwrap()
                .execution_id;
            assert_eq!(
                session.update(
                    execution,
                    Stage::Instant,
                    StageResult::Instant(InstantOutcome::Failure("instant failed".into())),
                ),
                UpdateResult::Pending
            );
            if deep_enabled {
                assert_eq!(
                    session.update(
                        execution,
                        Stage::Deep,
                        StageResult::Deep(DeepOutcome::Failure("deep failed".into())),
                    ),
                    UpdateResult::Pending
                );
            }
            if review_enabled {
                assert_eq!(
                    session.update(
                        execution,
                        Stage::Review,
                        StageResult::Review(ReviewOutcome::Failure("review failed".into())),
                    ),
                    UpdateResult::Pending
                );
            }
            assert_eq!(
                session.settle(execution, SettlementIntent::Success),
                SettlementResult::Rejected(SettlementError::NotResolved)
            );
            assert_eq!(
                session.settle(execution, SettlementIntent::Failed),
                SettlementResult::Applied(AppliedOutcome::Failed)
            );
        }

        let mut session = ConversationSession::execution_only(AssistantPolicy {
            deep_enabled: true,
            review_enabled: true,
            ..policy()
        });
        let execution = session
            .admit_with_context("fallback", "prompt", Vec::new())
            .unwrap()
            .execution_id;
        assert!(matches!(
            session.update(
                execution,
                Stage::Instant,
                StageResult::Instant(InstantOutcome::Complete(Content::Text("instant".into())))
            ),
            UpdateResult::Provisional(_)
        ));
        assert_eq!(
            session.update(
                execution,
                Stage::Deep,
                StageResult::Deep(DeepOutcome::Success(DeepReport {
                    answer: Content::Text("deep".into()),
                }))
            ),
            UpdateResult::Pending
        );
        assert_eq!(
            session.update(
                execution,
                Stage::Review,
                StageResult::Review(ReviewOutcome::Failure("review failed".into()))
            ),
            UpdateResult::Resolved {
                content: Content::Text("deep".into()),
                final_stage: Stage::Review,
            }
        );
    }

    #[test]
    fn reset_rotates_session_and_active_summary_tracks_transitions() {
        let mut s = ConversationSession::execution_only(policy());
        assert!(s.active_summary().is_none());
        let empty_reset = s.reset();
        assert_ne!(empty_reset.old_session_id, empty_reset.new_session_id);
        let e = s.admit_with_context("r", "p", Vec::new()).unwrap();
        assert_eq!(
            s.active_summary().as_ref().unwrap().execution_id,
            e.execution_id
        );
        let reset = s.reset();
        assert_ne!(reset.old_session_id, reset.new_session_id);
        assert!(s.active_summary().is_none());
        assert_eq!(
            s.update(
                e.execution_id,
                Stage::Instant,
                StageResult::Instant(InstantOutcome::Complete(Content::Text("late".into())))
            ),
            UpdateResult::IgnoredStale
        );
    }

    #[test]
    fn oversized_update_and_whole_turn_bounds_are_explicit() {
        let mut s = ConversationSession::execution_only(policy());
        let mut durable = DurableTestHistory::new();
        let e = s.admit_with_context("r", "p", Vec::new()).unwrap();
        assert_eq!(
            s.update(
                e.execution_id,
                Stage::Instant,
                StageResult::Instant(InstantOutcome::Complete(Content::Text(
                    "x".repeat(MAX_UPDATE_BYTES + 1)
                )))
            ),
            UpdateResult::RejectedOversized
        );
        assert_eq!(
            s.settle(e.execution_id, SettlementIntent::Cancelled),
            SettlementResult::Applied(AppliedOutcome::Cancelled)
        );
        for i in 0..21 {
            let e = s
                .admit_with_context(&i.to_string(), "p", durable.context_for("p"))
                .unwrap();
            complete(&mut s, e.execution_id, Content::Text("a".into()));
            let SettlementResult::Applied(AppliedOutcome::Completed(turn)) =
                s.settle(e.execution_id, SettlementIntent::Success)
            else {
                panic!("expected completed turn")
            };
            durable.commit_turn(&turn);
        }
        assert_eq!(durable.context_for("p").len(), MAX_COMMITTED_MESSAGES);

        let mut byte_bounded = ConversationSession::execution_only(policy());
        let mut byte_durable = DurableTestHistory::new();
        for i in 0..9 {
            let prompt = format!("p-{i}");
            let e = byte_bounded
                .admit_with_context(&i.to_string(), &prompt, byte_durable.context_for(&prompt))
                .unwrap();
            complete(
                &mut byte_bounded,
                e.execution_id,
                Content::Text("a".repeat(32 * 1024)),
            );
            let SettlementResult::Applied(AppliedOutcome::Completed(turn)) =
                byte_bounded.settle(e.execution_id, SettlementIntent::Success)
            else {
                panic!("expected completed turn")
            };
            byte_durable.commit_turn(&turn);
        }
        let trimmed_context = byte_durable.context_for("p");
        assert_eq!(trimmed_context.len(), 14);
        assert_eq!(trimmed_context[0].content, Content::Text("p-2".into()));

        let too_large = byte_bounded
            .admit_with_context("large", &"p".repeat(MAX_PROMPT_BYTES), Vec::new())
            .unwrap();
        complete(
            &mut byte_bounded,
            too_large.execution_id,
            Content::Text("a".repeat(MAX_UPDATE_BYTES)),
        );
        assert_eq!(
            byte_bounded.settle(too_large.execution_id, SettlementIntent::Success),
            SettlementResult::Rejected(SettlementError::TranscriptTooLarge)
        );
        assert_eq!(
            byte_bounded.active_execution(),
            Some(too_large.execution_id)
        );
    }
}
