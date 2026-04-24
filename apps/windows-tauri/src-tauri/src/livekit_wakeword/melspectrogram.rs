use ndarray::{Array1, Array2, Axis};
use ort::value::Tensor;

use crate::livekit_wakeword::{build_session_from_memory, WakeWordError};

const MODEL_BYTES: &[u8] = include_bytes!("onnx/melspectrogram.onnx");

pub struct MelspectrogramModel {
    session: ort::session::Session,
}

impl MelspectrogramModel {
    pub fn new() -> Result<Self, WakeWordError> {
        Ok(Self {
            session: build_session_from_memory(MODEL_BYTES)?,
        })
    }

    pub fn detect(&mut self, samples: &[f32]) -> Result<Array2<f32>, WakeWordError> {
        let audio = Array1::from_vec(samples.to_vec()).insert_axis(Axis(0));
        let tensor = Tensor::from_array(audio)?;
        let outputs = self.session.run(ort::inputs![tensor])?;
        let raw = outputs["output"].try_extract_array::<f32>()?;
        let rows = raw.shape()[2];
        let cols = raw.shape()[3];
        let mut output = raw.into_owned().into_shape_with_order((rows, cols))?;
        output.mapv_inplace(|value| value / 10.0 + 2.0);
        Ok(output)
    }
}
