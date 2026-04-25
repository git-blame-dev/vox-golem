#![forbid(unsafe_code)]
#![deny(unused_must_use)]

use serde::{Deserialize, Serialize};
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, TryLockError};
use std::time::{SystemTime, UNIX_EPOCH};
use tauri::{path::BaseDirectory, Manager};

mod livekit_wakeword;
mod transcription;
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
const LLAMA_CPP_ROLLOVER_REASON: &str =
    "Context budget reached; started a new local Gemma conversation for this reply.";

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
    llama_cpp_runtime: Arc<Mutex<Option<voxgolem_platform::llama_cpp::LlamaCppRuntime>>>,
    llama_cpp_conversation: Mutex<Vec<LlamaConversationTurn>>,
    llama_cpp_system_prompt: Option<String>,
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
    StepStart,
    StepFinish {
        reason: Option<String>,
    },
    Error {
        name: String,
        message: String,
    },
    ToolUse {
        tool: String,
        status: String,
        detail: String,
    },
}

#[derive(Clone, Debug, Serialize)]
struct PromptExecutionPayload {
    events: Vec<PromptExecutionEventPayload>,
    stderr: String,
    exit_code: Option<i32>,
    runtime_phase: RuntimePhasePayload,
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

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
struct SwitchResponseProfilePayload {
    selected_response_profile: ResponseProfilePayload,
    supported_response_profiles: Vec<ResponseProfilePayload>,
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
    },
    Ready {
        cue_asset_paths: CueAssetPathsPayload,
        runtime_phase: RuntimePhasePayload,
        voice_input_available: bool,
        voice_input_error: Option<String>,
        silence_timeout_ms: u64,
        selected_response_profile: ResponseProfilePayload,
        supported_response_profiles: Vec<ResponseProfilePayload>,
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

    {
        let mut runtime = app_state
            .llama_cpp_runtime
            .lock()
            .map_err(|_| String::from("local llama.cpp runtime lock is poisoned"))?;
        *runtime = None;
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
        };
    }

