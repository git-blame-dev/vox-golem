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
    SpeechDetected { now_ms: u64 },
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
    ExpiredWithoutSpeech,
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
            capture.begin_utterance_without_preroll();

            Ok((
                VoicePipelineState { session, capture },
                VoicePipelineAction::StartedListening,
            ))
        }
        VoicePipelineEvent::SpeechDetected { now_ms } => Ok((
            VoicePipelineState {
                session: apply_session_event(
                    state.session(),
                    config.session(),
                    SessionEvent::SpeechDetected { now_ms },
                )
                .map_err(VoicePipelineError::Session)?,
                capture: state.capture.clone(),
            },
            VoicePipelineAction::None,
        )),
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
            let mut next_state = state.clone();
            let action = prepare_silence_check(&mut next_state, config, now_ms)?.commit();
            Ok((next_state, action))
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

/// A prepared silence transition. Dropping it preserves session and audio state,
/// allowing callers to finish fallible input-runtime resets before committing.
#[must_use = "a prepared silence transition must be committed to update the pipeline"]
pub struct VoiceSilenceUpdate<'a> {
    state: &'a mut VoicePipelineState,
    next_session: SessionState,
    action: VoicePipelineAction,
}

impl VoiceSilenceUpdate<'_> {
    pub fn action(&self) -> &VoicePipelineAction {
        &self.action
    }

    pub fn commit(self) -> VoicePipelineAction {
        if matches!(
            self.action,
            VoicePipelineAction::FinishedUtterance { .. }
                | VoicePipelineAction::ExpiredWithoutSpeech
        ) {
            self.state.capture.reset();
        }
        self.state.session = self.next_session;
        self.action
    }
}

pub fn prepare_silence_check(
    state: &mut VoicePipelineState,
    config: VoicePipelineConfig,
    now_ms: u64,
) -> Result<VoiceSilenceUpdate<'_>, VoicePipelineError> {
    let previous_phase = state.session().runtime().phase();
    let next_session = apply_session_event(
        state.session(),
        config.session(),
        SessionEvent::SilenceCheck { now_ms },
    )
    .map_err(VoicePipelineError::Session)?;
    let action = if previous_phase == RuntimePhase::Listening
        && next_session.runtime().phase() == RuntimePhase::Processing
    {
        // Copy only at the endpoint, retaining the capture allocation for reuse.
        // Validation must succeed before either captured audio or phase changes.
        let transcription_input = ParakeetTranscriptionInput::new(
            config.sample_rate_hz(),
            state.capture().utterance_samples().to_vec(),
        )
        .map_err(VoicePipelineError::TranscriptionInput)?;
        VoicePipelineAction::FinishedUtterance {
            transcription_input,
        }
    } else if previous_phase == RuntimePhase::Listening
        && next_session.runtime().phase() == RuntimePhase::Sleeping
    {
        VoicePipelineAction::ExpiredWithoutSpeech
    } else {
        VoicePipelineAction::None
    };
    Ok(VoiceSilenceUpdate {
        state,
        next_session,
        action,
    })
}

/// A validated frame update. Dropping it leaves the pipeline unchanged, allowing
/// callers to complete fallible runtime work before committing captured audio.
pub struct VoiceFrameUpdate<'a> {
    state: &'a mut VoicePipelineState,
    frame: &'a [f32],
    next_session: SessionState,
    begin_without_preroll: bool,
    listening: bool,
}

impl VoiceFrameUpdate<'_> {
    pub fn commit(self) {
        if self.begin_without_preroll {
            self.state.capture.begin_utterance_without_preroll();
        }
        if self.listening {
            // Preparation checked the actual buffer capacity and this plan holds
            // its exclusive borrow until commit, so the append cannot overflow.
            self.state
                .capture
                .record_listening_frame(self.frame)
                .expect("frame capacity validated under exclusive pipeline borrow");
        } else {
            self.state.capture.record_sleeping_frame(self.frame);
        }
        self.state.session = self.next_session;
    }
}

