use std::path::Path;

const VAD_CONTEXT_SIZE_SAMPLES: usize = 64;
const VAD_CHUNK_SIZE_SAMPLES: usize = 512;
const VAD_SPEECH_THRESHOLD: f32 = 0.5;
pub const VAD_SAMPLE_RATE_HZ: u32 = 16_000;

pub struct VoiceActivityRuntime {
    model: SileroVadModel,
    pending_samples: Vec<f32>,
}

impl VoiceActivityRuntime {
    pub fn load(model_path: &Path) -> Result<Self, VoiceActivityError> {
        Ok(Self {
            model: SileroVadModel::load(model_path)?,
            pending_samples: Vec::new(),
        })
    }

    pub fn process_frame(&mut self, frame: &[f32]) -> Result<bool, VoiceActivityError> {
        self.pending_samples.extend_from_slice(frame);
        detect_speech_in_pending_samples(&mut self.model, &mut self.pending_samples)
    }

    pub fn reset(&mut self) {
        self.pending_samples.clear();
        self.model.reset();
    }
}

struct SileroVadModel {
    session: ort::session::Session,
    sample_rate: ndarray::Array1<i64>,
    state: ndarray::Array3<f32>,
    context: ndarray::Array2<f32>,
    last_sample_rate_hz: u32,
}

impl SileroVadModel {
    fn load(model_path: &Path) -> Result<Self, VoiceActivityError> {
        if !model_path.exists() {
            return Err(VoiceActivityError::LoadFailed {
                details: format!("voice activity model not found: {}", model_path.display()),
            });
        }

        let mut session = ort::session::Session::builder()
            .map_err(|error| VoiceActivityError::LoadFailed {
                details: error.to_string(),
            })?
            .with_optimization_level(ort::session::builder::GraphOptimizationLevel::Level3)
            .map_err(|error| VoiceActivityError::LoadFailed {
                details: error.to_string(),
            })?
            .with_intra_threads(1)
            .map_err(|error| VoiceActivityError::LoadFailed {
                details: error.to_string(),
            })?;
        let session = session.commit_from_file(model_path).map_err(|error| {
            VoiceActivityError::LoadFailed {
                details: error.to_string(),
            }
        })?;

        let mut model = Self {
            session,
            sample_rate: ndarray::arr1(&[VAD_SAMPLE_RATE_HZ as i64]),
            state: ndarray::Array3::zeros((2, 1, 128)),
            context: ndarray::Array2::zeros((1, VAD_CONTEXT_SIZE_SAMPLES)),
            last_sample_rate_hz: 0,
        };

        model.speech_probability(&[0.0; VAD_CHUNK_SIZE_SAMPLES])?;
        model.reset();

        Ok(model)
    }

    fn speech_probability(&mut self, chunk: &[f32]) -> Result<f32, VoiceActivityError> {
        if chunk.len() != VAD_CHUNK_SIZE_SAMPLES {
            return Err(VoiceActivityError::DetectFailed {
                details: format!(
                    "voice activity chunk must be {VAD_CHUNK_SIZE_SAMPLES} samples, got {}",
                    chunk.len()
                ),
            });
        }

        let batch_size = 1;

        if self.last_sample_rate_hz != 0 && self.last_sample_rate_hz != VAD_SAMPLE_RATE_HZ {
            self.reset();
        }

        let input = ndarray::Array2::from_shape_fn(
            (batch_size, chunk.len() + VAD_CONTEXT_SIZE_SAMPLES),
            |(_, index)| {
                if index < VAD_CONTEXT_SIZE_SAMPLES {
                    self.context[[0, index]]
                } else {
                    chunk[index - VAD_CONTEXT_SIZE_SAMPLES]
                }
            },
        );
        let outputs = self
            .session
            .run(ort::inputs![
                ort::value::TensorRef::from_array_view(&input).map_err(|error| {
                    VoiceActivityError::DetectFailed {
                        details: error.to_string(),
                    }
                })?,
                ort::value::TensorRef::from_array_view(&self.state).map_err(|error| {
                    VoiceActivityError::DetectFailed {
                        details: error.to_string(),
                    }
                })?,
                ort::value::TensorRef::from_array_view(&self.sample_rate).map_err(|error| {
                    VoiceActivityError::DetectFailed {
                        details: error.to_string(),
                    }
                })?,
            ])
            .map_err(|error| VoiceActivityError::DetectFailed {
                details: error.to_string(),
            })?;
        let output = outputs
            .get("output")
            .ok_or_else(|| missing_output_error("output"))?;
        let output_tensor = output.try_extract_tensor::<f32>().map_err(|error| {
            VoiceActivityError::DetectFailed {
                details: error.to_string(),
            }
        })?;
        let state = outputs
            .get("stateN")
            .ok_or_else(|| missing_output_error("recurrent state"))?;
        let next_state = state.try_extract_tensor::<f32>().map_err(|error| {
            VoiceActivityError::DetectFailed {
                details: error.to_string(),
            }
        })?;

        self.state = ndarray::Array3::from_shape_vec((2, 1, 128), next_state.1.to_vec()).map_err(
            |error| VoiceActivityError::DetectFailed {
                details: error.to_string(),
            },
        )?;
        self.context =
            ndarray::Array2::from_shape_fn((batch_size, VAD_CONTEXT_SIZE_SAMPLES), |(_, index)| {
                chunk[chunk.len() - VAD_CONTEXT_SIZE_SAMPLES + index]
            });
        self.last_sample_rate_hz = VAD_SAMPLE_RATE_HZ;

        Ok(output_tensor.1.iter().next().copied().unwrap_or(0.0))
    }

