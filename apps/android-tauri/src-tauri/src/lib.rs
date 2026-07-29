use std::sync::Mutex;

use tauri::State;
use voxgolem_core::{assistant, conversation::ConversationSession, ipc};

const UNAVAILABLE_MESSAGE: &str = "Android shell has no configured response provider";
const INVALID_REQUEST_ID: &str = "Request ID is empty or too long";
const INVALID_PROMPT: &str = "Prompt is empty or too long";

struct CanonicalState {
    session: ConversationSession,
    preferences: ipc::ValidatedPreferences,
}

struct AppState(Mutex<CanonicalState>);

impl AppState {
    fn new() -> Self {
        let preferences = default_preferences();
        Self(Mutex::new(CanonicalState {
            session: ConversationSession::new(policy(&preferences)),
            preferences,
        }))
    }
}

fn error(code: ipc::CommandErrorCode, message: &'static str) -> ipc::CommandError {
    ipc::CommandError::new(code, message).expect("constant command error is valid")
}

fn unavailable() -> ipc::CommandError {
    error(ipc::CommandErrorCode::Unavailable, UNAVAILABLE_MESSAGE)
}

fn invalid(message: &'static str) -> ipc::CommandError {
    error(ipc::CommandErrorCode::InvalidInput, message)
}

fn lock(state: &AppState) -> Result<std::sync::MutexGuard<'_, CanonicalState>, ipc::CommandError> {
    state.0.lock().map_err(|_| {
        error(
            ipc::CommandErrorCode::Internal,
            "Android shell state is unavailable",
        )
    })
}

fn default_preferences() -> ipc::ValidatedPreferences {
    ipc::ValidatedPreferences {
        instant: ipc::InstantModelChoice::LocalFast,
        deep: ipc::AgentModelChoice::OpenCodeSolHigh,
        review: ipc::AgentModelChoice::OpenCodeSolHigh,
        deep_enabled: false,
        review_enabled: false,
        prefetch: false,
        completion: false,
        response_profile: ipc::ResponseProfile::Fast,
        tts_enabled: false,
        ui_theme: ipc::UiTheme::Dark,
        ui_text_size: ipc::UiTextSize::Medium,
    }
}

fn policy(p: &ipc::ValidatedPreferences) -> assistant::AssistantPolicy {
    assistant::AssistantPolicy {
        instant_model: match p.instant {
            ipc::InstantModelChoice::LocalFast => assistant::InstantModel::LocalFast,
            ipc::InstantModelChoice::LocalQuality => assistant::InstantModel::LocalQuality,
            ipc::InstantModelChoice::CustomSolHigh => assistant::InstantModel::CustomSolHigh,
            ipc::InstantModelChoice::CustomLunaLow => assistant::InstantModel::CustomLunaLow,
            ipc::InstantModelChoice::OpenCodeSolHigh => assistant::InstantModel::OpenCodeSolHigh,
            ipc::InstantModelChoice::OpenCodeLunaLow => assistant::InstantModel::OpenCodeLunaLow,
        },
        deep_model: agent_model(p.deep),
        review_model: agent_model(p.review),
        deep_enabled: p.deep_enabled,
        review_enabled: p.review_enabled,
        prefetch_enabled: p.prefetch,
        completion_enabled: p.completion,
    }
}

fn agent_model(model: ipc::AgentModelChoice) -> assistant::AgentModel {
    match model {
        ipc::AgentModelChoice::CustomSolHigh => assistant::AgentModel::CustomSolHigh,
        ipc::AgentModelChoice::CustomLunaLow => assistant::AgentModel::CustomLunaLow,
        ipc::AgentModelChoice::OpenCodeSolHigh => assistant::AgentModel::OpenCodeSolHigh,
        ipc::AgentModelChoice::OpenCodeLunaLow => assistant::AgentModel::OpenCodeLunaLow,
    }
}

fn validate_request_id(request_id: &str) -> Result<(), ipc::CommandError> {
    if request_id.is_empty() || request_id.len() > voxgolem_core::conversation::MAX_REQUEST_ID_BYTES
    {
        return Err(invalid(INVALID_REQUEST_ID));
    }
    Ok(())
}

fn validate_prompt(request_id: &str, prompt: &str) -> Result<(), ipc::CommandError> {
    validate_request_id(request_id)?;
    if prompt.trim().is_empty() || prompt.len() > voxgolem_core::conversation::MAX_PROMPT_BYTES {
        return Err(invalid(INVALID_PROMPT));
    }
    Ok(())
}

