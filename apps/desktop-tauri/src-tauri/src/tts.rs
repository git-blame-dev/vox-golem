use std::env;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Condvar, Mutex, OnceLock};
use std::thread;
use std::time::Duration;

use espeak_rs::text_to_phonemes;
use serde::Deserialize;
use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct LocalTtsRuntimeSpec {
    pub neural: Option<NeuralTtsRuntimeSpec>,
    pub model_path: Option<PathBuf>,
    pub worker_count: usize,
    pub max_queue: usize,
    pub sample_rate_hz: u32,
    pub max_duration_s: u64,
    pub provider_policy: TtsProviderPolicy,
}

#[derive(Debug, Clone)]
pub struct NeuralTtsRuntimeSpec {
    pub options: voxgolem_platform::neural_tts::NeuralTtsOptions,
    pub voice: String,
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
    neural: Option<Arc<voxgolem_platform::neural_tts::NeuralTtsClient>>,
    neural_voice: Mutex<String>,
    neural_default_voice: String,
    cue_epoch: u64,
    neural_cancellations: Mutex<Vec<std::sync::Weak<voxgolem_platform::neural_tts::Cancellation>>>,
    generation: Arc<AtomicU64>,
    sample_rate_hz: u32,
    max_duration_ms: u64,
    max_duration_samples: usize,
    sender: Option<mpsc::SyncSender<SynthesisJob>>,
    _workers: Vec<thread::JoinHandle<()>>,
    actual_provider: Option<TtsActualProvider>,
    shutdown: Arc<AtomicBool>,
}

static NEXT_NEURAL_RUNTIME: AtomicU64 = AtomicU64::new(1);
const MAX_TTS_INPUT_BYTES: usize = 4096;
const MAX_SYNTHESIS_WAIT_MS: u64 = 10 * 60 * 1_000;
const WORKER_INITIALIZATION_TIMEOUT: Duration = Duration::from_secs(8);

type SynthesisEngineFactory = Arc<
    dyn Fn(
            PathBuf,
            u32,
            TtsProviderPolicy,
        ) -> Result<(Box<dyn SynthesisEngine>, TtsActualProvider), String>
        + Send
        + Sync,
>;

