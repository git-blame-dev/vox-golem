use std::env;
use std::ffi::OsString;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;

use espeak_rs::text_to_phonemes;
use serde::Deserialize;
use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct LocalTtsRuntimeSpec {
    pub enabled: bool,
    pub model_path: Option<PathBuf>,
    pub worker_count: usize,
    pub max_queue: usize,
    pub sample_rate_hz: u32,
    pub max_duration_s: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LocalTtsAudio {
    pub pcm_f32: Vec<f32>,
    pub sample_rate_hz: u32,
    pub duration_ms: u64,
}

#[derive(Debug)]
pub struct LocalTtsRuntime {
    enabled: Arc<AtomicBool>,
    sample_rate_hz: u32,
    max_duration_ms: u64,
    sender: Option<mpsc::SyncSender<SynthesisJob>>,
    _workers: Vec<thread::JoinHandle<()>>,
}

type SynthesisEngineFactory =
    Arc<dyn Fn(PathBuf, u32) -> Result<Box<dyn SynthesisEngine>, String> + Send + Sync>;

trait SynthesisEngine: Send {
    fn synthesize(&mut self, text: &str) -> Result<LocalTtsAudio, String>;
}

struct PiperOnnxEngine {
    session: ort::session::Session,
    config: PiperVoiceConfig,
    contract: PiperModelContract,
    sample_rate_hz: u32,
}

#[derive(Debug, Clone, PartialEq)]
struct PiperModelContract {
    has_speaker_input: bool,
    output_name: String,
}

#[derive(Debug, Clone, Deserialize)]
struct PiperVoiceConfig {
    audio: PiperAudioConfig,
    espeak: PiperEspeakConfig,
    inference: PiperInferenceConfig,
    #[serde(default)]
    num_speakers: u32,
    #[serde(default)]
    speaker_id_map: HashMap<String, i64>,
    phoneme_id_map: HashMap<char, Vec<i64>>,
}

#[derive(Debug, Clone, Deserialize)]
struct PiperAudioConfig {
    sample_rate: u32,
}

#[derive(Debug, Clone, Deserialize)]
struct PiperEspeakConfig {
    voice: String,
}

#[derive(Debug, Clone, Deserialize)]
struct PiperInferenceConfig {
    noise_scale: f32,
    length_scale: f32,
    noise_w: f32,
}

#[derive(Debug)]
struct SynthesisJob {
    text: String,
    response: mpsc::Sender<Result<LocalTtsAudio, String>>,
}

impl LocalTtsRuntime {
    pub fn new(spec: LocalTtsRuntimeSpec) -> Result<Self, String> {
        let engine_factory: SynthesisEngineFactory = Arc::new(default_engine_factory);
        Self::new_with_engine_factory(spec, engine_factory)
    }

    fn new_with_engine_factory(
        spec: LocalTtsRuntimeSpec,
        engine_factory: SynthesisEngineFactory,
    ) -> Result<Self, String> {
        if spec.worker_count == 0 {
            return Err(String::from("tts worker_count must be greater than zero"));
        }

        if spec.max_queue == 0 {
            return Err(String::from("tts max_queue must be greater than zero"));
        }

        if spec.sample_rate_hz == 0 {
            return Err(String::from("tts sample_rate_hz must be greater than zero"));
        }

        if spec.max_duration_s == 0 {
            return Err(String::from("tts max_duration_s must be greater than zero"));
        }

        let max_duration_ms = spec.max_duration_s.saturating_mul(1_000);

        let model_path = if spec.enabled {
            let Some(model_path) = spec.model_path.as_ref() else {
                return Err(String::from("tts model_path is required when enabled"));
            };

            if !model_path.is_file() {
                return Err(format!(
                    "tts model_path must reference an existing file: {}",
                    model_path.display()
                ));
            }
            Some(model_path.to_path_buf())
        } else {
            None
        };

        if !spec.enabled {
            return Ok(Self {
                enabled: Arc::new(AtomicBool::new(false)),
                sample_rate_hz: spec.sample_rate_hz,
                max_duration_ms,
                sender: None,
                _workers: Vec::new(),
            });
        }

        let (sender, receiver) = mpsc::sync_channel::<SynthesisJob>(spec.max_queue);
        let shared_receiver = Arc::new(Mutex::new(receiver));
        let mut workers = Vec::with_capacity(spec.worker_count);
        let (init_tx, init_rx) = mpsc::channel::<Result<(), String>>();
        let model_path = model_path.expect("model path must exist for enabled runtime");

        for _ in 0..spec.worker_count {
            let worker_receiver = Arc::clone(&shared_receiver);
            let sample_rate_hz = spec.sample_rate_hz;
            let worker_model_path = model_path.clone();
            let worker_engine_factory = Arc::clone(&engine_factory);
            let worker_init_tx = init_tx.clone();
            workers.push(thread::spawn(move || {
                let mut engine = match worker_engine_factory(worker_model_path, sample_rate_hz) {
                    Ok(engine) => {
                        let _ = worker_init_tx.send(Ok(()));
                        engine
                    }
                    Err(error) => {
                        let _ = worker_init_tx.send(Err(error));
                        return;
                    }
                };

                loop {
                    let next_job = {
                        let guard = match worker_receiver.lock() {
                            Ok(guard) => guard,
                            Err(_) => return,
                        };
                        guard.recv()
                    };

                    let Ok(job) = next_job else {
                        return;
                    };

                    let result = engine.synthesize(&job.text);
                    let _ = job.response.send(result);
                }
            }));
        }

        drop(init_tx);
        for _ in 0..spec.worker_count {
            let init_result = init_rx
                .recv()
                .map_err(|_| String::from("failed to initialize local tts worker runtime"))?;
            init_result?;
        }

        Ok(Self {
            enabled: Arc::new(AtomicBool::new(true)),
            sample_rate_hz: spec.sample_rate_hz,
            max_duration_ms,
            sender: Some(sender),
            _workers: workers,
        })
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }

    pub fn sample_rate_hz(&self) -> u32 {
        self.sample_rate_hz
    }

    pub fn is_available(&self) -> bool {
        self.sender.is_some()
    }

    pub fn set_enabled(&self, enabled: bool) {
        self.enabled.store(enabled, Ordering::Relaxed);
    }

    pub fn synthesize(&self, text: &str) -> Result<LocalTtsAudio, String> {
        if !self.is_enabled() {
            return Err(String::from("tts is disabled"));
        }

        let text = text.trim();
        if text.is_empty() {
            return Err(String::from("tts text must not be empty"));
        }

        let Some(sender) = self.sender.as_ref() else {
            return Err(String::from("tts runtime is not available"));
        };

        let (response_tx, response_rx) = mpsc::channel();
        sender
            .send(SynthesisJob {
                text: text.to_string(),
                response: response_tx,
            })
            .map_err(|_| String::from("failed to enqueue local tts synthesis request"))?;

        let audio = response_rx
            .recv()
            .map_err(|_| String::from("failed to receive local tts synthesis result"))??;

        if audio.duration_ms > self.max_duration_ms {
            return Err(format!(
                "tts synthesized duration {}ms exceeds configured max {}ms",
                audio.duration_ms, self.max_duration_ms
            ));
        }

        Ok(audio)
    }
}

impl PiperOnnxEngine {
    fn new(model_path: &Path, sample_rate_hz: u32) -> Result<Self, String> {
        let config = load_piper_voice_config(model_path)?;
        if config.audio.sample_rate != sample_rate_hz {
            return Err(format!(
                "tts sample_rate_hz {} does not match Piper model config sample rate {}",
                sample_rate_hz, config.audio.sample_rate
            ));
        }

        ensure_windows_espeak_data_directory_env()?;

        ensure_windows_cuda_path_for_onnxruntime();

        let mut builder = ort::session::Session::builder()
            .map_err(|error| format!("failed to create local tts ONNX session builder: {error}"))?
            .with_execution_providers([ort::ep::CUDA::default().build().error_on_failure()])
            .map_err(|error| {
                format!("failed to initialize CUDA execution provider for local tts: {error}")
            })?
            .with_optimization_level(ort::session::builder::GraphOptimizationLevel::Level3)
            .map_err(|error| {
                format!("failed to configure local tts ONNX optimization level: {error}")
            })?
            .with_intra_threads(1)
            .map_err(|error| {
                format!("failed to configure local tts ONNX worker threads: {error}")
            })?;
        let session = builder.commit_from_file(model_path).map_err(|error| {
            format!(
                "failed to load local tts ONNX model from {}: {error}",
                model_path.display()
            )
        })?;

        let contract = resolve_piper_model_contract(&session, &config)?;

        Ok(Self {
            session,
            config,
            contract,
            sample_rate_hz,
        })
    }
}

fn piper_model_config_path(model_path: &Path) -> PathBuf {
    let mut config_path = model_path.as_os_str().to_os_string();
    config_path.push(".json");
    PathBuf::from(config_path)
}

fn load_piper_voice_config(model_path: &Path) -> Result<PiperVoiceConfig, String> {
    let config_path = piper_model_config_path(model_path);
    if !config_path.is_file() {
        return Err(format!(
            "tts model config must exist alongside model: {}",
            config_path.display()
        ));
    }

    let file = File::open(&config_path).map_err(|error| {
        format!(
            "failed to open local tts model config {}: {error}",
            config_path.display()
        )
    })?;
    let config = serde_json::from_reader::<_, PiperVoiceConfig>(file).map_err(|error| {
        format!(
            "failed to parse local tts model config {}: {error}",
            config_path.display()
        )
    })?;

    if config.audio.sample_rate == 0 {
        return Err(String::from(
            "local tts model config is invalid: audio.sample_rate must be greater than zero",
        ));
    }
    if config.espeak.voice.trim().is_empty() {
        return Err(String::from(
            "local tts model config is invalid: espeak.voice must not be empty",
        ));
    }
    if config.phoneme_id_map.is_empty() {
        return Err(String::from(
            "local tts model config is invalid: phoneme_id_map must not be empty",
        ));
    }

    for required in ['^', '$', '_'] {
        if config
            .phoneme_id_map
            .get(&required)
            .and_then(|values| values.first())
            .is_none()
        {
            return Err(format!(
                "local tts model config is invalid: phoneme_id_map must include `{required}`"
            ));
        }
    }

    if !config.inference.noise_scale.is_finite()
        || !config.inference.length_scale.is_finite()
        || !config.inference.noise_w.is_finite()
    {
        return Err(String::from(
            "local tts model config is invalid: inference values must be finite numbers",
        ));
    }

    Ok(config)
}

fn strict_windows_espeak_data_directory_from_appdata(
    appdata_dir: &Path,
) -> Result<PathBuf, String> {
    let tts_root = appdata_dir.join("VoxGolem").join("models").join("tts");
    if !tts_root.is_dir() {
        return Err(format!(
            "strict eSpeak directory is missing: {}",
            tts_root.display()
        ));
    }

    let espeak_data_dir = tts_root.join("espeak-ng-data");
    if !espeak_data_dir.is_dir() {
        return Err(format!(
            "strict eSpeak data directory is missing: {}",
            espeak_data_dir.display()
        ));
    }

    Ok(tts_root)
}

pub(crate) fn resolve_strict_windows_espeak_data_directory() -> Result<Option<PathBuf>, String> {
    #[cfg(not(windows))]
    {
        Ok(None)
    }

    #[cfg(windows)]
    {
        let appdata = env::var_os("APPDATA")
            .ok_or_else(|| String::from("APPDATA is not set; cannot resolve strict eSpeak path"))?;
        let strict_root =
            strict_windows_espeak_data_directory_from_appdata(&PathBuf::from(appdata))?;
        Ok(Some(strict_root))
    }
}

fn ensure_windows_espeak_data_directory_env() -> Result<(), String> {
    let Some(strict_root) = resolve_strict_windows_espeak_data_directory()? else {
        return Ok(());
    };
    env::set_var("PIPER_ESPEAKNG_DATA_DIRECTORY", &strict_root);
    Ok(())
}

fn ensure_windows_cuda_path_for_onnxruntime() {
    #[cfg(not(windows))]
    {
        return;
    }

    #[cfg(windows)]
    {
        let mut candidates = Vec::<PathBuf>::new();

        if let Some(cuda_path) = env::var_os("CUDA_PATH") {
            let bin = PathBuf::from(cuda_path).join("bin");
            if bin.is_dir() {
                candidates.push(bin);
            }
        }

        let standard_cuda_root =
            PathBuf::from(r"C:\Program Files\NVIDIA GPU Computing Toolkit\CUDA");
        if let Ok(entries) = standard_cuda_root.read_dir() {
            let mut versioned_cuda_dirs = entries
                .filter_map(Result::ok)
                .filter(|entry| entry.path().is_dir())
                .filter_map(|entry| {
                    let file_name = entry.file_name();
                    let value = file_name.to_string_lossy();
                    let version = value.strip_prefix('v')?;
                    let parsed = version
                        .split('.')
                        .map(|part| part.parse::<u32>().ok())
                        .collect::<Option<Vec<u32>>>()?;

                    Some((parsed, entry.path()))
                })
                .collect::<Vec<(Vec<u32>, PathBuf)>>();

            versioned_cuda_dirs
                .sort_by(|(left_version, _), (right_version, _)| right_version.cmp(left_version));

            for (_, dir) in versioned_cuda_dirs {
                let bin = dir.join("bin");
                if bin.is_dir() {
                    candidates.push(bin);
                }
            }
        }

        if candidates.is_empty() {
            return;
        }

        let mut combined = OsString::new();
        for candidate in &candidates {
            combined.push(candidate.as_os_str());
            combined.push(";");
        }

        let existing_path = env::var_os("PATH").unwrap_or_default();
        combined.push(existing_path);
        env::set_var("PATH", combined);
    }
}

impl SynthesisEngine for PiperOnnxEngine {
    fn synthesize(&mut self, text: &str) -> Result<LocalTtsAudio, String> {
        let token_ids = text_to_phoneme_ids(text, &self.config)?;
        let input_length = i64::try_from(token_ids.len()).map_err(|_| {
            String::from("local tts input is too long to represent in model input lengths")
        })?;

        let input_array = ndarray::Array2::from_shape_vec((1, token_ids.len()), token_ids)
            .map_err(|error| format!("failed to shape local tts token tensor: {error}"))?;
        let input_lengths_array = ndarray::Array1::from_vec(vec![input_length]);
        let scales_array = ndarray::Array1::from_vec(vec![
            self.config.inference.noise_scale,
            self.config.inference.length_scale,
            self.config.inference.noise_w,
        ]);

        let input_tensor = ort::value::TensorRef::from_array_view(&input_array)
            .map_err(|error| format!("failed to build local tts model input tensor: {error}"))?;
        let input_lengths_tensor = ort::value::TensorRef::from_array_view(&input_lengths_array)
            .map_err(|error| {
                format!("failed to build local tts model input lengths tensor: {error}")
            })?;
        let scales_tensor = ort::value::TensorRef::from_array_view(&scales_array)
            .map_err(|error| format!("failed to build local tts model scales tensor: {error}"))?;

        let outputs = if self.contract.has_speaker_input {
            let speaker_id = if self.config.num_speakers > 0 {
                self.config
                    .speaker_id_map
                    .values()
                    .next()
                    .copied()
                    .unwrap_or(0)
            } else {
                0
            };
            let speaker_array = ndarray::Array1::from_vec(vec![speaker_id]);
            let speaker_tensor = ort::value::TensorRef::from_array_view(&speaker_array)
                .map_err(|error| format!("failed to build local tts speaker tensor: {error}"))?;

            self.session
                .run(ort::inputs![
                    input_tensor,
                    input_lengths_tensor,
                    scales_tensor,
                    speaker_tensor
                ])
                .map_err(|error| format!("local tts ONNX inference failed: {error}"))?
        } else {
            self.session
                .run(ort::inputs![
                    input_tensor,
                    input_lengths_tensor,
                    scales_tensor
                ])
                .map_err(|error| format!("local tts ONNX inference failed: {error}"))?
        };

        decode_audio_from_outputs(
            outputs,
            self.contract.output_name.as_str(),
            self.sample_rate_hz,
        )
    }
}

fn decode_audio_from_outputs(
    outputs: ort::session::SessionOutputs<'_>,
    output_name: &str,
    sample_rate_hz: u32,
) -> Result<LocalTtsAudio, String> {
    let output = outputs.get(output_name).ok_or_else(|| {
        format!(
            "local tts model output `{}` was not returned by ONNX runtime",
            output_name
        )
    })?;
    let audio_tensor = output
        .try_extract_tensor::<f32>()
        .map_err(|error| format!("failed to extract local tts audio tensor: {error}"))?;
    let pcm_f32 = audio_tensor.1.to_vec();

    if pcm_f32.is_empty() {
        return Err(String::from(
            "local tts model produced an empty audio buffer",
        ));
    }

    let duration_ms =
        ((pcm_f32.len() as u64).saturating_mul(1_000) / u64::from(sample_rate_hz)).max(1);

    Ok(LocalTtsAudio {
        pcm_f32,
        sample_rate_hz,
        duration_ms,
    })
}

fn resolve_piper_model_contract(
    session: &ort::session::Session,
    config: &PiperVoiceConfig,
) -> Result<PiperModelContract, String> {
    let inputs = session.inputs();
    if inputs.len() != 3 && inputs.len() != 4 {
        return Err(format!(
            "local tts model contract is unsupported: expected 3 or 4 input tensors for Piper model, got {}",
            inputs.len()
        ));
    }

    for input in inputs.iter() {
        let shape = input.dtype().tensor_shape().ok_or_else(|| {
            format!(
                "local tts model contract is unsupported: input `{}` is not a tensor",
                input.name()
            )
        })?;
        if shape.is_empty() {
            return Err(format!(
                "local tts model contract is unsupported: input `{}` must not be scalar",
                input.name()
            ));
        }
    }

    let has_speaker_input = inputs.len() == 4;
    if config.num_speakers > 1 && !has_speaker_input {
        return Err(String::from(
            "local tts model contract is unsupported: multi-speaker config requires 4 model inputs",
        ));
    }
    if config.num_speakers <= 1 && has_speaker_input {
        return Err(String::from(
            "local tts model contract is unsupported: single-speaker config does not expect speaker input",
        ));
    }

    let output = session
        .outputs()
        .iter()
        .find(|output| {
            let shape = match output.dtype().tensor_shape() {
                Some(shape) => shape,
                None => return false,
            };

            matches!(output.name(), "audio" | "waveform" | "wav" | "output")
                || !shape.is_empty()
        })
        .ok_or_else(|| {
            String::from(
                "local tts model contract is unsupported: expected a non-scalar audio output tensor",
            )
        })?;

    Ok(PiperModelContract {
        has_speaker_input,
        output_name: output.name().to_string(),
    })
}

fn text_to_phoneme_ids(text: &str, config: &PiperVoiceConfig) -> Result<Vec<i64>, String> {
    let normalized = text
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .trim()
        .to_string();
    if normalized.is_empty() {
        return Err(String::from("tts text must not be empty"));
    }

    let phonemes = text_to_phonemes(&normalized, &config.espeak.voice, None, false, false)
        .map_err(|error| format!("failed to phonemize local tts input: {error}"))?
        .join(" ");
    if phonemes.trim().is_empty() {
        return Err(String::from("local tts phonemization produced no tokens"));
    }

    let pad_id = config
        .phoneme_id_map
        .get(&'_')
        .and_then(|values| values.first())
        .copied()
        .ok_or_else(|| {
            String::from("local tts model config is invalid: missing `_` phoneme mapping")
        })?;
    let bos_id = config
        .phoneme_id_map
        .get(&'^')
        .and_then(|values| values.first())
        .copied()
        .ok_or_else(|| {
            String::from("local tts model config is invalid: missing `^` phoneme mapping")
        })?;
    let eos_id = config
        .phoneme_id_map
        .get(&'$')
        .and_then(|values| values.first())
        .copied()
        .ok_or_else(|| {
            String::from("local tts model config is invalid: missing `$` phoneme mapping")
        })?;

    let mut token_ids = Vec::<i64>::with_capacity((phonemes.chars().count() + 1).saturating_mul(2));
    token_ids.push(bos_id);
    for phoneme in phonemes.chars() {
        if let Some(id) = config
            .phoneme_id_map
            .get(&phoneme)
            .and_then(|values| values.first())
        {
            token_ids.push(*id);
            token_ids.push(pad_id);
        }
    }
    token_ids.push(eos_id);

    if token_ids.len() <= 2 {
        return Err(String::from(
            "local tts phoneme map produced no model token ids",
        ));
    }

    Ok(token_ids)
}

fn default_engine_factory(
    model_path: PathBuf,
    sample_rate_hz: u32,
) -> Result<Box<dyn SynthesisEngine>, String> {
    let engine = PiperOnnxEngine::new(&model_path, sample_rate_hz)?;
    Ok(Box::new(engine))
}

fn synthesize_fake_audio(text: &str, sample_rate_hz: u32) -> Result<LocalTtsAudio, String> {
    let char_count = text.chars().count().max(1) as u64;
    let duration_ms = (char_count.saturating_mul(35)).clamp(220, 8_000);
    let sample_count = ((sample_rate_hz as u64).saturating_mul(duration_ms) / 1_000) as usize;
    if sample_count == 0 {
        return Err(String::from("local tts generated an empty sample buffer"));
    }

    let mut pcm_f32 = Vec::with_capacity(sample_count);
    let amplitude = 0.12_f32;
    let frequency_hz = 170.0_f32;
    let two_pi = std::f32::consts::PI * 2.0;
    let step = two_pi * frequency_hz / sample_rate_hz as f32;
    for index in 0..sample_count {
        let envelope = if index < sample_count / 10 {
            index as f32 / (sample_count / 10).max(1) as f32
        } else {
            1.0
        };
        pcm_f32.push((step * index as f32).sin() * amplitude * envelope);
    }

    Ok(LocalTtsAudio {
        pcm_f32,
        sample_rate_hz,
        duration_ms,
    })
}

#[cfg(test)]
mod tests {
    use super::{LocalTtsRuntime, LocalTtsRuntimeSpec};
    use std::fs;
    use std::path::PathBuf;
    use std::sync::Arc;

    struct FakeSynthesisEngine {
        sample_rate_hz: u32,
    }

    impl super::SynthesisEngine for FakeSynthesisEngine {
        fn synthesize(&mut self, text: &str) -> Result<super::LocalTtsAudio, String> {
            super::synthesize_fake_audio(text, self.sample_rate_hz)
        }
    }

    #[test]
    fn piper_model_config_path_uses_onnx_json_companion() {
        let path = PathBuf::from(r"models/tts/jarvis-high.onnx");
        let config_path = super::piper_model_config_path(&path);
        assert_eq!(
            config_path.to_string_lossy().replace('\\', "/"),
            "models/tts/jarvis-high.onnx.json"
        );
    }

    #[test]
    fn piper_config_loader_rejects_missing_companion_file() {
        let temp_dir = tempfile::tempdir().expect("temp dir should be created");
        let model_path = temp_dir.path().join("jarvis.onnx");
        fs::write(&model_path, b"fixture").expect("model fixture should be written");

        let error = super::load_piper_voice_config(&model_path)
            .expect_err("missing .onnx.json companion must fail");
        assert!(error.contains("tts model config must exist alongside model"));
    }

    #[test]
    fn strict_espeak_path_requires_tts_root_directory() {
        let temp_dir = tempfile::tempdir().expect("temp dir should be created");

        let error = super::strict_windows_espeak_data_directory_from_appdata(temp_dir.path())
            .expect_err("missing strict tts root should fail");

        assert!(error.contains("strict eSpeak directory is missing"));
    }

    #[test]
    fn strict_espeak_path_requires_espeak_ng_data_directory() {
        let temp_dir = tempfile::tempdir().expect("temp dir should be created");
        let tts_root = temp_dir.path().join("VoxGolem").join("models").join("tts");
        fs::create_dir_all(&tts_root).expect("strict tts root should be created");

        let error = super::strict_windows_espeak_data_directory_from_appdata(temp_dir.path())
            .expect_err("missing espeak-ng-data should fail");

        assert!(error.contains("strict eSpeak data directory is missing"));
    }

    #[test]
    fn strict_espeak_path_accepts_expected_directory_layout() {
        let temp_dir = tempfile::tempdir().expect("temp dir should be created");
        let tts_root = temp_dir.path().join("VoxGolem").join("models").join("tts");
        let espeak_data_dir = tts_root.join("espeak-ng-data");
        fs::create_dir_all(&espeak_data_dir).expect("espeak-ng-data directory should be created");

        let resolved = super::strict_windows_espeak_data_directory_from_appdata(temp_dir.path())
            .expect("expected strict path resolution to succeed");

        assert_eq!(resolved, tts_root);
    }

    fn fake_engine_factory(
        _model_path: PathBuf,
        sample_rate_hz: u32,
    ) -> Result<Box<dyn super::SynthesisEngine>, String> {
        Ok(Box::new(FakeSynthesisEngine { sample_rate_hz }))
    }

    #[test]
    fn disabled_runtime_rejects_synthesis_requests() {
        let runtime = LocalTtsRuntime::new(LocalTtsRuntimeSpec {
            enabled: false,
            model_path: None,
            worker_count: 1,
            max_queue: 4,
            sample_rate_hz: 22_050,
            max_duration_s: 300,
        })
        .expect("runtime should initialize");

        let error = runtime
            .synthesize("hello")
            .expect_err("disabled runtime should reject synthesis");
        assert_eq!(error, "tts is disabled");
    }

    #[test]
    fn enabled_runtime_generates_audio() {
        let temp_dir = tempfile::tempdir().expect("temp dir should be created");
        let model_path = temp_dir.path().join("jarvis.onnx");
        fs::write(&model_path, b"fixture").expect("model fixture should be written");

        let runtime = LocalTtsRuntime::new_with_engine_factory(
            LocalTtsRuntimeSpec {
                enabled: true,
                model_path: Some(model_path),
                worker_count: 2,
                max_queue: 8,
                sample_rate_hz: 22_050,
                max_duration_s: 300,
            },
            Arc::new(fake_engine_factory),
        )
        .expect("runtime should initialize with fake engine");

        let audio = runtime
            .synthesize("voice check")
            .expect("synthesis should succeed");

        assert_eq!(audio.sample_rate_hz, 22_050);
        assert!(!audio.pcm_f32.is_empty());
    }

    #[test]
    fn enabled_runtime_surfaces_cuda_provider_init_failure() {
        let temp_dir = tempfile::tempdir().expect("temp dir should be created");
        let model_path = temp_dir.path().join("jarvis.onnx");
        fs::write(&model_path, b"fixture").expect("model fixture should be written");

        let runtime_result = LocalTtsRuntime::new_with_engine_factory(
            LocalTtsRuntimeSpec {
                enabled: true,
                model_path: Some(model_path),
                worker_count: 1,
                max_queue: 4,
                sample_rate_hz: 22_050,
                max_duration_s: 300,
            },
            Arc::new(|_, _| {
                Err(String::from(
                    "failed to initialize CUDA execution provider for local tts: unavailable",
                ))
            }),
        );

        let error = runtime_result.expect_err("runtime should fail when CUDA provider init fails");
        assert!(error.contains("failed to initialize CUDA execution provider for local tts"));
    }

    #[test]
    fn enabled_runtime_rejects_audio_exceeding_max_duration() {
        let temp_dir = tempfile::tempdir().expect("temp dir should be created");
        let model_path = temp_dir.path().join("jarvis.onnx");
        fs::write(&model_path, b"fixture").expect("model fixture should be written");

        let runtime = LocalTtsRuntime::new_with_engine_factory(
            LocalTtsRuntimeSpec {
                enabled: true,
                model_path: Some(model_path),
                worker_count: 1,
                max_queue: 4,
                sample_rate_hz: 22_050,
                max_duration_s: 1,
            },
            Arc::new(fake_engine_factory),
        )
        .expect("runtime should initialize with fake engine");

        let long_text = "This sentence is intentionally long enough to exceed one second.";
        let error = runtime
            .synthesize(long_text)
            .expect_err("over-limit synthesis should hard-fail");

        assert!(error.contains("exceeds configured max"));
    }
}
