use std::env;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex, OnceLock};
use std::thread;
use std::time::Duration;

use espeak_rs::text_to_phonemes;
use serde::Deserialize;
use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct LocalTtsRuntimeSpec {
    pub model_path: Option<PathBuf>,
    pub worker_count: usize,
    pub max_queue: usize,
    pub sample_rate_hz: u32,
    pub max_duration_s: u64,
    pub provider_policy: TtsProviderPolicy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TtsProviderPolicy {
    Auto,
    Cuda,
    Cpu,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TtsActualProvider {
    Cuda,
    Cpu,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LocalTtsAudio {
    pub pcm_f32: Vec<f32>,
    pub sample_rate_hz: u32,
    pub duration_ms: u64,
}

#[derive(Debug)]
pub struct LocalTtsRuntime {
    generation: Arc<AtomicU64>,
    sample_rate_hz: u32,
    max_duration_ms: u64,
    sender: Option<mpsc::SyncSender<SynthesisJob>>,
    _workers: Vec<thread::JoinHandle<()>>,
    actual_provider: Option<TtsActualProvider>,
    shutdown: Arc<AtomicBool>,
}

const MAX_TTS_INPUT_BYTES: usize = 4096;
const MAX_SYNTHESIS_WAIT_MS: u64 = 10 * 60 * 1_000;

type SynthesisEngineFactory = Arc<
    dyn Fn(
            PathBuf,
            u32,
            TtsProviderPolicy,
        ) -> Result<(Box<dyn SynthesisEngine>, TtsActualProvider), String>
        + Send
        + Sync,
>;

type AbandonedSynthesis = (
    thread::JoinHandle<()>,
    mpsc::Receiver<Result<(Box<dyn SynthesisEngine>, LocalTtsAudio), String>>,
);

static PENDING_INITIALIZATIONS: OnceLock<Mutex<Vec<thread::JoinHandle<()>>>> = OnceLock::new();
static ABANDONED_SYNTHESIS: OnceLock<Mutex<Vec<thread::JoinHandle<()>>>> = OnceLock::new();
static SYNTHESIS_PERMIT_OCCUPIED: AtomicBool = AtomicBool::new(false);

struct SynthesisPermit;

impl SynthesisPermit {
    fn acquire() -> Option<Self> {
        SYNTHESIS_PERMIT_OCCUPIED
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .ok()
            .map(|_| Self)
    }
}

impl Drop for SynthesisPermit {
    fn drop(&mut self) {
        SYNTHESIS_PERMIT_OCCUPIED.store(false, Ordering::Release);
    }
}

fn reap_abandoned_synthesis() -> bool {
    let pending = ABANDONED_SYNTHESIS.get_or_init(|| Mutex::new(Vec::new()));
    let Ok(mut pending) = pending.lock() else {
        return true;
    };
    let mut active = Vec::with_capacity(pending.len());
    for runner in pending.drain(..) {
        if runner.is_finished() {
            let _ = runner.join();
        } else {
            active.push(runner);
        }
    }
    let occupied = !active.is_empty();
    *pending = active;
    occupied
}

fn retain_abandoned_synthesis(runner: thread::JoinHandle<()>) {
    let pending = ABANDONED_SYNTHESIS.get_or_init(|| Mutex::new(Vec::new()));
    if let Ok(mut pending) = pending.lock() {
        pending.push(runner);
    }
}

fn reap_pending_initializations() -> bool {
    let pending = PENDING_INITIALIZATIONS.get_or_init(|| Mutex::new(Vec::new()));
    let Ok(mut pending) = pending.lock() else {
        return false;
    };
    let mut active = Vec::with_capacity(pending.len());
    for worker in pending.drain(..) {
        if worker.is_finished() {
            let _ = worker.join();
        } else {
            active.push(worker);
        }
    }
    let has_active = !active.is_empty();
    *pending = active;
    has_active
}

fn retain_pending_initializations(workers: Vec<thread::JoinHandle<()>>) {
    let pending = PENDING_INITIALIZATIONS.get_or_init(|| Mutex::new(Vec::new()));
    if let Ok(mut pending) = pending.lock() {
        pending.extend(workers);
    }
}

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
    cancelled: Arc<AtomicBool>,
    generation: u64,
}

const SYNTHESIS_CANCELLED: &str = "local tts synthesis request was cancelled or superseded";

impl LocalTtsRuntime {
    pub fn new(spec: LocalTtsRuntimeSpec) -> Result<Self, String> {
        let engine_factory: SynthesisEngineFactory = Arc::new(default_engine_factory);
        Self::new_with_engine_factory(spec, engine_factory)
    }

    fn new_with_engine_factory(
        spec: LocalTtsRuntimeSpec,
        engine_factory: SynthesisEngineFactory,
    ) -> Result<Self, String> {
        if reap_pending_initializations() {
            return Err(String::from(
                "local tts initialization is still pending from a previous attempt",
            ));
        }
        if reap_abandoned_synthesis() {
            return Err(String::from(
                "local tts abandoned synthesis budget is still occupied",
            ));
        }
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

        let model_path = spec
            .model_path
            .as_ref()
            .ok_or_else(|| String::from("tts model_path is required"))?;
        if !model_path.is_file() {
            return Err(format!(
                "tts model_path must reference an existing file: {}",
                model_path.display()
            ));
        }
        ensure_espeak_data_directory_env(model_path)?;

        let (sender, receiver) = mpsc::sync_channel::<SynthesisJob>(spec.max_queue);
        let generation = Arc::new(AtomicU64::new(0));
        let shutdown = Arc::new(AtomicBool::new(false));
        let shared_receiver = Arc::new(Mutex::new(receiver));
        let mut workers = Vec::with_capacity(spec.worker_count);
        let (init_tx, init_rx) = mpsc::channel::<Result<TtsActualProvider, String>>();
        let provider_policy = spec.provider_policy;
        for _ in 0..spec.worker_count {
            let worker_receiver = Arc::clone(&shared_receiver);
            let sample_rate_hz = spec.sample_rate_hz;
            let worker_model_path = model_path.clone();
            let worker_engine_factory = Arc::clone(&engine_factory);
            let worker_generation = Arc::clone(&generation);
            let worker_init_tx = init_tx.clone();
            let worker_shutdown = Arc::clone(&shutdown);
            workers.push(thread::spawn(move || {
                match worker_engine_factory(
                    worker_model_path.clone(),
                    sample_rate_hz,
                    provider_policy,
                ) {
                    Ok((engine, provider)) => {
                        let _ = worker_init_tx.send(Ok(provider));

                        // The engine is retained for ordinary sequential requests. A cancelled
                        // native call may take ownership of it until the runner finishes.
                        let mut available_engine = Some(engine);

                        // A native inference call cannot be forcibly stopped portably. Keep at
                        // most one abandoned invocation per worker while its replacement runs.
                        let mut abandoned: Option<AbandonedSynthesis> = None;

                        loop {
                            if worker_shutdown.load(Ordering::Acquire) {
                                if let Some((runner, _)) = abandoned.take() {
                                    retain_abandoned_synthesis(runner);
                                }
                                return;
                            }
                            let next_job = {
                                let guard = match worker_receiver.lock() {
                                    Ok(guard) => guard,
                                    Err(_) => return,
                                };
                                guard.recv()
                            };

                            let Ok(job) = next_job else {
                                if let Some((runner, _)) = abandoned.take() {
                                    retain_abandoned_synthesis(runner);
                                }
                                return;
                            };

                            // Completed abandoned work no longer consumes the budget. Reclaim
                            // its engine before deciding whether another runner may start.
                            if let Some((runner, result_rx)) = abandoned.take() {
                                match result_rx.try_recv() {
                                    Ok(Ok((engine, _))) => {
                                        available_engine = Some(engine);
                                        let _ = runner.join();
                                    }
                                    Ok(Err(_)) | Err(mpsc::TryRecvError::Disconnected) => {
                                        let _ = runner.join();
                                    }
                                    Err(mpsc::TryRecvError::Empty) => {
                                        abandoned = Some((runner, result_rx));
                                    }
                                }
                            }
                            if abandoned.is_some() {
                                let _ = job.response.send(Err(String::from(
                                    "local tts abandoned synthesis budget exhausted",
                                )));
                                continue;
                            }
                            if job.cancelled.load(Ordering::Relaxed)
                                || job.generation != worker_generation.load(Ordering::Acquire)
                            {
                                let _ = job.response.send(Err(String::from(SYNTHESIS_CANCELLED)));
                                continue;
                            }

                            let engine = if let Some(engine) = available_engine.take() {
                                engine
                            } else {
                                match worker_engine_factory(
                                    worker_model_path.clone(),
                                    sample_rate_hz,
                                    provider_policy,
                                ) {
                                    Ok((engine, _)) => engine,
                                    Err(error) => {
                                        let _ = job.response.send(Err(error));
                                        continue;
                                    }
                                }
                            };
                            let Some(permit) = SynthesisPermit::acquire() else {
                                available_engine = Some(engine);
                                let _ = job.response.send(Err(String::from(
                                    "local tts abandoned synthesis budget exhausted",
                                )));
                                continue;
                            };
                            let (result_tx, result_rx) = mpsc::channel();
                            let child_text = job.text.clone();
                            let child = thread::spawn(move || {
                                let _permit = permit;
                                let mut engine = engine;
                                let result =
                                    engine.synthesize(&child_text).map(|audio| (engine, audio));
                                let _ = result_tx.send(result);
                            });

                            let mut cancelled = false;
                            let result = loop {
                                match result_rx.recv_timeout(Duration::from_millis(10)) {
                                    Ok(result) => break result,
                                    Err(mpsc::RecvTimeoutError::Timeout) => {
                                        if job.cancelled.load(Ordering::Relaxed)
                                            || job.generation
                                                != worker_generation.load(Ordering::Acquire)
                                        {
                                            cancelled = true;
                                            break Err(String::from(SYNTHESIS_CANCELLED));
                                        }
                                    }
                                    Err(mpsc::RecvTimeoutError::Disconnected) => {
                                        break Err(String::from(
                                            "local tts synthesis worker exited unexpectedly",
                                        ))
                                    }
                                }
                            };

                            if cancelled {
                                let _ = job.response.send(Err(result
                                    .err()
                                    .unwrap_or_else(|| String::from(SYNTHESIS_CANCELLED))));
                                abandoned = Some((child, result_rx));
                            } else {
                                let _ = child.join();
                                match result {
                                    Ok((engine, audio)) => {
                                        available_engine = Some(engine);
                                        let result = if job.cancelled.load(Ordering::Relaxed)
                                            || job.generation
                                                != worker_generation.load(Ordering::Acquire)
                                        {
                                            Err(String::from(SYNTHESIS_CANCELLED))
                                        } else {
                                            Ok(audio)
                                        };
                                        let _ = job.response.send(result);
                                    }
                                    Err(error) => {
                                        let _ = job.response.send(Err(error));
                                    }
                                }
                            }
                        }
                    }
                    Err(error) => {
                        let _ = worker_init_tx.send(Err(error));
                    }
                }
            }));
        }

        drop(init_tx);
        let mut actual_provider = None;
        for _ in 0..spec.worker_count {
            let init_result = init_rx
                .recv_timeout(Duration::from_secs(2))
                .map_err(|error| match error {
                    mpsc::RecvTimeoutError::Timeout => {
                        String::from("timed out initializing local tts worker runtime")
                    }
                    mpsc::RecvTimeoutError::Disconnected => {
                        String::from("failed to initialize local tts worker runtime")
                    }
                });
            let provider = match init_result {
                Ok(result) => match result {
                    Ok(provider) => provider,
                    Err(error) => {
                        shutdown.store(true, Ordering::Release);
                        drop(sender);
                        retain_pending_initializations(workers);
                        return Err(error);
                    }
                },
                Err(error) => {
                    shutdown.store(true, Ordering::Release);
                    drop(sender);
                    retain_pending_initializations(workers);
                    return Err(error);
                }
            };
            if actual_provider.is_some_and(|selected| selected != provider) {
                shutdown.store(true, Ordering::Release);
                drop(sender);
                retain_pending_initializations(workers);
                return Err(String::from(
                    "local tts workers selected inconsistent execution providers",
                ));
            }
            actual_provider = Some(provider);
        }

        Ok(Self {
            generation,
            sample_rate_hz: spec.sample_rate_hz,
            max_duration_ms,
            sender: Some(sender),
            _workers: workers,
            actual_provider,
            shutdown,
        })
    }

    pub fn sample_rate_hz(&self) -> u32 {
        self.sample_rate_hz
    }

    pub fn actual_provider(&self) -> Option<TtsActualProvider> {
        self.actual_provider
    }

    pub fn start_generation(&self) -> u64 {
        self.generation
            .fetch_add(1, Ordering::AcqRel)
            .saturating_add(1)
    }

    pub fn cancel_generation(&self) {
        self.generation.fetch_add(1, Ordering::AcqRel);
    }

    pub fn shutdown_bounded(&mut self) {
        self.shutdown.store(true, Ordering::Release);
        self.sender.take();
        let mut running_workers = Vec::new();
        for worker in self._workers.drain(..) {
            if worker.is_finished() {
                // Joining only completed workers keeps shutdown bounded even when native
                // inference cannot be interrupted.
                let _ = worker.join();
            } else {
                running_workers.push(worker);
            }
        }
        self._workers = running_workers;
    }

    pub fn shutdown_owned(&mut self) {
        self.shutdown_bounded();
    }

    pub fn synthesize_for_generation(
        &self,
        text: &str,
        generation: u64,
    ) -> Result<LocalTtsAudio, String> {
        self.synthesize_inner(text, generation)
    }

    pub fn synthesize(&self, text: &str) -> Result<LocalTtsAudio, String> {
        self.synthesize_inner(text, self.generation.load(Ordering::Acquire))
    }

    fn synthesize_inner(&self, text: &str, generation: u64) -> Result<LocalTtsAudio, String> {
        let text = text.trim();
        if text.is_empty() {
            return Err(String::from("tts text must not be empty"));
        }
        if text.len() > MAX_TTS_INPUT_BYTES {
            return Err(format!(
                "tts text exceeds maximum of {MAX_TTS_INPUT_BYTES} bytes"
            ));
        }

        let Some(sender) = self.sender.as_ref() else {
            return Err(String::from("tts runtime is not available"));
        };

        let (response_tx, response_rx) = mpsc::channel();
        let cancelled = Arc::new(AtomicBool::new(false));
        sender
            .try_send(SynthesisJob {
                text: text.to_string(),
                response: response_tx,
                cancelled: Arc::clone(&cancelled),
                generation,
            })
            .map_err(|error| match error {
                mpsc::TrySendError::Full(_) => String::from("local tts synthesis queue is full"),
                mpsc::TrySendError::Disconnected(_) => {
                    String::from("failed to enqueue local tts synthesis request")
                }
            })?;

        let wait_ms = self
            .max_duration_ms
            .saturating_add(30_000)
            .min(MAX_SYNTHESIS_WAIT_MS);
        let audio = match response_rx.recv_timeout(Duration::from_millis(wait_ms)) {
            Ok(result) => result?,
            Err(_) => {
                cancelled.store(true, Ordering::Relaxed);
                return Err(String::from("local tts synthesis timed out"));
            }
        };

        if audio.duration_ms > self.max_duration_ms {
            return Err(format!(
                "tts synthesized duration {}ms exceeds configured max {}ms",
                audio.duration_ms, self.max_duration_ms
            ));
        }

        if generation != self.generation.load(Ordering::Acquire) {
            return Err(String::from(SYNTHESIS_CANCELLED));
        }

        Ok(audio)
    }
}

impl Drop for LocalTtsRuntime {
    fn drop(&mut self) {
        self.shutdown_bounded();
    }
}

impl PiperOnnxEngine {
    fn new(
        model_path: &Path,
        sample_rate_hz: u32,
        policy: TtsProviderPolicy,
    ) -> Result<(Self, TtsActualProvider), String> {
        let config = load_piper_voice_config(model_path)?;
        if config.audio.sample_rate != sample_rate_hz {
            return Err(format!(
                "tts sample_rate_hz {} does not match Piper model config sample rate {}",
                sample_rate_hz, config.audio.sample_rate
            ));
        }

        let try_provider = |cuda: bool| -> Result<ort::session::Session, String> {
            if cuda {
                ensure_windows_tts_cuda_runtime_dlls_beside_current_exe()?;
            }
            let mut providers = if cuda {
                ort::session::Session::builder()
                    .map_err(|e| format!("failed to create local tts ONNX session builder: {e}"))?
                    .with_execution_providers([ort::ep::CUDA::default().build().error_on_failure()])
            } else {
                ort::session::Session::builder()
                    .map_err(|e| format!("failed to create local tts ONNX session builder: {e}"))?
                    .with_execution_providers([ort::ep::CPU::default().build()])
            }
            .map_err(|e| format!("failed to initialize local tts execution provider: {e}"))?;
            providers.commit_from_file(model_path).map_err(|e| {
                format!(
                    "failed to load local tts ONNX model from {}: {e}",
                    model_path.display()
                )
            })
        };
        let (session, actual_provider) = select_tts_provider(policy, try_provider)?;
        let contract = resolve_piper_model_contract(&session, &config)?;

        Ok((
            Self {
                session,
                config,
                contract,
                sample_rate_hz,
            },
            actual_provider,
        ))
    }
}

fn select_tts_provider<T>(
    policy: TtsProviderPolicy,
    mut initialize: impl FnMut(bool) -> Result<T, String>,
) -> Result<(T, TtsActualProvider), String> {
    match policy {
        TtsProviderPolicy::Cpu => Ok((initialize(false)?, TtsActualProvider::Cpu)),
        TtsProviderPolicy::Cuda => Ok((initialize(true)?, TtsActualProvider::Cuda)),
        TtsProviderPolicy::Auto => match initialize(true) {
            Ok(runtime) => Ok((runtime, TtsActualProvider::Cuda)),
            Err(_) => Ok((initialize(false)?, TtsActualProvider::Cpu)),
        },
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

fn adjacent_espeak_data_root(model_path: &Path) -> Option<PathBuf> {
    let root = model_path.parent()?.to_path_buf();
    root.join("espeak-ng-data").is_dir().then_some(root)
}

fn ensure_espeak_data_directory_env(model_path: &Path) -> Result<(), String> {
    #[cfg(not(windows))]
    {
        if env::var_os("PIPER_ESPEAKNG_DATA_DIRECTORY").is_none() {
            if let Some(root) = adjacent_espeak_data_root(model_path) {
                env::set_var("PIPER_ESPEAKNG_DATA_DIRECTORY", root);
            }
        }
        Ok(())
    }

    #[cfg(windows)]
    {
        let Some(strict_root) = resolve_strict_windows_espeak_data_directory()? else {
            return Ok(());
        };
        env::set_var("PIPER_ESPEAKNG_DATA_DIRECTORY", &strict_root);
        Ok(())
    }
}

const TTS_CUDA_RUNTIME_DLLS: &[&str] = &[
    "cublasLt64_12.dll",
    "cublas64_12.dll",
    "cufft64_11.dll",
    "cudart64_12.dll",
    "cudnn64_9.dll",
    "cudnn_adv64_9.dll",
    "cudnn_cnn64_9.dll",
    "cudnn_engines_precompiled64_9.dll",
    "cudnn_engines_runtime_compiled64_9.dll",
    "cudnn_graph64_9.dll",
    "cudnn_heuristic64_9.dll",
    "cudnn_ops64_9.dll",
    "onnxruntime_providers_cuda.dll",
    "onnxruntime_providers_shared.dll",
];

fn verify_tts_cuda_runtime_dlls_in_dir(executable_dir: &Path) -> Result<(), String> {
    let missing_dlls = TTS_CUDA_RUNTIME_DLLS
        .iter()
        .copied()
        .filter(|dll_name| !executable_dir.join(dll_name).is_file())
        .collect::<Vec<_>>();

    if missing_dlls.is_empty() {
        return Ok(());
    }

    Err(format!(
    "local tts CUDA runtime DLLs are missing beside the current executable in {}: {}. Copy the missing DLLs into that directory before starting VoxGolem.",
    executable_dir.display(),
    missing_dlls.join(", ")
))
}

fn ensure_windows_tts_cuda_runtime_dlls_beside_current_exe() -> Result<(), String> {
    #[cfg(not(windows))]
    {
        Ok(())
    }

    #[cfg(windows)]
    {
        let executable_path = env::current_exe().map_err(|error| {
            format!("failed to resolve current executable for local tts CUDA DLL check: {error}")
        })?;
        let executable_dir = executable_path.parent().ok_or_else(|| {
            format!(
                "failed to resolve current executable directory for local tts CUDA DLL check: {}",
                executable_path.display()
            )
        })?;

        verify_tts_cuda_runtime_dlls_in_dir(executable_dir)
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
    let mut pcm_f32 = audio_tensor.1.to_vec();

    if pcm_f32.is_empty() {
        return Err(String::from(
            "local tts model produced an empty audio buffer",
        ));
    }
    normalize_pcm_samples(&mut pcm_f32)?;

    let duration_ms =
        ((pcm_f32.len() as u64).saturating_mul(1_000) / u64::from(sample_rate_hz)).max(1);

    Ok(LocalTtsAudio {
        pcm_f32,
        sample_rate_hz,
        duration_ms,
    })
}

fn normalize_pcm_samples(samples: &mut [f32]) -> Result<(), String> {
    if samples.iter().any(|sample| !sample.is_finite()) {
        return Err(String::from(
            "local tts model produced non-finite audio samples",
        ));
    }
    for sample in samples {
        *sample = sample.clamp(-1.0, 1.0);
    }
    Ok(())
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

    let output_shapes = session
        .outputs()
        .iter()
        .map(|output| {
            (
                output.name(),
                output
                    .dtype()
                    .tensor_shape()
                    .is_some_and(|shape| !shape.is_empty()),
            )
        })
        .collect::<Vec<_>>();
    let output_name = select_piper_audio_output(&output_shapes)?;

    Ok(PiperModelContract {
        has_speaker_input,
        output_name: output_name.to_string(),
    })
}

fn select_piper_audio_output<'a>(outputs: &[(&'a str, bool)]) -> Result<&'a str, String> {
    const RECOGNIZED_AUDIO_NAMES: &[&str] = &["audio", "waveform", "wav", "output"];

    if let Some((name, _)) = outputs
        .iter()
        .find(|(name, non_scalar)| *non_scalar && RECOGNIZED_AUDIO_NAMES.contains(name))
    {
        return Ok(name);
    }

    let mut non_scalar_outputs = outputs.iter().filter(|(_, non_scalar)| *non_scalar);
    let Some((name, _)) = non_scalar_outputs.next() else {
        return Err(String::from(
            "local tts model contract is unsupported: expected a non-scalar audio output tensor",
        ));
    };
    if non_scalar_outputs.next().is_some() {
        return Err(String::from(
            "local tts model contract is unsupported: ambiguous unnamed non-scalar audio outputs",
        ));
    }

    Ok(name)
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
    policy: TtsProviderPolicy,
) -> Result<(Box<dyn SynthesisEngine>, TtsActualProvider), String> {
    let (engine, provider) = PiperOnnxEngine::new(&model_path, sample_rate_hz, policy)?;
    Ok((Box::new(engine), provider))
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
    use std::sync::{atomic::AtomicUsize, mpsc, Arc, Condvar, Mutex};
    use std::thread;

    static TTS_RUNTIME_TEST_LOCK: Mutex<()> = Mutex::new(());

    struct TestRuntimeGuard(std::sync::MutexGuard<'static, ()>);

    impl Drop for TestRuntimeGuard {
        fn drop(&mut self) {
            while super::SYNTHESIS_PERMIT_OCCUPIED.load(std::sync::atomic::Ordering::Acquire) {
                thread::yield_now();
            }
            let _ = super::reap_abandoned_synthesis();
        }
    }

    fn test_runtime_guard() -> TestRuntimeGuard {
        TestRuntimeGuard(
            TTS_RUNTIME_TEST_LOCK
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()),
        )
    }

    struct FakeSynthesisEngine {
        sample_rate_hz: u32,
    }

    struct BlockingSynthesisEngine {
        sample_rate_hz: u32,
        started: mpsc::Sender<()>,
        release: Arc<(Mutex<bool>, Condvar)>,
    }

    impl super::SynthesisEngine for FakeSynthesisEngine {
        fn synthesize(&mut self, text: &str) -> Result<super::LocalTtsAudio, String> {
            super::synthesize_fake_audio(text, self.sample_rate_hz)
        }
    }

    impl super::SynthesisEngine for BlockingSynthesisEngine {
        fn synthesize(&mut self, text: &str) -> Result<super::LocalTtsAudio, String> {
            self.started
                .send(())
                .map_err(|_| String::from("failed to signal synthesis start"))?;
            let (lock, ready) = &*self.release;
            let mut released = lock
                .lock()
                .map_err(|_| String::from("release lock is poisoned"))?;
            while !*released {
                released = ready
                    .wait(released)
                    .map_err(|_| String::from("release lock is poisoned"))?;
            }
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
    fn piper_audio_output_selection_prefers_recognized_non_scalar_name() {
        let outputs = [("metadata", true), ("waveform", true), ("other", true)];

        assert_eq!(super::select_piper_audio_output(&outputs), Ok("waveform"));
    }

    #[test]
    fn piper_audio_output_selection_accepts_only_unnamed_single_non_scalar_output() {
        let outputs = [("result", true)];

        assert_eq!(super::select_piper_audio_output(&outputs), Ok("result"));
    }

    #[test]
    fn auto_provider_falls_back_to_cpu() {
        let mut attempts = Vec::new();
        let (runtime, provider) =
            super::select_tts_provider(super::TtsProviderPolicy::Auto, |cuda| {
                attempts.push(cuda);
                if cuda {
                    Err(String::from("CUDA unavailable"))
                } else {
                    Ok("cpu-runtime")
                }
            })
            .expect("auto policy should fall back to CPU");

        assert_eq!(attempts, [true, false]);
        assert_eq!(runtime, "cpu-runtime");
        assert_eq!(provider, super::TtsActualProvider::Cpu);
    }

    #[test]
    fn pcm_normalization_rejects_non_finite_and_clamps_amplitude() {
        let mut invalid = [0.0, f32::NAN];
        assert!(super::normalize_pcm_samples(&mut invalid).is_err());

        let mut samples = [-2.0, -0.5, 0.5, 2.0];
        super::normalize_pcm_samples(&mut samples).expect("finite samples should normalize");
        assert_eq!(samples, [-1.0, -0.5, 0.5, 1.0]);
    }

    #[test]
    fn piper_audio_output_selection_rejects_ambiguous_unnamed_outputs() {
        let outputs = [("result_a", true), ("result_b", true)];

        let error = super::select_piper_audio_output(&outputs)
            .expect_err("multiple unnamed non-scalar outputs must be rejected");
        assert!(error.contains("ambiguous"));
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
    fn contract_cuda_runtime_dll_check_lists_missing_executable_directory_dlls() {
        let temp_dir = tempfile::tempdir().expect("temp dir should be created");
        fs::write(temp_dir.path().join("cublas64_12.dll"), b"fixture")
            .expect("present dll fixture should be written");

        let error = super::verify_tts_cuda_runtime_dlls_in_dir(temp_dir.path())
            .expect_err("missing CUDA runtime DLLs should fail before provider init");

        assert!(
            error.contains("local tts CUDA runtime DLLs are missing beside the current executable")
        );
        assert!(error.contains("cublasLt64_12.dll"));
        assert!(error.contains("cufft64_11.dll"));
        assert!(error.contains("cudart64_12.dll"));
        assert!(error.contains("cudnn64_9.dll"));
        assert!(error.contains("cudnn_ops64_9.dll"));
        assert!(error.contains("onnxruntime_providers_cuda.dll"));
        assert!(error.contains("onnxruntime_providers_shared.dll"));
        assert!(!error.contains("CUDA_PATH"));
        assert!(!error.contains("PATH"));
    }

    #[test]
    fn contract_cuda_runtime_dll_check_accepts_complete_executable_directory() {
        let temp_dir = tempfile::tempdir().expect("temp dir should be created");
        for dll_name in super::TTS_CUDA_RUNTIME_DLLS {
            fs::write(temp_dir.path().join(dll_name), b"fixture")
                .expect("dll fixture should be written");
        }

        super::verify_tts_cuda_runtime_dlls_in_dir(temp_dir.path())
            .expect("complete CUDA runtime DLL set should pass preflight");
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

    #[test]
    fn piper_espeak_path_accepts_data_adjacent_to_model() {
        let temp_dir = tempfile::tempdir().expect("temp dir should be created");
        let model_path = temp_dir.path().join("jarvis.onnx");
        fs::create_dir_all(temp_dir.path().join("espeak-ng-data"))
            .expect("espeak-ng-data directory should be created");

        assert_eq!(
            super::adjacent_espeak_data_root(&model_path),
            Some(temp_dir.path().to_path_buf())
        );
    }

    fn fake_engine_factory(
        _model_path: PathBuf,
        sample_rate_hz: u32,
        _policy: super::TtsProviderPolicy,
    ) -> Result<(Box<dyn super::SynthesisEngine>, super::TtsActualProvider), String> {
        Ok((
            Box::new(FakeSynthesisEngine { sample_rate_hz }),
            super::TtsActualProvider::Cpu,
        ))
    }

    #[test]
    fn enabled_runtime_generates_audio() {
        let _test_runtime_guard = test_runtime_guard();
        let temp_dir = tempfile::tempdir().expect("temp dir should be created");
        let model_path = temp_dir.path().join("jarvis.onnx");
        fs::write(&model_path, b"fixture").expect("model fixture should be written");

        let runtime = LocalTtsRuntime::new_with_engine_factory(
            LocalTtsRuntimeSpec {
                model_path: Some(model_path),
                worker_count: 2,
                max_queue: 8,
                sample_rate_hz: 22_050,
                max_duration_s: 300,
                provider_policy: super::TtsProviderPolicy::Auto,
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
    fn sequential_synthesis_reuses_one_initialized_engine() {
        let _test_runtime_guard = test_runtime_guard();
        let temp_dir = tempfile::tempdir().expect("temp dir should be created");
        let model_path = temp_dir.path().join("jarvis.onnx");
        fs::write(&model_path, b"fixture").expect("model fixture should be written");
        let calls = Arc::new(AtomicUsize::new(0));
        let factory_calls = Arc::clone(&calls);
        let factory: super::SynthesisEngineFactory = Arc::new(move |_, sample_rate_hz, _| {
            factory_calls.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
            Ok((
                Box::new(FakeSynthesisEngine { sample_rate_hz }),
                super::TtsActualProvider::Cpu,
            ))
        });
        let runtime = LocalTtsRuntime::new_with_engine_factory(
            LocalTtsRuntimeSpec {
                model_path: Some(model_path),
                worker_count: 1,
                max_queue: 2,
                sample_rate_hz: 22_050,
                max_duration_s: 300,
                provider_policy: super::TtsProviderPolicy::Cpu,
            },
            factory,
        )
        .expect("runtime should initialize");
        runtime
            .synthesize("first")
            .expect("first synthesis should succeed");
        runtime
            .synthesize("second")
            .expect("second synthesis should succeed");
        assert_eq!(calls.load(std::sync::atomic::Ordering::Acquire), 1);
    }

    #[test]
    fn enabled_runtime_rejects_oversized_text_before_synthesis() {
        let _test_runtime_guard = test_runtime_guard();
        let temp_dir = tempfile::tempdir().expect("temp dir should be created");
        let model_path = temp_dir.path().join("jarvis.onnx");
        fs::write(&model_path, b"fixture").expect("model fixture should be written");
        let runtime = LocalTtsRuntime::new_with_engine_factory(
            LocalTtsRuntimeSpec {
                model_path: Some(model_path),
                worker_count: 1,
                max_queue: 1,
                sample_rate_hz: 22_050,
                max_duration_s: 300,
                provider_policy: super::TtsProviderPolicy::Auto,
            },
            Arc::new(fake_engine_factory),
        )
        .expect("runtime should initialize with fake engine");

        let error = runtime
            .synthesize(&"x".repeat(super::MAX_TTS_INPUT_BYTES + 1))
            .expect_err("oversized input must be rejected");

        assert!(error.contains("tts text exceeds maximum"));
    }

    #[test]
    fn synthesis_result_is_rejected_when_generation_is_cancelled_in_flight() {
        let _test_runtime_guard = test_runtime_guard();
        let temp_dir = tempfile::tempdir().expect("temp dir should be created");
        let model_path = temp_dir.path().join("jarvis.onnx");
        fs::write(&model_path, b"fixture").expect("model fixture should be written");
        let (started_tx, started_rx) = mpsc::channel();
        let release = Arc::new((Mutex::new(false), Condvar::new()));
        let engine_release = Arc::clone(&release);
        let runtime = Arc::new(
            LocalTtsRuntime::new_with_engine_factory(
                LocalTtsRuntimeSpec {
                    model_path: Some(model_path),
                    worker_count: 1,
                    max_queue: 1,
                    sample_rate_hz: 22_050,
                    max_duration_s: 300,
                    provider_policy: super::TtsProviderPolicy::Auto,
                },
                Arc::new(move |_, sample_rate_hz, _| {
                    Ok((
                        Box::new(BlockingSynthesisEngine {
                            sample_rate_hz,
                            started: started_tx.clone(),
                            release: Arc::clone(&engine_release),
                        }),
                        super::TtsActualProvider::Cpu,
                    ))
                }),
            )
            .expect("runtime should initialize with blocking engine"),
        );
        let generation = runtime.start_generation();
        let synthesizing_runtime = Arc::clone(&runtime);
        let synthesis = thread::spawn(move || {
            synthesizing_runtime.synthesize_for_generation("hello", generation)
        });
        started_rx
            .recv()
            .expect("synthesis should reach the blocking engine");

        runtime.cancel_generation();
        let (lock, ready) = &*release;
        *lock.lock().expect("release lock") = true;
        ready.notify_all();

        let error = synthesis
            .join()
            .expect("synthesis thread should join")
            .expect_err("superseded in-flight synthesis must not return audio");
        assert_eq!(error, super::SYNTHESIS_CANCELLED);

        let next_generation = runtime.start_generation();
        runtime
            .synthesize_for_generation("after release", next_generation)
            .expect("completed abandoned work should release the budget");
    }

    #[test]
    fn second_cancellation_exhausts_abandoned_work_budget() {
        let _test_runtime_guard = test_runtime_guard();
        let temp_dir = tempfile::tempdir().expect("temp dir should be created");
        let model_path = temp_dir.path().join("jarvis.onnx");
        fs::write(&model_path, b"fixture").expect("model fixture should be written");
        let (started_tx, started_rx) = mpsc::channel();
        let release = Arc::new((Mutex::new(false), Condvar::new()));
        let factory_calls = Arc::new(AtomicUsize::new(0));
        let engine_release = Arc::clone(&release);
        let factory_call_count = Arc::clone(&factory_calls);
        let runtime = Arc::new(
            LocalTtsRuntime::new_with_engine_factory(
                LocalTtsRuntimeSpec {
                    model_path: Some(model_path),
                    worker_count: 1,
                    max_queue: 1,
                    sample_rate_hz: 22_050,
                    max_duration_s: 300,
                    provider_policy: super::TtsProviderPolicy::Auto,
                },
                Arc::new(move |_, sample_rate_hz, _| {
                    if factory_call_count.fetch_add(1, std::sync::atomic::Ordering::AcqRel) == 0 {
                        Ok((
                            Box::new(BlockingSynthesisEngine {
                                sample_rate_hz,
                                started: started_tx.clone(),
                                release: Arc::clone(&engine_release),
                            }),
                            super::TtsActualProvider::Cpu,
                        ))
                    } else {
                        Ok((
                            Box::new(FakeSynthesisEngine { sample_rate_hz }),
                            super::TtsActualProvider::Cpu,
                        ))
                    }
                }),
            )
            .expect("runtime should initialize with blocking engine"),
        );

        let generation = runtime.start_generation();
        let first_runtime = Arc::clone(&runtime);
        let (first_result_tx, first_result_rx) = mpsc::channel();
        thread::spawn(move || {
            let _ =
                first_result_tx.send(first_runtime.synthesize_for_generation("first", generation));
        });
        started_rx
            .recv()
            .expect("first synthesis should reach the blocking engine");

        runtime.cancel_generation();
        let first_error = first_result_rx
            .recv_timeout(std::time::Duration::from_millis(500))
            .expect("cancelled caller should settle promptly")
            .expect_err("cancelled synthesis must fail");
        assert_eq!(first_error, super::SYNTHESIS_CANCELLED);

        let replacement_generation = runtime.start_generation();
        let error = runtime
            .synthesize_for_generation("replacement", replacement_generation)
            .expect_err("a second blocked cancellation must exhaust the worker budget");
        assert!(error.contains("abandoned synthesis budget exhausted"));

        // Do not leave the deliberately blocked native runner contaminating later tests.
        *release.0.lock().expect("release lock") = true;
        release.1.notify_all();
    }

    #[test]
    fn queued_generation_is_skipped_and_new_generation_runs() {
        let _test_runtime_guard = test_runtime_guard();
        let temp_dir = tempfile::tempdir().expect("temp dir should be created");
        let model_path = temp_dir.path().join("jarvis.onnx");
        fs::write(&model_path, b"fixture").expect("model fixture should be written");
        let runtime = Arc::new(
            LocalTtsRuntime::new_with_engine_factory(
                LocalTtsRuntimeSpec {
                    model_path: Some(model_path),
                    worker_count: 1,
                    max_queue: 2,
                    sample_rate_hz: 22_050,
                    max_duration_s: 300,
                    provider_policy: super::TtsProviderPolicy::Auto,
                },
                Arc::new(fake_engine_factory),
            )
            .expect("runtime should initialize"),
        );
        let old_generation = runtime.start_generation();
        runtime.cancel_generation();
        let stale = Arc::clone(&runtime);
        let stale_result =
            thread::spawn(move || stale.synthesize_for_generation("stale", old_generation))
                .join()
                .expect("stale request should join")
                .expect_err("cancelled generation must be rejected");
        assert_eq!(stale_result, super::SYNTHESIS_CANCELLED);

        let new_generation = runtime.start_generation();
        runtime
            .synthesize_for_generation("current", new_generation)
            .expect("new generation should not be blocked by stale work");
    }

    #[test]
    fn queued_request_is_rejected_when_generation_is_cancelled() {
        let _test_runtime_guard = test_runtime_guard();
        let temp_dir = tempfile::tempdir().expect("temp dir should be created");
        let model_path = temp_dir.path().join("jarvis.onnx");
        fs::write(&model_path, b"fixture").expect("model fixture should be written");
        let (started_tx, started_rx) = mpsc::channel();
        let release = Arc::new((Mutex::new(false), Condvar::new()));
        let engine_release = Arc::clone(&release);
        let runtime = Arc::new(
            LocalTtsRuntime::new_with_engine_factory(
                LocalTtsRuntimeSpec {
                    model_path: Some(model_path),
                    worker_count: 1,
                    max_queue: 2,
                    sample_rate_hz: 22_050,
                    max_duration_s: 300,
                    provider_policy: super::TtsProviderPolicy::Auto,
                },
                Arc::new(move |_, sample_rate_hz, _| {
                    Ok((
                        Box::new(BlockingSynthesisEngine {
                            sample_rate_hz,
                            started: started_tx.clone(),
                            release: Arc::clone(&engine_release),
                        }),
                        super::TtsActualProvider::Cpu,
                    ))
                }),
            )
            .expect("runtime should initialize with blocking engine"),
        );

        let generation = runtime.start_generation();
        let first_runtime = Arc::clone(&runtime);
        let first =
            thread::spawn(move || first_runtime.synthesize_for_generation("first", generation));
        started_rx
            .recv()
            .expect("first synthesis should reach the blocking engine");

        let queued_runtime = Arc::clone(&runtime);
        let queued =
            thread::spawn(move || queued_runtime.synthesize_for_generation("queued", generation));
        runtime.cancel_generation();

        let (lock, ready) = &*release;
        *lock.lock().expect("release lock") = true;
        ready.notify_all();

        assert_eq!(
            first
                .join()
                .expect("first synthesis should join")
                .unwrap_err(),
            super::SYNTHESIS_CANCELLED
        );
        assert_eq!(
            queued
                .join()
                .expect("queued synthesis should join")
                .unwrap_err(),
            super::SYNTHESIS_CANCELLED
        );
    }

    #[test]
    fn enabled_runtime_surfaces_cuda_provider_init_failure() {
        let _test_runtime_guard = test_runtime_guard();
        let temp_dir = tempfile::tempdir().expect("temp dir should be created");
        let model_path = temp_dir.path().join("jarvis.onnx");
        fs::write(&model_path, b"fixture").expect("model fixture should be written");

        let runtime_result = LocalTtsRuntime::new_with_engine_factory(
            LocalTtsRuntimeSpec {
                model_path: Some(model_path),
                worker_count: 1,
                max_queue: 4,
                sample_rate_hz: 22_050,
                max_duration_s: 300,
                provider_policy: super::TtsProviderPolicy::Auto,
            },
            Arc::new(|_, _, _| {
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
        let _test_runtime_guard = test_runtime_guard();
        let temp_dir = tempfile::tempdir().expect("temp dir should be created");
        let model_path = temp_dir.path().join("jarvis.onnx");
        fs::write(&model_path, b"fixture").expect("model fixture should be written");

        let runtime = LocalTtsRuntime::new_with_engine_factory(
            LocalTtsRuntimeSpec {
                model_path: Some(model_path),
                worker_count: 1,
                max_queue: 4,
                sample_rate_hz: 22_050,
                max_duration_s: 1,
                provider_policy: super::TtsProviderPolicy::Auto,
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

    #[test]
    fn timed_out_initialization_is_owned_and_reaped_before_retry() {
        let _test_runtime_guard = test_runtime_guard();
        let temp_dir = tempfile::tempdir().expect("temp dir should be created");
        let model_path = temp_dir.path().join("jarvis.onnx");
        fs::write(&model_path, b"fixture").expect("model fixture should be written");
        let release = Arc::new((Mutex::new(false), Condvar::new()));
        let calls = Arc::new(AtomicUsize::new(0));
        let factory_release = Arc::clone(&release);
        let factory_calls = Arc::clone(&calls);
        let factory: super::SynthesisEngineFactory = Arc::new(move |_, sample_rate_hz, _| {
            factory_calls.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
            let (lock, ready) = &*factory_release;
            let mut released = lock.lock().expect("release lock");
            while !*released {
                released = ready.wait(released).expect("release lock");
            }
            Ok((
                Box::new(FakeSynthesisEngine { sample_rate_hz }) as Box<dyn super::SynthesisEngine>,
                super::TtsActualProvider::Cpu,
            ))
        });
        let spec = || LocalTtsRuntimeSpec {
            model_path: Some(model_path.clone()),
            worker_count: 1,
            max_queue: 1,
            sample_rate_hz: 22_050,
            max_duration_s: 300,
            provider_policy: super::TtsProviderPolicy::Cpu,
        };

        let first = LocalTtsRuntime::new_with_engine_factory(spec(), Arc::clone(&factory));
        assert!(first
            .expect_err("blocked initialization should time out")
            .contains("timed out initializing"));
        let second = LocalTtsRuntime::new_with_engine_factory(spec(), Arc::clone(&factory))
            .expect_err("active initialization must block a replacement factory");
        assert!(second.contains("still pending"));
        assert_eq!(calls.load(std::sync::atomic::Ordering::Acquire), 1);

        let (lock, ready) = &*release;
        *lock.lock().expect("release lock") = true;
        ready.notify_all();
        let third = loop {
            match LocalTtsRuntime::new_with_engine_factory(spec(), Arc::clone(&factory)) {
                Ok(runtime) => break runtime,
                Err(error) if error.contains("still pending") => {
                    thread::sleep(std::time::Duration::from_millis(10));
                }
                Err(error) => panic!("reaped initialization should retry: {error}"),
            }
        };
        assert_eq!(calls.load(std::sync::atomic::Ordering::Acquire), 2);
        third
            .synthesize("after retry")
            .expect("restarted runtime should synthesize");
    }
}