    std::thread::spawn(move || {
        let start_result = voxgolem_platform::llama_cpp::LlamaCppRuntime::start(server_spec);
        if response_profile_switch_generation.load(Ordering::SeqCst) != switch_generation {
            return;
        }

        let next_state = match start_result {
            Ok(runtime) => {
                if let Ok(mut guard) = llama_cpp_runtime.lock() {
                    *guard = Some(runtime);
                }

                if let Err(error) = persist_selected_response_profile(profile) {
                    eprintln!("failed to persist response profile state: {error}");
                }

                if let Ok(mut selected) = selected_response_profile.lock() {
                    *selected = profile;
                }

                startup_ready_state_from_snapshot(&startup_snapshot, profile)
            }
            Err(error) => match voxgolem_platform::llama_cpp::LlamaCppRuntime::start(
                fallback_server_spec,
            ) {
                Ok(runtime) => {
                    if let Ok(mut guard) = llama_cpp_runtime.lock() {
                        *guard = Some(runtime);
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
            },
        };

        if let Ok(mut guard) = startup_state.lock() {
            *guard = next_state;
        }
    });

    Ok(response)
}

#[tauri::command]
fn submit_prompt(
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

#[tauri::command]
fn begin_listening(
    app_state: tauri::State<'_, AppState>,
) -> Result<RuntimePhaseResponsePayload, String> {
    let _operation_guard =
        lock_response_backend_operation(&app_state.response_backend_operation_lock)?;
    ensure_startup_ready_for_prompt(&app_state.startup_state)?;
    let now_ms = current_time_ms()?;

    apply_voice_pipeline_transition_with_input_runtime_reset(
        &app_state.voice_pipeline_state,
        &app_state.wake_word_runtime,
        &app_state.voice_activity_runtime,
        app_state.voice_pipeline_config,
        voxgolem_core::voice_pipeline::VoicePipelineEvent::WakeWordDetected { now_ms },
    )?;

    Ok(RuntimePhaseResponsePayload {
        ..current_runtime_phase_response(&app_state.voice_pipeline_state, None, None)?
    })
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
fn reset_session(
    app_state: tauri::State<'_, AppState>,
) -> Result<RuntimePhaseResponsePayload, String> {
    let _operation_guard =
        lock_response_backend_operation(&app_state.response_backend_operation_lock)?;
    ensure_startup_ready_for_prompt(&app_state.startup_state)?;
    apply_voice_pipeline_transition_with_input_runtime_reset(
        &app_state.voice_pipeline_state,
        &app_state.wake_word_runtime,
        &app_state.voice_activity_runtime,
        app_state.voice_pipeline_config,
        voxgolem_core::voice_pipeline::VoicePipelineEvent::ResetToIdle,
    )?;

    if let Ok(mut conversation) = app_state.llama_cpp_conversation.lock() {
        conversation.clear();
    }

    Ok(RuntimePhaseResponsePayload {
        ..current_runtime_phase_response(&app_state.voice_pipeline_state, None, None)?
    })
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
            ..
        }
        | StartupStatePayload::Ready {
            cue_asset_paths,
            voice_input_available,
            voice_input_error,
            silence_timeout_ms,
            ..
        } => Ok(StartupSnapshot {
            cue_asset_paths: cue_asset_paths.clone(),
            voice_input_available: *voice_input_available,
            voice_input_error: voice_input_error.clone(),
            silence_timeout_ms: *silence_timeout_ms,
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

fn response_profile_state_path() -> Result<PathBuf, String> {
    let config_path = voxgolem_core::config::default_config_path()
        .map_err(|error| format!("failed to resolve %APPDATA%\\VoxGolem\\config.toml: {error}"))?;

    Ok(config_path.with_file_name(RESPONSE_PROFILE_STATE_FILE))
}

fn load_selected_response_profile() -> Result<Option<ResponseProfilePayload>, String> {
    let state_path = response_profile_state_path()?;
    let contents = match fs::read_to_string(&state_path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(format!(
                "failed to read response profile state {}: {error}",
                state_path.display()
            ));
        }
    };

    parse_selected_response_profile(&contents)
}

fn parse_selected_response_profile(
    contents: &str,
) -> Result<Option<ResponseProfilePayload>, String> {
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
            return match value.as_str() {
                "fast" => Ok(Some(ResponseProfilePayload::Fast)),
                "quality" => Ok(Some(ResponseProfilePayload::Quality)),
                _ => Err(format!(
                    "invalid state.toml: unsupported selected_response_profile `{value}`"
                )),
            };
        }
    }

    Ok(None)
}

fn persist_selected_response_profile(profile: ResponseProfilePayload) -> Result<(), String> {
    let state_path = response_profile_state_path()?;
    if let Some(parent) = state_path.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            format!(
                "failed to create response profile state directory {}: {error}",
                parent.display()
            )
        })?;
    }

    fs::write(
        &state_path,
        format!("selected_response_profile = \"{}\"\n", profile.as_str()),
    )
    .map_err(|error| {
        format!(
            "failed to write response profile state {}: {error}",
            state_path.display()
        )
    })
}

fn build_app_state<R: tauri::Runtime>(app: &tauri::AppHandle<R>) -> AppState {
    let fallback_voice_pipeline_config = default_voice_pipeline_config();
    let cue_asset_paths = match resolve_bundled_cue_asset_paths(app) {
        Ok(cue_asset_paths) => cue_asset_paths,
        Err(error) => return build_startup_error_app_state(fallback_voice_pipeline_config, error),
    };

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
            let wake_word_runtime =
                match wake_word::WakeWordRuntime::new(&config.wake_word_model_path) {
                    Ok(runtime) => runtime,
                    Err(error) => {
                        return build_startup_error_app_state(
                            voice_pipeline_config,
                            format!("failed to initialize wake word detector: {error}"),
                        );
                    }
                };
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
                        return;
                    }

                    let next_state = match start_result {
                        Ok(runtime) => {
                            if let Ok(mut guard) = llama_cpp_runtime.lock() {
                                *guard = Some(runtime);
                            }

                            StartupStatePayload::Ready {
                                cue_asset_paths,
                                runtime_phase: RuntimePhasePayload::Sleeping,
                                voice_input_available,
                                voice_input_error,
                                silence_timeout_ms,
                                selected_response_profile,
                                supported_response_profiles,
                            }
                        }
                        Err(error) => StartupStatePayload::Error {
                            message: format!(
                                "failed to initialize local llama.cpp runtime: {error}"
                            ),
                        },
                    };

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
                llama_cpp_runtime,
                llama_cpp_conversation: Mutex::new(Vec::new()),
                llama_cpp_system_prompt,
            }
        }
        Err(error) => {
            build_startup_error_app_state(fallback_voice_pipeline_config, error.to_string())
        }
    }
}

