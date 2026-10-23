use std::path::{Path, PathBuf};
use voxgolem_core::config::CustomOpenAiConfig;

pub(super) fn path() -> Result<PathBuf, String> {
    #[cfg(feature = "app-benchmark")]
    if let Some(config) = super::app_benchmark::live_auth_configuration() {
        return Ok(config.auth_path.clone());
    }
    Ok(voxgolem_core::config::managed_chatgpt_auth_path(
        &super::application_config_path()?,
    ))
}

pub(super) fn configuration(fallback: Option<&CustomOpenAiConfig>) -> Option<CustomOpenAiConfig> {
    #[cfg(feature = "app-benchmark")]
    if let Some(config) = super::app_benchmark::live_auth_configuration() {
        return Some(config.clone());
    }
    configuration_at(path().ok().as_deref(), fallback)
}

fn configuration_at(
    path: Option<&Path>,
    fallback: Option<&CustomOpenAiConfig>,
) -> Option<CustomOpenAiConfig> {
    voxgolem_core::config::effective_custom_openai_config(path, fallback)
}

pub(super) fn apply_capabilities(capabilities: &mut [super::CapabilityPayload]) {
    let Ok(path) = path() else { return };
    if configuration(None).is_none() {
        return;
    }
    let ready = voxgolem_platform::custom_openai::observe_custom_openai_auth(&path)
        .status
        .is_ok();
    let other_provider = super::capability_available(capabilities, "opencode");
    for capability in capabilities {
        if capability.id == "custom_provider" || matches!(capability.id, "deep" | "review") {
            let available = ready || capability.id != "custom_provider" && other_provider;
            capability.state = if available {
                super::CapabilityStatePayload::Available
            } else {
                super::CapabilityStatePayload::Unavailable
            };
            capability.reason = if available {
                "ready"
            } else {
                "Sign in with ChatGPT to restore access"
            }
            .into();
        }
    }
}

pub(super) fn refresh(app: &super::AppState) {
    if let Ok(mut state) = app.startup_state.lock() {
        match &mut *state {
            super::StartupStatePayload::Ready { capabilities, .. }
            | super::StartupStatePayload::WarmingModel { capabilities, .. } => {
                apply_capabilities(capabilities)
            }
            super::StartupStatePayload::Error { .. } => {}
        }
    }
}

#[tauri::command]
pub(super) async fn chatgpt_auth(
    action: String,
    app_state: tauri::State<'_, super::AppState>,
) -> Result<serde_json::Value, String> {
    if !matches!(action.as_str(), "start" | "status" | "cancel" | "sign_out") {
        return Err("Unknown ChatGPT authentication action".into());
    }
    let _update_guard =
        super::begin_update_sensitive_operation(&app_state.update_installation_gate)?;
    super::ensure_app_running(&app_state.exit_cleanup_started)?;
    if matches!(action.as_str(), "start" | "sign_out") {
        super::invalidate_prefetch(&app_state)?;
    }
    if action == "sign_out" {
        let active = app_state
            .active_prompt
            .lock()
            .map_err(|_| "Prompt state is unavailable")?
            .as_ref()
            .map(|active| active.request_id.clone());
        if let Some(active) = active {
            super::cancel_prompt_if_active(&app_state, &active)?;
        }
    }
    let status = voxgolem_platform::chatgpt_auth::command(&path()?, &action)
        .await
        .map_err(|error| error.to_string())?;
    refresh(&app_state);
    serde_json::to_value(status).map_err(|_| "ChatGPT authentication status is unavailable".into())
}

