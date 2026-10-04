#[cfg(target_os = "android")]
use tauri::Manager;
use tauri::{plugin::Builder, Runtime};

#[cfg(not(target_os = "android"))]
const UNAVAILABLE: &str = "Voice notes are available only in the Android app";

#[derive(Debug, serde::Serialize)]
struct CaptureError {
    message: String,
}

#[cfg(target_os = "android")]
struct CaptureHandle<R: Runtime>(tauri::plugin::PluginHandle<R>);

fn run_command<R: Runtime>(
    app: tauri::AppHandle<R>,
    command: &str,
    arguments: serde_json::Value,
) -> Result<serde_json::Value, CaptureError> {
    #[cfg(target_os = "android")]
    {
        app.state::<CaptureHandle<R>>()
            .0
            .run_mobile_plugin(command, arguments)
            .map_err(|error| CaptureError {
                message: error.to_string(),
            })
    }
    #[cfg(not(target_os = "android"))]
    {
        let _ = (app, command, arguments);
        Err(CaptureError {
            message: UNAVAILABLE.to_owned(),
        })
    }
}

#[tauri::command]
fn snapshot<R: Runtime>(app: tauri::AppHandle<R>) -> Result<serde_json::Value, CaptureError> {
    run_command(app, "snapshot", serde_json::json!({}))
}

#[tauri::command]
fn enable_capture<R: Runtime>(app: tauri::AppHandle<R>) -> Result<serde_json::Value, CaptureError> {
    run_command(app, "enable_capture", serde_json::json!({}))
}

#[tauri::command]
fn list_captures<R: Runtime>(
    app: tauri::AppHandle<R>,
    offset: u32,
    limit: u32,
) -> Result<Vec<serde_json::Value>, CaptureError> {
    let payload = run_command(
        app,
        "list_captures",
        serde_json::json!({ "offset": offset, "limit": limit }),
    )?;
    serde_json::from_value(
        payload
            .get("captures")
            .cloned()
            .unwrap_or(serde_json::Value::Null),
    )
    .map_err(|error| CaptureError {
        message: error.to_string(),
    })
}

#[tauri::command]
fn select_notes_folder<R: Runtime>(
    app: tauri::AppHandle<R>,
) -> Result<serde_json::Value, CaptureError> {
    run_command(app, "select_notes_folder", serde_json::json!({}))
}

#[tauri::command]
fn play_capture<R: Runtime>(
    app: tauri::AppHandle<R>,
    capture_id: String,
) -> Result<(), CaptureError> {
    run_command(
        app,
        "play_capture",
        serde_json::json!({ "captureId": capture_id }),
    )
    .map(|_| ())
}

#[tauri::command]
fn stop_playback<R: Runtime>(app: tauri::AppHandle<R>) -> Result<(), CaptureError> {
    run_command(app, "stop_playback", serde_json::json!({})).map(|_| ())
}

#[tauri::command]
fn retry_capture<R: Runtime>(
    app: tauri::AppHandle<R>,
    capture_id: String,
) -> Result<(), CaptureError> {
    run_command(
        app,
        "retry_capture",
        serde_json::json!({ "captureId": capture_id }),
    )
    .map(|_| ())
}

#[tauri::command]
fn finalize_capture<R: Runtime>(
    app: tauri::AppHandle<R>,
    capture_id: String,
) -> Result<(), CaptureError> {
    run_command(
        app,
        "finalize_capture",
        serde_json::json!({ "captureId": capture_id }),
    )
    .map(|_| ())
}

#[tauri::command]
fn update_transcript<R: Runtime>(
    app: tauri::AppHandle<R>,
    capture_id: String,
    text: String,
) -> Result<(), CaptureError> {
    run_command(
        app,
        "update_transcript",
        serde_json::json!({ "captureId": capture_id, "text": text }),
    )
    .map(|_| ())
}

pub fn init<R: Runtime>() -> tauri::plugin::TauriPlugin<R> {
    Builder::new("capture")
        .invoke_handler(tauri::generate_handler![
            snapshot,
            enable_capture,
            list_captures,
            select_notes_folder,
            play_capture,
            stop_playback,
            retry_capture,
            finalize_capture,
            update_transcript
        ])
        .setup(|app, api| {
            #[cfg(target_os = "android")]
            {
                let handle =
                    api.register_android_plugin("dev.gitblame.voxgolem.capture", "CapturePlugin")?;
                app.manage(CaptureHandle(handle));
            }
            #[cfg(not(target_os = "android"))]
            let _ = (app, api);
            Ok(())
        })
        .build()
}
