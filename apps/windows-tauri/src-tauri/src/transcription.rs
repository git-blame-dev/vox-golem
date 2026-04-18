use std::path::Path;

use transcribe_rs::onnx::parakeet::ParakeetModel;
use transcribe_rs::onnx::Quantization;
use transcribe_rs::{SpeechModel, TranscribeOptions};

pub struct ParakeetRuntime {
    model: ParakeetModel,
}

impl ParakeetRuntime {
    pub fn load(model_dir: &Path) -> Result<Self, ParakeetRuntimeError> {
        let model = ParakeetModel::load(model_dir, &Quantization::Int8).map_err(|error| {
            ParakeetRuntimeError::LoadFailed {
                details: error.to_string(),
            }
        })?;

        Ok(Self { model })
    }

    pub fn transcribe(
        &mut self,
        input: &voxgolem_model::parakeet::ParakeetTranscriptionInput,
    ) -> Result<voxgolem_model::parakeet::Transcript, ParakeetRuntimeError> {
        transcribe_with_engine(&mut self.model, input)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParakeetRuntimeError {
    LoadFailed { details: String },
    TranscribeFailed { details: String },
    InvalidTranscript(voxgolem_model::parakeet::TranscriptError),
}

trait ParakeetSpeechEngine {
    fn transcribe_text(
        &mut self,
        input: &voxgolem_model::parakeet::ParakeetTranscriptionInput,
    ) -> Result<String, ParakeetRuntimeError>;
}

impl ParakeetSpeechEngine for ParakeetModel {
    fn transcribe_text(
        &mut self,
        input: &voxgolem_model::parakeet::ParakeetTranscriptionInput,
    ) -> Result<String, ParakeetRuntimeError> {
        self.transcribe(input.samples(), &TranscribeOptions::default())
            .map(|result| result.text)
            .map_err(|error| ParakeetRuntimeError::TranscribeFailed {
                details: error.to_string(),
            })
    }
}

fn transcribe_with_engine<E: ParakeetSpeechEngine>(
    engine: &mut E,
    input: &voxgolem_model::parakeet::ParakeetTranscriptionInput,
) -> Result<voxgolem_model::parakeet::Transcript, ParakeetRuntimeError> {
    let text = engine.transcribe_text(input)?;

    voxgolem_model::parakeet::Transcript::new(text).map_err(ParakeetRuntimeError::InvalidTranscript)
}

#[cfg(test)]
mod tests {
    use super::{transcribe_with_engine, ParakeetRuntimeError, ParakeetSpeechEngine};

    struct FakeEngine {
        result: Result<String, ParakeetRuntimeError>,
    }

    impl ParakeetSpeechEngine for FakeEngine {
        fn transcribe_text(
            &mut self,
            _input: &voxgolem_model::parakeet::ParakeetTranscriptionInput,
        ) -> Result<String, ParakeetRuntimeError> {
            self.result.clone()
        }
    }

    #[test]
    fn transcribe_with_engine_accepts_non_empty_text() {
        let mut engine = FakeEngine {
            result: Ok("open the pull request".to_string()),
        };
        let input = voxgolem_model::parakeet::ParakeetTranscriptionInput::new(
            voxgolem_model::parakeet::PARAKEET_SAMPLE_RATE_HZ,
            vec![0.1, 0.2],
        )
        .expect("valid input");

        let transcript = transcribe_with_engine(&mut engine, &input)
            .expect("non-empty engine transcript should be accepted");

        assert_eq!(transcript.text(), "open the pull request");
    }

    #[test]
    fn transcribe_with_engine_rejects_blank_text() {
        let mut engine = FakeEngine {
            result: Ok("   ".to_string()),
        };
        let input = voxgolem_model::parakeet::ParakeetTranscriptionInput::new(
            voxgolem_model::parakeet::PARAKEET_SAMPLE_RATE_HZ,
            vec![0.1, 0.2],
        )
        .expect("valid input");

        assert_eq!(
            transcribe_with_engine(&mut engine, &input),
            Err(ParakeetRuntimeError::InvalidTranscript(
                voxgolem_model::parakeet::TranscriptError::EmptyText,
            ))
        );
    }
}
