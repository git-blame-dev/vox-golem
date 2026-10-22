use serde_json::{json, Value};
use tauri::{AppHandle, Runtime};
use tauri_plugin_assistant::StructuredPluginInvokeError;

#[tauri::command]
async fn get_app_snapshot<R: Runtime>(
    app: AppHandle<R>,
) -> Result<Value, StructuredPluginInvokeError> {
    tauri_plugin_assistant::forward(app, "get_app_snapshot", json!({})).await
}

#[tauri::command]
async fn report_startup_stage<R: Runtime>(
    app: AppHandle<R>,
    stage: String,
) -> Result<Value, StructuredPluginInvokeError> {
    tauri_plugin_assistant::forward(app, "report_startup_stage", json!({ "stage": stage })).await
}

#[tauri::command]
async fn submit_prompt<R: Runtime>(
    app: AppHandle<R>,
    request_id: String,
    prompt: String,
    updates: tauri::ipc::Channel<Value>,
) -> Result<Value, StructuredPluginInvokeError> {
    tauri_plugin_assistant::submit(app, request_id, prompt, updates).await
}

#[tauri::command]
async fn cancel_prompt<R: Runtime>(
    app: AppHandle<R>,
    request_id: String,
) -> Result<Value, StructuredPluginInvokeError> {
    tauri_plugin_assistant::forward(app, "cancel_prompt", json!({ "requestId": request_id })).await
}

// Compatibility name: New retains existing conversations; it is never a delete/reset operation.
#[tauri::command]
async fn reset_session<R: Runtime>(
    app: AppHandle<R>,
    draft: Option<String>,
) -> Result<Value, StructuredPluginInvokeError> {
    tauri_plugin_assistant::forward(app, "new_conversation", json!({ "draft": draft })).await
}

#[tauri::command]
async fn set_preferences<R: Runtime>(
    app: AppHandle<R>,
    preferences: Value,
) -> Result<Value, StructuredPluginInvokeError> {
    tauri_plugin_assistant::forward(
        app,
        "set_preferences",
        json!({ "preferences": preferences }),
    )
    .await
}

#[tauri::command]
async fn select_conversation<R: Runtime>(
    app: AppHandle<R>,
    conversation_id: String,
) -> Result<Value, StructuredPluginInvokeError> {
    tauri_plugin_assistant::forward(
        app,
        "select_conversation",
        json!({ "conversationId": conversation_id }),
    )
    .await
}

#[tauri::command]
async fn select_conversation_head<R: Runtime>(
    app: AppHandle<R>,
    head_id: String,
) -> Result<Value, StructuredPluginInvokeError> {
    tauri_plugin_assistant::forward(
        app,
        "select_conversation_head",
        json!({ "headId": head_id }),
    )
    .await
}

#[tauri::command]
async fn save_assistant_draft<R: Runtime>(
    app: AppHandle<R>,
    text: String,
) -> Result<Value, StructuredPluginInvokeError> {
    tauri_plugin_assistant::forward(app, "save_assistant_draft", json!({ "text": text })).await
}

#[tauri::command]
async fn list_assistant_conversations<R: Runtime>(
    app: AppHandle<R>,
    offset: u32,
    expected_revision: String,
) -> Result<Value, StructuredPluginInvokeError> {
    tauri_plugin_assistant::forward(
        app,
        "list_assistant_conversations",
        json!({"offset":offset,"expectedRevision":expected_revision}),
    )
    .await
}

macro_rules! native_command {
    ($name:ident) => {
        #[tauri::command]
        async fn $name<R: Runtime>(
            app: AppHandle<R>,
        ) -> Result<Value, StructuredPluginInvokeError> {
            tauri_plugin_assistant::forward(app, stringify!($name), json!({})).await
        }
    };
}