    fn reset(&mut self) {
        self.state.fill(0.0);
        self.context.fill(0.0);
        self.last_sample_rate_hz = 0;
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VoiceActivityError {
    LoadFailed { details: String },
    DetectFailed { details: String },
}

trait SpeechDetector {
    fn speech_probability(&mut self, chunk: &[f32]) -> Result<f32, VoiceActivityError>;
}

impl SpeechDetector for SileroVadModel {
    fn speech_probability(&mut self, chunk: &[f32]) -> Result<f32, VoiceActivityError> {
        SileroVadModel::speech_probability(self, chunk)
    }
}

fn missing_output_error(description: &'static str) -> VoiceActivityError {
    VoiceActivityError::DetectFailed {
        details: format!("voice activity model did not return a {description} tensor"),
    }
}

fn detect_speech_in_pending_samples<E: SpeechDetector>(
    detector: &mut E,
    pending_samples: &mut Vec<f32>,
) -> Result<bool, VoiceActivityError> {
    let mut consumed_samples = 0;
    let mut speech_detected = false;

    while pending_samples.len().saturating_sub(consumed_samples) >= VAD_CHUNK_SIZE_SAMPLES {
        let chunk_end = consumed_samples + VAD_CHUNK_SIZE_SAMPLES;
        let probability =
            detector.speech_probability(&pending_samples[consumed_samples..chunk_end])?;

        if probability >= VAD_SPEECH_THRESHOLD {
            speech_detected = true;
        }

        consumed_samples = chunk_end;
    }

    if consumed_samples > 0 {
        pending_samples.drain(..consumed_samples);
    }

    Ok(speech_detected)
}

#[cfg(test)]
mod tests {
    use super::{
        detect_speech_in_pending_samples, missing_output_error, SpeechDetector, VoiceActivityError,
    };

    struct FakeDetector {
        probabilities: Vec<f32>,
    }

    impl SpeechDetector for FakeDetector {
        fn speech_probability(&mut self, _chunk: &[f32]) -> Result<f32, VoiceActivityError> {
            Ok(self.probabilities.remove(0))
        }
    }

    #[test]
    fn buffered_detection_processes_full_chunks_and_keeps_remainder() {
        let mut detector = FakeDetector {
            probabilities: vec![0.2, 0.7, 0.1],
        };
        let mut pending_samples = vec![0.0; 1_600];

        assert_eq!(
            detect_speech_in_pending_samples(&mut detector, &mut pending_samples),
            Ok(true)
        );
        assert_eq!(pending_samples.len(), 64);
    }

    #[test]
    fn buffered_detection_reports_no_speech_below_threshold() {
        let mut detector = FakeDetector {
            probabilities: vec![0.2],
        };
        let mut pending_samples = vec![0.0; 512];

        assert_eq!(
            detect_speech_in_pending_samples(&mut detector, &mut pending_samples),
            Ok(false)
        );
        assert!(pending_samples.is_empty());
    }

    #[test]
    fn required_output_reports_missing_entries_without_panicking() {
        assert_eq!(
            missing_output_error("output"),
            VoiceActivityError::DetectFailed {
                details: String::from("voice activity model did not return a output tensor"),
            }
        );
    }
}
