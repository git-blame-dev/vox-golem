#![forbid(unsafe_code)]
#![deny(unused_must_use)]

use base64::Engine;
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, TryLockError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tauri::{Emitter, Manager};

#[cfg(target_os = "linux")]
use webkit2gtk::glib::prelude::Cast;
#[cfg(target_os = "linux")]
use webkit2gtk::UserMediaPermissionRequest;
#[cfg(target_os = "linux")]
use webkit2gtk::{PermissionRequestExt, UserMediaPermissionRequestExt, WebViewExt};

mod livekit_wakeword;
mod partial_transcription;
mod telemetry;
mod transcription;
#[allow(dead_code)]
mod tts;
mod voice_activity;
mod wake_word;

const DEFAULT_SILENCE_TIMEOUT_MS: u64 = 1_500;
const DEFAULT_PREROLL_MAX_SAMPLES: usize = 4_000;
const DEFAULT_UTTERANCE_MAX_SAMPLES: usize = 4_800_000;
const PARTIAL_TRANSCRIPTION_MINIMUM_SAMPLES: usize = 8_000;
const PARTIAL_TRANSCRIPTION_MAXIMUM_SAMPLES: usize = 480_000;
const PARTIAL_TRANSCRIPTION_THROTTLE: Duration = Duration::from_millis(350);
const COMPLETION_PROMPT_MAX_BYTES: usize = 32 * 1024;
const DEFAULT_TELEMETRY_MAX_BYTES: usize = 10 * 1024 * 1024;
const DEFAULT_TELEMETRY_BACKUP_COUNT: u8 = 3;
const LLAMA_CPP_MODEL_ALIAS: &str = "default";
const LLAMA_CPP_MAX_TOKENS: u16 = 512;
const LLAMA_CPP_CONTEXT_WINDOW_TOKENS: usize = 8_192;
const LLAMA_CPP_CONTEXT_SAFETY_MARGIN_TOKENS: usize = 512;
const LLAMA_CPP_CHAT_WRAPPER_TOKENS: usize = 64;
const RESPONSE_PROFILE_STATE_FILE: &str = "state.toml";
const RUNTIME_LOG_DIR: &str = "logs";
const RUNTIME_LOG_FILE: &str = "runtime.log";
const RUNTIME_LOG_MESSAGE_MAX_CHARS: usize = 16_384;
const PROMPT_MAX_BYTES: usize = 64 * 1024;
const PROVIDER_HISTORY_MAX_BYTES: usize = 512 * 1024;
const WINDOWS_SOUL_FILE_NAME: &str = "SOUL.md";
const OPENCODE_PROMPT_INACTIVITY_TIMEOUT: Duration = Duration::from_secs(300);
const OPENCODE_PROMPT_CANCELLATION_TIMEOUT: Duration = Duration::from_secs(5);
const LLAMA_CPP_ROLLOVER_REASON: &str =
    "Context budget reached; started a new local Gemma conversation for this reply.";
