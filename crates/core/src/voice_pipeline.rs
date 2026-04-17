use crate::runtime::RuntimePhase;
use crate::session::{apply_session_event, SessionConfig, SessionEvent, SessionState};
use crate::turn_capture::{TurnCaptureConfig, TurnCaptureState};
use voxgolem_model::parakeet::{ParakeetTranscriptionInput, TranscriptionInputError};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VoicePipelineConfig {
    session: SessionConfig,
    capture: TurnCaptureConfig,
    sample_rate_hz: u32,
}

impl VoicePipelineConfig {
    pub fn new(session: SessionConfig, capture: TurnCaptureConfig, sample_rate_hz: u32) -> Self {
        Self {
            session,
            capture,
            sample_rate_hz,
        }
    }

    pub fn session(&self) -> SessionConfig {
        self.session
    }

    pub fn capture(&self) -> TurnCaptureConfig {
        self.capture
    }

    pub fn sample_rate_hz(&self) -> u32 {
        self.sample_rate_hz
    }
}

#[derive(Debug, Clone)]
pub struct VoicePipelineState {
    session: SessionState,
    capture: TurnCaptureState,
}

impl VoicePipelineState {
    pub fn new(
        config: VoicePipelineConfig,
    ) -> Result<Self, voxgolem_audio::buffers::AudioBufferError> {
        Ok(Self {
            session: SessionState::new(),
            capture: TurnCaptureState::new(config.capture())?,
        })
    }

    pub fn session(&self) -> &SessionState {
        &self.session
    }

