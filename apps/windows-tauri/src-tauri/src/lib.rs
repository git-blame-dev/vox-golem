#![forbid(unsafe_code)]
#![deny(unused_must_use)]

use serde::Serialize;
use std::sync::Mutex;

const DEFAULT_SILENCE_TIMEOUT_MS: u64 = 1_200;
const DEFAULT_PREROLL_MAX_SAMPLES: usize = 4_000;
const DEFAULT_UTTERANCE_MAX_SAMPLES: usize = 160_000;

struct AppState {
    startup_state: StartupStatePayload,
    runtime_config: Option<voxgolem_core::config::RuntimeConfig>,
    voice_pipeline_config: voxgolem_core::voice_pipeline::VoicePipelineConfig,
    voice_pipeline_state: Mutex<voxgolem_core::voice_pipeline::VoicePipelineState>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
enum RuntimePhasePayload {
    Initializing,
    Sleeping,
    Listening,
    Processing,
    Executing,
    ResultReady,
    Error,
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

#[derive(Clone, Debug, Serialize)]
struct RuntimePhaseResponsePayload {
    runtime_phase: RuntimePhasePayload,
    transcription_ready_samples: Option<usize>,
}

#[derive(Clone, Debug, Serialize)]
struct CueAssetPathsPayload {
    start_listening: String,
    stop_listening: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum StartupStatePayload {
    Ready {
        cue_asset_paths: CueAssetPathsPayload,
        runtime_phase: RuntimePhasePayload,
    },
    Error {
        message: String,
    },
}

#[tauri::command]
fn get_startup_state(app_state: tauri::State<'_, AppState>) -> StartupStatePayload {
    app_state.startup_state.clone()
}

#[tauri::command]
fn submit_prompt(
    prompt: String,
    app_state: tauri::State<'_, AppState>,
) -> Result<PromptExecutionPayload, String> {
    apply_voice_pipeline_transition(
        &app_state.voice_pipeline_state,
        app_state.voice_pipeline_config,
        voxgolem_core::voice_pipeline::VoicePipelineEvent::SubmitPrompt,
    )?;

    let config = app_state
        .runtime_config
        .as_ref()
        .ok_or_else(|| String::from("startup config is not ready"))?;
    let prompt = voxgolem_platform::opencode::OpencodePrompt::new(prompt)
        .map_err(|error| format!("invalid prompt: {error:?}"))?;
    let spec =
        voxgolem_platform::opencode::OpencodeCommandSpec::new(config.opencode_path.clone(), prompt)
            .with_output_format(voxgolem_platform::opencode::OpencodeOutputFormat::Json);
    let result = match voxgolem_platform::opencode::run_opencode_json(&spec) {
        Ok(result) => result,
        Err(error) => {
            apply_voice_pipeline_transition(
                &app_state.voice_pipeline_state,
                app_state.voice_pipeline_config,
                voxgolem_core::voice_pipeline::VoicePipelineEvent::PromptFailed {
                    message: error.to_string(),
                },
            )?;

            return Err(format!("failed to execute opencode: {error}"));
        }
    };

    let result_error = prompt_result_error_message(&result);

    let completion_event = match result_error {
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
        events: result
            .events
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
            .collect(),
        stderr: result.stderr,
        exit_code: result.exit_code,
        runtime_phase,
    })
}

#[tauri::command]
fn begin_listening(
    app_state: tauri::State<'_, AppState>,
) -> Result<RuntimePhaseResponsePayload, String> {
    apply_voice_pipeline_transition(
        &app_state.voice_pipeline_state,
        app_state.voice_pipeline_config,
        voxgolem_core::voice_pipeline::VoicePipelineEvent::WakeWordDetected { now_ms: 0 },
    )?;

    Ok(RuntimePhaseResponsePayload {
        runtime_phase: current_runtime_phase(&app_state.voice_pipeline_state)?,
        transcription_ready_samples: None,
    })
}

#[tauri::command]
fn mark_silence(
    app_state: tauri::State<'_, AppState>,
) -> Result<RuntimePhaseResponsePayload, String> {
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

    Ok(RuntimePhaseResponsePayload {
        runtime_phase: current_runtime_phase(&app_state.voice_pipeline_state)?,
        transcription_ready_samples: transcription_ready_samples(&action),
    })
}

#[tauri::command]
fn mark_result_ready(
    app_state: tauri::State<'_, AppState>,
) -> Result<RuntimePhaseResponsePayload, String> {
    apply_voice_pipeline_transition(
        &app_state.voice_pipeline_state,
        app_state.voice_pipeline_config,
        voxgolem_core::voice_pipeline::VoicePipelineEvent::PromptCompleted,
    )?;

    Ok(RuntimePhaseResponsePayload {
        runtime_phase: current_runtime_phase(&app_state.voice_pipeline_state)?,
        transcription_ready_samples: None,
    })
}

#[tauri::command]
fn reset_session(
    app_state: tauri::State<'_, AppState>,
) -> Result<RuntimePhaseResponsePayload, String> {
    apply_voice_pipeline_transition(
        &app_state.voice_pipeline_state,
        app_state.voice_pipeline_config,
        voxgolem_core::voice_pipeline::VoicePipelineEvent::ResetToIdle,
    )?;

    Ok(RuntimePhaseResponsePayload {
        runtime_phase: current_runtime_phase(&app_state.voice_pipeline_state)?,
        transcription_ready_samples: None,
    })
}

fn build_app_state() -> AppState {
    let voice_pipeline_config = default_voice_pipeline_config();

    match voxgolem_core::config::load_runtime_config(None) {
        Ok(config) => {
            let voice_pipeline_state = apply_voice_pipeline_event_or_panic(
                voxgolem_core::voice_pipeline::VoicePipelineState::new(voice_pipeline_config)
                    .expect("voice pipeline should initialize with valid constants"),
                voice_pipeline_config,
                voxgolem_core::voice_pipeline::VoicePipelineEvent::StartupValidated,
                "startup validation should initialize the session to sleeping",
            );

            AppState {
                startup_state: StartupStatePayload::Ready {
                    cue_asset_paths: CueAssetPathsPayload {
                        start_listening: config.start_listening_cue.to_string_lossy().into_owned(),
                        stop_listening: config.stop_listening_cue.to_string_lossy().into_owned(),
                    },
                    runtime_phase: RuntimePhasePayload::Sleeping,
                },
                runtime_config: Some(config),
                voice_pipeline_config,
                voice_pipeline_state: Mutex::new(voice_pipeline_state),
            }
        }
        Err(error) => {
            let message = error.to_string();
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
                startup_state: StartupStatePayload::Error { message },
                runtime_config: None,
                voice_pipeline_config,
                voice_pipeline_state: Mutex::new(voice_pipeline_state),
            }
        }
    }
}

fn current_runtime_phase(
    voice_pipeline_state: &Mutex<voxgolem_core::voice_pipeline::VoicePipelineState>,
) -> Result<RuntimePhasePayload, String> {
    let guard = voice_pipeline_state
        .lock()
        .map_err(|_| String::from("voice pipeline lock is poisoned"))?;

    Ok(to_runtime_phase_payload(guard.session().runtime().phase()))
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

fn to_runtime_phase_payload(
    runtime_phase: voxgolem_core::runtime::RuntimePhase,
) -> RuntimePhasePayload {
    match runtime_phase {
        voxgolem_core::runtime::RuntimePhase::Initializing => RuntimePhasePayload::Initializing,
        voxgolem_core::runtime::RuntimePhase::Sleeping => RuntimePhasePayload::Sleeping,
        voxgolem_core::runtime::RuntimePhase::Listening => RuntimePhasePayload::Listening,
        voxgolem_core::runtime::RuntimePhase::Processing => RuntimePhasePayload::Processing,
        voxgolem_core::runtime::RuntimePhase::Executing => RuntimePhasePayload::Executing,
        voxgolem_core::runtime::RuntimePhase::ResultReady => RuntimePhasePayload::ResultReady,
        voxgolem_core::runtime::RuntimePhase::Error => RuntimePhasePayload::Error,
    }
}

fn default_voice_pipeline_config() -> voxgolem_core::voice_pipeline::VoicePipelineConfig {
    let voice_turn = voxgolem_core::voice_turn::VoiceTurnConfig::new(DEFAULT_SILENCE_TIMEOUT_MS)
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

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let app_state = build_app_state();
    let builder =
        tauri::Builder::default()
            .manage(app_state)
            .invoke_handler(tauri::generate_handler![
                get_startup_state,
                submit_prompt,
                begin_listening,
                mark_silence,
                mark_result_ready,
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
        current_silence_deadline, default_voice_pipeline_config, prompt_result_error_message,
        to_runtime_phase_payload, transcription_ready_samples, RuntimePhasePayload,
        DEFAULT_SILENCE_TIMEOUT_MS,
    };
    use std::sync::Mutex;

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
    fn maps_core_runtime_phase_to_payload() {
        assert!(matches!(
            to_runtime_phase_payload(voxgolem_core::runtime::RuntimePhase::Processing),
            RuntimePhasePayload::Processing
        ));
    }

    #[test]
    fn current_silence_deadline_uses_last_activity_plus_timeout() {
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
        let locked_state = Mutex::new(listening_state.0);

        assert_eq!(
            current_silence_deadline(&locked_state, voice_pipeline_config),
            Ok(DEFAULT_SILENCE_TIMEOUT_MS + 100)
        );
    }

    #[test]
    fn transcription_ready_samples_reads_finished_utterance_length() {
        let action = voxgolem_core::voice_pipeline::VoicePipelineAction::FinishedUtterance {
            transcription_input: voxgolem_model::parakeet::ParakeetTranscriptionInput::new(
                voxgolem_model::parakeet::PARAKEET_SAMPLE_RATE_HZ,
                vec![0.1, 0.2, 0.3],
            )
            .expect("valid transcription input"),
        };

        assert_eq!(transcription_ready_samples(&action), Some(3));
    }
}
