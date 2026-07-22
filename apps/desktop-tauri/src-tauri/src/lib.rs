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
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tauri::{Emitter, Manager};

mod livekit_wakeword;
mod transcription;
#[allow(dead_code)]
mod tts;
mod voice_activity;
mod wake_word;

const DEFAULT_SILENCE_TIMEOUT_MS: u64 = 1_500;
const DEFAULT_PREROLL_MAX_SAMPLES: usize = 4_000;
const DEFAULT_UTTERANCE_MAX_SAMPLES: usize = 4_800_000;
const LLAMA_CPP_MODEL_ALIAS: &str = "default";
const LLAMA_CPP_MAX_TOKENS: u16 = 512;
const LLAMA_CPP_CONTEXT_WINDOW_TOKENS: usize = 8_192;
const LLAMA_CPP_CONTEXT_SAFETY_MARGIN_TOKENS: usize = 512;
const LLAMA_CPP_CHAT_WRAPPER_TOKENS: usize = 64;
const RESPONSE_PROFILE_STATE_FILE: &str = "state.toml";
const RUNTIME_LOG_DIR: &str = "logs";
const RUNTIME_LOG_FILE: &str = "runtime.log";
const RUNTIME_LOG_MESSAGE_MAX_CHARS: usize = 16_384;
const OPENCODE_PROMPT_INACTIVITY_TIMEOUT: Duration = Duration::from_secs(300);
const OPENCODE_PROMPT_CANCELLATION_TIMEOUT: Duration = Duration::from_secs(5);
const LLAMA_CPP_ROLLOVER_REASON: &str =
    "Context budget reached; started a new local Gemma conversation for this reply.";
const CUE_AUDIO_DATA_URL_PREFIX: &str = "data:audio/wav;base64,";
const START_LISTENING_CUE_WAV: &[u8] = include_bytes!("../resources/start-listening.wav");
const STOP_LISTENING_CUE_WAV: &[u8] = include_bytes!("../resources/stop-listening.wav");

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
    parakeet_runtime: Option<Mutex<transcription::ParakeetRuntime>>,
    local_tts_runtime: Mutex<Option<tts::LocalTtsRuntime>>,
    llama_cpp_runtime: Arc<Mutex<Option<voxgolem_platform::llama_cpp::LlamaCppRuntime>>>,
    llama_cpp_conversation: Mutex<Vec<LlamaConversationTurn>>,
    llama_cpp_system_prompt: Option<String>,
    opencode_server: Arc<Mutex<Option<voxgolem_platform::opencode::OpencodeServer>>>,
    active_prompt: Arc<Mutex<Option<ActivePrompt>>>,
    active_prompt_generation: AtomicU64,
}

