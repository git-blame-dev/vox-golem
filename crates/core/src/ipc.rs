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

    pub fn from_operation_id(
        id: &crate::durable_conversation::OperationId,
    ) -> Result<Self, crate::durable_conversation::DomainError> {
        id.validate()?;
        Ok(Self(format!("{}:{}", id.replica, id.sequence)))
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
        if let Some((replica, sequence)) = value.split_once(':') {
            let id = crate::durable_conversation::OperationId {
                replica: replica.to_string(),
                sequence: sequence.to_string(),
            };
            id.validate().map_err(|_| {
                serde::de::Error::custom("identifier must use canonical graph operation encoding")
            })?;
            return Ok(Self(value));
        }
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
#[serde(try_from = "TurnWire")]
pub struct Turn {
    pub sources: Vec<crate::sources::SourceLink>,
    pub request_id: String,
    pub execution_id: ExecutionId,
    pub user: Message,
    pub answer: Message,
    pub final_stage: Stage,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TurnWire {
    sources: Vec<crate::sources::SourceLink>,
    request_id: String,
    execution_id: ExecutionId,
    user: Message,
    answer: Message,
    final_stage: Stage,
}
impl TryFrom<TurnWire> for Turn {
    type Error = &'static str;
    fn try_from(wire: TurnWire) -> Result<Self, Self::Error> {
        crate::sources::validate_sources(&wire.sources).map_err(|_| "invalid sources")?;
        if matches!(wire.answer.content, MessageContentKind::Refusal(_)) && !wire.sources.is_empty()
        {
            return Err("refusal cannot carry sources");
        }
        Ok(Self {
            sources: wire.sources,
            request_id: wire.request_id,
            execution_id: wire.execution_id,
            user: wire.user,
            answer: wire.answer,
            final_stage: wire.final_stage,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActiveRequestSummary {
    pub execution_id: ExecutionId,
    pub request_id: String,
    pub prompt: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnsavedReason {
    StorageError,
    ContextUnavailable,
}

/// A validated process-owned result, never a canonical transcript turn. Its identity
/// is the already prepared durable request; it has no invented operation/message IDs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "UnsavedResultWire")]
pub struct UnsavedResult {
    request_id: String,
    execution_id: ExecutionId,
    prompt: String,
    answer: MessageContentKind,
    sources: Vec<crate::sources::SourceLink>,
    final_stage: Stage,
    reason: UnsavedReason,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UnsavedResultWire {
    request_id: String,
    execution_id: ExecutionId,
    prompt: String,
    answer: MessageContentKind,
    sources: Vec<crate::sources::SourceLink>,
    final_stage: Stage,
    reason: UnsavedReason,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnsavedResultValidationError {
    InvalidRequestId,
    ExpectedDurableIdentity,
    InvalidContent,
}

impl fmt::Display for UnsavedResultValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidRequestId => "invalid unsaved request correlation",
            Self::ExpectedDurableIdentity => "unsaved result requires prepared durable identity",
            Self::InvalidContent => "invalid unsaved completed content",
        })
    }
}

impl UnsavedResult {
    pub fn new(
        request_id: String,
        execution_id: ExecutionId,
        prompt: String,
        answer: crate::durable_conversation::Answer,
        sources: Vec<crate::sources::SourceLink>,
        final_stage: Stage,
        reason: UnsavedReason,
    ) -> Result<Self, UnsavedResultValidationError> {
        conversation::validate_request_id(&request_id)
            .map_err(|_| UnsavedResultValidationError::InvalidRequestId)?;
        // WireId already guarantees canonical syntax; only graph IDs contain ':'.
        if !execution_id.session_id.as_str().contains(':')
            || !execution_id.generation.as_str().contains(':')
        {
            return Err(UnsavedResultValidationError::ExpectedDurableIdentity);
        }
        crate::durable_conversation::validate_completed_content(&prompt, &answer, &sources)
            .map_err(|_| UnsavedResultValidationError::InvalidContent)?;
        let answer = match answer {
            crate::durable_conversation::Answer::Text { text } => MessageContentKind::Text(text),
            crate::durable_conversation::Answer::Refusal { reason } => {
                MessageContentKind::Refusal(reason)
            }
        };
        Ok(Self {
            request_id,
            execution_id,
            prompt,
            answer,
            sources,
            final_stage,
            reason,
        })
    }

    pub fn request_id(&self) -> &str {
        &self.request_id
    }
    pub fn execution_id(&self) -> &ExecutionId {
        &self.execution_id
    }
    pub fn prompt(&self) -> &str {
        &self.prompt
    }
    pub fn answer(&self) -> &MessageContentKind {
        &self.answer
    }
    pub fn sources(&self) -> &[crate::sources::SourceLink] {
        &self.sources
    }
    pub fn final_stage(&self) -> Stage {
        self.final_stage
    }
    pub fn reason(&self) -> UnsavedReason {
        self.reason
    }
}

impl TryFrom<UnsavedResultWire> for UnsavedResult {
    type Error = UnsavedResultValidationError;
    fn try_from(wire: UnsavedResultWire) -> Result<Self, Self::Error> {
        let answer = match wire.answer {
            MessageContentKind::Text(text) => crate::durable_conversation::Answer::Text { text },
            MessageContentKind::Refusal(reason) => {
                crate::durable_conversation::Answer::Refusal { reason }
            }
        };
        Self::new(
            wire.request_id,
            wire.execution_id,
            wire.prompt,
            answer,
            wire.sources,
            wire.final_stage,
            wire.reason,
        )
    }
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
    CustomSolLow,
    CustomSolMedium,
    CustomSolHigh,
    CustomSolXhigh,
    CustomSolMax,
    CustomLunaNone,
    CustomLuna6Low,
    CustomLuna6Medium,
    CustomLuna6High,
    CustomLuna6Xhigh,
    CustomLuna6Max,
    CustomLuna56None,
    CustomLunaLow,
    CustomLuna56Medium,
    CustomLuna56High,
    CustomLuna56Xhigh,
    CustomLuna56Max,
    #[serde(rename = "opencode-sol-low")]
    OpenCodeSolLow,
    #[serde(rename = "opencode-sol-medium")]
    OpenCodeSolMedium,
    #[serde(rename = "opencode-sol-high")]
    OpenCodeSolHigh,
    #[serde(rename = "opencode-sol-xhigh")]
    OpenCodeSolXhigh,
    #[serde(rename = "opencode-sol-max")]
    OpenCodeSolMax,
    #[serde(rename = "opencode-luna-none")]
    OpenCodeLunaNone,
    #[serde(rename = "opencode-luna-low")]
    OpenCodeLunaLow,
    #[serde(rename = "opencode-luna-medium")]
    OpenCodeLunaMedium,
    #[serde(rename = "opencode-luna-high")]
    OpenCodeLunaHigh,
    #[serde(rename = "opencode-luna-xhigh")]
    OpenCodeLunaXhigh,
    #[serde(rename = "opencode-luna-max")]
    OpenCodeLunaMax,
}

impl InstantModelChoice {
    pub const ALL: [Self; 30] = [
        Self::LocalFast,
        Self::LocalQuality,
        Self::CustomSolLow,
        Self::CustomSolMedium,
        Self::CustomSolHigh,
        Self::CustomSolXhigh,
        Self::CustomSolMax,
        Self::CustomLunaNone,
        Self::CustomLuna6Low,
        Self::CustomLuna6Medium,
        Self::CustomLuna6High,
        Self::CustomLuna6Xhigh,
        Self::CustomLuna6Max,
        Self::CustomLuna56None,
        Self::CustomLunaLow,
        Self::CustomLuna56Medium,
        Self::CustomLuna56High,
        Self::CustomLuna56Xhigh,
        Self::CustomLuna56Max,
        Self::OpenCodeSolLow,
        Self::OpenCodeSolMedium,
        Self::OpenCodeSolHigh,
        Self::OpenCodeSolXhigh,
        Self::OpenCodeSolMax,
        Self::OpenCodeLunaNone,
        Self::OpenCodeLunaLow,
        Self::OpenCodeLunaMedium,
        Self::OpenCodeLunaHigh,
        Self::OpenCodeLunaXhigh,
        Self::OpenCodeLunaMax,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::LocalFast => "local-fast",
            Self::LocalQuality => "local-quality",
            Self::CustomSolLow => "custom-sol-low",
            Self::CustomSolMedium => "custom-sol-medium",
            Self::CustomSolHigh => "custom-sol-high",
            Self::CustomSolXhigh => "custom-sol-xhigh",
            Self::CustomSolMax => "custom-sol-max",
            Self::CustomLunaNone => "custom-luna-none",
            Self::CustomLuna6Low => "custom-luna6-low",
            Self::CustomLuna6Medium => "custom-luna6-medium",
            Self::CustomLuna6High => "custom-luna6-high",
            Self::CustomLuna6Xhigh => "custom-luna6-xhigh",
            Self::CustomLuna6Max => "custom-luna6-max",
            Self::CustomLuna56None => "custom-luna56-none",
            Self::CustomLunaLow => "custom-luna-low",
            Self::CustomLuna56Medium => "custom-luna56-medium",
            Self::CustomLuna56High => "custom-luna56-high",
            Self::CustomLuna56Xhigh => "custom-luna56-xhigh",
            Self::CustomLuna56Max => "custom-luna56-max",
            Self::OpenCodeSolLow => "opencode-sol-low",
            Self::OpenCodeSolMedium => "opencode-sol-medium",
            Self::OpenCodeSolHigh => "opencode-sol-high",
            Self::OpenCodeSolXhigh => "opencode-sol-xhigh",
            Self::OpenCodeSolMax => "opencode-sol-max",
            Self::OpenCodeLunaNone => "opencode-luna-none",
            Self::OpenCodeLunaLow => "opencode-luna-low",
            Self::OpenCodeLunaMedium => "opencode-luna-medium",
            Self::OpenCodeLunaHigh => "opencode-luna-high",
            Self::OpenCodeLunaXhigh => "opencode-luna-xhigh",
            Self::OpenCodeLunaMax => "opencode-luna-max",
        }
    }

    pub fn capability_id(self) -> &'static str {
        match self {
            Self::LocalFast => "local_fast",
            Self::LocalQuality => "local_quality",
            Self::CustomSolLow
            | Self::CustomSolMedium
            | Self::CustomSolHigh
            | Self::CustomSolXhigh
            | Self::CustomSolMax
            | Self::CustomLunaNone
            | Self::CustomLuna6Low
            | Self::CustomLuna6Medium
            | Self::CustomLuna6High
            | Self::CustomLuna6Xhigh
            | Self::CustomLuna6Max
            | Self::CustomLuna56None
            | Self::CustomLunaLow
            | Self::CustomLuna56Medium
            | Self::CustomLuna56High
            | Self::CustomLuna56Xhigh
            | Self::CustomLuna56Max => "custom_provider",
            Self::OpenCodeSolLow
            | Self::OpenCodeSolMedium
            | Self::OpenCodeSolHigh
            | Self::OpenCodeSolXhigh
            | Self::OpenCodeSolMax
            | Self::OpenCodeLunaNone
            | Self::OpenCodeLunaLow
            | Self::OpenCodeLunaMedium
            | Self::OpenCodeLunaHigh
            | Self::OpenCodeLunaXhigh
            | Self::OpenCodeLunaMax => "opencode",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::LocalFast => "Local: Fast",
            Self::LocalQuality => "Local: Quality",
            Self::CustomSolLow => "Custom: GPT-6.1 Sol Low",
            Self::CustomSolMedium => "Custom: GPT-6.1 Sol Medium",
            Self::CustomSolHigh => "Custom: GPT-6.1 Sol High",
            Self::CustomSolXhigh => "Custom: GPT-6.1 Sol X-High",
            Self::CustomSolMax => "Custom: GPT-6.1 Sol Max",
            Self::CustomLunaNone => "Custom: GPT-6 Luna None",
            Self::CustomLuna6Low => "Custom: GPT-6 Luna Low",
            Self::CustomLuna6Medium => "Custom: GPT-6 Luna Medium",
            Self::CustomLuna6High => "Custom: GPT-6 Luna High",
            Self::CustomLuna6Xhigh => "Custom: GPT-6 Luna X-High",
            Self::CustomLuna6Max => "Custom: GPT-6 Luna Max",
            Self::CustomLuna56None => "Custom: GPT-5.6 Luna None",
            Self::CustomLunaLow => "Custom: GPT-5.6 Luna Low",
            Self::CustomLuna56Medium => "Custom: GPT-5.6 Luna Medium",
            Self::CustomLuna56High => "Custom: GPT-5.6 Luna High",
            Self::CustomLuna56Xhigh => "Custom: GPT-5.6 Luna X-High",
            Self::CustomLuna56Max => "Custom: GPT-5.6 Luna Max",
            Self::OpenCodeSolLow => "OpenCode: GPT-6.1 Sol Low",
            Self::OpenCodeSolMedium => "OpenCode: GPT-6.1 Sol Medium",
            Self::OpenCodeSolHigh => "OpenCode: GPT-6.1 Sol High",
            Self::OpenCodeSolXhigh => "OpenCode: GPT-6.1 Sol X-High",
            Self::OpenCodeSolMax => "OpenCode: GPT-6.1 Sol Max",
            Self::OpenCodeLunaNone => "OpenCode: GPT-5.6 Luna None",
            Self::OpenCodeLunaLow => "OpenCode: GPT-5.6 Luna Low",
            Self::OpenCodeLunaMedium => "OpenCode: GPT-5.6 Luna Medium",
            Self::OpenCodeLunaHigh => "OpenCode: GPT-5.6 Luna High",
            Self::OpenCodeLunaXhigh => "OpenCode: GPT-5.6 Luna X-High",
            Self::OpenCodeLunaMax => "OpenCode: GPT-5.6 Luna Max",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AgentModelChoice {
    CustomSolLow,
    CustomSolMedium,
    CustomSolHigh,
    CustomSolXhigh,
    CustomSolMax,
    CustomLuna6Low,
    CustomLuna6Medium,
    CustomLuna6High,
    CustomLuna6Xhigh,
    CustomLuna6Max,
    CustomLunaLow,
    CustomLuna56Medium,
    CustomLuna56High,
    CustomLuna56Xhigh,
    CustomLuna56Max,
    #[serde(rename = "opencode-sol-low")]
    OpenCodeSolLow,
    #[serde(rename = "opencode-sol-medium")]
    OpenCodeSolMedium,
    #[serde(rename = "opencode-sol-high")]
    OpenCodeSolHigh,
    #[serde(rename = "opencode-sol-xhigh")]
    OpenCodeSolXhigh,
    #[serde(rename = "opencode-sol-max")]
    OpenCodeSolMax,
    #[serde(rename = "opencode-luna-low")]
    OpenCodeLunaLow,
    #[serde(rename = "opencode-luna-medium")]
    OpenCodeLunaMedium,
    #[serde(rename = "opencode-luna-high")]
    OpenCodeLunaHigh,
    #[serde(rename = "opencode-luna-xhigh")]
    OpenCodeLunaXhigh,
    #[serde(rename = "opencode-luna-max")]
    OpenCodeLunaMax,
}

impl AgentModelChoice {
    pub fn capability_id(self) -> &'static str {
        match self {
            Self::CustomSolLow
            | Self::CustomSolMedium
            | Self::CustomSolHigh
            | Self::CustomSolXhigh
            | Self::CustomSolMax
            | Self::CustomLuna6Low
            | Self::CustomLuna6Medium
            | Self::CustomLuna6High
            | Self::CustomLuna6Xhigh
            | Self::CustomLuna6Max
            | Self::CustomLunaLow
            | Self::CustomLuna56Medium
            | Self::CustomLuna56High
            | Self::CustomLuna56Xhigh
            | Self::CustomLuna56Max => "custom_provider",
            Self::OpenCodeSolLow
            | Self::OpenCodeSolMedium
            | Self::OpenCodeSolHigh
            | Self::OpenCodeSolXhigh
            | Self::OpenCodeSolMax
            | Self::OpenCodeLunaLow
            | Self::OpenCodeLunaMedium
            | Self::OpenCodeLunaHigh
            | Self::OpenCodeLunaXhigh
            | Self::OpenCodeLunaMax => "opencode",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResponseProfile {
    Fast,
    Quality,
}

impl ResponseProfile {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Fast => "fast",
            Self::Quality => "quality",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UiTheme {
    Light,
    Dark,
    Slate,
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
    #[serde(default)]
    pub fast_mode: bool,
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
    #[serde(
        deserialize_with = "required_unsaved_option",
        serialize_with = "serialize_unsaved_option"
    )]
    pub unsaved_result: Option<std::sync::Arc<UnsavedResult>>,
    pub capabilities: Vec<Capability>,
    pub model_options: Vec<ModelOption>,
    pub preferences: ValidatedPreferences,
}

fn required_unsaved_option<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<std::sync::Arc<UnsavedResult>>, D::Error> {
    Option::<UnsavedResult>::deserialize(deserializer).map(|value| value.map(std::sync::Arc::new))
}

fn serialize_unsaved_option<S: Serializer>(
    value: &Option<std::sync::Arc<UnsavedResult>>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    value.as_deref().serialize(serializer)
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
    Completed {
        turn: CompletedTurn,
        save_disposition: SavedDisposition,
    },
    Refused {
        turn: RefusedTurn,
        save_disposition: SavedDisposition,
    },
    Unsaved {
        #[serde(serialize_with = "serialize_unsaved_arc")]
        unsaved_result: std::sync::Arc<UnsavedResult>,
        save_disposition: UnsavedDisposition,
    },
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SavedDisposition {
    Saved,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UnsavedDisposition {
    Unsaved,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct CompletedTurn(Turn);

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct RefusedTurn(Turn);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptOutcomeValidationError {
    InvalidSources,
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
        crate::sources::validate_sources(&turn.sources)
            .map_err(|_| PromptOutcomeValidationError::InvalidSources)?;
        if !matches!(turn.answer.content, MessageContentKind::Text(_)) {
            return Err(PromptOutcomeValidationError::ExpectedTextAnswer);
        }
        Ok(Self::Completed {
            turn: CompletedTurn(turn),
            save_disposition: SavedDisposition::Saved,
        })
    }

    pub fn refused(turn: Turn) -> Result<Self, PromptOutcomeValidationError> {
        if !turn.sources.is_empty() {
            return Err(PromptOutcomeValidationError::InvalidSources);
        }
        if !matches!(turn.answer.content, MessageContentKind::Refusal(_)) {
            return Err(PromptOutcomeValidationError::ExpectedRefusalAnswer);
        }
        Ok(Self::Refused {
            turn: RefusedTurn(turn),
            save_disposition: SavedDisposition::Saved,
        })
    }

    pub fn unsaved(unsaved_result: std::sync::Arc<UnsavedResult>) -> Self {
        Self::Unsaved {
            unsaved_result,
            save_disposition: UnsavedDisposition::Unsaved,
        }
    }
}

fn serialize_unsaved_arc<S: Serializer>(
    value: &std::sync::Arc<UnsavedResult>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    value.as_ref().serialize(serializer)
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
            assistant::InstantModel::CustomSolLow => Self::CustomSolLow,
            assistant::InstantModel::CustomSolMedium => Self::CustomSolMedium,
            assistant::InstantModel::CustomSolHigh => Self::CustomSolHigh,
            assistant::InstantModel::CustomSolXhigh => Self::CustomSolXhigh,
            assistant::InstantModel::CustomSolMax => Self::CustomSolMax,
            assistant::InstantModel::CustomLunaNone => Self::CustomLunaNone,
            assistant::InstantModel::CustomLuna6Low => Self::CustomLuna6Low,
            assistant::InstantModel::CustomLuna6Medium => Self::CustomLuna6Medium,
            assistant::InstantModel::CustomLuna6High => Self::CustomLuna6High,
            assistant::InstantModel::CustomLuna6Xhigh => Self::CustomLuna6Xhigh,
            assistant::InstantModel::CustomLuna6Max => Self::CustomLuna6Max,
            assistant::InstantModel::CustomLuna56None => Self::CustomLuna56None,
            assistant::InstantModel::CustomLunaLow => Self::CustomLunaLow,
            assistant::InstantModel::CustomLuna56Medium => Self::CustomLuna56Medium,
            assistant::InstantModel::CustomLuna56High => Self::CustomLuna56High,
            assistant::InstantModel::CustomLuna56Xhigh => Self::CustomLuna56Xhigh,
            assistant::InstantModel::CustomLuna56Max => Self::CustomLuna56Max,
            assistant::InstantModel::OpenCodeSolLow => Self::OpenCodeSolLow,
            assistant::InstantModel::OpenCodeSolMedium => Self::OpenCodeSolMedium,
            assistant::InstantModel::OpenCodeSolHigh => Self::OpenCodeSolHigh,
            assistant::InstantModel::OpenCodeSolXhigh => Self::OpenCodeSolXhigh,
            assistant::InstantModel::OpenCodeSolMax => Self::OpenCodeSolMax,
            assistant::InstantModel::OpenCodeLunaNone => Self::OpenCodeLunaNone,
            assistant::InstantModel::OpenCodeLunaLow => Self::OpenCodeLunaLow,
            assistant::InstantModel::OpenCodeLunaMedium => Self::OpenCodeLunaMedium,
            assistant::InstantModel::OpenCodeLunaHigh => Self::OpenCodeLunaHigh,
            assistant::InstantModel::OpenCodeLunaXhigh => Self::OpenCodeLunaXhigh,
            assistant::InstantModel::OpenCodeLunaMax => Self::OpenCodeLunaMax,
        }
    }
}

impl From<InstantModelChoice> for assistant::InstantModel {
    fn from(choice: InstantModelChoice) -> Self {
        match choice {
            InstantModelChoice::LocalFast => Self::LocalFast,
            InstantModelChoice::LocalQuality => Self::LocalQuality,
            InstantModelChoice::CustomSolLow => Self::CustomSolLow,
            InstantModelChoice::CustomSolMedium => Self::CustomSolMedium,
            InstantModelChoice::CustomSolHigh => Self::CustomSolHigh,
            InstantModelChoice::CustomSolXhigh => Self::CustomSolXhigh,
            InstantModelChoice::CustomSolMax => Self::CustomSolMax,
            InstantModelChoice::CustomLunaNone => Self::CustomLunaNone,
            InstantModelChoice::CustomLuna6Low => Self::CustomLuna6Low,
            InstantModelChoice::CustomLuna6Medium => Self::CustomLuna6Medium,
            InstantModelChoice::CustomLuna6High => Self::CustomLuna6High,
            InstantModelChoice::CustomLuna6Xhigh => Self::CustomLuna6Xhigh,
            InstantModelChoice::CustomLuna6Max => Self::CustomLuna6Max,
            InstantModelChoice::CustomLuna56None => Self::CustomLuna56None,
            InstantModelChoice::CustomLunaLow => Self::CustomLunaLow,
            InstantModelChoice::CustomLuna56Medium => Self::CustomLuna56Medium,
            InstantModelChoice::CustomLuna56High => Self::CustomLuna56High,
            InstantModelChoice::CustomLuna56Xhigh => Self::CustomLuna56Xhigh,
            InstantModelChoice::CustomLuna56Max => Self::CustomLuna56Max,
            InstantModelChoice::OpenCodeSolLow => Self::OpenCodeSolLow,
            InstantModelChoice::OpenCodeSolMedium => Self::OpenCodeSolMedium,
            InstantModelChoice::OpenCodeSolHigh => Self::OpenCodeSolHigh,
            InstantModelChoice::OpenCodeSolXhigh => Self::OpenCodeSolXhigh,
            InstantModelChoice::OpenCodeSolMax => Self::OpenCodeSolMax,
            InstantModelChoice::OpenCodeLunaNone => Self::OpenCodeLunaNone,
            InstantModelChoice::OpenCodeLunaLow => Self::OpenCodeLunaLow,
            InstantModelChoice::OpenCodeLunaMedium => Self::OpenCodeLunaMedium,
            InstantModelChoice::OpenCodeLunaHigh => Self::OpenCodeLunaHigh,
            InstantModelChoice::OpenCodeLunaXhigh => Self::OpenCodeLunaXhigh,
            InstantModelChoice::OpenCodeLunaMax => Self::OpenCodeLunaMax,
        }
    }
}
impl From<assistant::AgentModel> for AgentModelChoice {
    fn from(v: assistant::AgentModel) -> Self {
        match v {
            assistant::AgentModel::CustomSolLow => Self::CustomSolLow,
            assistant::AgentModel::CustomSolMedium => Self::CustomSolMedium,
            assistant::AgentModel::CustomSolHigh => Self::CustomSolHigh,
            assistant::AgentModel::CustomSolXhigh => Self::CustomSolXhigh,
            assistant::AgentModel::CustomSolMax => Self::CustomSolMax,
            assistant::AgentModel::CustomLuna6Low => Self::CustomLuna6Low,
            assistant::AgentModel::CustomLuna6Medium => Self::CustomLuna6Medium,
            assistant::AgentModel::CustomLuna6High => Self::CustomLuna6High,
            assistant::AgentModel::CustomLuna6Xhigh => Self::CustomLuna6Xhigh,
            assistant::AgentModel::CustomLuna6Max => Self::CustomLuna6Max,
            assistant::AgentModel::CustomLunaLow => Self::CustomLunaLow,
            assistant::AgentModel::CustomLuna56Medium => Self::CustomLuna56Medium,
            assistant::AgentModel::CustomLuna56High => Self::CustomLuna56High,
            assistant::AgentModel::CustomLuna56Xhigh => Self::CustomLuna56Xhigh,
            assistant::AgentModel::CustomLuna56Max => Self::CustomLuna56Max,
            assistant::AgentModel::OpenCodeSolLow => Self::OpenCodeSolLow,
            assistant::AgentModel::OpenCodeSolMedium => Self::OpenCodeSolMedium,
            assistant::AgentModel::OpenCodeSolHigh => Self::OpenCodeSolHigh,
            assistant::AgentModel::OpenCodeSolXhigh => Self::OpenCodeSolXhigh,
            assistant::AgentModel::OpenCodeSolMax => Self::OpenCodeSolMax,
            assistant::AgentModel::OpenCodeLunaLow => Self::OpenCodeLunaLow,
            assistant::AgentModel::OpenCodeLunaMedium => Self::OpenCodeLunaMedium,
            assistant::AgentModel::OpenCodeLunaHigh => Self::OpenCodeLunaHigh,
            assistant::AgentModel::OpenCodeLunaXhigh => Self::OpenCodeLunaXhigh,
            assistant::AgentModel::OpenCodeLunaMax => Self::OpenCodeLunaMax,
        }
    }
}

impl From<AgentModelChoice> for assistant::AgentModel {
    fn from(choice: AgentModelChoice) -> Self {
        match choice {
            AgentModelChoice::CustomSolLow => Self::CustomSolLow,
            AgentModelChoice::CustomSolMedium => Self::CustomSolMedium,
            AgentModelChoice::CustomSolHigh => Self::CustomSolHigh,
            AgentModelChoice::CustomSolXhigh => Self::CustomSolXhigh,
            AgentModelChoice::CustomSolMax => Self::CustomSolMax,
            AgentModelChoice::CustomLuna6Low => Self::CustomLuna6Low,
            AgentModelChoice::CustomLuna6Medium => Self::CustomLuna6Medium,
            AgentModelChoice::CustomLuna6High => Self::CustomLuna6High,
            AgentModelChoice::CustomLuna6Xhigh => Self::CustomLuna6Xhigh,
            AgentModelChoice::CustomLuna6Max => Self::CustomLuna6Max,
            AgentModelChoice::CustomLunaLow => Self::CustomLunaLow,
            AgentModelChoice::CustomLuna56Medium => Self::CustomLuna56Medium,
            AgentModelChoice::CustomLuna56High => Self::CustomLuna56High,
            AgentModelChoice::CustomLuna56Xhigh => Self::CustomLuna56Xhigh,
            AgentModelChoice::CustomLuna56Max => Self::CustomLuna56Max,
            AgentModelChoice::OpenCodeSolLow => Self::OpenCodeSolLow,
            AgentModelChoice::OpenCodeSolMedium => Self::OpenCodeSolMedium,
            AgentModelChoice::OpenCodeSolHigh => Self::OpenCodeSolHigh,
            AgentModelChoice::OpenCodeSolXhigh => Self::OpenCodeSolXhigh,
            AgentModelChoice::OpenCodeSolMax => Self::OpenCodeSolMax,
            AgentModelChoice::OpenCodeLunaLow => Self::OpenCodeLunaLow,
            AgentModelChoice::OpenCodeLunaMedium => Self::OpenCodeLunaMedium,
            AgentModelChoice::OpenCodeLunaHigh => Self::OpenCodeLunaHigh,
            AgentModelChoice::OpenCodeLunaXhigh => Self::OpenCodeLunaXhigh,
            AgentModelChoice::OpenCodeLunaMax => Self::OpenCodeLunaMax,
        }
    }
}
impl From<conversation::Turn> for Turn {
    fn from(v: conversation::Turn) -> Self {
        Self {
            sources: v.sources,
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
            sources: v.sources.clone(),
            request_id: v.request_id.0.clone(),
            execution_id: v.execution_id.into(),
            user: (&v.user).into(),
            answer: (&v.answer).into(),
            final_stage: v.final_stage.into(),
        }
    }
}

/// Project a trusted, native hash-verified durable record without deriving
/// identity or source provenance from answer text. The alias is request-local
/// presentation correlation, never part of the immutable stored operation.
pub fn turn_from_record(
    record: &crate::durable_conversation::Record,
    client_request_alias: Option<&str>,
) -> Result<Turn, crate::durable_conversation::DomainError> {
    use crate::durable_conversation::{Answer, DomainError, OperationBody, TerminalStage};
    record.validate()?;
    let OperationBody::TurnCompleted {
        conversation,
        request,
        prompt,
        answer,
        sources,
        terminal_stage,
        ..
    } = &record.operation.body
    else {
        return Err(DomainError::Invalid {
            field: "canonical_turn".into(),
        });
    };
    let turn_id = WireId::from_operation_id(&record.operation.id)?;
    let request_id = WireId::from_operation_id(request)?;
    let alias = client_request_alias.unwrap_or(request_id.as_str());
    if conversation::validate_request_id(alias).is_err() {
        return Err(DomainError::Invalid {
            field: "client_request_id".into(),
        });
    }
    Ok(Turn {
        request_id: alias.into(),
        execution_id: ExecutionId {
            session_id: WireId::from_operation_id(conversation)?,
            generation: request_id,
        },
        user: Message {
            id: turn_id.clone(),
            role: MessageRole::User,
            content: MessageContentKind::Text(prompt.clone()),
        },
        answer: Message {
            id: turn_id,
            role: MessageRole::Assistant,
            content: match answer {
                Answer::Text { text } => MessageContentKind::Text(text.clone()),
                Answer::Refusal { reason } => MessageContentKind::Refusal(reason.clone()),
            },
        },
        final_stage: match terminal_stage {
            TerminalStage::Instant => Stage::Instant,
            TerminalStage::Deep => Stage::Deep,
            TerminalStage::Review => Stage::Review,
        },
        sources: sources.clone(),
    })
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
    #[test]
    fn unsaved_projection_is_bounded_strict_and_has_no_fabricated_turn_ids() {
        use super::*;
        use crate::durable_conversation::{Answer, OperationId};
        let replica = "00000000-0000-4000-8000-000000000001";
        let execution_id = ExecutionId {
            session_id: WireId::from_operation_id(&OperationId {
                replica: replica.into(),
                sequence: "1".into(),
            })
            .unwrap(),
            generation: WireId::from_operation_id(&OperationId {
                replica: replica.into(),
                sequence: "2".into(),
            })
            .unwrap(),
        };
        let result = UnsavedResult::new(
            "synthetic-renderer-request".into(),
            execution_id,
            "Synthetic prompt".into(),
            Answer::Text {
                text: "Synthetic unsaved answer".into(),
            },
            vec![],
            Stage::Review,
            UnsavedReason::StorageError,
        )
        .unwrap();
        let value = serde_json::to_value(&result).unwrap();
        assert_eq!(value.as_object().unwrap().len(), 7);
        for (text, accepted) in [("\u{feff}", true), (" \u{feff}\t", true), ("\u{85}", false)] {
            for field in ["prompt", "text", "refusal", "source_title"] {
                let mut candidate = value.clone();
                match field {
                    "prompt" => candidate["prompt"] = serde_json::json!(text),
                    "text" | "refusal" => {
                        candidate["answer"] = serde_json::json!({"kind": field, "text": text});
                    }
                    "source_title" => {
                        candidate["sources"] = serde_json::json!([{"url": "https://example.test/synthetic", "title": text}]);
                    }
                    _ => unreachable!(),
                }
                let parsed = serde_json::from_value::<UnsavedResult>(candidate.clone());
                assert_eq!(parsed.is_ok(), accepted, "{field}: {text:?}");
                if let Ok(parsed) = parsed {
                    assert_eq!(serde_json::to_value(parsed).unwrap(), candidate);
                }
            }
        }
        for (suffix, accepted) in [("\u{feff}", true), ("\u{85}", false), ("\0", false)] {
            let mut candidate = value.clone();
            candidate["sources"] = serde_json::json!([{"url": format!("https://example.test/{suffix}"), "title": "Synthetic source"}]);
            let parsed = serde_json::from_value::<UnsavedResult>(candidate.clone());
            assert_eq!(parsed.is_ok(), accepted, "source URL: {suffix:?}");
            if let Ok(parsed) = parsed {
                assert_eq!(serde_json::to_value(parsed).unwrap(), candidate);
            }
        }
        assert_eq!(
            value["answer"],
            serde_json::json!({"kind":"text", "text":"Synthetic unsaved answer"})
        );
        assert_eq!(value["reason"], "storage_error");
        assert_eq!(
            serde_json::from_value::<UnsavedResult>(value.clone()).unwrap(),
            result
        );
        assert_eq!(result.request_id(), "synthetic-renderer-request");
        assert_eq!(
            result.execution_id().generation.as_str(),
            format!("{replica}:2")
        );
        assert_eq!(result.prompt(), "Synthetic prompt");
        assert_eq!(
            result.answer(),
            &MessageContentKind::Text("Synthetic unsaved answer".into())
        );
        assert!(result.sources().is_empty());
        assert_eq!(result.final_stage(), Stage::Review);
        assert_eq!(result.reason(), UnsavedReason::StorageError);
        for key in [
            "request_id",
            "execution_id",
            "prompt",
            "answer",
            "sources",
            "final_stage",
            "reason",
        ] {
            let mut missing = value.clone();
            missing.as_object_mut().unwrap().remove(key);
            assert!(
                serde_json::from_value::<UnsavedResult>(missing).is_err(),
                "missing {key}"
            );
        }
        for (key, replacement) in [
            ("request_id", serde_json::json!("")),
            ("request_id", serde_json::json!(" \t\r\n")),
            (
                "request_id",
                serde_json::json!("😀".repeat(conversation::MAX_REQUEST_ID_BYTES / 4 + 1)),
            ),
            (
                "request_id",
                serde_json::json!("x".repeat(conversation::MAX_REQUEST_ID_BYTES + 1)),
            ),
            (
                "execution_id",
                serde_json::json!({"session_id":"1","generation":"2"}),
            ),
            ("prompt", serde_json::json!(" ")),
            (
                "prompt",
                serde_json::json!("😀".repeat(conversation::MAX_PROMPT_BYTES / 4 + 1)),
            ),
            (
                "answer",
                serde_json::json!({"kind":"text","text":"x".repeat(conversation::MAX_COMMITTED_TEXT_BYTES)}),
            ),
            (
                "sources",
                serde_json::json!([{"url":"not a source URL","title":"Synthetic"}]),
            ),
            ("reason", serde_json::json!("raw_storage_error")),
            ("turn", serde_json::json!({})),
        ] {
            let mut invalid = value.clone();
            invalid[key] = replacement;
            assert!(
                serde_json::from_value::<UnsavedResult>(invalid).is_err(),
                "invalid {key}"
            );
        }
        let mut refusal = value;
        refusal["answer"] = serde_json::json!({"kind":"refusal","text":"Synthetic refusal"});
        refusal["reason"] = serde_json::json!("context_unavailable");
        assert!(serde_json::from_value::<UnsavedResult>(refusal.clone()).is_ok());
        refusal["sources"] = serde_json::json!([{"url":"https://example.test/synthetic","title":"Synthetic source"}]);
        assert!(serde_json::from_value::<UnsavedResult>(refusal).is_err());
    }
    #[test]
    fn durable_turn_projection_keeps_canonical_sources_and_request_local_alias_separate() {
        use crate::durable_conversation::Record;
        let id = |sequence| {
            serde_json::json!({
                "replica": "11111111-1111-4111-8111-111111111111", "sequence": sequence,
            })
        };
        let record: Record = serde_json::from_value(serde_json::json!({
            "body_key": "a".repeat(64),
            "operation": {"version": 2, "id": id("3"), "body": {
                "kind": "turn_completed", "conversation": id("1"), "parent": null,
                "request": id("2"), "prompt": "Synthetic prompt",
                "answer": {"kind": "text", "text": "Synthetic answer"},
                "sources": [{"url": "https://example.test/document", "title": "Synthetic observed source"}],
                "terminal_stage": "instant", "provenance": {
                    "requested_model": "synthetic-model", "returned_model": null,
                    "policy_id": "synthetic-policy", "policy_version": "2",
                    "instruction_profile_id": "synthetic-profile", "instruction_profile_version": "2", "context": [],
                },
            }},
        })).unwrap();
        let original = record.clone();
        let projected = super::turn_from_record(&record, Some("synthetic-active-request")).unwrap();
        assert_eq!(projected.request_id, "synthetic-active-request");
        assert_eq!(projected.user.id, projected.answer.id);
        assert_eq!(projected.sources[0].url, "https://example.test/document");
        assert_eq!(
            projected.answer.content,
            super::MessageContentKind::Text("Synthetic answer".into())
        );
        assert_eq!(record, original);
        let cold = super::turn_from_record(&record, None).unwrap();
        assert_eq!(cold.request_id, "11111111-1111-4111-8111-111111111111:2");
        for invalid in [
            "".to_owned(),
            " \t\r\n".to_owned(),
            "\u{2003}".to_owned(),
            "😀".repeat(crate::conversation::MAX_REQUEST_ID_BYTES / 4 + 1),
        ] {
            assert!(super::turn_from_record(&record, Some(&invalid)).is_err());
        }
        for alias in [
            "synthetic-renderer-correlation".to_owned(),
            " synthetic preserved correlation ".to_owned(),
            "😀".repeat(crate::conversation::MAX_REQUEST_ID_BYTES / 4),
        ] {
            let mut session = crate::conversation::ConversationSession::execution_only(
                crate::assistant::AssistantPolicy::default(),
            );
            assert!(session
                .admit_with_context(&alias, "Synthetic prompt", Vec::new())
                .is_ok());
            let turn = super::turn_from_record(&record, Some(&alias)).unwrap();
            assert_eq!(turn.request_id, alias);
            let unsaved = super::UnsavedResult::new(
                alias.clone(),
                turn.execution_id,
                "Synthetic prompt".into(),
                crate::durable_conversation::Answer::Text {
                    text: "Synthetic answer".into(),
                },
                vec![],
                super::Stage::Instant,
                super::UnsavedReason::StorageError,
            )
            .unwrap();
            assert_eq!(unsaved.request_id(), alias);
        }
    }

    #[test]
    fn canonical_graph_ids_round_trip_without_numeric_coercion() {
        let value = "11111111-1111-4111-8111-111111111111:18446744073709551615";
        let id: super::WireId = serde_json::from_value(serde_json::json!(value)).unwrap();
        assert_eq!(id.as_str(), value);
        assert_eq!(serde_json::to_value(id).unwrap(), value);
        let operation = crate::durable_conversation::OperationId {
            replica: "11111111-1111-4111-8111-111111111111".into(),
            sequence: u64::MAX.to_string(),
        };
        assert_eq!(
            super::WireId::from_operation_id(&operation)
                .unwrap()
                .as_str(),
            value
        );
        for invalid in [
            "11111111-1111-4111-8111-111111111111:0",
            "11111111-1111-4111-8111-111111111111:01",
            "11111111-1111-4111-8111-111111111111:18446744073709551616",
            "synthetic-invalid:1",
        ] {
            assert!(serde_json::from_value::<super::WireId>(serde_json::json!(invalid)).is_err());
        }
    }

    #[test]
    fn portable_model_catalog_matches_wire_and_domain_contracts() {
        use super::InstantModelChoice as Wire;
        use crate::assistant::InstantModel as Domain;
        let cases = [
            (
                Wire::LocalFast,
                Domain::LocalFast,
                "local-fast",
                "local_fast",
            ),
            (
                Wire::LocalQuality,
                Domain::LocalQuality,
                "local-quality",
                "local_quality",
            ),
            (
                Wire::CustomSolLow,
                Domain::CustomSolLow,
                "custom-sol-low",
                "custom_provider",
            ),
            (
                Wire::CustomSolMedium,
                Domain::CustomSolMedium,
                "custom-sol-medium",
                "custom_provider",
            ),
            (
                Wire::CustomSolHigh,
                Domain::CustomSolHigh,
                "custom-sol-high",
                "custom_provider",
            ),
            (
                Wire::CustomSolXhigh,
                Domain::CustomSolXhigh,
                "custom-sol-xhigh",
                "custom_provider",
            ),
            (
                Wire::CustomSolMax,
                Domain::CustomSolMax,
                "custom-sol-max",
                "custom_provider",
            ),
            (
                Wire::CustomLunaNone,
                Domain::CustomLunaNone,
                "custom-luna-none",
                "custom_provider",
            ),
            (
                Wire::CustomLuna6Low,
                Domain::CustomLuna6Low,
                "custom-luna6-low",
                "custom_provider",
            ),
            (
                Wire::CustomLuna6Medium,
                Domain::CustomLuna6Medium,
                "custom-luna6-medium",
                "custom_provider",
            ),
            (
                Wire::CustomLuna6High,
                Domain::CustomLuna6High,
                "custom-luna6-high",
                "custom_provider",
            ),
            (
                Wire::CustomLuna6Xhigh,
                Domain::CustomLuna6Xhigh,
                "custom-luna6-xhigh",
                "custom_provider",
            ),
            (
                Wire::CustomLuna6Max,
                Domain::CustomLuna6Max,
                "custom-luna6-max",
                "custom_provider",
            ),
            (
                Wire::CustomLuna56None,
                Domain::CustomLuna56None,
                "custom-luna56-none",
                "custom_provider",
            ),
            (
                Wire::CustomLunaLow,
                Domain::CustomLunaLow,
                "custom-luna-low",
                "custom_provider",
            ),
            (
                Wire::CustomLuna56Medium,
                Domain::CustomLuna56Medium,
                "custom-luna56-medium",
                "custom_provider",
            ),
            (
                Wire::CustomLuna56High,
                Domain::CustomLuna56High,
                "custom-luna56-high",
                "custom_provider",
            ),
            (
                Wire::CustomLuna56Xhigh,
                Domain::CustomLuna56Xhigh,
                "custom-luna56-xhigh",
                "custom_provider",
            ),
            (
                Wire::CustomLuna56Max,
                Domain::CustomLuna56Max,
                "custom-luna56-max",
                "custom_provider",
            ),
            (
                Wire::OpenCodeSolLow,
                Domain::OpenCodeSolLow,
                "opencode-sol-low",
                "opencode",
            ),
            (
                Wire::OpenCodeSolMedium,
                Domain::OpenCodeSolMedium,
                "opencode-sol-medium",
                "opencode",
            ),
            (
                Wire::OpenCodeSolHigh,
                Domain::OpenCodeSolHigh,
                "opencode-sol-high",
                "opencode",
            ),
            (
                Wire::OpenCodeSolXhigh,
                Domain::OpenCodeSolXhigh,
                "opencode-sol-xhigh",
                "opencode",
            ),
            (
                Wire::OpenCodeSolMax,
                Domain::OpenCodeSolMax,
                "opencode-sol-max",
                "opencode",
            ),
            (
                Wire::OpenCodeLunaNone,
                Domain::OpenCodeLunaNone,
                "opencode-luna-none",
                "opencode",
            ),
            (
                Wire::OpenCodeLunaLow,
                Domain::OpenCodeLunaLow,
                "opencode-luna-low",
                "opencode",
            ),
            (
                Wire::OpenCodeLunaMedium,
                Domain::OpenCodeLunaMedium,
                "opencode-luna-medium",
                "opencode",
            ),
            (
                Wire::OpenCodeLunaHigh,
                Domain::OpenCodeLunaHigh,
                "opencode-luna-high",
                "opencode",
            ),
            (
                Wire::OpenCodeLunaXhigh,
                Domain::OpenCodeLunaXhigh,
                "opencode-luna-xhigh",
                "opencode",
            ),
            (
                Wire::OpenCodeLunaMax,
                Domain::OpenCodeLunaMax,
                "opencode-luna-max",
                "opencode",
            ),
        ];
        assert_eq!(
            Wire::ALL.to_vec(),
            cases.iter().map(|case| case.0).collect::<Vec<_>>()
        );
        for (wire, domain, id, capability) in cases {
            assert_eq!(serde_json::to_value(wire).unwrap(), id);
            assert_eq!(
                serde_json::from_value::<Wire>(serde_json::json!(id)).unwrap(),
                wire
            );
            assert_eq!(Domain::from(wire), domain);
            assert_eq!(Wire::from(domain), wire);
            assert_eq!(wire.as_str(), id);
            assert_eq!(wire.capability_id(), capability);
            assert!(!wire.label().trim().is_empty());
        }
    }
    use super::*;
    use crate::assistant::{Content, Role, Stage as AssistantStage};
    use crate::conversation::{
        ExecutionId as ConversationExecutionId, GenerationId, MessageId, RequestId, SessionId,
    };

    fn refusal_turn() -> conversation::Turn {
        conversation::Turn {
            sources: Vec::new(),
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
            fast_mode: false,
            ui_theme: UiTheme::Dark,
            ui_text_size: UiTextSize::Large,
            speech_language: BoundedText::default(),
            speech_tts_provider: BoundedText::default(),
            speech_tts_voice: BoundedText::default(),
        }
    }

    #[test]
    fn preferences_round_trip_all_themes_and_reject_unknown_values() {
        for theme in ["dark", "slate", "light"] {
            let mut wire = serde_json::to_value(preferences()).unwrap();
            wire["ui_theme"] = theme.into();
            let parsed: ValidatedPreferences = serde_json::from_value(wire).unwrap();
            assert_eq!(serde_json::to_value(parsed).unwrap()["ui_theme"], theme);
        }
        for theme in ["sepia", "dim", "sleek-dark"] {
            let mut wire = serde_json::to_value(preferences()).unwrap();
            wire["ui_theme"] = theme.into();
            assert!(serde_json::from_value::<ValidatedPreferences>(wire).is_err());
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
            (assistant::InstantModel::CustomLunaNone, "custom-luna-none"),
            (
                assistant::InstantModel::CustomLuna56None,
                "custom-luna56-none",
            ),
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
    fn sourceful_ipc_roundtrip_rejects_invalid_metadata() {
        let mut canonical = text_turn();
        canonical.sources = vec![crate::sources::SourceLink {
            url: "https://example.test/synthetic".into(),
            title: "Synthetic source".into(),
        }];
        let turn = Turn::from(canonical);
        let encoded = serde_json::to_string(&turn).unwrap();
        assert_eq!(serde_json::from_str::<Turn>(&encoded).unwrap(), turn);
        assert!(PromptOutcome::completed(turn.clone()).is_ok());
        let mut invalid = turn.clone();
        invalid.sources = vec![turn.sources[0].clone(); 33];
        assert_eq!(
            PromptOutcome::completed(invalid.clone()),
            Err(PromptOutcomeValidationError::InvalidSources)
        );
        assert!(serde_json::from_value::<Turn>(serde_json::to_value(invalid).unwrap()).is_err());
        let mut refusal = Turn::from(refusal_turn());
        refusal.sources = turn.sources;
        assert_eq!(
            PromptOutcome::refused(refusal.clone()),
            Err(PromptOutcomeValidationError::InvalidSources)
        );
        assert!(serde_json::from_value::<Turn>(serde_json::to_value(refusal).unwrap()).is_err());
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
                "final_stage": "review",
                "sources": []
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
            serde_json::json!({ "kind": "refused", "save_disposition": "saved", "turn": refused_turn })
        );
        assert_eq!(
            serde_json::to_value(PromptOutcome::completed(completed_turn.clone()).unwrap())
                .unwrap(),
            serde_json::json!({ "kind": "completed", "save_disposition": "saved", "turn": completed_turn })
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
            unsaved_result: None,
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
        let mut value = serde_json::to_value(snapshot).unwrap();
        assert_eq!(value["session_id"], "4");
        assert_eq!(value["runtime_phase"], "executing");
        assert_eq!(
            value["transcript"][0]["answer"]["content"]["kind"],
            "refusal"
        );
        assert_eq!(value["active_request"]["execution_id"]["generation"], "10");
        assert_eq!(value["preferences"]["instant"], "opencode-sol-high");
        assert_eq!(value["model_options"][0]["available"], true);
        assert_eq!(value["unsaved_result"], serde_json::Value::Null);
        assert!(serde_json::from_value::<AppSnapshot>(value.clone()).is_ok());
        value.as_object_mut().unwrap().remove("unsaved_result");
        assert!(serde_json::from_value::<AppSnapshot>(value).is_err());
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
