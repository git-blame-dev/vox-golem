use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use voxgolem_platform::neural_tts::{Cancellation, NeuralTtsClient, NeuralTtsOptions};

#[derive(Debug, Clone)]
pub struct LocalTtsRuntimeSpec {
    pub options: NeuralTtsOptions,
    pub voice: String,
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
    client: Arc<NeuralTtsClient>,
    neural_voice: Mutex<String>,
    neural_default_voice: String,
    cue_epoch: u64,
    admission: SynthesisAdmission,
    limits: voxgolem_core::config::ValidatedTtsAudioLimits,
}

static NEXT_NEURAL_RUNTIME: AtomicU64 = AtomicU64::new(1);
const SYNTHESIS_CANCELLED: &str = "local tts synthesis request was cancelled or superseded";
const MAX_ACTIVE_REQUESTS: usize = 16;

/// Stop and request registration share one transaction. Weak tokens bound admission
/// without retaining completed requests; cancelled requests keep their slot until
/// their actual caller releases the token.
#[derive(Debug, Default)]
struct SynthesisAdmission {
    state: Mutex<AdmissionState>,
}

#[derive(Debug, Default)]
struct AdmissionState {
    generation: u64,
    closed: bool,
    tokens: Vec<std::sync::Weak<Cancellation>>,
}

impl SynthesisAdmission {
    fn advance(&self, close: bool) -> u64 {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        state.closed |= close;
        state.generation = state.generation.saturating_add(1);
        state.tokens.retain(|token| {
            if let Some(token) = token.upgrade() {
                token.cancel();
                true
            } else {
                false
            }
        });
        state.generation
    }

    fn generation(&self) -> u64 {
        self.state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .generation
    }

    fn register(&self, expected_generation: u64) -> Result<Arc<Cancellation>, String> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| String::from("Neural TTS cancellation state unavailable"))?;
        if state.generation != expected_generation {
            return Err(String::from(SYNTHESIS_CANCELLED));
        }
        state.tokens.retain(|token| token.strong_count() != 0);
        if state.closed || state.tokens.len() >= MAX_ACTIVE_REQUESTS {
            return Err(String::from("Neural TTS admission unavailable"));
        }
        let token = Arc::new(Cancellation::default());
        state.tokens.push(Arc::downgrade(&token));
        Ok(token)
    }
}

static PHYSICAL_WORK: Mutex<usize> = Mutex::new(0);
static PHYSICAL_WORK_RETIRED: Condvar = Condvar::new();