fn snapshot(state: &CanonicalState) -> ipc::AppSnapshot {
    let session = state.session.snapshot();
    ipc::AppSnapshot {
        session_id: session.session_id.0.into(),
        runtime_phase: ipc::RuntimePhase::Sleeping,
        transcript: session.turns.iter().map(ipc::Turn::from).collect(),
        active_request: session.active.as_ref().map(ipc::ActiveRequestSummary::from),
        capabilities: [
            (
                "custom_provider",
                "No custom response provider is configured",
            ),
            ("opencode", "No OpenCode agent is configured"),
            ("local_fast", "No fast local model is configured"),
            ("local_quality", "No quality local model is configured"),
            ("qwen_prediction", "No prediction model is configured"),
            ("wake_word", "Wake word detection is not configured"),
            ("vad", "Voice activity detection is not configured"),
            ("parakeet", "Speech transcription is not configured"),
            ("tts", "Text to speech is not configured"),
            ("deep", "Deep execution is not configured"),
            ("review", "Review execution is not configured"),
        ]
        .into_iter()
        .map(|(id, reason)| ipc::Capability {
            id: ipc::BoundedText::new(id).expect("static capability id fits"),
            state: ipc::CapabilityState::Unavailable,
            reason: ipc::BoundedText::new(reason).expect("static reason fits"),
            actual_provider: None,
        })
        .collect(),
        model_options: Vec::new(),
        preferences: state.preferences.clone(),
    }
}

#[tauri::command]
fn get_app_snapshot(state: State<'_, AppState>) -> Result<ipc::AppSnapshot, ipc::CommandError> {
    let state = lock(&state)?;
    Ok(snapshot(&state))
}

fn submit_prompt_inner(
    request_id: &str,
    prompt: &str,
) -> Result<ipc::PromptOutcome, ipc::CommandError> {
    validate_prompt(request_id, prompt)?;
    Err(unavailable())
}

#[tauri::command]
fn submit_prompt(
    request_id: String,
    prompt: String,
    updates: tauri::ipc::Channel<ipc::PromptUpdate>,
) -> Result<ipc::PromptOutcome, ipc::CommandError> {
    let _ = updates;
    submit_prompt_inner(&request_id, &prompt)
}

#[tauri::command]
fn cancel_prompt(
    request_id: String,
    state: State<'_, AppState>,
) -> Result<bool, ipc::CommandError> {
    validate_request_id(&request_id)?;
    let state = lock(&state)?;
    Ok(state
        .session
        .snapshot()
        .active
        .as_ref()
        .is_some_and(|active| active.request_id.0.as_str() == request_id))
}

#[tauri::command]
fn reset_session(state: State<'_, AppState>) -> Result<ipc::AppSnapshot, ipc::CommandError> {
    let mut state = lock(&state)?;
    state.session.reset();
    Ok(snapshot(&state))
}

fn set_preferences_inner(
    next: ipc::ValidatedPreferences,
    state: &AppState,
) -> Result<ipc::ValidatedPreferences, ipc::CommandError> {
    let mut state = lock(state)?;
    if next.instant != state.preferences.instant
        || next.deep != state.preferences.deep
        || next.review != state.preferences.review
        || next.response_profile != state.preferences.response_profile
        || (next.prefetch && !state.preferences.prefetch)
        || (next.completion && !state.preferences.completion)
        || (next.deep_enabled && !state.preferences.deep_enabled)
        || (next.review_enabled && !state.preferences.review_enabled)
        || (next.tts_enabled && !state.preferences.tts_enabled)
    {
        return Err(unavailable());
    }
    if state.session.has_active_execution() {
        return Err(error(
            ipc::CommandErrorCode::Busy,
            "A request is already active",
        ));
    }
    let new_policy = policy(&next);
    state
        .session
        .replace_policy(new_policy)
        .map_err(|_| error(ipc::CommandErrorCode::Busy, "A request is already active"))?;
    state.preferences = next.clone();
    Ok(next)
}

