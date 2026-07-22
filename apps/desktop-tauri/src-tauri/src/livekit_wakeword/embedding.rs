use ndarray::{Array, Array1};
use ort::value::Tensor;

use crate::livekit_wakeword::{build_session_from_memory, WakeWordError};

const MODEL_BYTES: &[u8] = include_bytes!("onnx/embedding_model.onnx");

pub struct EmbeddingModel {
    session: ort::session::Session,
}

impl EmbeddingModel {
    pub fn new() -> Result<Self, WakeWordError> {
        Ok(Self {
            session: build_session_from_memory(MODEL_BYTES)?,
        })
    }

    pub fn detect(&mut self, mel_features: &[f32]) -> Result<Array1<f32>, WakeWordError> {
        let input = Array::from_shape_vec((1, 76, 32, 1), mel_features.to_vec())?;
        let tensor = Tensor::from_array(input)?;
        let outputs = self.session.run(ort::inputs![tensor])?;
        let raw = outputs["conv2d_19"].try_extract_array::<f32>()?;
        let embedding = raw
            .into_owned()
            .into_shape_with_order(crate::livekit_wakeword::EMBEDDING_DIM)?;

        Ok(embedding)
    }
}