#[derive(Clone)]
struct ActivePrompt {
    request_id: String,
    generation: u64,
    cancelled: Arc<AtomicBool>,
    cancellation_signal: Arc<tokio::sync::Notify>,
    completion_signal: Arc<tokio::sync::Notify>,
    client: voxgolem_platform::opencode::OpencodeClient,
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
fn set_tts_enabled(
    enabled: bool,
    app_state: tauri::State<'_, AppState>,
) -> Result<SetTtsEnabledPayload, String> {
    let config = app_state
        .runtime_config
        .as_ref()
        .ok_or_else(|| String::from("startup config is not ready"))?;

    let mut runtime_guard = app_state
        .local_tts_runtime
        .lock()
        .map_err(|_| String::from("local tts runtime lock is poisoned"))?;

    if enabled {
        if runtime_guard.is_none() {
            match initialize_local_tts_runtime(&config.local_tts, true, config.logging.enabled) {
                Ok(runtime) => {
                    *runtime_guard = runtime;
                    log_tts_runtime_event(config.logging.enabled, "runtime enabled");
                }
                Err(error) => {
                    log_tts_runtime_event(
                        config.logging.enabled,
                        &format!("runtime enable failed: {error}"),
                    );
                    return Err(error);
                }
            }
        }
    } else {
        *runtime_guard = None;
        log_tts_runtime_event(config.logging.enabled, "runtime disabled and unloaded");
    }

    persist_tts_enabled(enabled)?;
    set_startup_tts_enabled(&app_state.startup_state, enabled);

    let sample_rate_hz = runtime_guard
        .as_ref()
        .map(tts::LocalTtsRuntime::sample_rate_hz)
        .unwrap_or(config.local_tts.sample_rate_hz);

    Ok(SetTtsEnabledPayload {
        enabled,
        sample_rate_hz,
    })
}

#[tauri::command]
fn synthesize_local_tts(
    text: String,
    app_state: tauri::State<'_, AppState>,
) -> Result<SynthesizeLocalTtsPayload, String> {
    let runtime_file_logging_enabled = app_state
        .runtime_config
        .as_ref()
        .map(|config| config.logging.enabled)
        .unwrap_or(false);
    let runtime_guard = app_state
        .local_tts_runtime
        .lock()
        .map_err(|_| String::from("local tts runtime lock is poisoned"))?;
    let runtime = runtime_guard.as_ref().ok_or_else(|| {
        log_tts_runtime_event(
            runtime_file_logging_enabled,
            "synthesis rejected: runtime unavailable",
        );
        String::from("local tts runtime is not available")
    })?;

    let audio = runtime.synthesize(&text).map_err(|error| {
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
    let response_profile_switch_generation =
        Arc::clone(&app_state.response_profile_switch_generation);
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
        };
    }

    std::thread::spawn(move || {
        let start_result = voxgolem_platform::llama_cpp::LlamaCppRuntime::start(server_spec);
        if response_profile_switch_generation.load(Ordering::SeqCst) != switch_generation {
            shutdown_llama_start_result(start_result);
            return;
        }

        let next_state = match start_result {
            Ok(runtime) => {
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

                if let Err(error) = persist_selected_response_profile(profile) {
                    eprintln!("failed to persist response profile state: {error}");
                }

                if let Ok(mut selected) = selected_response_profile.lock() {
                    *selected = profile;
                }

                startup_ready_state_from_snapshot(&startup_snapshot, profile)
            }
            Err(error) => {
                let restore_result =
                    voxgolem_platform::llama_cpp::LlamaCppRuntime::start(fallback_server_spec);
                if response_profile_switch_generation.load(Ordering::SeqCst) != switch_generation {
                    shutdown_llama_start_result(restore_result);
                    return;
                }

                match restore_result {
                    Ok(runtime) => {
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

                        startup_ready_state_from_snapshot(&startup_snapshot, current_profile)
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
    Completed,
    Cancelled,
    Failed(String),
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
    app: tauri::AppHandle,
    app_state: tauri::State<'_, AppState>,
) -> Result<PromptFinalPayload, String> {
    ensure_startup_ready_for_prompt(&app_state.startup_state)?;
    if request_id.trim().is_empty() {
        return Err(String::from("request ID must not be empty"));
    }
    let prompt = validate_prompt_text(prompt)?;
    let is_opencode = matches!(
        app_state
            .runtime_config
            .as_ref()
            .map(|config| &config.response_backend),
        Some(voxgolem_core::config::ResponseBackendConfig::Opencode { .. })
    );
    let registered_prompt = {
        let server = app_state
            .opencode_server
            .lock()
            .map_err(|_| String::from("opencode server lock is poisoned"))?;
        server
            .as_ref()
            .map(|server| {
                let client = server.client();
                register_active_prompt(
                    &app_state.active_prompt,
                    &app_state.active_prompt_generation,
                    &request_id,
                    client.clone(),
                )
                .map(|registration| (client, registration))
            })
            .transpose()?
    };
    let Some((client, (generation, cancelled, cancellation_signal))) = registered_prompt else {
        if is_opencode {
            return Err(String::from("OpenCode server is not available"));
        }
        let result = match submit_prompt_sync(prompt, app_state.clone()) {
            Ok(result) => result,
            Err(error) => {
                emit_prompt_event(
                    &app,
                    &request_id,
                    PromptExecutionEventPayload::Error {
                        message: error.clone(),
                    },
                )?;
                return Err(error);
            }
        };
        let phase = result.runtime_phase.clone();
        for event in result.events {
            emit_prompt_event(&app, &request_id, event)?;
        }
        emit_prompt_event(
            &app,
            &request_id,
            PromptExecutionEventPayload::Completed {
                runtime_phase: phase.clone(),
            },
        )?;
        return Ok(PromptFinalPayload {
            request_id,
            runtime_phase: phase,
            outcome: "completed".into(),
            error_message: None,
        });
    };
    if let Err(error) = apply_voice_pipeline_transition(
        &app_state.voice_pipeline_state,
        app_state.voice_pipeline_config,
        voxgolem_core::voice_pipeline::VoicePipelineEvent::SubmitPrompt,
    ) {
        clear_active_prompt(&app_state.active_prompt, &request_id, generation)?;
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
        &prompt,
    )
    .await;
    let still_owned = clear_active_prompt(&app_state.active_prompt, &request_id, generation)?;
    if !still_owned {
        let phase = current_runtime_phase(&app_state.voice_pipeline_state)?;
        emit_prompt_event(
            &app,
            &request_id,
            PromptExecutionEventPayload::Cancelled {
                runtime_phase: phase.clone(),
            },
        )?;
        return Ok(PromptFinalPayload {
            request_id,
            runtime_phase: phase,
            outcome: String::from("cancelled"),
            error_message: None,
        });
    }

    let (outcome, event, error_message) = match result {
        OpencodePromptResult::Completed => {
            apply_voice_pipeline_transition(
                &app_state.voice_pipeline_state,
                app_state.voice_pipeline_config,
                voxgolem_core::voice_pipeline::VoicePipelineEvent::PromptCompleted,
            )?;
            let phase = current_runtime_phase(&app_state.voice_pipeline_state)?;
            (
                "completed",
                PromptExecutionEventPayload::Completed {
                    runtime_phase: phase,
                },
                None,
            )
        }
        OpencodePromptResult::Cancelled => {
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
    emit_prompt_event(&app, &request_id, event)?;
    Ok(PromptFinalPayload {
        request_id,
        runtime_phase: phase,
        outcome: outcome.to_string(),
        error_message,
    })
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
    client: voxgolem_platform::opencode::OpencodeClient,
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
    if let Err(error) = context.client.prompt(&prompt).await {
        return OpencodePromptResult::Failed(error.to_string());
    }

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
                return OpencodePromptResult::Completed;
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
    let active = app_state
        .active_prompt
        .lock()
        .map_err(|_| String::from("active prompt lock is poisoned"))?
        .clone();
    let active = active
        .filter(|active| active.request_id == request_id)
        .ok_or_else(|| String::from("prompt request is no longer active"))?;
    active.cancelled.store(true, Ordering::SeqCst);
    active.cancellation_signal.notify_one();
    active
        .client
        .abort()
        .await
        .map_err(|error| format!("failed to abort OpenCode prompt: {error}"))
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
    let silence_deadline = current_silence_deadline(
        &app_state.voice_pipeline_state,
        app_state.voice_pipeline_config,
    )?;

    let action = apply_voice_pipeline_transition(
        &app_state.voice_pipeline_state,
        app_state.voice_pipeline_config,
        voxgolem_core::voice_pipeline::VoicePipelineEvent::SilenceCheck {
            now_ms: silence_deadline,
        },
    )?;

    let should_measure_transcription = matches!(
        action,
        voxgolem_core::voice_pipeline::VoicePipelineAction::FinishedUtterance { .. }
    );
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
    let is_opencode = matches!(
        app_state
            .runtime_config
            .as_ref()
            .map(|config| &config.response_backend),
        Some(voxgolem_core::config::ResponseBackendConfig::Opencode { .. })
    );

    if is_opencode {
        reset_opencode_session(&app_state).await?;
    } else {
        reset_llama_session(&app_state)?;
    }

    reset_runtime_session(&app_state)?;

    Ok(RuntimePhaseResponsePayload {
        ..current_runtime_phase_response(&app_state.voice_pipeline_state, None, None)?
    })
}

fn reset_runtime_session(app_state: &AppState) -> Result<(), String> {
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

    let backend_ingest_completed_ms = current_time_ms()?;

    Ok(runtime_phase_response_from_state(
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
    ))
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
        prompt_cancellation_available: false,
        tts_enabled: startup_snapshot.tts_enabled,
        tts_output_gain_db: startup_snapshot.tts_output_gain_db,
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
            ..
        }
        | StartupStatePayload::Ready {
            cue_asset_paths,
            voice_input_available,
            voice_input_error,
            silence_timeout_ms,
            tts_enabled,
            tts_output_gain_db,
            ..
        } => Ok(StartupSnapshot {
            cue_asset_paths: cue_asset_paths.clone(),
            voice_input_available: *voice_input_available,
            voice_input_error: voice_input_error.clone(),
            silence_timeout_ms: *silence_timeout_ms,
            tts_enabled: *tts_enabled,
            tts_output_gain_db: *tts_output_gain_db,
            supported_response_profiles,
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
    let mut profiles = vec![ResponseProfilePayload::Fast];
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

fn resolve_effective_tts_enabled(default_enabled: bool) -> bool {
    let persisted_tts_enabled = load_persisted_tts_enabled().unwrap_or_else(|error| {
        eprintln!("failed to read tts state: {error}");
        None
    });

    persisted_tts_enabled.unwrap_or(default_enabled)
}

fn response_profile_state_path() -> Result<PathBuf, String> {
    let config_path = voxgolem_core::config::default_config_path()
        .map_err(|error| format!("failed to resolve %APPDATA%\\VoxGolem\\config.toml: {error}"))?;

    Ok(config_path.with_file_name(RESPONSE_PROFILE_STATE_FILE))
}

fn runtime_log_path() -> Result<PathBuf, String> {
    let config_path = voxgolem_core::config::default_config_path()
        .map_err(|error| format!("failed to resolve %APPDATA%\\VoxGolem\\config.toml: {error}"))?;

    Ok(config_path
        .with_file_name(RUNTIME_LOG_DIR)
        .join(RUNTIME_LOG_FILE))
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
}

fn parse_persisted_state(contents: &str) -> Result<PersistedState, String> {
    let mut state = PersistedState::default();

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
        }
    }

    Ok(state)
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

    let contents = if lines.is_empty() {
        String::new()
    } else {
        format!("{}\n", lines.join("\n"))
    };

    fs::write(&state_path, contents).map_err(|error| {
        format!(
            "failed to write response profile state {}: {error}",
            state_path.display()
        )
    })
}

fn load_selected_response_profile() -> Result<Option<ResponseProfilePayload>, String> {
    Ok(load_persisted_state()?.selected_response_profile)
}

fn persist_selected_response_profile(profile: ResponseProfilePayload) -> Result<(), String> {
    let mut persisted = load_persisted_state().unwrap_or_default();
    persisted.selected_response_profile = Some(profile);
    persist_state(persisted)
}

fn load_persisted_tts_enabled() -> Result<Option<bool>, String> {
    Ok(load_persisted_state()?.tts_enabled)
}

fn persist_tts_enabled(enabled: bool) -> Result<(), String> {
    let mut persisted = load_persisted_state().unwrap_or_default();
    persisted.tts_enabled = Some(enabled);
    persist_state(persisted)
}

fn load_persisted_ui_text_size() -> Result<Option<UiTextSizePayload>, String> {
    Ok(load_persisted_state()?.ui_text_size)
}

fn persist_ui_text_size(text_size: UiTextSizePayload) -> Result<(), String> {
    let mut persisted = load_persisted_state().unwrap_or_default();
    persisted.ui_text_size = Some(text_size);
    persist_state(persisted)
}

fn load_persisted_ui_theme() -> Result<Option<UiThemePayload>, String> {
    Ok(load_persisted_state()?.ui_theme)
}

fn persist_ui_theme(theme: UiThemePayload) -> Result<(), String> {
    let mut persisted = load_persisted_state().unwrap_or_default();
    persisted.ui_theme = Some(theme);
    persist_state(persisted)
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

fn build_app_state<R: tauri::Runtime>(_app: &tauri::AppHandle<R>) -> AppState {
    let fallback_voice_pipeline_config = default_voice_pipeline_config();
    let cue_asset_paths = embedded_cue_asset_paths();

    match voxgolem_core::config::load_runtime_config(None) {
        Ok(config) => {
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
            let response_profile_switch_generation = Arc::new(AtomicU64::new(0));
            let wake_word_runtime = match wake_word::WakeWordRuntime::new(
                &config.wake_word_model_path,
                config.wake_word_detection_threshold,
            ) {
                Ok(runtime) => runtime,
                Err(error) => {
                    return build_startup_error_app_state(
                        voice_pipeline_config,
                        format!("failed to initialize wake word detector: {error}"),
                    );
                }
            };
            let effective_tts_enabled = resolve_effective_tts_enabled(config.local_tts.enabled);
            let local_tts_runtime = match initialize_local_tts_runtime(
                &config.local_tts,
                effective_tts_enabled,
                config.logging.enabled,
            ) {
                Ok(runtime) => runtime,
                Err(error) => {
                    return build_startup_error_app_state(voice_pipeline_config, error);
                }
            };
            let tts_enabled = local_tts_runtime.is_some();
            let tts_output_gain_db = config.local_tts.output_gain_db;
            let mut voice_input_errors = Vec::new();
            let parakeet_runtime =
                match transcription::ParakeetRuntime::load(&config.parakeet_model_dir) {
                    Ok(runtime) => Some(Mutex::new(runtime)),
                    Err(error) => {
                        let error_message =
                            format!("failed to initialize parakeet transcriber: {error:?}");
                        eprintln!("{error_message}");
                        voice_input_errors.push(error_message);
                        None
                    }
                };
            let voice_activity_runtime =
                match voice_activity::VoiceActivityRuntime::load(&config.silero_vad_model) {
                    Ok(runtime) => Some(Mutex::new(runtime)),
                    Err(error) => {
                        let error_message =
                            format!("failed to initialize voice activity detector: {error:?}");
                        eprintln!("{error_message}");
                        voice_input_errors.push(error_message);
                        None
                    }
                };
            let voice_input_available =
                parakeet_runtime.is_some() && voice_activity_runtime.is_some();
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
            };
            let startup_state = Arc::new(Mutex::new(match &config.response_backend {
                voxgolem_core::config::ResponseBackendConfig::LlamaCpp { .. } => {
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
                    }
                }
            }));
            let llama_cpp_runtime = Arc::new(Mutex::new(None));
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
                let startup_generation = response_profile_switch_generation.load(Ordering::SeqCst);
                let cue_asset_paths = cue_asset_paths.clone();
                let voice_input_error = voice_input_error.clone();
                let silence_timeout_ms = config.silence_timeout_ms;
                let selected_response_profile = selected_profile_at_startup;
                let supported_response_profiles = supported_response_profiles.clone();
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

                std::thread::spawn(move || {
                    let start_result =
                        voxgolem_platform::llama_cpp::LlamaCppRuntime::start(server_spec);
                    if response_profile_switch_generation.load(Ordering::SeqCst)
                        != startup_generation
                    {
                        shutdown_llama_start_result(start_result);
                        return;
                    }

                    let next_state = match start_result {
                        Ok(runtime) => {
                            if !store_llama_runtime_if_current(
                                runtime,
                                &llama_cpp_runtime,
                                &response_profile_switch_generation,
                                startup_generation,
                            ) {
                                return;
                            }

                            StartupStatePayload::Ready {
                                cue_asset_paths,
                                runtime_phase: RuntimePhasePayload::Sleeping,
                                voice_input_available,
                                voice_input_error,
                                silence_timeout_ms,
                                selected_response_profile,
                                supported_response_profiles,
                                prompt_cancellation_available: false,
                                tts_enabled,
                                tts_output_gain_db,
                            }
                        }
                        Err(error) => StartupStatePayload::Error {
                            message: format!(
                                "failed to initialize local llama.cpp runtime: {error}"
                            ),
                        },
                    };

                    if response_profile_switch_generation.load(Ordering::SeqCst)
                        != startup_generation
                    {
                        return;
                    }

                    if let Ok(mut guard) = startup_state.lock() {
                        *guard = next_state;
                    }
                });
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
                wake_word_runtime: Some(Mutex::new(wake_word_runtime)),
                voice_activity_runtime,
                parakeet_runtime,
                local_tts_runtime: Mutex::new(local_tts_runtime),
                llama_cpp_runtime,
                llama_cpp_conversation: Mutex::new(Vec::new()),
                llama_cpp_system_prompt,
                opencode_server: Arc::new(Mutex::new(None)),
                active_prompt: Arc::new(Mutex::new(None)),
                active_prompt_generation: AtomicU64::new(0),
            }
        }
        Err(error) => {
            build_startup_error_app_state(fallback_voice_pipeline_config, error.to_string())
        }
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
        local_tts_runtime: Mutex::new(None),
        llama_cpp_runtime: Arc::new(Mutex::new(None)),
        llama_cpp_conversation: Mutex::new(Vec::new()),
        llama_cpp_system_prompt: None,
        opencode_server: Arc::new(Mutex::new(None)),
        active_prompt: Arc::new(Mutex::new(None)),
        active_prompt_generation: AtomicU64::new(0),
    }
}

fn load_llama_cpp_system_prompt() -> Result<String, String> {
    let soul_path =
        voxgolem_core::config::default_soul_path().map_err(|error| error.to_string())?;
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

    Ok(prompt)
}

fn execute_prompt_backend(
    config: &voxgolem_core::config::RuntimeConfig,
    prompt: &str,
    llama_cpp_runtime: &Arc<Mutex<Option<voxgolem_platform::llama_cpp::LlamaCppRuntime>>>,
    llama_cpp_conversation: &Mutex<Vec<LlamaConversationTurn>>,
    llama_cpp_system_prompt: Option<&str>,
) -> Result<PromptExecutionOutcome, String> {
    match &config.response_backend {
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
    parakeet_runtime: &Option<Mutex<transcription::ParakeetRuntime>>,
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
            if let Some(config) = app_state.runtime_config.as_ref() {
                if let voxgolem_core::config::ResponseBackendConfig::Opencode { path } =
                    &config.response_backend
                {
                    match tauri::async_runtime::block_on(
                        voxgolem_platform::opencode::OpencodeServer::start(
                            voxgolem_platform::opencode::OpencodeServerConfig::new(path),
                        ),
                    ) {
                        Ok(server) => {
                            *app_state
                                .opencode_server
                                .lock()
                                .expect("opencode server lock") = Some(server)
                        }
                        Err(error) => {
                            *app_state.startup_state.lock().expect("startup state lock") =
                                StartupStatePayload::Error {
                                    message: format!("failed to start OpenCode server: {error}"),
                                }
                        }
                    }
                }
            }
            app.manage(app_state);
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
            switch_response_profile,
            record_frontend_runtime_diagnostic,
            submit_prompt,
            cancel_prompt,
            record_speech_activity,
            ingest_audio_frame,
            mark_silence,
            reset_session
        ]);

    let app = match builder.build(tauri::generate_context!()) {
        Ok(app) => app,
        Err(error) => {
            eprintln!("failed to build vox-golem tauri shell: {error}");
            std::process::exit(1);
        }
    };

    app.run(|app_handle, event| {
        if matches!(event, tauri::RunEvent::Exit) {
            let app_state = app_handle.state::<AppState>();
            shutdown_llama_cpp_runtime_for_exit(&app_state);
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

#[cfg(test)]
mod tests {
    use super::{
        apply_optional_speech_activity, build_llama_prompt_input, build_mark_silence_response,
        build_startup_error_app_state, current_runtime_phase_response, current_silence_deadline,
        default_response_profile, default_voice_pipeline_config, execute_prompt_backend,
        ingest_audio_frame_with_optional_wake_word_detection, is_llama_context_overflow_error,
        llama_cpp_input_token_limit, load_llama_cpp_system_prompt, load_persisted_state,
        load_persisted_tts_enabled, load_persisted_ui_text_size, load_persisted_ui_theme,
        model_path_for_profile, parse_persisted_state, persist_selected_response_profile,
        persist_tts_enabled, persist_ui_text_size, persist_ui_theme, process_wake_word_frame,
        reset_voice_pipeline_to_waiting, reset_wake_word_runtime, resolve_effective_tts_enabled,
        response_profile_state_path, runtime_log_path, runtime_phase_response_from_state,
        shutdown_llama_cpp_runtime_for_exit, supported_response_profiles, to_runtime_phase_payload,
        transcribe_finished_utterance, transcription_ready_samples, wake_word_event_timestamp,
        LlamaConversationTurn, PromptEventEnvelope, PromptExecutionEventPayload,
        ResponseProfilePayload, RuntimePhasePayload, RuntimePhaseResponsePayload,
        RuntimeTelemetryPayload, UiTextSizePayload, UiThemePayload, DEFAULT_SILENCE_TIMEOUT_MS,
        LLAMA_CPP_ROLLOVER_REASON,
    };
    use crate::wake_word::{WakeWordDetection, WakeWordRuntime};
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Mutex};
    use std::thread;

    static APPDATA_ENV_LOCK: Mutex<()> = Mutex::new(());

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
        let effective = resolve_effective_tts_enabled(false);

        match previous_appdata {
            Some(value) => std::env::set_var("APPDATA", value),
            None => std::env::remove_var("APPDATA"),
        }

        assert!(effective);
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