    pub fn capture(&self) -> &TurnCaptureState {
        &self.capture
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum VoicePipelineEvent {
    StartupValidated,
    StartupFailed { message: String },
    RecordSleepingFrame { frame: Vec<f32> },
    WakeWordDetected { now_ms: u64 },
    RecordListeningFrame { frame: Vec<f32> },
    SilenceCheck { now_ms: u64 },
    SubmitPrompt,
    PromptCompleted,
    PromptFailed { message: String },
    ResetToIdle,
}

#[derive(Debug, Clone, PartialEq)]
pub enum VoicePipelineAction {
    None,
    StartedListening,
    FinishedUtterance {
        transcription_input: ParakeetTranscriptionInput,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub enum VoicePipelineError {
    Session(crate::session::SessionTransitionError),
    Capture(voxgolem_audio::buffers::AudioBufferError),
    TranscriptionInput(TranscriptionInputError),
}

pub fn apply_voice_pipeline_event(
    state: &VoicePipelineState,
    config: VoicePipelineConfig,
    event: VoicePipelineEvent,
) -> Result<(VoicePipelineState, VoicePipelineAction), VoicePipelineError> {
    match event {
        VoicePipelineEvent::StartupValidated => Ok((
            VoicePipelineState {
                session: apply_session_event(
                    state.session(),
                    config.session(),
                    SessionEvent::StartupValidated,
                )
                .map_err(VoicePipelineError::Session)?,
                capture: state.capture.clone(),
            },
            VoicePipelineAction::None,
        )),
        VoicePipelineEvent::StartupFailed { message } => Ok((
            VoicePipelineState {
                session: apply_session_event(
                    state.session(),
                    config.session(),
                    SessionEvent::StartupFailed { message },
                )
                .map_err(VoicePipelineError::Session)?,
                capture: state.capture.clone(),
            },
            VoicePipelineAction::None,
        )),
        VoicePipelineEvent::RecordSleepingFrame { frame } => {
            let mut capture = state.capture.clone();
            capture.record_sleeping_frame(&frame);

            Ok((
                VoicePipelineState {
                    session: state.session.clone(),
                    capture,
                },
                VoicePipelineAction::None,
            ))
        }
        VoicePipelineEvent::WakeWordDetected { now_ms } => {
            let session = apply_session_event(
                state.session(),
                config.session(),
                SessionEvent::WakeWordDetected { now_ms },
            )
            .map_err(VoicePipelineError::Session)?;
            let mut capture = state.capture.clone();
            capture
                .begin_utterance()
                .map_err(VoicePipelineError::Capture)?;

            Ok((
                VoicePipelineState { session, capture },
                VoicePipelineAction::StartedListening,
            ))
        }
        VoicePipelineEvent::RecordListeningFrame { frame } => {
            let mut capture = state.capture.clone();
            capture
                .record_listening_frame(&frame)
                .map_err(VoicePipelineError::Capture)?;

            Ok((
                VoicePipelineState {
                    session: state.session.clone(),
                    capture,
                },
                VoicePipelineAction::None,
            ))
        }
        VoicePipelineEvent::SilenceCheck { now_ms } => {
            let previous_phase = state.session().runtime().phase();
            let session = apply_session_event(
                state.session(),
                config.session(),
                SessionEvent::SilenceCheck { now_ms },
            )
            .map_err(VoicePipelineError::Session)?;
            let mut capture = state.capture.clone();

            let action = if previous_phase == RuntimePhase::Listening
                && session.runtime().phase() == RuntimePhase::Processing
            {
                let transcription_input = ParakeetTranscriptionInput::new(
                    config.sample_rate_hz(),
                    capture.finish_utterance(),
                )
                .map_err(VoicePipelineError::TranscriptionInput)?;

                VoicePipelineAction::FinishedUtterance {
                    transcription_input,
                }
            } else {
                VoicePipelineAction::None
            };

            Ok((VoicePipelineState { session, capture }, action))
        }
        VoicePipelineEvent::SubmitPrompt => {
            let session = apply_session_event(
                state.session(),
                config.session(),
                SessionEvent::SubmitPrompt,
            )
            .map_err(VoicePipelineError::Session)?;
            let mut capture = state.capture.clone();
            capture.reset();

            Ok((
                VoicePipelineState { session, capture },
                VoicePipelineAction::None,
            ))
        }
        VoicePipelineEvent::PromptCompleted => {
            let session = apply_session_event(
                state.session(),
                config.session(),
                SessionEvent::PromptCompleted,
            )
            .map_err(VoicePipelineError::Session)?;
            let mut capture = state.capture.clone();
            capture.reset();

            Ok((
                VoicePipelineState { session, capture },
                VoicePipelineAction::None,
            ))
        }
        VoicePipelineEvent::PromptFailed { message } => {
            let session = apply_session_event(
                state.session(),
                config.session(),
                SessionEvent::PromptFailed { message },
            )
            .map_err(VoicePipelineError::Session)?;
            let mut capture = state.capture.clone();
            capture.reset();

            Ok((
                VoicePipelineState { session, capture },
                VoicePipelineAction::None,
            ))
        }
        VoicePipelineEvent::ResetToIdle => {
            let session =
                apply_session_event(state.session(), config.session(), SessionEvent::ResetToIdle)
                    .map_err(VoicePipelineError::Session)?;
            let mut capture = state.capture.clone();
            capture.reset();

            Ok((
                VoicePipelineState { session, capture },
                VoicePipelineAction::None,
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::runtime::RuntimePhase;
    use crate::session::SessionConfig;
    use crate::turn_capture::TurnCaptureConfig;
    use crate::voice_turn::VoiceTurnConfig;
    use voxgolem_model::parakeet::{
        ParakeetTranscriptionInput, TranscriptionInputError, PARAKEET_SAMPLE_RATE_HZ,
    };

    use super::{
        apply_voice_pipeline_event, VoicePipelineAction, VoicePipelineConfig, VoicePipelineError,
        VoicePipelineEvent, VoicePipelineState,
    };

    fn pipeline_config() -> VoicePipelineConfig {
        VoicePipelineConfig::new(
            SessionConfig::new(VoiceTurnConfig::new(1_200).expect("valid silence timeout")),
            TurnCaptureConfig::new(4, 16).expect("valid turn capture config"),
            PARAKEET_SAMPLE_RATE_HZ,
        )
    }

    #[test]
    fn wake_word_starts_listening_and_seeds_preroll() {
        let config = pipeline_config();
        let (ready_state, _) = apply_voice_pipeline_event(
            &VoicePipelineState::new(config).expect("pipeline should initialize"),
            config,
            VoicePipelineEvent::StartupValidated,
        )
        .expect("startup validation should succeed");
        let (sleeping_state, _) = apply_voice_pipeline_event(
            &ready_state,
            config,
            VoicePipelineEvent::RecordSleepingFrame {
                frame: vec![0.1, 0.2, 0.3, 0.4],
            },
        )
        .expect("sleeping frame should be recorded");

        let (listening_state, action) = apply_voice_pipeline_event(
            &sleeping_state,
            config,
            VoicePipelineEvent::WakeWordDetected { now_ms: 100 },
        )
        .expect("wake word should start listening");

        assert_eq!(
            listening_state.session().runtime().phase(),
            RuntimePhase::Listening
        );
        assert!(listening_state.capture().capturing_utterance());
        assert_eq!(listening_state.capture().utterance_len(), 4);
        assert_eq!(action, VoicePipelineAction::StartedListening);
    }

    #[test]
    fn silence_finishes_utterance_and_returns_audio() {
        let config = pipeline_config();
        let (ready_state, _) = apply_voice_pipeline_event(
            &VoicePipelineState::new(config).expect("pipeline should initialize"),
            config,
            VoicePipelineEvent::StartupValidated,
        )
        .expect("startup validation should succeed");
        let (sleeping_state, _) = apply_voice_pipeline_event(
            &ready_state,
            config,
            VoicePipelineEvent::RecordSleepingFrame {
                frame: vec![0.1, 0.2, 0.3, 0.4],
            },
        )
        .expect("sleeping frame should be recorded");
        let (listening_state, _) = apply_voice_pipeline_event(
            &sleeping_state,
            config,
            VoicePipelineEvent::WakeWordDetected { now_ms: 100 },
        )
        .expect("wake word should start listening");
        let (recording_state, _) = apply_voice_pipeline_event(
            &listening_state,
            config,
            VoicePipelineEvent::RecordListeningFrame {
                frame: vec![0.5, 0.6],
            },
        )
        .expect("listening frame should be recorded");

        let (processing_state, action) = apply_voice_pipeline_event(
            &recording_state,
            config,
            VoicePipelineEvent::SilenceCheck { now_ms: 1_300 },
        )
        .expect("silence timeout should finish the utterance");

        assert_eq!(
            processing_state.session().runtime().phase(),
            RuntimePhase::Processing
        );
        assert!(!processing_state.capture().capturing_utterance());
        assert_eq!(processing_state.capture().utterance_len(), 0);
        assert_eq!(
            action,
            VoicePipelineAction::FinishedUtterance {
                transcription_input: ParakeetTranscriptionInput::new(
                    PARAKEET_SAMPLE_RATE_HZ,
                    vec![0.1, 0.2, 0.3, 0.4, 0.5, 0.6],
                )
                .expect("captured utterance should become valid transcription input"),
            }
        );
    }

    #[test]
    fn silence_returns_transcription_input_error_for_wrong_sample_rate() {
        let config = VoicePipelineConfig::new(
            SessionConfig::new(VoiceTurnConfig::new(1_200).expect("valid silence timeout")),
            TurnCaptureConfig::new(4, 16).expect("valid turn capture config"),
            44_100,
        );
        let (ready_state, _) = apply_voice_pipeline_event(
            &VoicePipelineState::new(config).expect("pipeline should initialize"),
            config,
            VoicePipelineEvent::StartupValidated,
        )
        .expect("startup validation should succeed");
        let (sleeping_state, _) = apply_voice_pipeline_event(
            &ready_state,
            config,
            VoicePipelineEvent::RecordSleepingFrame {
                frame: vec![0.1, 0.2],
            },
        )
        .expect("sleeping frame should be recorded");
        let (listening_state, _) = apply_voice_pipeline_event(
            &sleeping_state,
            config,
            VoicePipelineEvent::WakeWordDetected { now_ms: 100 },
        )
        .expect("wake word should start listening");

        let error = apply_voice_pipeline_event(
            &listening_state,
            config,
            VoicePipelineEvent::SilenceCheck { now_ms: 1_300 },
        )
        .expect_err("wrong sample rate should fail transcription input validation");

        assert_eq!(
            error,
            VoicePipelineError::TranscriptionInput(
                TranscriptionInputError::UnsupportedSampleRate {
                    expected_hz: PARAKEET_SAMPLE_RATE_HZ,
                    received_hz: 44_100,
                }
            )
        );
    }

    #[test]
    fn prompt_failure_resets_capture_state() {
        let config = pipeline_config();
        let (ready_state, _) = apply_voice_pipeline_event(
            &VoicePipelineState::new(config).expect("pipeline should initialize"),
            config,
            VoicePipelineEvent::StartupValidated,
        )
        .expect("startup validation should succeed");
        let (listening_state, _) = apply_voice_pipeline_event(
            &ready_state,
            config,
            VoicePipelineEvent::WakeWordDetected { now_ms: 100 },
        )
        .expect("wake word should start listening");

        let (failed_state, action) = apply_voice_pipeline_event(
            &listening_state,
            config,
            VoicePipelineEvent::PromptFailed {
                message: "transcription failed".to_string(),
            },
        )
        .expect("prompt failure should succeed");

        assert_eq!(
            failed_state.session().runtime().phase(),
            RuntimePhase::Error
        );
        assert!(!failed_state.capture().capturing_utterance());
        assert_eq!(failed_state.capture().utterance_len(), 0);
        assert_eq!(action, VoicePipelineAction::None);
    }

    #[test]
    fn submit_prompt_moves_runtime_to_executing_from_sleeping() {
        let config = pipeline_config();
        let (ready_state, _) = apply_voice_pipeline_event(
            &VoicePipelineState::new(config).expect("pipeline should initialize"),
            config,
            VoicePipelineEvent::StartupValidated,
        )
        .expect("startup validation should succeed");

        let (executing_state, action) =
            apply_voice_pipeline_event(&ready_state, config, VoicePipelineEvent::SubmitPrompt)
                .expect("submit prompt should enter executing");

        assert_eq!(
            executing_state.session().runtime().phase(),
            RuntimePhase::Executing
        );
        assert!(!executing_state.capture().capturing_utterance());
        assert_eq!(executing_state.capture().utterance_len(), 0);
        assert_eq!(action, VoicePipelineAction::None);
    }

    #[test]
    fn reset_to_idle_clears_active_capture() {
        let config = pipeline_config();
        let (ready_state, _) = apply_voice_pipeline_event(
            &VoicePipelineState::new(config).expect("pipeline should initialize"),
            config,
            VoicePipelineEvent::StartupValidated,
        )
        .expect("startup validation should succeed");
        let (listening_state, _) = apply_voice_pipeline_event(
            &ready_state,
            config,
            VoicePipelineEvent::WakeWordDetected { now_ms: 100 },
        )
        .expect("wake word should start listening");

        let (reset_state, action) =
            apply_voice_pipeline_event(&listening_state, config, VoicePipelineEvent::ResetToIdle)
                .expect("reset should succeed");

        assert_eq!(
            reset_state.session().runtime().phase(),
            RuntimePhase::Sleeping
        );
        assert!(!reset_state.capture().capturing_utterance());
        assert_eq!(reset_state.capture().utterance_len(), 0);
        assert_eq!(action, VoicePipelineAction::None);
    }
}
