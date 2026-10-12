use crate::{assistant, conversation, runtime};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;

pub const CAPABILITY_ID_MAX_BYTES: usize = 64;
pub const MODEL_ID_MAX_BYTES: usize = 128;
pub const LABEL_MAX_BYTES: usize = 128;
pub const DISPLAY_TEXT_MAX_BYTES: usize = 512;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextValidationError {
    TooLong,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct BoundedText<const MAX_BYTES: usize>(String);

impl<const MAX_BYTES: usize> BoundedText<MAX_BYTES> {
    pub fn new(value: impl Into<String>) -> Result<Self, TextValidationError> {
        let value = value.into();
        if value.len() > MAX_BYTES {
            return Err(TextValidationError::TooLong);
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn into_inner(self) -> String {
        self.0
    }
}

impl<'de, const MAX_BYTES: usize> Deserialize<'de> for BoundedText<MAX_BYTES> {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(|_| serde::de::Error::custom("text exceeds byte limit"))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WireId(String);

impl From<u64> for WireId {
    fn from(value: u64) -> Self {
        Self(value.to_string())
    }
}

impl WireId {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Serialize for WireId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for WireId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        let parsed = value.parse::<u64>().map_err(|_| {
            serde::de::Error::custom("identifier must be an unsigned decimal string")
        })?;
        if parsed.to_string() != value {
            return Err(serde::de::Error::custom(
                "identifier must use canonical decimal encoding",
            ));
        }
        Ok(Self(value))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimePhase {
    Initializing,
    Sleeping,
    Listening,
    Processing,
    Executing,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionId {
    pub session_id: WireId,
    pub generation: WireId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageRole {
    User,
    Assistant,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "text")]
pub enum MessageContentKind {
    Text(String),
    Refusal(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Message {
    pub id: WireId,
    pub role: MessageRole,
    pub content: MessageContentKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Turn {
    pub request_id: String,
    pub execution_id: ExecutionId,
    pub user: Message,
    pub answer: Message,
    pub final_stage: Stage,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActiveRequestSummary {
    pub execution_id: ExecutionId,
    pub request_id: String,
    pub prompt: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityState {
    Available,
    Warming,
    NotConfigured,
    Unavailable,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capability {
    pub id: BoundedText<CAPABILITY_ID_MAX_BYTES>,
    pub state: CapabilityState,
    pub reason: BoundedText<DISPLAY_TEXT_MAX_BYTES>,
    pub actual_provider: Option<BoundedText<CAPABILITY_ID_MAX_BYTES>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelOption {
    pub id: BoundedText<MODEL_ID_MAX_BYTES>,
    pub label: BoundedText<LABEL_MAX_BYTES>,
    pub available: bool,
    pub unavailable_reason: Option<BoundedText<DISPLAY_TEXT_MAX_BYTES>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    Instant,
    Deep,
    Review,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StageStatus {
    Queued,
    Running,
    Completed,
    Kept,
    Corrected,
    Cancelled,
    Failed,
    Stale,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum InstantModelChoice {
    LocalFast,
    LocalQuality,
    CustomSolHigh,
    CustomSolNone,
    CustomLunaNone,
    CustomLunaLow,
    #[serde(rename = "opencode-sol-high")]
    OpenCodeSolHigh,
    #[serde(rename = "opencode-luna-low")]
    OpenCodeLunaLow,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AgentModelChoice {
    CustomSolHigh,
    CustomLunaLow,
    #[serde(rename = "opencode-sol-high")]
    OpenCodeSolHigh,
    #[serde(rename = "opencode-luna-low")]
    OpenCodeLunaLow,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResponseProfile {
    Fast,
    Quality,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UiTheme {
    Light,
    Dark,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UiTextSize {
    Small,
    Medium,
    Large,
    ExtraLarge,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValidatedPreferences {
    pub instant: InstantModelChoice,
    pub deep: AgentModelChoice,
    pub review: AgentModelChoice,
    pub deep_enabled: bool,
    pub review_enabled: bool,
    pub prefetch: bool,
    pub completion: bool,
    pub response_profile: ResponseProfile,
    pub tts_enabled: bool,
    #[serde(default = "default_enabled")]
    pub web_enabled: bool,
    #[serde(default = "default_enabled")]
    pub speech_network_fallback: bool,
    pub ui_theme: UiTheme,
    pub ui_text_size: UiTextSize,
    /// Empty selects the system/automatic language rather than a fixed locale.
    #[serde(default)]
    pub speech_language: BoundedText<DISPLAY_TEXT_MAX_BYTES>,
    /// Empty selects the system/automatic TTS provider.
    #[serde(default)]
    pub speech_tts_provider: BoundedText<DISPLAY_TEXT_MAX_BYTES>,
    /// Empty selects the system/automatic voice rather than a fixed voice ID.
    #[serde(default)]
    pub speech_tts_voice: BoundedText<DISPLAY_TEXT_MAX_BYTES>,
}

fn default_enabled() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppSnapshot {
    pub session_id: WireId,
    pub runtime_phase: RuntimePhase,
    pub transcript: Vec<Turn>,
    pub active_request: Option<ActiveRequestSummary>,
    pub capabilities: Vec<Capability>,
    pub model_options: Vec<ModelOption>,
    pub preferences: ValidatedPreferences,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PromptUpdate {
    Delta {
        text: BoundedText<{ conversation::MAX_UPDATE_BYTES }>,
    },
    Stage {
        stage: Stage,
        status: StageStatus,
        detail: Option<BoundedText<DISPLAY_TEXT_MAX_BYTES>>,
    },
    Correction {
        stage: Stage,
        text: BoundedText<{ conversation::MAX_UPDATE_BYTES }>,
        correction: BoundedText<DISPLAY_TEXT_MAX_BYTES>,
    },
    Status {
        message: BoundedText<DISPLAY_TEXT_MAX_BYTES>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PromptOutcome {
    Completed { turn: CompletedTurn },
    Refused { turn: RefusedTurn },
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct CompletedTurn(Turn);

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct RefusedTurn(Turn);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptOutcomeValidationError {
    ExpectedTextAnswer,
    ExpectedRefusalAnswer,
    FailedIsNotAnOutcome,
}

impl CompletedTurn {
    pub fn turn(&self) -> &Turn {
        &self.0
    }
}

impl RefusedTurn {
    pub fn turn(&self) -> &Turn {
        &self.0
    }
}

impl PromptOutcome {
    pub fn completed(turn: Turn) -> Result<Self, PromptOutcomeValidationError> {
        if !matches!(turn.answer.content, MessageContentKind::Text(_)) {
            return Err(PromptOutcomeValidationError::ExpectedTextAnswer);
        }
        Ok(Self::Completed {
            turn: CompletedTurn(turn),
        })
    }

    pub fn refused(turn: Turn) -> Result<Self, PromptOutcomeValidationError> {
        if !matches!(turn.answer.content, MessageContentKind::Refusal(_)) {
            return Err(PromptOutcomeValidationError::ExpectedRefusalAnswer);
        }
        Ok(Self::Refused {
            turn: RefusedTurn(turn),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandErrorCode {
    InvalidInput,
    NotFound,
    Busy,
    Unavailable,
    Internal,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CommandError {
    code: CommandErrorCode,
    message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandErrorValidationError {
    EmptyMessage,
    MessageTooLong,
}

impl CommandError {
    pub const MAX_MESSAGE_BYTES: usize = 512;
    pub fn new(
        code: CommandErrorCode,
        message: impl Into<String>,
    ) -> Result<Self, CommandErrorValidationError> {
        let message = message.into();
        if message.is_empty() {
            return Err(CommandErrorValidationError::EmptyMessage);
        }
        if message.len() > Self::MAX_MESSAGE_BYTES {
            return Err(CommandErrorValidationError::MessageTooLong);
        }
        Ok(Self { code, message })
    }

    pub fn code(&self) -> CommandErrorCode {
        self.code
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}
impl fmt::Display for CommandError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)
    }
}
impl std::error::Error for CommandError {}

impl From<runtime::RuntimePhase> for RuntimePhase {
    fn from(v: runtime::RuntimePhase) -> Self {
        match v {
            runtime::RuntimePhase::Initializing => Self::Initializing,
            runtime::RuntimePhase::Sleeping => Self::Sleeping,
            runtime::RuntimePhase::Listening => Self::Listening,
            runtime::RuntimePhase::Processing => Self::Processing,
            runtime::RuntimePhase::Executing => Self::Executing,
            runtime::RuntimePhase::Error => Self::Error,
        }
    }
}
impl From<assistant::Content> for MessageContentKind {
    fn from(v: assistant::Content) -> Self {
        match v {
            assistant::Content::Text(v) => Self::Text(v),
            assistant::Content::Refusal(v) => Self::Refusal(v),
        }
    }
}
impl From<&assistant::Content> for MessageContentKind {
    fn from(v: &assistant::Content) -> Self {
        v.clone().into()
    }
}
impl From<assistant::Role> for MessageRole {
    fn from(v: assistant::Role) -> Self {
        match v {
            assistant::Role::User => Self::User,
            assistant::Role::Assistant => Self::Assistant,
        }
    }
}
impl From<conversation::Message> for Message {
    fn from(v: conversation::Message) -> Self {
        Self {
            id: v.id.0.into(),
            role: v.role.into(),
            content: v.content.into(),
        }
    }
}
impl From<&conversation::Message> for Message {
    fn from(v: &conversation::Message) -> Self {
        Self {
            id: v.id.0.into(),
            role: v.role.into(),
            content: (&v.content).into(),
        }
    }
}
impl From<conversation::ExecutionId> for ExecutionId {
    fn from(v: conversation::ExecutionId) -> Self {
        Self {
            session_id: v.session_id.0.into(),
            generation: v.generation.0.into(),
        }
    }
}
impl From<assistant::Stage> for Stage {
    fn from(v: assistant::Stage) -> Self {
        match v {
            assistant::Stage::Instant => Self::Instant,
            assistant::Stage::Deep => Self::Deep,
            assistant::Stage::Review => Self::Review,
        }
    }
}
impl From<assistant::InstantModel> for InstantModelChoice {
    fn from(v: assistant::InstantModel) -> Self {
        match v {
            assistant::InstantModel::LocalFast => Self::LocalFast,
            assistant::InstantModel::LocalQuality => Self::LocalQuality,
            assistant::InstantModel::CustomSolHigh => Self::CustomSolHigh,
            assistant::InstantModel::CustomSolNone => Self::CustomSolNone,
            assistant::InstantModel::CustomLunaNone => Self::CustomLunaNone,
            assistant::InstantModel::CustomLunaLow => Self::CustomLunaLow,
            assistant::InstantModel::OpenCodeSolHigh => Self::OpenCodeSolHigh,
            assistant::InstantModel::OpenCodeLunaLow => Self::OpenCodeLunaLow,
        }
    }
}
impl From<assistant::AgentModel> for AgentModelChoice {
    fn from(v: assistant::AgentModel) -> Self {
        match v {
            assistant::AgentModel::CustomSolHigh => Self::CustomSolHigh,
            assistant::AgentModel::CustomLunaLow => Self::CustomLunaLow,
            assistant::AgentModel::OpenCodeSolHigh => Self::OpenCodeSolHigh,
            assistant::AgentModel::OpenCodeLunaLow => Self::OpenCodeLunaLow,
        }
    }
}
impl From<conversation::Turn> for Turn {
    fn from(v: conversation::Turn) -> Self {
        Self {
            request_id: v.request_id.0,
            execution_id: v.execution_id.into(),
            user: v.user.into(),
            answer: v.answer.into(),
            final_stage: v.final_stage.into(),
        }
    }
}
impl From<&conversation::Turn> for Turn {
    fn from(v: &conversation::Turn) -> Self {
        Self {
            request_id: v.request_id.0.clone(),
            execution_id: v.execution_id.into(),
            user: (&v.user).into(),
            answer: (&v.answer).into(),
            final_stage: v.final_stage.into(),
        }
    }
}
impl From<&conversation::ActiveExecutionSummary> for ActiveRequestSummary {
    fn from(v: &conversation::ActiveExecutionSummary) -> Self {
        Self {
            execution_id: v.execution_id.into(),
            request_id: v.request_id.0.clone(),
            prompt: v.prompt.clone(),
        }
    }
}

impl TryFrom<conversation::AppliedOutcome> for PromptOutcome {
    type Error = PromptOutcomeValidationError;

    fn try_from(value: conversation::AppliedOutcome) -> Result<Self, Self::Error> {
        match value {
            conversation::AppliedOutcome::Completed(turn) => Self::completed(turn.into()),
            conversation::AppliedOutcome::Refused(turn) => Self::refused(turn.into()),
            conversation::AppliedOutcome::Cancelled => Ok(Self::Cancelled),
            conversation::AppliedOutcome::Failed => {
                Err(PromptOutcomeValidationError::FailedIsNotAnOutcome)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assistant::{Content, Role, Stage as AssistantStage};
    use crate::conversation::{
        ExecutionId as ConversationExecutionId, GenerationId, MessageId, RequestId, SessionId,
    };

    fn refusal_turn() -> conversation::Turn {
        conversation::Turn {
            request_id: RequestId("request".into()),
            execution_id: ConversationExecutionId {
                session_id: SessionId(4),
                generation: GenerationId(9),
            },
            user: conversation::Message {
                id: MessageId(1),
                role: Role::User,
                content: Content::Text("ask".into()),
            },
            answer: conversation::Message {
                id: MessageId(2),
                role: Role::Assistant,
                content: Content::Refusal("no".into()),
            },
            final_stage: AssistantStage::Review,
        }
    }

    fn text_turn() -> conversation::Turn {
        let mut turn = refusal_turn();
        turn.answer.content = Content::Text("yes".into());
        turn
    }

    fn preferences() -> ValidatedPreferences {
        ValidatedPreferences {
            instant: InstantModelChoice::OpenCodeSolHigh,
            deep: AgentModelChoice::CustomSolHigh,
            review: AgentModelChoice::OpenCodeLunaLow,
            deep_enabled: true,
            review_enabled: false,
            prefetch: true,
            completion: false,
            response_profile: ResponseProfile::Fast,
            tts_enabled: false,
            web_enabled: true,
            speech_network_fallback: true,
            ui_theme: UiTheme::Dark,
            ui_text_size: UiTextSize::Large,
            speech_language: BoundedText::default(),
            speech_tts_provider: BoundedText::default(),
            speech_tts_voice: BoundedText::default(),
        }
    }

    #[test]
    fn legacy_preferences_default_web_and_speech_fallback_to_enabled() {
        let mut old = serde_json::to_value(preferences()).unwrap();
        old.as_object_mut().unwrap().remove("web_enabled");
        old.as_object_mut()
            .unwrap()
            .remove("speech_network_fallback");
        let preferences: ValidatedPreferences = serde_json::from_value(old).unwrap();
        let wire = serde_json::to_value(preferences).unwrap();
        assert_eq!(wire["web_enabled"], true);
        assert_eq!(wire["speech_network_fallback"], true);
    }

    #[test]
    fn preferences_preserve_explicit_web_and_speech_fallback_denial() {
        let mut wire = serde_json::to_value(preferences()).unwrap();
        wire["web_enabled"] = false.into();
        wire["speech_network_fallback"] = false.into();
        let preferences: ValidatedPreferences = serde_json::from_value(wire).unwrap();
        let wire = serde_json::to_value(preferences).unwrap();
        assert_eq!(wire["web_enabled"], false);
        assert_eq!(wire["speech_network_fallback"], false);
    }

    #[test]
    fn web_and_speech_fallback_preferences_are_independent_and_require_booleans() {
        for web_enabled in [true, false] {
            for fallback in [true, false] {
                let mut wire = serde_json::to_value(preferences()).unwrap();
                wire["web_enabled"] = web_enabled.into();
                wire["speech_network_fallback"] = fallback.into();
                let parsed: ValidatedPreferences = serde_json::from_value(wire.clone()).unwrap();
                assert_eq!(parsed.web_enabled, web_enabled);
                assert_eq!(parsed.speech_network_fallback, fallback);
                for field in ["web_enabled", "speech_network_fallback"] {
                    let mut old = wire.clone();
                    old.as_object_mut().unwrap().remove(field);
                    let parsed: ValidatedPreferences = serde_json::from_value(old).unwrap();
                    assert_eq!(parsed.web_enabled, field == "web_enabled" || web_enabled);
                    assert_eq!(
                        parsed.speech_network_fallback,
                        field == "speech_network_fallback" || fallback
                    );
                }
            }
        }
        for field in ["web_enabled", "speech_network_fallback"] {
            for invalid in [
                serde_json::Value::Null,
                serde_json::json!("false"),
                serde_json::json!(0),
            ] {
                let mut wire = serde_json::to_value(preferences()).unwrap();
                wire[field] = invalid;
                assert!(serde_json::from_value::<ValidatedPreferences>(wire).is_err());
            }
        }
    }

    const SPEECH_SELECTION_FIELDS: [&str; 3] =
        ["speech_language", "speech_tts_provider", "speech_tts_voice"];

    #[test]
    fn speech_selection_preferences_default_to_automatic_for_legacy_payloads() {
        let mut legacy = serde_json::to_value(preferences()).unwrap();
        for field in SPEECH_SELECTION_FIELDS {
            legacy.as_object_mut().unwrap().remove(field);
        }
        let parsed: ValidatedPreferences = serde_json::from_value(legacy.clone()).unwrap();
        let mut restored = serde_json::to_value(parsed).unwrap();
        for field in SPEECH_SELECTION_FIELDS {
            assert_eq!(
                restored.as_object_mut().unwrap().remove(field),
                Some(serde_json::json!(""))
            );
        }
        assert_eq!(restored, legacy);
    }

    #[test]
    fn speech_selection_preferences_roundtrip_verbatim_and_default_independently() {
        let mut wire = serde_json::to_value(preferences()).unwrap();
        for (field, choice) in SPEECH_SELECTION_FIELDS.into_iter().zip([
            "en-AU",
            "dev.synthetic.tts",
            "Synthetic 🧪 voice",
        ]) {
            wire[field] = choice.into();
        }
        let parsed: ValidatedPreferences = serde_json::from_value(wire.clone()).unwrap();
        assert_eq!(serde_json::to_value(parsed).unwrap(), wire);
        for field in SPEECH_SELECTION_FIELDS {
            let mut older = wire.clone();
            older.as_object_mut().unwrap().remove(field);
            let parsed: ValidatedPreferences = serde_json::from_value(older).unwrap();
            let mut expected = wire.clone();
            expected[field] = "".into();
            assert_eq!(serde_json::to_value(parsed).unwrap(), expected);
        }
    }

    #[test]
    fn speech_selection_preferences_enforce_utf8_byte_bounds_per_field() {
        for choice in ["".to_owned(), "x".repeat(512), "🧪".repeat(128)] {
            let mut wire = serde_json::to_value(preferences()).unwrap();
            for field in SPEECH_SELECTION_FIELDS {
                wire[field] = choice.clone().into();
            }
            let parsed: ValidatedPreferences = serde_json::from_value(wire.clone()).unwrap();
            assert_eq!(serde_json::to_value(parsed).unwrap(), wire);
        }
        for field in SPEECH_SELECTION_FIELDS {
            for invalid in [
                serde_json::Value::Null,
                serde_json::json!(false),
                serde_json::json!(42),
                serde_json::json!([]),
                serde_json::json!({}),
                serde_json::json!("x".repeat(513)),
                serde_json::json!(format!("{}x", "🧪".repeat(128))),
                serde_json::json!("🧪".repeat(129)),
            ] {
                let mut wire = serde_json::to_value(preferences()).unwrap();
                wire[field] = invalid;
                assert!(
                    serde_json::from_value::<ValidatedPreferences>(wire).is_err(),
                    "{field} must reject malformed or oversized choices"
                );
            }
        }
    }

    #[test]
    fn model_choices_use_exact_kebab_case_names() {
        for (model, name) in [
            (assistant::InstantModel::CustomSolNone, "custom-sol-none"),
            (assistant::InstantModel::CustomLunaNone, "custom-luna-none"),
        ] {
            let choice = InstantModelChoice::from(model);
            assert_eq!(serde_json::to_value(choice).unwrap(), name);
            assert_eq!(
                serde_json::from_value::<InstantModelChoice>(serde_json::json!(name)).unwrap(),
                choice
            );
        }
        assert_eq!(
            serde_json::to_value(InstantModelChoice::OpenCodeSolHigh).unwrap(),
            "opencode-sol-high"
        );
        assert_eq!(
            serde_json::from_str::<AgentModelChoice>("\"custom-luna-low\"").unwrap(),
            AgentModelChoice::CustomLunaLow
        );
    }

    #[test]
    fn canonical_turn_conversion_preserves_full_identity_and_refusal() {
        let ipc = Turn::from(refusal_turn());
        assert_eq!(
            ipc.execution_id,
            ExecutionId {
                session_id: 4.into(),
                generation: 9.into(),
            }
        );
        assert_eq!(ipc.answer.content, MessageContentKind::Refusal("no".into()));
        assert_eq!(ipc.final_stage, super::Stage::Review);
        assert_eq!(
            serde_json::to_value(&ipc).unwrap(),
            serde_json::json!({
                "request_id": "request",
                "execution_id": { "session_id": "4", "generation": "9" },
                "user": { "id": "1", "role": "user", "content": { "kind": "text", "text": "ask" } },
                "answer": { "id": "2", "role": "assistant", "content": { "kind": "refusal", "text": "no" } },
                "final_stage": "review"
            })
        );
    }

    #[test]
    fn prompt_updates_and_outcomes_have_exact_tagged_shapes() {
        assert_eq!(
            serde_json::to_value(PromptUpdate::Delta {
                text: BoundedText::new("hello").unwrap()
            })
            .unwrap(),
            serde_json::json!({ "kind": "delta", "text": "hello" })
        );
        assert_eq!(
            serde_json::to_value(PromptUpdate::Stage {
                stage: super::Stage::Deep,
                status: StageStatus::Running,
                detail: None,
            })
            .unwrap(),
            serde_json::json!({ "kind": "stage", "stage": "deep", "status": "running", "detail": null })
        );
        assert_eq!(
            serde_json::to_value(PromptUpdate::Correction {
                stage: super::Stage::Review,
                text: BoundedText::new("fixed").unwrap(),
                correction: BoundedText::new("Correction: fixed").unwrap(),
            })
            .unwrap(),
            serde_json::json!({ "kind": "correction", "stage": "review", "text": "fixed", "correction": "Correction: fixed" })
        );
        assert_eq!(
            serde_json::to_value(PromptUpdate::Status {
                message: BoundedText::new("working").unwrap()
            })
            .unwrap(),
            serde_json::json!({ "kind": "status", "message": "working" })
        );
        let refused_turn = Turn::from(refusal_turn());
        let completed_turn = Turn::from(text_turn());
        assert_eq!(
            serde_json::to_value(PromptOutcome::refused(refused_turn.clone()).unwrap()).unwrap(),
            serde_json::json!({ "kind": "refused", "turn": refused_turn })
        );
        assert_eq!(
            serde_json::to_value(PromptOutcome::completed(completed_turn.clone()).unwrap())
                .unwrap(),
            serde_json::json!({ "kind": "completed", "turn": completed_turn })
        );
        assert_eq!(
            serde_json::to_value(PromptOutcome::Cancelled).unwrap(),
            serde_json::json!({ "kind": "cancelled" })
        );
        assert_eq!(
            PromptOutcome::completed(refused_turn),
            Err(PromptOutcomeValidationError::ExpectedTextAnswer)
        );
        assert_eq!(
            PromptOutcome::refused(completed_turn),
            Err(PromptOutcomeValidationError::ExpectedRefusalAnswer)
        );
        assert!(matches!(
            PromptOutcome::try_from(conversation::AppliedOutcome::Refused(refusal_turn())),
            Ok(PromptOutcome::Refused { .. })
        ));
        assert_eq!(
            PromptOutcome::try_from(conversation::AppliedOutcome::Failed),
            Err(PromptOutcomeValidationError::FailedIsNotAnOutcome)
        );
    }

    #[test]
    fn app_snapshot_serializes_one_canonical_state() {
        let snapshot = AppSnapshot {
            session_id: 4.into(),
            runtime_phase: RuntimePhase::Executing,
            transcript: vec![Turn::from(refusal_turn())],
            active_request: Some(ActiveRequestSummary {
                execution_id: ExecutionId {
                    session_id: 4.into(),
                    generation: 10.into(),
                },
                request_id: "next".into(),
                prompt: "continue".into(),
            }),
            capabilities: vec![Capability {
                id: BoundedText::new("opencode").unwrap(),
                state: CapabilityState::Available,
                reason: BoundedText::new("ready").unwrap(),
                actual_provider: Some(BoundedText::new("cpu").unwrap()),
            }],
            model_options: vec![ModelOption {
                id: BoundedText::new("opencode-sol-high").unwrap(),
                label: BoundedText::new("Sol High").unwrap(),
                available: true,
                unavailable_reason: None,
            }],
            preferences: preferences(),
        };
        let value = serde_json::to_value(snapshot).unwrap();
        assert_eq!(value["session_id"], "4");
        assert_eq!(value["runtime_phase"], "executing");
        assert_eq!(
            value["transcript"][0]["answer"]["content"]["kind"],
            "refusal"
        );
        assert_eq!(value["active_request"]["execution_id"]["generation"], "10");
        assert_eq!(value["preferences"]["instant"], "opencode-sol-high");
        assert_eq!(value["model_options"][0]["available"], true);
    }

    #[test]
    fn command_error_enforces_display_message_bounds() {
        assert_eq!(
            CommandError::new(CommandErrorCode::InvalidInput, ""),
            Err(CommandErrorValidationError::EmptyMessage)
        );
        assert_eq!(
            CommandError::new(CommandErrorCode::InvalidInput, "x".repeat(513)),
            Err(CommandErrorValidationError::MessageTooLong)
        );
        assert!(CommandError::new(CommandErrorCode::InvalidInput, "x".repeat(512)).is_ok());
        assert_eq!(
            serde_json::to_value(CommandError::new(CommandErrorCode::Busy, "busy").unwrap())
                .unwrap(),
            serde_json::json!({ "code": "busy", "message": "busy" })
        );
        assert!(CommandError::new(CommandErrorCode::Internal, "界".repeat(170)).is_ok());
        assert_eq!(
            CommandError::new(CommandErrorCode::Internal, "界".repeat(171)),
            Err(CommandErrorValidationError::MessageTooLong)
        );
    }

    #[test]
    fn bounded_wire_text_and_identifiers_reject_invalid_values() {
        assert!(BoundedText::<DISPLAY_TEXT_MAX_BYTES>::new("界".repeat(170)).is_ok());
        assert_eq!(
            BoundedText::<DISPLAY_TEXT_MAX_BYTES>::new("界".repeat(171)),
            Err(TextValidationError::TooLong)
        );
        assert!(serde_json::from_str::<BoundedText<4>>("\"12345\"").is_err());

        let max = WireId::from(u64::MAX);
        assert_eq!(
            serde_json::to_string(&max).unwrap(),
            format!("\"{}\"", u64::MAX)
        );
        assert_eq!(
            serde_json::from_str::<WireId>(&format!("\"{}\"", u64::MAX)).unwrap(),
            max
        );
        assert!(serde_json::from_str::<WireId>("18446744073709551615").is_err());
        assert!(serde_json::from_str::<WireId>("\"01\"").is_err());

        let display_overflow = "x".repeat(DISPLAY_TEXT_MAX_BYTES + 1);
        let update_overflow = "x".repeat(conversation::MAX_UPDATE_BYTES + 1);
        assert!(serde_json::from_value::<Capability>(serde_json::json!({
            "id": "opencode",
            "state": "available",
            "reason": display_overflow,
            "actual_provider": null
        }))
        .is_err());
        assert!(serde_json::from_value::<ModelOption>(serde_json::json!({
            "id": "model",
            "label": "x".repeat(LABEL_MAX_BYTES + 1),
            "available": false,
            "unavailable_reason": null
        }))
        .is_err());
        assert!(serde_json::from_value::<PromptUpdate>(serde_json::json!({
            "kind": "delta",
            "text": update_overflow
        }))
        .is_err());
        assert!(serde_json::from_value::<PromptUpdate>(serde_json::json!({
            "kind": "stage",
            "stage": "instant",
            "status": "running",
            "detail": "x".repeat(DISPLAY_TEXT_MAX_BYTES + 1)
        }))
        .is_err());
        assert!(serde_json::from_value::<PromptUpdate>(serde_json::json!({
            "kind": "correction",
            "stage": "review",
            "text": "answer",
            "correction": "x".repeat(DISPLAY_TEXT_MAX_BYTES + 1)
        }))
        .is_err());
        assert!(serde_json::from_value::<PromptUpdate>(serde_json::json!({
            "kind": "status",
            "message": "x".repeat(DISPLAY_TEXT_MAX_BYTES + 1)
        }))
        .is_err());
    }
}
