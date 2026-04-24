use std::collections::HashMap;
use std::path::Path;

use ndarray::Axis;
use ort::value::Tensor;

use crate::livekit_wakeword::embedding::EmbeddingModel;
use crate::livekit_wakeword::melspectrogram::MelspectrogramModel;
use crate::livekit_wakeword::{
    build_session_from_file, WakeWordError, EMBEDDING_STRIDE, EMBEDDING_WINDOW, MIN_EMBEDDINGS,
    SAMPLE_RATE,
};

pub struct WakeWordModel {
    mel_model: MelspectrogramModel,
    emb_model: EmbeddingModel,
    classifiers: HashMap<String, ort::session::Session>,
}

impl WakeWordModel {
    pub fn new(models: &[impl AsRef<Path>], sample_rate: u32) -> Result<Self, WakeWordError> {
        if sample_rate as usize != SAMPLE_RATE {
            return Err(WakeWordError::UnsupportedSampleRate(sample_rate));
        }

        let mut wakeword = Self {
            mel_model: MelspectrogramModel::new()?,
            emb_model: EmbeddingModel::new()?,
            classifiers: HashMap::new(),
        };

        for path in models {
            wakeword.load_model(path, None)?;
        }

        Ok(wakeword)
    }

    pub fn load_model(
        &mut self,
        model_path: impl AsRef<Path>,
        model_name: Option<&str>,
    ) -> Result<(), WakeWordError> {
        let path = model_path.as_ref();
        if !path.exists() {
            return Err(WakeWordError::ModelNotFound(path.display().to_string()));
        }

        let name = match model_name {
            Some(name) => name.to_string(),
            None => path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .unwrap_or("unknown")
                .to_string(),
        };

        let session = build_session_from_file(path)?;
        validate_classifier_contract(&session)?;
        self.classifiers.insert(name, session);
        Ok(())
    }

    pub fn predict(&mut self, audio_chunk: &[i16]) -> Result<HashMap<String, f32>, WakeWordError> {
        if self.classifiers.is_empty() {
            return Ok(HashMap::new());
        }

        let samples_f32: Vec<f32> = audio_chunk
            .iter()
            .map(|&sample| sample as f32 / 32_768.0)
            .collect();

        let mel = self.mel_model.detect(&samples_f32)?;
        let num_frames = mel.shape()[0];
        if num_frames < EMBEDDING_WINDOW {
            return Ok(self.zero_scores());
        }

        let mut embeddings = Vec::new();
        let mut start = 0;
        while start + EMBEDDING_WINDOW <= num_frames {
            let window = mel.slice_axis(
                Axis(0),
                ndarray::Slice::from(start..start + EMBEDDING_WINDOW),
            );
            let window = window.as_standard_layout();
            let embedding = self
                .emb_model
                .detect(window.as_slice().expect("contiguous mel window"))?;
            embeddings.push(embedding);
            start += EMBEDDING_STRIDE;
        }

        if embeddings.len() < MIN_EMBEDDINGS {
            return Ok(self.zero_scores());
        }

        let last = &embeddings[embeddings.len() - MIN_EMBEDDINGS..];
        let views: Vec<_> = last.iter().map(|embedding| embedding.view()).collect();
        let emb_sequence = ndarray::stack(Axis(0), &views)?;
        let emb_input = emb_sequence.insert_axis(Axis(0));

        let mut predictions = HashMap::new();
        for (name, session) in &mut self.classifiers {
            let tensor = Tensor::from_array(emb_input.clone())?;
            let outputs = session.run(ort::inputs!["embeddings" => tensor])?;
            let raw = outputs["score"].try_extract_array::<f32>()?;
            let score = raw.iter().copied().next().unwrap_or(0.0);
            predictions.insert(name.clone(), score);
        }

        Ok(predictions)
    }

    fn zero_scores(&self) -> HashMap<String, f32> {
        self.classifiers
            .keys()
            .map(|name| (name.clone(), 0.0))
            .collect()
    }
}