pub fn prepare_audio_frame_update<'a>(
    state: &'a mut VoicePipelineState,
    config: VoicePipelineConfig,
    frame: &'a [f32],
    wake_at: Option<u64>,
    speech_at: Option<u64>,
) -> Result<VoiceFrameUpdate<'a>, VoicePipelineError> {
    let begin_without_preroll =
        state.session.runtime().phase() == RuntimePhase::Sleeping && wake_at.is_some();
    let mut next_session = if begin_without_preroll {
        apply_session_event(
            &state.session,
            config.session(),
            SessionEvent::WakeWordDetected {
                now_ms: wake_at.expect("wake timestamp checked above"),
            },
        )
        .map_err(VoicePipelineError::Session)?
    } else {
        state.session.clone()
    };
    let listening = next_session.runtime().phase() == RuntimePhase::Listening;
    if listening && (begin_without_preroll || state.capture.capturing_utterance()) {
        let attempted_total = if begin_without_preroll {
            frame.len()
        } else {
            state.capture.utterance_len().saturating_add(frame.len())
        };
        let max_samples = state.capture.max_utterance_samples();
        if attempted_total > max_samples {
            return Err(VoicePipelineError::Capture(
                voxgolem_audio::buffers::AudioBufferError::CapacityExceeded {
                    max_samples,
                    attempted_total,
                },
            ));
        }
    }
    if listening {
        if let Some(now_ms) = speech_at {
            next_session = apply_session_event(
                &next_session,
                config.session(),
                SessionEvent::SpeechDetected { now_ms },
            )
            .map_err(VoicePipelineError::Session)?;
        }
    }
    Ok(VoiceFrameUpdate {
        state,
        frame,
        next_session,
        begin_without_preroll,
        listening,
    })
}

pub fn ingest_audio_frame(
    state: &VoicePipelineState,
    config: VoicePipelineConfig,
    frame: Vec<f32>,
) -> Result<VoicePipelineState, VoicePipelineError> {
    let event = if state.session().runtime().phase() == RuntimePhase::Listening {
        VoicePipelineEvent::RecordListeningFrame { frame }
    } else {
        VoicePipelineEvent::RecordSleepingFrame { frame }
    };

    apply_voice_pipeline_event(state, config, event).map(|(next_state, _)| next_state)
}

#[cfg(test)]
mod tests {
    use super::prepare_audio_frame_update;
    use crate::runtime::RuntimePhase;
    use crate::session::SessionConfig;
    use crate::turn_capture::TurnCaptureConfig;
    use crate::voice_turn::VoiceTurnConfig;
    use voxgolem_model::parakeet::{
        ParakeetTranscriptionInput, TranscriptionInputError, PARAKEET_SAMPLE_RATE_HZ,
    };

    use super::{
        apply_voice_pipeline_event, ingest_audio_frame, VoicePipelineAction, VoicePipelineConfig,
        VoicePipelineError, VoicePipelineEvent, VoicePipelineState,
    };

    fn pipeline_config() -> VoicePipelineConfig {
        VoicePipelineConfig::new(
            SessionConfig::new(
                VoiceTurnConfig::with_timeouts(1_200, 1_200).expect("valid silence timeouts"),
            ),
            TurnCaptureConfig::new(4, 16).expect("valid turn capture config"),
            PARAKEET_SAMPLE_RATE_HZ,
        )
    }

    fn silence_test_state(heard_speech: bool, samples: &[f32]) -> VoicePipelineState {
        let config = pipeline_config();
        let mut state = VoicePipelineState::new(config).unwrap();
        for event in [
            VoicePipelineEvent::StartupValidated,
            VoicePipelineEvent::WakeWordDetected { now_ms: 100 },
            VoicePipelineEvent::RecordListeningFrame {
                frame: samples.to_vec(),
            },
        ] {
            state = apply_voice_pipeline_event(&state, config, event).unwrap().0;
        }
        if heard_speech {
            state = apply_voice_pipeline_event(
                &state,
                config,
                VoicePipelineEvent::SpeechDetected { now_ms: 200 },
            )
            .unwrap()
            .0;
        }
        state
    }

