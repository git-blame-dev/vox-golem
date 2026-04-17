#![forbid(unsafe_code)]
#![deny(unused_must_use)]

use serde::Serialize;

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum PromptExecutionEventPayload {
    Text { text: String },
    Error { name: String, message: String },
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
fn get_startup_state(startup_state: tauri::State<'_, StartupStatePayload>) -> StartupStatePayload {
    startup_state.inner().clone()
}

#[tauri::command]
fn submit_prompt(prompt: String) -> Result<PromptExecutionPayload, String> {
    let config =
        voxgolem_core::config::load_runtime_config(None).map_err(|error| error.to_string())?;
    let prompt = voxgolem_platform::opencode::OpencodePrompt::new(prompt)
        .map_err(|error| format!("invalid prompt: {error:?}"))?;
    let spec = voxgolem_platform::opencode::OpencodeCommandSpec::new(config.opencode_path, prompt)
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
                voxgolem_platform::opencode::OpencodeJsonEvent::Error { name, message } => {
                    PromptExecutionEventPayload::Error { name, message }
                }
            })
            .collect(),
        stderr: result.stderr,
        exit_code: result.exit_code,
    })
}

fn resolve_startup_state() -> StartupStatePayload {
    match voxgolem_core::config::load_runtime_config(None) {
        Ok(config) => StartupStatePayload::Ready {
            cue_asset_paths: CueAssetPathsPayload {
                start_listening: config.start_listening_cue.to_string_lossy().into_owned(),
                stop_listening: config.stop_listening_cue.to_string_lossy().into_owned(),
            },
        },
        Err(error) => StartupStatePayload::Error {
            message: error.to_string(),
        },
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let startup_state = resolve_startup_state();
    let builder = tauri::Builder::default()
        .manage(startup_state)
        .invoke_handler(tauri::generate_handler![get_startup_state, submit_prompt]);

    if let Err(error) = builder.run(tauri::generate_context!()) {
        eprintln!("failed to run vox-golem tauri shell: {error}");
        std::process::exit(1);
    }
}
