#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VoiceTurnConfig {
    initial_silence_timeout_ms: u64,
    silence_timeout_ms: u64,
}

impl VoiceTurnConfig {
    pub fn new(silence_timeout_ms: u64) -> Result<Self, VoiceTurnConfigError> {
        Self::with_timeouts(2_500, silence_timeout_ms)
    }

    pub fn with_timeouts(
        initial_silence_timeout_ms: u64,
        silence_timeout_ms: u64,
    ) -> Result<Self, VoiceTurnConfigError> {
        if initial_silence_timeout_ms == 0 || silence_timeout_ms == 0 {
            return Err(VoiceTurnConfigError::InvalidSilenceTimeout);
        }

        Ok(Self {
            initial_silence_timeout_ms,
            silence_timeout_ms,
        })
    }

    pub fn initial_silence_timeout_ms(&self) -> u64 {
        self.initial_silence_timeout_ms
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
    heard_speech: bool,
    last_activity_ms: Option<u64>,
}

impl VoiceTurnState {
    pub fn new() -> Self {
        Self {
            listening: false,
            heard_speech: false,
            last_activity_ms: None,
        }
    }

    pub fn listening(&self) -> bool {
        self.listening
    }

    pub fn last_activity_ms(&self) -> Option<u64> {
        self.last_activity_ms
    }

    pub fn heard_speech(&self) -> bool {
        self.heard_speech
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
    StopWithoutSpeech,
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
                heard_speech: false,
                last_activity_ms: Some(now_ms),
            },
            VoiceTurnAction::StartListening,
        ),
        VoiceTurnEvent::WakeWordDetected { now_ms } => (
            VoiceTurnState {
                listening: true,
                heard_speech: false,
                last_activity_ms: Some(now_ms),
            },
            VoiceTurnAction::None,
        ),
        VoiceTurnEvent::SpeechDetected { now_ms } if state.listening => (
            VoiceTurnState {
                listening: true,
                heard_speech: true,
                last_activity_ms: Some(now_ms),
            },
            VoiceTurnAction::None,
        ),
        VoiceTurnEvent::SilenceCheck { now_ms }
            if state.listening
                && state.last_activity_ms.is_some_and(|last_activity_ms| {
                    now_ms.saturating_sub(last_activity_ms)
                        >= if state.heard_speech {
                            config.silence_timeout_ms
                        } else {
                            config.initial_silence_timeout_ms
                        }
                }) =>
        {
            let action = if state.heard_speech {
                VoiceTurnAction::StopListening
            } else {
                VoiceTurnAction::StopWithoutSpeech
            };
            (
                VoiceTurnState {
                    listening: false,
                    heard_speech: false,
                    last_activity_ms: None,
                },
                action,
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
    fn legacy_constructor_keeps_the_post_speech_argument_and_new_initial_default() {
        let config = VoiceTurnConfig::new(750).expect("post-speech timeout is valid");

        assert_eq!(config.initial_silence_timeout_ms(), 2_500);
        assert_eq!(config.silence_timeout_ms(), 750);
        assert!(VoiceTurnConfig::with_timeouts(0, 750).is_err());
        assert!(VoiceTurnConfig::with_timeouts(2_500, 0).is_err());
    }

    #[test]
    fn separates_initial_and_post_speech_timeouts_at_boundaries() {
        let config = VoiceTurnConfig::with_timeouts(2_500, 750).expect("timeouts are valid");
        let (listening, _) = apply_voice_turn_event(
            &VoiceTurnState::new(),
            config,
            VoiceTurnEvent::WakeWordDetected { now_ms: 0 },
        );

        let (before_initial_expiry, before_action) = apply_voice_turn_event(
            &listening,
            config,
            VoiceTurnEvent::SilenceCheck { now_ms: 2_499 },
        );
        assert!(before_initial_expiry.listening());
        assert_eq!(before_action, VoiceTurnAction::None);

        let (expired, expiry_action) = apply_voice_turn_event(
            &listening,
            config,
            VoiceTurnEvent::SilenceCheck { now_ms: 2_500 },
        );
        assert!(!expired.listening());
        assert_eq!(expiry_action, VoiceTurnAction::StopWithoutSpeech);

        let (heard, _) = apply_voice_turn_event(
            &listening,
            config,
            VoiceTurnEvent::SpeechDetected { now_ms: 100 },
        );
        assert!(heard.heard_speech());
        let (before_post_speech_expiry, before_action) =
            apply_voice_turn_event(&heard, config, VoiceTurnEvent::SilenceCheck { now_ms: 849 });
        assert!(before_post_speech_expiry.listening());
        assert_eq!(before_action, VoiceTurnAction::None);

        let (post_speech_expired, post_action) =
            apply_voice_turn_event(&heard, config, VoiceTurnEvent::SilenceCheck { now_ms: 850 });
        assert!(!post_speech_expired.listening());
        assert_eq!(post_action, VoiceTurnAction::StopListening);
    }

    #[test]
    fn later_speech_refreshes_post_speech_silence_deadline() {
        let config = VoiceTurnConfig::with_timeouts(2_500, 750).expect("timeouts are valid");
        let (listening, _) = apply_voice_turn_event(
            &VoiceTurnState::new(),
            config,
            VoiceTurnEvent::WakeWordDetected { now_ms: 0 },
        );
        let (first_speech, _) = apply_voice_turn_event(
            &listening,
            config,
            VoiceTurnEvent::SpeechDetected { now_ms: 100 },
        );
        let (refreshed, _) = apply_voice_turn_event(
            &first_speech,
            config,
            VoiceTurnEvent::SpeechDetected { now_ms: 700 },
        );

        let (still_listening, action) = apply_voice_turn_event(
            &refreshed,
            config,
            VoiceTurnEvent::SilenceCheck { now_ms: 1_449 },
        );
        assert!(still_listening.listening());
        assert_eq!(action, VoiceTurnAction::None);
        let (_, action) = apply_voice_turn_event(
            &refreshed,
            config,
            VoiceTurnEvent::SilenceCheck { now_ms: 1_450 },
        );
        assert_eq!(action, VoiceTurnAction::StopListening);
    }

    #[test]
    fn wake_and_reset_clear_heard_speech() {
        let config = VoiceTurnConfig::new(750).expect("timeout is valid");
        let (listening, _) = apply_voice_turn_event(
            &VoiceTurnState::new(),
            config,
            VoiceTurnEvent::WakeWordDetected { now_ms: 0 },
        );
        let (heard, _) = apply_voice_turn_event(
            &listening,
            config,
            VoiceTurnEvent::SpeechDetected { now_ms: 100 },
        );
        let (reset, _) = apply_voice_turn_event(&heard, config, VoiceTurnEvent::Reset);
        assert!(!reset.heard_speech());

        let (rewoken, _) = apply_voice_turn_event(
            &heard,
            config,
            VoiceTurnEvent::WakeWordDetected { now_ms: 200 },
        );
        assert!(!rewoken.heard_speech());
        assert_eq!(rewoken.last_activity_ms(), Some(200));
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
        let (speaking_state, _) = apply_voice_turn_event(
            &listening_state,
            config,
            VoiceTurnEvent::SpeechDetected { now_ms: 100 },
        );

        let (next_state, action) = apply_voice_turn_event(
            &speaking_state,
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
        let (speaking_state, _) = apply_voice_turn_event(
            &listening_state,
            config,
            VoiceTurnEvent::SpeechDetected { now_ms: 100 },
        );

        let (next_state, action) = apply_voice_turn_event(
            &speaking_state,
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