    #[test]
    fn silence_transaction_preserves_audio_allocation_on_noop_and_finish() {
        let config = pipeline_config();
        let mut state = silence_test_state(true, &[0.25, 0.5]);
        let pointer = state.capture().utterance_samples().as_ptr();
        let update = super::prepare_silence_check(&mut state, config, 1_399).unwrap();
        assert_eq!(update.action(), &VoicePipelineAction::None);
        assert_eq!(update.commit(), VoicePipelineAction::None);
        assert_eq!(state.capture().utterance_samples().as_ptr(), pointer);
        let update = super::prepare_silence_check(&mut state, config, 1_400).unwrap();
        let action = update.commit();
        assert_eq!(
            action,
            VoicePipelineAction::FinishedUtterance {
                transcription_input: ParakeetTranscriptionInput::new(16_000, vec![0.25, 0.5])
                    .unwrap(),
            }
        );
        assert_eq!(state.session().runtime().phase(), RuntimePhase::Processing);
        assert!(!state.capture().capturing_utterance());
        assert!(state.capture().utterance_samples().is_empty());
        assert_eq!(state.capture().utterance_samples().as_ptr(), pointer);
    }

    #[test]
    fn dropping_silence_transaction_preserves_state_until_successful_retry() {
        let config = pipeline_config();
        for heard_speech in [false, true] {
            let mut state = silence_test_state(heard_speech, &[0.25, 0.5]);
            let before = state.clone();
            let pointer = state.capture().utterance_samples().as_ptr();
            // Equivalent to a caller returning early after a fallible reset.
            drop(super::prepare_silence_check(&mut state, config, 1_400).unwrap());
            assert_same_state(&state, &before);
            assert_eq!(state.capture().utterance_samples().as_ptr(), pointer);
            let action = super::prepare_silence_check(&mut state, config, 1_400)
                .unwrap()
                .commit();
            assert_eq!(
                state.session().runtime().phase(),
                if heard_speech {
                    RuntimePhase::Processing
                } else {
                    RuntimePhase::Sleeping
                }
            );
            assert_eq!(
                matches!(action, VoicePipelineAction::ExpiredWithoutSpeech),
                !heard_speech
            );
            assert!(!state.capture().capturing_utterance());
            assert!(state.capture().utterance_samples().is_empty());
            assert_eq!(
                super::prepare_silence_check(&mut state, config, 1_401)
                    .unwrap()
                    .commit(),
                VoicePipelineAction::None
            );
        }
    }

    #[test]
    fn silence_transaction_validation_errors_preserve_state_and_precedence() {
        let config = pipeline_config();
        let cases = [
            (Vec::new(), TranscriptionInputError::EmptyAudio),
            (
                vec![2.0, f32::INFINITY],
                TranscriptionInputError::NonFiniteSample {
                    index: 1,
                    value: f32::INFINITY,
                },
            ),
            (
                vec![0.0, 2.0],
                TranscriptionInputError::OutOfRangeSample {
                    index: 1,
                    value: 2.0,
                },
            ),
        ];
        for (samples, expected) in cases {
            let mut state = silence_test_state(true, &samples);
            let before = state.clone();
            assert!(
                matches!(super::prepare_silence_check(&mut state, config, 1_400), Err(VoicePipelineError::TranscriptionInput(error)) if error == expected)
            );
            assert_same_state(&state, &before);
            let wrong_rate = VoicePipelineConfig::new(config.session(), config.capture(), 44_100);
            assert!(matches!(
                super::prepare_silence_check(&mut state, wrong_rate, 1_400),
                Err(VoicePipelineError::TranscriptionInput(
                    TranscriptionInputError::UnsupportedSampleRate { .. }
                ))
            ));
            assert_same_state(&state, &before);
        }
    }

