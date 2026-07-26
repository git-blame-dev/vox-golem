//! Provider-neutral, synchronous assistant coordination.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum InstantModel {
    LocalFast,
    LocalQuality,
    CustomSolHigh,
    CustomLunaLow,
    OpenCodeSolHigh,
    OpenCodeLunaLow,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AgentModel {
    CustomSolHigh,
    CustomLunaLow,
    OpenCodeSolHigh,
    OpenCodeLunaLow,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provider {
    Local,
    Custom,
    OpenCode,
}

impl InstantModel {
    pub fn provider(self) -> Provider {
        match self {
            Self::LocalFast | Self::LocalQuality => Provider::Local,
            Self::CustomSolHigh | Self::CustomLunaLow => Provider::Custom,
            Self::OpenCodeSolHigh | Self::OpenCodeLunaLow => Provider::OpenCode,
        }
    }
}
impl AgentModel {
    pub fn provider(self) -> Provider {
        match self {
            Self::CustomSolHigh | Self::CustomLunaLow => Provider::Custom,
            Self::OpenCodeSolHigh | Self::OpenCodeLunaLow => Provider::OpenCode,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssistantPreferences {
    pub instant_model: InstantModel,
    pub deep_model: AgentModel,
    pub review_model: AgentModel,
    pub deep_enabled: bool,
    pub review_enabled: bool,
    pub prefetch_enabled: bool,
    pub completion_enabled: bool,
}
impl Default for AssistantPreferences {
    fn default() -> Self {
        Self {
            instant_model: InstantModel::LocalFast,
            deep_model: AgentModel::OpenCodeSolHigh,
            review_model: AgentModel::OpenCodeSolHigh,
            deep_enabled: false,
            review_enabled: false,
            prefetch_enabled: false,
            completion_enabled: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Content {
    Text(String),
    Refusal(String),
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Role {
    User,
    Assistant,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConversationTurn {
    pub role: Role,
    pub content: Content,
}
pub fn request(prompt: impl Into<String>, history: &[ConversationTurn]) -> Vec<ConversationTurn> {
    let mut turns = history.to_vec();
    turns.push(ConversationTurn {
        role: Role::User,
        content: Content::Text(prompt.into()),
    });
    turns
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    Instant,
    Deep,
    Review,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstantOutcome {
    Complete(Content),
    NeedsDeep(Content),
    Failure(String),
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeepReport {
    pub answer: Content,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeepOutcome {
    Success(DeepReport),
    Failure(String),
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReviewDecision {
    Keep,
    Rewrite(Content),
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReviewOutcome {
    Success(ReviewDecision),
    Failure(String),
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Generation {
    pub id: u64,
    pub cancelled: bool,
}
impl Generation {
    pub fn new(id: u64) -> Self {
        Self {
            id,
            cancelled: false,
        }
    }
    pub fn cancel(self) -> Self {
        Self {
            cancelled: true,
            ..self
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StageResult {
    Instant(InstantOutcome),
    Deep(DeepOutcome),
    Review(ReviewOutcome),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssistantState {
    pub generation: Generation,
    pub instant: Option<InstantOutcome>,
    pub deep: Option<DeepOutcome>,
    pub review: Option<ReviewOutcome>,
    pub final_answer: Option<Content>,
}
impl AssistantState {
    pub fn new(generation: Generation) -> Self {
        Self {
            generation,
            instant: None,
            deep: None,
            review: None,
            final_answer: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartError {
    Busy,
    EmptyPrompt,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AcceptResult {
    Pending,
    Provisional(Content),
    Resolved(Content),
    Stale,
    Cancelled,
    WrongStage,
    DuplicateStage,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssistantCoordinator {
    preferences: AssistantPreferences,
    history: Vec<ConversationTurn>,
    active: Option<AssistantState>,
    active_prompt: Option<Content>,
    next_generation: u64,
}
impl AssistantCoordinator {
    pub fn new(preferences: AssistantPreferences) -> Self {
        Self {
            preferences,
            history: Vec::new(),
            active: None,
            active_prompt: None,
            next_generation: 1,
        }
    }
    pub fn preferences(&self) -> &AssistantPreferences {
        &self.preferences
    }
    pub fn history(&self) -> &[ConversationTurn] {
        &self.history
    }
    pub fn active(&self) -> Option<&AssistantState> {
        self.active.as_ref()
    }
    pub fn provisional_instant(&self) -> Option<&InstantOutcome> {
        self.active.as_ref().and_then(|s| s.instant.as_ref())
    }
    pub fn set_preferences(&mut self, preferences: AssistantPreferences) -> Result<(), StartError> {
        if self.active.is_some() {
            return Err(StartError::Busy);
        }
        self.preferences = preferences;
        Ok(())
    }
    pub fn start(&mut self, prompt: impl Into<String>) -> Result<Generation, StartError> {
        let prompt = prompt.into();
        if self.active.is_some() {
            return Err(StartError::Busy);
        }
        if prompt.trim().is_empty() {
            return Err(StartError::EmptyPrompt);
        }
        let generation = Generation::new(self.next_generation);
        self.next_generation += 1;
        self.active = Some(AssistantState::new(generation));
        self.active_prompt = Some(Content::Text(prompt));
        Ok(generation)
    }
    pub fn cancel(&mut self, generation: Generation) -> bool {
        if self
            .active
            .as_ref()
            .is_some_and(|s| s.generation == generation)
        {
            self.active = None;
            self.active_prompt = None;
            true
        } else {
            false
        }
    }
    pub fn accept(
        &mut self,
        generation: Generation,
        stage: Stage,
        result: StageResult,
    ) -> AcceptResult {
        let Some(state) = self.active.as_mut() else {
            return AcceptResult::Stale;
        };
        if state.generation != generation {
            return AcceptResult::Stale;
        }
        if generation.cancelled {
            return AcceptResult::Cancelled;
        }
        if matches!(stage, Stage::Deep) && !self.preferences.deep_enabled
            || matches!(stage, Stage::Review) && !self.preferences.review_enabled
        {
            return AcceptResult::WrongStage;
        }
        match (&stage, &result) {
            (Stage::Instant, StageResult::Instant(_)) if state.instant.is_some() => {
                return AcceptResult::DuplicateStage
            }
            (Stage::Deep, StageResult::Deep(_)) if state.deep.is_some() => {
                return AcceptResult::DuplicateStage
            }
            (Stage::Review, StageResult::Review(_)) if state.review.is_some() => {
                return AcceptResult::DuplicateStage
            }
            (Stage::Instant, StageResult::Instant(v)) => state.instant = Some(v.clone()),
            (Stage::Deep, StageResult::Deep(v)) => state.deep = Some(v.clone()),
            (Stage::Review, StageResult::Review(v)) => state.review = Some(v.clone()),
            _ => return AcceptResult::WrongStage,
        }
        let answer = resolve(state, &self.preferences);
        if let Some(answer) = answer {
            state.final_answer = Some(answer.clone());
            return AcceptResult::Resolved(answer);
        }
        match result {
            StageResult::Instant(InstantOutcome::Complete(answer))
            | StageResult::Instant(InstantOutcome::NeedsDeep(answer)) => {
                AcceptResult::Provisional(answer)
            }
            _ => AcceptResult::Pending,
        }
    }
    pub fn commit(&mut self, generation: Generation) -> Option<Content> {
        let state = self.active.as_ref()?;
        if state.generation != generation || generation.cancelled {
            return None;
        }
        let answer = state.final_answer.clone()?;
        let prompt = self
            .active_prompt
            .take()
            .expect("an active generation always has a prompt");
        self.history.push(ConversationTurn {
            role: Role::User,
            content: prompt,
        });
        self.history.push(ConversationTurn {
            role: Role::Assistant,
            content: answer.clone(),
        });
        self.active = None;
        Some(answer)
    }
    pub fn reset(&mut self) {
        self.history.clear();
        self.active = None;
        self.active_prompt = None;
    }
}

fn instant_answer(i: &InstantOutcome) -> Option<Content> {
    match i {
        InstantOutcome::Complete(a) | InstantOutcome::NeedsDeep(a) => Some(a.clone()),
        InstantOutcome::Failure(_) => None,
    }
}
fn resolve(s: &AssistantState, p: &AssistantPreferences) -> Option<Content> {
    let instant = s.instant.as_ref();
    if instant.is_none()
        || (p.deep_enabled && s.deep.is_none())
        || (p.review_enabled && s.review.is_none())
    {
        return None;
    }
    let instant_answer = instant.and_then(instant_answer);
    let deep_answer = s.deep.as_ref().and_then(|d| match d {
        DeepOutcome::Success(report) => Some(report.answer.clone()),
        DeepOutcome::Failure(_) => None,
    });
    if p.review_enabled {
        let review = s.review.as_ref()?;
        if p.deep_enabled && s.deep.is_none() {
            return None;
        }
        return match review {
            ReviewOutcome::Success(ReviewDecision::Rewrite(a)) => Some(a.clone()),
            ReviewOutcome::Success(ReviewDecision::Keep) => deep_answer.or(instant_answer),
            ReviewOutcome::Failure(_) => deep_answer.or(instant_answer),
        };
    }
    if p.deep_enabled {
        return deep_answer.or(instant_answer);
    }
    instant_answer
}

#[cfg(test)]
mod tests {
    use super::*;
    fn text(value: &str) -> Content {
        Content::Text(value.into())
    }
    fn instant(
        c: &mut AssistantCoordinator,
        g: Generation,
        outcome: InstantOutcome,
    ) -> AcceptResult {
        c.accept(g, Stage::Instant, StageResult::Instant(outcome))
    }
    #[test]
    fn defaults_and_model_classification() {
        let p = AssistantPreferences::default();
        assert!(!p.deep_enabled && !p.review_enabled);
        assert_eq!(p.review_model, AgentModel::OpenCodeSolHigh);
        assert_eq!(InstantModel::CustomLunaLow.provider(), Provider::Custom);
        assert_eq!(AgentModel::CustomLunaLow.provider(), Provider::Custom);
    }
    #[test]
    fn history_continues_and_reset_clears() {
        let mut c = AssistantCoordinator::new(Default::default());
        let g = c.start(" one ").unwrap();
        assert_eq!(
            instant(&mut c, g, InstantOutcome::Complete(text("a"))),
            AcceptResult::Resolved(text("a"))
        );
        assert_eq!(c.commit(g), Some(text("a")));
        assert_eq!(c.history().len(), 2);
        c.set_preferences(AssistantPreferences {
            instant_model: InstantModel::CustomLunaLow,
            ..Default::default()
        })
        .unwrap();
        let g = c.start("two").unwrap();
        assert_eq!(
            instant(&mut c, g, InstantOutcome::Complete(text("b"))),
            AcceptResult::Resolved(text("b"))
        );
        assert_eq!(c.commit(g), Some(text("b")));
        assert_eq!(c.history().len(), 4);
        c.reset();
        assert!(c.history().is_empty() && c.active().is_none());
    }

    #[test]
    fn resolved_answer_does_not_commit_until_explicitly_finalized() {
        let mut coordinator = AssistantCoordinator::new(Default::default());
        let generation = coordinator.start("question").unwrap();
        assert_eq!(
            instant(
                &mut coordinator,
                generation,
                InstantOutcome::Complete(text("answer"))
            ),
            AcceptResult::Resolved(text("answer"))
        );
        assert!(coordinator.history().is_empty());
        assert!(coordinator.active().is_some());
        assert!(coordinator.cancel(generation));
        assert!(coordinator.history().is_empty());
    }
    #[test]
    fn deep_and_review_matrix_is_deterministic() {
        for deep in [false, true] {
            for review in [false, true] {
                let p = AssistantPreferences {
                    deep_enabled: deep,
                    review_enabled: review,
                    ..Default::default()
                };
                let mut c = AssistantCoordinator::new(p);
                let g = c.start("x").unwrap();
                let instant_result = instant(&mut c, g, InstantOutcome::NeedsDeep(text("instant")));
                if !deep && !review {
                    assert_eq!(instant_result, AcceptResult::Resolved(text("instant")));
                } else {
                    assert_eq!(instant_result, AcceptResult::Provisional(text("instant")));
                }
                if deep {
                    assert!(c.active().is_some());
                    let deep_result = c.accept(
                        g,
                        Stage::Deep,
                        StageResult::Deep(DeepOutcome::Success(DeepReport {
                            answer: text("deep"),
                        })),
                    );
                    assert_eq!(
                        deep_result,
                        if review {
                            AcceptResult::Pending
                        } else {
                            AcceptResult::Resolved(text("deep"))
                        }
                    );
                }
                if review {
                    if deep {
                        assert!(c.active().is_some());
                    }
                    assert_eq!(
                        c.accept(
                            g,
                            Stage::Review,
                            StageResult::Review(ReviewOutcome::Success(ReviewDecision::Keep))
                        ),
                        AcceptResult::Resolved(if deep { text("deep") } else { text("instant") })
                    );
                }
                assert!(c.active().is_some());
                assert!(c.commit(g).is_some());
                assert!(c.active().is_none());
            }
        }
    }
    #[test]
    fn deep_only_waits_for_and_commits_the_deep_answer() {
        let mut c = AssistantCoordinator::new(AssistantPreferences {
            deep_enabled: true,
            ..Default::default()
        });
        let generation = c.start("x").unwrap();
        assert_eq!(
            instant(
                &mut c,
                generation,
                InstantOutcome::Complete(text("instant"))
            ),
            AcceptResult::Provisional(text("instant"))
        );
        assert_eq!(
            c.accept(
                generation,
                Stage::Deep,
                StageResult::Deep(DeepOutcome::Success(DeepReport {
                    answer: text("deep"),
                }))
            ),
            AcceptResult::Resolved(text("deep"))
        );
        assert_eq!(c.commit(generation), Some(text("deep")));
    }
    #[test]
    fn rewrite_out_of_order_and_stale_results_are_safe() {
        let p = AssistantPreferences {
            review_enabled: true,
            ..Default::default()
        };
        let mut c = AssistantCoordinator::new(p);
        let g = c.start("x").unwrap();
        assert_eq!(
            c.accept(
                g,
                Stage::Review,
                StageResult::Review(ReviewOutcome::Success(ReviewDecision::Rewrite(text("r"))))
            ),
            AcceptResult::Pending
        );
        assert_eq!(
            c.accept(
                g,
                Stage::Instant,
                StageResult::Instant(InstantOutcome::Complete(text("i")))
            ),
            AcceptResult::Resolved(text("r"))
        );
        assert_eq!(c.commit(g), Some(text("r")));
        let g = c.start("y").unwrap();
        assert!(c.cancel(g));
        assert_eq!(
            c.accept(
                g,
                Stage::Instant,
                StageResult::Instant(InstantOutcome::Complete(text("x")))
            ),
            AcceptResult::Stale
        );
    }
    #[test]
    fn disabled_stages_reject_results_without_mutating_state() {
        let mut coordinator = AssistantCoordinator::new(Default::default());
        let generation = coordinator.start("question").unwrap();

        assert_eq!(
            coordinator.accept(
                generation,
                Stage::Deep,
                StageResult::Deep(DeepOutcome::Success(DeepReport {
                    answer: text("deep"),
                })),
            ),
            AcceptResult::WrongStage
        );
        assert_eq!(
            coordinator.accept(
                generation,
                Stage::Review,
                StageResult::Review(ReviewOutcome::Success(ReviewDecision::Rewrite(text(
                    "review"
                )))),
            ),
            AcceptResult::WrongStage
        );
        let active = coordinator
            .active()
            .expect("generation should remain active");
        assert!(active.deep.is_none());
        assert!(active.review.is_none());
    }
    #[test]
    fn busy_and_invalid_prompts_rejected() {
        let mut c = AssistantCoordinator::new(Default::default());
        assert_eq!(c.start(" "), Err(StartError::EmptyPrompt));
        let g = c.start("x").unwrap();
        assert_eq!(c.start("y"), Err(StartError::Busy));
        c.cancel(g);
        assert!(c.history().is_empty());
    }

    #[test]
    fn refusal_is_typed_and_survives_commit() {
        let mut c = AssistantCoordinator::new(Default::default());
        let g = c.start("question").unwrap();
        let refusal = Content::Refusal("cannot help".into());
        assert_eq!(
            instant(&mut c, g, InstantOutcome::Complete(refusal.clone())),
            AcceptResult::Resolved(refusal.clone())
        );
        assert_eq!(c.commit(g), Some(refusal.clone()));
        assert_eq!(c.history()[1].content, refusal);
    }

    #[test]
    fn failures_follow_the_approved_matrix() {
        for (deep, review) in [(false, false), (true, false), (false, true), (true, true)] {
            let mut c = AssistantCoordinator::new(AssistantPreferences {
                deep_enabled: deep,
                review_enabled: review,
                ..Default::default()
            });
            let g = c.start("x").unwrap();
            assert_eq!(
                instant(&mut c, g, InstantOutcome::Failure("instant failed".into())),
                AcceptResult::Pending
            );
            if deep {
                assert_eq!(
                    c.accept(
                        g,
                        Stage::Deep,
                        StageResult::Deep(DeepOutcome::Failure("deep failed".into()))
                    ),
                    AcceptResult::Pending
                );
            }
            if review {
                assert_eq!(
                    c.accept(
                        g,
                        Stage::Review,
                        StageResult::Review(ReviewOutcome::Failure("review failed".into()))
                    ),
                    AcceptResult::Pending
                );
            }
            assert!(c.commit(g).is_none());
        }
    }

    #[test]
    fn review_failure_falls_back_to_deep_then_instant() {
        let mut c = AssistantCoordinator::new(AssistantPreferences {
            deep_enabled: true,
            review_enabled: true,
            ..Default::default()
        });
        let g = c.start("x").unwrap();
        assert_eq!(
            instant(&mut c, g, InstantOutcome::Complete(text("instant"))),
            AcceptResult::Provisional(text("instant"))
        );
        assert_eq!(
            c.accept(
                g,
                Stage::Deep,
                StageResult::Deep(DeepOutcome::Success(DeepReport {
                    answer: text("deep"),
                }))
            ),
            AcceptResult::Pending
        );
        assert_eq!(
            c.accept(
                g,
                Stage::Review,
                StageResult::Review(ReviewOutcome::Failure("failed".into()))
            ),
            AcceptResult::Resolved(text("deep"))
        );

        let mut c = AssistantCoordinator::new(AssistantPreferences {
            review_enabled: true,
            ..Default::default()
        });
        let g = c.start("x").unwrap();
        instant(&mut c, g, InstantOutcome::Complete(text("instant")));
        assert_eq!(
            c.accept(
                g,
                Stage::Review,
                StageResult::Review(ReviewOutcome::Failure("failed".into()))
            ),
            AcceptResult::Resolved(text("instant"))
        );
    }

    #[test]
    fn review_rewrite_preserves_refusal_content() {
        let mut c = AssistantCoordinator::new(AssistantPreferences {
            review_enabled: true,
            ..Default::default()
        });
        let g = c.start("x").unwrap();
        instant(&mut c, g, InstantOutcome::Complete(text("instant")));
        let refusal = Content::Refusal("cannot help".into());
        assert_eq!(
            c.accept(
                g,
                Stage::Review,
                StageResult::Review(ReviewOutcome::Success(ReviewDecision::Rewrite(
                    refusal.clone()
                )))
            ),
            AcceptResult::Resolved(refusal.clone())
        );
        assert_eq!(c.commit(g), Some(refusal));
    }
}