type ThreadSpawner = Arc<
    dyn Fn(String, Box<dyn FnOnce() + Send + 'static>) -> std::io::Result<thread::JoinHandle<()>>
        + Send
        + Sync,
>;

fn default_thread_spawner(
    name: String,
    work: Box<dyn FnOnce() + Send + 'static>,
) -> std::io::Result<thread::JoinHandle<()>> {
    thread::Builder::new().name(name).spawn(work)
}

type AbandonedSynthesis = (
    thread::JoinHandle<()>,
    mpsc::Receiver<Result<(Box<dyn SynthesisEngine>, LocalTtsAudio), String>>,
);

static PENDING_INITIALIZATIONS: OnceLock<Mutex<Vec<thread::JoinHandle<()>>>> = OnceLock::new();
static ABANDONED_SYNTHESIS: OnceLock<Mutex<Vec<thread::JoinHandle<()>>>> = OnceLock::new();
static SYNTHESIS_PERMIT_OCCUPIED: AtomicBool = AtomicBool::new(false);
static PHYSICAL_WORK: Mutex<usize> = Mutex::new(0);
static PHYSICAL_WORK_RETIRED: Condvar = Condvar::new();

/// Snapshot of queued/admitted synthesis, constructor/engine initialization, and
/// actual native synthesis (including cancelled calls whose callers already returned).
/// Idle engine-owning workers and finished retained thread handles are not busy work.
/// This does not close admission: callers must hold their lifecycle/update gate to
/// prevent new constructors or synthesis requests racing an installation idle check.
pub(crate) fn physical_work_pending() -> bool {
    match PHYSICAL_WORK.lock() {
        Ok(work) => *work != 0 || SYNTHESIS_PERMIT_OCCUPIED.load(Ordering::Acquire),
        Err(_) => true,
    }
}

/// Wait without polling or cancelling native work. True observes physical quiescence
/// under the notifier mutex; it is not a promise that later admission cannot occur.
/// Close admission first for shutdown/update. Timeout or poisoned tracking fails closed.
pub(crate) fn wait_for_physical_retirement(timeout: Duration) -> bool {
    let Ok(work) = PHYSICAL_WORK.lock() else {
        return false;
    };
    match PHYSICAL_WORK_RETIRED.wait_timeout_while(work, timeout, |work| {
        *work != 0 || SYNTHESIS_PERMIT_OCCUPIED.load(Ordering::Acquire)
    }) {
        Ok((work, _)) => *work == 0 && !SYNTHESIS_PERMIT_OCCUPIED.load(Ordering::Acquire),
        Err(_) => false,
    }
}

/// Reserved before publishing work to another thread, so startup and queue handoffs
/// cannot temporarily appear idle. The native permit takes over cancelled synthesis.
#[derive(Debug)]
pub(crate) struct PhysicalWork;

impl PhysicalWork {
    fn begin() -> Self {
        let mut work = PHYSICAL_WORK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        *work = work
            .checked_add(1)
            .expect("tts physical work count exhausted");
        Self
    }
}

impl Drop for PhysicalWork {
    fn drop(&mut self) {
        let mut work = PHYSICAL_WORK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        *work -= 1;
        PHYSICAL_WORK_RETIRED.notify_all();
    }
}

struct SynthesisPermit;

impl SynthesisPermit {
    fn acquire() -> Option<Self> {
        let _work = PHYSICAL_WORK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        SYNTHESIS_PERMIT_OCCUPIED
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .ok()
            .map(|_| Self)
    }
}

impl Drop for SynthesisPermit {
    fn drop(&mut self) {
        let _work = PHYSICAL_WORK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        SYNTHESIS_PERMIT_OCCUPIED.store(false, Ordering::Release);
        PHYSICAL_WORK_RETIRED.notify_all();
    }
}

fn reap_abandoned_synthesis() -> bool {
    let pending = ABANDONED_SYNTHESIS.get_or_init(|| Mutex::new(Vec::new()));
    let mut pending = pending
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
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
    let mut pending = pending
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    pending.push(runner);
}

fn reap_pending_initializations() -> bool {
    let pending = PENDING_INITIALIZATIONS.get_or_init(|| Mutex::new(Vec::new()));
    let mut pending = pending
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
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
    let mut pending = pending
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    pending.extend(workers);
}

trait SynthesisEngine: Send {
    fn synthesize(&mut self, text: &str) -> Result<LocalTtsAudio, String>;
}

struct PiperOnnxEngine {
    session: ort::session::Session,
    config: PiperVoiceConfig,
    contract: PiperModelContract,
    limits: voxgolem_core::config::ValidatedTtsResourceLimits,
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
    _physical_work: PhysicalWork,
}

const SYNTHESIS_CANCELLED: &str = "local tts synthesis request was cancelled or superseded";

impl LocalTtsRuntime {
    pub fn new(spec: LocalTtsRuntimeSpec) -> Result<Self, String> {
        if let Some(neural) = spec.neural.clone() {
            let client = tauri::async_runtime::block_on(
                voxgolem_platform::neural_tts::NeuralTtsClient::connect(&neural.options),
            )
            .map_err(|error| error.to_string())?;
            let rate = client.capabilities().sample_rate_hz;
            let voice = client
                .capabilities()
                .resolve_voice(&neural.voice)
                .ok_or_else(|| String::from("configured Neural TTS voice is unavailable"))?
                .id
                .clone();
            let limits = voxgolem_core::config::validate_tts_resource_limits(
                1,
                spec.max_queue,
                rate,
                spec.max_duration_s,
            )
            .map_err(|error| error.format_with_prefix("tts "))?;
            return Ok(Self {
                neural: Some(Arc::new(client)),
                neural_default_voice: voice.clone(),
                neural_voice: Mutex::new(voice),
                cue_epoch: NEXT_NEURAL_RUNTIME.fetch_add(1, Ordering::Relaxed),
                neural_cancellations: Mutex::new(Vec::new()),
                generation: Arc::new(AtomicU64::new(0)),
                sample_rate_hz: rate,
                max_duration_ms: limits.max_duration_ms,
                max_duration_samples: limits.max_duration_samples,
                sender: None,
                _workers: Vec::new(),
                actual_provider: None,
                shutdown: Arc::new(AtomicBool::new(false)),
            });
        }
        let max_duration_s = spec.max_duration_s;
        let engine_factory: SynthesisEngineFactory =
            Arc::new(move |model_path, sample_rate_hz, policy| {
                default_engine_factory(model_path, sample_rate_hz, max_duration_s, policy)
            });
        Self::new_with_engine_factory(spec, engine_factory)
    }

    fn new_with_engine_factory(
        spec: LocalTtsRuntimeSpec,
        engine_factory: SynthesisEngineFactory,
    ) -> Result<Self, String> {
        let spawner: ThreadSpawner = Arc::new(default_thread_spawner);
        Self::new_with_engine_factory_and_spawners(
            spec,
            engine_factory,
            Arc::clone(&spawner),
            spawner,
        )
    }

    fn new_with_engine_factory_and_spawners(
        spec: LocalTtsRuntimeSpec,
        engine_factory: SynthesisEngineFactory,
        worker_spawner: ThreadSpawner,
        runner_spawner: ThreadSpawner,
    ) -> Result<Self, String> {
        let _construction_work = PhysicalWork::begin();
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
        let limits = voxgolem_core::config::validate_tts_resource_limits(
            spec.worker_count,
            spec.max_queue,
            spec.sample_rate_hz,
            spec.max_duration_s,
        )
        .map_err(|error| error.format_with_prefix("tts "))?;

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

        let (sender, receiver) = mpsc::sync_channel::<SynthesisJob>(limits.max_queue);
        let generation = Arc::new(AtomicU64::new(0));
        let shutdown = Arc::new(AtomicBool::new(false));
        let shared_receiver = Arc::new(Mutex::new(receiver));
        let mut workers = Vec::with_capacity(limits.worker_count);
        let (init_tx, init_rx) = mpsc::channel::<Result<TtsActualProvider, String>>();
        let provider_policy = spec.provider_policy;
        for index in 0..limits.worker_count {
            let worker_receiver = Arc::clone(&shared_receiver);
            let sample_rate_hz = limits.sample_rate_hz;
            let worker_model_path = model_path.clone();
            let worker_engine_factory = Arc::clone(&engine_factory);
            let worker_runner_spawner = Arc::clone(&runner_spawner);
            let worker_generation = Arc::clone(&generation);
            let worker_init_tx = init_tx.clone();
            let worker_shutdown = Arc::clone(&shutdown);
            let initialization_work = PhysicalWork::begin();
            let worker_name = format!("voxgolem-tts-worker-{index}");
            let worker_handle = match worker_spawner(
                worker_name,
                Box::new(move || {
                    let mut initialization_work = Some(initialization_work);
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
                                // An initializer that outlived its constructor remains
                                // tracked through shutdown cleanup, not just factory return.
                                // Ordinary idle, engine-owning workers do not count as work.
                                drop(initialization_work.take());
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
                                    let _ =
                                        job.response.send(Err(String::from(SYNTHESIS_CANCELLED)));
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
                                let handoff = Arc::new(Mutex::new(Some((engine, permit))));
                                let child_handoff = Arc::clone(&handoff);
                                let (result_tx, result_rx) = mpsc::channel();
                                let child_text = job.text.clone();
                                let child = match worker_runner_spawner(
                                    String::from("voxgolem-tts-synth"),
                                    Box::new(move || {
                                        let Some((mut engine, _permit)) = child_handoff
                                            .lock()
                                            .unwrap_or_else(|poisoned| poisoned.into_inner())
                                            .take()
                                        else {
                                            return;
                                        };
                                        let result = engine
                                            .synthesize(&child_text)
                                            .map(|audio| (engine, audio));
                                        let _ = result_tx.send(result);
                                    }),
                                ) {
                                    Ok(child) => child,
                                    Err(error) => {
                                        if let Some((recovered_engine, recovered_permit)) = handoff
                                            .lock()
                                            .unwrap_or_else(|poisoned| poisoned.into_inner())
                                            .take()
                                        {
                                            available_engine = Some(recovered_engine);
                                            drop(recovered_permit);
                                        }
                                        let _ = job.response.send(Err(format!(
                                            "failed to spawn local tts synthesis runner: {error}"
                                        )));
                                        continue;
                                    }
                                };

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
                                            ));
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
                }),
            ) {
                Ok(handle) => handle,
                Err(error) => {
                    shutdown.store(true, Ordering::Release);
                    drop(sender);
                    retain_pending_initializations(workers);
                    return Err(format!("failed to spawn local tts worker thread: {error}"));
                }
            };
            workers.push(worker_handle);
        }

        drop(init_tx);
        let mut actual_provider = None;
        for _ in 0..limits.worker_count {
            let init_result =
                init_rx
                    .recv_timeout(WORKER_INITIALIZATION_TIMEOUT)
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
            neural: None,
            neural_voice: Mutex::new(String::new()),
            neural_default_voice: String::new(),
            cue_epoch: 0,
            neural_cancellations: Mutex::new(Vec::new()),
            generation,
            sample_rate_hz: limits.sample_rate_hz,
            max_duration_ms: limits.max_duration_ms,
            max_duration_samples: limits.max_duration_samples,
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
        self.advance_generation()
    }

    fn advance_generation(&self) -> u64 {
        // Admission and cancellation share this lock: a stale token can neither
        // register after Stop nor escape the cancellation already in progress.
        let mut tokens = self
            .neural_cancellations
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let generation = self
            .generation
            .fetch_add(1, Ordering::AcqRel)
            .saturating_add(1);
        tokens.retain(|token| {
            if let Some(token) = token.upgrade() {
                token.cancel();
                true
            } else {
                false
            }
        });
        generation
    }

    pub fn cancel_generation(&self) {
        self.advance_generation();
    }

    pub fn shutdown_bounded(&mut self) {
        self.cancel_generation();
        if let Some(client) = &self.neural {
            client.close();
        }
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
        self.synthesize_inner(text, generation, None)
    }

    pub fn synthesize(&self, text: &str) -> Result<LocalTtsAudio, String> {
        self.synthesize_inner(text, self.generation.load(Ordering::Acquire), None)
    }

    pub(crate) fn current_generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }

    pub(crate) fn synthesize_cue(
        &self,
        text: &str,
        voice: &str,
        generation: u64,
    ) -> Result<LocalTtsAudio, String> {
        self.synthesize_inner(text, generation, Some(voice))
    }

    fn synthesize_inner(
        &self,
        text: &str,
        generation: u64,
        voice_override: Option<&str>,
    ) -> Result<LocalTtsAudio, String> {
        let text = text.trim();
        if text.is_empty() {
            return Err(String::from("tts text must not be empty"));
        }
        if text.len() > MAX_TTS_INPUT_BYTES {
            return Err(format!(
                "tts text exceeds maximum of {MAX_TTS_INPUT_BYTES} bytes"
            ));
        }

        if generation != self.generation.load(Ordering::Acquire) {
            return Err(String::from(SYNTHESIS_CANCELLED));
        }
        if let Some(client) = &self.neural {
            let _work = PhysicalWork::begin();
            let (cancellation, voice) = self.neural_request(generation)?;
            let audio = tauri::async_runtime::block_on(async {
                if let Some(voice) = voice_override {
                    client.collect_background(text, voice, cancellation).await
                } else {
                    client.collect(text, &voice, cancellation).await
                }
            })
            .map_err(|error| error.to_string())?;
            if generation != self.generation.load(Ordering::Acquire) {
                return Err(String::from(SYNTHESIS_CANCELLED));
            }
            let duration_ms = audio.samples.len() as u64 * 1000 / u64::from(audio.sample_rate_hz);
            if audio.samples.len() > self.max_duration_samples || duration_ms > self.max_duration_ms
            {
                return Err(String::from("Neural TTS audio exceeds configured duration"));
            }
            return Ok(LocalTtsAudio {
                pcm_f32: audio.samples,
                sample_rate_hz: audio.sample_rate_hz,
                duration_ms,
            });
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
                _physical_work: PhysicalWork::begin(),
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

        if audio.pcm_f32.len() > voxgolem_core::config::MAX_TTS_RESULT_SAMPLES {
            return Err(format!(
                "tts synthesized audio buffer of {} samples exceeds max copy limit of {} samples",
                audio.pcm_f32.len(),
                voxgolem_core::config::MAX_TTS_RESULT_SAMPLES
            ));
        }

        if audio.duration_ms > self.max_duration_ms
            || audio.pcm_f32.len() > self.max_duration_samples
        {
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

    pub(crate) fn neural_client(
        &self,
    ) -> Option<Arc<voxgolem_platform::neural_tts::NeuralTtsClient>> {
        self.neural.clone()
    }

    pub(crate) fn selected_neural_voice(&self) -> Option<String> {
        self.neural.as_ref()?;
        self.neural_voice.lock().ok().map(|voice| voice.clone())
    }

    pub(crate) fn neural_request(
        &self,
        expected_generation: u64,
    ) -> Result<(Arc<voxgolem_platform::neural_tts::Cancellation>, String), String> {
        let token = Arc::new(voxgolem_platform::neural_tts::Cancellation::default());
        let mut tokens = self
            .neural_cancellations
            .lock()
            .map_err(|_| String::from("Neural TTS cancellation state unavailable"))?;
        tokens.retain(|token| token.strong_count() != 0);
        if self.generation.load(Ordering::Acquire) != expected_generation {
            return Err(String::from(SYNTHESIS_CANCELLED));
        }
        if tokens.len() >= 16 || self.shutdown.load(Ordering::Acquire) {
            return Err(String::from("Neural TTS admission unavailable"));
        }
        tokens.push(Arc::downgrade(&token));
        let voice = self
            .neural_voice
            .lock()
            .map_err(|_| String::from("Neural TTS voice state unavailable"))?
            .clone();
        Ok((token, voice))
    }

    pub(crate) fn set_neural_voice(&self, requested: &str, configured: &str) -> Result<(), String> {
        if let Some(client) = &self.neural {
            let requested = if requested.is_empty() {
                if configured.is_empty() {
                    &self.neural_default_voice
                } else {
                    configured
                }
            } else {
                requested
            };
            let voice = client
                .capabilities()
                .resolve_voice(requested)
                .ok_or_else(|| String::from("selected Neural TTS voice is unavailable"))?;
            *self
                .neural_voice
                .lock()
                .map_err(|_| String::from("Neural TTS voice state unavailable"))? =
                voice.id.clone();
        }
        Ok(())
    }

    pub(crate) fn cue_key(&self) -> String {
        self.cue_key_for_voice(&self.selected_neural_voice().unwrap_or_default())
    }

    pub(crate) fn cue_key_for_voice(&self, voice: &str) -> String {
        if self.neural.is_some() {
            format!("neural:{}:{}", self.cue_epoch, voice)
        } else {
            String::from("piper")
        }
    }

    pub(crate) fn track_neural_work(&self) -> PhysicalWork {
        PhysicalWork::begin()
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
        limits: voxgolem_core::config::ValidatedTtsResourceLimits,
        policy: TtsProviderPolicy,
    ) -> Result<(Self, TtsActualProvider), String> {
        let config = load_piper_voice_config(model_path)?;
        if config.audio.sample_rate != limits.sample_rate_hz {
            return Err(format!(
                "tts sample_rate_hz {} does not match Piper model config sample rate {}",
                limits.sample_rate_hz, config.audio.sample_rate
            ));
        }

        let try_provider = |cuda: bool| -> Result<ort::session::Session, String> {
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
                limits,
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

        decode_audio_from_outputs(outputs, self.contract.output_name.as_str(), self.limits)
    }
}

/// Extracts, validates, and copies a synthesized ONNX output tensor into a `LocalTtsAudio` buffer.
///
/// The 64 MiB (`MAX_TTS_RESULT_SAMPLES` = `16,777,216` `f32` samples) bound and the
/// configured duration bound (`max_duration_samples` / `max_duration_ms`) are checked on
/// the borrowed ONNX slice before allocating or copying into a Rust `Vec<f32>`.
/// Native ONNX Runtime tensor allocation and total retained process memory are not bounded
/// by this per-result post-inference copy check.
fn decode_audio_from_outputs(
    outputs: ort::session::SessionOutputs<'_>,
    output_name: &str,
    limits: voxgolem_core::config::ValidatedTtsResourceLimits,
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
    decode_audio_from_samples(audio_tensor.1, limits)
}

fn validate_synthesized_sample_bounds(
    sample_count: usize,
    limits: voxgolem_core::config::ValidatedTtsResourceLimits,
) -> Result<u64, String> {
    if sample_count > voxgolem_core::config::MAX_TTS_RESULT_SAMPLES {
        return Err(format!(
            "tts synthesized audio buffer of {} samples ({} bytes) exceeds max copy limit of {} samples ({} bytes)",
            sample_count,
            sample_count.saturating_mul(std::mem::size_of::<f32>()),
            voxgolem_core::config::MAX_TTS_RESULT_SAMPLES,
            voxgolem_core::config::MAX_TTS_RESULT_BYTES,
        ));
    }
    let scaled_ms = u64::try_from(sample_count)
        .ok()
        .and_then(|count| count.checked_mul(1_000))
        .ok_or_else(|| {
            String::from("tts synthesized sample count overflowed duration calculation")
        })?;
    let duration_ms = (scaled_ms / u64::from(limits.sample_rate_hz)).max(1);
    if sample_count > limits.max_duration_samples || duration_ms > limits.max_duration_ms {
        return Err(format!(
            "tts synthesized duration {}ms ({} samples) exceeds configured max {}ms ({} samples)",
            duration_ms, sample_count, limits.max_duration_ms, limits.max_duration_samples
        ));
    }
    Ok(duration_ms)
}

fn decode_audio_from_samples(
    samples: &[f32],
    limits: voxgolem_core::config::ValidatedTtsResourceLimits,
) -> Result<LocalTtsAudio, String> {
    if samples.is_empty() {
        return Err(String::from(
            "local tts model produced an empty audio buffer",
        ));
    }
    let duration_ms = validate_synthesized_sample_bounds(samples.len(), limits)?;
    let mut pcm_f32 = Vec::new();
    pcm_f32
        .try_reserve_exact(samples.len())
        .map_err(|error| format!("failed to allocate local tts audio buffer: {error}"))?;
    pcm_f32.extend_from_slice(samples);
    normalize_pcm_samples(&mut pcm_f32)?;

    Ok(LocalTtsAudio {
        pcm_f32,
        sample_rate_hz: limits.sample_rate_hz,
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
    max_duration_s: u64,
    policy: TtsProviderPolicy,
) -> Result<(Box<dyn SynthesisEngine>, TtsActualProvider), String> {
    let limits =
        voxgolem_core::config::validate_tts_resource_limits(1, 1, sample_rate_hz, max_duration_s)
            .map_err(|error| error.format_with_prefix("tts "))?;
    ensure_espeak_data_directory_env(&model_path)?;
    let (engine, provider) = PiperOnnxEngine::new(&model_path, limits, policy)?;
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
pub(super) fn test_runtime_guard() -> impl Drop {
    tests::test_runtime_guard()
}

#[cfg(test)]
mod tests {
    use super::{LocalTtsRuntime, LocalTtsRuntimeSpec};
    use std::fs;
    use std::path::PathBuf;
    use std::sync::{atomic::AtomicUsize, mpsc, Arc, Condvar, Mutex};
    use std::thread;
    use std::time::{Duration, Instant};

    const TEST_WAIT_BOUND: Duration = Duration::from_secs(20);

    static TTS_RUNTIME_TEST_LOCK: Mutex<()> = Mutex::new(());

    pub(super) struct TestRuntimeGuard(std::sync::MutexGuard<'static, ()>);

    impl Drop for TestRuntimeGuard {
        fn drop(&mut self) {
            let retired = super::wait_for_physical_retirement(TEST_WAIT_BOUND);
            let _ = super::reap_abandoned_synthesis();
            if !thread::panicking() {
                assert!(
                    retired,
                    "test physical work did not retire within the cleanup bound"
                );
            }
        }
    }

    pub(super) fn test_runtime_guard() -> TestRuntimeGuard {
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

    struct ReleaseOnDrop(Arc<(Mutex<bool>, Condvar)>);

    impl ReleaseOnDrop {
        fn release(&self) {
            let (lock, ready) = &*self.0;
            *lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = true;
            ready.notify_all();
        }
    }

    impl Drop for ReleaseOnDrop {
        fn drop(&mut self) {
            self.release();
        }
    }

    fn wait_for_release(release: &(Mutex<bool>, Condvar)) -> Result<(), String> {
        let (lock, ready) = release;
        let released = lock
            .lock()
            .map_err(|_| String::from("release lock is poisoned"))?;
        let (released, _) = ready
            .wait_timeout_while(released, TEST_WAIT_BOUND, |released| !*released)
            .map_err(|_| String::from("release lock is poisoned"))?;
        if !*released {
            return Err(String::from("test worker release wait exceeded its bound"));
        }
        Ok(())
    }

    fn spawn_test_worker<T: Send + 'static>(
        work: impl FnOnce() -> T + Send + 'static,
    ) -> mpsc::Receiver<T> {
        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || {
            let _ = sender.send(work());
        });
        receiver
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
            wait_for_release(&self.release)?;
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
    fn auto_provider_keeps_successful_cuda_without_trying_cpu() {
        let mut attempts = Vec::new();
        let (runtime, provider) =
            super::select_tts_provider(super::TtsProviderPolicy::Auto, |cuda| {
                attempts.push(cuda);
                Ok("synthetic-cuda-runtime")
            })
            .expect("successful CUDA initialization should be selected");

        assert_eq!(attempts, [true]);
        assert_eq!(runtime, "synthetic-cuda-runtime");
        assert_eq!(provider, super::TtsActualProvider::Cuda);
    }

    #[test]
    fn cpu_provider_does_not_attempt_cuda() {
        let mut attempts = Vec::new();
        let (_, provider) = super::select_tts_provider(super::TtsProviderPolicy::Cpu, |cuda| {
            attempts.push(cuda);
            Ok(())
        })
        .expect("CPU initialization should succeed");

        assert_eq!(attempts, [false]);
        assert_eq!(provider, super::TtsActualProvider::Cpu);
    }

    #[test]
    fn blocking_fixture_releases_worker_on_unwind() {
        let release = Arc::new((Mutex::new(false), Condvar::new()));
        let worker_release = Arc::clone(&release);
        let result = spawn_test_worker(move || wait_for_release(&worker_release));
        let unwound = std::panic::catch_unwind(move || {
            let _release_on_drop = ReleaseOnDrop(release);
            panic!("synthetic assertion failure before explicit release");
        });
        assert!(unwound.is_err());
        result
            .recv_timeout(TEST_WAIT_BOUND)
            .expect("unwinding must release the blocked worker")
            .expect("worker should observe release rather than its timeout");
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
    fn cuda_provider_failure_is_preserved_without_cpu_fallback() {
        let mut attempts = Vec::new();
        let error = super::select_tts_provider::<()>(super::TtsProviderPolicy::Cuda, |cuda| {
            attempts.push(cuda);
            Err(String::from("synthetic ORT CUDA initialization failure"))
        })
        .expect_err("explicit CUDA must not silently fall back");

        assert_eq!(attempts, [true]);
        assert_eq!(error, "synthetic ORT CUDA initialization failure");
    }

    #[test]
    fn auto_provider_preserves_cpu_error_when_both_initializations_fail() {
        let mut attempts = Vec::new();
        let error = super::select_tts_provider::<()>(super::TtsProviderPolicy::Auto, |cuda| {
            attempts.push(cuda);
            Err(String::from(if cuda {
                "synthetic CUDA failure"
            } else {
                "synthetic CPU failure"
            }))
        })
        .expect_err("failed CPU fallback must surface its error");

        assert_eq!(attempts, [true, false]);
        assert_eq!(error, "synthetic CPU failure");
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
    fn stop_between_generation_check_and_token_registration_rejects_stale_admission() {
        let runtime = Arc::new(LocalTtsRuntime {
            neural: None,
            neural_voice: Mutex::new(String::new()),
            neural_default_voice: String::new(),
            cue_epoch: 0,
            neural_cancellations: Mutex::new(Vec::new()),
            generation: Arc::new(super::AtomicU64::new(0)),
            sample_rate_hz: 24000,
            max_duration_ms: 1000,
            max_duration_samples: 24000,
            sender: None,
            _workers: Vec::new(),
            actual_provider: None,
            shutdown: Arc::new(super::AtomicBool::new(false)),
        });
        let old_generation = runtime.start_generation();
        let gate = Arc::new(std::sync::Barrier::new(2));
        let delayed_runtime = Arc::clone(&runtime);
        let delayed_gate = Arc::clone(&gate);
        let delayed = thread::spawn(move || {
            delayed_gate.wait();
            delayed_runtime.neural_request(old_generation)
        });
        runtime.cancel_generation();
        gate.wait();
        assert!(
            delayed.join().unwrap().is_err(),
            "obsolete inference must not be admitted"
        );
        assert_eq!(
            runtime.synthesize_cue("Synthetic remaining cue.", "synthetic", old_generation),
            Err(String::from(super::SYNTHESIS_CANCELLED)),
            "remaining fixed cues cannot join the replacement generation",
        );
        let next_generation = runtime.start_generation();
        let (token, _) = runtime.neural_request(next_generation).unwrap();
        runtime.cancel_generation();
        assert!(
            token.is_cancelled(),
            "already registered work must observe Stop"
        );
    }

    #[test]
    fn enabled_runtime_generates_audio() {
        let _test_runtime_guard = test_runtime_guard();
        let temp_dir = tempfile::tempdir().expect("temp dir should be created");
        let model_path = temp_dir.path().join("jarvis.onnx");
        fs::write(&model_path, b"fixture").expect("model fixture should be written");

        let runtime = LocalTtsRuntime::new_with_engine_factory(
            LocalTtsRuntimeSpec {
                neural: None,
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
                neural: None,
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
                neural: None,
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
        let release_on_drop = ReleaseOnDrop(Arc::clone(&release));
        let engine_release = Arc::clone(&release);
        let runtime = Arc::new(
            LocalTtsRuntime::new_with_engine_factory(
                LocalTtsRuntimeSpec {
                    neural: None,
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
        let synthesis = spawn_test_worker(move || {
            synthesizing_runtime.synthesize_for_generation("hello", generation)
        });
        started_rx
            .recv_timeout(TEST_WAIT_BOUND)
            .expect("synthesis should reach the blocking engine");

        runtime.cancel_generation();
        release_on_drop.release();

        let error = synthesis
            .recv_timeout(TEST_WAIT_BOUND)
            .expect("synthesis caller should finish within the bound")
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
        let release_on_drop = ReleaseOnDrop(Arc::clone(&release));
        let factory_calls = Arc::new(AtomicUsize::new(0));
        let engine_release = Arc::clone(&release);
        let factory_call_count = Arc::clone(&factory_calls);
        let runtime = Arc::new(
            LocalTtsRuntime::new_with_engine_factory(
                LocalTtsRuntimeSpec {
                    neural: None,
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
            .recv_timeout(TEST_WAIT_BOUND)
            .expect("first synthesis should reach the blocking engine");

        runtime.cancel_generation();
        let first_error = first_result_rx
            .recv_timeout(std::time::Duration::from_millis(500))
            .expect("cancelled caller should settle promptly")
            .expect_err("cancelled synthesis must fail");
        assert_eq!(first_error, super::SYNTHESIS_CANCELLED);

        assert!(super::physical_work_pending());
        assert!(!super::wait_for_physical_retirement(Duration::from_millis(
            20
        )));

        let replacement_generation = runtime.start_generation();
        let error = runtime
            .synthesize_for_generation("replacement", replacement_generation)
            .expect_err("a second blocked cancellation must exhaust the worker budget");
        assert!(error.contains("abandoned synthesis budget exhausted"));

        let retirement = spawn_test_worker(|| super::wait_for_physical_retirement(TEST_WAIT_BOUND));
        release_on_drop.release();
        assert!(retirement.recv_timeout(TEST_WAIT_BOUND).unwrap());
        assert!(super::wait_for_physical_retirement(TEST_WAIT_BOUND));
        assert!(!super::physical_work_pending());
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
                    neural: None,
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
            spawn_test_worker(move || stale.synthesize_for_generation("stale", old_generation))
                .recv_timeout(TEST_WAIT_BOUND)
                .expect("stale request should finish within the bound")
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
        let release_on_drop = ReleaseOnDrop(Arc::clone(&release));
        let engine_release = Arc::clone(&release);
        let runtime = Arc::new(
            LocalTtsRuntime::new_with_engine_factory(
                LocalTtsRuntimeSpec {
                    neural: None,
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
            spawn_test_worker(move || first_runtime.synthesize_for_generation("first", generation));
        started_rx
            .recv_timeout(TEST_WAIT_BOUND)
            .expect("first synthesis should reach the blocking engine");

        let queued_runtime = Arc::clone(&runtime);
        let queued = spawn_test_worker(move || {
            queued_runtime.synthesize_for_generation("queued", generation)
        });
        runtime.cancel_generation();

        release_on_drop.release();

        assert_eq!(
            first
                .recv_timeout(TEST_WAIT_BOUND)
                .expect("first synthesis should finish within the bound")
                .unwrap_err(),
            super::SYNTHESIS_CANCELLED
        );
        assert_eq!(
            queued
                .recv_timeout(TEST_WAIT_BOUND)
                .expect("queued synthesis should finish within the bound")
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
                neural: None,
                model_path: Some(model_path),
                worker_count: 1,
                max_queue: 4,
                sample_rate_hz: 22_050,
                max_duration_s: 300,
                provider_policy: super::TtsProviderPolicy::Cuda,
            },
            Arc::new(|_, _, policy| {
                super::select_tts_provider(policy, |cuda| {
                    assert!(cuda, "explicit CUDA must not initialize CPU fallback");
                    Err(String::from(
                        "failed to initialize CUDA execution provider for local tts: unavailable",
                    ))
                })
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
                neural: None,
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
    fn physical_work_tracks_replacement_engine_factory() {
        struct FailingEngine;
        impl super::SynthesisEngine for FailingEngine {
            fn synthesize(&mut self, _: &str) -> Result<super::LocalTtsAudio, String> {
                Err("synthetic synthesis failure".into())
            }
        }

        let _test_runtime_guard = test_runtime_guard();
        let temp_dir = tempfile::tempdir().expect("temp dir should be created");
        let model_path = temp_dir.path().join("synthetic-voice.onnx");
        fs::write(&model_path, b"fixture").expect("model fixture should be written");
        let release = Arc::new((Mutex::new(false), Condvar::new()));
        let release_on_drop = ReleaseOnDrop(Arc::clone(&release));
        let (started_tx, started_rx) = mpsc::channel();
        let calls = AtomicUsize::new(0);
        let runtime = Arc::new(
            LocalTtsRuntime::new_with_engine_factory(
                LocalTtsRuntimeSpec {
                    neural: None,
                    model_path: Some(model_path),
                    worker_count: 1,
                    max_queue: 1,
                    sample_rate_hz: 22_050,
                    max_duration_s: 300,
                    provider_policy: super::TtsProviderPolicy::Cpu,
                },
                Arc::new(move |_, sample_rate_hz, _| {
                    if calls.fetch_add(1, std::sync::atomic::Ordering::AcqRel) == 0 {
                        return Ok((Box::new(FailingEngine), super::TtsActualProvider::Cpu));
                    }
                    let _ = started_tx.send(());
                    wait_for_release(&release)?;
                    Ok((
                        Box::new(FakeSynthesisEngine { sample_rate_hz }),
                        super::TtsActualProvider::Cpu,
                    ))
                }),
            )
            .unwrap(),
        );
        assert!(runtime.synthesize("first").is_err());
        let synthesizing_runtime = Arc::clone(&runtime);
        let result = spawn_test_worker(move || synthesizing_runtime.synthesize("replacement"));
        started_rx.recv_timeout(TEST_WAIT_BOUND).unwrap();
        assert!(!super::SYNTHESIS_PERMIT_OCCUPIED.load(std::sync::atomic::Ordering::Acquire));
        assert!(super::physical_work_pending());
        assert!(!super::wait_for_physical_retirement(Duration::ZERO));
        release_on_drop.release();
        result.recv_timeout(TEST_WAIT_BOUND).unwrap().unwrap();
        assert!(super::wait_for_physical_retirement(TEST_WAIT_BOUND));
    }

    #[test]
    fn physical_work_tracks_initialization_before_constructor_returns() {
        let _test_runtime_guard = test_runtime_guard();
        let temp_dir = tempfile::tempdir().expect("temp dir should be created");
        let model_path = temp_dir.path().join("synthetic-voice.onnx");
        fs::write(&model_path, b"fixture").expect("model fixture should be written");
        let release = Arc::new((Mutex::new(false), Condvar::new()));
        let release_on_drop = ReleaseOnDrop(Arc::clone(&release));
        let (started_tx, started_rx) = mpsc::channel();
        let result = spawn_test_worker(move || {
            LocalTtsRuntime::new_with_engine_factory(
                LocalTtsRuntimeSpec {
                    neural: None,
                    model_path: Some(model_path),
                    worker_count: 1,
                    max_queue: 1,
                    sample_rate_hz: 22_050,
                    max_duration_s: 300,
                    provider_policy: super::TtsProviderPolicy::Cpu,
                },
                Arc::new(move |_, sample_rate_hz, _| {
                    let _ = started_tx.send(());
                    wait_for_release(&release)?;
                    Ok((
                        Box::new(FakeSynthesisEngine { sample_rate_hz }),
                        super::TtsActualProvider::Cpu,
                    ))
                }),
            )
        });
        started_rx.recv_timeout(TEST_WAIT_BOUND).unwrap();
        assert!(super::physical_work_pending());
        assert!(!super::wait_for_physical_retirement(Duration::ZERO));
        release_on_drop.release();
        let runtime = result.recv_timeout(TEST_WAIT_BOUND).unwrap().unwrap();
        assert!(super::wait_for_physical_retirement(TEST_WAIT_BOUND));
        assert!(
            !super::physical_work_pending(),
            "idle workers are not physical work"
        );
        drop(runtime);
    }

    #[test]
    fn timed_out_initialization_is_owned_and_reaped_before_retry() {
        let _test_runtime_guard = test_runtime_guard();
        let temp_dir = tempfile::tempdir().expect("temp dir should be created");
        let model_path = temp_dir.path().join("jarvis.onnx");
        fs::write(&model_path, b"fixture").expect("model fixture should be written");
        let release = Arc::new((Mutex::new(false), Condvar::new()));
        let release_on_drop = ReleaseOnDrop(Arc::clone(&release));
        let calls = Arc::new(AtomicUsize::new(0));
        let factory_release = Arc::clone(&release);
        let factory_calls = Arc::clone(&calls);
        let factory: super::SynthesisEngineFactory = Arc::new(move |_, sample_rate_hz, _| {
            factory_calls.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
            wait_for_release(&factory_release)?;
            Ok((
                Box::new(FakeSynthesisEngine { sample_rate_hz }) as Box<dyn super::SynthesisEngine>,
                super::TtsActualProvider::Cpu,
            ))
        });
        let spec = || LocalTtsRuntimeSpec {
            neural: None,
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

        assert!(super::physical_work_pending());
        assert!(!super::wait_for_physical_retirement(Duration::from_millis(
            20
        )));
        release_on_drop.release();
        assert!(super::wait_for_physical_retirement(TEST_WAIT_BOUND));
        let deadline = Instant::now() + TEST_WAIT_BOUND;
        let third = loop {
            match LocalTtsRuntime::new_with_engine_factory(spec(), Arc::clone(&factory)) {
                Ok(runtime) => break runtime,
                Err(error) if error.contains("still pending") => {
                    assert!(
                        Instant::now() < deadline,
                        "initialization was not reaped within the bound"
                    );
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

    #[test]
    fn tolerates_a_slow_but_successful_local_voice_initialization() {
        let _test_runtime_guard = test_runtime_guard();
        let temp_dir = tempfile::tempdir().expect("temp dir should be created");
        let model_path = temp_dir.path().join("example-voice.onnx");
        fs::write(&model_path, b"fixture").expect("model fixture should be written");
        let runtime = LocalTtsRuntime::new_with_engine_factory(
            LocalTtsRuntimeSpec {
                neural: None,
                model_path: Some(model_path),
                worker_count: 1,
                max_queue: 1,
                sample_rate_hz: 22_050,
                max_duration_s: 300,
                provider_policy: super::TtsProviderPolicy::Cpu,
            },
            Arc::new(|_, sample_rate_hz, _| {
                thread::sleep(std::time::Duration::from_millis(2_400));
                Ok((
                    Box::new(FakeSynthesisEngine { sample_rate_hz })
                        as Box<dyn super::SynthesisEngine>,
                    super::TtsActualProvider::Cpu,
                ))
            }),
        )
        .expect("local voice initialization should tolerate a short slow start");
        runtime.synthesize("Yes?").expect("voice should synthesize");
    }

    #[test]
    fn runtime_validates_canonical_tts_resource_limits() {
        let _test_runtime_guard = test_runtime_guard();
        let temp_dir = tempfile::tempdir().expect("temp dir should be created");
        let model_path = temp_dir.path().join("jarvis.onnx");
        fs::write(&model_path, b"fixture").expect("model fixture should be written");

        for (worker_count, max_queue, sample_rate_hz, max_duration_s, expected_error) in [
            (
                0,
                8,
                22_050,
                300,
                "tts worker_count must be greater than zero",
            ),
            (
                3,
                8,
                22_050,
                300,
                "tts worker_count must be between 1 and 2 inclusive",
            ),
            (1, 0, 22_050, 300, "tts max_queue must be greater than zero"),
            (
                1,
                17,
                22_050,
                300,
                "tts max_queue must be between 1 and 16 inclusive",
            ),
            (1, 8, 0, 300, "tts sample_rate_hz must be greater than zero"),
            (
                1,
                8,
                22_050,
                0,
                "tts max_duration_s must be greater than zero",
            ),
            (
                1,
                8,
                22_050,
                361,
                "tts max_duration_s must be between 1 and 360 inclusive",
            ),
        ] {
            let error = LocalTtsRuntime::new_with_engine_factory(
                LocalTtsRuntimeSpec {
                    neural: None,
                    model_path: Some(model_path.clone()),
                    worker_count,
                    max_queue,
                    sample_rate_hz,
                    max_duration_s,
                    provider_policy: super::TtsProviderPolicy::Cpu,
                },
                Arc::new(fake_engine_factory),
            )
            .expect_err("invalid TTS resource configuration must be rejected");
            assert_eq!(error, expected_error);
        }

        for (worker_count, max_queue, sample_rate_hz, max_duration_s) in [
            (2, 16, 24_000, 360),
            (1, 8, 44_100, 360),
            (2, 16, 48_000, 300),
            (2, 16, 48_000, 360),
        ] {
            let runtime = LocalTtsRuntime::new_with_engine_factory(
                LocalTtsRuntimeSpec {
                    neural: None,
                    model_path: Some(model_path.clone()),
                    worker_count,
                    max_queue,
                    sample_rate_hz,
                    max_duration_s,
                    provider_policy: super::TtsProviderPolicy::Cpu,
                },
                Arc::new(fake_engine_factory),
            )
            .expect("valid high-rate TTS resource configuration should initialize");
            assert_eq!(runtime.sample_rate_hz(), sample_rate_hz);
        }
    }

    #[test]
    fn pre_copy_output_bounds_reject_empty_over_copy_limit_and_over_duration_before_allocation() {
        let limits_1s =
            voxgolem_core::config::validate_tts_resource_limits(1, 8, 22_050, 1).unwrap();
        assert_eq!(
            super::decode_audio_from_samples(&[], limits_1s),
            Err(String::from(
                "local tts model produced an empty audio buffer"
            ))
        );

        // Exceeds configured 1s duration (22_051 > 22_050 samples) before copy.
        let over_duration = vec![0.25_f32; 22_051];
        let error = super::decode_audio_from_samples(&over_duration, limits_1s)
            .expect_err("samples exceeding configured duration must be rejected");
        assert!(error.contains("exceeds configured max"));

        // 48kHz * 360s = 17_280_000 samples is within configured duration, but exceeds
        // the independent 64 MiB (16_777_216 f32 samples) per-result Rust copy limit.
        let limits_48k_360s =
            voxgolem_core::config::validate_tts_resource_limits(2, 16, 48_000, 360).unwrap();
        let copy_limit_error =
            super::validate_synthesized_sample_bounds(17_280_000, limits_48k_360s).expect_err(
                "samples exceeding 64 MiB per-result copy limit must be rejected before copy",
            );
        assert!(copy_limit_error.contains("exceeds max copy limit"));

        let valid = super::decode_audio_from_samples(&[-1.5, 0.25, 1.5], limits_1s)
            .expect("bounded slice should copy and normalize");
        assert_eq!(valid.pcm_f32, vec![-1.0, 0.25, 1.0]);
        assert_eq!(valid.sample_rate_hz, 22_050);
    }

    #[test]
    fn partial_worker_spawn_failure_retains_started_initializer_and_blocks_retry_until_retired() {
        let _test_runtime_guard = test_runtime_guard();
        let temp_dir = tempfile::tempdir().expect("temp dir should be created");
        let model_path = temp_dir.path().join("jarvis.onnx");
        fs::write(&model_path, b"fixture").expect("model fixture should be written");

        let (started_tx, started_rx) = mpsc::channel();
        let release = Arc::new((Mutex::new(false), Condvar::new()));
        let release_on_drop = ReleaseOnDrop(Arc::clone(&release));
        let engine_release = Arc::clone(&release);
        let factory_calls = Arc::new(AtomicUsize::new(0));
        let factory_call_count = Arc::clone(&factory_calls);
        let factory: super::SynthesisEngineFactory = Arc::new(move |_, sample_rate_hz, _| {
            if factory_call_count.fetch_add(1, std::sync::atomic::Ordering::AcqRel) == 0 {
                let _ = started_tx.send(());
                wait_for_release(&engine_release)?;
            }
            Ok((
                Box::new(FakeSynthesisEngine { sample_rate_hz }),
                super::TtsActualProvider::Cpu,
            ))
        });

        let spawn_attempts = Arc::new(AtomicUsize::new(0));
        let worker_attempts = Arc::clone(&spawn_attempts);
        let failing_second_worker_spawner: super::ThreadSpawner = Arc::new(move |name, work| {
            if worker_attempts.fetch_add(1, std::sync::atomic::Ordering::AcqRel) == 0 {
                super::default_thread_spawner(name, work)
            } else {
                Err(std::io::Error::new(
                    std::io::ErrorKind::WouldBlock,
                    "synthetic worker thread spawn failure",
                ))
            }
        });

        let spec = || LocalTtsRuntimeSpec {
            neural: None,
            model_path: Some(model_path.clone()),
            worker_count: 2,
            max_queue: 2,
            sample_rate_hz: 22_050,
            max_duration_s: 300,
            provider_policy: super::TtsProviderPolicy::Cpu,
        };

        let error = LocalTtsRuntime::new_with_engine_factory_and_spawners(
            spec(),
            Arc::clone(&factory),
            failing_second_worker_spawner,
            Arc::new(super::default_thread_spawner),
        )
        .expect_err("partial worker spawn failure must fail runtime construction");
        assert!(error.contains("failed to spawn local tts worker thread"));

        started_rx
            .recv_timeout(TEST_WAIT_BOUND)
            .expect("worker 0 should have started engine initialization");
        assert!(
            super::physical_work_pending(),
            "started worker 0 must keep physical initialization work pending"
        );
        let retry_while_pending =
            LocalTtsRuntime::new_with_engine_factory(spec(), Arc::clone(&factory))
                .expect_err("retained worker 0 must block retry while still initializing");
        assert!(retry_while_pending.contains("still pending"));

        release_on_drop.release();
        assert!(super::wait_for_physical_retirement(TEST_WAIT_BOUND));
        let deadline = Instant::now() + TEST_WAIT_BOUND;
        let recovered = loop {
            match LocalTtsRuntime::new_with_engine_factory(spec(), Arc::clone(&factory)) {
                Ok(runtime) => break runtime,
                Err(error) if error.contains("still pending") => {
                    assert!(
                        Instant::now() < deadline,
                        "retained worker 0 was not reaped within the bound"
                    );
                    thread::sleep(Duration::from_millis(10));
                }
                Err(error) => panic!("retry after worker 0 retirement should succeed: {error}"),
            }
        };
        recovered
            .synthesize("recovered")
            .expect("recovered runtime should synthesize");
    }

    #[test]
    fn synthesis_runner_spawn_failure_recovers_engine_and_permit_for_subsequent_request() {
        let _test_runtime_guard = test_runtime_guard();
        let temp_dir = tempfile::tempdir().expect("temp dir should be created");
        let model_path = temp_dir.path().join("jarvis.onnx");
        fs::write(&model_path, b"fixture").expect("model fixture should be written");

        let factory_calls = Arc::new(AtomicUsize::new(0));
        let factory_call_count = Arc::clone(&factory_calls);
        let factory: super::SynthesisEngineFactory = Arc::new(move |_, sample_rate_hz, _| {
            factory_call_count.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
            Ok((
                Box::new(FakeSynthesisEngine { sample_rate_hz }),
                super::TtsActualProvider::Cpu,
            ))
        });

        let runner_attempts = Arc::new(AtomicUsize::new(0));
        let runner_attempt_count = Arc::clone(&runner_attempts);
        let flaky_runner_spawner: super::ThreadSpawner = Arc::new(move |name, work| {
            if runner_attempt_count.fetch_add(1, std::sync::atomic::Ordering::AcqRel) == 0 {
                drop(work);
                Err(std::io::Error::new(
                    std::io::ErrorKind::WouldBlock,
                    "synthetic runner spawn failure",
                ))
            } else {
                super::default_thread_spawner(name, work)
            }
        });

        let runtime = LocalTtsRuntime::new_with_engine_factory_and_spawners(
            LocalTtsRuntimeSpec {
                neural: None,
                model_path: Some(model_path),
                worker_count: 1,
                max_queue: 2,
                sample_rate_hz: 22_050,
                max_duration_s: 300,
                provider_policy: super::TtsProviderPolicy::Cpu,
            },
            factory,
            Arc::new(super::default_thread_spawner),
            flaky_runner_spawner,
        )
        .expect("runtime should initialize");

        let error = runtime
            .synthesize("first")
            .expect_err("failed runner spawn must report an error");
        assert!(error.contains("failed to spawn local tts synthesis runner"));
        // The error response may arrive just before the worker drops its job lease.
        // Caller completion does not certify physical retirement.
        assert!(super::wait_for_physical_retirement(TEST_WAIT_BOUND));
        assert!(!super::SYNTHESIS_PERMIT_OCCUPIED.load(std::sync::atomic::Ordering::Acquire));
        assert!(!super::physical_work_pending());

        let audio = runtime
            .synthesize("second")
            .expect("second synthesis should reuse the recovered engine");
        assert!(!audio.pcm_f32.is_empty());
        assert_eq!(
            factory_calls.load(std::sync::atomic::Ordering::Acquire),
            1,
            "engine must be recovered from handoff slot without calling factory again"
        );
    }

    #[test]
    fn poisoned_registries_still_retain_and_reap_handles() {
        let _test_runtime_guard = test_runtime_guard();
        let pending_init = super::PENDING_INITIALIZATIONS.get_or_init(|| Mutex::new(Vec::new()));
        let abandoned_synth = super::ABANDONED_SYNTHESIS.get_or_init(|| Mutex::new(Vec::new()));
        let _ = std::panic::catch_unwind(|| {
            let _guard = pending_init.lock().unwrap();
            panic!("synthetic poison for PENDING_INITIALIZATIONS");
        });
        let _ = std::panic::catch_unwind(|| {
            let _guard = abandoned_synth.lock().unwrap();
            panic!("synthetic poison for ABANDONED_SYNTHESIS");
        });

        let release = Arc::new((Mutex::new(false), Condvar::new()));
        let release_on_drop = ReleaseOnDrop(Arc::clone(&release));
        let init_release = Arc::clone(&release);
        let synth_release = Arc::clone(&release);
        let init_handle = thread::spawn(move || {
            let _ = wait_for_release(&init_release);
        });
        let synth_handle = thread::spawn(move || {
            let _ = wait_for_release(&synth_release);
        });

        super::retain_pending_initializations(vec![init_handle]);
        super::retain_abandoned_synthesis(synth_handle);
        assert!(super::reap_pending_initializations());
        assert!(super::reap_abandoned_synthesis());

        release_on_drop.release();
        let deadline = Instant::now() + TEST_WAIT_BOUND;
        while super::reap_pending_initializations() || super::reap_abandoned_synthesis() {
            assert!(
                Instant::now() < deadline,
                "poisoned registries did not reap finished handles within bound"
            );
            thread::sleep(Duration::from_millis(10));
        }
    }
}