    #[test]
    fn silence_transaction_only_validates_audio_at_speech_endpoint() {
        let config = pipeline_config();
        let mut state = silence_test_state(true, &[f32::NAN]);
        let pointer = state.capture().utterance_samples().as_ptr();
        assert_eq!(
            super::prepare_silence_check(&mut state, config, 1_399)
                .unwrap()
                .commit(),
            VoicePipelineAction::None
        );
        assert!(matches!(
            super::prepare_silence_check(&mut state, config, 1_400),
            Err(VoicePipelineError::TranscriptionInput(
                TranscriptionInputError::NonFiniteSample { index: 0, value }
            )) if value.is_nan()
        ));
        assert_eq!(state.capture().utterance_samples().as_ptr(), pointer);
        assert!(state.capture().utterance_samples()[0].is_nan());
        assert!(state.capture().capturing_utterance());
        assert_eq!(state.session().runtime().phase(), RuntimePhase::Listening);
        assert_eq!(state.session().voice_turn().last_activity_ms(), Some(200));

        let mut no_speech = silence_test_state(false, &[f32::NAN]);
        assert_eq!(
            super::prepare_silence_check(&mut no_speech, config, 1_300)
                .unwrap()
                .commit(),
            VoicePipelineAction::ExpiredWithoutSpeech
        );
        assert!(no_speech.capture().utterance_samples().is_empty());
    }

    fn immutable_frame_update(
        state: &VoicePipelineState,
        config: VoicePipelineConfig,
        frame: &[f32],
        wake_at: Option<u64>,
        speech_at: Option<u64>,
    ) -> Result<VoicePipelineState, VoicePipelineError> {
        let mut next = state.clone();
        if state.session().runtime().phase() == RuntimePhase::Sleeping {
            if let Some(now_ms) = wake_at {
                next = apply_voice_pipeline_event(
                    &next,
                    config,
                    VoicePipelineEvent::WakeWordDetected { now_ms },
                )?
                .0;
            }
        }
        next = ingest_audio_frame(&next, config, frame.to_vec())?;
        if next.session().runtime().phase() == RuntimePhase::Listening {
            if let Some(now_ms) = speech_at {
                next = apply_voice_pipeline_event(
                    &next,
                    config,
                    VoicePipelineEvent::SpeechDetected { now_ms },
                )?
                .0;
            }
        }
        Ok(next)
    }

    fn assert_same_state(actual: &VoicePipelineState, expected: &VoicePipelineState) {
        assert_eq!(actual.session(), expected.session());
        assert_eq!(
            actual.capture().utterance_samples(),
            expected.capture().utterance_samples()
        );
        assert_eq!(
            actual.capture().capturing_utterance(),
            expected.capture().capturing_utterance()
        );
        assert_eq!(
            actual.capture().max_utterance_samples(),
            expected.capture().max_utterance_samples()
        );
        // Observe the complete private preroll through the real capture API.
        let mut actual_capture = actual.capture().clone();
        let mut expected_capture = expected.capture().clone();
        assert_eq!(
            actual_capture.begin_utterance(),
            expected_capture.begin_utterance()
        );
        assert_eq!(
            actual_capture.utterance_samples(),
            expected_capture.utterance_samples()
        );
    }

