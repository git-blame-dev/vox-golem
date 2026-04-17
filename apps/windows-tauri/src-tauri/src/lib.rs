#![forbid(unsafe_code)]
#![deny(unused_must_use)]

use serde::Serialize;

struct AppState {
    startup_state: StartupStatePayload,
    runtime_config: Option<voxgolem_core::config::RuntimeConfig>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum PromptExecutionEventPayload {
    Text {
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
    let config = app_state
        .runtime_config
        .as_ref()
        .ok_or_else(|| String::from("startup config is not ready"))?;
    let prompt = voxgolem_platform::opencode::OpencodePrompt::new(prompt)
        .map_err(|error| format!("invalid prompt: {error:?}"))?;
    let spec =
        voxgolem_platform::opencode::OpencodeCommandSpec::new(config.opencode_path.clone(), prompt)
            .with_output_format(voxgolem_platform::opencode::OpencodeOutputFormat::Json);
    let result = voxgolem_platform::opencode::run_opencode_json(&spec)
        .map_err(|error| format!("failed to execute opencode: {error}"))?;

    Ok(PromptExecutionPayload {
        events: result
            .events
            .into_iter()
            .map(|event| match event {
                voxgolem_platform::opencode::OpencodeJsonEvent::Text { text } => {
                    PromptExecutionEventPayload::Text { text }
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
    })
}

fn build_app_state() -> AppState {
    match voxgolem_core::config::load_runtime_config(None) {
        Ok(config) => AppState {
            startup_state: StartupStatePayload::Ready {
                cue_asset_paths: CueAssetPathsPayload {
                    start_listening: config.start_listening_cue.to_string_lossy().into_owned(),
                    stop_listening: config.stop_listening_cue.to_string_lossy().into_owned(),
                },
            },
            runtime_config: Some(config),
        },
        Err(error) => AppState {
            startup_state: StartupStatePayload::Error {
                message: error.to_string(),
            },
            runtime_config: None,
        },
    }
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
