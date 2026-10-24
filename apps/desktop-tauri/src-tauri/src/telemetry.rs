//! Closed metadata vocabulary; persistence belongs exclusively to diagnostics.
use serde::Serialize;

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InputSource {
    Voice,
    Text,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Provider {
    Local,
    Custom,
    OpenCode,
}
#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    Generation,
    PartialTranscription,
    CompletionPrediction,
    Prefetch,
    Deep,
    Review,
}
#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Transport {
    InProcess,
    Http,
}
#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InferenceProvider {
    Cuda,
    Cpu,
    AttachedUnknown,
    Remote,
}
#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SpeculativeOrigin {
    None,
    User,
    System,
}
#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Ok,
    Error,
}
#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCategory {
    None,
    Internal,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum Model {
    #[serde(rename = "gemma-fast")]
    GemmaFast,
    #[serde(rename = "gemma-quality")]
    GemmaQuality,
    #[serde(rename = "gpt-6.1-sol")]
    Sol,
    #[serde(rename = "gpt-6-luna")]
    Luna6,
    #[serde(rename = "gpt-5.6-luna")]
    Luna56,
    #[serde(rename = "parakeet-v2-int8")]
    Parakeet,
    #[serde(rename = "qwen-completion")]
    QwenCompletion,
}

#[derive(Clone, Debug, Serialize)]
pub struct TelemetryMetadata {
    pub generation: u64,
    pub input_source: InputSource,
    pub provider: Provider,
    pub model: Model,
    pub stage: Stage,
    pub transport: Transport,
    pub inference_provider: InferenceProvider,
    pub speculative_origin: SpeculativeOrigin,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub duration_ms: Option<u64>,
    pub status: Status,
    pub error_category: ErrorCategory,
}