    #[test]
    fn prepared_frames_match_immutable_events_across_phases_and_overflow() {
        let config = pipeline_config();
        let initial = VoicePipelineState::new(config).unwrap();
        let ready =
            apply_voice_pipeline_event(&initial, config, VoicePipelineEvent::StartupValidated)
                .unwrap()
                .0;
        let listening = immutable_frame_update(&ready, config, &[0.25], Some(100), None).unwrap();
        let mut inactive = listening.clone();
        inactive.capture.reset();
        let error = apply_voice_pipeline_event(
            &initial,
            config,
            VoicePipelineEvent::StartupFailed {
                message: "synthetic failure".into(),
            },
        )
        .unwrap()
        .0;
        let executing =
            apply_voice_pipeline_event(&ready, config, VoicePipelineEvent::SubmitPrompt)
                .unwrap()
                .0;
        for state in [initial, ready, listening, inactive, error, executing] {
            for (frame, wake, speech) in [
                (vec![0.5, -0.5], None, None),
                (vec![2.0, -2.0], Some(200), Some(250)),
                (vec![0.0; 17], Some(200), Some(250)),
                (vec![], Some(50), Some(1)),
            ] {
                let expected = immutable_frame_update(&state, config, &frame, wake, speech);
                let mut actual = state.clone();
                match prepare_audio_frame_update(&mut actual, config, &frame, wake, speech) {
                    Ok(plan) => {
                        plan.commit();
                        assert_same_state(&actual, &expected.unwrap());
                    }
                    Err(error) => {
                        assert_eq!(error, expected.unwrap_err());
                        assert_same_state(&actual, &state);
                    }
                }
            }
        }
    }

    #[test]
    fn dropped_plan_and_wake_overflow_preserve_every_sample_and_session() {
        let config = pipeline_config();
        let mut state = apply_voice_pipeline_event(
            &VoicePipelineState::new(config).unwrap(),
            config,
            VoicePipelineEvent::StartupValidated,
        )
        .unwrap()
        .0;
        state.capture.record_sleeping_frame(&[0.1, 0.2, 0.3]);
        // Retained old capture must not be cleared by a planned wake.
        state.capture.begin_utterance_without_preroll();
        state.capture.record_listening_frame(&[0.75]).unwrap();
        let before = state.clone();
        let pointer = state.capture().utterance_samples().as_ptr();
        {
            let _uncommitted =
                prepare_audio_frame_update(&mut state, config, &[0.5], Some(100), None).unwrap();
            // Models any caller-side clock/reset error before commit.
        }
        assert_same_state(&state, &before);
        assert_eq!(pointer, state.capture().utterance_samples().as_ptr());
        let larger_config = VoicePipelineConfig::new(
            config.session(),
            TurnCaptureConfig::new(4, 64).unwrap(),
            config.sample_rate_hz(),
        );
        assert!(matches!(
            prepare_audio_frame_update(&mut state, larger_config, &[0.0; 17], Some(100), None),
            Err(VoicePipelineError::Capture(
                voxgolem_audio::buffers::AudioBufferError::CapacityExceeded {
                    max_samples: 16,
                    attempted_total: 17
                }
            ))
        ));
        assert_same_state(&state, &before);
        assert_eq!(pointer, state.capture().utterance_samples().as_ptr());
    }

    #[test]
    fn long_frame_sequence_retains_reserved_capture_and_matches_immutable_audio() {
        let base = pipeline_config();
        let config = VoicePipelineConfig::new(
            base.session(),
            TurnCaptureConfig::new(4, 512).unwrap(),
            base.sample_rate_hz(),
        );
        let mut state = apply_voice_pipeline_event(
            &VoicePipelineState::new(config).unwrap(),
            config,
            VoicePipelineEvent::StartupValidated,
        )
        .unwrap()
        .0;
        for _ in 0..32 {
            prepare_audio_frame_update(&mut state, config, &[0.9; 8], None, None)
                .unwrap()
                .commit();
            assert_eq!(state.capture().preroll_len(), 4);
        }
        let mut reference = state.clone();
        let mut audio = Vec::new();
        let mut pointer = None;
        for index in 0..128 {
            let frame = [index as f32 / 128.0; 4];
            let wake = (index == 0).then_some(100);
            let speech = (index > 0).then_some(100 + index);
            reference = immutable_frame_update(&reference, config, &frame, wake, speech).unwrap();
            prepare_audio_frame_update(&mut state, config, &frame, wake, speech)
                .unwrap()
                .commit();
            audio.extend_from_slice(&frame);
            if let Some(pointer) = pointer {
                assert_eq!(state.capture().utterance_samples().as_ptr(), pointer);
            } else {
                // Admission may restore capacity lost by the immutable startup clone.
                pointer = Some(state.capture().utterance_samples().as_ptr());
            }
            assert_eq!(state.capture().utterance_samples(), audio);
            assert_same_state(&state, &reference);
            assert!(state.capture().utterance_len() <= state.capture().max_utterance_samples());
        }
        let before = state.clone();
        drop(prepare_audio_frame_update(&mut state, config, &[], None, Some(300)).unwrap());
        assert_same_state(&state, &before);
        assert!(prepare_audio_frame_update(&mut state, config, &[0.0], None, Some(300)).is_err());
        assert_same_state(&state, &before);
        assert_eq!(state.capture().utterance_len(), 512);
        assert_eq!(Some(state.capture().utterance_samples().as_ptr()), pointer);
    }

