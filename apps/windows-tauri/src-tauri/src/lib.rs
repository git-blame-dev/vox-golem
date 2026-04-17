#![forbid(unsafe_code)]
#![deny(unused_must_use)]

use serde::Serialize;
use std::sync::Mutex;

const DEFAULT_SILENCE_TIMEOUT_MS: u64 = 1_200;

struct AppState {
    startup_state: StartupStatePayload,
    runtime_config: Option<voxgolem_core::config::RuntimeConfig>,
    session_config: voxgolem_core::session::SessionConfig,
    session_state: Mutex<voxgolem_core::session::SessionState>,
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
    apply_session_transition(
        &app_state.session_state,
        app_state.session_config,
        voxgolem_core::session::SessionEvent::SubmitPrompt,
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
            apply_session_transition(
                &app_state.session_state,
                app_state.session_config,
                voxgolem_core::session::SessionEvent::PromptFailed {
                    message: error.to_string(),
                },
            )?;

            return Err(format!("failed to execute opencode: {error}"));
        }
    };

    let result_error = prompt_result_error_message(&result);

    let completion_event = match result_error {
        Some(message) => voxgolem_core::session::SessionEvent::PromptFailed { message },
        None => voxgolem_core::session::SessionEvent::PromptCompleted,
    };

    apply_session_transition(
        &app_state.session_state,
        app_state.session_config,
        completion_event,
    )?;

    let runtime_phase = current_runtime_phase(&app_state.session_state)?;

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

fn build_app_state() -> AppState {
    let session_config = default_session_config();

    match voxgolem_core::config::load_runtime_config(None) {
        Ok(config) => {
            let session_state = apply_session_event_or_panic(
                voxgolem_core::session::SessionState::new(),
                session_config,
                voxgolem_core::session::SessionEvent::StartupValidated,
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
                session_config,
                session_state: Mutex::new(session_state),
            }
        }
        Err(error) => {
            let message = error.to_string();
            let session_state = apply_session_event_or_panic(
                voxgolem_core::session::SessionState::new(),
                session_config,
                voxgolem_core::session::SessionEvent::StartupFailed {
                    message: message.clone(),
                },
                "startup failure should initialize the session to error",
            );

            AppState {
                startup_state: StartupStatePayload::Error { message },
                runtime_config: None,
                session_config,
                session_state: Mutex::new(session_state),
            }
        }
    }
}

fn current_runtime_phase(
    session_state: &Mutex<voxgolem_core::session::SessionState>,
) -> Result<RuntimePhasePayload, String> {
    let guard = session_state
        .lock()
        .map_err(|_| String::from("session state lock is poisoned"))?;

    Ok(to_runtime_phase_payload(guard.runtime().phase()))
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

fn default_session_config() -> voxgolem_core::session::SessionConfig {
    let voice_turn = voxgolem_core::voice_turn::VoiceTurnConfig::new(DEFAULT_SILENCE_TIMEOUT_MS)
        .expect("silence timeout constant should be valid");

    voxgolem_core::session::SessionConfig::new(voice_turn)
}

fn apply_session_transition(
    session_state: &Mutex<voxgolem_core::session::SessionState>,
    session_config: voxgolem_core::session::SessionConfig,
    event: voxgolem_core::session::SessionEvent,
) -> Result<(), String> {
    let mut guard = session_state
        .lock()
        .map_err(|_| String::from("session state lock is poisoned"))?;

    let next_state = voxgolem_core::session::apply_session_event(&guard, session_config, event)
        .map_err(|error| format!("session transition failed: {error:?}"))?;

    *guard = next_state;
    Ok(())
}

fn apply_session_event_or_panic(
    state: voxgolem_core::session::SessionState,
    config: voxgolem_core::session::SessionConfig,
    event: voxgolem_core::session::SessionEvent,
    message: &str,
) -> voxgolem_core::session::SessionState {
    voxgolem_core::session::apply_session_event(&state, config, event).expect(message)
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

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let app_state = build_app_state();
    let builder = tauri::Builder::default()
        .manage(app_state)
        .invoke_handler(tauri::generate_handler![get_startup_state, submit_prompt]);

    if let Err(error) = builder.run(tauri::generate_context!()) {
        eprintln!("failed to run vox-golem tauri shell: {error}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::{prompt_result_error_message, to_runtime_phase_payload, RuntimePhasePayload};

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
}