#[tauri::command]
async fn list_assistant_heads<R: Runtime>(
    app: AppHandle<R>,
    offset: u32,
    expected_revision: String,
    conversation_id: String,
) -> Result<Value, StructuredPluginInvokeError> {
    tauri_plugin_assistant::forward(app, "list_assistant_heads", json!({"offset":offset,"expectedRevision":expected_revision,"conversationId":conversation_id})).await
}
native_command!(assistant_voice_status);
native_command!(open_speech_services_store);
native_command!(finish_assistant_dictation);
#[tauri::command]
async fn stop_assistant_voice<R: Runtime>(
    app: AppHandle<R>,
    request: tauri::ipc::Request<'_>,
) -> Result<Value, StructuredPluginInvokeError> {
    let arguments = match request.body() {
        tauri::ipc::InvokeBody::Json(body) => tauri_plugin_assistant::voice_stop_arguments(body)?,
        tauri::ipc::InvokeBody::Raw(_) => {
            return Err(StructuredPluginInvokeError {
                code: "invalid_input".into(),
                message: "Voice session ID is invalid".into(),
            })
        }
    };
    tauri_plugin_assistant::forward(app, "stop_assistant_voice", arguments).await
}
native_command!(assistant_pairing_status);
native_command!(open_chatgpt_sign_in);

#[tauri::command]
async fn chatgpt_auth<R: Runtime>(
    app: AppHandle<R>,
    action: String,
) -> Result<Value, StructuredPluginInvokeError> {
    tauri_plugin_assistant::forward(app, "chatgpt_auth", json!({"action": action})).await
}
native_command!(new_voice_shortcut_status);
native_command!(pin_new_voice_shortcut);
native_command!(consume_new_voice_shortcut);
native_command!(stop_send_new_voice_shortcut);

#[tauri::command]
async fn release_new_voice_shortcut<R: Runtime>(
    app: AppHandle<R>,
    token: String,
) -> Result<Value, StructuredPluginInvokeError> {
    tauri_plugin_assistant::forward(app, "release_new_voice_shortcut", json!({"token":token})).await
}
#[tauri::command]
async fn prepare_new_voice_shortcut<R: Runtime>(
    app: AppHandle<R>,
    session_id: String,
) -> Result<Value, StructuredPluginInvokeError> {
    tauri_plugin_assistant::forward(
        app,
        "prepare_new_voice_shortcut",
        json!({"sessionId":session_id}),
    )
    .await
}

#[tauri::command]
async fn start_assistant_voice<R: Runtime>(
    app: AppHandle<R>,
    session_id: String,
    loop_mode: bool,
    draft: String,
    updates: tauri::ipc::Channel<Value>,
    completion_policy: Option<String>,
    launch_token: Option<String>,
) -> Result<Value, StructuredPluginInvokeError> {
    tauri_plugin_assistant::start_input(
        app,
        session_id,
        loop_mode,
        draft,
        updates,
        completion_policy,
        launch_token,
    )
    .await
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_capture::init())
        .plugin(tauri_plugin_assistant::init())
        .invoke_handler(tauri::generate_handler![
            get_app_snapshot,
            report_startup_stage,
            submit_prompt,
            cancel_prompt,
            reset_session,
            set_preferences,
            select_conversation,
            select_conversation_head,
            save_assistant_draft,
            list_assistant_conversations,
            list_assistant_heads,
            assistant_voice_status,
            open_speech_services_store,
            start_assistant_voice,
            finish_assistant_dictation,
            stop_assistant_voice,
            assistant_pairing_status,
            chatgpt_auth,
            open_chatgpt_sign_in,
            new_voice_shortcut_status,
            pin_new_voice_shortcut,
            consume_new_voice_shortcut,
            stop_send_new_voice_shortcut,
            release_new_voice_shortcut,
            prepare_new_voice_shortcut,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Android shell");
}

#[cfg(test)]
mod tests {
    #[test]
    fn webview_commands_are_declared_and_granted() {
        let capability: serde_json::Value =
            serde_json::from_str(include_str!("../capabilities/main.json"))
                .expect("main capability should be valid JSON");
        let granted = capability["permissions"].as_array().expect("permissions");
        let build = include_str!("../build.rs");
        for (command, permission) in [
            ("chatgpt_auth", "allow-chatgpt-auth"),
            ("open_chatgpt_sign_in", "allow-open-chatgpt-sign-in"),
        ] {
            assert!(
                build.contains(&format!("\"{command}\"")),
                "{command} missing from the app manifest"
            );
            assert!(
                granted.iter().any(|value| value == permission),
                "{permission} missing from the main capability"
            );
        }
    }
}
