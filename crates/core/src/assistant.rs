//! Provider-neutral assistant policy and stage outcome types.

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
pub struct AssistantPolicy {
    pub instant_model: InstantModel,
    pub deep_model: AgentModel,
    pub review_model: AgentModel,
    pub deep_enabled: bool,
    pub review_enabled: bool,
    pub prefetch_enabled: bool,
    pub completion_enabled: bool,
}
impl Default for AssistantPolicy {
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StageResult {
    Instant(InstantOutcome),
    Deep(DeepOutcome),
    Review(ReviewOutcome),
}

fn instant_answer(i: &InstantOutcome) -> Option<Content> {
    match i {
        InstantOutcome::Complete(a) | InstantOutcome::NeedsDeep(a) => Some(a.clone()),
        InstantOutcome::Failure(_) => None,
    }
}
pub(crate) fn resolve_with_stage(
    instant: Option<&InstantOutcome>,
    deep: Option<&DeepOutcome>,
    review: Option<&ReviewOutcome>,
    p: &AssistantPolicy,
) -> Option<(Content, Stage)> {
    if instant.is_none()
        || (p.deep_enabled && deep.is_none())
        || (p.review_enabled && review.is_none())
    {
        return None;
    }
    let instant_answer = instant.and_then(instant_answer);
    let deep_answer = deep.and_then(|d| match d {
        DeepOutcome::Success(report) => Some(report.answer.clone()),
        DeepOutcome::Failure(_) => None,
    });
    if p.review_enabled {
        return match review? {
            ReviewOutcome::Success(ReviewDecision::Rewrite(a)) => Some((a.clone(), Stage::Review)),
            ReviewOutcome::Success(ReviewDecision::Keep) => {
                deep_answer.or(instant_answer).map(|a| (a, Stage::Review))
            }
            ReviewOutcome::Failure(_) => deep_answer.or(instant_answer).map(|a| (a, Stage::Review)),
        };
    }
    if p.deep_enabled {
        deep_answer.or(instant_answer).map(|a| (a, Stage::Deep))
    } else {
        instant_answer.map(|a| (a, Stage::Instant))
    }
}

#[cfg(test)]
mod tests {
    use super::{AgentModel, AssistantPolicy, InstantModel, Provider};

    #[test]
    fn defaults_and_model_classification_are_stable() {
        let policy = AssistantPolicy::default();
        assert!(!policy.deep_enabled && !policy.review_enabled);
        assert_eq!(policy.review_model, AgentModel::OpenCodeSolHigh);
        assert_eq!(InstantModel::CustomLunaLow.provider(), Provider::Custom);
        assert_eq!(AgentModel::CustomLunaLow.provider(), Provider::Custom);
    }
}
