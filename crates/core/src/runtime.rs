#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimePhase {
    Initializing,
    Sleeping,
    Listening,
    Processing,
    Executing,
    ResultReady,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeState {
    phase: RuntimePhase,
    last_error: Option<String>,
}

impl RuntimeState {
    pub fn new() -> Self {
        Self {
            phase: RuntimePhase::Initializing,
            last_error: None,
        }
    }

    pub fn phase(&self) -> RuntimePhase {
        self.phase
    }

    pub fn last_error(&self) -> Option<&str> {
        self.last_error.as_deref()
    }
}

impl Default for RuntimeState {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeEvent {
    StartupValidated,
    StartupFailed { message: String },
    BeginListening,
    EndListening,
    SubmitPrompt,
    ResponseReady,
    ResetToIdle,
    Fail { message: String },
    RecoverFromError,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeEventKind {
    StartupValidated,
    StartupFailed,
    BeginListening,
    EndListening,
    SubmitPrompt,
    ResponseReady,
    ResetToIdle,
    Fail,
    RecoverFromError,
}

impl RuntimeEvent {
    fn kind(&self) -> RuntimeEventKind {
        match self {
            Self::StartupValidated => RuntimeEventKind::StartupValidated,
            Self::StartupFailed { .. } => RuntimeEventKind::StartupFailed,
            Self::BeginListening => RuntimeEventKind::BeginListening,
            Self::EndListening => RuntimeEventKind::EndListening,
            Self::SubmitPrompt => RuntimeEventKind::SubmitPrompt,
            Self::ResponseReady => RuntimeEventKind::ResponseReady,
            Self::ResetToIdle => RuntimeEventKind::ResetToIdle,
            Self::Fail { .. } => RuntimeEventKind::Fail,
            Self::RecoverFromError => RuntimeEventKind::RecoverFromError,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeTransitionError {
    pub phase: RuntimePhase,
    pub event: RuntimeEventKind,
}

pub fn apply_runtime_event(
    state: &RuntimeState,
    event: RuntimeEvent,
) -> Result<RuntimeState, RuntimeTransitionError> {
    let current_phase = state.phase();
    let event_kind = event.kind();

    let next_state = match event {
        RuntimeEvent::StartupValidated if current_phase == RuntimePhase::Initializing => {
            RuntimeState {
                phase: RuntimePhase::Sleeping,
                last_error: None,
            }
        }
        RuntimeEvent::StartupFailed { message } if current_phase == RuntimePhase::Initializing => {
            RuntimeState {
                phase: RuntimePhase::Error,
                last_error: Some(message),
            }
        }
        RuntimeEvent::BeginListening
            if current_phase == RuntimePhase::Sleeping
                || current_phase == RuntimePhase::ResultReady =>
        {
            RuntimeState {
                phase: RuntimePhase::Listening,
                last_error: None,
            }
        }
        RuntimeEvent::EndListening if current_phase == RuntimePhase::Listening => RuntimeState {
            phase: RuntimePhase::Processing,
            last_error: None,
        },
        RuntimeEvent::SubmitPrompt
            if current_phase == RuntimePhase::Sleeping
                || current_phase == RuntimePhase::ResultReady =>
        {
            RuntimeState {
                phase: RuntimePhase::Executing,
                last_error: None,
            }
        }
        RuntimeEvent::ResponseReady
            if current_phase == RuntimePhase::Processing
                || current_phase == RuntimePhase::Executing =>
        {
            RuntimeState {
                phase: RuntimePhase::ResultReady,
                last_error: None,
            }
        }
        RuntimeEvent::ResetToIdle if current_phase == RuntimePhase::ResultReady => RuntimeState {
            phase: RuntimePhase::Sleeping,
            last_error: None,
        },
        RuntimeEvent::Fail { message } => RuntimeState {
            phase: RuntimePhase::Error,
            last_error: Some(message),
        },
        RuntimeEvent::RecoverFromError if current_phase == RuntimePhase::Error => RuntimeState {
            phase: RuntimePhase::Sleeping,
            last_error: None,
        },
        _ => {
            return Err(RuntimeTransitionError {
                phase: current_phase,
                event: event_kind,
            });
        }
    };

    Ok(next_state)
}

pub fn reset_runtime_to_idle(state: &RuntimeState) -> Result<RuntimeState, RuntimeTransitionError> {
    match state.phase() {
        RuntimePhase::Initializing => Err(RuntimeTransitionError {
            phase: RuntimePhase::Initializing,
            event: RuntimeEventKind::ResetToIdle,
        }),
        RuntimePhase::Error => apply_runtime_event(state, RuntimeEvent::RecoverFromError),
        RuntimePhase::Sleeping
        | RuntimePhase::Listening
        | RuntimePhase::Processing
        | RuntimePhase::Executing
        | RuntimePhase::ResultReady => Ok(RuntimeState {
            phase: RuntimePhase::Sleeping,
            last_error: None,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        apply_runtime_event, reset_runtime_to_idle, RuntimeEvent, RuntimeEventKind, RuntimePhase,
        RuntimeState, RuntimeTransitionError,
    };

    #[test]
    fn starts_in_initializing_phase() {
        let state = RuntimeState::new();

        assert_eq!(state.phase(), RuntimePhase::Initializing);
        assert_eq!(state.last_error(), None);
    }

    #[test]
    fn enters_sleeping_after_successful_startup_validation() {
        let state = RuntimeState::new();

        let next = apply_runtime_event(&state, RuntimeEvent::StartupValidated)
            .expect("startup validation should move initializing to sleeping");

        assert_eq!(next.phase(), RuntimePhase::Sleeping);
        assert_eq!(next.last_error(), None);
    }

    #[test]
    fn stores_error_when_startup_fails() {
        let state = RuntimeState::new();

        let next = apply_runtime_event(
            &state,
            RuntimeEvent::StartupFailed {
                message: "missing config".to_string(),
            },
        )
        .expect("startup failure should move initializing to error");

        assert_eq!(next.phase(), RuntimePhase::Error);
        assert_eq!(next.last_error(), Some("missing config"));
    }

    #[test]
    fn walks_voice_flow_from_sleeping_to_result_ready() {
        let state = RuntimeState {
            phase: RuntimePhase::Sleeping,
            last_error: None,
        };

        let listening = apply_runtime_event(&state, RuntimeEvent::BeginListening)
            .expect("sleeping should transition to listening");
        let processing = apply_runtime_event(&listening, RuntimeEvent::EndListening)
            .expect("listening should transition to processing");
        let result = apply_runtime_event(&processing, RuntimeEvent::ResponseReady)
            .expect("processing should transition to result_ready");

        assert_eq!(listening.phase(), RuntimePhase::Listening);
        assert_eq!(processing.phase(), RuntimePhase::Processing);
        assert_eq!(result.phase(), RuntimePhase::ResultReady);
    }

    #[test]
    fn walks_typed_prompt_flow_from_sleeping_to_result_ready() {
        let state = RuntimeState {
            phase: RuntimePhase::Sleeping,
            last_error: None,
        };

        let executing = apply_runtime_event(&state, RuntimeEvent::SubmitPrompt)
            .expect("sleeping should transition to executing");
        let result = apply_runtime_event(&executing, RuntimeEvent::ResponseReady)
            .expect("executing should transition to result_ready");

        assert_eq!(executing.phase(), RuntimePhase::Executing);
        assert_eq!(result.phase(), RuntimePhase::ResultReady);
    }

    #[test]
    fn fail_event_enters_error_from_any_runtime_phase() {
        let state = RuntimeState {
            phase: RuntimePhase::Listening,
            last_error: None,
        };

        let next = apply_runtime_event(
            &state,
            RuntimeEvent::Fail {
                message: "cue playback failed".to_string(),
            },
        )
        .expect("fail event should always enter error");

        assert_eq!(next.phase(), RuntimePhase::Error);
        assert_eq!(next.last_error(), Some("cue playback failed"));
    }

    #[test]
    fn recover_from_error_clears_last_error_and_returns_to_sleeping() {
        let state = RuntimeState {
            phase: RuntimePhase::Error,
            last_error: Some("device disconnected".to_string()),
        };

        let next = apply_runtime_event(&state, RuntimeEvent::RecoverFromError)
            .expect("recover_from_error should return to sleeping");

        assert_eq!(next.phase(), RuntimePhase::Sleeping);
        assert_eq!(next.last_error(), None);
    }

    #[test]
    fn rejects_invalid_transition_requests() {
        let state = RuntimeState {
            phase: RuntimePhase::Listening,
            last_error: None,
        };

        let error = apply_runtime_event(&state, RuntimeEvent::SubmitPrompt)
            .expect_err("submitting a prompt while listening should be invalid");

        assert_eq!(
            error,
            RuntimeTransitionError {
                phase: RuntimePhase::Listening,
                event: RuntimeEventKind::SubmitPrompt,
            }
        );
    }

    #[test]
    fn reset_runtime_to_idle_clears_active_execution_states() {
        let state = RuntimeState {
            phase: RuntimePhase::Executing,
            last_error: None,
        };

        let next =
            reset_runtime_to_idle(&state).expect("executing should be resettable back to sleeping");

        assert_eq!(next.phase(), RuntimePhase::Sleeping);
        assert_eq!(next.last_error(), None);
    }

    #[test]
    fn reset_runtime_to_idle_recovers_error() {
        let state = RuntimeState {
            phase: RuntimePhase::Error,
            last_error: Some("provider failed".to_string()),
        };

        let next =
            reset_runtime_to_idle(&state).expect("error should be resettable back to sleeping");

        assert_eq!(next.phase(), RuntimePhase::Sleeping);
        assert_eq!(next.last_error(), None);
    }
}
