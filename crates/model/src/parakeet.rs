pub const PARAKEET_SAMPLE_RATE_HZ: u32 = 16_000;

#[derive(Debug, Clone, PartialEq)]
pub struct ParakeetTranscriptionInput {
    sample_rate_hz: u32,
    samples: Vec<f32>,
}

impl ParakeetTranscriptionInput {
    pub fn new(sample_rate_hz: u32, samples: Vec<f32>) -> Result<Self, TranscriptionInputError> {
        if sample_rate_hz != PARAKEET_SAMPLE_RATE_HZ {
            return Err(TranscriptionInputError::UnsupportedSampleRate {
                expected_hz: PARAKEET_SAMPLE_RATE_HZ,
                received_hz: sample_rate_hz,
            });
        }

        if samples.is_empty() {
            return Err(TranscriptionInputError::EmptyAudio);
        }

        if let Some((index, value)) = samples
            .iter()
            .copied()
            .enumerate()
            .find(|(_, value)| !value.is_finite())
        {
            return Err(TranscriptionInputError::NonFiniteSample { index, value });
        }

        if let Some((index, value)) = samples
            .iter()
            .copied()
            .enumerate()
            .find(|(_, value)| !(-1.0..=1.0).contains(value))
        {
            return Err(TranscriptionInputError::OutOfRangeSample { index, value });
        }

        Ok(Self {
            sample_rate_hz,
            samples,
        })
    }

    pub fn sample_rate_hz(&self) -> u32 {
        self.sample_rate_hz
    }

    pub fn samples(&self) -> &[f32] {
        &self.samples
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TranscriptionInputError {
    EmptyAudio,
    UnsupportedSampleRate { expected_hz: u32, received_hz: u32 },
    NonFiniteSample { index: usize, value: f32 },
    OutOfRangeSample { index: usize, value: f32 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Transcript {
    text: String,
}

impl Transcript {
    pub fn new(text: impl Into<String>) -> Result<Self, TranscriptError> {
        let text = text.into();

        if text.trim().is_empty() {
            return Err(TranscriptError::EmptyText);
        }

        Ok(Self { text })
    }

    pub fn text(&self) -> &str {
        &self.text
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TranscriptError {
    EmptyText,
}

#[cfg(test)]
mod tests {
    use super::{
        ParakeetTranscriptionInput, Transcript, TranscriptError, TranscriptionInputError,
        PARAKEET_SAMPLE_RATE_HZ,
    };

    #[test]
    fn accepts_normalized_audio_at_expected_sample_rate() {
        let input = ParakeetTranscriptionInput::new(PARAKEET_SAMPLE_RATE_HZ, vec![-1.0, 0.0, 1.0])
            .expect("normalized parakeet input should be accepted");

        assert_eq!(input.sample_rate_hz(), PARAKEET_SAMPLE_RATE_HZ);
        assert_eq!(input.samples(), &[-1.0, 0.0, 1.0]);
    }

    #[test]
    fn rejects_empty_audio() {
        assert_eq!(
            ParakeetTranscriptionInput::new(PARAKEET_SAMPLE_RATE_HZ, Vec::new()),
            Err(TranscriptionInputError::EmptyAudio)
        );
    }

    #[test]
    fn rejects_wrong_sample_rate() {
        assert_eq!(
            ParakeetTranscriptionInput::new(44_100, vec![0.0]),
            Err(TranscriptionInputError::UnsupportedSampleRate {
                expected_hz: PARAKEET_SAMPLE_RATE_HZ,
                received_hz: 44_100,
            })
        );
    }

    #[test]
    fn rejects_non_finite_samples() {
        let result = ParakeetTranscriptionInput::new(PARAKEET_SAMPLE_RATE_HZ, vec![0.0, f32::NAN]);

        assert!(matches!(
            result,
            Err(TranscriptionInputError::NonFiniteSample { index: 1, value }) if value.is_nan()
        ));
    }

    #[test]
    fn rejects_out_of_range_samples() {
        assert_eq!(
            ParakeetTranscriptionInput::new(PARAKEET_SAMPLE_RATE_HZ, vec![0.0, 1.25]),
            Err(TranscriptionInputError::OutOfRangeSample {
                index: 1,
                value: 1.25,
            })
        );
    }

    #[test]
    fn accepts_non_empty_transcript_text() {
        let transcript = Transcript::new("open the pull request")
            .expect("non-empty transcript text should be accepted");

        assert_eq!(transcript.text(), "open the pull request");
    }

    #[test]
    fn rejects_blank_transcript_text() {
        assert_eq!(Transcript::new("   \n\t "), Err(TranscriptError::EmptyText));
    }
}