/// Snapshot of client construction, cue synthesis, and owned native output sessions.
/// Network completion does not establish remote inference or acoustic retirement.
/// This does not close admission: callers must hold their lifecycle/update gate to
/// prevent new constructors or synthesis requests racing an installation idle check.
pub(crate) fn physical_work_pending() -> bool {
    match PHYSICAL_WORK.lock() {
        Ok(work) => *work != 0,
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
    match PHYSICAL_WORK_RETIRED.wait_timeout_while(work, timeout, |work| *work != 0) {
        Ok((work, _)) => *work == 0,
        Err(_) => false,
    }
}

/// Retained by the owner until its operation and native output drain have returned.
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

impl LocalTtsRuntime {
    pub fn new(spec: LocalTtsRuntimeSpec) -> Result<Self, String> {
        let _construction_work = PhysicalWork::begin();
        let client = tauri::async_runtime::block_on(NeuralTtsClient::connect(&spec.options))
            .map_err(|error| error.to_string())?;
        let voice = client
            .capabilities()
            .resolve_voice(&spec.voice)
            .ok_or_else(|| String::from("configured Neural TTS voice is unavailable"))?
            .id
            .clone();
        let limits = voxgolem_core::config::validate_tts_audio_limits(
            client.capabilities().sample_rate_hz,
            spec.max_duration_s,
        )
        .map_err(|error| error.format_with_prefix("tts "))?;
        Ok(Self {
            client: Arc::new(client),
            neural_default_voice: voice.clone(),
            neural_voice: Mutex::new(voice),
            cue_epoch: NEXT_NEURAL_RUNTIME.fetch_add(1, Ordering::Relaxed),
            admission: SynthesisAdmission::default(),
            limits,
        })
    }

    pub fn sample_rate_hz(&self) -> u32 {
        self.limits.sample_rate_hz
    }
    pub(crate) fn maximum_samples(&self) -> usize {
        self.limits
            .max_duration_samples
            .min(voxgolem_core::config::MAX_TTS_RESULT_SAMPLES)
    }
    pub fn start_generation(&self) -> u64 {
        self.admission.advance(false)
    }
    pub fn cancel_generation(&self) {
        self.admission.advance(false);
    }
    pub(crate) fn current_generation(&self) -> u64 {
        self.admission.generation()
    }

    pub fn shutdown_bounded(&mut self) {
        self.admission.advance(true);
        self.client.close();
    }

    pub fn shutdown_owned(&mut self) {
        self.shutdown_bounded();
    }

    pub(crate) fn synthesize_cue(
        &self,
        text: &str,
        voice: &str,
        generation: u64,
    ) -> Result<LocalTtsAudio, String> {
        let _work = PhysicalWork::begin();
        let (cancellation, _) = self.neural_request(generation)?;
        let audio = tauri::async_runtime::block_on(self.client.collect_background(
            text,
            voice,
            cancellation,
        ))
        .map_err(|error| error.to_string())?;
        if generation != self.current_generation() {
            return Err(String::from(SYNTHESIS_CANCELLED));
        }
        let duration_ms = audio.samples.len() as u64 * 1000 / u64::from(audio.sample_rate_hz);
        if audio.samples.len() > self.maximum_samples() || duration_ms > self.limits.max_duration_ms
        {
            return Err(String::from("Neural TTS audio exceeds configured duration"));
        }
        Ok(LocalTtsAudio {
            pcm_f32: audio.samples,
            sample_rate_hz: audio.sample_rate_hz,
            duration_ms,
        })
    }

    pub(crate) fn neural_client(&self) -> Arc<NeuralTtsClient> {
        Arc::clone(&self.client)
    }

    pub(crate) fn selected_neural_voice(&self) -> Option<String> {
        self.neural_voice.lock().ok().map(|voice| voice.clone())
    }

    pub(crate) fn neural_request(
        &self,
        expected_generation: u64,
    ) -> Result<(Arc<Cancellation>, String), String> {
        let token = self.admission.register(expected_generation)?;
        let voice = self
            .neural_voice
            .lock()
            .map_err(|_| String::from("Neural TTS voice state unavailable"))?
            .clone();
        Ok((token, voice))
    }

    pub(crate) fn set_neural_voice(&self, requested: &str) -> Result<(), String> {
        let requested = if requested.is_empty() {
            &self.neural_default_voice
        } else {
            requested
        };
        let voice = self
            .client
            .capabilities()
            .resolve_voice(requested)
            .ok_or_else(|| String::from("selected Neural TTS voice is unavailable"))?;
        *self
            .neural_voice
            .lock()
            .map_err(|_| String::from("Neural TTS voice state unavailable"))? = voice.id.clone();
        Ok(())
    }

    pub(crate) fn cue_key(&self) -> String {
        self.cue_key_for_voice(&self.selected_neural_voice().unwrap_or_default())
    }

    pub(crate) fn cue_key_for_voice(&self, voice: &str) -> String {
        format!("neural:{}:{}", self.cue_epoch, voice)
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

#[cfg(test)]
pub(super) fn test_runtime_guard() -> impl Drop {
    tests::test_runtime_guard()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    const TEST_WAIT_BOUND: Duration = Duration::from_secs(5);
    static TTS_RUNTIME_TEST_LOCK: Mutex<()> = Mutex::new(());
    pub(super) struct TestRuntimeGuard(std::sync::MutexGuard<'static, ()>);
    impl Drop for TestRuntimeGuard {
        fn drop(&mut self) {
            let retired = wait_for_physical_retirement(TEST_WAIT_BOUND);
            if !thread::panicking() {
                assert!(retired, "test physical work did not retire");
            }
        }
    }
    pub(super) fn test_runtime_guard() -> TestRuntimeGuard {
        TestRuntimeGuard(
            TTS_RUNTIME_TEST_LOCK
                .lock()
                .unwrap_or_else(|error| error.into_inner()),
        )
    }

    #[test]
    fn stop_between_generation_check_and_token_registration_rejects_stale_admission() {
        let admission = Arc::new(SynthesisAdmission::default());
        let old_generation = admission.advance(false);
        let gate = Arc::new(std::sync::Barrier::new(2));
        let delayed_admission = Arc::clone(&admission);
        let delayed_gate = Arc::clone(&gate);
        let delayed = thread::spawn(move || {
            delayed_gate.wait();
            delayed_admission.register(old_generation)
        });
        admission.advance(false);
        gate.wait();
        assert_eq!(delayed.join().unwrap().unwrap_err(), SYNTHESIS_CANCELLED);
        // The remaining fixed cue batch carries the same frozen generation.
        assert_eq!(
            admission.register(old_generation).unwrap_err(),
            SYNTHESIS_CANCELLED
        );
        let next_generation = admission.advance(false);
        let token = admission.register(next_generation).unwrap();
        admission.advance(false);
        assert!(
            token.is_cancelled(),
            "already registered work must observe Stop"
        );
    }

    #[test]
    fn cancelled_work_keeps_bounded_admission_until_tokens_retire() {
        let admission = SynthesisAdmission::default();
        let generation = admission.generation();
        let tokens: Vec<_> = (0..MAX_ACTIVE_REQUESTS)
            .map(|_| admission.register(generation).unwrap())
            .collect();
        assert!(admission.register(generation).is_err());
        let replacement = admission.advance(false);
        assert!(tokens.iter().all(|token| token.is_cancelled()));
        assert!(admission.register(replacement).is_err());
        drop(tokens);
        assert!(admission.register(replacement).is_ok());
    }

    #[test]
    fn shutdown_closes_admission_even_for_current_generation() {
        let admission = SynthesisAdmission::default();
        let token = admission.register(admission.generation()).unwrap();
        let closed = admission.advance(true);
        assert!(token.is_cancelled());
        assert!(admission.register(closed).is_err());
        assert!(admission.register(admission.advance(false)).is_err());
    }

    #[test]
    fn physical_work_remains_pending_after_logical_cancellation_until_owner_drops() {
        let _serial = test_runtime_guard();
        let admission = SynthesisAdmission::default();
        let work = PhysicalWork::begin();
        let token = admission.register(admission.generation()).unwrap();
        admission.advance(false);
        assert!(token.is_cancelled());
        assert!(physical_work_pending());
        assert!(!wait_for_physical_retirement(Duration::ZERO));
        drop(work);
        assert!(wait_for_physical_retirement(TEST_WAIT_BOUND));
        assert!(!physical_work_pending());
    }
}