#[tauri::command]
pub(super) async fn open_chatgpt_sign_in() -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(|| {
        #[cfg(target_os = "windows")]
        let mut command = {
            let mut command = std::process::Command::new("rundll32.exe");
            command.arg("url.dll,FileProtocolHandler");
            command
        };
        #[cfg(not(target_os = "windows"))]
        let mut command = std::process::Command::new("xdg-open");
        let mut child = command
            .arg("https://auth.openai.com/codex/device")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|_| {
                "Could not open the browser; visit auth.openai.com/codex/device".to_string()
            })?;
        // Reap the launcher without making sign-in depend on a browser process exiting.
        let _ = std::thread::spawn(move || {
            let _ = child.wait();
        });
        Ok(())
    })
    .await
    .map_err(|_| "Could not open the browser".to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;

    fn configured() -> CustomOpenAiConfig {
        CustomOpenAiConfig {
            endpoint: "https://synthetic.invalid/responses".into(),
            auth_path: "/synthetic/managed-auth.json".into(),
        }
    }

    #[test]
    fn canonical_selection_observes_replacement_and_signout_without_fallback() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("auth.json");
        std::fs::write(
            &path,
            r#"{"openai":{"type":"oauth","access":"synthetic-access","expires":4102444800000}}"#,
        )
        .unwrap();
        let selected = configuration_at(Some(&path), None).unwrap();
        assert_eq!(selected.auth_path, path);
        assert_eq!(
            selected.endpoint,
            "https://chatgpt.com/backend-api/codex/responses"
        );
        assert!(
            voxgolem_platform::custom_openai::observe_custom_openai_auth(&selected.auth_path)
                .status
                .is_ok()
        );
        let replacement = dir.path().join("replacement.json");
        std::fs::write(&replacement, "{}").unwrap();
        std::fs::rename(&replacement, &path).unwrap();
        assert!(
            voxgolem_platform::custom_openai::observe_custom_openai_auth(&selected.auth_path)
                .status
                .is_err()
        );
        std::fs::remove_file(&path).unwrap();
        assert_eq!(selected.auth_path, path);
        assert!(
            voxgolem_platform::custom_openai::observe_custom_openai_auth(&selected.auth_path)
                .status
                .is_err()
        );
    }

    #[test]
    fn device_sign_in_selects_its_own_credentials_and_fixed_codex_route() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("chatgpt-auth.json");
        std::fs::write(&path, "{}").unwrap();
        let config = configuration_at(Some(&path), Some(&configured())).unwrap();
        assert_eq!(config.auth_path, path);
        assert_eq!(
            config.endpoint,
            "https://chatgpt.com/backend-api/codex/responses"
        );
    }

    #[test]
    fn signed_out_record_keeps_the_managed_path() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("chatgpt-auth.json");
        std::fs::write(&path, "{}").unwrap();
        assert_eq!(configuration_at(Some(&path), None).unwrap().auth_path, path);
    }

    #[cfg(unix)]
    #[test]
    fn dangling_managed_selection_stays_on_the_managed_path() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("chatgpt-auth.json");
        std::os::unix::fs::symlink(dir.path().join("absent-target.json"), &path).unwrap();

        let selected = configuration_at(Some(&path), Some(&configured())).unwrap();
        assert_eq!(selected.auth_path, path);
    }

    #[test]
    fn untouched_installations_keep_existing_provider_configuration() {
        let config = configured();
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            configuration_at(Some(&dir.path().join("absent")), Some(&config)),
            Some(config)
        );
        assert!(configuration_at(None, None).is_none());
    }

    #[test]
    fn managed_access_refreshes_only_its_related_capabilities() {
        let dir = tempfile::tempdir().unwrap();
        let _root = super::super::test_storage::ScopedRoot::new(dir.path().into());
        let path = path().unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            r#"{"openai":{"type":"oauth","access":"synthetic-access","expires":4102444800000}}"#,
        )
        .unwrap();
        let mut capabilities = vec![
            super::super::CapabilityPayload {
                id: "custom_provider",
                state: super::super::CapabilityStatePayload::Unavailable,
                reason: "synthetic old failure".into(),
                actual_provider: None,
            },
            super::super::CapabilityPayload {
                id: "wake_word",
                state: super::super::CapabilityStatePayload::Unavailable,
                reason: "synthetic missing asset".into(),
                actual_provider: None,
            },
        ];
        apply_capabilities(&mut capabilities);
        assert_eq!(
            capabilities[0].state,
            super::super::CapabilityStatePayload::Available
        );
        assert_eq!(capabilities[1].reason, "synthetic missing asset");
        std::fs::write(path, "{}").unwrap();
        apply_capabilities(&mut capabilities);
        assert_eq!(
            capabilities[0].state,
            super::super::CapabilityStatePayload::Unavailable
        );
    }

    #[tokio::test]
    async fn native_storage_uses_a_private_subdirectory_without_changing_config_permissions() {
        let dir = tempfile::tempdir().unwrap();
        let _root = super::super::test_storage::ScopedRoot::new(dir.path().into());
        let config = super::super::application_config_path().unwrap();
        std::fs::create_dir_all(config.parent().unwrap()).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(
                config.parent().unwrap(),
                std::fs::Permissions::from_mode(0o755),
            )
            .unwrap();
        }
        let auth_path = path().unwrap();
        let status = voxgolem_platform::chatgpt_auth::command(&auth_path, "sign_out")
            .await
            .unwrap();
        assert_eq!(serde_json::to_value(status).unwrap()["phase"], "signed_out");
        assert!(auth_path.is_file());
        assert_eq!(configuration(None).unwrap().auth_path, auth_path);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(config.parent().unwrap())
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o755
            );
            assert_eq!(
                std::fs::metadata(auth_path.parent().unwrap())
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
        }
    }
}