#[tauri::command]
fn set_preferences(
    preferences: ipc::ValidatedPreferences,
    state: State<'_, AppState>,
) -> Result<ipc::ValidatedPreferences, ipc::CommandError> {
    set_preferences_inner(preferences, &state)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .manage(AppState::new())
        .invoke_handler(tauri::generate_handler![
            get_app_snapshot,
            submit_prompt,
            cancel_prompt,
            reset_session,
            set_preferences
        ])
        .setup(|_| {
            eprintln!("VOXGOLEM_ANDROID_SHELL_STARTED");
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running Android shell");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_is_canonical_and_serializable() {
        let state = AppState::new();
        let value = snapshot(&state.0.lock().unwrap());
        assert_eq!(value.session_id.as_str(), "1");
        assert!(!value.preferences.completion);
        assert!(serde_json::to_value(value).is_ok());
    }

    #[test]
    fn main_capability_grants_only_canonical_commands() {
        let capability: serde_json::Value =
            serde_json::from_str(include_str!("../capabilities/main.json")).unwrap();
        assert_eq!(
            capability["permissions"],
            serde_json::json!([
                "allow-get-app-snapshot",
                "allow-submit-prompt",
                "allow-cancel-prompt",
                "allow-reset-session",
                "allow-set-preferences"
            ])
        );
        assert!(!capability.to_string().contains("event"));
        assert!(!capability.to_string().contains("legacy"));
    }

    #[test]
    fn snapshot_has_exact_zero_asset_capabilities() {
        let state = AppState::new();
        let capabilities = snapshot(&state.0.lock().unwrap()).capabilities;
        let ids: Vec<_> = capabilities
            .iter()
            .map(|capability| capability.id.as_str())
            .collect();
        assert_eq!(
            ids,
            vec![
                "custom_provider",
                "opencode",
                "local_fast",
                "local_quality",
                "qwen_prediction",
                "wake_word",
                "vad",
                "parakeet",
                "tts",
                "deep",
                "review"
            ]
        );
        assert_eq!(ids.len(), 11);
        assert!(capabilities.iter().all(|capability| {
            capability.state == ipc::CapabilityState::Unavailable
                && !capability.reason.as_str().is_empty()
                && capability.actual_provider.is_none()
        }));
    }

    #[test]
    fn invalid_prompt_is_rejected_without_mutation() {
        let state = AppState::new();
        let before = snapshot(&state.0.lock().unwrap());
        assert_eq!(
            submit_prompt_inner("", "hello").unwrap_err().code(),
            ipc::CommandErrorCode::InvalidInput
        );
        assert_eq!(snapshot(&state.0.lock().unwrap()), before);
    }

    #[test]
    fn valid_prompt_is_unavailable_without_admission() {
        let state = AppState::new();
        assert_eq!(
            submit_prompt_inner("request", "hello").unwrap_err().code(),
            ipc::CommandErrorCode::Unavailable
        );
        assert!(!state.0.lock().unwrap().session.has_active_execution());
    }

    #[test]
    fn reset_rotates_session_and_ui_preferences_are_atomic() {
        let state = AppState::new();
        let mut next = default_preferences();
        next.ui_theme = ipc::UiTheme::Light;
        assert_eq!(set_preferences_inner(next.clone(), &state).unwrap(), next);
        let mut locked = state.0.lock().unwrap();
        locked.session.reset();
        assert_eq!(snapshot(&locked).session_id.as_str(), "2");
        assert_eq!(locked.preferences.ui_theme, ipc::UiTheme::Light);
    }

    #[test]
    fn unavailable_preference_proposals_leave_all_state_unchanged() {
        for change in [
            |preferences: &mut ipc::ValidatedPreferences| {
                preferences.response_profile = ipc::ResponseProfile::Quality;
            },
            |preferences: &mut ipc::ValidatedPreferences| preferences.prefetch = true,
            |preferences: &mut ipc::ValidatedPreferences| preferences.deep_enabled = true,
            |preferences: &mut ipc::ValidatedPreferences| preferences.tts_enabled = true,
        ] as [fn(&mut ipc::ValidatedPreferences); 4]
        {
            let state = AppState::new();
            let before = snapshot(&state.0.lock().unwrap());
            let mut next = before.preferences.clone();
            change(&mut next);
            assert_eq!(
                set_preferences_inner(next, &state).unwrap_err().code(),
                ipc::CommandErrorCode::Unavailable
            );
            assert_eq!(snapshot(&state.0.lock().unwrap()), before);
        }

        let state = AppState::new();
        let mut disabled = default_preferences();
        disabled.completion = false;
        set_preferences_inner(disabled, &state).unwrap();
        let mut reenabled = default_preferences();
        reenabled.completion = true;
        assert_eq!(
            set_preferences_inner(reenabled, &state).unwrap_err().code(),
            ipc::CommandErrorCode::Unavailable
        );
    }
}
