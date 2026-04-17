#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VoiceTurnConfig {
    silence_timeout_ms: u64,
}

impl VoiceTurnConfig {
    pub fn new(silence_timeout_ms: u64) -> Result<Self, VoiceTurnConfigError> {
        if silence_timeout_ms == 0 {
            return Err(VoiceTurnConfigError::InvalidSilenceTimeout);
        }

        Ok(Self { silence_timeout_ms })
    }

    pub fn silence_timeout_ms(&self) -> u64 {
        self.silence_timeout_ms
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VoiceTurnConfigError {
    InvalidSilenceTimeout,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VoiceTurnState {
    listening: bool,
    last_activity_ms: Option<u64>,
}

impl VoiceTurnState {
    pub fn new() -> Self {
        Self {
            listening: false,
            last_activity_ms: None,
        }
    }

    pub fn listening(&self) -> bool {
        self.listening
    }

    pub fn last_activity_ms(&self) -> Option<u64> {
        self.last_activity_ms
    }
}

impl Default for VoiceTurnState {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VoiceTurnEvent {
    WakeWordDetected { now_ms: u64 },
    SpeechDetected { now_ms: u64 },
    SilenceCheck { now_ms: u64 },
    Reset,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VoiceTurnAction {
    None,
    StartListening,
    StopListening,
}

pub fn apply_voice_turn_event(
    state: &VoiceTurnState,
    config: VoiceTurnConfig,
    event: VoiceTurnEvent,
) -> (VoiceTurnState, VoiceTurnAction) {
    match event {
        VoiceTurnEvent::WakeWordDetected { now_ms } if !state.listening => (
            VoiceTurnState {
                listening: true,
                last_activity_ms: Some(now_ms),
            },
            VoiceTurnAction::StartListening,
        ),
        VoiceTurnEvent::WakeWordDetected { now_ms } => (
            VoiceTurnState {
                listening: true,
                last_activity_ms: Some(now_ms),
            },
            VoiceTurnAction::None,
        ),
        VoiceTurnEvent::SpeechDetected { now_ms } if state.listening => (
            VoiceTurnState {
                listening: true,
                last_activity_ms: Some(now_ms),
            },
            VoiceTurnAction::None,
        ),
        VoiceTurnEvent::SilenceCheck { now_ms }
            if state.listening
                && state.last_activity_ms.is_some_and(|last_activity_ms| {
                    now_ms.saturating_sub(last_activity_ms) >= config.silence_timeout_ms
                }) =>
        {
            (
                VoiceTurnState {
                    listening: false,
                    last_activity_ms: None,
                },
                VoiceTurnAction::StopListening,
            )
        }
        VoiceTurnEvent::Reset => (VoiceTurnState::new(), VoiceTurnAction::None),
        _ => (*state, VoiceTurnAction::None),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        apply_voice_turn_event, VoiceTurnAction, VoiceTurnConfig, VoiceTurnConfigError,
        VoiceTurnEvent, VoiceTurnState,
    };

    #[test]
    fn rejects_zero_silence_timeout() {
        assert_eq!(
            VoiceTurnConfig::new(0),
            Err(VoiceTurnConfigError::InvalidSilenceTimeout)
        );
    }

    #[test]
    fn wake_word_starts_listening() {
        let state = VoiceTurnState::new();
        let config = VoiceTurnConfig::new(1_200).expect("valid silence timeout");

        let (next_state, action) = apply_voice_turn_event(
            &state,
            config,
            VoiceTurnEvent::WakeWordDetected { now_ms: 100 },
        );

        assert!(next_state.listening());
        assert_eq!(next_state.last_activity_ms(), Some(100));
        assert_eq!(action, VoiceTurnAction::StartListening);
    }

    #[test]
    fn speech_refreshes_activity_while_listening() {
        let config = VoiceTurnConfig::new(1_200).expect("valid silence timeout");
        let (listening_state, _) = apply_voice_turn_event(
            &VoiceTurnState::new(),
            config,
            VoiceTurnEvent::WakeWordDetected { now_ms: 100 },
        );

        let (next_state, action) = apply_voice_turn_event(
            &listening_state,
            config,
            VoiceTurnEvent::SpeechDetected { now_ms: 450 },
        );

        assert!(next_state.listening());
        assert_eq!(next_state.last_activity_ms(), Some(450));
        assert_eq!(action, VoiceTurnAction::None);
    }

    #[test]
    fn silence_before_timeout_keeps_listening() {
        let config = VoiceTurnConfig::new(1_200).expect("valid silence timeout");
        let (listening_state, _) = apply_voice_turn_event(
            &VoiceTurnState::new(),
            config,
            VoiceTurnEvent::WakeWordDetected { now_ms: 100 },
        );

        let (next_state, action) = apply_voice_turn_event(
            &listening_state,
            config,
            VoiceTurnEvent::SilenceCheck { now_ms: 1_000 },
        );

        assert!(next_state.listening());
        assert_eq!(next_state.last_activity_ms(), Some(100));
        assert_eq!(action, VoiceTurnAction::None);
    }

    #[test]
    fn silence_after_timeout_stops_listening() {
        let config = VoiceTurnConfig::new(1_200).expect("valid silence timeout");
        let (listening_state, _) = apply_voice_turn_event(
            &VoiceTurnState::new(),
            config,
            VoiceTurnEvent::WakeWordDetected { now_ms: 100 },
        );

        let (next_state, action) = apply_voice_turn_event(
            &listening_state,
            config,
            VoiceTurnEvent::SilenceCheck { now_ms: 1_300 },
        );

        assert!(!next_state.listening());
        assert_eq!(next_state.last_activity_ms(), None);
        assert_eq!(action, VoiceTurnAction::StopListening);
    }

    #[test]
    fn reset_returns_to_sleeping_state() {
        let config = VoiceTurnConfig::new(1_200).expect("valid silence timeout");
        let (listening_state, _) = apply_voice_turn_event(
            &VoiceTurnState::new(),
            config,
            VoiceTurnEvent::WakeWordDetected { now_ms: 100 },
        );

        let (next_state, action) =
            apply_voice_turn_event(&listening_state, config, VoiceTurnEvent::Reset);

        assert!(!next_state.listening());
        assert_eq!(next_state.last_activity_ms(), None);
        assert_eq!(action, VoiceTurnAction::None);
    }

    #[test]
    fn speech_while_sleeping_is_ignored() {
        let config = VoiceTurnConfig::new(1_200).expect("valid silence timeout");
        let state = VoiceTurnState::new();

        let (next_state, action) = apply_voice_turn_event(
            &state,
            config,
            VoiceTurnEvent::SpeechDetected { now_ms: 200 },
        );

        assert_eq!(next_state, state);
        assert_eq!(action, VoiceTurnAction::None);
    }
}
