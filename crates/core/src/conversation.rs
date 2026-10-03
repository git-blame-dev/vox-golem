use crate::assistant::{
    resolve_with_stage, AssistantPolicy, Content, DeepOutcome, InstantOutcome, Role, Stage,
    StageResult,
};
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
    pub history: Vec<Message>,
    pub policy: AssistantPolicy,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActiveExecutionSummary {
    pub execution_id: ExecutionId,
    pub request_id: RequestId,
    pub prompt: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConversationSnapshot {
    pub session_id: SessionId,
    pub turns: Vec<Turn>,
    pub active: Option<ActiveExecutionSummary>,
}

#[derive(Debug, Clone)]
struct Active {
    snapshot: AdmissionSnapshot,
    instant: Option<InstantOutcome>,
    deep: Option<DeepOutcome>,
    review: Option<crate::assistant::ReviewOutcome>,
    resolved: Option<(Content, Stage)>,
}
#[derive(Debug, Clone)]
pub struct ConversationSession {
    session_id: SessionId,
    policy: AssistantPolicy,
    active: Option<Active>,
    turns: Vec<Turn>,
    text_bytes: usize,
    next_session: u64,
    next_generation: u64,
    next_message: u64,
}

impl ConversationSession {
    pub fn new(policy: AssistantPolicy) -> Self {
        Self {
            session_id: SessionId(1),
            policy,
            active: None,
            turns: Vec::new(),
            text_bytes: 0,
            next_session: 2,
            next_generation: 1,
            next_message: 1,
        }
    }
    pub fn session_id(&self) -> SessionId {
        self.session_id
    }
    pub fn policy(&self) -> &AssistantPolicy {
        &self.policy
    }
    pub fn turns(&self) -> &[Turn] {
        &self.turns
    }
    pub fn has_active_execution(&self) -> bool {
        self.active.is_some()
    }
    pub fn provider_history(&self) -> Vec<crate::assistant::ConversationTurn> {
        self.turns
            .iter()
            .flat_map(|turn| {
                [
                    crate::assistant::ConversationTurn {
                        role: Role::User,
                        content: turn.user.content.clone(),
                    },
                    crate::assistant::ConversationTurn {
                        role: Role::Assistant,
                        content: turn.answer.content.clone(),
                    },
                ]
            })
            .collect()
    }
    pub fn active_execution(&self) -> Option<ExecutionId> {
        self.active.as_ref().map(|a| a.snapshot.execution_id)
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
    pub fn snapshot(&self) -> ConversationSnapshot {
        ConversationSnapshot {
            session_id: self.session_id,
            turns: self.turns.clone(),
            active: self.active.as_ref().map(|a| ActiveExecutionSummary {
                execution_id: a.snapshot.execution_id,
                request_id: a.snapshot.request_id.clone(),
                prompt: a.snapshot.prompt.clone(),
            }),
        }
    }
    pub fn admit(
        &mut self,
        request_id: &str,
        prompt: &str,
    ) -> Result<AdmissionSnapshot, AdmissionError> {
        if request_id.is_empty() {
            return Err(AdmissionError::EmptyRequestId);
        }
        if request_id.len() > MAX_REQUEST_ID_BYTES {
            return Err(AdmissionError::RequestIdTooLong);
        }
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
        let history = self
            .turns
            .iter()
            .flat_map(|t| [t.user.clone(), t.answer.clone()])
            .collect();
        let snapshot = AdmissionSnapshot {
            execution_id,
            request_id: RequestId(request_id.into()),
            prompt: prompt.into(),
            history,
            policy: self.policy.clone(),
        };
        self.active = Some(Active {
            instant: None,
            deep: None,
            review: None,
            snapshot: snapshot.clone(),
            resolved: None,
        });
        Ok(snapshot)
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
        if active.snapshot.execution_id != execution_id {
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
        let text = active_text_bytes(&active.snapshot.prompt, &content);
        if text > MAX_COMMITTED_TEXT_BYTES {
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
            request_id: active.snapshot.request_id,
            execution_id,
            user,
            answer,
            final_stage,
        };
        self.text_bytes += text;
        self.turns.push(turn.clone());
        self.trim();
        SettlementResult::Applied(if matches!(content, Content::Refusal(_)) {
            AppliedOutcome::Refused(turn)
        } else {
            AppliedOutcome::Completed(turn)
        })
    }
    pub fn reset(&mut self) -> ResetResult {
        let old = self.session_id;
        let invalidated = self.active.take().map(|a| a.snapshot.execution_id);
        self.turns.clear();
        self.text_bytes = 0;
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
    fn trim(&mut self) {
        while self.turns.len() * 2 > MAX_COMMITTED_MESSAGES
            || self.text_bytes > MAX_COMMITTED_TEXT_BYTES
        {
            let old = self.turns.remove(0);
            self.text_bytes -=
                content_bytes(&old.user.content) + content_bytes(&old.answer.content);
        }
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
    fn policy() -> AssistantPolicy {
        AssistantPolicy::default()
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
        let mut s = ConversationSession::new(policy());
        assert_eq!(s.admit("", "hello"), Err(AdmissionError::EmptyRequestId));
        let snapshot = s.admit("frontend/id/1", "hello").unwrap();
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
        assert!(matches!(
            s.settle(snapshot.execution_id, SettlementIntent::Success),
            SettlementResult::Applied(AppliedOutcome::Completed(_))
        ));
        s.replace_policy(AssistantPolicy {
            deep_enabled: true,
            ..policy()
        })
        .unwrap();
        let next = s.admit("next", "question").unwrap();
        assert_eq!(next.history.len(), 2);
        assert!(next.policy.deep_enabled);
    }

    #[test]
    fn cancel_fail_and_rejected_success_have_typed_lifecycle() {
        let mut s = ConversationSession::new(policy());
        let a = s.admit("a", "a").unwrap();
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
        let b = s.admit("b", "b").unwrap();
        assert_eq!(
            s.settle(b.execution_id, SettlementIntent::Failed),
            SettlementResult::Applied(AppliedOutcome::Failed)
        );
        assert!(s.turns().is_empty());
    }

    #[test]
    fn refusal_derives_terminal_type_and_review_metadata_is_order_independent() {
        let mut s = ConversationSession::new(AssistantPolicy {
            review_enabled: true,
            ..policy()
        });
        let e = s.admit("r", "p").unwrap();
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
                let mut session = ConversationSession::new(AssistantPolicy {
                    deep_enabled,
                    review_enabled,
                    ..policy()
                });
                let execution = session.admit("request", "prompt").unwrap().execution_id;
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
            let mut session = ConversationSession::new(AssistantPolicy {
                deep_enabled,
                review_enabled,
                ..policy()
            });
            let execution = session.admit("request", "prompt").unwrap().execution_id;
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

        let mut session = ConversationSession::new(AssistantPolicy {
            deep_enabled: true,
            review_enabled: true,
            ..policy()
        });
        let execution = session.admit("fallback", "prompt").unwrap().execution_id;
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
    fn reset_rotates_session_and_snapshots_track_transitions() {
        let mut s = ConversationSession::new(policy());
        assert!(s.snapshot().active.is_none());
        let empty_reset = s.reset();
        assert_ne!(empty_reset.old_session_id, empty_reset.new_session_id);
        let e = s.admit("r", "p").unwrap();
        assert_eq!(
            s.snapshot().active.as_ref().unwrap().execution_id,
            e.execution_id
        );
        let reset = s.reset();
        assert_ne!(reset.old_session_id, reset.new_session_id);
        assert!(s.snapshot().active.is_none());
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
        let mut s = ConversationSession::new(policy());
        let e = s.admit("r", "p").unwrap();
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
            let e = s.admit(&i.to_string(), "p").unwrap();
            complete(&mut s, e.execution_id, Content::Text("a".into()));
            assert!(matches!(
                s.settle(e.execution_id, SettlementIntent::Success),
                SettlementResult::Applied(_)
            ));
        }
        assert_eq!(s.snapshot().turns.len(), 20);

        let mut byte_bounded = ConversationSession::new(policy());
        for i in 0..9 {
            let e = byte_bounded.admit(&i.to_string(), "p").unwrap();
            complete(
                &mut byte_bounded,
                e.execution_id,
                Content::Text("a".repeat(32 * 1024)),
            );
            assert!(matches!(
                byte_bounded.settle(e.execution_id, SettlementIntent::Success),
                SettlementResult::Applied(_)
            ));
        }
        assert_eq!(byte_bounded.turns().len(), 7);
        assert_eq!(byte_bounded.turns()[0].request_id, RequestId("2".into()));

        let too_large = byte_bounded
            .admit("large", &"p".repeat(MAX_PROMPT_BYTES))
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
