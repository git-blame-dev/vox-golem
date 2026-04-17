use std::collections::VecDeque;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioBufferError {
    InvalidCapacity,
    CapacityExceeded {
        max_samples: usize,
        attempted_total: usize,
    },
}

pub fn max_samples_for_duration(sample_rate_hz: u32, duration_ms: u32) -> usize {
    let rate = sample_rate_hz as u64;
    let duration = duration_ms as u64;
    let samples = (rate.saturating_mul(duration).saturating_add(999)) / 1_000;

    if samples > usize::MAX as u64 {
        usize::MAX
    } else {
        samples as usize
    }
}

#[derive(Debug, Clone)]
// Holds a sliding analysis window for sleeping-state detection. Old samples are disposable.
pub struct RollingAudioBuffer {
    max_samples: usize,
    samples: VecDeque<f32>,
}

impl RollingAudioBuffer {
    pub fn new(max_samples: usize) -> Result<Self, AudioBufferError> {
        if max_samples == 0 {
            return Err(AudioBufferError::InvalidCapacity);
        }

        Ok(Self {
            max_samples,
            samples: VecDeque::with_capacity(max_samples),
        })
    }

    pub fn append_frame(&mut self, frame: &[f32]) -> usize {
        self.samples.extend(frame.iter().copied());

        let overflow = self.samples.len().saturating_sub(self.max_samples);

        if overflow > 0 {
            self.samples.drain(0..overflow);
        }

        overflow
    }

    pub fn len(&self) -> usize {
        self.samples.len()
    }

    pub fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }

    pub fn as_vec(&self) -> Vec<f32> {
        self.samples.iter().copied().collect()
    }
}

#[derive(Debug, Clone)]
// Holds the active utterance from start-listening to end-of-speech. Samples are retained intact.
pub struct UtteranceAudioBuffer {
    max_samples: usize,
    samples: Vec<f32>,
}

impl UtteranceAudioBuffer {
    pub fn new(max_samples: usize) -> Result<Self, AudioBufferError> {
        if max_samples == 0 {
            return Err(AudioBufferError::InvalidCapacity);
        }

        Ok(Self {
            max_samples,
            samples: Vec::with_capacity(max_samples),
        })
    }

    pub fn append_frame(&mut self, frame: &[f32]) -> Result<(), AudioBufferError> {
        let attempted_total = self.samples.len().saturating_add(frame.len());

        if attempted_total > self.max_samples {
            return Err(AudioBufferError::CapacityExceeded {
                max_samples: self.max_samples,
                attempted_total,
            });
        }

        self.samples.extend_from_slice(frame);
        Ok(())
    }

    pub fn len(&self) -> usize {
        self.samples.len()
    }

    pub fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }

    pub fn as_slice(&self) -> &[f32] {
        &self.samples
    }

    pub fn clear(&mut self) {
        self.samples.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::{
        max_samples_for_duration, AudioBufferError, RollingAudioBuffer, UtteranceAudioBuffer,
    };

    #[test]
    fn computes_capacity_from_duration_and_sample_rate() {
        assert_eq!(max_samples_for_duration(16_000, 250), 4_000);
        assert_eq!(max_samples_for_duration(16_000, 1), 16);
        assert_eq!(max_samples_for_duration(16_000, 0), 0);
    }

    #[test]
    fn rolling_buffer_keeps_newest_samples_within_capacity() {
        let mut buffer =
            RollingAudioBuffer::new(4).expect("rolling buffer should accept non-zero capacity");

        assert_eq!(buffer.append_frame(&[0.1, 0.2, 0.3]), 0);
        assert_eq!(buffer.append_frame(&[0.4, 0.5]), 1);
        assert_eq!(buffer.as_vec(), vec![0.2, 0.3, 0.4, 0.5]);
        assert_eq!(buffer.len(), 4);
    }

    #[test]
    fn utterance_buffer_returns_error_when_capacity_would_be_exceeded() {
        let mut buffer =
            UtteranceAudioBuffer::new(3).expect("utterance buffer should accept non-zero capacity");

        buffer
            .append_frame(&[0.1, 0.2])
            .expect("initial append should fit");

        assert_eq!(
            buffer.append_frame(&[0.3, 0.4]),
            Err(AudioBufferError::CapacityExceeded {
                max_samples: 3,
                attempted_total: 4,
            })
        );

        assert_eq!(buffer.as_slice(), &[0.1, 0.2]);
        assert_eq!(buffer.len(), 2);
    }

    #[test]
    fn rejects_zero_capacity_for_both_buffer_types() {
        assert!(matches!(
            RollingAudioBuffer::new(0),
            Err(AudioBufferError::InvalidCapacity)
        ));
        assert!(matches!(
            UtteranceAudioBuffer::new(0),
            Err(AudioBufferError::InvalidCapacity)
        ));
    }

    #[test]
    fn utterance_buffer_clear_removes_recorded_samples() {
        let mut buffer =
            UtteranceAudioBuffer::new(4).expect("utterance buffer should accept non-zero capacity");

        buffer
            .append_frame(&[0.1, 0.2, 0.3])
            .expect("append should fit");
        buffer.clear();

        assert!(buffer.as_slice().is_empty());
    }
}