fn validate_classifier_contract(session: &ort::session::Session) -> Result<(), WakeWordError> {
    let input = session
        .inputs()
        .iter()
        .find(|input| input.name() == "embeddings")
        .ok_or_else(|| {
            WakeWordError::InvalidModelContract(
                "classifier model is missing expected `embeddings` input".to_string(),
            )
        })?;
    let input_shape = input.dtype().tensor_shape().ok_or_else(|| {
        WakeWordError::InvalidModelContract(
            "classifier `embeddings` input is not a tensor".to_string(),
        )
    })?;

    if input_shape.len() != 3
        || input_shape[1] != MIN_EMBEDDINGS as i64
        || input_shape[2] != crate::livekit_wakeword::EMBEDDING_DIM as i64
    {
        return Err(WakeWordError::InvalidModelContract(format!(
            "classifier `embeddings` input shape must be [batch, {MIN_EMBEDDINGS}, {}], got {:?}",
            crate::livekit_wakeword::EMBEDDING_DIM,
            input_shape,
        )));
    }

    let output = session
        .outputs()
        .iter()
        .find(|output| output.name() == "score")
        .ok_or_else(|| {
            WakeWordError::InvalidModelContract(
                "classifier model is missing expected `score` output".to_string(),
            )
        })?;
    let output_shape = output.dtype().tensor_shape().ok_or_else(|| {
        WakeWordError::InvalidModelContract("classifier `score` output is not a tensor".to_string())
    })?;

    if output_shape.len() != 2 || output_shape[1] != 1 {
        return Err(WakeWordError::InvalidModelContract(format!(
            "classifier `score` output shape must be [batch, 1], got {:?}",
            output_shape,
        )));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::WakeWordModel;
    use crate::livekit_wakeword::WakeWordError;
    use hound::WavReader;
    use std::path::{Path, PathBuf};

    fn fixtures_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
    }

    fn read_wav_i16(path: &Path) -> (u32, Vec<i16>) {
        let mut reader = WavReader::open(path)
            .unwrap_or_else(|error| panic!("open {}: {error}", path.display()));
        let spec = reader.spec();
        let all_samples: Vec<i16> = reader
            .samples::<i16>()
            .map(|sample| sample.unwrap())
            .collect();
        let samples = if spec.channels > 1 {
            all_samples
                .chunks(spec.channels as usize)
                .map(|chunk| chunk[0])
                .collect()
        } else {
            all_samples
        };

        (spec.sample_rate, samples)
    }

    #[test]
    fn load_model_rejects_invalid_classifier_contract() {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("src")
            .join("livekit_wakeword")
            .join("onnx")
            .join("melspectrogram.onnx");
        let error = WakeWordModel::new(&[&path], 16_000)
            .err()
            .expect("non-classifier file should fail");

        assert!(matches!(error, WakeWordError::InvalidModelContract(_)));
    }

    #[test]
    fn real_livekit_classifier_scores_positive_higher_than_negative() {
        let classifier_path = fixtures_dir().join("hey_livekit.onnx");
        let (positive_rate, positive_samples) = read_wav_i16(&fixtures_dir().join("positive.wav"));
        let (negative_rate, negative_samples) = read_wav_i16(&fixtures_dir().join("negative.wav"));
        let mut model = WakeWordModel::new(&[&classifier_path], positive_rate)
            .expect("official livekit classifier should load");

        assert_eq!(positive_rate, 16_000);
        assert_eq!(negative_rate, 16_000);

        let positive_score = model
            .predict(&positive_samples)
            .expect("positive inference should succeed")["hey_livekit"];
        let negative_score = model
            .predict(&negative_samples)
            .expect("negative inference should succeed")["hey_livekit"];

        assert!(
            positive_score >= 0.5,
            "positive score {positive_score} should meet threshold"
        );
        assert!(
            negative_score < 0.5,
            "negative score {negative_score} should stay below threshold"
        );
        assert!(
            positive_score > negative_score,
            "positive score {positive_score} should exceed negative score {negative_score}"
        );
    }
}
