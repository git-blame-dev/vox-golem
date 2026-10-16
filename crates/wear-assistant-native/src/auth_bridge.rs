use crate::{clear_exception, read_string, search, valid_auth_path};
use jni::objects::{JObject, JString};
use jni::sys::jstring;
use jni::JNIEnv;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::Path;

const AUTH_FAILURE: &str = r#"{"phase":"error","error":"ChatGPT authentication is unavailable"}"#;
const EXPORT_FAILURE: &str = r#"{"error":"ChatGPT access is unavailable"}"#;

fn allowed_path(path: &str) -> bool {
    valid_auth_path(path)
        && Path::new(path)
            .file_name()
            .is_some_and(|name| name == "chatgpt-auth.json")
}

fn command(path: &str, action: &str) -> String {
    if !allowed_path(path) || !matches!(action, "start" | "status" | "cancel" | "sign_out") {
        return AUTH_FAILURE.into();
    }
    let Ok(process) = search::process() else {
        return AUTH_FAILURE.into();
    };
    match process
        .runtime
        .block_on(voxgolem_platform::chatgpt_auth::command(
            Path::new(path),
            action,
        )) {
        Ok(status) => serde_json::to_string(&status).unwrap_or_else(|_| AUTH_FAILURE.into()),
        Err(error) => serde_json::json!({"phase":"error", "error":error.to_string()}).to_string(),
    }
}

fn export(path: &str) -> String {
    if !allowed_path(path) {
        return EXPORT_FAILURE.into();
    }
    let Ok(process) = search::process() else {
        return EXPORT_FAILURE.into();
    };
    match process
        .runtime
        .block_on(voxgolem_platform::chatgpt_auth::export_access(Path::new(
            path,
        ))) {
        Ok(Some(access)) => access,
        Ok(None) => "null".into(),
        Err(_) => EXPORT_FAILURE.into(),
    }
}

#[allow(unsafe_code)]
#[no_mangle]
pub extern "system" fn Java_dev_gitblame_voxgolem_assistant_runtime_AssistantProviderNative_chatGptAuth(
    mut env: JNIEnv<'_>,
    _receiver: JObject<'_>,
    path: JString<'_>,
    action: JString<'_>,
) -> jstring {
    let output = catch_unwind(AssertUnwindSafe(|| {
        match (
            read_string(&mut env, &path, 4096),
            read_string(&mut env, &action, 16),
        ) {
            (Ok(path), Ok(action)) => command(&path, &action),
            _ => AUTH_FAILURE.into(),
        }
    }))
    .unwrap_or_else(|_| AUTH_FAILURE.into());
    clear_exception(&env);
    env.new_string(output)
        .map(JString::into_raw)
        .unwrap_or(std::ptr::null_mut())
}

#[allow(unsafe_code)]
#[no_mangle]
pub extern "system" fn Java_dev_gitblame_voxgolem_assistant_runtime_AssistantProviderNative_exportChatGptAccess(
    mut env: JNIEnv<'_>,
    _receiver: JObject<'_>,
    path: JString<'_>,
) -> jstring {
    let output = catch_unwind(AssertUnwindSafe(|| {
        read_string(&mut env, &path, 4096)
            .map(|path| export(&path))
            .unwrap_or_else(|_| EXPORT_FAILURE.into())
    }))
    .unwrap_or_else(|_| EXPORT_FAILURE.into());
    clear_exception(&env);
    env.new_string(output)
        .map(JString::into_raw)
        .unwrap_or(std::ptr::null_mut())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bridge_rejects_imported_or_relative_paths_and_unknown_actions() {
        for path in [
            "relative/chatgpt-auth.json",
            "/synthetic/assistant-auth.json",
            "/synthetic/../chatgpt-auth.json",
        ] {
            assert_eq!(command(path, "start"), AUTH_FAILURE);
            assert_eq!(export(path), EXPORT_FAILURE);
        }
        assert_eq!(
            command("/synthetic/chatgpt-auth.json", "unknown"),
            AUTH_FAILURE
        );
    }

    #[test]
    fn bridge_status_and_export_have_distinct_noncredential_errors() {
        let auth: serde_json::Value = serde_json::from_str(AUTH_FAILURE).unwrap();
        let access: serde_json::Value = serde_json::from_str(EXPORT_FAILURE).unwrap();
        assert_eq!(auth["phase"], "error");
        assert!(access.get("error").is_some());
        assert_ne!(EXPORT_FAILURE, "null");
    }
}
