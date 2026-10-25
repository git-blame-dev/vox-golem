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

async fn run_command<R: Runtime>(
    app: tauri::AppHandle<R>,
    command: &str,
    arguments: serde_json::Value,
) -> Result<serde_json::Value, CaptureError> {
    #[cfg(target_os = "android")]
    {
        app.state::<CaptureHandle<R>>()
            .0
            .run_mobile_plugin_async(command, arguments)
            .await
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
async fn snapshot<R: Runtime>(app: tauri::AppHandle<R>) -> Result<serde_json::Value, CaptureError> {
    run_command(app, "snapshot", serde_json::json!({})).await
}

#[tauri::command]
async fn enable_capture<R: Runtime>(
    app: tauri::AppHandle<R>,
) -> Result<serde_json::Value, CaptureError> {
    run_command(app, "enable_capture", serde_json::json!({})).await
}

#[tauri::command]
async fn list_captures<R: Runtime>(
    app: tauri::AppHandle<R>,
    offset: u32,
    limit: u32,
) -> Result<Vec<serde_json::Value>, CaptureError> {
    let payload = run_command(
        app,
        "list_captures",
        serde_json::json!({ "offset": offset, "limit": limit }),
    )
    .await?;
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
async fn select_notes_folder<R: Runtime>(
    app: tauri::AppHandle<R>,
) -> Result<serde_json::Value, CaptureError> {
    run_command(app, "select_notes_folder", serde_json::json!({})).await
}

#[tauri::command]
async fn play_capture<R: Runtime>(
    app: tauri::AppHandle<R>,
    capture_id: String,
) -> Result<(), CaptureError> {
    run_command(
        app,
        "play_capture",
        serde_json::json!({ "captureId": capture_id }),
    )
    .await
    .map(|_| ())
}

#[tauri::command]
async fn stop_playback<R: Runtime>(app: tauri::AppHandle<R>) -> Result<(), CaptureError> {
    run_command(app, "stop_playback", serde_json::json!({}))
        .await
        .map(|_| ())
}

#[tauri::command]
async fn retry_capture<R: Runtime>(
    app: tauri::AppHandle<R>,
    capture_id: String,
) -> Result<(), CaptureError> {
    run_command(
        app,
        "retry_capture",
        serde_json::json!({ "captureId": capture_id }),
    )
    .await
    .map(|_| ())
}

#[tauri::command]
async fn finalize_capture<R: Runtime>(
    app: tauri::AppHandle<R>,
    capture_id: String,
) -> Result<(), CaptureError> {
    run_command(
        app,
        "finalize_capture",
        serde_json::json!({ "captureId": capture_id }),
    )
    .await
    .map(|_| ())
}

#[tauri::command]
async fn update_transcript<R: Runtime>(
    app: tauri::AppHandle<R>,
    request: tauri::ipc::Request<'_>,
) -> Result<serde_json::Value, CaptureError> {
    dispatch_transcript_update(request.body(), |arguments| {
        run_command(app, "update_transcript", arguments)
    })
    .await
}

async fn dispatch_transcript_update<F, Fut>(
    body: &tauri::ipc::InvokeBody,
    native: F,
) -> Result<serde_json::Value, CaptureError>
where
    F: FnOnce(serde_json::Value) -> Fut,
    Fut: std::future::Future<Output = Result<serde_json::Value, CaptureError>>,
{
    let arguments = transcript_arguments(body)?;
    native(arguments).await
}

#[derive(serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct UpdateTranscriptArguments {
    capture_id: String,
    text: String,
    #[serde(
        default,
        deserialize_with = "supplied_transcript_version",
        skip_serializing_if = "Option::is_none"
    )]
    expected_version: Option<u64>,
}

fn supplied_transcript_version<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<u64>, D::Error> {
    let version = <u64 as serde::Deserialize>::deserialize(deserializer)?;
    if version > i64::MAX as u64 {
        return Err(serde::de::Error::custom(
            "transcript version exceeds native range",
        ));
    }
    Ok(Some(version))
}