const CUE_AUDIO_DATA_URL_PREFIX: &str = "data:audio/wav;base64,";
const START_LISTENING_CUE_WAV: &[u8] = include_bytes!("../resources/start-listening.wav");
const STOP_LISTENING_CUE_WAV: &[u8] = include_bytes!("../resources/stop-listening.wav");
const STARTUP_READY_MARKER: &str = "VOXGOLEM_STARTUP_READY";
static PERSISTED_STATE_LOCK: Mutex<()> = Mutex::new(());
static TELEMETRY_ERROR_REPORTED: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Debug, PartialEq, Eq)]
struct LlamaConversationTurn {
    user: String,
    assistant: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct LlamaPromptInput {
    user_prompt: String,
    rolled_over: bool,
}

type LlamaStartupRegistry = Arc<
    Mutex<
        Vec<(
            voxgolem_platform::llama_cpp::LlamaCppStartupCancellation,
            std::thread::JoinHandle<()>,
        )>,
    >,
>;

struct AppState {
    startup_state: Arc<Mutex<StartupStatePayload>>,
    runtime_config: Option<voxgolem_core::config::RuntimeConfig>,
    selected_response_profile: Arc<Mutex<ResponseProfilePayload>>,
    supported_response_profiles: Vec<ResponseProfilePayload>,
    response_profile_switch_generation: Arc<AtomicU64>,
    response_backend_operation_lock: Mutex<()>,
    voice_pipeline_config: voxgolem_core::voice_pipeline::VoicePipelineConfig,
    voice_pipeline_state: Mutex<voxgolem_core::voice_pipeline::VoicePipelineState>,
    wake_word_runtime: Option<Mutex<wake_word::WakeWordRuntime>>,
    voice_activity_runtime: Option<Mutex<voice_activity::VoiceActivityRuntime>>,
    parakeet_runtime: Option<Arc<Mutex<transcription::ParakeetRuntime>>>,
    partial_transcription: Arc<Mutex<partial_transcription::PartialTranscriptionScheduler>>,
    partial_voice_session: AtomicU64,
    completion_runtime: Mutex<Option<voxgolem_platform::completion::CompletionRuntime>>,
    completion_request: Arc<Mutex<Option<voxgolem_platform::completion::CompletionRequestHandle>>>,
    completion_context: Arc<Mutex<Option<CompletionRequestContext>>>,
    completion_generation: AtomicU64,
    completion_lifecycle_lock: Mutex<()>,
    telemetry_sink: Option<Arc<Mutex<telemetry::TelemetrySink>>>,
    assistant_coordinator: Arc<Mutex<voxgolem_core::assistant::AssistantCoordinator>>,
    assistant_settings_generation: Arc<AtomicU64>,
    tts_operation_lock: tokio::sync::Mutex<()>,
    local_tts_runtime: Mutex<Option<Arc<tts::LocalTtsRuntime>>>,
    llama_cpp_runtime: Arc<Mutex<Option<voxgolem_platform::llama_cpp::LlamaCppRuntime>>>,
    llama_cpp_conversation: Mutex<Vec<LlamaConversationTurn>>,
    llama_cpp_system_prompt: Option<String>,
    opencode_server: Arc<Mutex<Option<voxgolem_platform::opencode::OpencodeServer>>>,
    active_prompt: Arc<Mutex<Option<ActivePrompt>>>,
    active_prompt_generation: AtomicU64,
    prefetch_generation: AtomicU64,
    prefetch_cache: Mutex<Option<PrefetchEntry>>,
    prefetch_task: Mutex<Option<ActivePrefetch>>,
    llama_startups: LlamaStartupRegistry,
    exit_cleanup_started: AtomicBool,
}

#[derive(Clone)]
struct ActivePrompt {
    request_id: String,
    generation: u64,
    assistant_generation: voxgolem_core::assistant::Generation,
    cancelled: Arc<AtomicBool>,
    cancellation_signal: Arc<tokio::sync::Notify>,
    completion_signal: Arc<tokio::sync::Notify>,
    client: Option<voxgolem_platform::opencode::OpencodeClient>,
}

struct ActivePromptGuard {
    active_prompt: Arc<Mutex<Option<ActivePrompt>>>,
    request_id: String,
    generation: u64,
}

struct AssistantRequestGuard {
    coordinator: Arc<Mutex<voxgolem_core::assistant::AssistantCoordinator>>,
    generation: voxgolem_core::assistant::Generation,
}

impl Drop for AssistantRequestGuard {
    fn drop(&mut self) {
        if let Ok(mut coordinator) = self.coordinator.lock() {
            coordinator.cancel(self.generation);
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct PrefetchKey {
    prompt: String,
    history: Vec<voxgolem_core::assistant::ConversationTurn>,
    model: voxgolem_core::assistant::InstantModel,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct PrefetchEntry {
    generation: u64,
    key: PrefetchKey,
    answer: String,
}

struct ActivePrefetch {
    generation: u64,
    cancelled: Arc<AtomicBool>,
    cancellation_signal: Arc<tokio::sync::Notify>,
    task: Option<tauri::async_runtime::JoinHandle<()>>,
}

impl Drop for ActivePromptGuard {
    fn drop(&mut self) {
        let _ = clear_active_prompt(&self.active_prompt, &self.request_id, self.generation);
    }
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum RuntimePhasePayload {
    Initializing,
    Sleeping,
    Listening,
    Processing,
    Executing,
    Error,
}

impl RuntimePhasePayload {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Initializing => "initializing",
            Self::Sleeping => "sleeping",
            Self::Listening => "listening",
            Self::Processing => "processing",
            Self::Executing => "executing",
            Self::Error => "error",
        }
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum PromptExecutionEventPayload {
    Text {
        text: String,
    },
    Correction {
        text: String,
        correction: String,
    },
    Reasoning {
        text: String,
    },
    Status {
        message: String,
    },
    Tool {
        tool: String,
        status: String,
        detail: String,
    },
    Error {
        message: String,
    },
    Completed {
        runtime_phase: RuntimePhasePayload,
    },
    Cancelled {
        runtime_phase: RuntimePhasePayload,
    },
}

#[derive(Clone, Debug, Serialize)]
struct PromptEventEnvelope {
    request_id: String,
    #[serde(flatten)]
    event: PromptExecutionEventPayload,
}

#[derive(Clone, Debug, Serialize)]
struct PartialTranscriptionEventPayload {
    session_id: partial_transcription::VoiceSessionId,
    revision: u64,
    text: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum CompletionSource {
    Typed,
    Voice,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct CompletionRequestContext {
    backend_revision: u64,
    client_revision: u64,
    source: CompletionSource,
    voice_session_id: Option<partial_transcription::VoiceSessionId>,
    prompt: String,
    started_ms: u64,
}

#[derive(Clone, Debug, Serialize)]
struct CompletionEventPayload {
    source: CompletionSource,
    revision: u64,
    voice_session_id: Option<partial_transcription::VoiceSessionId>,
    suffix: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
struct PromptExecutionPayload {
    events: Vec<PromptExecutionEventPayload>,
    stderr: String,
    exit_code: Option<i32>,
    runtime_phase: RuntimePhasePayload,
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum FrontendRuntimeDiagnosticKind {
    FrontendNotice,
    Cue,
    RuntimeControl,
    Execution,
    Tts,
    Audio,
    Profile,
}

impl FrontendRuntimeDiagnosticKind {
    fn as_log_subsystem(self) -> &'static str {
        match self {
            Self::FrontendNotice => "frontend",
            Self::Cue => "cue",
            Self::RuntimeControl => "runtime-control",
            Self::Execution => "execution",
            Self::Tts => "tts",
            Self::Audio => "audio",
            Self::Profile => "profile",
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
struct FrontendRuntimeDiagnosticPayload {
    kind: FrontendRuntimeDiagnosticKind,
    detail: String,
}

struct PromptExecutionOutcome {
    events: Vec<PromptExecutionEventPayload>,
    stderr: String,
    exit_code: Option<i32>,
    error_message: Option<String>,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
struct RuntimePhaseResponsePayload {
    runtime_phase: RuntimePhasePayload,
    transcription_ready_samples: Option<usize>,
    transcript_text: Option<String>,
    last_activity_ms: Option<u64>,
    capturing_utterance: bool,
    preroll_samples: usize,
    utterance_samples: usize,
    telemetry: Option<RuntimeTelemetryPayload>,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
struct RuntimeTelemetryPayload {
    frame_id: Option<String>,
    backend_ingest_started_ms: Option<u64>,
    backend_ingest_completed_ms: Option<u64>,
    wake_detected_ms: Option<u64>,
    wake_confidence: Option<f32>,
    transcription_started_ms: Option<u64>,
    transcription_completed_ms: Option<u64>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
struct CueAssetPathsPayload {
    start_listening: String,
    stop_listening: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum ResponseProfilePayload {
    Fast,
    Quality,
}

impl ResponseProfilePayload {
    fn as_str(self) -> &'static str {
        match self {
            Self::Fast => "fast",
            Self::Quality => "quality",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
enum InstantChoicePayload {
    LocalFast,
    LocalQuality,
    CustomSolHigh,
    CustomLunaLow,
    #[serde(rename = "opencode-sol-high")]
    OpenCodeSolHigh,
    #[serde(rename = "opencode-luna-low")]
    OpenCodeLunaLow,
}

impl InstantChoicePayload {
    fn as_str(self) -> &'static str {
        match self {
            Self::LocalFast => "local-fast",
            Self::LocalQuality => "local-quality",
            Self::CustomSolHigh => "custom-sol-high",
            Self::CustomLunaLow => "custom-luna-low",
            Self::OpenCodeSolHigh => "opencode-sol-high",
            Self::OpenCodeLunaLow => "opencode-luna-low",
        }
    }

    fn capability_id(self) -> &'static str {
        match self {
            Self::LocalFast => "local_fast",
            Self::LocalQuality => "local_quality",
            Self::CustomSolHigh | Self::CustomLunaLow => "custom_provider",
            Self::OpenCodeSolHigh | Self::OpenCodeLunaLow => "opencode",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
enum AgentChoicePayload {
    CustomSolHigh,
    CustomLunaLow,
    #[serde(rename = "opencode-sol-high")]
    OpenCodeSolHigh,
    #[serde(rename = "opencode-luna-low")]
    OpenCodeLunaLow,
}

impl AgentChoicePayload {
    fn as_str(self) -> &'static str {
        match self {
            Self::CustomSolHigh => "custom-sol-high",
            Self::CustomLunaLow => "custom-luna-low",
            Self::OpenCodeSolHigh => "opencode-sol-high",
            Self::OpenCodeLunaLow => "opencode-luna-low",
        }
    }

    fn capability_id(self) -> &'static str {
        match self {
            Self::CustomSolHigh | Self::CustomLunaLow => "custom_provider",
            Self::OpenCodeSolHigh | Self::OpenCodeLunaLow => "opencode",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
struct AssistantSettingsPayload {
    instant: InstantChoicePayload,
    deep: AgentChoicePayload,
    review: AgentChoicePayload,
    deep_enabled: bool,
    review_enabled: bool,
    prefetch: bool,
    completion: bool,
}

impl Default for AssistantSettingsPayload {
    fn default() -> Self {
        Self {
            instant: InstantChoicePayload::LocalFast,
            deep: AgentChoicePayload::OpenCodeSolHigh,
            review: AgentChoicePayload::OpenCodeSolHigh,
            deep_enabled: false,
            review_enabled: false,
            prefetch: false,
            completion: true,
        }
    }
}

impl From<AssistantSettingsPayload> for voxgolem_core::assistant::AssistantPreferences {
    fn from(settings: AssistantSettingsPayload) -> Self {
        use voxgolem_core::assistant::InstantModel;
        Self {
            instant_model: match settings.instant {
                InstantChoicePayload::LocalFast => InstantModel::LocalFast,
                InstantChoicePayload::LocalQuality => InstantModel::LocalQuality,
                InstantChoicePayload::CustomSolHigh => InstantModel::CustomSolHigh,
                InstantChoicePayload::CustomLunaLow => InstantModel::CustomLunaLow,
                InstantChoicePayload::OpenCodeSolHigh => InstantModel::OpenCodeSolHigh,
                InstantChoicePayload::OpenCodeLunaLow => InstantModel::OpenCodeLunaLow,
            },
            deep_model: agent_model(settings.deep),
            review_model: agent_model(settings.review),
            deep_enabled: settings.deep_enabled,
            review_enabled: settings.review_enabled,
            prefetch_enabled: settings.prefetch,
            completion_enabled: settings.completion,
        }
    }
}

fn agent_model(choice: AgentChoicePayload) -> voxgolem_core::assistant::AgentModel {
    use voxgolem_core::assistant::AgentModel;
    match choice {
        AgentChoicePayload::CustomSolHigh => AgentModel::CustomSolHigh,
        AgentChoicePayload::CustomLunaLow => AgentModel::CustomLunaLow,
        AgentChoicePayload::OpenCodeSolHigh => AgentModel::OpenCodeSolHigh,
        AgentChoicePayload::OpenCodeLunaLow => AgentModel::OpenCodeLunaLow,
    }
}

impl From<&voxgolem_core::assistant::AssistantPreferences> for AssistantSettingsPayload {
    fn from(preferences: &voxgolem_core::assistant::AssistantPreferences) -> Self {
        use voxgolem_core::assistant::{AgentModel, InstantModel};
        Self {
            instant: match preferences.instant_model {
                InstantModel::LocalFast => InstantChoicePayload::LocalFast,
                InstantModel::LocalQuality => InstantChoicePayload::LocalQuality,
                InstantModel::CustomSolHigh => InstantChoicePayload::CustomSolHigh,
                InstantModel::CustomLunaLow => InstantChoicePayload::CustomLunaLow,
                InstantModel::OpenCodeSolHigh => InstantChoicePayload::OpenCodeSolHigh,
                InstantModel::OpenCodeLunaLow => InstantChoicePayload::OpenCodeLunaLow,
            },
            deep: match preferences.deep_model {
                AgentModel::CustomSolHigh => AgentChoicePayload::CustomSolHigh,
                AgentModel::CustomLunaLow => AgentChoicePayload::CustomLunaLow,
                AgentModel::OpenCodeSolHigh => AgentChoicePayload::OpenCodeSolHigh,
                AgentModel::OpenCodeLunaLow => AgentChoicePayload::OpenCodeLunaLow,
            },
            review: match preferences.review_model {
                AgentModel::CustomSolHigh => AgentChoicePayload::CustomSolHigh,
                AgentModel::CustomLunaLow => AgentChoicePayload::CustomLunaLow,
                AgentModel::OpenCodeSolHigh => AgentChoicePayload::OpenCodeSolHigh,
                AgentModel::OpenCodeLunaLow => AgentChoicePayload::OpenCodeLunaLow,
            },
            deep_enabled: preferences.deep_enabled,
            review_enabled: preferences.review_enabled,
            prefetch: preferences.prefetch_enabled,
            completion: preferences.completion_enabled,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum UiTextSizePayload {
    Small,
    Medium,
    Large,
    ExtraLarge,
}

impl UiTextSizePayload {
    fn as_str(self) -> &'static str {
        match self {
            Self::Small => "small",
            Self::Medium => "medium",
            Self::Large => "large",
            Self::ExtraLarge => "extra_large",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum UiThemePayload {
    Light,
    Dark,
}

impl UiThemePayload {
    fn as_str(self) -> &'static str {
        match self {
            Self::Light => "light",
            Self::Dark => "dark",
        }
    }
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
struct SwitchResponseProfilePayload {
    selected_response_profile: ResponseProfilePayload,
    supported_response_profiles: Vec<ResponseProfilePayload>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
struct SetTtsEnabledPayload {
    enabled: bool,
    sample_rate_hz: u32,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
struct SynthesizeLocalTtsPayload {
    pcm_f32: Vec<f32>,
    sample_rate_hz: u32,
    duration_ms: u64,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
struct CapabilityPayload {
    id: &'static str,
    state: CapabilityStatePayload,
    reason: String,
    actual_provider: Option<&'static str>,
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum CapabilityStatePayload {
    Available,
    Warming,
    NotConfigured,
    Unavailable,
    Failed,
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum StartupStatePayload {
    WarmingModel {
        cue_asset_paths: CueAssetPathsPayload,
        runtime_phase: RuntimePhasePayload,
        voice_input_available: bool,
        voice_input_error: Option<String>,
        silence_timeout_ms: u64,
        message: String,
        selected_response_profile: ResponseProfilePayload,
        supported_response_profiles: Vec<ResponseProfilePayload>,
        prompt_cancellation_available: bool,
        tts_enabled: bool,
        tts_output_gain_db: f32,
        capabilities: Vec<CapabilityPayload>,
    },
    Ready {
        cue_asset_paths: CueAssetPathsPayload,
        runtime_phase: RuntimePhasePayload,
        voice_input_available: bool,
        voice_input_error: Option<String>,
        silence_timeout_ms: u64,
        selected_response_profile: ResponseProfilePayload,
        supported_response_profiles: Vec<ResponseProfilePayload>,
        prompt_cancellation_available: bool,
        tts_enabled: bool,
        tts_output_gain_db: f32,
        capabilities: Vec<CapabilityPayload>,
    },
    Error {
        message: String,
    },
}

#[tauri::command]
fn get_startup_state(app_state: tauri::State<'_, AppState>) -> StartupStatePayload {
    app_state
        .startup_state
        .lock()
        .expect("startup state lock should not be poisoned")
        .clone()
}

#[tauri::command]
async fn set_tts_enabled(
    enabled: bool,
    app_state: tauri::State<'_, AppState>,
) -> Result<SetTtsEnabledPayload, String> {
    let config = app_state
        .runtime_config
        .as_ref()
        .ok_or_else(|| String::from("startup config is not ready"))?;

    let tts_config = config.local_tts.clone();
    let runtime_file_logging_enabled = config.logging.enabled;
    let _operation_guard = app_state.tts_operation_lock.lock().await;
    let needs_runtime = enabled
        && app_state
            .local_tts_runtime
            .lock()
            .map_err(|_| String::from("local tts runtime lock is poisoned"))?
            .is_none();
    let proposed_runtime = if needs_runtime {
        tauri::async_runtime::spawn_blocking(move || {
            initialize_local_tts_runtime(&tts_config, true, runtime_file_logging_enabled)
        })
        .await
        .map_err(|error| format!("local tts initialization task failed: {error}"))??
    } else {
        None
    };
    let mut runtime_guard = app_state
        .local_tts_runtime
        .lock()
        .map_err(|_| String::from("local tts runtime lock is poisoned"))?;
    persist_tts_enabled(enabled)?;
    if enabled {
        if runtime_guard.is_none() {
            *runtime_guard = proposed_runtime.map(Arc::new);
            log_tts_runtime_event(config.logging.enabled, "runtime enabled");
        }
    } else {
        if let Some(runtime) = runtime_guard.as_ref() {
            runtime.set_enabled(false);
        }
        *runtime_guard = None;
        log_tts_runtime_event(config.logging.enabled, "runtime disabled and unloaded");
    }
    if let Ok(mut state) = app_state.startup_state.lock() {
        let capabilities = match &mut *state {
            StartupStatePayload::WarmingModel { capabilities, .. }
            | StartupStatePayload::Ready { capabilities, .. } => capabilities,
            StartupStatePayload::Error { .. } => &mut Vec::new(),
        };
        repair_tts_capability(capabilities, enabled, runtime_guard.as_deref());
    }
    set_startup_tts_enabled(&app_state.startup_state, enabled);

    let sample_rate_hz = runtime_guard
        .as_ref()
        .map(|runtime| runtime.sample_rate_hz())
        .unwrap_or(config.local_tts.sample_rate_hz);

    Ok(SetTtsEnabledPayload {
        enabled,
        sample_rate_hz,
    })
}

#[tauri::command]
async fn synthesize_local_tts(
    text: String,
    app_state: tauri::State<'_, AppState>,
) -> Result<SynthesizeLocalTtsPayload, String> {
    let runtime_file_logging_enabled = app_state
        .runtime_config
        .as_ref()
        .map(|config| config.logging.enabled)
        .unwrap_or(false);
    let runtime = {
        let runtime_guard = app_state
            .local_tts_runtime
            .lock()
            .map_err(|_| String::from("local tts runtime lock is poisoned"))?;
        Arc::clone(runtime_guard.as_ref().ok_or_else(|| {
            log_tts_runtime_event(
                runtime_file_logging_enabled,
                "synthesis rejected: runtime unavailable",
            );
            String::from("local tts runtime is not available")
        })?)
    };
    let audio = tauri::async_runtime::spawn_blocking(move || runtime.synthesize(&text))
        .await
        .map_err(|error| format!("local tts synthesis task failed: {error}"))?
        .map_err(|error| {
            log_tts_runtime_event(
                runtime_file_logging_enabled,
                &format!("synthesis failed: {error}"),
            );
            error
        })?;

    Ok(SynthesizeLocalTtsPayload {
        pcm_f32: audio.pcm_f32,
        sample_rate_hz: audio.sample_rate_hz,
        duration_ms: audio.duration_ms,
    })
}

#[tauri::command]
fn record_frontend_runtime_diagnostic(
    event: FrontendRuntimeDiagnosticPayload,
    app_state: tauri::State<'_, AppState>,
) -> Result<(), String> {
    let runtime_file_logging_enabled = app_state
        .runtime_config
        .as_ref()
        .map(|config| config.logging.enabled)
        .unwrap_or(false);

    append_runtime_log_line(
        runtime_file_logging_enabled,
        event.kind.as_log_subsystem(),
        &event.detail,
    )
}

#[tauri::command]
fn get_ui_text_size() -> Result<UiTextSizePayload, String> {
    Ok(load_persisted_ui_text_size()?.unwrap_or(default_ui_text_size()))
}

#[tauri::command]
fn set_ui_text_size(text_size: UiTextSizePayload) -> Result<UiTextSizePayload, String> {
    persist_ui_text_size(text_size)?;
    Ok(text_size)
}

#[tauri::command]
fn get_ui_theme() -> Result<UiThemePayload, String> {
    Ok(load_persisted_ui_theme()?.unwrap_or(default_ui_theme()))
}

#[tauri::command]
fn set_ui_theme(theme: UiThemePayload) -> Result<UiThemePayload, String> {
    persist_ui_theme(theme)?;
    Ok(theme)
}

#[tauri::command]
fn get_assistant_settings(
    app_state: tauri::State<'_, AppState>,
) -> Result<AssistantSettingsPayload, String> {
    let coordinator = app_state
        .assistant_coordinator
        .lock()
        .map_err(|_| String::from("assistant coordinator lock is poisoned"))?;
    Ok(AssistantSettingsPayload::from(coordinator.preferences()))
}

#[tauri::command]
fn set_assistant_settings(
    settings: AssistantSettingsPayload,
    app_state: tauri::State<'_, AppState>,
) -> Result<AssistantSettingsPayload, String> {
    ensure_assistant_settings_available(&settings, &app_state.startup_state)?;
    let mut coordinator = app_state
        .assistant_coordinator
        .lock()
        .map_err(|_| String::from("assistant coordinator lock is poisoned"))?;
    let previous = coordinator.preferences().clone();
    coordinator
        .set_preferences(settings.into())
        .map_err(|_| String::from("assistant settings cannot change while a prompt is active"))?;
    if let Err(error) = persist_assistant_settings(settings) {
        let _ = coordinator.set_preferences(previous);
        return Err(error);
    }
    app_state
        .assistant_settings_generation
        .fetch_add(1, Ordering::SeqCst);
    drop(coordinator);
    if !settings.completion {
        clear_completion_state(&app_state)?;
    }
    invalidate_prefetch(&app_state)?;
    Ok(settings)
}

fn ensure_assistant_settings_available(
    settings: &AssistantSettingsPayload,
    startup_state: &Arc<Mutex<StartupStatePayload>>,
) -> Result<(), String> {
    let guard = startup_state
        .lock()
        .map_err(|_| String::from("startup state lock is poisoned"))?;
    let capabilities = match &*guard {
        StartupStatePayload::WarmingModel { capabilities, .. }
        | StartupStatePayload::Ready { capabilities, .. } => capabilities,
        StartupStatePayload::Error { .. } => {
            return Err(String::from("startup is not ready"));
        }
    };
    let mut required = vec![settings.instant.capability_id()];
    if settings.deep_enabled {
        required.push(settings.deep.capability_id());
        required.push("deep");
    }
    if settings.review_enabled {
        required.push(settings.review.capability_id());
        required.push("review");
    }
    if settings.completion {
        required.push("qwen_prediction");
    }
    for capability_id in required {
        let available = capabilities.iter().any(|capability| {
            capability.id == capability_id && capability.state == CapabilityStatePayload::Available
        });
        if !available {
            return Err(format!(
                "assistant capability `{capability_id}` is unavailable"
            ));
        }
    }
    Ok(())
}

#[tauri::command]
fn switch_response_profile(
    profile: ResponseProfilePayload,
    app_state: tauri::State<'_, AppState>,
) -> Result<SwitchResponseProfilePayload, String> {
    let _operation_guard =
        try_lock_response_backend_operation(&app_state.response_backend_operation_lock)?;
    let supported_response_profiles = app_state.supported_response_profiles.clone();
    if !supported_response_profiles.contains(&profile) {
        return Err(format!(
            "response profile `{}` is not supported",
            profile.as_str()
        ));
    }

    ensure_startup_ready_for_profile_switch(&app_state.startup_state)?;

    let response = SwitchResponseProfilePayload {
        selected_response_profile: profile,
        supported_response_profiles: supported_response_profiles.clone(),
    };

    let current_profile = *app_state
        .selected_response_profile
        .lock()
        .map_err(|_| String::from("selected response profile lock is poisoned"))?;
    if current_profile == profile {
        return Ok(response);
    }

    ensure_response_profile_switch_runtime_is_idle(&app_state.voice_pipeline_state)?;
    invalidate_prefetch(&app_state)?;
    {
        let coordinator = app_state
            .assistant_coordinator
            .lock()
            .map_err(|_| String::from("assistant coordinator lock is poisoned"))?;
        if coordinator.active().is_some() {
            return Err(String::from(
                "response profile cannot change while a prompt is active",
            ));
        }
    }

    let switch_generation = app_state
        .response_profile_switch_generation
        .fetch_add(1, Ordering::SeqCst)
        .saturating_add(1);

    if let Ok(mut conversation) = app_state.llama_cpp_conversation.lock() {
        conversation.clear();
    }

    let Some(config) = app_state.runtime_config.as_ref() else {
        return Ok(response);
    };

    let voxgolem_core::config::ResponseBackendConfig::LlamaCpp {
        server_path,
        host,
        port,
        fast_model_path,
        quality_model_path,
    } = &config.response_backend
    else {
        return Ok(response);
    };

    let model_path = model_path_for_profile(profile, fast_model_path, quality_model_path.as_ref())?
        .to_path_buf();
    let previous_model_path = model_path_for_profile(
        current_profile,
        fast_model_path,
        quality_model_path.as_ref(),
    )?
    .to_path_buf();
    let startup_snapshot = startup_snapshot_for_profile_switch(
        &app_state.startup_state,
        profile,
        supported_response_profiles,
    )?;

    let previous_runtime = app_state
        .llama_cpp_runtime
        .lock()
        .map_err(|_| String::from("local llama.cpp runtime lock is poisoned"))?
        .take();
    if let Some(mut runtime) = previous_runtime {
        runtime.shutdown_owned();
    }

    let startup_state = Arc::clone(&app_state.startup_state);
    let llama_cpp_runtime = Arc::clone(&app_state.llama_cpp_runtime);
    let selected_response_profile = Arc::clone(&app_state.selected_response_profile);
    let assistant_coordinator = Arc::clone(&app_state.assistant_coordinator);
    let assistant_settings_generation = Arc::clone(&app_state.assistant_settings_generation);
    let response_profile_switch_generation =
        Arc::clone(&app_state.response_profile_switch_generation);
    let inference_policy = config
        .llama_cpp
        .as_ref()
        .map(|llama| platform_inference_policy(llama.inference_provider))
        .unwrap_or(voxgolem_platform::inference::InferencePolicy::Auto);
    let server_spec = voxgolem_platform::llama_cpp::LlamaCppServerSpec::new(
        server_path.clone(),
        model_path,
        host.clone(),
        *port,
        LLAMA_CPP_MODEL_ALIAS,
    );
    let fallback_server_spec = voxgolem_platform::llama_cpp::LlamaCppServerSpec::new(
        server_path.clone(),
        previous_model_path,
        host.clone(),
        *port,
        LLAMA_CPP_MODEL_ALIAS,
    );

    if let Ok(mut startup_guard) = startup_state.lock() {
        *startup_guard = StartupStatePayload::WarmingModel {
            cue_asset_paths: startup_snapshot.cue_asset_paths.clone(),
            runtime_phase: RuntimePhasePayload::Initializing,
            voice_input_available: startup_snapshot.voice_input_available,
            voice_input_error: startup_snapshot.voice_input_error.clone(),
            silence_timeout_ms: startup_snapshot.silence_timeout_ms,
            message: String::from("Loading local Gemma model..."),
            selected_response_profile: profile,
            supported_response_profiles: startup_snapshot.supported_response_profiles.clone(),
            prompt_cancellation_available: false,
            tts_enabled: startup_snapshot.tts_enabled,
            tts_output_gain_db: startup_snapshot.tts_output_gain_db,
            capabilities: startup_snapshot.capabilities.clone(),
        };
    }
    let expected_assistant_settings_generation =
        assistant_settings_generation.load(Ordering::SeqCst);

    let llama_startups = Arc::clone(&app_state.llama_startups);
    let (startup_cancellation, startup_worker) =
        voxgolem_platform::llama_cpp::LlamaCppRuntime::start_with_policy_cancellable(
            server_spec,
            inference_policy,
        );
    let fallback_cancellation = startup_cancellation.clone();
    let startup_coordinator = std::thread::spawn(move || {
        let start_result = startup_worker.join().unwrap_or_else(|_| {
            Err(voxgolem_platform::llama_cpp::LlamaCppRuntimeError::StartupCancelled)
        });
        if response_profile_switch_generation.load(Ordering::SeqCst) != switch_generation {
            shutdown_llama_start_result(start_result);
            return;
        }

        let next_state = match start_result {
            Ok(runtime) => {
                let actual_provider = actual_inference_provider_name(runtime.actual_provider());
                if !store_llama_runtime_if_current(
                    runtime,
                    &llama_cpp_runtime,
                    &response_profile_switch_generation,
                    switch_generation,
                ) {
                    return;
                }

                if response_profile_switch_generation.load(Ordering::SeqCst) != switch_generation {
                    return;
                }

                match synchronize_local_instant_model(
                    &assistant_coordinator,
                    &assistant_settings_generation,
                    expected_assistant_settings_generation,
                    profile,
                ) {
                    Ok(Some(())) => {}
                    Ok(None) => {
                        if let Err(error) = persist_selected_response_profile(profile) {
                            eprintln!("failed to persist response profile state: {error}");
                        }
                    }
                    Err(error) => {
                        eprintln!("failed to synchronize response profile state: {error}");
                    }
                }

                if let Ok(mut selected) = selected_response_profile.lock() {
                    *selected = profile;
                }

                let mut ready_snapshot = startup_snapshot.clone();
                if let Some(capability) =
                    ready_snapshot.capabilities.iter_mut().find(|capability| {
                        capability.id
                            == if profile == ResponseProfilePayload::Quality {
                                "local_quality"
                            } else {
                                "local_fast"
                            }
                    })
                {
                    capability.actual_provider = Some(actual_provider);
                }
                startup_ready_state_from_snapshot(&ready_snapshot, profile)
            }
            Err(error) => {
                let restore_result = {
                    let worker = voxgolem_platform::llama_cpp::LlamaCppRuntime::start_with_policy_cancellation(fallback_server_spec, inference_policy, fallback_cancellation);
                    worker.join().unwrap_or_else(|_| {
                        Err(voxgolem_platform::llama_cpp::LlamaCppRuntimeError::StartupCancelled)
                    })
                };
                if response_profile_switch_generation.load(Ordering::SeqCst) != switch_generation {
                    shutdown_llama_start_result(restore_result);
                    return;
                }

                match restore_result {
                    Ok(runtime) => {
                        let actual_provider =
                            actual_inference_provider_name(runtime.actual_provider());
                        if !store_llama_runtime_if_current(
                            runtime,
                            &llama_cpp_runtime,
                            &response_profile_switch_generation,
                            switch_generation,
                        ) {
                            return;
                        }

                        if response_profile_switch_generation.load(Ordering::SeqCst)
                            != switch_generation
                        {
                            return;
                        }

                        if let Ok(mut selected) = selected_response_profile.lock() {
                            *selected = current_profile;
                        }
                        match synchronize_local_instant_model(
                            &assistant_coordinator,
                            &assistant_settings_generation,
                            expected_assistant_settings_generation,
                            current_profile,
                        ) {
                            Ok(Some(())) => {}
                            Ok(None) => {
                                if let Err(error) =
                                    persist_selected_response_profile(current_profile)
                                {
                                    eprintln!("failed to persist restored response profile state: {error}");
                                }
                            }
                            Err(error) => {
                                eprintln!("failed to synchronize restored response profile state: {error}");
                            }
                        }

                        let mut ready_snapshot = startup_snapshot.clone();
                        if let Some(capability) = ready_snapshot.capabilities.iter_mut().find(
                            |capability| {
                                capability.id
                                    == if current_profile == ResponseProfilePayload::Quality {
                                        "local_quality"
                                    } else {
                                        "local_fast"
                                    }
                            },
                        ) {
                            capability.actual_provider = Some(actual_provider);
                        }
                        startup_ready_state_from_snapshot(&ready_snapshot, current_profile)
                    }
                    Err(restore_error) => StartupStatePayload::Error {
                        message: format!(
                            "failed to initialize local llama.cpp runtime: {error}; failed to restore previous profile runtime: {restore_error}"
                        ),
                    },
                }
            }
        };

        if response_profile_switch_generation.load(Ordering::SeqCst) != switch_generation {
            return;
        }

        if let Ok(mut guard) = startup_state.lock() {
            *guard = next_state;
        }
    });
    register_llama_startup(&llama_startups, startup_cancellation, startup_coordinator);

    Ok(response)
}

fn submit_prompt_sync(
    prompt: String,
    app_state: tauri::State<'_, AppState>,
) -> Result<PromptExecutionPayload, String> {
    let _operation_guard =
        lock_response_backend_operation(&app_state.response_backend_operation_lock)?;
    ensure_startup_ready_for_prompt(&app_state.startup_state)?;

    apply_voice_pipeline_transition(
        &app_state.voice_pipeline_state,
        app_state.voice_pipeline_config,
        voxgolem_core::voice_pipeline::VoicePipelineEvent::SubmitPrompt,
    )?;

    let config = app_state
        .runtime_config
        .as_ref()
        .ok_or_else(|| String::from("startup config is not ready"))?;
    let prompt = validate_prompt_text(prompt)?;
    let outcome = match execute_prompt_backend(
        config,
        &prompt,
        &app_state.llama_cpp_runtime,
        &app_state.llama_cpp_conversation,
        app_state.llama_cpp_system_prompt.as_deref(),
    ) {
        Ok(outcome) => outcome,
        Err(error) => {
            apply_voice_pipeline_transition(
                &app_state.voice_pipeline_state,
                app_state.voice_pipeline_config,
                voxgolem_core::voice_pipeline::VoicePipelineEvent::PromptFailed {
                    message: error.clone(),
                },
            )?;

            return Err(error);
        }
    };

    let completion_event = match outcome.error_message.clone() {
        Some(message) => {
            voxgolem_core::voice_pipeline::VoicePipelineEvent::PromptFailed { message }
        }
        None => voxgolem_core::voice_pipeline::VoicePipelineEvent::PromptCompleted,
    };

    apply_voice_pipeline_transition(
        &app_state.voice_pipeline_state,
        app_state.voice_pipeline_config,
        completion_event,
    )?;

    let runtime_phase = current_runtime_phase(&app_state.voice_pipeline_state)?;

    Ok(PromptExecutionPayload {
        events: outcome.events,
        stderr: outcome.stderr,
        exit_code: outcome.exit_code,
        runtime_phase,
    })
}

#[derive(Clone, Debug, Serialize)]
struct PromptFinalPayload {
    request_id: String,
    runtime_phase: RuntimePhasePayload,
    outcome: String,
    error_message: Option<String>,
}

enum OpencodePromptResult {
    Completed(String),
    Cancelled,
    Failed(String),
}

struct AssistantRequestContext {
    generation: voxgolem_core::assistant::Generation,
    instant_model: voxgolem_core::assistant::InstantModel,
    preferences: voxgolem_core::assistant::AssistantPreferences,
    history: Vec<voxgolem_core::assistant::ConversationTurn>,
    started_ms: u64,
    source: CompletionSource,
    prompt: String,
}

#[derive(Clone, Copy)]
struct PromptCancellation<'a> {
    active_generation: u64,
    cancelled: &'a AtomicBool,
    signal: &'a tokio::sync::Notify,
}

fn begin_assistant_request(
    app_state: &AppState,
    prompt: &str,
    source: CompletionSource,
) -> Result<AssistantRequestContext, String> {
    let started_ms = current_time_ms()?;
    let mut coordinator = app_state
        .assistant_coordinator
        .lock()
        .map_err(|_| String::from("assistant coordinator lock is poisoned"))?;
    let history = coordinator.history().to_vec();
    let preferences = coordinator.preferences().clone();
    let instant_model = preferences.instant_model;
    let generation = coordinator
        .start(prompt.to_string())
        .map_err(|error| format!("assistant request could not start: {error:?}"))?;
    Ok(AssistantRequestContext {
        generation,
        instant_model,
        preferences,
        history,
        started_ms,
        source,
        prompt: prompt.to_string(),
    })
}

fn finish_assistant_request(
    app_state: &AppState,
    request_id: &str,
    active_generation: u64,
    generation: voxgolem_core::assistant::Generation,
    result: voxgolem_core::assistant::InstantOutcome,
) -> Result<voxgolem_core::assistant::AcceptResult, String> {
    accept_assistant_stage_if_active(
        app_state,
        request_id,
        active_generation,
        generation,
        voxgolem_core::assistant::Stage::Instant,
        voxgolem_core::assistant::StageResult::Instant(result),
    )
}

fn accept_assistant_stage_if_active(
    app_state: &AppState,
    request_id: &str,
    active_generation: u64,
    generation: voxgolem_core::assistant::Generation,
    stage: voxgolem_core::assistant::Stage,
    result: voxgolem_core::assistant::StageResult,
) -> Result<voxgolem_core::assistant::AcceptResult, String> {
    let active = app_state
        .active_prompt
        .lock()
        .map_err(|_| String::from("active prompt lock is poisoned"))?;
    if !active.as_ref().is_some_and(|active| {
        active.request_id == request_id
            && active.generation == active_generation
            && !active.cancelled.load(Ordering::SeqCst)
    }) {
        return Ok(voxgolem_core::assistant::AcceptResult::Cancelled);
    }
    Ok(app_state
        .assistant_coordinator
        .lock()
        .map_err(|_| String::from("assistant coordinator lock is poisoned"))?
        .accept(generation, stage, result))
}

fn commit_assistant_request_if_active(
    app_state: &AppState,
    request_id: &str,
    active_generation: u64,
    generation: voxgolem_core::assistant::Generation,
) -> Result<(), String> {
    let active = app_state
        .active_prompt
        .lock()
        .map_err(|_| String::from("active prompt lock is poisoned"))?;
    if !active.as_ref().is_some_and(|active| {
        active.request_id == request_id
            && active.generation == active_generation
            && !active.cancelled.load(Ordering::SeqCst)
    }) {
        return Err(String::from("assistant request is no longer active"));
    }
    app_state
        .assistant_coordinator
        .lock()
        .map_err(|_| String::from("assistant coordinator lock is poisoned"))?
        .commit(generation)
        .ok_or_else(|| String::from("assistant request did not resolve"))?;
    Ok(())
}

fn cancel_assistant_request(
    app_state: &AppState,
    generation: voxgolem_core::assistant::Generation,
) {
    if let Ok(mut coordinator) = app_state.assistant_coordinator.lock() {
        coordinator.cancel(generation);
    }
}

struct OpencodeStreamContext<'a> {
    app: &'a tauri::AppHandle,
    active_prompt: &'a Mutex<Option<ActivePrompt>>,
    request_id: &'a str,
    generation: u64,
    client: &'a voxgolem_platform::opencode::OpencodeClient,
    cancelled: &'a AtomicBool,
    cancellation_signal: &'a tokio::sync::Notify,
}

#[tauri::command]
async fn submit_prompt(
    request_id: String,
    prompt: String,
    source: Option<CompletionSource>,
    app: tauri::AppHandle,
    app_state: tauri::State<'_, AppState>,
) -> Result<PromptFinalPayload, String> {
    ensure_startup_ready_for_prompt(&app_state.startup_state)?;
    validate_prompt_request_id(&request_id)?;
    let prompt = validate_prompt_text(prompt)?;
    let assistant_request = begin_assistant_request(
        &app_state,
        &prompt,
        source.unwrap_or(CompletionSource::Typed),
    )?;
    let _assistant_request_guard = AssistantRequestGuard {
        coordinator: Arc::clone(&app_state.assistant_coordinator),
        generation: assistant_request.generation,
    };
    let is_opencode = matches!(
        assistant_request.instant_model,
        voxgolem_core::assistant::InstantModel::OpenCodeSolHigh
            | voxgolem_core::assistant::InstantModel::OpenCodeLunaLow
    );
    let opencode_client = {
        let server = app_state
            .opencode_server
            .lock()
            .map_err(|_| String::from("opencode server lock is poisoned"))?;
        server
            .as_ref()
            .map(voxgolem_platform::opencode::OpencodeServer::client)
    };
    if is_opencode && opencode_client.is_none() {
        cancel_assistant_request(&app_state, assistant_request.generation);
        return Err(String::from("OpenCode server is not available"));
    }
    let (generation, cancelled, cancellation_signal) = register_active_prompt(
        &app_state.active_prompt,
        &app_state.active_prompt_generation,
        &request_id,
        assistant_request.generation,
        if is_opencode {
            opencode_client.clone()
        } else {
            None
        },
    )?;
    let _active_prompt_guard = ActivePromptGuard {
        active_prompt: Arc::clone(&app_state.active_prompt),
        request_id: request_id.clone(),
        generation,
    };
    let prefetch_key = PrefetchKey {
        prompt: prompt.clone(),
        history: assistant_request.history.clone(),
        model: assistant_request.instant_model,
    };
    let prefetched_answer = take_and_invalidate_prefetch(
        &app_state.prefetch_cache,
        &app_state.prefetch_generation,
        &prefetch_key,
    )?;
    invalidate_prefetch(&app_state)?;
    if let Some(answer) = prefetched_answer {
        apply_voice_pipeline_transition(
            &app_state.voice_pipeline_state,
            app_state.voice_pipeline_config,
            voxgolem_core::voice_pipeline::VoicePipelineEvent::SubmitPrompt,
        )?;
        emit_prompt_event(
            &app,
            &request_id,
            PromptExecutionEventPayload::Text {
                text: answer.clone(),
            },
        )?;
        let instant_result = finish_assistant_request(
            &app_state,
            &request_id,
            generation,
            assistant_request.generation,
            voxgolem_core::assistant::InstantOutcome::Complete(
                voxgolem_core::assistant::Content::Text(answer.clone()),
            ),
        )?;
        if let Err(error) = resolve_enabled_agents(
            &app,
            &app_state,
            &request_id,
            &assistant_request,
            &answer,
            instant_result,
            PromptCancellation {
                active_generation: generation,
                cancelled: &cancelled,
                signal: &cancellation_signal,
            },
        )
        .await
        {
            return fail_started_prompt(
                &app,
                &app_state,
                &request_id,
                assistant_request.generation,
                error,
            );
        }
        if cancelled.load(Ordering::SeqCst) {
            return finish_cancelled_prompt(
                &app,
                &app_state,
                &request_id,
                assistant_request.generation,
            );
        }
        apply_voice_pipeline_transition(
            &app_state.voice_pipeline_state,
            app_state.voice_pipeline_config,
            voxgolem_core::voice_pipeline::VoicePipelineEvent::PromptCompleted,
        )?;
        let runtime_phase = current_runtime_phase(&app_state.voice_pipeline_state)?;
        commit_assistant_request_if_active(
            &app_state,
            &request_id,
            generation,
            assistant_request.generation,
        )?;
        let _ = emit_prompt_event(
            &app,
            &request_id,
            PromptExecutionEventPayload::Completed {
                runtime_phase: runtime_phase.clone(),
            },
        );
        return Ok(PromptFinalPayload {
            request_id,
            runtime_phase,
            outcome: String::from("completed"),
            error_message: None,
        });
    }
    if matches!(
        assistant_request.instant_model,
        voxgolem_core::assistant::InstantModel::CustomSolHigh
            | voxgolem_core::assistant::InstantModel::CustomLunaLow
    ) {
        return submit_custom_prompt(
            &request_id,
            &prompt,
            assistant_request,
            &app,
            &app_state,
            PromptCancellation {
                active_generation: generation,
                cancelled: &cancelled,
                signal: &cancellation_signal,
            },
        )
        .await;
    }
    if !is_opencode {
        let expected_profile = match assistant_request.instant_model {
            voxgolem_core::assistant::InstantModel::LocalFast => ResponseProfilePayload::Fast,
            voxgolem_core::assistant::InstantModel::LocalQuality => ResponseProfilePayload::Quality,
            _ => unreachable!("non-local providers return before local dispatch"),
        };
        if *app_state
            .selected_response_profile
            .lock()
            .map_err(|_| String::from("selected response profile lock is poisoned"))?
            != expected_profile
        {
            cancel_assistant_request(&app_state, assistant_request.generation);
            return Err(String::from("selected local model is not loaded"));
        }
        sync_llama_history(&app_state, &assistant_request.history)?;
        let blocking_app = app.clone();
        let blocking_prompt = prompt.clone();
        let result = match tauri::async_runtime::spawn_blocking(move || {
            let blocking_state = blocking_app.state::<AppState>();
            submit_prompt_sync(blocking_prompt, blocking_state)
        })
        .await
        {
            Ok(Ok(result)) => result,
            Ok(Err(error)) => {
                emit_prompt_event(
                    &app,
                    &request_id,
                    PromptExecutionEventPayload::Error {
                        message: error.clone(),
                    },
                )?;
                cancel_assistant_request(&app_state, assistant_request.generation);
                return Err(error);
            }
            Err(error) => {
                let error = format!("local response task failed: {error}");
                emit_prompt_event(
                    &app,
                    &request_id,
                    PromptExecutionEventPayload::Error {
                        message: error.clone(),
                    },
                )?;
                cancel_assistant_request(&app_state, assistant_request.generation);
                return Err(error);
            }
        };
        let answer = prompt_execution_text(&result.events);
        if cancelled.load(Ordering::SeqCst) {
            sync_llama_history(&app_state, &assistant_request.history)?;
            return finish_cancelled_prompt(
                &app,
                &app_state,
                &request_id,
                assistant_request.generation,
            );
        }
        if answer.trim().is_empty() {
            cancel_assistant_request(&app_state, assistant_request.generation);
            return Err(String::from(
                "response provider completed without visible text",
            ));
        }
        let phase = result.runtime_phase.clone();
        for event in &result.events {
            emit_prompt_event(&app, &request_id, event.clone())?;
        }
        let instant_result = finish_assistant_request(
            &app_state,
            &request_id,
            generation,
            assistant_request.generation,
            voxgolem_core::assistant::InstantOutcome::Complete(
                voxgolem_core::assistant::Content::Text(answer.clone()),
            ),
        )?;
        if let Err(error) = resolve_enabled_agents(
            &app,
            &app_state,
            &request_id,
            &assistant_request,
            &answer,
            instant_result,
            PromptCancellation {
                active_generation: generation,
                cancelled: &cancelled,
                signal: &cancellation_signal,
            },
        )
        .await
        {
            return fail_started_prompt(
                &app,
                &app_state,
                &request_id,
                assistant_request.generation,
                error,
            );
        }
        if cancelled.load(Ordering::SeqCst) {
            return finish_cancelled_prompt(
                &app,
                &app_state,
                &request_id,
                assistant_request.generation,
            );
        }
        commit_assistant_request_if_active(
            &app_state,
            &request_id,
            generation,
            assistant_request.generation,
        )?;
        let _ = emit_prompt_event(
            &app,
            &request_id,
            PromptExecutionEventPayload::Completed {
                runtime_phase: phase.clone(),
            },
        );
        return Ok(PromptFinalPayload {
            request_id,
            runtime_phase: phase,
            outcome: "completed".into(),
            error_message: None,
        });
    }
    let client = opencode_client.expect("OpenCode availability was checked before dispatch");
    if let Err(error) = apply_voice_pipeline_transition(
        &app_state.voice_pipeline_state,
        app_state.voice_pipeline_config,
        voxgolem_core::voice_pipeline::VoicePipelineEvent::SubmitPrompt,
    ) {
        cancel_assistant_request(&app_state, assistant_request.generation);
        return Err(error);
    }

    let result = stream_opencode_prompt(
        OpencodeStreamContext {
            app: &app,
            active_prompt: &app_state.active_prompt,
            request_id: &request_id,
            generation,
            client: &client,
            cancelled: &cancelled,
            cancellation_signal: &cancellation_signal,
        },
        &render_provider_prompt(&assistant_request.history, &prompt),
        assistant_request.instant_model,
    )
    .await;
    if cancelled.load(Ordering::SeqCst) {
        return finish_cancelled_prompt(
            &app,
            &app_state,
            &request_id,
            assistant_request.generation,
        );
    }

    let (outcome, event, error_message) = match result {
        OpencodePromptResult::Completed(answer) => {
            if answer.trim().is_empty() {
                cancel_assistant_request(&app_state, assistant_request.generation);
                return Err(String::from("OpenCode completed without visible text"));
            }
            let instant_result = finish_assistant_request(
                &app_state,
                &request_id,
                generation,
                assistant_request.generation,
                voxgolem_core::assistant::InstantOutcome::Complete(
                    voxgolem_core::assistant::Content::Text(answer.clone()),
                ),
            )?;
            if let Err(error) = resolve_enabled_agents(
                &app,
                &app_state,
                &request_id,
                &assistant_request,
                &answer,
                instant_result,
                PromptCancellation {
                    active_generation: generation,
                    cancelled: &cancelled,
                    signal: &cancellation_signal,
                },
            )
            .await
            {
                return fail_started_prompt(
                    &app,
                    &app_state,
                    &request_id,
                    assistant_request.generation,
                    error,
                );
            }
            if cancelled.load(Ordering::SeqCst) {
                return finish_cancelled_prompt(
                    &app,
                    &app_state,
                    &request_id,
                    assistant_request.generation,
                );
            }
            apply_voice_pipeline_transition(
                &app_state.voice_pipeline_state,
                app_state.voice_pipeline_config,
                voxgolem_core::voice_pipeline::VoicePipelineEvent::PromptCompleted,
            )?;
            let phase = current_runtime_phase(&app_state.voice_pipeline_state)?;
            commit_assistant_request_if_active(
                &app_state,
                &request_id,
                generation,
                assistant_request.generation,
            )?;
            (
                "completed",
                PromptExecutionEventPayload::Completed {
                    runtime_phase: phase,
                },
                None,
            )
        }
        OpencodePromptResult::Cancelled => {
            cancel_assistant_request(&app_state, assistant_request.generation);
            ensure_cancelled_prompt_is_sleeping(&app_state)?;
            let phase = current_runtime_phase(&app_state.voice_pipeline_state)?;
            (
                "cancelled",
                PromptExecutionEventPayload::Cancelled {
                    runtime_phase: phase,
                },
                None,
            )
        }
        OpencodePromptResult::Failed(message) => {
            cancel_assistant_request(&app_state, assistant_request.generation);
            apply_voice_pipeline_transition(
                &app_state.voice_pipeline_state,
                app_state.voice_pipeline_config,
                voxgolem_core::voice_pipeline::VoicePipelineEvent::PromptFailed {
                    message: message.clone(),
                },
            )?;
            (
                "error",
                PromptExecutionEventPayload::Error {
                    message: message.clone(),
                },
                Some(message),
            )
        }
    };
    let phase = current_runtime_phase(&app_state.voice_pipeline_state)?;
    if outcome == "completed" {
        let _ = emit_prompt_event(&app, &request_id, event);
    } else {
        emit_prompt_event(&app, &request_id, event)?;
    }
    Ok(PromptFinalPayload {
        request_id,
        runtime_phase: phase,
        outcome: outcome.to_string(),
        error_message,
    })
}

async fn submit_custom_prompt(
    request_id: &str,
    prompt: &str,
    assistant_request: AssistantRequestContext,
    app: &tauri::AppHandle,
    app_state: &AppState,
    cancellation: PromptCancellation<'_>,
) -> Result<PromptFinalPayload, String> {
    if let Err(error) = apply_voice_pipeline_transition(
        &app_state.voice_pipeline_state,
        app_state.voice_pipeline_config,
        voxgolem_core::voice_pipeline::VoicePipelineEvent::SubmitPrompt,
    ) {
        cancel_assistant_request(app_state, assistant_request.generation);
        return Err(error);
    }
    let config = app_state
        .runtime_config
        .as_ref()
        .and_then(|config| config.custom_openai.as_ref());
    let Some(config) = config else {
        return fail_started_prompt(
            app,
            app_state,
            request_id,
            assistant_request.generation,
            String::from("Custom provider is not configured"),
        );
    };
    let model = match assistant_request.instant_model {
        voxgolem_core::assistant::InstantModel::CustomSolHigh => {
            voxgolem_platform::custom_openai::CustomOpenAiModel::SolHigh
        }
        voxgolem_core::assistant::InstantModel::CustomLunaLow => {
            voxgolem_platform::custom_openai::CustomOpenAiModel::LunaLow
        }
        _ => unreachable!("custom dispatch requires a Custom model"),
    };
    let client = voxgolem_platform::custom_openai::CustomOpenAiClient::new(
        voxgolem_platform::custom_openai::CustomOpenAiConfig {
            endpoint: config.endpoint.clone(),
            auth_path: config.auth_path.clone(),
            model,
            ..Default::default()
        },
    );
    let client = match client {
        Ok(client) => client,
        Err(error) => {
            return fail_started_prompt(
                app,
                app_state,
                request_id,
                assistant_request.generation,
                error.to_string(),
            );
        }
    };
    let provider_prompt = voxgolem_platform::custom_openai::CustomOpenAiPrompt {
        session_id: request_id.to_string(),
        prompt: prompt.to_string(),
        history: custom_history(&assistant_request.history),
    };
    let response = tokio::select! {
        biased;
        _ = cancellation.signal.notified() => {
            return finish_cancelled_prompt(
                app,
                app_state,
                request_id,
                assistant_request.generation,
            );
        }
        response = client.respond(&provider_prompt, |text| {
            if !cancellation.cancelled.load(Ordering::SeqCst) {
                let _ = emit_prompt_event(
                    app,
                    request_id,
                    PromptExecutionEventPayload::Text {
                        text: text.to_string(),
                    },
                );
            }
        }) => response,
    };
    if cancellation.cancelled.load(Ordering::SeqCst) {
        return finish_cancelled_prompt(app, app_state, request_id, assistant_request.generation);
    }
    let response = match response {
        Ok(response) => response,
        Err(error) => {
            let message = error.to_string();
            cancel_assistant_request(app_state, assistant_request.generation);
            apply_voice_pipeline_transition(
                &app_state.voice_pipeline_state,
                app_state.voice_pipeline_config,
                voxgolem_core::voice_pipeline::VoicePipelineEvent::PromptFailed {
                    message: message.clone(),
                },
            )?;
            emit_prompt_event(
                app,
                request_id,
                PromptExecutionEventPayload::Error {
                    message: message.clone(),
                },
            )?;
            return Err(message);
        }
    };
    let answer = response.text;
    let instant_result = finish_assistant_request(
        app_state,
        request_id,
        cancellation.active_generation,
        assistant_request.generation,
        voxgolem_core::assistant::InstantOutcome::Complete(
            voxgolem_core::assistant::Content::Text(answer.clone()),
        ),
    )?;
    if let Err(error) = resolve_enabled_agents(
        app,
        app_state,
        request_id,
        &assistant_request,
        &answer,
        instant_result,
        cancellation,
    )
    .await
    {
        return fail_started_prompt(
            app,
            app_state,
            request_id,
            assistant_request.generation,
            error,
        );
    }
    if cancellation.cancelled.load(Ordering::SeqCst) {
        return finish_cancelled_prompt(app, app_state, request_id, assistant_request.generation);
    }
    apply_voice_pipeline_transition(
        &app_state.voice_pipeline_state,
        app_state.voice_pipeline_config,
        voxgolem_core::voice_pipeline::VoicePipelineEvent::PromptCompleted,
    )?;
    let runtime_phase = current_runtime_phase(&app_state.voice_pipeline_state)?;
    commit_assistant_request_if_active(
        app_state,
        request_id,
        cancellation.active_generation,
        assistant_request.generation,
    )?;
    let _ = emit_prompt_event(
        app,
        request_id,
        PromptExecutionEventPayload::Completed {
            runtime_phase: runtime_phase.clone(),
        },
    );
    Ok(PromptFinalPayload {
        request_id: request_id.to_string(),
        runtime_phase,
        outcome: String::from("completed"),
        error_message: None,
    })
}

fn fail_started_prompt<T>(
    app: &tauri::AppHandle,
    app_state: &AppState,
    request_id: &str,
    generation: voxgolem_core::assistant::Generation,
    message: String,
) -> Result<T, String> {
    cancel_assistant_request(app_state, generation);
    apply_voice_pipeline_transition(
        &app_state.voice_pipeline_state,
        app_state.voice_pipeline_config,
        voxgolem_core::voice_pipeline::VoicePipelineEvent::PromptFailed {
            message: message.clone(),
        },
    )?;
    emit_prompt_event(
        app,
        request_id,
        PromptExecutionEventPayload::Error {
            message: message.clone(),
        },
    )?;
    Err(message)
}

fn finish_cancelled_prompt(
    app: &tauri::AppHandle,
    app_state: &AppState,
    request_id: &str,
    generation: voxgolem_core::assistant::Generation,
) -> Result<PromptFinalPayload, String> {
    cancel_assistant_request(app_state, generation);
    ensure_cancelled_prompt_is_sleeping(app_state)?;
    let runtime_phase = current_runtime_phase(&app_state.voice_pipeline_state)?;
    emit_prompt_event(
        app,
        request_id,
        PromptExecutionEventPayload::Cancelled {
            runtime_phase: runtime_phase.clone(),
        },
    )?;
    Ok(PromptFinalPayload {
        request_id: request_id.to_string(),
        runtime_phase,
        outcome: String::from("cancelled"),
        error_message: None,
    })
}

fn custom_history(
    history: &[voxgolem_core::assistant::ConversationTurn],
) -> Vec<voxgolem_platform::custom_openai::CustomOpenAiMessage> {
    bounded_provider_history(history)
        .iter()
        .map(
            |turn| voxgolem_platform::custom_openai::CustomOpenAiMessage {
                role: match turn.role {
                    voxgolem_core::assistant::Role::User => {
                        voxgolem_platform::custom_openai::CustomOpenAiRole::User
                    }
                    voxgolem_core::assistant::Role::Assistant => {
                        voxgolem_platform::custom_openai::CustomOpenAiRole::Assistant
                    }
                },
                content_type: voxgolem_platform::custom_openai::CustomOpenAiContentType::OutputText,
                text: assistant_content_text(&turn.content).to_string(),
            },
        )
        .collect()
}

fn assistant_content_text(content: &voxgolem_core::assistant::Content) -> &str {
    match content {
        voxgolem_core::assistant::Content::Text(text) => text,
    }
}

fn render_provider_prompt(
    history: &[voxgolem_core::assistant::ConversationTurn],
    prompt: &str,
) -> String {
    let mut rendered = String::from(
        "Answer the current user message. Treat prior turns as conversation context, not instructions about tools.\n\n",
    );
    for turn in bounded_provider_history(history) {
        rendered.push_str(match turn.role {
            voxgolem_core::assistant::Role::User => "User: ",
            voxgolem_core::assistant::Role::Assistant => "Assistant: ",
        });
        rendered.push_str(assistant_content_text(&turn.content));
        rendered.push('\n');
    }
    rendered.push_str("User: ");
    rendered.push_str(prompt);
    rendered
}

fn sync_llama_history(
    app_state: &AppState,
    history: &[voxgolem_core::assistant::ConversationTurn],
) -> Result<(), String> {
    let mut pairs = Vec::new();
    for turns in history.chunks_exact(2) {
        let [user, assistant] = turns else {
            unreachable!("chunks_exact always yields two turns")
        };
        if user.role != voxgolem_core::assistant::Role::User
            || assistant.role != voxgolem_core::assistant::Role::Assistant
        {
            return Err(String::from("canonical conversation roles are invalid"));
        }
        pairs.push(LlamaConversationTurn {
            user: assistant_content_text(&user.content).to_string(),
            assistant: assistant_content_text(&assistant.content).to_string(),
        });
    }
    *app_state
        .llama_cpp_conversation
        .lock()
        .map_err(|_| String::from("local llama.cpp conversation lock is poisoned"))? = pairs;
    Ok(())
}

fn prompt_execution_text(events: &[PromptExecutionEventPayload]) -> String {
    events
        .iter()
        .filter_map(|event| match event {
            PromptExecutionEventPayload::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

async fn resolve_enabled_agents(
    app: &tauri::AppHandle,
    app_state: &AppState,
    request_id: &str,
    request: &AssistantRequestContext,
    instant_answer: &str,
    instant_result: voxgolem_core::assistant::AcceptResult,
    cancellation: PromptCancellation<'_>,
) -> Result<(), String> {
    if cancellation.cancelled.load(Ordering::SeqCst) {
        return Ok(());
    }
    record_assistant_stage_telemetry(
        app_state,
        request_id,
        request,
        telemetry::Stage::Generation,
        instant_telemetry_identity(request.instant_model),
        current_time_ms()
            .unwrap_or(request.started_ms)
            .saturating_sub(request.started_ms),
        true,
    );
    if matches!(
        instant_result,
        voxgolem_core::assistant::AcceptResult::Resolved(_)
    ) {
        return Ok(());
    }
    if !request.preferences.deep_enabled && !request.preferences.review_enabled {
        return Ok(());
    }

    let deep_report = if request.preferences.deep_enabled {
        emit_prompt_event(
            app,
            request_id,
            PromptExecutionEventPayload::Status {
                message: String::from("Deep running"),
            },
        )?;
        let deep_request = voxgolem_core::agent_pipeline::DeepRequest {
            original_request: request.prompt.clone(),
            canonical_history: agent_history(&request.history),
        };
        let is_opencode = matches!(
            request.preferences.deep_model,
            voxgolem_core::assistant::AgentModel::OpenCodeSolHigh
                | voxgolem_core::assistant::AgentModel::OpenCodeLunaLow
        );
        let deep_prompt = if is_opencode {
            voxgolem_core::agent_pipeline::opencode_deep_prompt(&deep_request)
        } else {
            voxgolem_core::agent_pipeline::custom_deep_prompt(&deep_request)
        };
        let started = Instant::now();
        let report = run_agent_text(
            app_state,
            request.preferences.deep_model,
            if is_opencode {
                voxgolem_platform::opencode::OpencodeToolPolicy::Research
            } else {
                voxgolem_platform::opencode::OpencodeToolPolicy::AnswerOnly
            },
            &format!("{request_id}-deep"),
            &deep_prompt,
            cancellation.cancelled,
            cancellation.signal,
        )
        .await;
        if cancellation.cancelled.load(Ordering::SeqCst) {
            return Ok(());
        }
        let report = report.and_then(|text| {
            parse_deep_agent_json(&text, started.elapsed().as_millis() as u64, is_opencode)
        });
        let deep_succeeded = report.is_ok();
        record_assistant_stage_telemetry(
            app_state,
            request_id,
            request,
            telemetry::Stage::Deep,
            agent_telemetry_identity(request.preferences.deep_model),
            started.elapsed().as_millis() as u64,
            deep_succeeded,
        );
        let report = match report {
            Ok(report) => {
                emit_prompt_event(
                    app,
                    request_id,
                    PromptExecutionEventPayload::Status {
                        message: String::from("Deep completed"),
                    },
                )?;
                report
            }
            Err(_) => {
                emit_prompt_event(
                    app,
                    request_id,
                    PromptExecutionEventPayload::Status {
                        message: if request.preferences.review_enabled {
                            String::from("Deep failed; Review will use Instant")
                        } else {
                            String::from("Deep failed; Instant retained")
                        },
                    },
                )?;
                voxgolem_core::agent_pipeline::DeepReport {
                    complete_answer: instant_answer.to_string(),
                    voice_summary: String::from("Instant answer retained."),
                    sources: Vec::new(),
                    timings: voxgolem_core::agent_pipeline::Timings {
                        elapsed_ms: started.elapsed().as_millis() as u64,
                    },
                    outcome: String::from("completed"),
                }
            }
        };
        let accepted = accept_assistant_stage_if_active(
            app_state,
            request_id,
            cancellation.active_generation,
            request.generation,
            voxgolem_core::assistant::Stage::Deep,
            voxgolem_core::assistant::StageResult::Deep(voxgolem_core::assistant::DeepReport {
                answer: voxgolem_core::assistant::Content::Text(report.complete_answer.clone()),
                findings: report
                    .sources
                    .iter()
                    .map(|source| source.title.clone())
                    .collect(),
            }),
        )?;
        if cancellation.cancelled.load(Ordering::SeqCst) {
            return Ok(());
        }
        if !request.preferences.review_enabled {
            if !matches!(
                accepted,
                voxgolem_core::assistant::AcceptResult::Resolved(_)
            ) {
                return Err(String::from("Deep did not resolve the assistant request"));
            }
            if deep_succeeded {
                emit_prompt_event(
                    app,
                    request_id,
                    PromptExecutionEventPayload::Correction {
                        text: report.complete_answer.clone(),
                        correction: format!("Correction: {}", report.voice_summary.trim()),
                    },
                )?;
            }
            return Ok(());
        }
        Some(report)
    } else {
        None
    };

    emit_prompt_event(
        app,
        request_id,
        PromptExecutionEventPayload::Status {
            message: String::from("Review running"),
        },
    )?;
    let review_prompt = voxgolem_core::agent_pipeline::review_prompt(
        &request.prompt,
        instant_answer,
        deep_report.as_ref(),
    );
    let review_started = Instant::now();
    let review = run_agent_text(
        app_state,
        request.preferences.review_model,
        if matches!(
            request.preferences.review_model,
            voxgolem_core::assistant::AgentModel::OpenCodeSolHigh
                | voxgolem_core::assistant::AgentModel::OpenCodeLunaLow
        ) {
            voxgolem_platform::opencode::OpencodeToolPolicy::Research
        } else {
            voxgolem_platform::opencode::OpencodeToolPolicy::AnswerOnly
        },
        &format!("{request_id}-review"),
        &review_prompt,
        cancellation.cancelled,
        cancellation.signal,
    )
    .await;
    if cancellation.cancelled.load(Ordering::SeqCst) {
        return Ok(());
    }
    let review = review.and_then(|text| parse_review_agent_json(&text));
    let review_succeeded = review.is_ok();
    record_assistant_stage_telemetry(
        app_state,
        request_id,
        request,
        telemetry::Stage::Review,
        agent_telemetry_identity(request.preferences.review_model),
        review_started.elapsed().as_millis() as u64,
        review_succeeded,
    );
    let review = match review {
        Ok(review) => review,
        Err(_) => {
            emit_prompt_event(
                app,
                request_id,
                PromptExecutionEventPayload::Status {
                    message: String::from("Review failed; Instant retained"),
                },
            )?;
            voxgolem_core::agent_pipeline::ReviewReport {
                decision: voxgolem_core::agent_pipeline::ReviewDecision::Keep,
            }
        }
    };
    let (decision, correction) = match review.decision {
        voxgolem_core::agent_pipeline::ReviewDecision::Keep => {
            (voxgolem_core::assistant::ReviewDecision::Keep, None)
        }
        voxgolem_core::agent_pipeline::ReviewDecision::Rewrite {
            replacement,
            correction,
        } => (
            voxgolem_core::assistant::ReviewDecision::Rewrite(
                voxgolem_core::assistant::Content::Text(replacement.clone()),
            ),
            Some((replacement, correction)),
        ),
    };
    let accepted = accept_assistant_stage_if_active(
        app_state,
        request_id,
        cancellation.active_generation,
        request.generation,
        voxgolem_core::assistant::Stage::Review,
        voxgolem_core::assistant::StageResult::Review(decision),
    )?;
    if cancellation.cancelled.load(Ordering::SeqCst) {
        return Ok(());
    }
    if !matches!(
        accepted,
        voxgolem_core::assistant::AcceptResult::Resolved(_)
    ) {
        return Err(String::from("Review did not resolve the assistant request"));
    }
    if let Some((text, correction)) = correction {
        emit_prompt_event(
            app,
            request_id,
            PromptExecutionEventPayload::Correction { text, correction },
        )?;
    } else if review_succeeded {
        emit_prompt_event(
            app,
            request_id,
            PromptExecutionEventPayload::Status {
                message: String::from("Review kept Instant answer"),
            },
        )?;
    }
    Ok(())
}

fn instant_telemetry_identity(
    model: voxgolem_core::assistant::InstantModel,
) -> (
    telemetry::Provider,
    &'static str,
    telemetry::InferenceProvider,
) {
    use voxgolem_core::assistant::InstantModel;
    match model {
        InstantModel::LocalFast => (
            telemetry::Provider::Local,
            "gemma-fast",
            telemetry::InferenceProvider::AttachedUnknown,
        ),
        InstantModel::LocalQuality => (
            telemetry::Provider::Local,
            "gemma-quality",
            telemetry::InferenceProvider::AttachedUnknown,
        ),
        InstantModel::CustomSolHigh => (
            telemetry::Provider::Custom,
            "gpt-5.6-sol",
            telemetry::InferenceProvider::Remote,
        ),
        InstantModel::CustomLunaLow => (
            telemetry::Provider::Custom,
            "gpt-5.6-luna",
            telemetry::InferenceProvider::Remote,
        ),
        InstantModel::OpenCodeSolHigh => (
            telemetry::Provider::OpenCode,
            "gpt-5.6-sol",
            telemetry::InferenceProvider::Remote,
        ),
        InstantModel::OpenCodeLunaLow => (
            telemetry::Provider::OpenCode,
            "gpt-5.6-luna",
            telemetry::InferenceProvider::Remote,
        ),
    }
}

fn agent_telemetry_identity(
    model: voxgolem_core::assistant::AgentModel,
) -> (
    telemetry::Provider,
    &'static str,
    telemetry::InferenceProvider,
) {
    use voxgolem_core::assistant::AgentModel;
    match model {
        AgentModel::CustomSolHigh => (
            telemetry::Provider::Custom,
            "gpt-5.6-sol",
            telemetry::InferenceProvider::Remote,
        ),
        AgentModel::CustomLunaLow => (
            telemetry::Provider::Custom,
            "gpt-5.6-luna",
            telemetry::InferenceProvider::Remote,
        ),
        AgentModel::OpenCodeSolHigh => (
            telemetry::Provider::OpenCode,
            "gpt-5.6-sol",
            telemetry::InferenceProvider::Remote,
        ),
        AgentModel::OpenCodeLunaLow => (
            telemetry::Provider::OpenCode,
            "gpt-5.6-luna",
            telemetry::InferenceProvider::Remote,
        ),
    }
}

fn record_assistant_stage_telemetry(
    app_state: &AppState,
    request_id: &str,
    request: &AssistantRequestContext,
    stage: telemetry::Stage,
    identity: (
        telemetry::Provider,
        &'static str,
        telemetry::InferenceProvider,
    ),
    duration_ms: u64,
    succeeded: bool,
) {
    let inference_provider = if identity.0 == telemetry::Provider::Local {
        app_state
            .llama_cpp_runtime
            .lock()
            .ok()
            .and_then(|runtime| {
                runtime
                    .as_ref()
                    .map(|runtime| telemetry_inference_provider(runtime.actual_provider()))
            })
            .unwrap_or(identity.2)
    } else {
        identity.2
    };
    append_telemetry(
        &app_state.telemetry_sink,
        telemetry::TelemetryMetadata {
            schema_version: telemetry::SCHEMA_VERSION,
            timestamp_ms: current_time_ms().unwrap_or_default(),
            request_id: request_id.to_string(),
            generation: request.generation.id,
            input_source: match request.source {
                CompletionSource::Typed => telemetry::InputSource::Text,
                CompletionSource::Voice => telemetry::InputSource::Voice,
            },
            provider: identity.0,
            model: identity.1.to_string(),
            stage,
            transport: telemetry::Transport::Http,
            inference_provider,
            speculative_origin: telemetry::SpeculativeOrigin::None,
            input_tokens: None,
            output_tokens: None,
            duration_ms: Some(duration_ms),
            status: if succeeded {
                telemetry::Status::Ok
            } else {
                telemetry::Status::Error
            },
            error_category: if succeeded {
                telemetry::ErrorCategory::None
            } else {
                telemetry::ErrorCategory::Internal
            },
        },
    );
}

fn parse_deep_agent_json(
    input: &str,
    elapsed_ms: u64,
    sources_allowed: bool,
) -> Result<voxgolem_core::agent_pipeline::DeepReport, String> {
    let wire = serde_json::from_str::<voxgolem_core::agent_pipeline::DeepWireReport>(input)
        .map_err(|_| String::from("invalid Deep JSON"))?;
    voxgolem_core::agent_pipeline::validate_deep_wire(wire, elapsed_ms, sources_allowed)
        .map_err(|error| error.to_string())
}

fn parse_review_agent_json(
    input: &str,
) -> Result<voxgolem_core::agent_pipeline::ReviewReport, String> {
    let wire = serde_json::from_str::<voxgolem_core::agent_pipeline::ReviewWireReport>(input)
        .map_err(|_| String::from("invalid Review JSON"))?;
    voxgolem_core::agent_pipeline::validate_review_wire(wire).map_err(|error| error.to_string())
}

fn agent_history(
    history: &[voxgolem_core::assistant::ConversationTurn],
) -> Vec<voxgolem_core::agent_pipeline::HistoryEntry> {
    bounded_provider_history(history)
        .iter()
        .map(|turn| voxgolem_core::agent_pipeline::HistoryEntry {
            role: match turn.role {
                voxgolem_core::assistant::Role::User => String::from("user"),
                voxgolem_core::assistant::Role::Assistant => String::from("assistant"),
            },
            content: assistant_content_text(&turn.content).to_string(),
        })
        .collect()
}

async fn run_agent_text(
    app_state: &AppState,
    model: voxgolem_core::assistant::AgentModel,
    tool_policy: voxgolem_platform::opencode::OpencodeToolPolicy,
    request_id: &str,
    prompt: &str,
    cancelled: &AtomicBool,
    cancellation_signal: &tokio::sync::Notify,
) -> Result<String, String> {
    match model {
        voxgolem_core::assistant::AgentModel::CustomSolHigh
        | voxgolem_core::assistant::AgentModel::CustomLunaLow => {
            let config = app_state
                .runtime_config
                .as_ref()
                .and_then(|config| config.custom_openai.as_ref())
                .ok_or_else(|| String::from("Custom provider is not configured"))?;
            let model = match model {
                voxgolem_core::assistant::AgentModel::CustomSolHigh => {
                    voxgolem_platform::custom_openai::CustomOpenAiModel::SolHigh
                }
                voxgolem_core::assistant::AgentModel::CustomLunaLow => {
                    voxgolem_platform::custom_openai::CustomOpenAiModel::LunaLow
                }
                _ => unreachable!(),
            };
            let client = voxgolem_platform::custom_openai::CustomOpenAiClient::new(
                voxgolem_platform::custom_openai::CustomOpenAiConfig {
                    endpoint: config.endpoint.clone(),
                    auth_path: config.auth_path.clone(),
                    model,
                    ..Default::default()
                },
            )
            .map_err(|error| error.to_string())?;
            let agent_prompt = voxgolem_platform::custom_openai::CustomOpenAiPrompt {
                session_id: request_id.to_string(),
                prompt: prompt.to_string(),
                history: Vec::new(),
            };
            tokio::select! {
                biased;
                _ = cancellation_signal.notified() => Err(String::from("assistant request cancelled")),
                result = client.respond(&agent_prompt, |_| {}) => {
                    result.map(|response| response.text).map_err(|error| error.to_string())
                },
            }
        }
        voxgolem_core::assistant::AgentModel::OpenCodeSolHigh
        | voxgolem_core::assistant::AgentModel::OpenCodeLunaLow => {
            rotate_opencode_session(app_state).await?;
            let client = app_state
                .opencode_server
                .lock()
                .map_err(|_| String::from("opencode server lock is poisoned"))?
                .as_ref()
                .map(voxgolem_platform::opencode::OpencodeServer::client)
                .ok_or_else(|| String::from("OpenCode server is not available"))?;
            let result = collect_opencode_agent(
                &client,
                request_id,
                prompt,
                match model {
                    voxgolem_core::assistant::AgentModel::OpenCodeSolHigh => {
                        voxgolem_platform::opencode::OpencodeModel::Gpt56SolHigh
                    }
                    voxgolem_core::assistant::AgentModel::OpenCodeLunaLow => {
                        voxgolem_platform::opencode::OpencodeModel::Gpt56LunaLow
                    }
                    _ => unreachable!(),
                },
                tool_policy,
                cancelled,
                cancellation_signal,
            )
            .await;
            let cleanup = rotate_opencode_session(app_state).await;
            match (result, cleanup) {
                (Ok(text), Ok(())) => Ok(text),
                (Err(error), Ok(())) | (Ok(_), Err(error)) => Err(error),
                (Err(error), Err(cleanup_error)) => Err(format!("{error}; {cleanup_error}")),
            }
        }
    }
}

async fn collect_opencode_agent(
    client: &voxgolem_platform::opencode::OpencodeClient,
    request_id: &str,
    prompt: &str,
    model: voxgolem_platform::opencode::OpencodeModel,
    tool_policy: voxgolem_platform::opencode::OpencodeToolPolicy,
    cancelled: &AtomicBool,
    cancellation_signal: &tokio::sync::Notify,
) -> Result<String, String> {
    let message_id = format!("agent-{request_id}");
    let prompt = voxgolem_platform::opencode::OpencodePrompt::new(prompt.to_string())
        .map_err(|error| format!("invalid agent prompt: {error:?}"))?
        .with_message_id(message_id.clone());
    let events = client
        .events_for_message(message_id)
        .await
        .map_err(|error| error.to_string())?;
    futures_util::pin_mut!(events);
    client
        .prompt_with_options(
            &prompt,
            voxgolem_platform::opencode::OpencodePromptOptions::new(model, tool_policy),
        )
        .await
        .map_err(|error| error.to_string())?;
    let mut output = String::new();
    loop {
        if cancelled.load(Ordering::SeqCst) {
            let _ = client.abort().await;
            return Err(String::from("assistant request cancelled"));
        }
        let event = tokio::select! {
            biased;
            _ = cancellation_signal.notified() => {
                let _ = client.abort().await;
                return Err(String::from("assistant request cancelled"));
            }
            event = tokio::time::timeout(OPENCODE_PROMPT_INACTIVITY_TIMEOUT, events.next()) => event,
        }
        .map_err(|_| String::from("OpenCode agent timed out"))?
        .ok_or_else(|| String::from("OpenCode agent stream closed"))?
        .map_err(|error| error.to_string())?;
        match event {
            voxgolem_platform::opencode::OpencodeEvent::Text(text) => output.push_str(&text),
            voxgolem_platform::opencode::OpencodeEvent::Error(message) => return Err(message),
            voxgolem_platform::opencode::OpencodeEvent::Completed => return Ok(output),
            voxgolem_platform::opencode::OpencodeEvent::Reasoning(_)
            | voxgolem_platform::opencode::OpencodeEvent::Status(_)
            | voxgolem_platform::opencode::OpencodeEvent::Tool { .. } => {}
        }
    }
}

fn emit_prompt_event(
    app: &tauri::AppHandle,
    request_id: &str,
    event: PromptExecutionEventPayload,
) -> Result<(), String> {
    app.emit(
        "prompt-execution-event",
        PromptEventEnvelope {
            request_id: request_id.to_string(),
            event,
        },
    )
    .map_err(|error| format!("failed to emit prompt event: {error}"))
}

fn register_active_prompt(
    active_prompt: &Mutex<Option<ActivePrompt>>,
    generation_counter: &AtomicU64,
    request_id: &str,
    assistant_generation: voxgolem_core::assistant::Generation,
    client: Option<voxgolem_platform::opencode::OpencodeClient>,
) -> Result<(u64, Arc<AtomicBool>, Arc<tokio::sync::Notify>), String> {
    let mut active = active_prompt
        .lock()
        .map_err(|_| String::from("active prompt lock is poisoned"))?;
    if active.is_some() {
        return Err(String::from("another prompt is already active"));
    }
    let generation = generation_counter
        .fetch_add(1, Ordering::SeqCst)
        .saturating_add(1);
    let cancelled = Arc::new(AtomicBool::new(false));
    let cancellation_signal = Arc::new(tokio::sync::Notify::new());
    let completion_signal = Arc::new(tokio::sync::Notify::new());
    *active = Some(ActivePrompt {
        request_id: request_id.to_string(),
        generation,
        assistant_generation,
        cancelled: Arc::clone(&cancelled),
        cancellation_signal: Arc::clone(&cancellation_signal),
        completion_signal,
        client,
    });
    Ok((generation, cancelled, cancellation_signal))
}

fn clear_active_prompt(
    active_prompt: &Mutex<Option<ActivePrompt>>,
    request_id: &str,
    generation: u64,
) -> Result<bool, String> {
    let mut active = active_prompt
        .lock()
        .map_err(|_| String::from("active prompt lock is poisoned"))?;
    if active
        .as_ref()
        .is_some_and(|active| active.request_id == request_id && active.generation == generation)
    {
        let completed = active.take().expect("active prompt should exist");
        completed.completion_signal.notify_one();
        return Ok(true);
    }
    Ok(false)
}

fn is_active_prompt(
    active_prompt: &Mutex<Option<ActivePrompt>>,
    request_id: &str,
    generation: u64,
) -> Result<bool, String> {
    active_prompt
        .lock()
        .map_err(|_| String::from("active prompt lock is poisoned"))
        .map(|active| {
            active.as_ref().is_some_and(|active| {
                active.request_id == request_id && active.generation == generation
            })
        })
}

async fn stream_opencode_prompt(
    context: OpencodeStreamContext<'_>,
    prompt: &str,
    model: voxgolem_core::assistant::InstantModel,
) -> OpencodePromptResult {
    let message_id = format!(
        "msg_voxgolem_{}_{}",
        context.generation,
        current_time_ms().unwrap_or(0)
    );
    let prompt = match voxgolem_platform::opencode::OpencodePrompt::new(prompt.to_string()) {
        Ok(prompt) => prompt.with_message_id(message_id.clone()),
        Err(error) => return OpencodePromptResult::Failed(format!("invalid prompt: {error:?}")),
    };
    let events = match context.client.events_for_message(message_id).await {
        Ok(events) => events,
        Err(error) => return OpencodePromptResult::Failed(error.to_string()),
    };
    futures_util::pin_mut!(events);
    let model = match model {
        voxgolem_core::assistant::InstantModel::OpenCodeSolHigh => {
            voxgolem_platform::opencode::OpencodeModel::Gpt56SolHigh
        }
        voxgolem_core::assistant::InstantModel::OpenCodeLunaLow => {
            voxgolem_platform::opencode::OpencodeModel::Gpt56LunaLow
        }
        _ => {
            return OpencodePromptResult::Failed(String::from("invalid OpenCode model selection"));
        }
    };
    if let Err(error) = context
        .client
        .prompt_with_options(
            &prompt,
            voxgolem_platform::opencode::OpencodePromptOptions::new(
                model,
                voxgolem_platform::opencode::OpencodeToolPolicy::AnswerOnly,
            ),
        )
        .await
    {
        return OpencodePromptResult::Failed(error.to_string());
    }

    let mut output = String::new();
    loop {
        let still_active = is_active_prompt(
            context.active_prompt,
            context.request_id,
            context.generation,
        )
        .unwrap_or(false);
        if context.cancelled.load(Ordering::SeqCst) || !still_active {
            return OpencodePromptResult::Cancelled;
        }
        let event = tokio::select! {
            _ = context.cancellation_signal.notified() => {
                return OpencodePromptResult::Cancelled;
            }
            event = tokio::time::timeout(OPENCODE_PROMPT_INACTIVITY_TIMEOUT, events.next()) => {
                match event {
                    Ok(Some(event)) => event,
                    Ok(None) => {
                        return OpencodePromptResult::Failed(String::from(
                            "OpenCode event stream closed before completion",
                        ));
                    }
                    Err(_) => {
                        let _ = context.client.abort().await;
                        return OpencodePromptResult::Failed(String::from(
                            "OpenCode prompt timed out waiting for activity",
                        ));
                    }
                }
            }
        };
        let event = match event {
            Ok(event) => event,
            Err(error) => return OpencodePromptResult::Failed(error.to_string()),
        };
        let payload = match event {
            voxgolem_platform::opencode::OpencodeEvent::Text(text) => {
                output.push_str(&text);
                PromptExecutionEventPayload::Text { text }
            }
            voxgolem_platform::opencode::OpencodeEvent::Reasoning(text) => {
                PromptExecutionEventPayload::Reasoning { text }
            }
            voxgolem_platform::opencode::OpencodeEvent::Status(message) => {
                PromptExecutionEventPayload::Status { message }
            }
            voxgolem_platform::opencode::OpencodeEvent::Tool {
                name,
                status,
                detail,
            } => PromptExecutionEventPayload::Tool {
                tool: name,
                status: format!("{status:?}").to_ascii_lowercase(),
                detail,
            },
            voxgolem_platform::opencode::OpencodeEvent::Error(message) => {
                return OpencodePromptResult::Failed(message);
            }
            voxgolem_platform::opencode::OpencodeEvent::Completed => {
                return OpencodePromptResult::Completed(output);
            }
        };
        if let Err(error) = emit_prompt_event(context.app, context.request_id, payload) {
            return OpencodePromptResult::Failed(error);
        }
    }
}

fn ensure_cancelled_prompt_is_sleeping(app_state: &AppState) -> Result<(), String> {
    if current_runtime_phase(&app_state.voice_pipeline_state)? != RuntimePhasePayload::Sleeping {
        reset_voice_pipeline_to_waiting(
            &app_state.voice_pipeline_state,
            &app_state.wake_word_runtime,
            &app_state.voice_activity_runtime,
            app_state.voice_pipeline_config,
        )?;
    }
    Ok(())
}

#[tauri::command]
async fn cancel_prompt(
    request_id: String,
    app_state: tauri::State<'_, AppState>,
) -> Result<(), String> {
    let client = {
        let active_guard = app_state
            .active_prompt
            .lock()
            .map_err(|_| String::from("active prompt lock is poisoned"))?;
        let active = active_guard
            .as_ref()
            .filter(|active| active.request_id == request_id)
            .ok_or_else(|| String::from("prompt request is no longer active"))?;
        let coordinator_cancelled = app_state
            .assistant_coordinator
            .lock()
            .map_err(|_| String::from("assistant coordinator lock is poisoned"))?
            .cancel(active.assistant_generation);
        if !coordinator_cancelled {
            return Err(String::from("prompt request is no longer active"));
        }
        active.cancelled.store(true, Ordering::SeqCst);
        active.cancellation_signal.notify_one();
        active.client.clone()
    };
    if let Some(client) = client {
        let _ = client.abort().await;
    }
    Ok(())
}

#[tauri::command]
fn record_speech_activity(
    now_ms: u64,
    app_state: tauri::State<'_, AppState>,
) -> Result<RuntimePhaseResponsePayload, String> {
    let _operation_guard =
        lock_response_backend_operation(&app_state.response_backend_operation_lock)?;
    ensure_startup_ready_for_prompt(&app_state.startup_state)?;
    apply_voice_pipeline_transition(
        &app_state.voice_pipeline_state,
        app_state.voice_pipeline_config,
        voxgolem_core::voice_pipeline::VoicePipelineEvent::SpeechDetected { now_ms },
    )?;

    Ok(RuntimePhaseResponsePayload {
        ..current_runtime_phase_response(&app_state.voice_pipeline_state, None, None)?
    })
}

#[tauri::command]
fn mark_silence(
    telemetry_frame_id: Option<String>,
    app_state: tauri::State<'_, AppState>,
) -> Result<RuntimePhaseResponsePayload, String> {
    let _operation_guard =
        lock_response_backend_operation(&app_state.response_backend_operation_lock)?;
    ensure_startup_ready_for_prompt(&app_state.startup_state)?;
    let now_ms = current_time_ms()?;

    let action = apply_voice_pipeline_transition(
        &app_state.voice_pipeline_state,
        app_state.voice_pipeline_config,
        voxgolem_core::voice_pipeline::VoicePipelineEvent::SilenceCheck { now_ms },
    )?;

    let should_measure_transcription = matches!(
        action,
        voxgolem_core::voice_pipeline::VoicePipelineAction::FinishedUtterance { .. }
    );
    if should_measure_transcription {
        app_state
            .partial_transcription
            .lock()
            .map_err(|_| String::from("partial transcription lock is poisoned"))?
            .finalize();
        clear_completion_state(&app_state)?;
    }
    let transcription_started_ms = if should_measure_transcription {
        Some(current_time_ms()?)
    } else {
        None
    };

    let transcript_text = match transcribe_finished_utterance(&action, &app_state.parakeet_runtime)
    {
        Ok(transcript_text) => transcript_text,
        Err(error) => {
            reset_voice_pipeline_to_waiting(
                &app_state.voice_pipeline_state,
                &app_state.wake_word_runtime,
                &app_state.voice_activity_runtime,
                app_state.voice_pipeline_config,
            )?;

            return Err(error);
        }
    };

    let transcription_completed_ms = if should_measure_transcription {
        Some(current_time_ms()?)
    } else {
        None
    };

    build_mark_silence_response(
        &app_state.voice_pipeline_state,
        &action,
        transcript_text,
        Some(RuntimeTelemetryPayload {
            frame_id: telemetry_frame_id,
            backend_ingest_started_ms: None,
            backend_ingest_completed_ms: None,
            wake_detected_ms: None,
            wake_confidence: None,
            transcription_started_ms,
            transcription_completed_ms,
        }),
    )
}

#[tauri::command]
async fn reset_session(
    app_state: tauri::State<'_, AppState>,
) -> Result<RuntimePhaseResponsePayload, String> {
    ensure_startup_ready_for_prompt(&app_state.startup_state)?;
    let active = app_state
        .active_prompt
        .lock()
        .map_err(|_| String::from("active prompt lock is poisoned"))?
        .clone();
    if let Some(active) = active {
        if let Ok(mut coordinator) = app_state.assistant_coordinator.lock() {
            coordinator.cancel(active.assistant_generation);
        }
        if let Some(client) = active.client.as_ref() {
            let _ = client.abort().await;
        }
        cancel_and_wait_for_prompt(
            &active.cancelled,
            &active.cancellation_signal,
            &active.completion_signal,
        )
        .await?;
    }
    invalidate_prefetch(&app_state)?;
    let has_opencode = app_state
        .opencode_server
        .lock()
        .map_err(|_| String::from("opencode server lock is poisoned"))?
        .is_some();
    if has_opencode {
        reset_opencode_session(&app_state).await?;
    }
    reset_llama_session(&app_state)?;
    app_state
        .assistant_coordinator
        .lock()
        .map_err(|_| String::from("assistant coordinator lock is poisoned"))?
        .reset();

    reset_runtime_session(&app_state)?;

    Ok(RuntimePhaseResponsePayload {
        ..current_runtime_phase_response(&app_state.voice_pipeline_state, None, None)?
    })
}

fn reset_runtime_session(app_state: &AppState) -> Result<(), String> {
    app_state
        .partial_transcription
        .lock()
        .map_err(|_| String::from("partial transcription lock is poisoned"))?
        .reset();
    clear_completion_state(app_state)?;
    if current_runtime_phase(&app_state.voice_pipeline_state)? == RuntimePhasePayload::Sleeping {
        reset_wake_word_runtime(&app_state.wake_word_runtime)?;
        reset_voice_activity_runtime(&app_state.voice_activity_runtime)?;
        return Ok(());
    }
    reset_voice_pipeline_to_waiting(
        &app_state.voice_pipeline_state,
        &app_state.wake_word_runtime,
        &app_state.voice_activity_runtime,
        app_state.voice_pipeline_config,
    )
}

fn reset_llama_session(app_state: &AppState) -> Result<(), String> {
    let _operation_guard =
        lock_response_backend_operation(&app_state.response_backend_operation_lock)?;
    app_state
        .llama_cpp_conversation
        .lock()
        .map_err(|_| String::from("local llama.cpp conversation lock is poisoned"))?
        .clear();
    Ok(())
}

async fn rotate_opencode_session(app_state: &AppState) -> Result<(), String> {
    let mut server = app_state
        .opencode_server
        .lock()
        .map_err(|_| String::from("opencode server lock is poisoned"))?
        .take()
        .ok_or_else(|| String::from("OpenCode server is not available"))?;
    let result = server
        .reset()
        .await
        .map_err(|error| format!("failed to rotate OpenCode session: {error}"));
    app_state
        .opencode_server
        .lock()
        .map_err(|_| String::from("opencode server lock is poisoned"))?
        .replace(server);
    result
}

async fn reset_opencode_session(app_state: &AppState) -> Result<(), String> {
    let mut server = app_state
        .opencode_server
        .lock()
        .map_err(|_| String::from("opencode server lock is poisoned"))?
        .take()
        .ok_or_else(|| String::from("OpenCode server is not available"))?;
    let active = app_state
        .active_prompt
        .lock()
        .map_err(|_| String::from("active prompt lock is poisoned"))?
        .clone();
    let cancellation_result = async {
        let Some(active) = active else {
            return Ok(());
        };

        cancel_and_wait_for_prompt(
            &active.cancelled,
            &active.cancellation_signal,
            &active.completion_signal,
        )
        .await
    }
    .await;
    let server_reset_result = server
        .reset()
        .await
        .map_err(|error| format!("failed to reset OpenCode session: {error}"));
    let reset_result = match (cancellation_result, server_reset_result) {
        (Ok(()), result) => result,
        (Err(cancellation_error), Ok(())) => Err(cancellation_error),
        (Err(cancellation_error), Err(reset_error)) => {
            Err(format!("{cancellation_error}; {reset_error}"))
        }
    };
    app_state
        .opencode_server
        .lock()
        .map_err(|_| String::from("opencode server lock is poisoned"))?
        .replace(server);
    reset_result
}

async fn cancel_and_wait_for_prompt(
    cancelled: &AtomicBool,
    cancellation_signal: &tokio::sync::Notify,
    completion_signal: &tokio::sync::Notify,
) -> Result<(), String> {
    cancelled.store(true, Ordering::SeqCst);
    cancellation_signal.notify_one();
    tokio::time::timeout(
        OPENCODE_PROMPT_CANCELLATION_TIMEOUT,
        completion_signal.notified(),
    )
    .await
    .map_err(|_| String::from("timed out waiting for the active prompt to stop"))?;
    Ok(())
}

#[tauri::command]
fn ingest_audio_frame(
    frame: Vec<f32>,
    telemetry_frame_id: Option<String>,
    app: tauri::AppHandle,
    app_state: tauri::State<'_, AppState>,
) -> Result<RuntimePhaseResponsePayload, String> {
    let maybe_operation_guard =
        try_lock_response_backend_operation_or_busy(&app_state.response_backend_operation_lock)?;
    if maybe_operation_guard.is_none() {
        return Ok(RuntimePhaseResponsePayload {
            ..current_runtime_phase_response(&app_state.voice_pipeline_state, None, None)?
        });
    }
    let _operation_guard = maybe_operation_guard;
    ensure_startup_ready_for_prompt(&app_state.startup_state)?;
    let backend_ingest_started_ms = current_time_ms()?;
    let mut guard = app_state
        .voice_pipeline_state
        .lock()
        .map_err(|_| String::from("voice pipeline lock is poisoned"))?;
    let now_ms = current_time_ms()?;
    let started_listening = matches!(
        guard.session().runtime().phase(),
        voxgolem_core::runtime::RuntimePhase::Listening
    );

    let wake_word_detection = if matches!(
        guard.session().runtime().phase(),
        voxgolem_core::runtime::RuntimePhase::Sleeping
    ) {
        process_wake_word_frame(&app_state.wake_word_runtime, &frame)?
    } else {
        None
    };
    let wake_word_now_ms = wake_word_event_timestamp(now_ms, wake_word_detection);
    let speech_detected = if started_listening {
        process_voice_activity_frame(&app_state.voice_activity_runtime, &frame)?
    } else {
        false
    };

    let mut next_state = ingest_audio_frame_with_optional_wake_word_detection(
        &guard,
        app_state.voice_pipeline_config,
        frame,
        wake_word_now_ms,
    )?;

    if wake_word_now_ms.is_some() {
        reset_voice_activity_runtime(&app_state.voice_activity_runtime)?;
    }

    next_state = apply_optional_speech_activity(
        next_state,
        app_state.voice_pipeline_config,
        speech_detected,
        now_ms,
    )?;

    *guard = next_state;

    let partial_action = if app_state.parakeet_runtime.is_none() {
        partial_transcription::PartialTranscriptionAction::Ignore
    } else if wake_word_now_ms.is_some() {
        let session_id = app_state
            .partial_voice_session
            .fetch_add(1, Ordering::SeqCst)
            + 1;
        app_state
            .partial_transcription
            .lock()
            .map_err(|_| String::from("partial transcription lock is poisoned"))?
            .start_session(session_id);
        partial_transcription::PartialTranscriptionAction::Ignore
    } else if started_listening {
        app_state
            .partial_transcription
            .lock()
            .map_err(|_| String::from("partial transcription lock is poisoned"))?
            .request_snapshot(now_ms, guard.capture().utterance_samples())
    } else {
        partial_transcription::PartialTranscriptionAction::Ignore
    };

    let backend_ingest_completed_ms = current_time_ms()?;
    let response = runtime_phase_response_from_state(
        &guard,
        None,
        None,
        Some(RuntimeTelemetryPayload {
            frame_id: telemetry_frame_id,
            backend_ingest_started_ms: Some(backend_ingest_started_ms),
            backend_ingest_completed_ms: Some(backend_ingest_completed_ms),
            wake_detected_ms: wake_word_now_ms,
            wake_confidence: wake_word_detection.map(|detection| detection.confidence),
            transcription_started_ms: None,
            transcription_completed_ms: None,
        }),
    );
    drop(guard);

    if let Some(parakeet_runtime) = app_state.parakeet_runtime.as_ref() {
        spawn_partial_transcription(
            app,
            partial_action,
            Arc::clone(parakeet_runtime),
            Arc::clone(&app_state.partial_transcription),
            app_state.voice_pipeline_config.sample_rate_hz(),
        );
    }

    Ok(response)
}

fn spawn_partial_transcription(
    app: tauri::AppHandle,
    action: partial_transcription::PartialTranscriptionAction,
    parakeet_runtime: Arc<Mutex<transcription::ParakeetRuntime>>,
    scheduler: Arc<Mutex<partial_transcription::PartialTranscriptionScheduler>>,
    sample_rate_hz: u32,
) {
    if !matches!(
        action,
        partial_transcription::PartialTranscriptionAction::StartSnapshot { .. }
    ) {
        return;
    }

    tauri::async_runtime::spawn(async move {
        let mut next_action = action;
        while let partial_transcription::PartialTranscriptionAction::StartSnapshot {
            session_id,
            revision,
            samples,
        } = next_action
        {
            let started = Instant::now();
            let runtime = Arc::clone(&parakeet_runtime);
            let result = tauri::async_runtime::spawn_blocking(move || {
                let input = voxgolem_model::parakeet::ParakeetTranscriptionInput::new(
                    sample_rate_hz,
                    samples,
                )
                .map_err(|_| String::from("invalid partial transcription input"))?;
                let transcript = runtime
                    .lock()
                    .map_err(|_| String::from("partial transcription runtime lock is poisoned"))?
                    .transcribe(&input)
                    .map_err(|_| String::from("partial transcription inference failed"))?;
                Ok::<String, String>(transcript.text().to_string())
            })
            .await
            .map_err(|_| String::from("partial transcription worker failed"))
            .and_then(|result| result);

            let succeeded = result.is_ok();
            let text = result.unwrap_or_default();
            let app_state = app.state::<AppState>();
            append_telemetry(
                &app_state.telemetry_sink,
                telemetry::TelemetryMetadata {
                    schema_version: telemetry::SCHEMA_VERSION,
                    timestamp_ms: current_time_ms().unwrap_or_default(),
                    request_id: format!("partial-{session_id}-{revision}"),
                    generation: revision,
                    input_source: telemetry::InputSource::Voice,
                    provider: telemetry::Provider::Local,
                    model: String::from("parakeet-v2-int8"),
                    stage: telemetry::Stage::PartialTranscription,
                    transport: telemetry::Transport::InProcess,
                    inference_provider: telemetry::InferenceProvider::Cpu,
                    speculative_origin: telemetry::SpeculativeOrigin::System,
                    input_tokens: None,
                    output_tokens: None,
                    duration_ms: Some(started.elapsed().as_millis() as u64),
                    status: if succeeded {
                        telemetry::Status::Ok
                    } else {
                        telemetry::Status::Error
                    },
                    error_category: if succeeded {
                        telemetry::ErrorCategory::None
                    } else {
                        telemetry::ErrorCategory::Internal
                    },
                },
            );
            next_action = match current_time_ms() {
                Ok(now_ms) => match scheduler.lock() {
                    Ok(mut scheduler) => scheduler.complete(now_ms, session_id, revision, text),
                    Err(_) => break,
                },
                Err(_) => break,
            };

            if let partial_transcription::PartialTranscriptionAction::PublishText {
                session_id,
                revision,
                text,
            } = &next_action
            {
                if !text.trim().is_empty() {
                    let _ = app.emit(
                        "partial-transcription-event",
                        PartialTranscriptionEventPayload {
                            session_id: *session_id,
                            revision: *revision,
                            text: text.clone(),
                        },
                    );
                    let _ = queue_completion_request(
                        CompletionSource::Voice,
                        *revision,
                        Some(*session_id),
                        text.clone(),
                        &app_state,
                    );
                }
                break;
            }
        }
    });
}

#[tauri::command]
fn request_completion(
    revision: u64,
    prompt: String,
    app_state: tauri::State<'_, AppState>,
) -> Result<(), String> {
    queue_completion_request(CompletionSource::Typed, revision, None, prompt, &app_state)
}

#[tauri::command]
fn clear_completion(app_state: tauri::State<'_, AppState>) -> Result<(), String> {
    let _lifecycle = app_state
        .completion_lifecycle_lock
        .lock()
        .map_err(|_| String::from("completion lifecycle lock is poisoned"))?;
    clear_completion_state_locked(&app_state, true)
}

fn clear_completion_state(app_state: &AppState) -> Result<(), String> {
    let _lifecycle = app_state
        .completion_lifecycle_lock
        .lock()
        .map_err(|_| String::from("completion lifecycle lock is poisoned"))?;
    clear_completion_state_locked(app_state, false)
}

fn clear_completion_state_locked(
    app_state: &AppState,
    require_runtime: bool,
) -> Result<(), String> {
    let request = app_state
        .completion_request
        .lock()
        .map_err(|_| String::from("completion request lock is poisoned"))?
        .clone();
    if require_runtime && request.is_none() {
        return Err(String::from("completion runtime is not available"));
    }
    if let Some(request) = request {
        request.clear();
    }
    app_state
        .completion_context
        .lock()
        .map_err(|_| String::from("completion context lock is poisoned"))?
        .take();
    invalidate_prefetch(app_state)?;
    Ok(())
}

fn queue_completion_request(
    source: CompletionSource,
    client_revision: u64,
    voice_session_id: Option<partial_transcription::VoiceSessionId>,
    prompt: String,
    app_state: &AppState,
) -> Result<(), String> {
    let _lifecycle = app_state
        .completion_lifecycle_lock
        .lock()
        .map_err(|_| String::from("completion lifecycle lock is poisoned"))?;
    invalidate_prefetch(app_state)?;
    if !assistant_completion_enabled(&app_state.assistant_coordinator)? {
        clear_completion_state_locked(app_state, false)?;
        return Ok(());
    }
    let request = app_state
        .completion_request
        .lock()
        .map_err(|_| String::from("completion request lock is poisoned"))?
        .clone()
        .ok_or_else(|| String::from("completion runtime is not available"))?;
    if prompt.trim().is_empty() || prompt.len() > COMPLETION_PROMPT_MAX_BYTES {
        request.clear();
        app_state
            .completion_context
            .lock()
            .map_err(|_| String::from("completion context lock is poisoned"))?
            .take();
        return Ok(());
    }
    let backend_revision = app_state
        .completion_generation
        .fetch_add(1, Ordering::SeqCst)
        .saturating_add(1);
    let started_ms = current_time_ms()?;
    *app_state
        .completion_context
        .lock()
        .map_err(|_| String::from("completion context lock is poisoned"))? =
        Some(CompletionRequestContext {
            backend_revision,
            client_revision,
            source,
            voice_session_id,
            prompt: prompt.clone(),
            started_ms,
        });
    request.request(backend_revision, prompt);
    Ok(())
}

fn assistant_completion_enabled(
    coordinator: &Mutex<voxgolem_core::assistant::AssistantCoordinator>,
) -> Result<bool, String> {
    coordinator
        .lock()
        .map(|coordinator| coordinator.preferences().completion_enabled)
        .map_err(|_| String::from("assistant coordinator lock is poisoned"))
}

async fn emit_completion_predictions(
    app: tauri::AppHandle,
    mut predictor: voxgolem_platform::completion::CompletionPredictor,
    context: Arc<Mutex<Option<CompletionRequestContext>>>,
) {
    while let Some(prediction) = predictor.next().await {
        let app_state = app.state::<AppState>();
        let Ok(_lifecycle) = app_state.completion_lifecycle_lock.lock() else {
            break;
        };
        if !assistant_completion_enabled(&app_state.assistant_coordinator).unwrap_or(false) {
            continue;
        }
        let matching = context.lock().ok().and_then(|mut current| {
            let matches = current.as_ref().is_some_and(|request| {
                request.backend_revision == prediction.revision
                    && request.prompt == prediction.prompt
            });
            matches.then(|| current.take()).flatten()
        });
        let Some(request) = matching else {
            continue;
        };
        append_telemetry(
            &app_state.telemetry_sink,
            telemetry::TelemetryMetadata {
                schema_version: telemetry::SCHEMA_VERSION,
                timestamp_ms: current_time_ms().unwrap_or_default(),
                request_id: format!("completion-{}", request.backend_revision),
                generation: request.backend_revision,
                input_source: match request.source {
                    CompletionSource::Typed => telemetry::InputSource::Text,
                    CompletionSource::Voice => telemetry::InputSource::Voice,
                },
                provider: telemetry::Provider::Local,
                model: String::from("qwen-completion"),
                stage: telemetry::Stage::CompletionPrediction,
                transport: telemetry::Transport::Http,
                inference_provider: app_state
                    .completion_runtime
                    .lock()
                    .ok()
                    .and_then(|runtime| {
                        runtime
                            .as_ref()
                            .map(|runtime| telemetry_inference_provider(runtime.actual_provider()))
                    })
                    .unwrap_or(telemetry::InferenceProvider::AttachedUnknown),
                speculative_origin: match request.source {
                    CompletionSource::Typed => telemetry::SpeculativeOrigin::User,
                    CompletionSource::Voice => telemetry::SpeculativeOrigin::System,
                },
                input_tokens: None,
                output_tokens: None,
                duration_ms: Some(
                    current_time_ms()
                        .unwrap_or(request.started_ms)
                        .saturating_sub(request.started_ms),
                ),
                status: telemetry::Status::Ok,
                error_category: telemetry::ErrorCategory::None,
            },
        );
        if let Some(suffix) = prediction
            .suffix
            .as_deref()
            .filter(|suffix| !suffix.is_empty())
        {
            let _ = queue_assistant_prefetch(
                &app,
                format!("{}{}", request.prompt, suffix),
                request.source,
            );
        }
        let _ = app.emit(
            "completion-event",
            CompletionEventPayload {
                source: request.source,
                revision: request.client_revision,
                voice_session_id: request.voice_session_id,
                suffix: prediction.suffix,
            },
        );
    }

    let app_state = app.state::<AppState>();
    let runtime = {
        let Ok(_lifecycle) = app_state.completion_lifecycle_lock.lock() else {
            return;
        };
        if let Ok(mut request) = app_state.completion_request.lock() {
            if let Some(request) = request.take() {
                request.clear();
            }
        }
        if let Ok(mut context) = app_state.completion_context.lock() {
            context.take();
        }
        if !app_state.exit_cleanup_started.load(Ordering::SeqCst) {
            fail_startup_capability(
                &app_state.startup_state,
                "qwen_prediction",
                String::from("completion prediction worker stopped unexpectedly"),
            );
        }
        app_state
            .completion_runtime
            .lock()
            .map(|mut runtime| runtime.take())
            .unwrap_or(None)
    };
    if let Some(mut runtime) = runtime {
        let _ = runtime.shutdown().await;
    }
}

fn invalidate_prefetch(app_state: &AppState) -> Result<(), String> {
    app_state.prefetch_generation.fetch_add(1, Ordering::SeqCst);
    let active = app_state
        .prefetch_task
        .lock()
        .map_err(|_| String::from("prefetch task lock is poisoned"))?;
    if let Some(task) = active.as_ref() {
        task.cancelled.store(true, Ordering::SeqCst);
        task.cancellation_signal.notify_one();
    }
    drop(active);
    app_state
        .prefetch_cache
        .lock()
        .map_err(|_| String::from("prefetch cache lock is poisoned"))?
        .take();
    Ok(())
}

fn take_and_invalidate_prefetch(
    cache: &Mutex<Option<PrefetchEntry>>,
    generation: &AtomicU64,
    key: &PrefetchKey,
) -> Result<Option<String>, String> {
    let mut cache = cache
        .lock()
        .map_err(|_| String::from("prefetch cache lock is poisoned"))?;
    let current_generation = generation.fetch_add(1, Ordering::SeqCst);
    Ok(cache
        .take()
        .filter(|entry| entry.generation == current_generation && entry.key == *key)
        .map(|entry| entry.answer))
}

fn queue_assistant_prefetch(
    app: &tauri::AppHandle,
    prompt: String,
    source: CompletionSource,
) -> Result<(), String> {
    let app_state = app.state::<AppState>();
    invalidate_prefetch(&app_state)?;
    if app_state
        .prefetch_task
        .lock()
        .map_err(|_| String::from("prefetch task lock is poisoned"))?
        .is_some()
    {
        return Ok(());
    }
    let key = {
        let coordinator = app_state
            .assistant_coordinator
            .lock()
            .map_err(|_| String::from("assistant coordinator lock is poisoned"))?;
        if coordinator.active().is_some() || !coordinator.preferences().prefetch_enabled {
            return Ok(());
        }
        PrefetchKey {
            prompt,
            history: coordinator.history().to_vec(),
            model: coordinator.preferences().instant_model,
        }
    };
    if !prefetch_supported_for_model(key.model) {
        return Ok(());
    }
    if let Some(expected_profile) = local_profile_for_model(key.model) {
        let selected_profile = *app_state
            .selected_response_profile
            .lock()
            .map_err(|_| String::from("selected response profile lock is poisoned"))?;
        if selected_profile != expected_profile {
            return Ok(());
        }
    }
    let generation = app_state
        .prefetch_generation
        .fetch_add(1, Ordering::SeqCst)
        .saturating_add(1);
    app_state
        .prefetch_cache
        .lock()
        .map_err(|_| String::from("prefetch cache lock is poisoned"))?
        .take();
    let app = app.clone();
    let cancelled = Arc::new(AtomicBool::new(false));
    let cancellation_signal = Arc::new(tokio::sync::Notify::new());
    app_state
        .prefetch_task
        .lock()
        .map_err(|_| String::from("prefetch task lock is poisoned"))?
        .replace(ActivePrefetch {
            generation,
            cancelled: Arc::clone(&cancelled),
            cancellation_signal: Arc::clone(&cancellation_signal),
            task: None,
        });
    let task_cancelled = Arc::clone(&cancelled);
    let task_cancellation_signal = Arc::clone(&cancellation_signal);
    let task = tauri::async_runtime::spawn(async move {
        let started = Instant::now();
        let result = run_assistant_prefetch(
            &app,
            &key,
            generation,
            &task_cancelled,
            &task_cancellation_signal,
        )
        .await;
        let app_state = app.state::<AppState>();
        let current = if let Ok(mut cache) = app_state.prefetch_cache.lock() {
            let current = app_state.prefetch_generation.load(Ordering::SeqCst) == generation
                && !task_cancelled.load(Ordering::SeqCst);
            if current {
                if let Ok(answer) = &result {
                    *cache = Some(PrefetchEntry {
                        generation,
                        key: key.clone(),
                        answer: answer.clone(),
                    });
                }
            }
            current
        } else {
            false
        };
        let (provider, model, inference_provider) = instant_telemetry_identity(key.model);
        append_telemetry(
            &app_state.telemetry_sink,
            telemetry::TelemetryMetadata {
                schema_version: telemetry::SCHEMA_VERSION,
                timestamp_ms: current_time_ms().unwrap_or_default(),
                request_id: format!("prefetch-{generation}"),
                generation,
                input_source: match source {
                    CompletionSource::Typed => telemetry::InputSource::Text,
                    CompletionSource::Voice => telemetry::InputSource::Voice,
                },
                provider,
                model: model.to_string(),
                stage: telemetry::Stage::Prefetch,
                transport: telemetry::Transport::Http,
                inference_provider,
                speculative_origin: match source {
                    CompletionSource::Typed => telemetry::SpeculativeOrigin::User,
                    CompletionSource::Voice => telemetry::SpeculativeOrigin::System,
                },
                input_tokens: None,
                output_tokens: None,
                duration_ms: Some(started.elapsed().as_millis() as u64),
                status: if result.is_ok() && current {
                    telemetry::Status::Ok
                } else {
                    telemetry::Status::Error
                },
                error_category: if result.is_ok() && current {
                    telemetry::ErrorCategory::None
                } else {
                    telemetry::ErrorCategory::Internal
                },
            },
        );
        if let Ok(mut active) = app_state.prefetch_task.lock() {
            if active
                .as_ref()
                .is_some_and(|active| active.generation == generation)
            {
                active.take();
            }
        };
    });
    let mut active = app_state
        .prefetch_task
        .lock()
        .map_err(|_| String::from("prefetch task lock is poisoned"))?;
    if let Some(active) = active
        .as_mut()
        .filter(|active| active.generation == generation)
    {
        active.task = Some(task);
    }
    Ok(())
}

fn local_profile_for_model(
    model: voxgolem_core::assistant::InstantModel,
) -> Option<ResponseProfilePayload> {
    use voxgolem_core::assistant::InstantModel;
    match model {
        InstantModel::LocalFast => Some(ResponseProfilePayload::Fast),
        InstantModel::LocalQuality => Some(ResponseProfilePayload::Quality),
        InstantModel::CustomSolHigh
        | InstantModel::CustomLunaLow
        | InstantModel::OpenCodeSolHigh
        | InstantModel::OpenCodeLunaLow => None,
    }
}

fn synchronize_local_instant_model(
    coordinator: &Mutex<voxgolem_core::assistant::AssistantCoordinator>,
    settings_generation: &AtomicU64,
    expected_generation: u64,
    profile: ResponseProfilePayload,
) -> Result<Option<()>, String> {
    synchronize_local_instant_model_with(
        coordinator,
        settings_generation,
        expected_generation,
        profile,
        persist_profile_and_assistant_settings,
    )
}

fn synchronize_local_instant_model_with<F>(
    coordinator: &Mutex<voxgolem_core::assistant::AssistantCoordinator>,
    settings_generation: &AtomicU64,
    expected_generation: u64,
    profile: ResponseProfilePayload,
    persist: F,
) -> Result<Option<()>, String>
where
    F: FnOnce(ResponseProfilePayload, AssistantSettingsPayload) -> Result<(), String>,
{
    let mut coordinator = coordinator
        .lock()
        .map_err(|_| String::from("assistant coordinator lock is poisoned"))?;
    if settings_generation.load(Ordering::SeqCst) != expected_generation {
        return Ok(None);
    }
    let mut preferences = coordinator.preferences().clone();
    preferences.instant_model = local_instant_model(profile);
    coordinator
        .set_preferences(preferences)
        .map_err(|_| String::from("assistant settings cannot change while a prompt is active"))?;
    let settings = AssistantSettingsPayload::from(coordinator.preferences());
    settings_generation.fetch_add(1, Ordering::SeqCst);
    persist(profile, settings)?;
    Ok(Some(()))
}

fn local_instant_model(profile: ResponseProfilePayload) -> voxgolem_core::assistant::InstantModel {
    match profile {
        ResponseProfilePayload::Fast => voxgolem_core::assistant::InstantModel::LocalFast,
        ResponseProfilePayload::Quality => voxgolem_core::assistant::InstantModel::LocalQuality,
    }
}

fn prefetch_supported_for_model(model: voxgolem_core::assistant::InstantModel) -> bool {
    local_profile_for_model(model).is_none()
}

async fn run_assistant_prefetch(
    app: &tauri::AppHandle,
    key: &PrefetchKey,
    generation: u64,
    cancelled: &AtomicBool,
    cancellation_signal: &tokio::sync::Notify,
) -> Result<String, String> {
    use voxgolem_core::assistant::InstantModel;
    match key.model {
        InstantModel::LocalFast | InstantModel::LocalQuality => {
            let app = app.clone();
            let key = key.clone();
            tauri::async_runtime::spawn_blocking(move || {
                let app_state = app.state::<AppState>();
                if app_state.prefetch_generation.load(Ordering::SeqCst) != generation {
                    return Err(String::from("prefetch cancelled"));
                }
                let _operation_guard =
                    lock_response_backend_operation(&app_state.response_backend_operation_lock)?;
                if app_state.prefetch_generation.load(Ordering::SeqCst) != generation {
                    return Err(String::from("prefetch cancelled"));
                }
                let expected_profile = local_profile_for_model(key.model)
                    .expect("local prefetch always has a local profile");
                if *app_state
                    .selected_response_profile
                    .lock()
                    .map_err(|_| String::from("selected response profile lock is poisoned"))?
                    != expected_profile
                {
                    return Err(String::from("selected local model is not loaded"));
                }
                let system_prompt = app_state
                    .llama_cpp_system_prompt
                    .as_deref()
                    .ok_or_else(|| String::from("SOUL.md is not loaded"))?;
                let conversation = assistant_history_as_llama(&key.history)?;
                let input = build_llama_prompt_input(system_prompt, &key.prompt, &conversation);
                let client = app_state
                    .llama_cpp_runtime
                    .lock()
                    .map_err(|_| String::from("local llama.cpp runtime lock is poisoned"))?
                    .as_ref()
                    .ok_or_else(|| String::from("local Gemma model is still warming up"))?
                    .client();
                client
                    .chat(
                        &voxgolem_platform::llama_cpp::LlamaCppPrompt::new(input.user_prompt)
                            .with_system_prompt(system_prompt)
                            .with_max_tokens(LLAMA_CPP_MAX_TOKENS),
                    )
                    .map(|response| response.text)
                    .map_err(|error| format!("failed to prefetch local response: {error}"))
            })
            .await
            .map_err(|error| format!("local prefetch task failed: {error}"))?
        }
        InstantModel::CustomSolHigh | InstantModel::CustomLunaLow => {
            let app_state = app.state::<AppState>();
            let config = app_state
                .runtime_config
                .as_ref()
                .and_then(|config| config.custom_openai.as_ref())
                .ok_or_else(|| String::from("Custom provider is not configured"))?;
            let model = if key.model == InstantModel::CustomSolHigh {
                voxgolem_platform::custom_openai::CustomOpenAiModel::SolHigh
            } else {
                voxgolem_platform::custom_openai::CustomOpenAiModel::LunaLow
            };
            let client = voxgolem_platform::custom_openai::CustomOpenAiClient::new(
                voxgolem_platform::custom_openai::CustomOpenAiConfig {
                    endpoint: config.endpoint.clone(),
                    auth_path: config.auth_path.clone(),
                    model,
                    ..Default::default()
                },
            )
            .map_err(|error| error.to_string())?;
            let prefetch_prompt = voxgolem_platform::custom_openai::CustomOpenAiPrompt {
                session_id: format!("prefetch-{generation}"),
                prompt: key.prompt.clone(),
                history: custom_history(&key.history),
            };
            tokio::select! {
                biased;
                _ = cancellation_signal.notified() => Err(String::from("prefetch cancelled")),
                result = client.respond(&prefetch_prompt, |_| {}) => {
                    result.map(|response| response.text).map_err(|error| error.to_string())
                },
            }
        }
        InstantModel::OpenCodeSolHigh | InstantModel::OpenCodeLunaLow => {
            let base_client = app
                .state::<AppState>()
                .opencode_server
                .lock()
                .map_err(|_| String::from("opencode server lock is poisoned"))?
                .as_ref()
                .map(voxgolem_platform::opencode::OpencodeServer::client)
                .ok_or_else(|| String::from("OpenCode server is not available"))?;
            let client = base_client
                .create_transient()
                .await
                .map_err(|error| error.to_string())?;
            let result = collect_opencode_agent(
                &client,
                &format!("prefetch-{generation}"),
                &render_provider_prompt(&key.history, &key.prompt),
                if key.model == InstantModel::OpenCodeSolHigh {
                    voxgolem_platform::opencode::OpencodeModel::Gpt56SolHigh
                } else {
                    voxgolem_platform::opencode::OpencodeModel::Gpt56LunaLow
                },
                voxgolem_platform::opencode::OpencodeToolPolicy::AnswerOnly,
                cancelled,
                cancellation_signal,
            )
            .await;
            let cleanup = client.delete().await.map_err(|error| error.to_string());
            match (result, cleanup) {
                (Ok(answer), Ok(())) => Ok(answer),
                (Err(error), Ok(())) | (Ok(_), Err(error)) => Err(error),
                (Err(error), Err(cleanup_error)) => Err(format!("{error}; {cleanup_error}")),
            }
        }
    }
}

fn assistant_history_as_llama(
    history: &[voxgolem_core::assistant::ConversationTurn],
) -> Result<Vec<LlamaConversationTurn>, String> {
    let mut conversation = Vec::new();
    for turns in history.chunks_exact(2) {
        let [user, assistant] = turns else {
            unreachable!("chunks_exact always yields two turns")
        };
        if user.role != voxgolem_core::assistant::Role::User
            || assistant.role != voxgolem_core::assistant::Role::Assistant
        {
            return Err(String::from("canonical conversation roles are invalid"));
        }
        conversation.push(LlamaConversationTurn {
            user: assistant_content_text(&user.content).to_string(),
            assistant: assistant_content_text(&assistant.content).to_string(),
        });
    }
    Ok(conversation)
}

#[derive(Clone)]
struct StartupSnapshot {
    cue_asset_paths: CueAssetPathsPayload,
    voice_input_available: bool,
    voice_input_error: Option<String>,
    silence_timeout_ms: u64,
    tts_enabled: bool,
    tts_output_gain_db: f32,
    supported_response_profiles: Vec<ResponseProfilePayload>,
    capabilities: Vec<CapabilityPayload>,
}

fn startup_ready_state_from_snapshot(
    startup_snapshot: &StartupSnapshot,
    selected_response_profile: ResponseProfilePayload,
) -> StartupStatePayload {
    StartupStatePayload::Ready {
        cue_asset_paths: startup_snapshot.cue_asset_paths.clone(),
        runtime_phase: RuntimePhasePayload::Sleeping,
        voice_input_available: startup_snapshot.voice_input_available,
        voice_input_error: startup_snapshot.voice_input_error.clone(),
        silence_timeout_ms: startup_snapshot.silence_timeout_ms,
        selected_response_profile,
        supported_response_profiles: startup_snapshot.supported_response_profiles.clone(),
        prompt_cancellation_available: true,
        tts_enabled: startup_snapshot.tts_enabled,
        tts_output_gain_db: startup_snapshot.tts_output_gain_db,
        capabilities: startup_snapshot.capabilities.clone(),
    }
}

fn startup_snapshot_for_profile_switch(
    startup_state: &Arc<Mutex<StartupStatePayload>>,
    profile: ResponseProfilePayload,
    supported_response_profiles: Vec<ResponseProfilePayload>,
) -> Result<StartupSnapshot, String> {
    let startup_state = startup_state
        .lock()
        .map_err(|_| String::from("startup state lock should not be poisoned"))?;

    match &*startup_state {
        StartupStatePayload::WarmingModel {
            cue_asset_paths,
            voice_input_available,
            voice_input_error,
            silence_timeout_ms,
            tts_enabled,
            tts_output_gain_db,
            capabilities,
            ..
        }
        | StartupStatePayload::Ready {
            cue_asset_paths,
            voice_input_available,
            voice_input_error,
            silence_timeout_ms,
            tts_enabled,
            tts_output_gain_db,
            capabilities,
            ..
        } => Ok(StartupSnapshot {
            cue_asset_paths: cue_asset_paths.clone(),
            voice_input_available: *voice_input_available,
            voice_input_error: voice_input_error.clone(),
            silence_timeout_ms: *silence_timeout_ms,
            tts_enabled: *tts_enabled,
            tts_output_gain_db: *tts_output_gain_db,
            supported_response_profiles,
            capabilities: capabilities.clone(),
        }),
        StartupStatePayload::Error { .. } => Err(format!(
            "cannot switch response profile `{}` while startup is in error",
            profile.as_str()
        )),
    }
}

fn supported_response_profiles(
    backend: &voxgolem_core::config::ResponseBackendConfig,
) -> Vec<ResponseProfilePayload> {
    let mut profiles = if matches!(
        backend,
        voxgolem_core::config::ResponseBackendConfig::Unconfigured
    ) {
        Vec::new()
    } else {
        vec![ResponseProfilePayload::Fast]
    };
    if let voxgolem_core::config::ResponseBackendConfig::LlamaCpp {
        quality_model_path: Some(_),
        ..
    } = backend
    {
        profiles.push(ResponseProfilePayload::Quality);
    }

    profiles
}

fn default_response_profile() -> ResponseProfilePayload {
    ResponseProfilePayload::Fast
}

fn default_ui_text_size() -> UiTextSizePayload {
    UiTextSizePayload::Medium
}

fn default_ui_theme() -> UiThemePayload {
    UiThemePayload::Dark
}

fn model_path_for_profile<'a>(
    profile: ResponseProfilePayload,
    fast_model_path: &'a Path,
    quality_model_path: Option<&'a PathBuf>,
) -> Result<&'a Path, String> {
    match profile {
        ResponseProfilePayload::Fast => Ok(fast_model_path),
        ResponseProfilePayload::Quality => quality_model_path
            .map(PathBuf::as_path)
            .ok_or_else(|| String::from("response profile `quality` is not supported")),
    }
}

fn resolve_selected_response_profile(
    supported_response_profiles: &[ResponseProfilePayload],
) -> ResponseProfilePayload {
    let default_profile = default_response_profile();
    let persisted_profile = load_selected_response_profile().unwrap_or_else(|error| {
        eprintln!("failed to read response profile state: {error}");
        None
    });

    let selected = persisted_profile
        .filter(|profile| supported_response_profiles.contains(profile))
        .unwrap_or(default_profile);

    if persisted_profile != Some(selected) {
        if let Err(error) = persist_selected_response_profile(selected) {
            eprintln!("failed to persist response profile state: {error}");
        }
    }

    selected
}

fn resolve_effective_tts_enabled(default_enabled: bool, tts_available: bool) -> bool {
    if !tts_available {
        return false;
    }
    let persisted_tts_enabled = load_persisted_tts_enabled().unwrap_or_else(|error| {
        eprintln!("failed to read tts state: {error}");
        None
    });

    persisted_tts_enabled.unwrap_or(default_enabled)
}

fn response_profile_state_path() -> Result<PathBuf, String> {
    let config_path = application_config_path()?;

    Ok(config_path.with_file_name(RESPONSE_PROFILE_STATE_FILE))
}

fn runtime_log_path() -> Result<PathBuf, String> {
    let config_path = application_config_path()?;

    Ok(config_path
        .with_file_name(RUNTIME_LOG_DIR)
        .join(RUNTIME_LOG_FILE))
}

fn application_config_path() -> Result<PathBuf, String> {
    #[cfg(test)]
    if let Some(appdata) = std::env::var_os("APPDATA") {
        return Ok(PathBuf::from(appdata).join("VoxGolem").join("config.toml"));
    }
    voxgolem_core::config::default_config_path()
        .map_err(|error| format!("failed to resolve application config path: {error}"))
}

fn append_runtime_log_line(enabled: bool, subsystem: &str, message: &str) -> Result<(), String> {
    if !enabled {
        return Ok(());
    }

    let log_path = runtime_log_path()?;
    if let Some(parent) = log_path.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            format!(
                "failed to create runtime log directory {}: {error}",
                parent.display()
            )
        })?;
    }

    let timestamp_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0);

    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .map_err(|error| format!("failed to open runtime log {}: {error}", log_path.display()))?;

    writeln!(
        file,
        "{timestamp_ms} [{}] {}",
        sanitize_log_subsystem(subsystem),
        sanitize_log_message(message)
    )
    .map_err(|error| {
        format!(
            "failed to append runtime log {}: {error}",
            log_path.display()
        )
    })
}

fn append_tts_runtime_log_line(enabled: bool, message: &str) -> Result<(), String> {
    append_runtime_log_line(enabled, "tts", message)
}

fn sanitize_log_subsystem(subsystem: &str) -> String {
    let sanitized: String = subsystem
        .chars()
        .filter(|character| {
            character.is_ascii_alphanumeric() || *character == '_' || *character == '-'
        })
        .take(32)
        .collect();

    if sanitized.is_empty() {
        return String::from("runtime");
    }

    sanitized
}

fn sanitize_log_message(message: &str) -> String {
    let mut characters = message.chars();
    let mut sanitized = String::new();

    for character in characters.by_ref().take(RUNTIME_LOG_MESSAGE_MAX_CHARS) {
        match character {
            '\r' => sanitized.push_str("\\r"),
            '\n' => sanitized.push_str("\\n"),
            _ => sanitized.push(character),
        }
    }

    if characters.next().is_some() {
        sanitized.push_str("…[truncated]");
    }

    sanitized
}

fn log_tts_runtime_event(enabled: bool, message: &str) {
    if let Err(error) = append_tts_runtime_log_line(enabled, message) {
        eprintln!("failed to append tts runtime log: {error}");
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct PersistedState {
    selected_response_profile: Option<ResponseProfilePayload>,
    tts_enabled: Option<bool>,
    ui_text_size: Option<UiTextSizePayload>,
    ui_theme: Option<UiThemePayload>,
    assistant_settings: Option<AssistantSettingsPayload>,
}

fn parse_persisted_state(contents: &str) -> Result<PersistedState, String> {
    let mut state = PersistedState::default();
    let mut assistant_settings = AssistantSettingsPayload::default();
    let mut assistant_settings_seen = false;

    for raw_line in contents.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with('[') {
            continue;
        }

        if let Some(value) = line.strip_prefix("selected_response_profile") {
            let Some(value) = value.trim_start().strip_prefix('=') else {
                return Err(String::from(
                    "invalid state.toml: expected `selected_response_profile = \"...\"`",
                ));
            };

            let value = value.trim().trim_matches('"').to_ascii_lowercase();
            state.selected_response_profile = match value.as_str() {
                "fast" => Some(ResponseProfilePayload::Fast),
                "quality" => Some(ResponseProfilePayload::Quality),
                _ => {
                    return Err(format!(
                        "invalid state.toml: unsupported selected_response_profile `{value}`"
                    ));
                }
            };
            continue;
        }

        if let Some(value) = line.strip_prefix("tts_enabled") {
            let Some(value) = value.trim_start().strip_prefix('=') else {
                return Err(String::from(
                    "invalid state.toml: expected `tts_enabled = true|false`",
                ));
            };

            let value = value.trim().trim_matches('"').to_ascii_lowercase();
            state.tts_enabled = match value.as_str() {
                "true" => Some(true),
                "false" => Some(false),
                _ => {
                    return Err(format!(
                        "invalid state.toml: unsupported tts_enabled `{value}`"
                    ));
                }
            };
            continue;
        }

        if let Some(value) = line.strip_prefix("ui_text_size") {
            let Some(value) = value.trim_start().strip_prefix('=') else {
                return Err(String::from(
                    "invalid state.toml: expected `ui_text_size = \"...\"`",
                ));
            };

            let value = value.trim().trim_matches('"').to_ascii_lowercase();
            state.ui_text_size = match value.as_str() {
                "small" => Some(UiTextSizePayload::Small),
                "medium" => Some(UiTextSizePayload::Medium),
                "large" => Some(UiTextSizePayload::Large),
                "extra_large" => Some(UiTextSizePayload::ExtraLarge),
                _ => {
                    return Err(format!(
                        "invalid state.toml: unsupported ui_text_size `{value}`"
                    ));
                }
            };
            continue;
        }

        if let Some(value) = line.strip_prefix("ui_theme") {
            let Some(value) = value.trim_start().strip_prefix('=') else {
                return Err(String::from(
                    "invalid state.toml: expected `ui_theme = \"...\"`",
                ));
            };

            let value = value.trim().trim_matches('"').to_ascii_lowercase();
            state.ui_theme = match value.as_str() {
                "light" => Some(UiThemePayload::Light),
                "dark" => Some(UiThemePayload::Dark),
                _ => {
                    return Err(format!(
                        "invalid state.toml: unsupported ui_theme `{value}`"
                    ));
                }
            };
            continue;
        }

        if let Some(value) = line.strip_prefix("assistant_instant") {
            assistant_settings.instant = parse_instant_choice(value)?;
            assistant_settings_seen = true;
            continue;
        }
        if let Some(value) = line.strip_prefix("assistant_deep_enabled") {
            assistant_settings.deep_enabled =
                parse_persisted_bool(value, "assistant_deep_enabled")?;
            assistant_settings_seen = true;
            continue;
        }
        if let Some(value) = line.strip_prefix("assistant_deep") {
            assistant_settings.deep = parse_agent_choice(value, "assistant_deep")?;
            assistant_settings_seen = true;
            continue;
        }
        if let Some(value) = line.strip_prefix("assistant_review_enabled") {
            assistant_settings.review_enabled =
                parse_persisted_bool(value, "assistant_review_enabled")?;
            assistant_settings_seen = true;
            continue;
        }
        if let Some(value) = line.strip_prefix("assistant_review") {
            assistant_settings.review = parse_agent_choice(value, "assistant_review")?;
            assistant_settings_seen = true;
            continue;
        }
        if let Some(value) = line.strip_prefix("assistant_prefetch") {
            assistant_settings.prefetch = parse_persisted_bool(value, "assistant_prefetch")?;
            assistant_settings_seen = true;
            continue;
        }
        if let Some(value) = line.strip_prefix("assistant_completion") {
            assistant_settings.completion = parse_persisted_bool(value, "assistant_completion")?;
            assistant_settings_seen = true;
        }
    }

    state.assistant_settings = assistant_settings_seen.then_some(assistant_settings);

    Ok(state)
}

fn persisted_value<'a>(value: &'a str, field: &str) -> Result<&'a str, String> {
    value
        .trim_start()
        .strip_prefix('=')
        .map(|value| value.trim().trim_matches('"'))
        .ok_or_else(|| format!("invalid state.toml: expected `{field} = ...`"))
}

fn parse_instant_choice(value: &str) -> Result<InstantChoicePayload, String> {
    let value = persisted_value(value, "assistant_instant")?;
    match value {
        "local-fast" => Ok(InstantChoicePayload::LocalFast),
        "local-quality" => Ok(InstantChoicePayload::LocalQuality),
        "custom-sol-high" => Ok(InstantChoicePayload::CustomSolHigh),
        "custom-luna-low" => Ok(InstantChoicePayload::CustomLunaLow),
        "opencode-sol-high" => Ok(InstantChoicePayload::OpenCodeSolHigh),
        "opencode-luna-low" => Ok(InstantChoicePayload::OpenCodeLunaLow),
        _ => Err(format!(
            "invalid state.toml: unsupported assistant_instant `{value}`"
        )),
    }
}

fn parse_agent_choice(value: &str, field: &str) -> Result<AgentChoicePayload, String> {
    let value = persisted_value(value, field)?;
    match value {
        "custom-sol-high" => Ok(AgentChoicePayload::CustomSolHigh),
        "custom-luna-low" => Ok(AgentChoicePayload::CustomLunaLow),
        "opencode-sol-high" => Ok(AgentChoicePayload::OpenCodeSolHigh),
        "opencode-luna-low" => Ok(AgentChoicePayload::OpenCodeLunaLow),
        _ => Err(format!("invalid state.toml: unsupported {field} `{value}`")),
    }
}

fn parse_persisted_bool(value: &str, field: &str) -> Result<bool, String> {
    match persisted_value(value, field)? {
        "true" => Ok(true),
        "false" => Ok(false),
        value => Err(format!("invalid state.toml: unsupported {field} `{value}`")),
    }
}

fn load_persisted_state() -> Result<PersistedState, String> {
    let state_path = response_profile_state_path()?;
    let contents = match fs::read_to_string(&state_path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(PersistedState::default()),
        Err(error) => {
            return Err(format!(
                "failed to read response profile state {}: {error}",
                state_path.display()
            ));
        }
    };

    parse_persisted_state(&contents)
}

fn persist_state(state: PersistedState) -> Result<(), String> {
    let state_path = response_profile_state_path()?;
    if let Some(parent) = state_path.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            format!(
                "failed to create response profile state directory {}: {error}",
                parent.display()
            )
        })?;
    }

    let mut lines = Vec::<String>::new();
    if let Some(profile) = state.selected_response_profile {
        lines.push(format!(
            "selected_response_profile = \"{}\"",
            profile.as_str()
        ));
    }
    if let Some(tts_enabled) = state.tts_enabled {
        lines.push(format!("tts_enabled = {tts_enabled}"));
    }
    if let Some(ui_text_size) = state.ui_text_size {
        lines.push(format!("ui_text_size = \"{}\"", ui_text_size.as_str()));
    }
    if let Some(ui_theme) = state.ui_theme {
        lines.push(format!("ui_theme = \"{}\"", ui_theme.as_str()));
    }
    if let Some(settings) = state.assistant_settings {
        lines.extend([
            format!("assistant_instant = \"{}\"", settings.instant.as_str()),
            format!("assistant_deep = \"{}\"", settings.deep.as_str()),
            format!("assistant_review = \"{}\"", settings.review.as_str()),
            format!("assistant_deep_enabled = {}", settings.deep_enabled),
            format!("assistant_review_enabled = {}", settings.review_enabled),
            format!("assistant_prefetch = {}", settings.prefetch),
            format!("assistant_completion = {}", settings.completion),
        ]);
    }

    let contents = if lines.is_empty() {
        String::new()
    } else {
        format!("{}\n", lines.join("\n"))
    };

    #[cfg(not(windows))]
    let result = {
        let temporary_path = state_path.with_extension(format!("tmp-{}", std::process::id()));
        let result = fs::write(&temporary_path, contents)
            .and_then(|_| fs::rename(&temporary_path, &state_path));
        if result.is_err() {
            let _ = fs::remove_file(&temporary_path);
        }
        result
    };
    #[cfg(windows)]
    let result = fs::write(&state_path, contents);
    if let Err(error) = result {
        return Err(format!(
            "failed to write response profile state {}: {error}",
            state_path.display()
        ));
    }
    Ok(())
}

fn load_selected_response_profile() -> Result<Option<ResponseProfilePayload>, String> {
    Ok(load_persisted_state()?.selected_response_profile)
}

fn persist_selected_response_profile(profile: ResponseProfilePayload) -> Result<(), String> {
    let _guard = PERSISTED_STATE_LOCK
        .lock()
        .map_err(|_| String::from("persisted state lock is poisoned"))?;
    let mut persisted = load_persisted_state()?;
    persisted.selected_response_profile = Some(profile);
    persist_state(persisted)
}

fn persist_profile_and_assistant_settings(
    profile: ResponseProfilePayload,
    settings: AssistantSettingsPayload,
) -> Result<(), String> {
    let _guard = PERSISTED_STATE_LOCK
        .lock()
        .map_err(|_| String::from("persisted state lock is poisoned"))?;
    let mut persisted = load_persisted_state()?;
    persisted.selected_response_profile = Some(profile);
    persisted.assistant_settings = Some(settings);
    persist_state(persisted)
}

fn load_persisted_tts_enabled() -> Result<Option<bool>, String> {
    Ok(load_persisted_state()?.tts_enabled)
}

fn persist_tts_enabled(enabled: bool) -> Result<(), String> {
    let _guard = PERSISTED_STATE_LOCK
        .lock()
        .map_err(|_| String::from("persisted state lock is poisoned"))?;
    let mut persisted = load_persisted_state()?;
    persisted.tts_enabled = Some(enabled);
    persist_state(persisted)
}

fn load_persisted_ui_text_size() -> Result<Option<UiTextSizePayload>, String> {
    Ok(load_persisted_state()?.ui_text_size)
}

fn persist_ui_text_size(text_size: UiTextSizePayload) -> Result<(), String> {
    let _guard = PERSISTED_STATE_LOCK
        .lock()
        .map_err(|_| String::from("persisted state lock is poisoned"))?;
    let mut persisted = load_persisted_state()?;
    persisted.ui_text_size = Some(text_size);
    persist_state(persisted)
}

fn load_persisted_ui_theme() -> Result<Option<UiThemePayload>, String> {
    Ok(load_persisted_state()?.ui_theme)
}

fn persist_ui_theme(theme: UiThemePayload) -> Result<(), String> {
    let _guard = PERSISTED_STATE_LOCK
        .lock()
        .map_err(|_| String::from("persisted state lock is poisoned"))?;
    let mut persisted = load_persisted_state()?;
    persisted.ui_theme = Some(theme);
    persist_state(persisted)
}

fn load_assistant_settings() -> Result<Option<AssistantSettingsPayload>, String> {
    Ok(load_persisted_state()?.assistant_settings)
}

fn persist_assistant_settings(settings: AssistantSettingsPayload) -> Result<(), String> {
    let _guard = PERSISTED_STATE_LOCK
        .lock()
        .map_err(|_| String::from("persisted state lock is poisoned"))?;
    let mut persisted = load_persisted_state()?;
    persisted.assistant_settings = Some(settings);
    persist_state(persisted)
}

fn resolve_assistant_settings(
    config: &voxgolem_core::config::RuntimeConfig,
    capabilities: &[CapabilityPayload],
    selected_profile: ResponseProfilePayload,
) -> AssistantSettingsPayload {
    let default_agent = if capability_available(capabilities, "opencode") {
        AgentChoicePayload::OpenCodeSolHigh
    } else {
        AgentChoicePayload::CustomSolHigh
    };
    let defaults = AssistantSettingsPayload {
        instant: match &config.response_backend {
            voxgolem_core::config::ResponseBackendConfig::LlamaCpp { .. } => match selected_profile
            {
                ResponseProfilePayload::Fast => InstantChoicePayload::LocalFast,
                ResponseProfilePayload::Quality => InstantChoicePayload::LocalQuality,
            },
            voxgolem_core::config::ResponseBackendConfig::Opencode { .. } => {
                InstantChoicePayload::OpenCodeSolHigh
            }
            voxgolem_core::config::ResponseBackendConfig::Unconfigured
                if capability_available(capabilities, "custom_provider") =>
            {
                InstantChoicePayload::CustomSolHigh
            }
            voxgolem_core::config::ResponseBackendConfig::Unconfigured => {
                InstantChoicePayload::LocalFast
            }
        },
        deep: default_agent,
        review: default_agent,
        ..AssistantSettingsPayload::default()
    };
    let persisted = load_assistant_settings().unwrap_or_else(|error| {
        eprintln!("failed to read assistant settings: {error}");
        None
    });
    let mut resolved = persisted.unwrap_or(defaults);
    if !capability_available(capabilities, resolved.instant.capability_id()) {
        resolved.instant = defaults.instant;
    }
    if !capability_available(capabilities, resolved.deep.capability_id()) {
        resolved.deep = defaults.deep;
        resolved.deep_enabled = false;
    }
    if !capability_available(capabilities, resolved.review.capability_id()) {
        resolved.review = defaults.review;
        resolved.review_enabled = false;
    }
    if matches!(
        resolved.instant,
        InstantChoicePayload::LocalFast | InstantChoicePayload::LocalQuality
    ) {
        resolved.instant = match selected_profile {
            ResponseProfilePayload::Fast => InstantChoicePayload::LocalFast,
            ResponseProfilePayload::Quality => InstantChoicePayload::LocalQuality,
        };
    }
    if !capabilities.iter().any(|capability| {
        capability.id == "qwen_prediction"
            && matches!(
                capability.state,
                CapabilityStatePayload::Available | CapabilityStatePayload::Warming
            )
    }) {
        resolved.completion = false;
    }
    if persisted != Some(resolved) {
        if let Err(error) = persist_assistant_settings(resolved) {
            eprintln!("failed to persist resolved assistant settings: {error}");
        }
    }
    resolved
}

fn capability_available(capabilities: &[CapabilityPayload], id: &str) -> bool {
    capabilities.iter().any(|capability| {
        capability.id == id && capability.state == CapabilityStatePayload::Available
    })
}

fn set_startup_tts_enabled(startup_state: &Arc<Mutex<StartupStatePayload>>, enabled: bool) {
    if let Ok(mut guard) = startup_state.lock() {
        match &mut *guard {
            StartupStatePayload::WarmingModel { tts_enabled, .. }
            | StartupStatePayload::Ready { tts_enabled, .. } => {
                *tts_enabled = enabled;
            }
            StartupStatePayload::Error { .. } => {}
        }
    }
}

fn fail_startup_capability(
    startup_state: &Arc<Mutex<StartupStatePayload>>,
    capability_id: &str,
    reason: String,
) {
    if let Ok(mut guard) = startup_state.lock() {
        let capabilities = match &mut *guard {
            StartupStatePayload::WarmingModel { capabilities, .. }
            | StartupStatePayload::Ready { capabilities, .. } => capabilities,
            StartupStatePayload::Error { .. } => return,
        };
        if let Some(capability) = capabilities
            .iter_mut()
            .find(|item| item.id == capability_id)
        {
            capability.state = CapabilityStatePayload::Failed;
            capability.reason = reason;
            capability.actual_provider = None;
        }
    }
}

fn set_startup_capability_provider(
    startup_state: &Arc<Mutex<StartupStatePayload>>,
    capability_id: &str,
    actual_provider: &'static str,
) {
    if let Ok(mut guard) = startup_state.lock() {
        let capabilities = match &mut *guard {
            StartupStatePayload::WarmingModel { capabilities, .. }
            | StartupStatePayload::Ready { capabilities, .. } => capabilities,
            StartupStatePayload::Error { .. } => return,
        };
        if let Some(capability) = capabilities
            .iter_mut()
            .find(|capability| capability.id == capability_id)
        {
            capability.state = CapabilityStatePayload::Available;
            capability.reason = String::from("ready");
            capability.actual_provider = Some(actual_provider);
        }
    }
}

fn repair_tts_capability(
    capabilities: &mut [CapabilityPayload],
    enabled: bool,
    runtime: Option<&tts::LocalTtsRuntime>,
) {
    if let Some(capability) = capabilities.iter_mut().find(|item| item.id == "tts") {
        capability.actual_provider = if enabled {
            runtime
                .and_then(|runtime| runtime.actual_provider())
                .map(|provider| match provider {
                    tts::TtsActualProvider::Cuda => "cuda",
                    tts::TtsActualProvider::Cpu => "cpu",
                })
        } else {
            None
        };
        if enabled && runtime.is_some() {
            capability.state = CapabilityStatePayload::Available;
            capability.reason = String::from("ready");
        }
    }
}

const CAPABILITY_IDS: [&str; 11] = [
    "custom_provider",
    "opencode",
    "local_fast",
    "local_quality",
    "qwen_prediction",
    "wake_word",
    "vad",
    "parakeet",
    "tts",
    "deep",
    "review",
];

fn configured_capabilities(
    config: &voxgolem_core::config::RuntimeConfig,
) -> Vec<CapabilityPayload> {
    CAPABILITY_IDS
        .into_iter()
        .map(|id| {
            let issue = config.capability_issues.iter().find(|issue| {
                issue.capability == id
                    || (issue.capability == "response_provider"
                        && matches!(
                            id,
                            "custom_provider" | "opencode" | "local_fast" | "local_quality"
                        ))
            });
            let opencode_configured = config
                .opencode
                .as_ref()
                .is_some_and(|opencode| opencode.path.is_file());
            let custom_configured = config
                .custom_openai
                .as_ref()
                .is_some_and(|custom| custom.auth_path.is_file())
                && !config
                    .capability_issues
                    .iter()
                    .any(|issue| issue.capability == "custom_provider");
            let configured = match id {
                "custom_provider" => custom_configured,
                "opencode" => opencode_configured,
                "local_fast" => config.llama_cpp.as_ref().is_some_and(|llama| {
                    llama.server_path.is_file() && llama.fast_model_path.is_file()
                }),
                "local_quality" => config.llama_cpp.as_ref().is_some_and(|llama| {
                    llama.server_path.is_file()
                        && llama
                            .quality_model_path
                            .as_ref()
                            .is_some_and(|path| path.is_file())
                }),
                "qwen_prediction" => config.completion.as_ref().is_some_and(|completion| {
                    completion.server_path.is_file() && completion.model_path.is_file()
                }),
                "wake_word" => config.wake_word_model_path.is_file(),
                "vad" => config.silero_vad_model.is_file(),
                "parakeet" => config.parakeet_model_dir.is_dir(),
                "tts" => config.local_tts.model_path.is_file(),
                "deep" | "review" => custom_configured || opencode_configured,
                _ => false,
            };
            let state = if id == "qwen_prediction" && configured {
                CapabilityStatePayload::Warming
            } else if configured {
                CapabilityStatePayload::Available
            } else if issue.is_some_and(|issue| issue.reason != "not configured") {
                CapabilityStatePayload::Unavailable
            } else {
                CapabilityStatePayload::NotConfigured
            };
            CapabilityPayload {
                id,
                state,
                reason: if id == "qwen_prediction" && configured {
                    String::from("completion model is loading")
                } else if configured {
                    String::from("ready")
                } else {
                    issue
                        .map(|issue| issue.reason.clone())
                        .unwrap_or_else(|| String::from("not configured"))
                },
                actual_provider: if configured && matches!(id, "wake_word" | "vad" | "parakeet") {
                    Some("cpu")
                } else {
                    None
                },
            }
        })
        .collect()
}

fn failed_capabilities(reason: String) -> Vec<CapabilityPayload> {
    CAPABILITY_IDS
        .into_iter()
        .map(|id| CapabilityPayload {
            id,
            state: CapabilityStatePayload::Failed,
            reason: reason.clone(),
            actual_provider: None,
        })
        .collect()
}

fn mark_capability(
    capabilities: &mut [CapabilityPayload],
    id: &str,
    state: CapabilityStatePayload,
    reason: String,
) {
    if let Some(capability) = capabilities.iter_mut().find(|item| item.id == id) {
        capability.state = state;
        capability.reason = reason;
        capability.actual_provider = None;
    }
}

fn new_partial_transcription_scheduler(
) -> Arc<Mutex<partial_transcription::PartialTranscriptionScheduler>> {
    Arc::new(Mutex::new(
        partial_transcription::PartialTranscriptionScheduler::new(
            partial_transcription::PartialTranscriptionConfig {
                minimum_samples: PARTIAL_TRANSCRIPTION_MINIMUM_SAMPLES,
                throttle: PARTIAL_TRANSCRIPTION_THROTTLE,
                maximum_copied_samples: PARTIAL_TRANSCRIPTION_MAXIMUM_SAMPLES,
            },
        ),
    ))
}

fn platform_inference_policy(
    policy: voxgolem_core::config::InferencePolicy,
) -> voxgolem_platform::inference::InferencePolicy {
    match policy {
        voxgolem_core::config::InferencePolicy::Auto => {
            voxgolem_platform::inference::InferencePolicy::Auto
        }
        voxgolem_core::config::InferencePolicy::Cuda => {
            voxgolem_platform::inference::InferencePolicy::Cuda
        }
        voxgolem_core::config::InferencePolicy::Cpu => {
            voxgolem_platform::inference::InferencePolicy::Cpu
        }
    }
}

fn actual_inference_provider_name(
    provider: voxgolem_platform::inference::ActualInferenceProvider,
) -> &'static str {
    match provider {
        voxgolem_platform::inference::ActualInferenceProvider::Cuda => "cuda",
        voxgolem_platform::inference::ActualInferenceProvider::Cpu => "cpu",
        voxgolem_platform::inference::ActualInferenceProvider::AttachedUnknown => {
            "attached_unknown"
        }
        voxgolem_platform::inference::ActualInferenceProvider::RequestedCuda => "requested_cuda",
    }
}

fn telemetry_inference_provider(
    provider: voxgolem_platform::inference::ActualInferenceProvider,
) -> telemetry::InferenceProvider {
    match provider {
        voxgolem_platform::inference::ActualInferenceProvider::Cuda => {
            telemetry::InferenceProvider::Cuda
        }
        voxgolem_platform::inference::ActualInferenceProvider::Cpu => {
            telemetry::InferenceProvider::Cpu
        }
        voxgolem_platform::inference::ActualInferenceProvider::AttachedUnknown => {
            telemetry::InferenceProvider::AttachedUnknown
        }
        voxgolem_platform::inference::ActualInferenceProvider::RequestedCuda => {
            telemetry::InferenceProvider::AttachedUnknown
        }
    }
}

fn new_telemetry_sink(
    config: voxgolem_core::config::TelemetryConfig,
) -> Option<Arc<Mutex<telemetry::TelemetrySink>>> {
    let state_dir = voxgolem_core::config::default_state_dir().ok()?;
    Some(Arc::new(Mutex::new(telemetry::TelemetrySink::new(
        state_dir,
        telemetry::TelemetryConfig {
            enabled: config.enabled,
            max_bytes: config.max_bytes as u64,
            backup_count: usize::from(config.backup_count),
        },
    ))))
}

fn default_telemetry_sink() -> Option<Arc<Mutex<telemetry::TelemetrySink>>> {
    new_telemetry_sink(voxgolem_core::config::TelemetryConfig {
        enabled: true,
        max_bytes: DEFAULT_TELEMETRY_MAX_BYTES,
        backup_count: DEFAULT_TELEMETRY_BACKUP_COUNT,
    })
}

fn append_telemetry(
    sink: &Option<Arc<Mutex<telemetry::TelemetrySink>>>,
    metadata: telemetry::TelemetryMetadata,
) {
    if let Some(sink) = sink {
        let result = sink
            .lock()
            .map_err(|_| String::from("telemetry sink lock is poisoned"))
            .and_then(|sink| sink.append(&metadata).map_err(|error| error.to_string()));
        if let Err(error) = result {
            if !TELEMETRY_ERROR_REPORTED.swap(true, Ordering::Relaxed) {
                eprintln!("telemetry persistence failed: {error}");
            }
        }
    }
}

fn build_app_state<R: tauri::Runtime>(_app: &tauri::AppHandle<R>) -> AppState {
    let fallback_voice_pipeline_config = default_voice_pipeline_config();
    let cue_asset_paths = embedded_cue_asset_paths();

    match voxgolem_core::config::load_runtime_config(None) {
        Ok(config) => {
            let telemetry_sink = new_telemetry_sink(config.telemetry);
            let mut capabilities = configured_capabilities(&config);
            let voice_pipeline_config =
                voice_pipeline_config_with_silence_timeout(config.silence_timeout_ms);
            let supported_response_profiles = supported_response_profiles(&config.response_backend);
            let selected_response_profile = Arc::new(Mutex::new(
                resolve_selected_response_profile(&supported_response_profiles),
            ));
            let selected_profile_at_startup = selected_response_profile
                .lock()
                .map(|guard| *guard)
                .unwrap_or_else(|_| default_response_profile());
            let assistant_settings =
                resolve_assistant_settings(&config, &capabilities, selected_profile_at_startup);
            let response_profile_switch_generation = Arc::new(AtomicU64::new(0));
            let llama_startups = Arc::new(Mutex::new(Vec::new()));
            let wake_word_runtime = if config.wake_word_model_path.is_file() {
                wake_word::WakeWordRuntime::new(
                    &config.wake_word_model_path,
                    config.wake_word_detection_threshold,
                )
                .map(Mutex::new)
                .map_err(|error| eprintln!("failed to initialize wake word detector: {error}"))
                .ok()
            } else {
                None
            };
            if wake_word_runtime.is_none() && config.wake_word_model_path.is_file() {
                mark_capability(
                    &mut capabilities,
                    "wake_word",
                    CapabilityStatePayload::Failed,
                    String::from("wake word runtime failed to initialize"),
                );
            }
            let effective_tts_enabled = resolve_effective_tts_enabled(
                config.local_tts.enabled,
                config.local_tts.model_path.is_file(),
            );
            let local_tts_runtime = match initialize_local_tts_runtime(
                &config.local_tts,
                effective_tts_enabled,
                config.logging.enabled,
            ) {
                Ok(runtime) => runtime,
                Err(error) => {
                    eprintln!("{error}");
                    None
                }
            };
            let tts_enabled = local_tts_runtime.is_some();
            repair_tts_capability(&mut capabilities, tts_enabled, local_tts_runtime.as_ref());
            if effective_tts_enabled && local_tts_runtime.is_none() {
                mark_capability(
                    &mut capabilities,
                    "tts",
                    CapabilityStatePayload::Failed,
                    String::from("TTS runtime failed to initialize"),
                );
            }
            let tts_output_gain_db = config.local_tts.output_gain_db;
            let mut voice_input_errors = Vec::new();
            let parakeet_runtime = if config.parakeet_model_dir.is_dir() {
                match transcription::ParakeetRuntime::load(&config.parakeet_model_dir) {
                    Ok(runtime) => Some(Arc::new(Mutex::new(runtime))),
                    Err(error) => {
                        let error_message =
                            format!("failed to initialize parakeet transcriber: {error:?}");
                        eprintln!("{error_message}");
                        voice_input_errors.push(error_message);
                        None
                    }
                }
            } else {
                None
            };
            let voice_activity_runtime = if config.silero_vad_model.is_file() {
                match voice_activity::VoiceActivityRuntime::load(&config.silero_vad_model) {
                    Ok(runtime) => Some(Mutex::new(runtime)),
                    Err(error) => {
                        let error_message =
                            format!("failed to initialize voice activity detector: {error:?}");
                        eprintln!("{error_message}");
                        voice_input_errors.push(error_message);
                        None
                    }
                }
            } else {
                None
            };
            if parakeet_runtime.is_none() && config.parakeet_model_dir.is_dir() {
                mark_capability(
                    &mut capabilities,
                    "parakeet",
                    CapabilityStatePayload::Failed,
                    String::from("Parakeet runtime failed to initialize"),
                );
            }
            if voice_activity_runtime.is_none() && config.silero_vad_model.is_file() {
                mark_capability(
                    &mut capabilities,
                    "vad",
                    CapabilityStatePayload::Failed,
                    String::from("VAD runtime failed to initialize"),
                );
            }
            let voice_input_available = wake_word_runtime.is_some()
                && parakeet_runtime.is_some()
                && voice_activity_runtime.is_some();
            if wake_word_runtime.is_none() {
                voice_input_errors.push(String::from("wake word runtime is unavailable"));
            }
            let voice_input_error = if voice_input_errors.is_empty() {
                None
            } else {
                Some(voice_input_errors.join("\n"))
            };
            let llama_cpp_system_prompt = match &config.response_backend {
                voxgolem_core::config::ResponseBackendConfig::LlamaCpp { .. } => {
                    match load_llama_cpp_system_prompt() {
                        Ok(prompt) => Some(prompt),
                        Err(error) => {
                            return build_startup_error_app_state(
                                voice_pipeline_config,
                                format!("failed to load SOUL.md: {error}"),
                            );
                        }
                    }
                }
                voxgolem_core::config::ResponseBackendConfig::Opencode { .. } => None,
                voxgolem_core::config::ResponseBackendConfig::Unconfigured => None,
            };
            let startup_state = Arc::new(Mutex::new(match &config.response_backend {
                voxgolem_core::config::ResponseBackendConfig::LlamaCpp { .. } => {
                    let mut warming_capabilities = capabilities.clone();
                    mark_capability(
                        &mut warming_capabilities,
                        if selected_profile_at_startup == ResponseProfilePayload::Quality {
                            "local_quality"
                        } else {
                            "local_fast"
                        },
                        CapabilityStatePayload::Warming,
                        String::from("local model is loading"),
                    );
                    StartupStatePayload::WarmingModel {
                        cue_asset_paths: cue_asset_paths.clone(),
                        runtime_phase: RuntimePhasePayload::Initializing,
                        voice_input_available,
                        voice_input_error: voice_input_error.clone(),
                        silence_timeout_ms: config.silence_timeout_ms,
                        message: String::from("Loading local Gemma model..."),
                        selected_response_profile: selected_profile_at_startup,
                        supported_response_profiles: supported_response_profiles.clone(),
                        prompt_cancellation_available: false,
                        tts_enabled,
                        tts_output_gain_db,
                        capabilities: warming_capabilities,
                    }
                }
                voxgolem_core::config::ResponseBackendConfig::Opencode { .. } => {
                    StartupStatePayload::Ready {
                        cue_asset_paths: cue_asset_paths.clone(),
                        runtime_phase: RuntimePhasePayload::Sleeping,
                        voice_input_available,
                        voice_input_error: voice_input_error.clone(),
                        silence_timeout_ms: config.silence_timeout_ms,
                        selected_response_profile: selected_profile_at_startup,
                        supported_response_profiles: supported_response_profiles.clone(),
                        prompt_cancellation_available: true,
                        tts_enabled,
                        tts_output_gain_db,
                        capabilities: capabilities.clone(),
                    }
                }
                voxgolem_core::config::ResponseBackendConfig::Unconfigured => {
                    StartupStatePayload::Ready {
                        cue_asset_paths: cue_asset_paths.clone(),
                        runtime_phase: RuntimePhasePayload::Sleeping,
                        voice_input_available,
                        voice_input_error: voice_input_error.clone(),
                        silence_timeout_ms: config.silence_timeout_ms,
                        selected_response_profile: selected_profile_at_startup,
                        supported_response_profiles: Vec::new(),
                        prompt_cancellation_available: true,
                        tts_enabled,
                        tts_output_gain_db,
                        capabilities: capabilities.clone(),
                    }
                }
            }));
            let llama_cpp_runtime = Arc::new(Mutex::new(None));
            if capabilities.iter().any(|capability| {
                capability.id == "local_fast"
                    && capability.state == CapabilityStatePayload::Available
            }) {
                if let voxgolem_core::config::ResponseBackendConfig::LlamaCpp {
                    server_path,
                    host,
                    port,
                    fast_model_path,
                    quality_model_path,
                } = &config.response_backend
                {
                    let startup_state = Arc::clone(&startup_state);
                    let llama_cpp_runtime = Arc::clone(&llama_cpp_runtime);
                    let response_profile_switch_generation =
                        Arc::clone(&response_profile_switch_generation);
                    let startup_generation =
                        response_profile_switch_generation.load(Ordering::SeqCst);
                    let cue_asset_paths = cue_asset_paths.clone();
                    let voice_input_error = voice_input_error.clone();
                    let silence_timeout_ms = config.silence_timeout_ms;
                    let selected_response_profile = selected_profile_at_startup;
                    let supported_response_profiles = supported_response_profiles.clone();
                    let inference_policy = config
                        .llama_cpp
                        .as_ref()
                        .map(|llama| platform_inference_policy(llama.inference_provider))
                        .unwrap_or(voxgolem_platform::inference::InferencePolicy::Auto);
                    let model_path = match model_path_for_profile(
                        selected_response_profile,
                        fast_model_path,
                        quality_model_path.as_ref(),
                    ) {
                        Ok(path) => path.to_path_buf(),
                        Err(_) => fast_model_path.clone(),
                    };
                    let server_spec = voxgolem_platform::llama_cpp::LlamaCppServerSpec::new(
                        server_path.clone(),
                        model_path,
                        host.clone(),
                        *port,
                        LLAMA_CPP_MODEL_ALIAS,
                    );

                    let (startup_cancellation, startup_worker) =
                        voxgolem_platform::llama_cpp::LlamaCppRuntime::start_with_policy_cancellable(
                            server_spec, inference_policy);
                    let startup_coordinator = std::thread::spawn(move || {
                        let start_result = startup_worker.join().unwrap_or_else(|_| Err(voxgolem_platform::llama_cpp::LlamaCppRuntimeError::StartupCancelled));
                        if response_profile_switch_generation.load(Ordering::SeqCst)
                            != startup_generation
                        {
                            shutdown_llama_start_result(start_result);
                            return;
                        }

                        let local_capability_id =
                            if selected_response_profile == ResponseProfilePayload::Quality {
                                "local_quality"
                            } else {
                                "local_fast"
                            };
                        let local_start_result = match start_result {
                            Ok(runtime) => {
                                let actual_provider =
                                    actual_inference_provider_name(runtime.actual_provider());
                                if !store_llama_runtime_if_current(
                                    runtime,
                                    &llama_cpp_runtime,
                                    &response_profile_switch_generation,
                                    startup_generation,
                                ) {
                                    return;
                                }
                                Ok(actual_provider)
                            }
                            Err(error) => Err(format!(
                                "failed to initialize local llama.cpp runtime: {error}"
                            )),
                        };

                        if let Ok(mut guard) = startup_state.lock() {
                            if response_profile_switch_generation.load(Ordering::SeqCst)
                                != startup_generation
                            {
                                return;
                            }
                            let (mut latest_capabilities, current_tts_enabled) = match &*guard {
                                StartupStatePayload::WarmingModel {
                                    capabilities,
                                    tts_enabled,
                                    ..
                                }
                                | StartupStatePayload::Ready {
                                    capabilities,
                                    tts_enabled,
                                    ..
                                } => (capabilities.clone(), *tts_enabled),
                                StartupStatePayload::Error { .. } => return,
                            };
                            let local_ready = local_start_result.is_ok();
                            if let Some(capability) = latest_capabilities
                                .iter_mut()
                                .find(|item| item.id == local_capability_id)
                            {
                                match local_start_result {
                                    Ok(actual_provider) => {
                                        capability.state = CapabilityStatePayload::Available;
                                        capability.reason = String::from("ready");
                                        capability.actual_provider = Some(actual_provider);
                                    }
                                    Err(reason) => {
                                        capability.state = CapabilityStatePayload::Failed;
                                        capability.reason = reason;
                                        capability.actual_provider = None;
                                    }
                                }
                            }
                            *guard = StartupStatePayload::Ready {
                                cue_asset_paths,
                                runtime_phase: RuntimePhasePayload::Sleeping,
                                voice_input_available,
                                voice_input_error,
                                silence_timeout_ms,
                                selected_response_profile,
                                supported_response_profiles: if local_ready {
                                    supported_response_profiles
                                } else {
                                    Vec::new()
                                },
                                prompt_cancellation_available: true,
                                tts_enabled: current_tts_enabled,
                                tts_output_gain_db,
                                capabilities: latest_capabilities,
                            };
                        }
                    });
                    register_llama_startup(
                        &llama_startups,
                        startup_cancellation,
                        startup_coordinator,
                    );
                }
            }
            let voice_pipeline_state = apply_voice_pipeline_event_or_panic(
                voxgolem_core::voice_pipeline::VoicePipelineState::new(voice_pipeline_config)
                    .expect("voice pipeline should initialize with valid constants"),
                voice_pipeline_config,
                voxgolem_core::voice_pipeline::VoicePipelineEvent::StartupValidated,
                "startup validation should initialize the session to sleeping",
            );

            AppState {
                startup_state,
                runtime_config: Some(config),
                selected_response_profile,
                supported_response_profiles,
                response_profile_switch_generation,
                response_backend_operation_lock: Mutex::new(()),
                voice_pipeline_config,
                voice_pipeline_state: Mutex::new(voice_pipeline_state),
                wake_word_runtime,
                voice_activity_runtime,
                parakeet_runtime,
                partial_transcription: new_partial_transcription_scheduler(),
                partial_voice_session: AtomicU64::new(0),
                completion_runtime: Mutex::new(None),
                completion_request: Arc::new(Mutex::new(None)),
                completion_context: Arc::new(Mutex::new(None)),
                completion_generation: AtomicU64::new(0),
                completion_lifecycle_lock: Mutex::new(()),
                telemetry_sink,
                assistant_coordinator: Arc::new(Mutex::new(
                    voxgolem_core::assistant::AssistantCoordinator::new(assistant_settings.into()),
                )),
                assistant_settings_generation: Arc::new(AtomicU64::new(0)),
                tts_operation_lock: tokio::sync::Mutex::new(()),
                local_tts_runtime: Mutex::new(local_tts_runtime.map(Arc::new)),
                llama_cpp_runtime,
                llama_cpp_conversation: Mutex::new(Vec::new()),
                llama_cpp_system_prompt,
                opencode_server: Arc::new(Mutex::new(None)),
                active_prompt: Arc::new(Mutex::new(None)),
                active_prompt_generation: AtomicU64::new(0),
                prefetch_generation: AtomicU64::new(0),
                prefetch_cache: Mutex::new(None),
                prefetch_task: Mutex::new(None),
                llama_startups,
                exit_cleanup_started: AtomicBool::new(false),
            }
        }
        Err(error) => build_nonfatal_config_error_app_state(
            fallback_voice_pipeline_config,
            cue_asset_paths,
            error.to_string(),
        ),
    }
}

fn initialize_local_tts_runtime(
    config: &voxgolem_core::config::LocalTtsConfig,
    enabled: bool,
    runtime_file_logging_enabled: bool,
) -> Result<Option<tts::LocalTtsRuntime>, String> {
    if !enabled {
        log_tts_runtime_event(
            runtime_file_logging_enabled,
            "runtime initialization skipped: disabled",
        );
        return Ok(None);
    }

    if let Some(strict_espeak_root) = tts::resolve_strict_windows_espeak_data_directory()
        .map_err(|error| format!("failed to resolve strict eSpeak data root: {error}"))?
    {
        log_tts_runtime_event(
            runtime_file_logging_enabled,
            &format!(
                "resolved strict eSpeak data root: {}",
                strict_espeak_root.display()
            ),
        );
    }

    let spec = tts::LocalTtsRuntimeSpec {
        enabled: true,
        model_path: Some(config.model_path.clone()),
        worker_count: config.worker_count,
        max_queue: config.max_queue,
        sample_rate_hz: config.sample_rate_hz,
        max_duration_s: config.max_duration_s,
        provider_policy: tts::TtsProviderPolicy::Auto,
    };

    match tts::LocalTtsRuntime::new(spec) {
        Ok(runtime) => {
            log_tts_runtime_event(
                runtime_file_logging_enabled,
                "runtime initialized successfully",
            );
            Ok(Some(runtime))
        }
        Err(error) => {
            let message = format!("failed to initialize local tts runtime: {error}");
            log_tts_runtime_event(runtime_file_logging_enabled, &message);
            Err(message)
        }
    }
}

fn embedded_cue_asset_paths() -> CueAssetPathsPayload {
    CueAssetPathsPayload {
        start_listening: cue_audio_data_url(START_LISTENING_CUE_WAV),
        stop_listening: cue_audio_data_url(STOP_LISTENING_CUE_WAV),
    }
}

fn cue_audio_data_url(bytes: &[u8]) -> String {
    format!(
        "{CUE_AUDIO_DATA_URL_PREFIX}{}",
        base64::engine::general_purpose::STANDARD.encode(bytes)
    )
}

fn build_startup_error_app_state(
    voice_pipeline_config: voxgolem_core::voice_pipeline::VoicePipelineConfig,
    message: String,
) -> AppState {
    let voice_pipeline_state = apply_voice_pipeline_event_or_panic(
        voxgolem_core::voice_pipeline::VoicePipelineState::new(voice_pipeline_config)
            .expect("voice pipeline should initialize with valid constants"),
        voice_pipeline_config,
        voxgolem_core::voice_pipeline::VoicePipelineEvent::StartupFailed {
            message: message.clone(),
        },
        "startup failure should initialize the session to error",
    );

    AppState {
        startup_state: Arc::new(Mutex::new(StartupStatePayload::Error { message })),
        runtime_config: None,
        selected_response_profile: Arc::new(Mutex::new(default_response_profile())),
        supported_response_profiles: vec![default_response_profile()],
        response_profile_switch_generation: Arc::new(AtomicU64::new(0)),
        response_backend_operation_lock: Mutex::new(()),
        voice_pipeline_config,
        voice_pipeline_state: Mutex::new(voice_pipeline_state),
        wake_word_runtime: None,
        voice_activity_runtime: None,
        parakeet_runtime: None,
        partial_transcription: new_partial_transcription_scheduler(),
        partial_voice_session: AtomicU64::new(0),
        completion_runtime: Mutex::new(None),
        completion_request: Arc::new(Mutex::new(None)),
        completion_context: Arc::new(Mutex::new(None)),
        completion_generation: AtomicU64::new(0),
        completion_lifecycle_lock: Mutex::new(()),
        telemetry_sink: default_telemetry_sink(),
        assistant_coordinator: Arc::new(Mutex::new(
            voxgolem_core::assistant::AssistantCoordinator::new(
                AssistantSettingsPayload::default().into(),
            ),
        )),
        assistant_settings_generation: Arc::new(AtomicU64::new(0)),
        tts_operation_lock: tokio::sync::Mutex::new(()),
        local_tts_runtime: Mutex::new(None),
        llama_cpp_runtime: Arc::new(Mutex::new(None)),
        llama_cpp_conversation: Mutex::new(Vec::new()),
        llama_cpp_system_prompt: None,
        opencode_server: Arc::new(Mutex::new(None)),
        active_prompt: Arc::new(Mutex::new(None)),
        active_prompt_generation: AtomicU64::new(0),
        prefetch_generation: AtomicU64::new(0),
        prefetch_cache: Mutex::new(None),
        prefetch_task: Mutex::new(None),
        llama_startups: Arc::new(Mutex::new(Vec::new())),
        exit_cleanup_started: AtomicBool::new(false),
    }
}

fn build_nonfatal_config_error_app_state(
    voice_pipeline_config: voxgolem_core::voice_pipeline::VoicePipelineConfig,
    cue_asset_paths: CueAssetPathsPayload,
    message: String,
) -> AppState {
    let voice_pipeline_state = apply_voice_pipeline_event_or_panic(
        voxgolem_core::voice_pipeline::VoicePipelineState::new(voice_pipeline_config)
            .expect("voice pipeline should initialize with valid constants"),
        voice_pipeline_config,
        voxgolem_core::voice_pipeline::VoicePipelineEvent::StartupValidated,
        "nonfatal config failure should leave the shell usable",
    );
    AppState {
        startup_state: Arc::new(Mutex::new(StartupStatePayload::Ready {
            cue_asset_paths,
            runtime_phase: RuntimePhasePayload::Sleeping,
            voice_input_available: false,
            voice_input_error: Some(message.clone()),
            silence_timeout_ms: DEFAULT_SILENCE_TIMEOUT_MS,
            selected_response_profile: default_response_profile(),
            supported_response_profiles: Vec::new(),
            prompt_cancellation_available: false,
            tts_enabled: false,
            tts_output_gain_db: 3.0,
            capabilities: failed_capabilities(message),
        })),
        runtime_config: None,
        selected_response_profile: Arc::new(Mutex::new(default_response_profile())),
        supported_response_profiles: Vec::new(),
        response_profile_switch_generation: Arc::new(AtomicU64::new(0)),
        response_backend_operation_lock: Mutex::new(()),
        voice_pipeline_config,
        voice_pipeline_state: Mutex::new(voice_pipeline_state),
        wake_word_runtime: None,
        voice_activity_runtime: None,
        parakeet_runtime: None,
        partial_transcription: new_partial_transcription_scheduler(),
        partial_voice_session: AtomicU64::new(0),
        completion_runtime: Mutex::new(None),
        completion_request: Arc::new(Mutex::new(None)),
        completion_context: Arc::new(Mutex::new(None)),
        completion_generation: AtomicU64::new(0),
        completion_lifecycle_lock: Mutex::new(()),
        telemetry_sink: default_telemetry_sink(),
        assistant_coordinator: Arc::new(Mutex::new(
            voxgolem_core::assistant::AssistantCoordinator::new(
                AssistantSettingsPayload::default().into(),
            ),
        )),
        assistant_settings_generation: Arc::new(AtomicU64::new(0)),
        tts_operation_lock: tokio::sync::Mutex::new(()),
        local_tts_runtime: Mutex::new(None),
        llama_cpp_runtime: Arc::new(Mutex::new(None)),
        llama_cpp_conversation: Mutex::new(Vec::new()),
        llama_cpp_system_prompt: None,
        opencode_server: Arc::new(Mutex::new(None)),
        active_prompt: Arc::new(Mutex::new(None)),
        active_prompt_generation: AtomicU64::new(0),
        prefetch_generation: AtomicU64::new(0),
        prefetch_cache: Mutex::new(None),
        prefetch_task: Mutex::new(None),
        llama_startups: Arc::new(Mutex::new(Vec::new())),
        exit_cleanup_started: AtomicBool::new(false),
    }
}

fn load_llama_cpp_system_prompt() -> Result<String, String> {
    let soul_path = application_config_path()?.with_file_name(WINDOWS_SOUL_FILE_NAME);
    let contents = fs::read_to_string(&soul_path)
        .map_err(|error| format!("{}: {error}", soul_path.display()))?;
    let trimmed = contents.trim();

    if trimmed.is_empty() {
        return Err(format!("{} is empty", soul_path.display()));
    }

    Ok(trimmed.to_string())
}

fn current_runtime_phase(
    voice_pipeline_state: &Mutex<voxgolem_core::voice_pipeline::VoicePipelineState>,
) -> Result<RuntimePhasePayload, String> {
    let guard = voice_pipeline_state
        .lock()
        .map_err(|_| String::from("voice pipeline lock is poisoned"))?;

    Ok(to_runtime_phase_payload(guard.session().runtime().phase()))
}

fn ensure_response_profile_switch_runtime_is_idle(
    voice_pipeline_state: &Mutex<voxgolem_core::voice_pipeline::VoicePipelineState>,
) -> Result<(), String> {
    let runtime_phase = current_runtime_phase(voice_pipeline_state)?;

    if runtime_phase != RuntimePhasePayload::Sleeping {
        return Err(format!(
            "response profile switch is only allowed while runtime is sleeping; current phase is {}",
            runtime_phase.as_str()
        ));
    }

    Ok(())
}

fn ensure_startup_ready_for_prompt(
    startup_state: &Arc<Mutex<StartupStatePayload>>,
) -> Result<(), String> {
    let startup_state = startup_state
        .lock()
        .map_err(|_| String::from("startup state lock should not be poisoned"))?;

    match &*startup_state {
        StartupStatePayload::Ready { .. } => Ok(()),
        StartupStatePayload::WarmingModel { .. } => {
            Err(String::from("local Gemma model is still warming up"))
        }
        StartupStatePayload::Error { message } => Err(format!("startup error: {message}")),
    }
}

fn ensure_startup_ready_for_profile_switch(
    startup_state: &Arc<Mutex<StartupStatePayload>>,
) -> Result<(), String> {
    let startup_state = startup_state
        .lock()
        .map_err(|_| String::from("startup state lock should not be poisoned"))?;

    match &*startup_state {
        StartupStatePayload::Ready { .. } => Ok(()),
        StartupStatePayload::WarmingModel { .. } => Err(String::from(
            "response backend is busy; wait for the active operation to finish",
        )),
        StartupStatePayload::Error { message } => Err(format!("startup error: {message}")),
    }
}

fn lock_response_backend_operation<'a>(
    operation_lock: &'a Mutex<()>,
) -> Result<MutexGuard<'a, ()>, String> {
    operation_lock
        .lock()
        .map_err(|_| String::from("response backend operation lock is poisoned"))
}

fn try_lock_response_backend_operation<'a>(
    operation_lock: &'a Mutex<()>,
) -> Result<MutexGuard<'a, ()>, String> {
    match operation_lock.try_lock() {
        Ok(guard) => Ok(guard),
        Err(TryLockError::WouldBlock) => Err(String::from(
            "response backend is busy; wait for the active operation to finish",
        )),
        Err(TryLockError::Poisoned(_)) => {
            Err(String::from("response backend operation lock is poisoned"))
        }
    }
}

fn try_lock_response_backend_operation_or_busy<'a>(
    operation_lock: &'a Mutex<()>,
) -> Result<Option<MutexGuard<'a, ()>>, String> {
    match operation_lock.try_lock() {
        Ok(guard) => Ok(Some(guard)),
        Err(TryLockError::WouldBlock) => Ok(None),
        Err(TryLockError::Poisoned(_)) => {
            Err(String::from("response backend operation lock is poisoned"))
        }
    }
}

#[allow(dead_code)]
fn current_silence_deadline(
    voice_pipeline_state: &Mutex<voxgolem_core::voice_pipeline::VoicePipelineState>,
    voice_pipeline_config: voxgolem_core::voice_pipeline::VoicePipelineConfig,
) -> Result<u64, String> {
    let guard = voice_pipeline_state
        .lock()
        .map_err(|_| String::from("voice pipeline lock is poisoned"))?;

    let last_activity_ms = guard.session().voice_turn().last_activity_ms().unwrap_or(0);
    Ok(last_activity_ms.saturating_add(
        voice_pipeline_config
            .session()
            .voice_turn()
            .silence_timeout_ms(),
    ))
}

fn current_runtime_phase_response(
    voice_pipeline_state: &Mutex<voxgolem_core::voice_pipeline::VoicePipelineState>,
    transcription_ready_samples: Option<usize>,
    transcript_text: Option<String>,
) -> Result<RuntimePhaseResponsePayload, String> {
    let guard = voice_pipeline_state
        .lock()
        .map_err(|_| String::from("voice pipeline lock is poisoned"))?;

    Ok(runtime_phase_response_from_state(
        &guard,
        transcription_ready_samples,
        transcript_text,
        None,
    ))
}

fn runtime_phase_response_from_state(
    voice_pipeline_state: &voxgolem_core::voice_pipeline::VoicePipelineState,
    transcription_ready_samples: Option<usize>,
    transcript_text: Option<String>,
    telemetry: Option<RuntimeTelemetryPayload>,
) -> RuntimePhaseResponsePayload {
    RuntimePhaseResponsePayload {
        runtime_phase: to_runtime_phase_payload(voice_pipeline_state.session().runtime().phase()),
        transcription_ready_samples,
        transcript_text,
        last_activity_ms: voice_pipeline_state
            .session()
            .voice_turn()
            .last_activity_ms(),
        capturing_utterance: voice_pipeline_state.capture().capturing_utterance(),
        preroll_samples: voice_pipeline_state.capture().preroll_len(),
        utterance_samples: voice_pipeline_state.capture().utterance_len(),
        telemetry,
    }
}

fn process_wake_word_frame(
    wake_word_runtime: &Option<Mutex<wake_word::WakeWordRuntime>>,
    frame: &[f32],
) -> Result<Option<wake_word::WakeWordDetection>, String> {
    let Some(wake_word_runtime) = wake_word_runtime else {
        return Ok(None);
    };

    let mut guard = wake_word_runtime
        .lock()
        .map_err(|_| String::from("wake word runtime lock is poisoned"))?;

    guard.process_sleeping_frame(frame)
}

fn reset_wake_word_runtime(
    wake_word_runtime: &Option<Mutex<wake_word::WakeWordRuntime>>,
) -> Result<(), String> {
    let Some(wake_word_runtime) = wake_word_runtime else {
        return Ok(());
    };

    let mut guard = wake_word_runtime
        .lock()
        .map_err(|_| String::from("wake word runtime lock is poisoned"))?;
    guard.reset();
    Ok(())
}

fn process_voice_activity_frame(
    voice_activity_runtime: &Option<Mutex<voice_activity::VoiceActivityRuntime>>,
    frame: &[f32],
) -> Result<bool, String> {
    let Some(voice_activity_runtime) = voice_activity_runtime else {
        return Ok(false);
    };

    let mut guard = voice_activity_runtime
        .lock()
        .map_err(|_| String::from("voice activity runtime lock is poisoned"))?;

    guard
        .process_frame(frame)
        .map_err(|error| format!("voice activity detection failed: {error:?}"))
}

fn reset_voice_activity_runtime(
    voice_activity_runtime: &Option<Mutex<voice_activity::VoiceActivityRuntime>>,
) -> Result<(), String> {
    let Some(voice_activity_runtime) = voice_activity_runtime else {
        return Ok(());
    };

    let mut guard = voice_activity_runtime
        .lock()
        .map_err(|_| String::from("voice activity runtime lock is poisoned"))?;
    guard.reset();
    Ok(())
}

fn current_time_ms() -> Result<u64, String> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .map_err(|error| format!("system clock is before unix epoch: {error}"))
}

fn ingest_audio_frame_with_optional_wake_word_detection(
    voice_pipeline_state: &voxgolem_core::voice_pipeline::VoicePipelineState,
    voice_pipeline_config: voxgolem_core::voice_pipeline::VoicePipelineConfig,
    frame: Vec<f32>,
    wake_word_now_ms: Option<u64>,
) -> Result<voxgolem_core::voice_pipeline::VoicePipelineState, String> {
    if matches!(
        voice_pipeline_state.session().runtime().phase(),
        voxgolem_core::runtime::RuntimePhase::Sleeping
    ) {
        if let Some(now_ms) = wake_word_now_ms {
            let listening_state = voxgolem_core::voice_pipeline::apply_voice_pipeline_event(
                voice_pipeline_state,
                voice_pipeline_config,
                voxgolem_core::voice_pipeline::VoicePipelineEvent::WakeWordDetected { now_ms },
            )
            .map_err(|error| format!("wake word transition failed: {error:?}"))?
            .0;

            return voxgolem_core::voice_pipeline::ingest_audio_frame(
                &listening_state,
                voice_pipeline_config,
                frame,
            )
            .map_err(|error| format!("voice pipeline frame ingestion failed: {error:?}"));
        }
    }

    voxgolem_core::voice_pipeline::ingest_audio_frame(
        voice_pipeline_state,
        voice_pipeline_config,
        frame,
    )
    .map_err(|error| format!("voice pipeline frame ingestion failed: {error:?}"))
}

fn apply_optional_speech_activity(
    voice_pipeline_state: voxgolem_core::voice_pipeline::VoicePipelineState,
    voice_pipeline_config: voxgolem_core::voice_pipeline::VoicePipelineConfig,
    speech_detected: bool,
    now_ms: u64,
) -> Result<voxgolem_core::voice_pipeline::VoicePipelineState, String> {
    if !speech_detected
        || !matches!(
            voice_pipeline_state.session().runtime().phase(),
            voxgolem_core::runtime::RuntimePhase::Listening
        )
    {
        return Ok(voice_pipeline_state);
    }

    voxgolem_core::voice_pipeline::apply_voice_pipeline_event(
        &voice_pipeline_state,
        voice_pipeline_config,
        voxgolem_core::voice_pipeline::VoicePipelineEvent::SpeechDetected { now_ms },
    )
    .map(|(next_state, _)| next_state)
    .map_err(|error| format!("speech activity transition failed: {error:?}"))
}

fn wake_word_event_timestamp(
    now_ms: u64,
    wake_word_detection: Option<wake_word::WakeWordDetection>,
) -> Option<u64> {
    wake_word_detection.map(|_| now_ms)
}

fn to_runtime_phase_payload(
    runtime_phase: voxgolem_core::runtime::RuntimePhase,
) -> RuntimePhasePayload {
    match runtime_phase {
        voxgolem_core::runtime::RuntimePhase::Initializing => RuntimePhasePayload::Initializing,
        voxgolem_core::runtime::RuntimePhase::Sleeping => RuntimePhasePayload::Sleeping,
        voxgolem_core::runtime::RuntimePhase::Listening => RuntimePhasePayload::Listening,
        voxgolem_core::runtime::RuntimePhase::Processing => RuntimePhasePayload::Processing,
        voxgolem_core::runtime::RuntimePhase::Executing => RuntimePhasePayload::Executing,
        voxgolem_core::runtime::RuntimePhase::Error => RuntimePhasePayload::Error,
    }
}

fn default_voice_pipeline_config() -> voxgolem_core::voice_pipeline::VoicePipelineConfig {
    voice_pipeline_config_with_silence_timeout(DEFAULT_SILENCE_TIMEOUT_MS)
}

fn voice_pipeline_config_with_silence_timeout(
    silence_timeout_ms: u64,
) -> voxgolem_core::voice_pipeline::VoicePipelineConfig {
    let voice_turn = voxgolem_core::voice_turn::VoiceTurnConfig::new(silence_timeout_ms)
        .expect("silence timeout constant should be valid");
    let capture = voxgolem_core::turn_capture::TurnCaptureConfig::new(
        DEFAULT_PREROLL_MAX_SAMPLES,
        DEFAULT_UTTERANCE_MAX_SAMPLES,
    )
    .expect("turn capture constants should be valid");

    voxgolem_core::voice_pipeline::VoicePipelineConfig::new(
        voxgolem_core::session::SessionConfig::new(voice_turn),
        capture,
        voxgolem_model::parakeet::PARAKEET_SAMPLE_RATE_HZ,
    )
}

fn apply_voice_pipeline_transition(
    voice_pipeline_state: &Mutex<voxgolem_core::voice_pipeline::VoicePipelineState>,
    voice_pipeline_config: voxgolem_core::voice_pipeline::VoicePipelineConfig,
    event: voxgolem_core::voice_pipeline::VoicePipelineEvent,
) -> Result<voxgolem_core::voice_pipeline::VoicePipelineAction, String> {
    let mut guard = voice_pipeline_state
        .lock()
        .map_err(|_| String::from("voice pipeline lock is poisoned"))?;

    let (next_state, action) = voxgolem_core::voice_pipeline::apply_voice_pipeline_event(
        &guard,
        voice_pipeline_config,
        event,
    )
    .map_err(|error| format!("voice pipeline transition failed: {error:?}"))?;

    *guard = next_state;
    Ok(action)
}

fn apply_voice_pipeline_transition_with_input_runtime_reset(
    voice_pipeline_state: &Mutex<voxgolem_core::voice_pipeline::VoicePipelineState>,
    wake_word_runtime: &Option<Mutex<wake_word::WakeWordRuntime>>,
    voice_activity_runtime: &Option<Mutex<voice_activity::VoiceActivityRuntime>>,
    voice_pipeline_config: voxgolem_core::voice_pipeline::VoicePipelineConfig,
    event: voxgolem_core::voice_pipeline::VoicePipelineEvent,
) -> Result<voxgolem_core::voice_pipeline::VoicePipelineAction, String> {
    let mut voice_pipeline_guard = voice_pipeline_state
        .lock()
        .map_err(|_| String::from("voice pipeline lock is poisoned"))?;

    reset_wake_word_runtime(wake_word_runtime)?;
    reset_voice_activity_runtime(voice_activity_runtime)?;

    let (next_state, action) = voxgolem_core::voice_pipeline::apply_voice_pipeline_event(
        &voice_pipeline_guard,
        voice_pipeline_config,
        event,
    )
    .map_err(|error| format!("voice pipeline transition failed: {error:?}"))?;

    *voice_pipeline_guard = next_state;
    Ok(action)
}

fn apply_voice_pipeline_event_or_panic(
    state: voxgolem_core::voice_pipeline::VoicePipelineState,
    config: voxgolem_core::voice_pipeline::VoicePipelineConfig,
    event: voxgolem_core::voice_pipeline::VoicePipelineEvent,
    message: &str,
) -> voxgolem_core::voice_pipeline::VoicePipelineState {
    voxgolem_core::voice_pipeline::apply_voice_pipeline_event(&state, config, event)
        .expect(message)
        .0
}

fn validate_prompt_text(prompt: String) -> Result<String, String> {
    if prompt.trim().is_empty() {
        return Err(String::from("invalid prompt: prompt must not be empty"));
    }
    if prompt.len() > PROMPT_MAX_BYTES {
        return Err(format!(
            "invalid prompt: prompt exceeds {PROMPT_MAX_BYTES} bytes"
        ));
    }

    Ok(prompt)
}

fn bounded_provider_history(
    history: &[voxgolem_core::assistant::ConversationTurn],
) -> &[voxgolem_core::assistant::ConversationTurn] {
    let mut start = history.len();
    let mut bytes = 0_usize;

    while start >= 2 {
        let pair_start = start - 2;
        let pair_bytes = history[pair_start..start]
            .iter()
            .map(|turn| assistant_content_text(&turn.content).len())
            .sum::<usize>();
        let Some(next_bytes) = bytes.checked_add(pair_bytes) else {
            break;
        };
        if next_bytes > PROVIDER_HISTORY_MAX_BYTES {
            break;
        }
        bytes = next_bytes;
        start = pair_start;
    }

    &history[start..]
}

fn validate_prompt_request_id(request_id: &str) -> Result<(), String> {
    let Some((timestamp, nonce)) = request_id
        .strip_prefix("request-")
        .and_then(|value| value.split_once('-'))
    else {
        return Err(String::from(
            "request ID must be an opaque generated identifier",
        ));
    };
    if timestamp.is_empty()
        || timestamp.len() > 20
        || !timestamp.bytes().all(|byte| byte.is_ascii_digit())
        || nonce.is_empty()
        || nonce.len() > 32
        || !nonce.bytes().all(|byte| byte.is_ascii_alphanumeric())
    {
        return Err(String::from(
            "request ID must be an opaque generated identifier",
        ));
    }
    Ok(())
}

fn execute_prompt_backend(
    config: &voxgolem_core::config::RuntimeConfig,
    prompt: &str,
    llama_cpp_runtime: &Arc<Mutex<Option<voxgolem_platform::llama_cpp::LlamaCppRuntime>>>,
    llama_cpp_conversation: &Mutex<Vec<LlamaConversationTurn>>,
    llama_cpp_system_prompt: Option<&str>,
) -> Result<PromptExecutionOutcome, String> {
    match &config.response_backend {
        voxgolem_core::config::ResponseBackendConfig::Unconfigured => {
            Err(String::from("no response provider is available"))
        }
        voxgolem_core::config::ResponseBackendConfig::Opencode { .. } => Err(String::from(
            "OpenCode prompts require the persistent streaming runtime",
        )),
        voxgolem_core::config::ResponseBackendConfig::LlamaCpp { .. } => {
            let system_prompt =
                llama_cpp_system_prompt.ok_or_else(|| String::from("SOUL.md is not loaded"))?;
            let conversation_snapshot = llama_cpp_conversation
                .lock()
                .map_err(|_| String::from("local llama.cpp conversation lock is poisoned"))?
                .clone();
            let prompt_input =
                build_llama_prompt_input(system_prompt, prompt, &conversation_snapshot);
            let LlamaPromptInput {
                mut user_prompt,
                rolled_over: initially_rolled_over,
            } = prompt_input;
            let mut guard = llama_cpp_runtime
                .lock()
                .map_err(|_| String::from("local llama.cpp runtime lock is poisoned"))?;
            let runtime = guard
                .as_mut()
                .ok_or_else(|| String::from("local Gemma model is still warming up"))?;
            let mut rolled_over = initially_rolled_over;
            let can_retry_with_reset = !conversation_snapshot.is_empty() && !rolled_over;
            let response = match runtime.chat(
                &voxgolem_platform::llama_cpp::LlamaCppPrompt::new(user_prompt.clone())
                    .with_system_prompt(system_prompt)
                    .with_max_tokens(LLAMA_CPP_MAX_TOKENS),
            ) {
                Ok(response) => response,
                Err(error)
                    if can_retry_with_reset
                        && is_llama_context_overflow_error(&error.to_string()) =>
                {
                    user_prompt = render_llama_user_prompt(&[], prompt);
                    rolled_over = true;
                    runtime
                        .chat(
                            &voxgolem_platform::llama_cpp::LlamaCppPrompt::new(
                                user_prompt.clone(),
                            )
                            .with_system_prompt(system_prompt)
                            .with_max_tokens(LLAMA_CPP_MAX_TOKENS),
                        )
                        .map_err(|retry_error| {
                            format!(
                                "failed to execute local llama.cpp prompt after conversation reset: {retry_error}; initial error: {error}"
                            )
                        })?
                }
                Err(error) => {
                    return Err(format!("failed to execute local llama.cpp prompt: {error}"));
                }
            };

            let assistant_text = response.text;
            let mut conversation = llama_cpp_conversation
                .lock()
                .map_err(|_| String::from("local llama.cpp conversation lock is poisoned"))?;
            if rolled_over {
                conversation.clear();
            }
            conversation.push(LlamaConversationTurn {
                user: prompt.to_string(),
                assistant: assistant_text.clone(),
            });

            let mut events = Vec::new();
            if rolled_over {
                events.push(PromptExecutionEventPayload::Reasoning {
                    text: LLAMA_CPP_ROLLOVER_REASON.to_string(),
                });
            }
            events.push(PromptExecutionEventPayload::Text {
                text: assistant_text,
            });

            Ok(PromptExecutionOutcome {
                events,
                stderr: String::new(),
                exit_code: None,
                error_message: None,
            })
        }
    }
}

fn build_llama_prompt_input(
    system_prompt: &str,
    prompt: &str,
    conversation: &[LlamaConversationTurn],
) -> LlamaPromptInput {
    let user_prompt = render_llama_user_prompt(conversation, prompt);
    if estimate_llama_input_tokens(system_prompt, &user_prompt) <= llama_cpp_input_token_limit() {
        return LlamaPromptInput {
            user_prompt,
            rolled_over: false,
        };
    }

    if conversation.is_empty() {
        return LlamaPromptInput {
            user_prompt,
            rolled_over: false,
        };
    }

    LlamaPromptInput {
        user_prompt: render_llama_user_prompt(&[], prompt),
        rolled_over: true,
    }
}

fn render_llama_user_prompt(conversation: &[LlamaConversationTurn], prompt: &str) -> String {
    if conversation.is_empty() {
        return prompt.to_string();
    }

    let mut rendered = String::from("Conversation so far:\n");
    for turn in conversation {
        rendered.push_str("User: ");
        rendered.push_str(&turn.user);
        rendered.push('\n');
        rendered.push_str("Assistant: ");
        rendered.push_str(&turn.assistant);
        rendered.push_str("\n\n");
    }
    rendered.push_str("Current user message:\n");
    rendered.push_str(prompt);
    rendered
}

fn llama_cpp_input_token_limit() -> usize {
    LLAMA_CPP_CONTEXT_WINDOW_TOKENS
        .saturating_sub(usize::from(LLAMA_CPP_MAX_TOKENS))
        .saturating_sub(LLAMA_CPP_CONTEXT_SAFETY_MARGIN_TOKENS)
}

fn estimate_llama_input_tokens(system_prompt: &str, user_prompt: &str) -> usize {
    estimate_text_tokens(system_prompt)
        .saturating_add(estimate_text_tokens(user_prompt))
        .saturating_add(LLAMA_CPP_CHAT_WRAPPER_TOKENS)
}

fn estimate_text_tokens(text: &str) -> usize {
    let char_count = text.chars().count();
    if char_count == 0 {
        0
    } else {
        char_count.div_ceil(4)
    }
}

fn is_llama_context_overflow_error(error_message: &str) -> bool {
    let normalized = error_message.to_ascii_lowercase();
    let has_status = normalized.contains("status 400") || normalized.contains("status 413");
    let mentions_context = normalized.contains("context");
    let mentions_overflow = normalized.contains("exceed")
        || normalized.contains("too long")
        || normalized.contains("limit")
        || normalized.contains("maximum")
        || normalized.contains("window");

    has_status && mentions_context && mentions_overflow
}

fn transcription_ready_samples(
    action: &voxgolem_core::voice_pipeline::VoicePipelineAction,
) -> Option<usize> {
    match action {
        voxgolem_core::voice_pipeline::VoicePipelineAction::FinishedUtterance {
            transcription_input,
        } => Some(transcription_input.samples().len()),
        _ => None,
    }
}

fn transcribe_finished_utterance(
    action: &voxgolem_core::voice_pipeline::VoicePipelineAction,
    parakeet_runtime: &Option<Arc<Mutex<transcription::ParakeetRuntime>>>,
) -> Result<Option<String>, String> {
    let voxgolem_core::voice_pipeline::VoicePipelineAction::FinishedUtterance {
        transcription_input,
    } = action
    else {
        return Ok(None);
    };

    let parakeet_runtime = parakeet_runtime
        .as_ref()
        .ok_or_else(|| String::from("parakeet runtime is not ready"))?;
    let mut guard = parakeet_runtime
        .lock()
        .map_err(|_| String::from("parakeet runtime lock is poisoned"))?;
    let transcript = guard
        .transcribe(transcription_input)
        .map_err(|error| format!("utterance transcription failed: {error:?}"))?;

    Ok(Some(transcript.text().to_string()))
}

fn build_mark_silence_response(
    voice_pipeline_state: &Mutex<voxgolem_core::voice_pipeline::VoicePipelineState>,
    action: &voxgolem_core::voice_pipeline::VoicePipelineAction,
    transcript_text: Option<String>,
    telemetry: Option<RuntimeTelemetryPayload>,
) -> Result<RuntimePhaseResponsePayload, String> {
    let guard = voice_pipeline_state
        .lock()
        .map_err(|_| String::from("voice pipeline lock is poisoned"))?;

    Ok(runtime_phase_response_from_state(
        &guard,
        transcription_ready_samples(action),
        transcript_text,
        telemetry,
    ))
}

fn reset_voice_pipeline_to_waiting(
    voice_pipeline_state: &Mutex<voxgolem_core::voice_pipeline::VoicePipelineState>,
    wake_word_runtime: &Option<Mutex<wake_word::WakeWordRuntime>>,
    voice_activity_runtime: &Option<Mutex<voice_activity::VoiceActivityRuntime>>,
    voice_pipeline_config: voxgolem_core::voice_pipeline::VoicePipelineConfig,
) -> Result<(), String> {
    apply_voice_pipeline_transition_with_input_runtime_reset(
        voice_pipeline_state,
        wake_word_runtime,
        voice_activity_runtime,
        voice_pipeline_config,
        voxgolem_core::voice_pipeline::VoicePipelineEvent::ResetToIdle,
    )?;

    Ok(())
}

fn shutdown_llama_cpp_runtime_for_exit(app_state: &AppState) {
    app_state
        .response_profile_switch_generation
        .fetch_add(1, Ordering::SeqCst);

    let runtime = app_state
        .llama_cpp_runtime
        .lock()
        .map(|mut guard| guard.take())
        .unwrap_or(None);

    if let Some(mut runtime) = runtime {
        runtime.shutdown_owned();
    }
}

fn shutdown_completion_runtime_for_exit(app_state: &AppState) {
    let runtime = {
        let Ok(_lifecycle) = app_state.completion_lifecycle_lock.lock() else {
            return;
        };
        if let Ok(mut request) = app_state.completion_request.lock() {
            if let Some(request) = request.take() {
                request.clear();
            }
        }
        if let Ok(mut context) = app_state.completion_context.lock() {
            context.take();
        }
        app_state
            .completion_runtime
            .lock()
            .map(|mut runtime| runtime.take())
            .unwrap_or(None)
    };
    if let Some(mut runtime) = runtime {
        let _ = tauri::async_runtime::block_on(runtime.shutdown());
    }
}

fn shutdown_prefetch_for_exit(app_state: &AppState) {
    app_state.prefetch_generation.fetch_add(1, Ordering::SeqCst);
    let active = app_state
        .prefetch_task
        .lock()
        .map(|mut task| task.take())
        .unwrap_or(None);
    if let Some(mut active) = active {
        active.cancelled.store(true, Ordering::SeqCst);
        active.cancellation_signal.notify_one();
        if let Some(mut task) = active.task.take() {
            if tauri::async_runtime::block_on(tokio::time::timeout(
                Duration::from_secs(3),
                &mut task,
            ))
            .is_err()
            {
                task.abort();
            }
        }
    }
}

fn store_completion_runtime(
    lifecycle_lock: &Mutex<()>,
    runtime_slot: &Mutex<Option<voxgolem_platform::completion::CompletionRuntime>>,
    request_slot: &Mutex<Option<voxgolem_platform::completion::CompletionRequestHandle>>,
    exit_cleanup_started: &AtomicBool,
    runtime: voxgolem_platform::completion::CompletionRuntime,
    request: voxgolem_platform::completion::CompletionRequestHandle,
) -> Option<voxgolem_platform::completion::CompletionRuntime> {
    let Ok(_lifecycle) = lifecycle_lock.lock() else {
        return Some(runtime);
    };
    let Ok(mut request_slot) = request_slot.lock() else {
        return Some(runtime);
    };
    let Ok(mut runtime_slot) = runtime_slot.lock() else {
        return Some(runtime);
    };
    if exit_cleanup_started.load(Ordering::SeqCst) {
        return Some(runtime);
    }
    *request_slot = Some(request);
    *runtime_slot = Some(runtime);
    None
}

fn shutdown_llama_start_result(
    start_result: Result<
        voxgolem_platform::llama_cpp::LlamaCppRuntime,
        voxgolem_platform::llama_cpp::LlamaCppRuntimeError,
    >,
) {
    if let Ok(mut runtime) = start_result {
        runtime.shutdown_owned();
    }
}

fn store_llama_runtime_if_current(
    mut runtime: voxgolem_platform::llama_cpp::LlamaCppRuntime,
    llama_cpp_runtime: &Arc<Mutex<Option<voxgolem_platform::llama_cpp::LlamaCppRuntime>>>,
    response_profile_switch_generation: &Arc<AtomicU64>,
    expected_generation: u64,
) -> bool {
    if response_profile_switch_generation.load(Ordering::SeqCst) != expected_generation {
        runtime.shutdown_owned();
        return false;
    }

    let Ok(mut guard) = llama_cpp_runtime.lock() else {
        runtime.shutdown_owned();
        return false;
    };

    if response_profile_switch_generation.load(Ordering::SeqCst) != expected_generation {
        runtime.shutdown_owned();
        return false;
    }

    *guard = Some(runtime);

    if response_profile_switch_generation.load(Ordering::SeqCst) != expected_generation {
        if let Some(mut stale_runtime) = guard.take() {
            stale_runtime.shutdown_owned();
        }
        return false;
    }

    true
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let builder = tauri::Builder::default()
        .setup(|app| {
            let app_state = build_app_state(app.handle());
            let completion_config = app_state
                .runtime_config
                .as_ref()
                .and_then(|config| config.completion.clone());
            if let Some(config) = app_state.runtime_config.as_ref() {
                if let Some(opencode) = config.opencode.as_ref() {
                    match tauri::async_runtime::block_on(
                        voxgolem_platform::opencode::OpencodeServer::start(
                            voxgolem_platform::opencode::OpencodeServerConfig::new(&opencode.path),
                        ),
                    ) {
                        Ok(server) => {
                            *app_state
                                .opencode_server
                                .lock()
                                .expect("opencode server lock") = Some(server)
                        }
                        Err(error) => {
                            fail_startup_capability(
                                &app_state.startup_state,
                                "opencode",
                                format!("failed to start OpenCode server: {error}"),
                            );
                        }
                    }
                }
            }
            app.manage(app_state);
            #[cfg(target_os = "linux")]
            configure_linux_microphone_permission(app)?;
            if let Some(config) = completion_config {
                let app_handle = app.handle().clone();
                tauri::async_runtime::spawn(async move {
                    match voxgolem_platform::completion::CompletionRuntime::start_with_policy(
                        &config.server_path,
                        &config.model_path,
                        platform_inference_policy(config.inference_provider),
                    )
                    .await
                    {
                        Ok(runtime) => {
                            let actual_provider =
                                actual_inference_provider_name(runtime.actual_provider());
                            let predictor = runtime.client().predictor();
                            let request = predictor.request_handle();
                            let context = {
                                let state = app_handle.state::<AppState>();
                                if let Some(mut runtime) = store_completion_runtime(
                                    &state.completion_lifecycle_lock,
                                    &state.completion_runtime,
                                    &state.completion_request,
                                    &state.exit_cleanup_started,
                                    runtime,
                                    request,
                                ) {
                                    let _ = runtime.shutdown().await;
                                    return;
                                }
                                Arc::clone(&state.completion_context)
                            };
                            set_startup_capability_provider(
                                &app_handle.state::<AppState>().startup_state,
                                "qwen_prediction",
                                actual_provider,
                            );
                            emit_completion_predictions(app_handle, predictor, context).await;
                        }
                        Err(error) => fail_startup_capability(
                            &app_handle.state::<AppState>().startup_state,
                            "qwen_prediction",
                            format!("failed to start completion runtime: {error}"),
                        ),
                    }
                });
            }
            eprintln!("{STARTUP_READY_MARKER}");
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_startup_state,
            set_tts_enabled,
            synthesize_local_tts,
            get_ui_text_size,
            set_ui_text_size,
            get_ui_theme,
            set_ui_theme,
            get_assistant_settings,
            set_assistant_settings,
            switch_response_profile,
            record_frontend_runtime_diagnostic,
            submit_prompt,
            cancel_prompt,
            record_speech_activity,
            ingest_audio_frame,
            mark_silence,
            reset_session,
            request_completion,
            clear_completion
        ]);

    let app = match builder.build(tauri::generate_context!()) {
        Ok(app) => app,
        Err(error) => {
            eprintln!("failed to build vox-golem tauri shell: {error}");
            std::process::exit(1);
        }
    };

    #[cfg(unix)]
    install_unix_signal_exit_handlers(app.handle());

    app.run(|app_handle, event| {
        if matches!(event, tauri::RunEvent::Exit) {
            let app_state = app_handle.state::<AppState>();
            if !app_state.exit_cleanup_started.swap(true, Ordering::SeqCst) {
                shutdown_prefetch_for_exit(&app_state);
                shutdown_llama_startups_for_exit(&app_state);
                shutdown_llama_cpp_runtime_for_exit(&app_state);
                shutdown_completion_runtime_for_exit(&app_state);
            }
            let opencode_server = app_state
                .opencode_server
                .lock()
                .expect("opencode server lock")
                .take();
            if let Some(server) = opencode_server {
                let _ = tauri::async_runtime::block_on(server.shutdown());
            }
        }
    });
}

fn allows_user_media(audio: bool, video: bool) -> bool {
    audio && !video
}

fn allows_user_media_from_uri(uri: Option<&str>, audio: bool, video: bool) -> bool {
    let trusted = uri.is_some_and(is_trusted_user_media_origin);
    trusted && allows_user_media(audio, video)
}

fn is_trusted_user_media_origin(uri: &str) -> bool {
    let (scheme, rest) = uri.split_once("://").unwrap_or(("", ""));
    let (authority, _) = rest.split_once(['/', '?', '#']).unwrap_or((rest, ""));
    if authority.contains('@') {
        return false;
    }
    match scheme {
        "tauri" => authority == "localhost",
        "http" => cfg!(debug_assertions) && authority == "localhost:5173",
        _ => false,
    }
}

fn shutdown_llama_startups_for_exit(app_state: &AppState) {
    let startups = app_state
        .llama_startups
        .lock()
        .expect("llama startup lock")
        .drain(..)
        .collect::<Vec<_>>();
    for (cancellation, worker) in startups {
        cancellation.cancel();
        let _ = worker.join();
    }
}

#[cfg(unix)]
fn install_unix_signal_exit_handlers(app_handle: &tauri::AppHandle) {
    for kind in [
        tokio::signal::unix::SignalKind::terminate(),
        tokio::signal::unix::SignalKind::interrupt(),
        tokio::signal::unix::SignalKind::hangup(),
        tokio::signal::unix::SignalKind::quit(),
    ] {
        let app_handle = app_handle.clone();
        tauri::async_runtime::spawn(async move {
            let Ok(mut signals) = tokio::signal::unix::signal(kind) else {
                return;
            };
            if signals.recv().await.is_some() {
                app_handle.exit(0);
            }
        });
    }
}

fn register_llama_startup(
    registry: &LlamaStartupRegistry,
    cancellation: voxgolem_platform::llama_cpp::LlamaCppStartupCancellation,
    worker: std::thread::JoinHandle<()>,
) {
    let mut guard = registry.lock().expect("llama startup lock");
    let mut finished = Vec::new();
    let mut pending = Vec::new();
    std::mem::swap(&mut *guard, &mut pending);
    for entry in pending {
        if entry.1.is_finished() {
            finished.push(entry);
        } else {
            guard.push(entry);
        }
    }
    drop(guard);
    for (_, handle) in finished {
        let _ = handle.join();
    }
    let mut guard = registry.lock().expect("llama startup lock");
    guard.push((cancellation, worker));
}

#[cfg(target_os = "linux")]
fn configure_linux_microphone_permission(app: &mut tauri::App) -> tauri::Result<()> {
    if let Some(webview_window) = app.get_webview_window("main") {
        webview_window.with_webview(|webview| {
            webview
                .inner()
                .connect_permission_request(|webview, request| {
                    let Some(user_media_request) =
                        request.downcast_ref::<UserMediaPermissionRequest>()
                    else {
                        return false;
                    };
                    let audio = user_media_request.is_for_audio_device();
                    let video = user_media_request.is_for_video_device();
                    if allows_user_media_from_uri(webview.uri().as_deref(), audio, video) {
                        request.allow();
                    } else {
                        request.deny();
                    }
                    true
                });
        })?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        allows_user_media_from_uri, apply_optional_speech_activity, assistant_completion_enabled,
        bounded_provider_history, build_llama_prompt_input, build_mark_silence_response,
        build_startup_error_app_state, current_runtime_phase_response, current_silence_deadline,
        default_response_profile, default_voice_pipeline_config, execute_prompt_backend,
        ingest_audio_frame_with_optional_wake_word_detection, is_llama_context_overflow_error,
        llama_cpp_input_token_limit, load_llama_cpp_system_prompt, load_persisted_state,
        load_persisted_tts_enabled, load_persisted_ui_text_size, load_persisted_ui_theme,
        model_path_for_profile, parse_deep_agent_json, parse_persisted_state,
        parse_review_agent_json, persist_assistant_settings, persist_selected_response_profile,
        persist_tts_enabled, persist_ui_text_size, persist_ui_theme, prefetch_supported_for_model,
        process_wake_word_frame, reset_voice_pipeline_to_waiting, reset_wake_word_runtime,
        resolve_effective_tts_enabled, response_profile_state_path, runtime_log_path,
        runtime_phase_response_from_state, shutdown_llama_cpp_runtime_for_exit,
        supported_response_profiles, synchronize_local_instant_model_with,
        take_and_invalidate_prefetch, to_runtime_phase_payload, transcribe_finished_utterance,
        transcription_ready_samples, validate_prompt_request_id, validate_prompt_text,
        wake_word_event_timestamp, AgentChoicePayload, AssistantSettingsPayload,
        InstantChoicePayload, LlamaConversationTurn, PrefetchEntry, PrefetchKey,
        PromptEventEnvelope, PromptExecutionEventPayload, ResponseProfilePayload,
        RuntimePhasePayload, RuntimePhaseResponsePayload, RuntimeTelemetryPayload,
        UiTextSizePayload, UiThemePayload, DEFAULT_SILENCE_TIMEOUT_MS, LLAMA_CPP_ROLLOVER_REASON,
        PROMPT_MAX_BYTES, PROVIDER_HISTORY_MAX_BYTES,
    };

    #[test]
    fn user_media_policy_allows_audio_only() {
        assert!(super::allows_user_media(true, false));
        assert!(!super::allows_user_media(true, true));
        assert!(!super::allows_user_media(false, true));
        assert!(!super::allows_user_media(false, false));
        assert!(super::allows_user_media_from_uri(
            Some("tauri://localhost"),
            true,
            false
        ));
        assert!(!super::allows_user_media_from_uri(
            Some("https://evil.example"),
            true,
            false
        ));
        assert!(!super::allows_user_media_from_uri(None, true, false));
        assert!(!super::allows_user_media_from_uri(
            Some("tauri://localhost"),
            true,
            true
        ));
    }

    #[test]
    fn assistant_runtime_controls_follow_persisted_preferences() {
        let disabled = AssistantSettingsPayload {
            completion: false,
            ..AssistantSettingsPayload::default()
        };
        let coordinator = std::sync::Mutex::new(
            voxgolem_core::assistant::AssistantCoordinator::new(disabled.into()),
        );
        let settings_generation = std::sync::atomic::AtomicU64::new(0);
        assert!(!assistant_completion_enabled(&coordinator).unwrap());

        let mut persisted = None;
        synchronize_local_instant_model_with(
            &coordinator,
            &settings_generation,
            0,
            ResponseProfilePayload::Quality,
            |profile, settings| {
                persisted = Some((profile, settings));
                Ok(())
            },
        )
        .unwrap()
        .unwrap();
        let (profile, settings) = persisted.unwrap();
        assert_eq!(profile, ResponseProfilePayload::Quality);
        assert_eq!(settings.instant, InstantChoicePayload::LocalQuality);
        assert!(!settings.completion);
        assert!(synchronize_local_instant_model_with(
            &coordinator,
            &settings_generation,
            0,
            ResponseProfilePayload::Fast,
            |_, _| panic!("stale profile must not persist"),
        )
        .unwrap()
        .is_none());

        assert!(!prefetch_supported_for_model(
            voxgolem_core::assistant::InstantModel::LocalFast
        ));
        assert!(prefetch_supported_for_model(
            voxgolem_core::assistant::InstantModel::CustomLunaLow
        ));
    }

    #[test]
    fn user_media_policy_matches_only_trusted_origins() {
        assert!(allows_user_media_from_uri(
            Some("tauri://localhost/route?q=1#f"),
            true,
            false
        ));
        assert!(
            cfg!(debug_assertions)
                == allows_user_media_from_uri(Some("http://localhost:5173/nested"), true, false)
        );
        for uri in [
            "tauri://localhost.evil/",
            "tauri://user@localhost/",
            "tauri://localhost:443/",
            "https://localhost/",
            "http://localhost:5174/",
        ] {
            assert!(!allows_user_media_from_uri(Some(uri), true, false), "{uri}");
        }
    }
    use crate::wake_word::{WakeWordDetection, WakeWordRuntime};
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Mutex};
    use std::thread;

    static APPDATA_ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn prompt_request_ids_must_be_opaque_generated_identifiers() {
        assert!(validate_prompt_request_id("request-1721419200000-k3jf91").is_ok());
        assert!(validate_prompt_request_id("request-user-prompt-text").is_err());
        assert!(validate_prompt_request_id("release-notes").is_err());
    }

    #[test]
    fn assistant_settings_use_pinned_opencode_wire_names() {
        let settings = AssistantSettingsPayload {
            instant: InstantChoicePayload::OpenCodeSolHigh,
            deep: AgentChoicePayload::OpenCodeLunaLow,
            review: AgentChoicePayload::OpenCodeSolHigh,
            ..AssistantSettingsPayload::default()
        };

        let wire = serde_json::to_value(settings).expect("assistant settings should serialize");

        assert_eq!(wire["instant"], "opencode-sol-high");
        assert_eq!(wire["deep"], "opencode-luna-low");
        assert_eq!(wire["review"], "opencode-sol-high");
    }

    #[test]
    fn prompt_text_is_bounded_before_provider_dispatch() {
        assert!(validate_prompt_text("x".repeat(PROMPT_MAX_BYTES)).is_ok());
        assert!(validate_prompt_text("x".repeat(PROMPT_MAX_BYTES + 1)).is_err());
    }

    #[test]
    fn provider_history_keeps_only_complete_recent_pairs_within_budget() {
        use voxgolem_core::assistant::{Content, ConversationTurn, Role};

        let text = "x".repeat(PROVIDER_HISTORY_MAX_BYTES / 3);
        let history = vec![
            ConversationTurn {
                role: Role::User,
                content: Content::Text(format!("old-user-{text}")),
            },
            ConversationTurn {
                role: Role::Assistant,
                content: Content::Text(format!("old-assistant-{text}")),
            },
            ConversationTurn {
                role: Role::User,
                content: Content::Text(format!("new-user-{text}")),
            },
            ConversationTurn {
                role: Role::Assistant,
                content: Content::Text(format!("new-assistant-{text}")),
            },
        ];

        let bounded = bounded_provider_history(&history);

        assert_eq!(bounded, &history[2..]);
    }

    #[test]
    fn prefetch_promotes_only_an_exact_prompt_history_and_model_match() {
        let key = PrefetchKey {
            prompt: String::from("explain ownership"),
            history: Vec::new(),
            model: voxgolem_core::assistant::InstantModel::LocalFast,
        };
        let cache = Mutex::new(Some(PrefetchEntry {
            generation: 1,
            key: key.clone(),
            answer: String::from("Ownership controls resource lifetime."),
        }));
        let generation = std::sync::atomic::AtomicU64::new(1);
        let mut mismatch = key.clone();
        mismatch.prompt.push('?');
        assert_eq!(
            take_and_invalidate_prefetch(&cache, &generation, &mismatch).unwrap(),
            None
        );
        assert!(cache.lock().unwrap().is_none());

        *cache.lock().unwrap() = Some(PrefetchEntry {
            generation: 2,
            key: key.clone(),
            answer: String::from("Ownership controls resource lifetime."),
        });
        assert_eq!(
            take_and_invalidate_prefetch(&cache, &generation, &key)
                .unwrap()
                .as_deref(),
            Some("Ownership controls resource lifetime.")
        );
        assert!(cache.lock().unwrap().is_none());
    }

    #[test]
    fn shutdown_llama_cpp_runtime_for_exit_invalidates_pending_startups_behavior() {
        let app_state = build_startup_error_app_state(
            default_voice_pipeline_config(),
            String::from("startup failed"),
        );

        shutdown_llama_cpp_runtime_for_exit(&app_state);

        assert_eq!(
            app_state
                .response_profile_switch_generation
                .load(std::sync::atomic::Ordering::SeqCst),
            1
        );
        assert!(app_state
            .llama_cpp_runtime
            .lock()
            .expect("runtime lock should not be poisoned")
            .is_none());
    }

    #[test]
    fn prompt_event_envelope_serializes_flattened_stream_contract() {
        let payload = serde_json::to_value(PromptEventEnvelope {
            request_id: String::from("request-7"),
            event: PromptExecutionEventPayload::Tool {
                tool: String::from("bash"),
                status: String::from("running"),
                detail: String::from("Checking status"),
            },
        })
        .expect("prompt event should serialize");

        assert_eq!(payload["request_id"], "request-7");
        assert_eq!(payload["kind"], "tool");
        assert_eq!(payload["tool"], "bash");
        assert_eq!(payload["status"], "running");
        assert_eq!(payload["detail"], "Checking status");
    }

    #[test]
    fn main_capability_grants_only_event_listener_permissions() {
        let capability: serde_json::Value =
            serde_json::from_str(include_str!("../capabilities/main.json"))
                .expect("main capability should contain valid JSON");

        assert_eq!(capability["identifier"], "main");
        assert_eq!(capability["windows"], serde_json::json!(["main"]));
        assert_eq!(
            capability["permissions"],
            serde_json::json!(["core:event:allow-listen", "core:event:allow-unlisten"])
        );
    }

    #[test]
    fn prompt_reset_waits_for_active_prompt_completion() {
        let cancelled = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let cancellation_signal = Arc::new(tokio::sync::Notify::new());
        let completion_signal = Arc::new(tokio::sync::Notify::new());

        tauri::async_runtime::block_on(async {
            let observer = tauri::async_runtime::spawn({
                let cancelled = Arc::clone(&cancelled);
                let cancellation_signal = Arc::clone(&cancellation_signal);
                let completion_signal = Arc::clone(&completion_signal);
                async move {
                    cancellation_signal.notified().await;
                    assert!(cancelled.load(std::sync::atomic::Ordering::SeqCst));
                    completion_signal.notify_one();
                }
            });

            super::cancel_and_wait_for_prompt(&cancelled, &cancellation_signal, &completion_signal)
                .await
                .expect("reset should observe prompt completion");
            observer.await.expect("observer task should finish");
        });
    }

    #[test]
    fn execute_prompt_backend_uses_local_llama_runtime_for_fast_backend() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("listener should bind");
        let port = listener
            .local_addr()
            .expect("listener address should exist")
            .port();

        let server_thread = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("request should connect");
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(2)))
                .expect("read timeout should be configurable");
            let mut request = Vec::new();
            let mut buffer = [0_u8; 1024];

            loop {
                let read_len = stream
                    .read(&mut buffer)
                    .expect("request should be readable");
                if read_len == 0 {
                    break;
                }

                request.extend_from_slice(&buffer[..read_len]);
                if String::from_utf8_lossy(&request).contains("\"model\":\"default\"") {
                    break;
                }
            }

            let request_text = String::from_utf8_lossy(&request);

            assert!(request_text.starts_with("POST /v1/chat/completions HTTP/1.1"));
            assert!(request_text.contains("\"model\":\"default\""));
            assert!(request_text.contains("say hi"));

            let body = "{\"choices\":[{\"message\":{\"content\":\"Local Gemma says hi\"}}]}";
            let response = format!(
                "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nContent-Type: application/json\r\n\r\n{:X}\r\n{}\r\n0\r\n\r\n",
                body.len(),
                body,
            );

            stream
                .write_all(response.as_bytes())
                .expect("response should be writable");
        });

        let config = voxgolem_core::config::RuntimeConfig {
            wake_word_model_path: PathBuf::from("wake.onnx"),
            parakeet_model_dir: PathBuf::from("parakeet"),
            silero_vad_model: PathBuf::from("vad.onnx"),
            silence_timeout_ms: 1_500,
            wake_word_detection_threshold: 0.68,
            local_tts: voxgolem_core::config::LocalTtsConfig {
                enabled: false,
                model_path: PathBuf::from("models/tts/jarvis.onnx"),
                worker_count: 1,
                max_queue: 8,
                sample_rate_hz: 22_050,
                max_duration_s: 300,
                output_gain_db: 3.0,
            },
            logging: voxgolem_core::config::LoggingConfig { enabled: false },
            telemetry: voxgolem_core::config::TelemetryConfig {
                enabled: false,
                max_bytes: 1024,
                backup_count: 0,
            },
            opencode: None,
            llama_cpp: None,
            custom_openai: None,
            completion: None,
            capability_issues: Vec::new(),
            response_backend: voxgolem_core::config::ResponseBackendConfig::LlamaCpp {
                server_path: PathBuf::from("llama-server.exe"),
                host: String::from("127.0.0.1"),
                port,
                fast_model_path: PathBuf::from("fast.gguf"),
                quality_model_path: None,
            },
        };
        let runtime = Arc::new(Mutex::new(Some(
            voxgolem_platform::llama_cpp::LlamaCppRuntime::attach(
                voxgolem_platform::llama_cpp::LlamaCppServerSpec::new(
                    "llama-server.exe",
                    "fast.gguf",
                    "127.0.0.1",
                    port,
                    "default",
                ),
            ),
        )));
        let conversation = Mutex::new(Vec::<LlamaConversationTurn>::new());

        let outcome = execute_prompt_backend(
            &config,
            "say hi",
            &runtime,
            &conversation,
            Some("You are JARVIS."),
        )
        .expect("local backend should succeed");

        server_thread.join().expect("server thread should complete");

        assert_eq!(outcome.stderr, "");
        assert_eq!(outcome.exit_code, None);
        assert_eq!(outcome.error_message, None);
        assert_eq!(outcome.events.len(), 1);
        assert!(matches!(
            &outcome.events[0],
            super::PromptExecutionEventPayload::Text { text } if text == "Local Gemma says hi"
        ));
    }

    #[test]
    fn execute_prompt_backend_reports_warming_error_when_llama_runtime_is_unavailable() {
        let config = voxgolem_core::config::RuntimeConfig {
            wake_word_model_path: PathBuf::from("wake.onnx"),
            parakeet_model_dir: PathBuf::from("parakeet"),
            silero_vad_model: PathBuf::from("vad.onnx"),
            silence_timeout_ms: 1_500,
            wake_word_detection_threshold: 0.68,
            local_tts: voxgolem_core::config::LocalTtsConfig {
                enabled: false,
                model_path: PathBuf::from("models/tts/jarvis.onnx"),
                worker_count: 1,
                max_queue: 8,
                sample_rate_hz: 22_050,
                max_duration_s: 300,
                output_gain_db: 3.0,
            },
            logging: voxgolem_core::config::LoggingConfig { enabled: false },
            telemetry: voxgolem_core::config::TelemetryConfig {
                enabled: false,
                max_bytes: 1024,
                backup_count: 0,
            },
            opencode: None,
            llama_cpp: None,
            custom_openai: None,
            completion: None,
            capability_issues: Vec::new(),
            response_backend: voxgolem_core::config::ResponseBackendConfig::LlamaCpp {
                server_path: PathBuf::from("llama-server.exe"),
                host: String::from("127.0.0.1"),
                port: 11_435,
                fast_model_path: PathBuf::from("fast.gguf"),
                quality_model_path: None,
            },
        };
        let runtime = Arc::new(Mutex::new(None));
        let conversation = Mutex::new(Vec::<LlamaConversationTurn>::new());

        assert!(matches!(
            execute_prompt_backend(
                &config,
                "say hi",
                &runtime,
                &conversation,
                Some("You are JARVIS."),
            ),
            Err(message) if message == "local Gemma model is still warming up"
        ));
    }

    #[test]
    fn execute_prompt_backend_reports_missing_soul_prompt_for_llama_backend() {
        let config = voxgolem_core::config::RuntimeConfig {
            wake_word_model_path: PathBuf::from("wake.onnx"),
            parakeet_model_dir: PathBuf::from("parakeet"),
            silero_vad_model: PathBuf::from("vad.onnx"),
            silence_timeout_ms: 1_500,
            wake_word_detection_threshold: 0.68,
            local_tts: voxgolem_core::config::LocalTtsConfig {
                enabled: false,
                model_path: PathBuf::from("models/tts/jarvis.onnx"),
                worker_count: 1,
                max_queue: 8,
                sample_rate_hz: 22_050,
                max_duration_s: 300,
                output_gain_db: 3.0,
            },
            logging: voxgolem_core::config::LoggingConfig { enabled: false },
            telemetry: voxgolem_core::config::TelemetryConfig {
                enabled: false,
                max_bytes: 1024,
                backup_count: 0,
            },
            opencode: None,
            llama_cpp: None,
            custom_openai: None,
            completion: None,
            capability_issues: Vec::new(),
            response_backend: voxgolem_core::config::ResponseBackendConfig::LlamaCpp {
                server_path: PathBuf::from("llama-server.exe"),
                host: String::from("127.0.0.1"),
                port: 11_435,
                fast_model_path: PathBuf::from("fast.gguf"),
                quality_model_path: None,
            },
        };
        let runtime = Arc::new(Mutex::new(None));
        let conversation = Mutex::new(Vec::<LlamaConversationTurn>::new());

        assert!(matches!(
            execute_prompt_backend(&config, "say hi", &runtime, &conversation, None),
            Err(message) if message == "SOUL.md is not loaded"
        ));
    }

    #[test]
    fn build_llama_prompt_input_keeps_history_when_under_budget() {
        let conversation = vec![LlamaConversationTurn {
            user: "first user prompt".to_string(),
            assistant: "first assistant reply".to_string(),
        }];

        let prompt_input = build_llama_prompt_input("system", "second prompt", &conversation);

        assert!(!prompt_input.rolled_over);
        assert!(prompt_input.user_prompt.contains("Conversation so far:"));
        assert!(prompt_input.user_prompt.contains("first user prompt"));
        assert!(prompt_input.user_prompt.contains("first assistant reply"));
        assert!(prompt_input
            .user_prompt
            .contains("Current user message:\nsecond prompt"));
    }

    #[test]
    fn build_llama_prompt_input_rolls_over_when_history_exceeds_budget() {
        let oversized = "x".repeat(llama_cpp_input_token_limit() * 8);
        let conversation = vec![LlamaConversationTurn {
            user: oversized.clone(),
            assistant: oversized,
        }];

        let prompt_input = build_llama_prompt_input("system", "fresh prompt", &conversation);

        assert!(prompt_input.rolled_over);
        assert_eq!(prompt_input.user_prompt, "fresh prompt");
    }

    #[test]
    fn is_llama_context_overflow_error_detects_window_overflow_messages() {
        assert!(is_llama_context_overflow_error(
            "status 400: context window exceeded"
        ));
    }

    #[test]
    fn is_llama_context_overflow_error_rejects_non_overflow_context_messages() {
        assert!(!is_llama_context_overflow_error(
            "status 400: context serialization failed"
        ));
    }

    #[test]
    fn execute_prompt_backend_rolls_over_history_and_emits_reasoning_event() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("listener should bind");
        let port = listener
            .local_addr()
            .expect("listener address should exist")
            .port();

        let server_thread = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("request should connect");
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(2)))
                .expect("read timeout should be configurable");
            let mut request = Vec::new();
            let mut buffer = [0_u8; 1024];

            loop {
                let read_len = stream
                    .read(&mut buffer)
                    .expect("request should be readable");
                if read_len == 0 {
                    break;
                }

                request.extend_from_slice(&buffer[..read_len]);
                if String::from_utf8_lossy(&request).contains("\"model\":\"default\"") {
                    break;
                }
            }

            let body = "{\"choices\":[{\"message\":{\"content\":\"Local Gemma says hi\"}}]}";
            let response = format!(
                "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nContent-Type: application/json\r\n\r\n{:X}\r\n{}\r\n0\r\n\r\n",
                body.len(),
                body,
            );

            stream
                .write_all(response.as_bytes())
                .expect("response should be writable");
        });

        let config = voxgolem_core::config::RuntimeConfig {
            wake_word_model_path: PathBuf::from("wake.onnx"),
            parakeet_model_dir: PathBuf::from("parakeet"),
            silero_vad_model: PathBuf::from("vad.onnx"),
            silence_timeout_ms: 1_500,
            wake_word_detection_threshold: 0.68,
            local_tts: voxgolem_core::config::LocalTtsConfig {
                enabled: false,
                model_path: PathBuf::from("models/tts/jarvis.onnx"),
                worker_count: 1,
                max_queue: 8,
                sample_rate_hz: 22_050,
                max_duration_s: 300,
                output_gain_db: 3.0,
            },
            logging: voxgolem_core::config::LoggingConfig { enabled: false },
            telemetry: voxgolem_core::config::TelemetryConfig {
                enabled: false,
                max_bytes: 1024,
                backup_count: 0,
            },
            opencode: None,
            llama_cpp: None,
            custom_openai: None,
            completion: None,
            capability_issues: Vec::new(),
            response_backend: voxgolem_core::config::ResponseBackendConfig::LlamaCpp {
                server_path: PathBuf::from("llama-server.exe"),
                host: String::from("127.0.0.1"),
                port,
                fast_model_path: PathBuf::from("fast.gguf"),
                quality_model_path: None,
            },
        };
        let runtime = Arc::new(Mutex::new(Some(
            voxgolem_platform::llama_cpp::LlamaCppRuntime::attach(
                voxgolem_platform::llama_cpp::LlamaCppServerSpec::new(
                    "llama-server.exe",
                    "fast.gguf",
                    "127.0.0.1",
                    port,
                    "default",
                ),
            ),
        )));
        let oversized = "y".repeat(llama_cpp_input_token_limit() * 8);
        let conversation = Mutex::new(vec![LlamaConversationTurn {
            user: oversized.clone(),
            assistant: oversized,
        }]);

        let outcome = execute_prompt_backend(
            &config,
            "say hi",
            &runtime,
            &conversation,
            Some("You are JARVIS."),
        )
        .expect("local backend should succeed");

        server_thread.join().expect("server thread should complete");

        assert_eq!(outcome.events.len(), 2);
        assert!(matches!(
            &outcome.events[0],
            PromptExecutionEventPayload::Reasoning { text }
                if text == LLAMA_CPP_ROLLOVER_REASON
        ));
        assert!(matches!(
            &outcome.events[1],
            PromptExecutionEventPayload::Text { text } if text == "Local Gemma says hi"
        ));

        let conversation = conversation
            .lock()
            .expect("conversation lock should not be poisoned");
        assert_eq!(conversation.len(), 1);
        assert_eq!(conversation[0].user, "say hi");
        assert_eq!(conversation[0].assistant, "Local Gemma says hi");
    }

    #[test]
    fn execute_prompt_backend_retries_with_reset_after_context_overflow() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("listener should bind");
        let port = listener
            .local_addr()
            .expect("listener address should exist")
            .port();

        let server_thread = thread::spawn(move || {
            let mut attempt = 0;
            while attempt < 2 {
                let (mut stream, _) = listener.accept().expect("request should connect");
                stream
                    .set_read_timeout(Some(std::time::Duration::from_secs(2)))
                    .expect("read timeout should be configurable");
                let mut request = Vec::new();
                let mut buffer = [0_u8; 1024];

                loop {
                    let read_len = stream
                        .read(&mut buffer)
                        .expect("request should be readable");
                    if read_len == 0 {
                        break;
                    }

                    request.extend_from_slice(&buffer[..read_len]);
                    if String::from_utf8_lossy(&request).contains("\"model\":\"default\"") {
                        break;
                    }
                }

                let request_text = String::from_utf8_lossy(&request);
                if attempt == 0 {
                    assert!(request_text.contains("Conversation so far:"));
                    let body =
                        "{\"error\":{\"message\":\"context window exceeded for this prompt\"}}";
                    let response = format!(
                        "HTTP/1.1 400 Bad Request\r\nTransfer-Encoding: chunked\r\nContent-Type: application/json\r\n\r\n{:X}\r\n{}\r\n0\r\n\r\n",
                        body.len(),
                        body,
                    );
                    stream
                        .write_all(response.as_bytes())
                        .expect("error response should be writable");
                } else {
                    assert!(!request_text.contains("Conversation so far:"));
                    assert!(request_text.contains("say hi"));
                    let body = "{\"choices\":[{\"message\":{\"content\":\"Recovered response\"}}]}";
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nContent-Type: application/json\r\n\r\n{:X}\r\n{}\r\n0\r\n\r\n",
                        body.len(),
                        body,
                    );
                    stream
                        .write_all(response.as_bytes())
                        .expect("success response should be writable");
                }

                attempt += 1;
            }
        });