fn resolve_bundled_cue_asset_paths<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
) -> Result<CueAssetPathsPayload, String> {
    let start_listening = app
        .path()
        .resolve("resources/start-listening.wav", BaseDirectory::Resource)
        .map_err(|error| format!("failed to resolve bundled start-listening cue: {error}"))?;
    let stop_listening = app
        .path()
        .resolve("resources/stop-listening.wav", BaseDirectory::Resource)
        .map_err(|error| format!("failed to resolve bundled stop-listening cue: {error}"))?;

    if !start_listening.is_file() {
        return Err(format!(
            "bundled start-listening cue is missing at {}",
            start_listening.display()
        ));
    }

    if !stop_listening.is_file() {
        return Err(format!(
            "bundled stop-listening cue is missing at {}",
            stop_listening.display()
        ));
    }

    Ok(CueAssetPathsPayload {
        start_listening: String::from("resources/start-listening.wav"),
        stop_listening: String::from("resources/stop-listening.wav"),
    })
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
        llama_cpp_runtime: Arc::new(Mutex::new(None)),
        llama_cpp_conversation: Mutex::new(Vec::new()),
        llama_cpp_system_prompt: None,
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

fn prompt_result_error_message(
    result: &voxgolem_platform::opencode::OpencodeJsonRunResult,
) -> Option<String> {
    if let Some(exit_code) = result.exit_code {
        if exit_code != 0 {
            return Some(format!("opencode exited with code {exit_code}"));
        }
    }

    result.events.iter().find_map(|event| match event {
        voxgolem_platform::opencode::OpencodeJsonEvent::Error { message, .. } => {
            Some(message.clone())
        }
        _ => None,
    })
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
        voxgolem_core::config::ResponseBackendConfig::Opencode { path } => {
            let prompt = voxgolem_platform::opencode::OpencodePrompt::new(prompt.to_string())
                .map_err(|error| format!("invalid prompt: {error:?}"))?;
            let spec = voxgolem_platform::opencode::OpencodeCommandSpec::new(path.clone(), prompt)
                .with_output_format(voxgolem_platform::opencode::OpencodeOutputFormat::Json);
            let result = voxgolem_platform::opencode::run_opencode_json(&spec)
                .map_err(|error| format!("failed to execute opencode: {error}"))?;
            let error_message = prompt_result_error_message(&result);
            let voxgolem_platform::opencode::OpencodeJsonRunResult {
                events,
                stderr,
                exit_code,
            } = result;

            Ok(PromptExecutionOutcome {
                events: map_opencode_events(events),
                stderr,
                exit_code,
                error_message,
            })
        }
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

fn map_opencode_events(
    events: Vec<voxgolem_platform::opencode::OpencodeJsonEvent>,
) -> Vec<PromptExecutionEventPayload> {
    events
        .into_iter()
        .map(|event| match event {
            voxgolem_platform::opencode::OpencodeJsonEvent::Text { text } => {
                PromptExecutionEventPayload::Text { text }
            }
            voxgolem_platform::opencode::OpencodeJsonEvent::Reasoning { text } => {
                PromptExecutionEventPayload::Reasoning { text }
            }
            voxgolem_platform::opencode::OpencodeJsonEvent::StepStart => {
                PromptExecutionEventPayload::StepStart
            }
            voxgolem_platform::opencode::OpencodeJsonEvent::StepFinish { reason } => {
                PromptExecutionEventPayload::StepFinish { reason }
            }
            voxgolem_platform::opencode::OpencodeJsonEvent::Error { name, message } => {
                PromptExecutionEventPayload::Error { name, message }
            }
            voxgolem_platform::opencode::OpencodeJsonEvent::ToolUse {
                tool,
                status,
                detail,
            } => PromptExecutionEventPayload::ToolUse {
                tool,
                status: match status {
                    voxgolem_platform::opencode::OpencodeToolUseStatus::Completed => {
                        "completed".to_string()
                    }
                    voxgolem_platform::opencode::OpencodeToolUseStatus::Error => {
                        "error".to_string()
                    }
                },
                detail,
            },
        })
        .collect()
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

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let builder = tauri::Builder::default()
        .setup(|app| {
            let app_state = build_app_state(app.handle());
            app.manage(app_state);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_startup_state,
            switch_response_profile,
            submit_prompt,
            begin_listening,
            record_speech_activity,
            ingest_audio_frame,
            mark_silence,
            reset_session
        ]);

    if let Err(error) = builder.run(tauri::generate_context!()) {
        eprintln!("failed to run vox-golem tauri shell: {error}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::{
        apply_optional_speech_activity, build_llama_prompt_input, build_mark_silence_response,
        build_startup_error_app_state, current_runtime_phase_response, current_silence_deadline,
        default_response_profile, default_voice_pipeline_config, execute_prompt_backend,
        ingest_audio_frame_with_optional_wake_word_detection, is_llama_context_overflow_error,
        llama_cpp_input_token_limit, load_llama_cpp_system_prompt, model_path_for_profile,
        parse_selected_response_profile, persist_selected_response_profile,
        process_wake_word_frame, prompt_result_error_message, reset_voice_pipeline_to_waiting,
        reset_wake_word_runtime, response_profile_state_path, runtime_phase_response_from_state,
        supported_response_profiles, to_runtime_phase_payload, transcribe_finished_utterance,
        transcription_ready_samples, wake_word_event_timestamp, LlamaConversationTurn,
        PromptExecutionEventPayload, ResponseProfilePayload, RuntimePhasePayload,
        RuntimePhaseResponsePayload, RuntimeTelemetryPayload, DEFAULT_SILENCE_TIMEOUT_MS,
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
    fn prompt_result_error_message_prefers_non_zero_exit_code() {
        let result = voxgolem_platform::opencode::OpencodeJsonRunResult {
            events: vec![voxgolem_platform::opencode::OpencodeJsonEvent::Error {
                name: "APIError".to_string(),
                message: "provider failed".to_string(),
            }],
            stderr: String::new(),
            exit_code: Some(7),
        };

        assert_eq!(
            prompt_result_error_message(&result),
            Some("opencode exited with code 7".to_string())
        );
    }

    #[test]
    fn prompt_result_error_message_uses_structured_error_when_exit_code_is_zero() {
        let result = voxgolem_platform::opencode::OpencodeJsonRunResult {
            events: vec![voxgolem_platform::opencode::OpencodeJsonEvent::Error {
                name: "APIError".to_string(),
                message: "provider failed".to_string(),
            }],
            stderr: String::new(),
            exit_code: Some(0),
        };

        assert_eq!(
            prompt_result_error_message(&result),
            Some("provider failed".to_string())
        );
    }

    #[test]
    fn prompt_result_error_message_returns_none_for_successful_run() {
        let result = voxgolem_platform::opencode::OpencodeJsonRunResult {
            events: vec![voxgolem_platform::opencode::OpencodeJsonEvent::Text {
                text: "done".to_string(),
            }],
            stderr: String::new(),
            exit_code: Some(0),
        };

        assert_eq!(prompt_result_error_message(&result), None);
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
    fn parse_selected_response_profile_supports_fast_and_quality() {
        assert_eq!(
            parse_selected_response_profile("selected_response_profile = \"fast\"\n")
                .expect("fast profile should parse"),
            Some(ResponseProfilePayload::Fast)
        );
        assert_eq!(
            parse_selected_response_profile("selected_response_profile = \"quality\"\n")
                .expect("quality profile should parse"),
            Some(ResponseProfilePayload::Quality)
        );
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

        let error = WakeWordRuntime::new(&wake_word_model_path)
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
