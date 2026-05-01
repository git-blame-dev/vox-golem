use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;

#[derive(Debug, Clone)]
pub struct LocalTtsRuntimeSpec {
    pub enabled: bool,
    pub model_path: Option<PathBuf>,
    pub worker_count: usize,
    pub max_queue: usize,
    pub sample_rate_hz: u32,
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
    sender: Option<mpsc::SyncSender<SynthesisJob>>,
    _workers: Vec<thread::JoinHandle<()>>,
}

type SynthesisEngineFactory =
    Arc<dyn Fn(PathBuf, u32) -> Result<Box<dyn SynthesisEngine>, String> + Send + Sync>;

trait SynthesisEngine: Send {
    fn synthesize(&mut self, text: &str) -> Result<LocalTtsAudio, String>;
}

struct KokoroOnnxEngine {
    _session: ort::session::Session,
    sample_rate_hz: u32,
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

        response_rx
            .recv()
            .map_err(|_| String::from("failed to receive local tts synthesis result"))?
    }
}

impl KokoroOnnxEngine {
    fn new(model_path: &Path, sample_rate_hz: u32) -> Result<Self, String> {
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

        Ok(Self {
            _session: session,
            sample_rate_hz,
        })
    }
}

impl SynthesisEngine for KokoroOnnxEngine {
    fn synthesize(&mut self, text: &str) -> Result<LocalTtsAudio, String> {
        // Placeholder synthesis shape retained until full Kokoro tokenization/inference contract is wired.
        synthesize_placeholder_audio(text, self.sample_rate_hz)
    }
}

fn default_engine_factory(
    model_path: PathBuf,
    sample_rate_hz: u32,
) -> Result<Box<dyn SynthesisEngine>, String> {
    let engine = KokoroOnnxEngine::new(&model_path, sample_rate_hz)?;
    Ok(Box::new(engine))
}

fn synthesize_placeholder_audio(text: &str, sample_rate_hz: u32) -> Result<LocalTtsAudio, String> {
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
            super::synthesize_placeholder_audio(text, self.sample_rate_hz)
        }
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
}
