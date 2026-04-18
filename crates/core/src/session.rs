use crate::runtime::{
    apply_runtime_event, reset_runtime_to_idle, RuntimeEvent, RuntimeState, RuntimeTransitionError,
};
use crate::voice_turn::{
    apply_voice_turn_event, VoiceTurnAction, VoiceTurnConfig, VoiceTurnEvent, VoiceTurnState,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionConfig {
    voice_turn: VoiceTurnConfig,
}

impl SessionConfig {
    pub fn new(voice_turn: VoiceTurnConfig) -> Self {
        Self { voice_turn }
    }

    pub fn voice_turn(&self) -> VoiceTurnConfig {
        self.voice_turn
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionState {
    runtime: RuntimeState,
    voice_turn: VoiceTurnState,
}

impl SessionState {
    pub fn new() -> Self {
        Self {
            runtime: RuntimeState::new(),
            voice_turn: VoiceTurnState::new(),
        }
    }

    pub fn runtime(&self) -> &RuntimeState {
        &self.runtime
    }

    pub fn voice_turn(&self) -> VoiceTurnState {
        self.voice_turn
    }
}

impl Default for SessionState {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionEvent {
    StartupValidated,
    StartupFailed { message: String },
    WakeWordDetected { now_ms: u64 },
    SpeechDetected { now_ms: u64 },
    SilenceCheck { now_ms: u64 },
    SubmitPrompt,
    PromptCompleted,
    PromptFailed { message: String },
    ResetToIdle,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionTransitionError {
    Runtime(RuntimeTransitionError),
}

pub fn apply_session_event(
    state: &SessionState,
    config: SessionConfig,
    event: SessionEvent,
) -> Result<SessionState, SessionTransitionError> {
    match event {
        SessionEvent::StartupValidated => Ok(SessionState {
            runtime: apply_runtime_event(state.runtime(), RuntimeEvent::StartupValidated)
                .map_err(SessionTransitionError::Runtime)?,
            voice_turn: state.voice_turn,
        }),
        SessionEvent::StartupFailed { message } => Ok(SessionState {
            runtime: apply_runtime_event(state.runtime(), RuntimeEvent::StartupFailed { message })
                .map_err(SessionTransitionError::Runtime)?,
            voice_turn: state.voice_turn,
        }),
        SessionEvent::WakeWordDetected { now_ms } => {
            let (voice_turn, action) = apply_voice_turn_event(
                &state.voice_turn,
                config.voice_turn(),
                VoiceTurnEvent::WakeWordDetected { now_ms },
            );

            let runtime = match action {
                VoiceTurnAction::StartListening => {
                    apply_runtime_event(state.runtime(), RuntimeEvent::BeginListening)
                        .map_err(SessionTransitionError::Runtime)?
                }
                _ => state.runtime.clone(),
            };

            Ok(SessionState {
                runtime,
                voice_turn,
            })
        }
        SessionEvent::SpeechDetected { now_ms } => {
            let (voice_turn, _) = apply_voice_turn_event(
                &state.voice_turn,
                config.voice_turn(),
                VoiceTurnEvent::SpeechDetected { now_ms },
            );

            Ok(SessionState {
                runtime: state.runtime.clone(),
                voice_turn,
            })
        }
        SessionEvent::SilenceCheck { now_ms } => {
            let (voice_turn, action) = apply_voice_turn_event(
                &state.voice_turn,
                config.voice_turn(),
                VoiceTurnEvent::SilenceCheck { now_ms },
            );

            let runtime = match action {
                VoiceTurnAction::StopListening => {
                    apply_runtime_event(state.runtime(), RuntimeEvent::EndListening)
                        .map_err(SessionTransitionError::Runtime)?
                }
                _ => state.runtime.clone(),
            };

            Ok(SessionState {
                runtime,
                voice_turn,
            })
        }
        SessionEvent::SubmitPrompt => Ok(SessionState {
            runtime: apply_runtime_event(state.runtime(), RuntimeEvent::SubmitPrompt)
                .map_err(SessionTransitionError::Runtime)?,
            voice_turn: VoiceTurnState::new(),
        }),
        SessionEvent::PromptCompleted => Ok(SessionState {
            runtime: apply_runtime_event(state.runtime(), RuntimeEvent::ResponseReady)
                .map_err(SessionTransitionError::Runtime)?,
            voice_turn: VoiceTurnState::new(),
        }),
        SessionEvent::PromptFailed { message } => Ok(SessionState {
            runtime: apply_runtime_event(state.runtime(), RuntimeEvent::Fail { message })
                .map_err(SessionTransitionError::Runtime)?,
            voice_turn: VoiceTurnState::new(),
        }),
        SessionEvent::ResetToIdle => Ok(SessionState {
            runtime: reset_runtime_to_idle(state.runtime())
                .map_err(SessionTransitionError::Runtime)?,
            voice_turn: VoiceTurnState::new(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use crate::runtime::{RuntimeEventKind, RuntimePhase, RuntimeTransitionError};
    use crate::voice_turn::VoiceTurnConfig;

    use super::{
        apply_session_event, SessionConfig, SessionEvent, SessionState, SessionTransitionError,
    };

    fn session_config() -> SessionConfig {
        SessionConfig::new(VoiceTurnConfig::new(1_200).expect("valid silence timeout"))
    }

    #[test]
    fn startup_validation_enters_sleeping_without_turn_state() {
        let state = SessionState::new();

        let next = apply_session_event(&state, session_config(), SessionEvent::StartupValidated)
            .expect("startup validation should succeed");

        assert_eq!(next.runtime().phase(), RuntimePhase::Sleeping);
        assert!(!next.voice_turn().listening());
    }

    #[test]
    fn wake_word_starts_listening_once_startup_is_ready() {
        let ready = apply_session_event(
            &SessionState::new(),
            session_config(),
            SessionEvent::StartupValidated,
        )
        .expect("startup validation should succeed");

        let next = apply_session_event(
            &ready,
            session_config(),
            SessionEvent::WakeWordDetected { now_ms: 100 },
        )
        .expect("wake word should start listening from sleeping");

        assert_eq!(next.runtime().phase(), RuntimePhase::Listening);
        assert!(next.voice_turn().listening());
        assert_eq!(next.voice_turn().last_activity_ms(), Some(100));
    }

    #[test]
    fn silence_timeout_moves_listening_to_processing() {
        let ready = apply_session_event(
            &SessionState::new(),
            session_config(),
            SessionEvent::StartupValidated,
        )
        .expect("startup validation should succeed");
        let listening = apply_session_event(
            &ready,
            session_config(),
            SessionEvent::WakeWordDetected { now_ms: 100 },
        )
        .expect("wake word should start listening");

        let next = apply_session_event(
            &listening,
            session_config(),
            SessionEvent::SilenceCheck { now_ms: 1_300 },
        )
        .expect("silence timeout should stop listening");

        assert_eq!(next.runtime().phase(), RuntimePhase::Processing);
        assert!(!next.voice_turn().listening());
        assert_eq!(next.voice_turn().last_activity_ms(), None);
    }

    #[test]
    fn speech_detected_while_sleeping_is_ignored() {
        let ready = apply_session_event(
            &SessionState::new(),
            session_config(),
            SessionEvent::StartupValidated,
        )
        .expect("startup validation should succeed");

        let next = apply_session_event(
            &ready,
            session_config(),
            SessionEvent::SpeechDetected { now_ms: 200 },
        )
        .expect("speech while sleeping should be ignored");

        assert_eq!(next.runtime().phase(), RuntimePhase::Sleeping);
        assert!(!next.voice_turn().listening());
    }

    #[test]
    fn speech_detected_while_listening_refreshes_activity_without_changing_runtime() {
        let ready = apply_session_event(
            &SessionState::new(),
            session_config(),
            SessionEvent::StartupValidated,
        )
        .expect("startup validation should succeed");
        let listening = apply_session_event(
            &ready,
            session_config(),
            SessionEvent::WakeWordDetected { now_ms: 100 },
        )
        .expect("wake word should start listening");

        let next = apply_session_event(
            &listening,
            session_config(),
            SessionEvent::SpeechDetected { now_ms: 450 },
        )
        .expect("speech while listening should refresh activity");

        assert_eq!(next.runtime().phase(), RuntimePhase::Listening);
        assert!(next.voice_turn().listening());
        assert_eq!(next.voice_turn().last_activity_ms(), Some(450));
    }

    #[test]
    fn typed_prompt_flow_resets_voice_turn_and_enters_executing() {
        let ready = apply_session_event(
            &SessionState::new(),
            session_config(),
            SessionEvent::StartupValidated,
        )
        .expect("startup validation should succeed");

        let next = apply_session_event(&ready, session_config(), SessionEvent::SubmitPrompt)
            .expect("typed prompt should enter executing");

        assert_eq!(next.runtime().phase(), RuntimePhase::Executing);
        assert!(!next.voice_turn().listening());
    }

    #[test]
    fn prompt_completion_from_processing_enters_result_ready() {
        let ready = apply_session_event(
            &SessionState::new(),
            session_config(),
            SessionEvent::StartupValidated,
        )
        .expect("startup validation should succeed");
        let listening = apply_session_event(
            &ready,
            session_config(),
            SessionEvent::WakeWordDetected { now_ms: 100 },
        )
        .expect("wake word should start listening");
        let processing = apply_session_event(
            &listening,
            session_config(),
            SessionEvent::SilenceCheck { now_ms: 1_300 },
        )
        .expect("silence timeout should stop listening");

        let next =
            apply_session_event(&processing, session_config(), SessionEvent::PromptCompleted)
                .expect("processing should transition to result ready");

        assert_eq!(next.runtime().phase(), RuntimePhase::ResultReady);
        assert!(!next.voice_turn().listening());
    }

    #[test]
    fn submit_prompt_from_processing_enters_executing() {
        let ready = apply_session_event(
            &SessionState::new(),
            session_config(),
            SessionEvent::StartupValidated,
        )
        .expect("startup validation should succeed");
        let listening = apply_session_event(
            &ready,
            session_config(),
            SessionEvent::WakeWordDetected { now_ms: 100 },
        )
        .expect("wake word should start listening");
        let processing = apply_session_event(
            &listening,
            session_config(),
            SessionEvent::SilenceCheck { now_ms: 1_300 },
        )
        .expect("silence timeout should stop listening");

        let next = apply_session_event(&processing, session_config(), SessionEvent::SubmitPrompt)
            .expect("processing should transition to executing");

        assert_eq!(next.runtime().phase(), RuntimePhase::Executing);
        assert!(!next.voice_turn().listening());
    }

    #[test]
    fn prompt_failure_enters_error_and_clears_voice_turn() {
        let ready = apply_session_event(
            &SessionState::new(),
            session_config(),
            SessionEvent::StartupValidated,
        )
        .expect("startup validation should succeed");
        let listening = apply_session_event(
            &ready,
            session_config(),
            SessionEvent::WakeWordDetected { now_ms: 100 },
        )
        .expect("wake word should start listening");

        let next = apply_session_event(
            &listening,
            session_config(),
            SessionEvent::PromptFailed {
                message: "transcription failed".to_string(),
            },
        )
        .expect("prompt failure should enter error");

        assert_eq!(next.runtime().phase(), RuntimePhase::Error);
        assert_eq!(next.runtime().last_error(), Some("transcription failed"));
        assert!(!next.voice_turn().listening());
    }

    #[test]
    fn reset_recovers_error_back_to_sleeping() {
        let failed = apply_session_event(
            &SessionState::new(),
            session_config(),
            SessionEvent::StartupFailed {
                message: "missing config".to_string(),
            },
        )
        .expect("startup failure should enter error");

        let next = apply_session_event(&failed, session_config(), SessionEvent::ResetToIdle)
            .expect("reset should recover from error");

        assert_eq!(next.runtime().phase(), RuntimePhase::Sleeping);
        assert!(!next.voice_turn().listening());
    }

    #[test]
    fn reset_during_listening_returns_to_sleeping_and_clears_turn_state() {
        let ready = apply_session_event(
            &SessionState::new(),
            session_config(),
            SessionEvent::StartupValidated,
        )
        .expect("startup validation should succeed");
        let listening = apply_session_event(
            &ready,
            session_config(),
            SessionEvent::WakeWordDetected { now_ms: 100 },
        )
        .expect("wake word should start listening");

        let next = apply_session_event(&listening, session_config(), SessionEvent::ResetToIdle)
            .expect("reset should clear active listening state");

        assert_eq!(next.runtime().phase(), RuntimePhase::Sleeping);
        assert!(!next.voice_turn().listening());
        assert_eq!(next.voice_turn().last_activity_ms(), None);
    }

    #[test]
    fn wake_word_before_startup_ready_is_rejected() {
        let error = apply_session_event(
            &SessionState::new(),
            session_config(),
            SessionEvent::WakeWordDetected { now_ms: 100 },
        )
        .expect_err("wake word should be invalid before startup validation");

        assert_eq!(
            error,
            SessionTransitionError::Runtime(RuntimeTransitionError {
                phase: RuntimePhase::Initializing,
                event: RuntimeEventKind::BeginListening,
            })
        );
    }
}
