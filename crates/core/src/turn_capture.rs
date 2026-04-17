use voxgolem_audio::buffers::{AudioBufferError, RollingAudioBuffer, UtteranceAudioBuffer};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TurnCaptureConfig {
    preroll_max_samples: usize,
    utterance_max_samples: usize,
}

impl TurnCaptureConfig {
    pub fn new(
        preroll_max_samples: usize,
        utterance_max_samples: usize,
    ) -> Result<Self, AudioBufferError> {
        if preroll_max_samples == 0 || utterance_max_samples == 0 {
            return Err(AudioBufferError::InvalidCapacity);
        }

        Ok(Self {
            preroll_max_samples,
            utterance_max_samples,
        })
    }

    pub fn preroll_max_samples(&self) -> usize {
        self.preroll_max_samples
    }

    pub fn utterance_max_samples(&self) -> usize {
        self.utterance_max_samples
    }
}

#[derive(Debug, Clone)]
pub struct TurnCaptureState {
    preroll: RollingAudioBuffer,
    utterance: UtteranceAudioBuffer,
    capturing_utterance: bool,
}

impl TurnCaptureState {
    pub fn new(config: TurnCaptureConfig) -> Result<Self, AudioBufferError> {
        Ok(Self {
            preroll: RollingAudioBuffer::new(config.preroll_max_samples())?,
            utterance: UtteranceAudioBuffer::new(config.utterance_max_samples())?,
            capturing_utterance: false,
        })
    }

    pub fn capturing_utterance(&self) -> bool {
        self.capturing_utterance
    }

    pub fn preroll_len(&self) -> usize {
        self.preroll.len()
    }

    pub fn utterance_len(&self) -> usize {
        self.utterance.len()
    }

    pub fn record_sleeping_frame(&mut self, frame: &[f32]) {
        self.preroll.append_frame(frame);
    }

    pub fn begin_utterance(&mut self) -> Result<(), AudioBufferError> {
        self.utterance.clear();
        self.utterance.append_frame(&self.preroll.as_vec())?;
        self.capturing_utterance = true;
        Ok(())
    }

    pub fn record_listening_frame(&mut self, frame: &[f32]) -> Result<(), AudioBufferError> {
        if !self.capturing_utterance {
            return Ok(());
        }

        self.utterance.append_frame(frame)
    }

    pub fn finish_utterance(&mut self) -> Vec<f32> {
        self.capturing_utterance = false;
        let captured = self.utterance.as_slice().to_vec();
        self.utterance.clear();
        captured
    }

    pub fn reset(&mut self) {
        self.capturing_utterance = false;
        self.utterance.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::{TurnCaptureConfig, TurnCaptureState};
    use voxgolem_audio::buffers::AudioBufferError;

    #[test]
    fn rejects_zero_capacity_config() {
        assert_eq!(
            TurnCaptureConfig::new(0, 16),
            Err(AudioBufferError::InvalidCapacity)
        );
        assert_eq!(
            TurnCaptureConfig::new(16, 0),
            Err(AudioBufferError::InvalidCapacity)
        );
    }

    #[test]
    fn preroll_is_seeded_into_new_utterance() {
        let config = TurnCaptureConfig::new(4, 8).expect("valid capture config");
        let mut state = TurnCaptureState::new(config).expect("capture state should initialize");

        state.record_sleeping_frame(&[0.1, 0.2]);
        state.record_sleeping_frame(&[0.3, 0.4]);
        state
            .begin_utterance()
            .expect("preroll should fit in utterance buffer");
        state
            .record_listening_frame(&[0.5, 0.6])
            .expect("listening frame should fit");

        assert!(state.capturing_utterance());
        assert_eq!(state.preroll_len(), 4);
        assert_eq!(state.utterance_len(), 6);

        let captured = state.finish_utterance();
        assert_eq!(captured, vec![0.1, 0.2, 0.3, 0.4, 0.5, 0.6]);
        assert!(!state.capturing_utterance());
        assert_eq!(state.utterance_len(), 0);
    }

    #[test]
    fn sleeping_preroll_keeps_only_newest_samples() {
        let config = TurnCaptureConfig::new(3, 8).expect("valid capture config");
        let mut state = TurnCaptureState::new(config).expect("capture state should initialize");

        state.record_sleeping_frame(&[0.1, 0.2]);
        state.record_sleeping_frame(&[0.3, 0.4]);
        state
            .begin_utterance()
            .expect("preroll should fit in utterance buffer");

        let captured = state.finish_utterance();
        assert_eq!(captured, vec![0.2, 0.3, 0.4]);
    }

    #[test]
    fn listening_frames_are_ignored_until_capture_starts() {
        let config = TurnCaptureConfig::new(4, 8).expect("valid capture config");
        let mut state = TurnCaptureState::new(config).expect("capture state should initialize");

        state
            .record_listening_frame(&[0.9, 1.0])
            .expect("frames before capture should be ignored");

        assert_eq!(state.utterance_len(), 0);
        assert!(!state.capturing_utterance());
    }

    #[test]
    fn begin_utterance_propagates_capacity_errors() {
        let config = TurnCaptureConfig::new(4, 3).expect("valid capture config");
        let mut state = TurnCaptureState::new(config).expect("capture state should initialize");

        state.record_sleeping_frame(&[0.1, 0.2]);
        state.record_sleeping_frame(&[0.3, 0.4]);

        assert_eq!(
            state.begin_utterance(),
            Err(AudioBufferError::CapacityExceeded {
                max_samples: 3,
                attempted_total: 4,
            })
        );
    }

    #[test]
    fn reset_clears_active_utterance_but_preserves_preroll() {
        let config = TurnCaptureConfig::new(4, 8).expect("valid capture config");
        let mut state = TurnCaptureState::new(config).expect("capture state should initialize");

        state.record_sleeping_frame(&[0.1, 0.2, 0.3, 0.4]);
        state
            .begin_utterance()
            .expect("preroll should fit in utterance buffer");
        state
            .record_listening_frame(&[0.5, 0.6])
            .expect("listening frame should fit");

        state.reset();

        assert!(!state.capturing_utterance());
        assert_eq!(state.utterance_len(), 0);
        assert_eq!(state.preroll_len(), 4);
    }
}