        let config = voxgolem_core::config::RuntimeConfig {
            wake_word_model_path: PathBuf::from("wake.onnx"),
            parakeet_model_dir: PathBuf::from("parakeet"),
            silero_vad_model: PathBuf::from("vad.onnx"),
            silence_timeout_ms: 1_500,
            wake_word_detection_threshold: 0.68,
            local_tts: voxgolem_core::config::LocalTtsConfig {
                enabled: false,
                model_path: PathBuf::from("models/tts/jarvis.onnx"),
                worker_count: 1,
                max_queue: 8,
                sample_rate_hz: 22_050,
                max_duration_s: 300,
                output_gain_db: 3.0,
            },
            logging: voxgolem_core::config::LoggingConfig { enabled: false },
            telemetry: voxgolem_core::config::TelemetryConfig {
                enabled: false,
                max_bytes: 1024,
                backup_count: 0,
            },
            opencode: None,
            llama_cpp: None,
            custom_openai: None,
            completion: None,
            capability_issues: Vec::new(),
            response_backend: voxgolem_core::config::ResponseBackendConfig::LlamaCpp {
                server_path: PathBuf::from("llama-server.exe"),
                host: String::from("127.0.0.1"),
                port,
                fast_model_path: PathBuf::from("fast.gguf"),
                quality_model_path: None,
            },
        };
        let runtime = Arc::new(Mutex::new(Some(
            voxgolem_platform::llama_cpp::LlamaCppRuntime::attach(
                voxgolem_platform::llama_cpp::LlamaCppServerSpec::new(
                    "llama-server.exe",
                    "fast.gguf",
                    "127.0.0.1",
                    port,
                    "default",
                ),
            ),
        )));
        let conversation = Mutex::new(vec![LlamaConversationTurn {
            user: "prior turn".to_string(),
            assistant: "prior answer".to_string(),
        }]);