/// Parse the actual invoke body so omission remains distinct from an explicitly invalid null.
fn transcript_arguments(body: &tauri::ipc::InvokeBody) -> Result<serde_json::Value, CaptureError> {
    let invalid = || CaptureError {
        message: "transcript-update-unavailable".to_owned(),
    };
    let tauri::ipc::InvokeBody::Json(value) = body else {
        return Err(invalid());
    };
    let arguments: UpdateTranscriptArguments =
        serde_json::from_value(value.clone()).map_err(|_| invalid())?;
    serde_json::to_value(arguments).map_err(|_| invalid())
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

#[cfg(test)]
mod transcript_bridge_tests {
    use super::*;
    use serde_json::json;

    const ID: &str = "123e4567-e89b-42d3-a456-426614174094";

    #[test]
    fn command_composition_returns_committed_record_without_discarding_version() {
        let body = tauri::ipc::InvokeBody::Json(json!({
            "captureId": ID, "text": "Synthetic committed correction", "expectedVersion": 4
        }));
        let committed = json!({
            "captureId": ID, "finalText": "Synthetic committed correction", "transcriptVersion": 5
        });
        let result = tauri::async_runtime::block_on(dispatch_transcript_update(&body, |arguments| {
            assert_eq!(arguments, json!({
                "captureId": ID, "text": "Synthetic committed correction", "expectedVersion": 4
            }));
            std::future::ready(Ok(committed.clone()))
        }))
        .unwrap();
        assert_eq!(serde_json::to_value(result).unwrap(), committed);
    }

    #[test]
    fn command_composition_does_not_call_native_for_invalid_arguments() {
        let invalid = [
            tauri::ipc::InvokeBody::Raw(vec![1, 2, 3]),
            tauri::ipc::InvokeBody::Json(
                json!({"captureId": ID, "text": "Synthetic correction", "expectedVersion": null}),
            ),
            tauri::ipc::InvokeBody::Json(
                json!({"captureId": ID, "text": "Synthetic correction", "expectedVersion": -1}),
            ),
            tauri::ipc::InvokeBody::Json(json!({"captureId": ID, "expectedVersion": 4})),
        ];
        for body in invalid {
            let called = std::cell::Cell::new(false);
            let error = tauri::async_runtime::block_on(dispatch_transcript_update(&body, |_| {
                called.set(true);
                std::future::ready(Ok(json!({})))
            }))
            .unwrap_err();
            assert!(!called.get());
            assert_eq!(error.message, "transcript-update-unavailable");
        }
    }

    #[test]
    fn command_composition_preserves_native_failure() {
        let body = tauri::ipc::InvokeBody::Json(json!({
            "captureId": ID, "text": "Synthetic rejected correction", "expectedVersion": 4
        }));
        let error = tauri::async_runtime::block_on(dispatch_transcript_update(&body, |_| {
            std::future::ready(Err(CaptureError {
                message: "transcript-conflict".to_owned(),
            }))
        }))
        .unwrap_err();
        assert_eq!(error.message, "transcript-conflict");
    }

    #[test]
    fn actual_invoke_body_forwards_selected_version_and_preserves_legacy_omission() {
        for version in [0, 3, i64::MAX as u64] {
            let input = json!({"captureId": ID, "text": "Synthetic correction", "expectedVersion": version});
            assert_eq!(
                transcript_arguments(&tauri::ipc::InvokeBody::Json(input.clone())).unwrap(),
                input
            );
        }
        let legacy = json!({"captureId": ID, "text": "Synthetic legacy correction"});
        assert_eq!(
            transcript_arguments(&tauri::ipc::InvokeBody::Json(legacy.clone())).unwrap(),
            legacy
        );
    }

    #[test]
    fn actual_invoke_body_rejects_invalid_version_types_and_range_before_native_dispatch() {
        for version in [
            json!(-1),
            json!(1.0),
            json!(1.25),
            json!("3"),
            json!(true),
            json!(null),
            json!([]),
            json!({}),
            json!(i64::MAX as u64 + 1),
        ] {
            let input = json!({"captureId": ID, "text": "Synthetic rejected correction", "expectedVersion": version});
            assert_eq!(
                transcript_arguments(&tauri::ipc::InvokeBody::Json(input))
                    .unwrap_err()
                    .message,
                "transcript-update-unavailable"
            );
        }
        assert!(transcript_arguments(&tauri::ipc::InvokeBody::Raw(vec![1, 2, 3])).is_err());
    }
}
