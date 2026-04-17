#![forbid(unsafe_code)]
#![deny(unused_must_use)]

use serde::Serialize;

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum StartupStatePayload {
    Ready,
    Error { message: String },
}

#[tauri::command]
fn get_startup_state(
    startup_state: tauri::State<'_, StartupStatePayload>,
) -> StartupStatePayload {
    startup_state.inner().clone()
}

fn resolve_startup_state() -> StartupStatePayload {
    match voxgolem_core::config::load_runtime_config(None) {
        Ok(_) => StartupStatePayload::Ready,
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
        .invoke_handler(tauri::generate_handler![get_startup_state]);

    if let Err(error) = builder.run(tauri::generate_context!()) {
        eprintln!("failed to run vox-golem tauri shell: {error}");
        std::process::exit(1);
    }
}