        let outcome = execute_prompt_backend(
            &config,
            "say hi",
            &runtime,
            &conversation,
            Some("You are JARVIS."),
        )
        .expect("local backend should succeed after retry");

        server_thread.join().expect("server thread should complete");

        assert_eq!(outcome.events.len(), 2);
        assert!(matches!(
            &outcome.events[0],
            PromptExecutionEventPayload::Reasoning { text }
                if text == LLAMA_CPP_ROLLOVER_REASON
        ));
        assert!(matches!(
            &outcome.events[1],
            PromptExecutionEventPayload::Text { text } if text == "Recovered response"
        ));

        let conversation = conversation
            .lock()
            .expect("conversation lock should not be poisoned");
        assert_eq!(conversation.len(), 1);
        assert_eq!(conversation[0].user, "say hi");
        assert_eq!(conversation[0].assistant, "Recovered response");
    }

    #[test]
    fn execute_prompt_backend_does_not_retry_on_non_overflow_context_errors() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("listener should bind");
        let port = listener
            .local_addr()
            .expect("listener address should exist")
            .port();

        let server_thread = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("request should connect");
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(2)))
                .expect("read timeout should be configurable");
            let mut request = Vec::new();
            let mut buffer = [0_u8; 1024];

            loop {
                let read_len = stream
                    .read(&mut buffer)
                    .expect("request should be readable");
                if read_len == 0 {
                    break;
                }

                request.extend_from_slice(&buffer[..read_len]);
                if String::from_utf8_lossy(&request).contains("\"model\":\"default\"") {
                    break;
                }
            }

            let body = "{\"error\":{\"message\":\"context serialization failed\"}}";
            let response = format!(
                "HTTP/1.1 400 Bad Request\r\nTransfer-Encoding: chunked\r\nContent-Type: application/json\r\n\r\n{:X}\r\n{}\r\n0\r\n\r\n",
                body.len(),
                body,
            );

            stream
                .write_all(response.as_bytes())
                .expect("error response should be writable");
        });

        let config = voxgolem_core::config::RuntimeConfig {
            wake_word_model_path: PathBuf::from("wake.onnx"),
            parakeet_model_dir: PathBuf::from("parakeet"),
            silero_vad_model: PathBuf::from("vad.onnx"),
            silence_timeout_ms: 1_500,
            wake_word_detection_threshold: 0.68,
            local_tts: voxgolem_core::config::LocalTtsConfig {
                enabled: false,
                model_path: PathBuf::from("models/tts/jarvis.onnx"),
                worker_count: 1,
                max_queue: 8,
                sample_rate_hz: 22_050,
                max_duration_s: 300,
                output_gain_db: 3.0,
            },
            logging: voxgolem_core::config::LoggingConfig { enabled: false },
            telemetry: voxgolem_core::config::TelemetryConfig {
                enabled: false,
                max_bytes: 1024,
                backup_count: 0,
            },
            opencode: None,
            llama_cpp: None,
            custom_openai: None,
            completion: None,
            capability_issues: Vec::new(),
            response_backend: voxgolem_core::config::ResponseBackendConfig::LlamaCpp {
                server_path: PathBuf::from("llama-server.exe"),
                host: String::from("127.0.0.1"),
                port,
                fast_model_path: PathBuf::from("fast.gguf"),
                quality_model_path: None,
            },
        };
        let runtime = Arc::new(Mutex::new(Some(
            voxgolem_platform::llama_cpp::LlamaCppRuntime::attach(
                voxgolem_platform::llama_cpp::LlamaCppServerSpec::new(
                    "llama-server.exe",
                    "fast.gguf",
                    "127.0.0.1",
                    port,
                    "default",
                ),
            ),
        )));
        let conversation = Mutex::new(vec![LlamaConversationTurn {
            user: "prior turn".to_string(),
            assistant: "prior answer".to_string(),
        }]);

        let outcome = execute_prompt_backend(
            &config,
            "say hi",
            &runtime,
            &conversation,
            Some("You are JARVIS."),
        );

        server_thread.join().expect("server thread should complete");

        assert!(matches!(
            outcome,
            Err(message)
                if message.contains("failed to execute local llama.cpp prompt")
                    && !message.contains("after conversation reset")
        ));

        let conversation = conversation
            .lock()
            .expect("conversation lock should not be poisoned");
        assert_eq!(conversation.len(), 1);
        assert_eq!(conversation[0].user, "prior turn");
        assert_eq!(conversation[0].assistant, "prior answer");
    }

    #[test]
    fn load_llama_cpp_system_prompt_reads_soul_file_from_appdata() {
        let _appdata_lock = APPDATA_ENV_LOCK
            .lock()
            .expect("APPDATA test lock should not be poisoned");
        let temp_dir = tempfile::tempdir().expect("temp dir should be created");
        let appdata_path = temp_dir.path().join("VoxGolem");
        std::fs::create_dir_all(&appdata_path).expect("appdata path should be creatable");
        std::fs::write(
            appdata_path.join("SOUL.md"),
            "  You are JARVIS, concise and precise.  ",
        )
        .expect("SOUL.md should be writable");

        let previous_appdata = std::env::var_os("APPDATA");
        std::env::set_var("APPDATA", temp_dir.path());

        let result = load_llama_cpp_system_prompt();

        match previous_appdata {
            Some(value) => std::env::set_var("APPDATA", value),
            None => std::env::remove_var("APPDATA"),
        }

        assert_eq!(
            result.expect("SOUL.md should load"),
            "You are JARVIS, concise and precise."
        );
    }

    #[test]
    fn supported_response_profiles_includes_quality_when_configured() {
        let profiles =
            supported_response_profiles(&voxgolem_core::config::ResponseBackendConfig::LlamaCpp {
                server_path: PathBuf::from("llama-server.exe"),
                host: String::from("127.0.0.1"),
                port: 11_435,
                fast_model_path: PathBuf::from("fast.gguf"),
                quality_model_path: Some(PathBuf::from("quality.gguf")),
            });

        assert_eq!(
            profiles,
            vec![
                ResponseProfilePayload::Fast,
                ResponseProfilePayload::Quality
            ]
        );
    }

    #[test]
    fn model_path_for_profile_rejects_quality_when_missing() {
        let result = model_path_for_profile(
            ResponseProfilePayload::Quality,
            Path::new("fast.gguf"),
            None,
        );

        assert_eq!(
            result,
            Err(String::from("response profile `quality` is not supported"))
        );
    }

    #[test]
    fn parse_persisted_state_supports_fast_and_quality() {
        assert_eq!(
            parse_persisted_state("selected_response_profile = \"fast\"\n")
                .expect("fast profile should parse")
                .selected_response_profile,
            Some(ResponseProfilePayload::Fast)
        );
        assert_eq!(
            parse_persisted_state("selected_response_profile = \"quality\"\n")
                .expect("quality profile should parse")
                .selected_response_profile,
            Some(ResponseProfilePayload::Quality)
        );
    }

    #[test]
    fn parse_persisted_state_reads_profile_and_tts_flag() {
        let persisted =
            parse_persisted_state("selected_response_profile = \"fast\"\ntts_enabled = true\n")
                .expect("state should parse");

        assert_eq!(
            persisted.selected_response_profile,
            Some(ResponseProfilePayload::Fast)
        );
        assert_eq!(persisted.tts_enabled, Some(true));
    }

    #[test]
    fn parse_persisted_state_reads_ui_text_size() {
        let persisted = parse_persisted_state(
            "selected_response_profile = \"fast\"\ntts_enabled = true\nui_text_size = \"extra_large\"\n",
        )
        .expect("state should parse");

        assert_eq!(persisted.ui_text_size, Some(UiTextSizePayload::ExtraLarge));
    }

    #[test]
    fn parse_persisted_state_reads_ui_theme() {
        let persisted = parse_persisted_state(
            "selected_response_profile = \"fast\"\ntts_enabled = true\nui_text_size = \"large\"\nui_theme = \"dark\"\n",
        )
        .expect("state should parse");

        assert_eq!(persisted.ui_theme, Some(UiThemePayload::Dark));
    }

    #[test]
    fn parse_persisted_state_reads_assistant_settings() {
        let state = parse_persisted_state(
            "assistant_instant = \"custom-luna-low\"\nassistant_deep = \"opencode-sol-high\"\nassistant_review = \"custom-sol-high\"\nassistant_deep_enabled = true\nassistant_review_enabled = false\nassistant_prefetch = true\nassistant_completion = false\n",
        )
        .expect("assistant settings should parse");

        assert_eq!(
            state.assistant_settings,
            Some(AssistantSettingsPayload {
                instant: InstantChoicePayload::CustomLunaLow,
                deep: AgentChoicePayload::OpenCodeSolHigh,
                review: AgentChoicePayload::CustomSolHigh,
                deep_enabled: true,
                review_enabled: false,
                prefetch: true,
                completion: false,
            })
        );
    }

    #[test]
    fn agent_json_parsers_reject_unknown_fields_and_accept_escaped_text() {
        assert!(parse_deep_agent_json(
            r#"{"complete_answer":"answer","voice_summary":"Done.","sources":[],"extra":true}"#,
            1,
            true,
        )
        .is_err());
        let review = parse_review_agent_json(
            r#"{"decision":"rewrite","replacement":"Use \"quoted\" text, then continue.","correction":"Correction: Use the verified value."}"#,
        )
        .expect("escaped Review JSON should parse");
        assert!(matches!(
            review.decision,
            voxgolem_core::agent_pipeline::ReviewDecision::Rewrite { replacement, .. }
                if replacement.contains("quoted")
        ));
        assert!(parse_review_agent_json(r#"{"decision":"keep","extra":true}"#).is_err());
    }

    #[test]
    fn persist_assistant_settings_preserves_legacy_profile() {
        let _appdata_lock = APPDATA_ENV_LOCK
            .lock()
            .expect("APPDATA test lock should not be poisoned");
        let temp_dir = tempfile::tempdir().expect("temp dir should be created");
        let previous_appdata = std::env::var_os("APPDATA");
        std::env::set_var("APPDATA", temp_dir.path());

        persist_selected_response_profile(ResponseProfilePayload::Quality)
            .expect("profile should persist");
        let settings = AssistantSettingsPayload {
            instant: InstantChoicePayload::OpenCodeLunaLow,
            completion: false,
            ..AssistantSettingsPayload::default()
        };
        persist_assistant_settings(settings).expect("assistant settings should persist");
        let persisted = load_persisted_state().expect("state should reload");

        match previous_appdata {
            Some(value) => std::env::set_var("APPDATA", value),
            None => std::env::remove_var("APPDATA"),
        }

        assert_eq!(
            persisted.selected_response_profile,
            Some(ResponseProfilePayload::Quality)
        );
        assert_eq!(persisted.assistant_settings, Some(settings));
    }

    #[test]
    fn parse_persisted_state_rejects_invalid_ui_text_size() {
        let result = parse_persisted_state("ui_text_size = \"giant\"\n");

        assert_eq!(
            result,
            Err(String::from(
                "invalid state.toml: unsupported ui_text_size `giant`"
            ))
        );
    }

    #[test]
    fn parse_persisted_state_rejects_invalid_ui_theme() {
        let result = parse_persisted_state("ui_theme = \"sepia\"\n");

        assert_eq!(
            result,
            Err(String::from(
                "invalid state.toml: unsupported ui_theme `sepia`"
            ))
        );
    }

    #[test]
    fn load_persisted_tts_enabled_reads_boolean_flag_from_state_file() {
        let _appdata_lock = APPDATA_ENV_LOCK
            .lock()
            .expect("APPDATA test lock should not be poisoned");
        let temp_dir = tempfile::tempdir().expect("temp dir should be created");
        let previous_appdata = std::env::var_os("APPDATA");
        std::env::set_var("APPDATA", temp_dir.path());

        persist_tts_enabled(true).expect("tts flag should be written");
        let persisted = load_persisted_tts_enabled().expect("tts flag should load");

        match previous_appdata {
            Some(value) => std::env::set_var("APPDATA", value),
            None => std::env::remove_var("APPDATA"),
        }

        assert_eq!(persisted, Some(true));
    }

    #[test]
    fn load_persisted_ui_text_size_reads_text_size_flag_from_state_file() {
        let _appdata_lock = APPDATA_ENV_LOCK
            .lock()
            .expect("APPDATA test lock should not be poisoned");
        let temp_dir = tempfile::tempdir().expect("temp dir should be created");
        let previous_appdata = std::env::var_os("APPDATA");
        std::env::set_var("APPDATA", temp_dir.path());

        persist_ui_text_size(UiTextSizePayload::Large).expect("text size should be written");
        let persisted = load_persisted_ui_text_size().expect("text size should load");

        match previous_appdata {
            Some(value) => std::env::set_var("APPDATA", value),
            None => std::env::remove_var("APPDATA"),
        }

        assert_eq!(persisted, Some(UiTextSizePayload::Large));
    }

    #[test]
    fn load_persisted_ui_theme_reads_theme_flag_from_state_file() {
        let _appdata_lock = APPDATA_ENV_LOCK
            .lock()
            .expect("APPDATA test lock should not be poisoned");
        let temp_dir = tempfile::tempdir().expect("temp dir should be created");
        let previous_appdata = std::env::var_os("APPDATA");
        std::env::set_var("APPDATA", temp_dir.path());

        persist_ui_theme(UiThemePayload::Light).expect("theme should be written");
        let persisted = load_persisted_ui_theme().expect("theme should load");

        match previous_appdata {
            Some(value) => std::env::set_var("APPDATA", value),
            None => std::env::remove_var("APPDATA"),
        }

        assert_eq!(persisted, Some(UiThemePayload::Light));
    }

    #[test]
    fn persist_selected_response_profile_preserves_tts_enabled_flag() {
        let _appdata_lock = APPDATA_ENV_LOCK
            .lock()
            .expect("APPDATA test lock should not be poisoned");
        let temp_dir = tempfile::tempdir().expect("temp dir should be created");
        let previous_appdata = std::env::var_os("APPDATA");
        std::env::set_var("APPDATA", temp_dir.path());

        persist_tts_enabled(true).expect("tts flag should be written");
        persist_selected_response_profile(ResponseProfilePayload::Quality)
            .expect("profile state should be written");

        let persisted = load_persisted_tts_enabled().expect("tts flag should load");

        match previous_appdata {
            Some(value) => std::env::set_var("APPDATA", value),
            None => std::env::remove_var("APPDATA"),
        }

        assert_eq!(persisted, Some(true));
    }

    #[test]
    fn persist_ui_text_size_preserves_profile_and_tts_enabled_flags() {
        let _appdata_lock = APPDATA_ENV_LOCK
            .lock()
            .expect("APPDATA test lock should not be poisoned");
        let temp_dir = tempfile::tempdir().expect("temp dir should be created");
        let previous_appdata = std::env::var_os("APPDATA");
        std::env::set_var("APPDATA", temp_dir.path());

        persist_selected_response_profile(ResponseProfilePayload::Quality)
            .expect("profile state should be written");
        persist_tts_enabled(true).expect("tts flag should be written");
        persist_ui_text_size(UiTextSizePayload::Small).expect("text size should be written");
        let persisted = load_persisted_state().expect("persisted state should load");

        match previous_appdata {
            Some(value) => std::env::set_var("APPDATA", value),
            None => std::env::remove_var("APPDATA"),
        }

        assert_eq!(
            persisted.selected_response_profile,
            Some(ResponseProfilePayload::Quality)
        );
        assert_eq!(persisted.tts_enabled, Some(true));
        assert_eq!(persisted.ui_text_size, Some(UiTextSizePayload::Small));
    }

    #[test]
    fn persist_ui_theme_preserves_existing_state_flags() {
        let _appdata_lock = APPDATA_ENV_LOCK
            .lock()
            .expect("APPDATA test lock should not be poisoned");
        let temp_dir = tempfile::tempdir().expect("temp dir should be created");
        let previous_appdata = std::env::var_os("APPDATA");
        std::env::set_var("APPDATA", temp_dir.path());

        persist_selected_response_profile(ResponseProfilePayload::Quality)
            .expect("profile state should be written");
        persist_tts_enabled(true).expect("tts flag should be written");
        persist_ui_text_size(UiTextSizePayload::ExtraLarge).expect("text size should be written");
        persist_ui_theme(UiThemePayload::Dark).expect("theme should be written");
        let persisted = load_persisted_state().expect("persisted state should load");

        match previous_appdata {
            Some(value) => std::env::set_var("APPDATA", value),
            None => std::env::remove_var("APPDATA"),
        }

        assert_eq!(
            persisted.selected_response_profile,
            Some(ResponseProfilePayload::Quality)
        );
        assert_eq!(persisted.tts_enabled, Some(true));
        assert_eq!(persisted.ui_text_size, Some(UiTextSizePayload::ExtraLarge));
        assert_eq!(persisted.ui_theme, Some(UiThemePayload::Dark));
    }

    #[test]
    fn resolve_effective_tts_enabled_prefers_persisted_state() {
        let _appdata_lock = APPDATA_ENV_LOCK
            .lock()
            .expect("APPDATA test lock should not be poisoned");
        let temp_dir = tempfile::tempdir().expect("temp dir should be created");
        let previous_appdata = std::env::var_os("APPDATA");
        std::env::set_var("APPDATA", temp_dir.path());

        persist_tts_enabled(true).expect("tts flag should be written");
        let effective = resolve_effective_tts_enabled(false, true);

        match previous_appdata {
            Some(value) => std::env::set_var("APPDATA", value),
            None => std::env::remove_var("APPDATA"),
        }

        assert!(effective);
    }

    #[test]
    fn resolve_effective_tts_enabled_ignores_persisted_true_without_model() {
        let _appdata_lock = APPDATA_ENV_LOCK
            .lock()
            .expect("APPDATA test lock should not be poisoned");
        let temp_dir = tempfile::tempdir().expect("temp dir should be created");
        let previous_appdata = std::env::var_os("APPDATA");
        std::env::set_var("APPDATA", temp_dir.path());

        persist_tts_enabled(true).expect("tts flag should be written");
        let effective = resolve_effective_tts_enabled(false, false);

        match previous_appdata {
            Some(value) => std::env::set_var("APPDATA", value),
            None => std::env::remove_var("APPDATA"),
        }

        assert!(!effective);
    }

    #[test]
    fn persist_selected_response_profile_writes_state_file_in_appdata() {
        let _appdata_lock = APPDATA_ENV_LOCK
            .lock()
            .expect("APPDATA test lock should not be poisoned");
        let temp_dir = tempfile::tempdir().expect("temp dir should be created");
        let previous_appdata = std::env::var_os("APPDATA");
        std::env::set_var("APPDATA", temp_dir.path());

        persist_selected_response_profile(ResponseProfilePayload::Quality)
            .expect("profile state should be written");
        let state_path = response_profile_state_path().expect("state path should resolve");
        let state_contents =
            std::fs::read_to_string(&state_path).expect("state file should be readable");

        match previous_appdata {
            Some(value) => std::env::set_var("APPDATA", value),
            None => std::env::remove_var("APPDATA"),
        }

        assert!(state_path
            .to_string_lossy()
            .replace('\\', "/")
            .ends_with("VoxGolem/state.toml"));
        assert_eq!(state_contents, "selected_response_profile = \"quality\"\n");
    }

    #[test]
    fn runtime_log_path_resolves_in_appdata_logs_directory() {
        let _appdata_lock = APPDATA_ENV_LOCK
            .lock()
            .expect("APPDATA test lock should not be poisoned");
        let temp_dir = tempfile::tempdir().expect("temp dir should be created");
        let previous_appdata = std::env::var_os("APPDATA");
        std::env::set_var("APPDATA", temp_dir.path());

        let log_path = runtime_log_path().expect("runtime log path should resolve");

        match previous_appdata {
            Some(value) => std::env::set_var("APPDATA", value),
            None => std::env::remove_var("APPDATA"),
        }

        assert!(log_path
            .to_string_lossy()
            .replace('\\', "/")
            .ends_with("VoxGolem/logs/runtime.log"));
    }

    #[test]
    fn append_tts_runtime_log_line_writes_tts_tagged_entry() {
        let _appdata_lock = APPDATA_ENV_LOCK
            .lock()
            .expect("APPDATA test lock should not be poisoned");
        let temp_dir = tempfile::tempdir().expect("temp dir should be created");
        let previous_appdata = std::env::var_os("APPDATA");
        std::env::set_var("APPDATA", temp_dir.path());

        super::append_tts_runtime_log_line(true, "runtime initialized successfully")
            .expect("runtime log write should succeed");
        let log_path = runtime_log_path().expect("runtime log path should resolve");
        let contents = std::fs::read_to_string(&log_path).expect("runtime log should be readable");

        match previous_appdata {
            Some(value) => std::env::set_var("APPDATA", value),
            None => std::env::remove_var("APPDATA"),
        }

        assert!(contents.contains("[tts] runtime initialized successfully"));
    }

    #[test]
    fn append_tts_runtime_log_line_skips_file_when_disabled() {
        let _appdata_lock = APPDATA_ENV_LOCK
            .lock()
            .expect("APPDATA test lock should not be poisoned");
        let temp_dir = tempfile::tempdir().expect("temp dir should be created");
        let previous_appdata = std::env::var_os("APPDATA");
        std::env::set_var("APPDATA", temp_dir.path());

        super::append_tts_runtime_log_line(false, "runtime initialized successfully")
            .expect("disabled runtime log write should be a no-op");
        let log_path = runtime_log_path().expect("runtime log path should resolve");

        match previous_appdata {
            Some(value) => std::env::set_var("APPDATA", value),
            None => std::env::remove_var("APPDATA"),
        }

        assert!(!log_path.exists());
    }

    #[test]
    fn runtime_log_message_escapes_line_breaks_and_caps_length() {
        let message = format!(
            "first\r\n{}",
            "x".repeat(super::RUNTIME_LOG_MESSAGE_MAX_CHARS)
        );

        let sanitized = super::sanitize_log_message(&message);

        assert!(sanitized.starts_with("first\\r\\n"));
        assert!(sanitized.ends_with("…[truncated]"));
        assert!(!sanitized.contains('\n'));
        assert!(!sanitized.contains('\r'));
    }

    #[test]
    fn default_response_profile_stays_fast() {
        assert_eq!(default_response_profile(), ResponseProfilePayload::Fast);
    }

    #[test]
    fn maps_core_runtime_phase_to_payload() {
        assert!(matches!(
            to_runtime_phase_payload(voxgolem_core::runtime::RuntimePhase::Processing),
            RuntimePhasePayload::Processing
        ));
    }

    #[test]
    fn contract_response_profile_switch_requires_sleeping_runtime_phase() {
        let voice_pipeline_config = default_voice_pipeline_config();
        let voice_pipeline_state = Mutex::new(
            voxgolem_core::voice_pipeline::VoicePipelineState::new(voice_pipeline_config)
                .expect("voice pipeline should initialize"),
        );

        super::apply_voice_pipeline_transition(
            &voice_pipeline_state,
            voice_pipeline_config,
            voxgolem_core::voice_pipeline::VoicePipelineEvent::StartupValidated,
        )
        .expect("startup validation should set runtime to sleeping");
        assert_eq!(
            super::ensure_response_profile_switch_runtime_is_idle(&voice_pipeline_state),
            Ok(())
        );

        super::apply_voice_pipeline_transition(
            &voice_pipeline_state,
            voice_pipeline_config,
            voxgolem_core::voice_pipeline::VoicePipelineEvent::SubmitPrompt,
        )
        .expect("submit prompt should move runtime to executing");
        assert_eq!(
            super::ensure_response_profile_switch_runtime_is_idle(&voice_pipeline_state),
            Err(String::from(
                "response profile switch is only allowed while runtime is sleeping; current phase is executing"
            ))
        );
    }

    #[test]
    fn contract_response_profile_switch_lock_rejects_busy_backend_operation() {
        let operation_lock = Mutex::new(());
        let _submit_guard = super::lock_response_backend_operation(&operation_lock)
            .expect("lock should be acquired");

        match super::try_lock_response_backend_operation(&operation_lock) {
            Ok(_) => panic!("try_lock should report an active backend operation"),
            Err(message) => assert_eq!(
                message,
                String::from("response backend is busy; wait for the active operation to finish")
            ),
        };
    }

    #[test]
    fn contract_ingest_lock_can_drop_frames_while_backend_is_busy() {
        let operation_lock = Mutex::new(());
        let _submit_guard = super::lock_response_backend_operation(&operation_lock)
            .expect("lock should be acquired");

        let maybe_guard = super::try_lock_response_backend_operation_or_busy(&operation_lock)
            .expect("busy lock should not return a hard error");
        assert!(maybe_guard.is_none());
    }

    #[test]
    fn contract_cue_assets_are_embedded_data_urls() {
        let cue_asset_paths = super::embedded_cue_asset_paths();

        assert!(cue_asset_paths
            .start_listening
            .starts_with(super::CUE_AUDIO_DATA_URL_PREFIX));
        assert!(cue_asset_paths
            .stop_listening
            .starts_with(super::CUE_AUDIO_DATA_URL_PREFIX));
        assert!(!cue_asset_paths.start_listening.contains("resources/"));
        assert!(!cue_asset_paths.stop_listening.contains("resources/"));
    }

    #[test]
    fn contract_response_profile_switch_requires_ready_startup_state() {
        let warming_state = Arc::new(Mutex::new(super::StartupStatePayload::WarmingModel {
            cue_asset_paths: super::CueAssetPathsPayload {
                start_listening: String::from("resources/start-listening.wav"),
                stop_listening: String::from("resources/stop-listening.wav"),
            },
            runtime_phase: RuntimePhasePayload::Initializing,
            voice_input_available: true,
            voice_input_error: None,
            silence_timeout_ms: DEFAULT_SILENCE_TIMEOUT_MS,
            message: String::from("Loading local Gemma model..."),
            selected_response_profile: ResponseProfilePayload::Quality,
            supported_response_profiles: vec![
                ResponseProfilePayload::Fast,
                ResponseProfilePayload::Quality,
            ],
            prompt_cancellation_available: false,
            tts_enabled: false,
            tts_output_gain_db: 3.0,
            capabilities: Vec::new(),
        }));

        assert_eq!(
            super::ensure_startup_ready_for_profile_switch(&warming_state),
            Err(String::from(
                "response backend is busy; wait for the active operation to finish"
            ))
        );

        let ready_state = Arc::new(Mutex::new(super::StartupStatePayload::Ready {
            cue_asset_paths: super::CueAssetPathsPayload {
                start_listening: String::from("resources/start-listening.wav"),
                stop_listening: String::from("resources/stop-listening.wav"),
            },
            runtime_phase: RuntimePhasePayload::Sleeping,
            voice_input_available: true,
            voice_input_error: None,
            silence_timeout_ms: DEFAULT_SILENCE_TIMEOUT_MS,
            selected_response_profile: ResponseProfilePayload::Fast,
            supported_response_profiles: vec![
                ResponseProfilePayload::Fast,
                ResponseProfilePayload::Quality,
            ],
            prompt_cancellation_available: false,
            tts_enabled: false,
            tts_output_gain_db: 3.0,
            capabilities: Vec::new(),
        }));

        assert_eq!(
            super::ensure_startup_ready_for_profile_switch(&ready_state),
            Ok(())
        );
    }

    #[test]
    fn current_silence_deadline_uses_refreshed_speech_activity_plus_timeout() {
        let voice_pipeline_config = default_voice_pipeline_config();
        let voice_pipeline_state = voxgolem_core::voice_pipeline::apply_voice_pipeline_event(
            &voxgolem_core::voice_pipeline::VoicePipelineState::new(voice_pipeline_config)
                .expect("voice pipeline should initialize"),
            voice_pipeline_config,
            voxgolem_core::voice_pipeline::VoicePipelineEvent::StartupValidated,
        )
        .expect("startup validation should succeed");
        let listening_state = voxgolem_core::voice_pipeline::apply_voice_pipeline_event(
            &voice_pipeline_state.0,
            voice_pipeline_config,
            voxgolem_core::voice_pipeline::VoicePipelineEvent::WakeWordDetected { now_ms: 100 },
        )
        .expect("wake word should start listening");
        let refreshed_state = voxgolem_core::voice_pipeline::apply_voice_pipeline_event(
            &listening_state.0,
            voice_pipeline_config,
            voxgolem_core::voice_pipeline::VoicePipelineEvent::SpeechDetected { now_ms: 450 },
        )
        .expect("speech activity should refresh listening deadline");
        let locked_state = Mutex::new(refreshed_state.0);

        assert_eq!(
            current_silence_deadline(&locked_state, voice_pipeline_config),
            Ok(DEFAULT_SILENCE_TIMEOUT_MS + 450)
        );
    }

    #[test]
    fn invariant_transcription_ready_samples_matches_finished_utterance_length() {
        let action = voxgolem_core::voice_pipeline::VoicePipelineAction::FinishedUtterance {
            transcription_input: voxgolem_model::parakeet::ParakeetTranscriptionInput::new(
                voxgolem_model::parakeet::PARAKEET_SAMPLE_RATE_HZ,
                vec![0.1, 0.2, 0.3],
            )
            .expect("valid transcription input"),
        };

        assert_eq!(transcription_ready_samples(&action), Some(3));
    }

    #[test]
    fn runtime_phase_response_from_state_reflects_capture_lengths() {
        let voice_pipeline_config = default_voice_pipeline_config();
        let ready_state = voxgolem_core::voice_pipeline::apply_voice_pipeline_event(
            &voxgolem_core::voice_pipeline::VoicePipelineState::new(voice_pipeline_config)
                .expect("voice pipeline should initialize"),
            voice_pipeline_config,
            voxgolem_core::voice_pipeline::VoicePipelineEvent::StartupValidated,
        )
        .expect("startup validation should succeed")
        .0;
        let preroll_state = voxgolem_core::voice_pipeline::ingest_audio_frame(
            &ready_state,
            voice_pipeline_config,
            vec![0.1, 0.2, 0.3],
        )
        .expect("sleeping frame should be recorded");
        let listening_state = voxgolem_core::voice_pipeline::apply_voice_pipeline_event(
            &preroll_state,
            voice_pipeline_config,
            voxgolem_core::voice_pipeline::VoicePipelineEvent::WakeWordDetected { now_ms: 100 },
        )
        .expect("wake word should start listening")
        .0;
        let utterance_state = voxgolem_core::voice_pipeline::ingest_audio_frame(
            &listening_state,
            voice_pipeline_config,
            vec![0.4, 0.5],
        )
        .expect("listening frame should be recorded");

        assert_eq!(
            runtime_phase_response_from_state(&utterance_state, None, None, None),
            RuntimePhaseResponsePayload {
                runtime_phase: RuntimePhasePayload::Listening,
                transcription_ready_samples: None,
                transcript_text: None,
                last_activity_ms: Some(100),
                capturing_utterance: true,
                preroll_samples: 3,
                utterance_samples: 2,
                telemetry: None,
            }
        );
    }

    #[test]
    fn contract_runtime_phase_response_from_state_surfaces_telemetry() {
        let voice_pipeline_config = default_voice_pipeline_config();
        let ready_state = voxgolem_core::voice_pipeline::apply_voice_pipeline_event(
            &voxgolem_core::voice_pipeline::VoicePipelineState::new(voice_pipeline_config)
                .expect("voice pipeline should initialize"),
            voice_pipeline_config,
            voxgolem_core::voice_pipeline::VoicePipelineEvent::StartupValidated,
        )
        .expect("startup validation should succeed")
        .0;
        let preroll_state = voxgolem_core::voice_pipeline::ingest_audio_frame(
            &ready_state,
            voice_pipeline_config,
            vec![0.1, 0.2, 0.3],
        )
        .expect("sleeping frame should be recorded");
        let listening_state = voxgolem_core::voice_pipeline::apply_voice_pipeline_event(
            &preroll_state,
            voice_pipeline_config,
            voxgolem_core::voice_pipeline::VoicePipelineEvent::WakeWordDetected { now_ms: 100 },
        )
        .expect("wake word should start listening")
        .0;
        let utterance_state = voxgolem_core::voice_pipeline::ingest_audio_frame(
            &listening_state,
            voice_pipeline_config,
            vec![0.4, 0.5],
        )
        .expect("listening frame should be recorded");
        let telemetry = RuntimeTelemetryPayload {
            frame_id: Some("frame-1".to_string()),
            backend_ingest_started_ms: Some(110),
            backend_ingest_completed_ms: Some(120),
            wake_detected_ms: Some(118),
            wake_confidence: Some(0.72),
            transcription_started_ms: None,
            transcription_completed_ms: None,
        };

        let response = runtime_phase_response_from_state(
            &utterance_state,
            None,
            None,
            Some(telemetry.clone()),
        );

        assert_eq!(response.runtime_phase, RuntimePhasePayload::Listening);
        assert_eq!(response.telemetry, Some(telemetry));
    }

    #[test]
    fn current_runtime_phase_response_reads_single_snapshot() {
        let voice_pipeline_config = default_voice_pipeline_config();
        let ready_state = voxgolem_core::voice_pipeline::apply_voice_pipeline_event(
            &voxgolem_core::voice_pipeline::VoicePipelineState::new(voice_pipeline_config)
                .expect("voice pipeline should initialize"),
            voice_pipeline_config,
            voxgolem_core::voice_pipeline::VoicePipelineEvent::StartupValidated,
        )
        .expect("startup validation should succeed")
        .0;
        let preroll_state = voxgolem_core::voice_pipeline::ingest_audio_frame(
            &ready_state,
            voice_pipeline_config,
            vec![0.1, 0.2],
        )
        .expect("sleeping frame should be recorded");
        let locked_state = Mutex::new(preroll_state);

        assert_eq!(
            current_runtime_phase_response(
                &locked_state,
                Some(2),
                Some("draft release notes".to_string()),
            ),
            Ok(RuntimePhaseResponsePayload {
                runtime_phase: RuntimePhasePayload::Sleeping,
                transcription_ready_samples: Some(2),
                transcript_text: Some("draft release notes".to_string()),
                last_activity_ms: None,
                capturing_utterance: false,
                preroll_samples: 2,
                utterance_samples: 0,
                telemetry: None,
            })
        );
    }

    #[test]
    fn transcribe_finished_utterance_returns_none_for_non_transcription_actions() {
        assert_eq!(
            transcribe_finished_utterance(
                &voxgolem_core::voice_pipeline::VoicePipelineAction::None,
                &None,
            ),
            Ok(None)
        );
    }

    #[test]
    fn apply_optional_speech_activity_refreshes_last_activity_while_listening() {
        let voice_pipeline_config = default_voice_pipeline_config();
        let listening_state = voxgolem_core::voice_pipeline::apply_voice_pipeline_event(
            &voxgolem_core::voice_pipeline::VoicePipelineState::new(voice_pipeline_config)
                .expect("voice pipeline should initialize"),
            voice_pipeline_config,
            voxgolem_core::voice_pipeline::VoicePipelineEvent::StartupValidated,
        )
        .expect("startup validation should succeed")
        .0;
        let listening_state = voxgolem_core::voice_pipeline::apply_voice_pipeline_event(
            &listening_state,
            voice_pipeline_config,
            voxgolem_core::voice_pipeline::VoicePipelineEvent::WakeWordDetected { now_ms: 100 },
        )
        .expect("wake word should start listening")
        .0;

        let refreshed_state =
            apply_optional_speech_activity(listening_state, voice_pipeline_config, true, 450)
                .expect("speech activity should refresh listening state");

        assert_eq!(
            refreshed_state.session().voice_turn().last_activity_ms(),
            Some(450)
        );
    }

    #[test]
    fn build_mark_silence_response_surfaces_transcript_text() {
        let voice_pipeline_config = default_voice_pipeline_config();
        let processing_state = voxgolem_core::voice_pipeline::apply_voice_pipeline_event(
            &voxgolem_core::voice_pipeline::VoicePipelineState::new(voice_pipeline_config)
                .expect("voice pipeline should initialize"),
            voice_pipeline_config,
            voxgolem_core::voice_pipeline::VoicePipelineEvent::StartupValidated,
        )
        .expect("startup validation should succeed")
        .0;
        let processing_state = voxgolem_core::voice_pipeline::apply_voice_pipeline_event(
            &processing_state,
            voice_pipeline_config,
            voxgolem_core::voice_pipeline::VoicePipelineEvent::WakeWordDetected { now_ms: 100 },
        )
        .expect("wake word should start listening")
        .0;
        let processing_state = voxgolem_core::voice_pipeline::ingest_audio_frame(
            &processing_state,
            voice_pipeline_config,
            vec![0.1, 0.2, 0.3],
        )
        .expect("listening frame should be recorded before silence");
        let processing_state = voxgolem_core::voice_pipeline::apply_voice_pipeline_event(
            &processing_state,
            voice_pipeline_config,
            voxgolem_core::voice_pipeline::VoicePipelineEvent::SilenceCheck {
                now_ms: DEFAULT_SILENCE_TIMEOUT_MS + 101,
            },
        )
        .expect("silence should move runtime to processing")
        .0;
        let locked_state = Mutex::new(processing_state);
        let action = voxgolem_core::voice_pipeline::VoicePipelineAction::FinishedUtterance {
            transcription_input: voxgolem_model::parakeet::ParakeetTranscriptionInput::new(
                voxgolem_model::parakeet::PARAKEET_SAMPLE_RATE_HZ,
                vec![0.1, 0.2, 0.3],
            )
            .expect("valid transcription input"),
        };

        assert_eq!(
            build_mark_silence_response(
                &locked_state,
                &action,
                Some("draft release notes".to_string()),
                Some(RuntimeTelemetryPayload {
                    frame_id: Some("frame-2".to_string()),
                    backend_ingest_started_ms: None,
                    backend_ingest_completed_ms: None,
                    wake_detected_ms: None,
                    wake_confidence: None,
                    transcription_started_ms: Some(2000),
                    transcription_completed_ms: Some(2100),
                }),
            ),
            Ok(RuntimePhaseResponsePayload {
                runtime_phase: RuntimePhasePayload::Processing,
                transcription_ready_samples: Some(3),
                transcript_text: Some("draft release notes".to_string()),
                last_activity_ms: None,
                capturing_utterance: false,
                preroll_samples: 0,
                utterance_samples: 0,
                telemetry: Some(RuntimeTelemetryPayload {
                    frame_id: Some("frame-2".to_string()),
                    backend_ingest_started_ms: None,
                    backend_ingest_completed_ms: None,
                    wake_detected_ms: None,
                    wake_confidence: None,
                    transcription_started_ms: Some(2000),
                    transcription_completed_ms: Some(2100),
                }),
            })
        );
    }

    #[test]
    fn reset_voice_pipeline_to_waiting_returns_runtime_to_sleeping() {
        let voice_pipeline_config = default_voice_pipeline_config();
        let processing_state = voxgolem_core::voice_pipeline::apply_voice_pipeline_event(
            &voxgolem_core::voice_pipeline::VoicePipelineState::new(voice_pipeline_config)
                .expect("voice pipeline should initialize"),
            voice_pipeline_config,
            voxgolem_core::voice_pipeline::VoicePipelineEvent::StartupValidated,
        )
        .expect("startup validation should succeed")
        .0;
        let processing_state = voxgolem_core::voice_pipeline::apply_voice_pipeline_event(
            &processing_state,
            voice_pipeline_config,
            voxgolem_core::voice_pipeline::VoicePipelineEvent::WakeWordDetected { now_ms: 100 },
        )
        .expect("wake word should start listening")
        .0;
        let processing_state = voxgolem_core::voice_pipeline::ingest_audio_frame(
            &processing_state,
            voice_pipeline_config,
            vec![0.1, 0.2, 0.3],
        )
        .expect("listening frame should be recorded before silence");
        let processing_state = voxgolem_core::voice_pipeline::apply_voice_pipeline_event(
            &processing_state,
            voice_pipeline_config,
            voxgolem_core::voice_pipeline::VoicePipelineEvent::SilenceCheck {
                now_ms: DEFAULT_SILENCE_TIMEOUT_MS + 101,
            },
        )
        .expect("silence should move runtime to processing")
        .0;
        let locked_state = Mutex::new(processing_state);

        reset_voice_pipeline_to_waiting(&locked_state, &None, &None, voice_pipeline_config)
            .expect("reset to waiting should succeed");
        assert_eq!(
            current_runtime_phase_response(&locked_state, None, None)
                .expect("runtime snapshot should succeed")
                .runtime_phase,
            RuntimePhasePayload::Sleeping
        );
    }

    #[test]
    fn ingest_audio_frame_wake_detection_starts_listening_without_seeding_preroll() {
        let voice_pipeline_config = default_voice_pipeline_config();
        let ready_state = voxgolem_core::voice_pipeline::apply_voice_pipeline_event(
            &voxgolem_core::voice_pipeline::VoicePipelineState::new(voice_pipeline_config)
                .expect("voice pipeline should initialize"),
            voice_pipeline_config,
            voxgolem_core::voice_pipeline::VoicePipelineEvent::StartupValidated,
        )
        .expect("startup validation should succeed")
        .0;
        let listening_state = ingest_audio_frame_with_optional_wake_word_detection(
            &ready_state,
            voice_pipeline_config,
            vec![0.1, 0.2, 0.3],
            Some(100),
        )
        .expect("wake word detection should promote sleeping to listening");

        assert_eq!(
            listening_state.session().runtime().phase(),
            voxgolem_core::runtime::RuntimePhase::Listening
        );
        assert!(listening_state.capture().capturing_utterance());
        assert_eq!(listening_state.capture().preroll_len(), 0);
        assert_eq!(listening_state.capture().utterance_len(), 3);
    }

    #[test]
    fn process_wake_word_frame_is_a_no_op_without_runtime() {
        assert_eq!(process_wake_word_frame(&None, &[0.1, 0.2]), Ok(None));
    }

    #[test]
    fn process_wake_word_frame_propagates_detector_errors() {
        let runtime = Some(Mutex::new(WakeWordRuntime::new_failing_for_test()));

        assert_eq!(
            process_wake_word_frame(&runtime, &[0.0; 8]),
            Err(String::from("synthetic wake word scorer failure"))
        );
    }

    #[test]
    fn reset_wake_word_runtime_is_a_no_op_without_runtime() {
        assert_eq!(reset_wake_word_runtime(&None), Ok(()));
    }

    #[test]
    fn build_startup_error_app_state_tracks_error_runtime_and_no_wake_word_runtime() {
        let app_state = build_startup_error_app_state(
            default_voice_pipeline_config(),
            "wake word init failed".to_string(),
        );

        assert!(matches!(
            *app_state
                .startup_state
                .lock()
                .expect("startup state lock should not be poisoned"),
            super::StartupStatePayload::Error { .. }
        ));
        assert!(app_state.runtime_config.is_none());
        assert!(app_state.wake_word_runtime.is_none());
    }

    #[test]
    fn wake_word_runtime_reports_missing_model_file() {
        let temp_dir = tempfile::tempdir().expect("temp dir should be created");
        let wake_word_model_path = temp_dir.path().join("missing-hey-livekit.onnx");

        let error = WakeWordRuntime::new(&wake_word_model_path, 0.68)
            .err()
            .expect("missing model file should fail");

        assert!(error.contains("failed to load wake word model"));
    }

    #[test]
    fn wake_word_event_timestamp_uses_backend_now_ms() {
        assert_eq!(
            wake_word_event_timestamp(
                42_000,
                Some(WakeWordDetection {
                    detected_at_ms: 60,
                    confidence: 0.73,
                }),
            ),
            Some(42_000)
        );
        assert_eq!(wake_word_event_timestamp(42_000, None), None);
    }
}
