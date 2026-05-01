use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalTtsRequest {
    pub text: String,
    pub sample_rate_hz: u32,
}

impl LocalTtsRequest {
    pub fn new(text: impl Into<String>, sample_rate_hz: u32) -> Result<Self, LocalTtsError> {
        let text = text.into();
        if text.trim().is_empty() {
            return Err(LocalTtsError::EmptyText);
        }

        if sample_rate_hz == 0 {
            return Err(LocalTtsError::InvalidSampleRate);
        }

        Ok(Self {
            text,
            sample_rate_hz,
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct LocalTtsAudio {
    pub pcm_f32: Vec<f32>,
    pub sample_rate_hz: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalTtsModelSpec {
    pub model_path: PathBuf,
    pub worker_count: usize,
    pub max_queue: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalTtsError {
    EmptyText,
    InvalidSampleRate,
    InvalidWorkerCount,
    InvalidQueueSize,
}

impl LocalTtsModelSpec {
    pub fn validate(&self) -> Result<(), LocalTtsError> {
        if self.worker_count == 0 {
            return Err(LocalTtsError::InvalidWorkerCount);
        }

        if self.max_queue == 0 {
            return Err(LocalTtsError::InvalidQueueSize);
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{LocalTtsError, LocalTtsModelSpec, LocalTtsRequest};
    use std::path::PathBuf;

    #[test]
    fn rejects_empty_request_text() {
        assert_eq!(
            LocalTtsRequest::new("  ", 22_050),
            Err(LocalTtsError::EmptyText)
        );
    }

    #[test]
    fn rejects_invalid_sample_rate() {
        assert_eq!(
            LocalTtsRequest::new("hello", 0),
            Err(LocalTtsError::InvalidSampleRate)
        );
    }

    #[test]
    fn validates_model_spec_fields() {
        let invalid_workers = LocalTtsModelSpec {
            model_path: PathBuf::from("model.onnx"),
            worker_count: 0,
            max_queue: 16,
        };
        assert_eq!(
            invalid_workers.validate(),
            Err(LocalTtsError::InvalidWorkerCount)
        );

        let invalid_queue = LocalTtsModelSpec {
            model_path: PathBuf::from("model.onnx"),
            worker_count: 2,
            max_queue: 0,
        };
        assert_eq!(
            invalid_queue.validate(),
            Err(LocalTtsError::InvalidQueueSize)
        );
    }
}