    #[test]
    fn wake_word_starts_listening_without_seeding_sleeping_preroll() {
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
        assert_eq!(listening_state.capture().utterance_len(), 0);
        assert_eq!(action, VoicePipelineAction::StartedListening);
    }

    #[test]
    fn behavior_wake_word_detected_does_not_seed_sleeping_preroll_into_utterance() {
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
                frame: vec![0.9, 0.8, 0.7, 0.6],
            },
        )
        .expect("sleeping frame should be recorded");
        let (listening_state, _) = apply_voice_pipeline_event(
            &sleeping_state,
            config,
            VoicePipelineEvent::WakeWordDetected { now_ms: 100 },
        )
        .expect("wake word should start listening");
        let (speaking_state, _) = apply_voice_pipeline_event(
            &listening_state,
            config,
            VoicePipelineEvent::SpeechDetected { now_ms: 200 },
        )
        .expect("speech should be detected");
        let (recording_state, _) = apply_voice_pipeline_event(
            &speaking_state,
            config,
            VoicePipelineEvent::RecordListeningFrame {
                frame: vec![0.1, 0.2],
            },
        )
        .expect("listening frame should be recorded");

        let (_, action) = apply_voice_pipeline_event(
            &recording_state,
            config,
            VoicePipelineEvent::SilenceCheck { now_ms: 1_400 },
        )
        .expect("silence timeout should finish the utterance");

        assert_eq!(
            action,
            VoicePipelineAction::FinishedUtterance {
                transcription_input: ParakeetTranscriptionInput::new(
                    PARAKEET_SAMPLE_RATE_HZ,
                    vec![0.1, 0.2],
                )
                .expect("captured utterance should become valid transcription input"),
            }
        );
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
        let (speaking_state, _) = apply_voice_pipeline_event(
            &listening_state,
            config,
            VoicePipelineEvent::SpeechDetected { now_ms: 200 },
        )
        .expect("speech should be detected");
        let (recording_state, _) = apply_voice_pipeline_event(
            &speaking_state,
            config,
            VoicePipelineEvent::RecordListeningFrame {
                frame: vec![0.5, 0.6],
            },
        )
        .expect("listening frame should be recorded");

        let (processing_state, action) = apply_voice_pipeline_event(
            &recording_state,
            config,
            VoicePipelineEvent::SilenceCheck { now_ms: 1_400 },
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
                    vec![0.5, 0.6],
                )
                .expect("captured utterance should become valid transcription input"),
            }
        );
    }

    #[test]
    fn initial_silence_expiry_resets_capture_without_transcription_input() {
        let config = VoicePipelineConfig::new(
            SessionConfig::new(
                VoiceTurnConfig::with_timeouts(2_500, 750).expect("valid silence timeouts"),
            ),
            TurnCaptureConfig::new(4, 16).expect("valid turn capture config"),
            PARAKEET_SAMPLE_RATE_HZ,
        );
        let (ready, _) = apply_voice_pipeline_event(
            &VoicePipelineState::new(config).expect("pipeline should initialize"),
            config,
            VoicePipelineEvent::StartupValidated,
        )
        .expect("startup validation should succeed");
        let (listening, _) = apply_voice_pipeline_event(
            &ready,
            config,
            VoicePipelineEvent::WakeWordDetected { now_ms: 0 },
        )
        .expect("wake word should start listening");
        let (recording, _) = apply_voice_pipeline_event(
            &listening,
            config,
            VoicePipelineEvent::RecordListeningFrame {
                frame: vec![0.25, 0.5],
            },
        )
        .expect("listening frame should be captured");

        let (expired, action) = apply_voice_pipeline_event(
            &recording,
            config,
            VoicePipelineEvent::SilenceCheck { now_ms: 2_500 },
        )
        .expect("no-speech expiry should not transcribe");

        assert_eq!(action, VoicePipelineAction::ExpiredWithoutSpeech);
        assert_eq!(expired.session().runtime().phase(), RuntimePhase::Sleeping);
        assert!(!expired.capture().capturing_utterance());
        assert_eq!(expired.capture().utterance_len(), 0);
    }

    #[test]
    fn speech_detected_refreshes_activity_and_delays_silence_timeout() {
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

        let (refreshed_state, action) = apply_voice_pipeline_event(
            &listening_state,
            config,
            VoicePipelineEvent::SpeechDetected { now_ms: 450 },
        )
        .expect("speech should refresh listening activity");

        assert_eq!(
            refreshed_state.session().voice_turn().last_activity_ms(),
            Some(450)
        );
        assert_eq!(action, VoicePipelineAction::None);

        let (still_listening_state, action) = apply_voice_pipeline_event(
            &refreshed_state,
            config,
            VoicePipelineEvent::SilenceCheck { now_ms: 1_400 },
        )
        .expect("refreshed activity should delay silence timeout");

        assert_eq!(
            still_listening_state.session().runtime().phase(),
            RuntimePhase::Listening
        );
        assert_eq!(
            still_listening_state
                .session()
                .voice_turn()
                .last_activity_ms(),
            Some(450)
        );
        assert_eq!(action, VoicePipelineAction::None);
    }

    #[test]
    fn silence_returns_transcription_input_error_for_wrong_sample_rate() {
        let config = VoicePipelineConfig::new(
            SessionConfig::new(
                VoiceTurnConfig::with_timeouts(1_200, 1_200).expect("valid silence timeouts"),
            ),
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
        let (speaking_state, _) = apply_voice_pipeline_event(
            &listening_state,
            config,
            VoicePipelineEvent::SpeechDetected { now_ms: 200 },
        )
        .expect("speech should be detected");

        let error = apply_voice_pipeline_event(
            &speaking_state,
            config,
            VoicePipelineEvent::SilenceCheck { now_ms: 1_400 },
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

    #[test]
    fn ingest_audio_frame_routes_sleeping_audio_into_preroll() {
        let config = pipeline_config();
        let (ready_state, _) = apply_voice_pipeline_event(
            &VoicePipelineState::new(config).expect("pipeline should initialize"),
            config,
            VoicePipelineEvent::StartupValidated,
        )
        .expect("startup validation should succeed");

        let next_state = ingest_audio_frame(&ready_state, config, vec![0.1, 0.2, 0.3])
            .expect("sleeping audio frame should be routed into preroll");

        assert_eq!(next_state.capture().preroll_len(), 3);
        assert_eq!(next_state.capture().utterance_len(), 0);
    }

    #[test]
    fn ingest_audio_frame_routes_listening_audio_into_utterance() {
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

        let next_state = ingest_audio_frame(&listening_state, config, vec![0.5, 0.6])
            .expect("listening audio frame should be routed into utterance");

        assert_eq!(next_state.capture().utterance_len(), 2);
        assert_eq!(next_state.capture().preroll_len(), 0);
    }
}
