use std::fmt;
use std::num::{NonZeroU16, NonZeroU32};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use rodio::buffer::SamplesBuffer;
use rodio::cpal::BufferSize;
use rodio::{DeviceSinkBuilder, MixerDeviceSink, Player};

const MAX_PLAYBACK_ID: u64 = 9_007_199_254_740_991;
const PLAYBACK_POLL_INTERVAL: Duration = Duration::from_millis(10);
const PLAYBACK_DRAIN_GRACE: Duration = Duration::from_secs(5);
const OUTPUT_BUFFER_FALLBACK: Duration = Duration::from_millis(100);

#[derive(Debug, Clone, PartialEq)]
pub struct PlaybackRequest {
    pub playback_id: u64,
    pub pcm_f32: Vec<f32>,
    pub sample_rate_hz: u32,
    pub gain_db: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlaybackCompletion {
    pub playback_id: u64,
    pub duration_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlaybackError {
    InvalidPlaybackId,
    EmptyAudio,
    InvalidSampleRate,
    InvalidGain,
    InvalidSample,
    Superseded,
    Suspended,
    ShuttingDown,
    OutputUnavailable(String),
    OutputFailed(String),
    TimedOut,
    StatePoisoned,
}

impl fmt::Display for PlaybackError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidPlaybackId => write!(formatter, "audio playback id is invalid"),
            Self::EmptyAudio => write!(formatter, "audio playback buffer must not be empty"),
            Self::InvalidSampleRate => {
                write!(
                    formatter,
                    "audio playback sample rate must be greater than zero"
                )
            }
            Self::InvalidGain => write!(formatter, "audio playback gain must be finite"),
            Self::InvalidSample => write!(formatter, "audio playback samples must be finite"),
            Self::Superseded => write!(formatter, "audio playback was cancelled or superseded"),
            Self::Suspended => write!(formatter, "audio playback service is suspended"),
            Self::ShuttingDown => write!(formatter, "audio playback service is shutting down"),
            Self::OutputUnavailable(details) => {
                write!(formatter, "audio output is unavailable: {details}")
            }
            Self::OutputFailed(details) => write!(formatter, "audio output failed: {details}"),
            Self::TimedOut => write!(formatter, "audio playback timed out"),
            Self::StatePoisoned => write!(formatter, "audio playback state lock is poisoned"),
        }
    }
}

impl std::error::Error for PlaybackError {}

pub struct AudioPlaybackService {
    operation_lock: Mutex<()>,
    state: Mutex<PlaybackState>,
    backend: Arc<dyn PlaybackBackend>,
    suspended: AtomicBool,
    shutting_down: AtomicBool,
}

#[derive(Default)]
struct PlaybackState {
    latest_id: u64,
    current: Option<CurrentPlayback>,
}

struct CurrentPlayback {
    playback_id: u64,
    playback: Arc<dyn ActivePlayback>,
}

trait PlaybackBackend: Send + Sync {
    fn start(
        &self,
        pcm_f32: Vec<f32>,
        sample_rate_hz: u32,
        linear_gain: f32,
    ) -> Result<Arc<dyn ActivePlayback>, PlaybackError>;

    fn release(&self);
}

trait ActivePlayback: Send + Sync {
    fn activate(&self) -> Result<(), PlaybackError>;
    fn stop(&self);
    fn wait(&self, timeout: Duration) -> Result<(), PlaybackError>;
}

impl AudioPlaybackService {
    pub fn new() -> Self {
        Self::with_backend(Arc::new(RodioBackend))
    }

    fn with_backend(backend: Arc<dyn PlaybackBackend>) -> Self {
        Self {
            operation_lock: Mutex::new(()),
            state: Mutex::new(PlaybackState::default()),
            backend,
            suspended: AtomicBool::new(false),
            shutting_down: AtomicBool::new(false),
        }
    }

    pub fn play(&self, request: PlaybackRequest) -> Result<PlaybackCompletion, PlaybackError> {
        validate_request(&request)?;
        if self.shutting_down.load(Ordering::Acquire) {
            return Err(PlaybackError::ShuttingDown);
        }
        if self.suspended.load(Ordering::Acquire) {
            return Err(PlaybackError::Suspended);
        }
        let duration_ms = duration_ms(request.pcm_f32.len(), request.sample_rate_hz);
        let timeout = Duration::from_millis(duration_ms).saturating_add(PLAYBACK_DRAIN_GRACE);
        let linear_gain = 10_f32.powf(request.gain_db / 20.0);

        let playback = {
            let _operation_guard = self
                .operation_lock
                .lock()
                .map_err(|_| PlaybackError::StatePoisoned)?;
            let previous = {
                let mut state = self
                    .state
                    .lock()
                    .map_err(|_| PlaybackError::StatePoisoned)?;
                if self.shutting_down.load(Ordering::Acquire) {
                    return Err(PlaybackError::ShuttingDown);
                }
                if self.suspended.load(Ordering::Acquire) {
                    return Err(PlaybackError::Suspended);
                }
                if request.playback_id < state.latest_id
                    || (request.playback_id == state.latest_id && state.current.is_some())
                {
                    return Err(PlaybackError::Superseded);
                }
                state.latest_id = request.playback_id;
                state.current.take()
            };
            if let Some(previous) = previous {
                previous.playback.stop();
            }

            let playback =
                self.backend
                    .start(request.pcm_f32, request.sample_rate_hz, linear_gain)?;
            if self.shutting_down.load(Ordering::Acquire) {
                playback.stop();
                self.backend.release();
                return Err(PlaybackError::ShuttingDown);
            }
            if self.suspended.load(Ordering::Acquire) {
                playback.stop();
                self.backend.release();
                return Err(PlaybackError::Suspended);
            }
            let mut state = self
                .state
                .lock()
                .map_err(|_| PlaybackError::StatePoisoned)?;
            if request.playback_id < state.latest_id {
                playback.stop();
                return Err(PlaybackError::Superseded);
            }
            state.current = Some(CurrentPlayback {
                playback_id: request.playback_id,
                playback: Arc::clone(&playback),
            });
            if self.shutting_down.load(Ordering::Acquire) {
                playback.stop();
                state.current = None;
                self.backend.release();
                return Err(PlaybackError::ShuttingDown);
            }
            if self.suspended.load(Ordering::Acquire) {
                playback.stop();
                state.current = None;
                self.backend.release();
                return Err(PlaybackError::Suspended);
            }
            if let Err(error) = playback.activate() {
                state.current = None;
                self.backend.release();
                return Err(error);
            }
            playback
        };

        let wait_result = playback.wait(timeout);
        let is_current = {
            let mut state = self
                .state
                .lock()
                .map_err(|_| PlaybackError::StatePoisoned)?;
            if state
                .current
                .as_ref()
                .is_some_and(|current| current.playback_id == request.playback_id)
            {
                state.current = None;
                true
            } else {
                false
            }
        };
        if !is_current {
            return Err(PlaybackError::Superseded);
        }
        wait_result?;

        Ok(PlaybackCompletion {
            playback_id: request.playback_id,
            duration_ms,
        })
    }

    pub fn cancel(&self, playback_id: u64) -> Result<bool, PlaybackError> {
        validate_playback_id(playback_id)?;
        let playback = {
            let mut state = self
                .state
                .lock()
                .map_err(|_| PlaybackError::StatePoisoned)?;
            state.latest_id = state.latest_id.max(playback_id.saturating_add(1));
            if state
                .current
                .as_ref()
                .is_some_and(|current| current.playback_id == playback_id)
            {
                state.current.take().map(|current| current.playback)
            } else {
                None
            }
        };
        if let Some(playback) = playback {
            playback.stop();
            Ok(true)
        } else {
            Ok(false)
        }
    }

    pub fn suspend(&self) -> Result<(), PlaybackError> {
        self.suspended.store(true, Ordering::Release);
        let current = self
            .state
            .lock()
            .map_err(|_| PlaybackError::StatePoisoned)?
            .current
            .take();
        if let Some(current) = current {
            current.playback.stop();
        }
        if let Ok(_operation_guard) = self.operation_lock.try_lock() {
            self.backend.release();
        }
        Ok(())
    }

    pub fn resume(&self) -> Result<(), PlaybackError> {
        if self.shutting_down.load(Ordering::Acquire) {
            return Err(PlaybackError::ShuttingDown);
        }
        self.suspended.store(false, Ordering::Release);
        Ok(())
    }

    pub fn shutdown(&self) {
        if self.shutting_down.swap(true, Ordering::AcqRel) {
            return;
        }
        self.suspended.store(true, Ordering::Release);
        if let Ok(mut state) = self.state.lock() {
            if let Some(current) = state.current.take() {
                current.playback.stop();
            }
        }
        if let Ok(_operation_guard) = self.operation_lock.try_lock() {
            self.backend.release();
        }
    }
}

impl Default for AudioPlaybackService {
    fn default() -> Self {
        Self::new()
    }
}

fn validate_request(request: &PlaybackRequest) -> Result<(), PlaybackError> {
    validate_playback_id(request.playback_id)?;
    if request.pcm_f32.is_empty() {
        return Err(PlaybackError::EmptyAudio);
    }
    if request.sample_rate_hz == 0 {
        return Err(PlaybackError::InvalidSampleRate);
    }
    if !request.gain_db.is_finite() {
        return Err(PlaybackError::InvalidGain);
    }
    if request.pcm_f32.iter().any(|sample| !sample.is_finite()) {
        return Err(PlaybackError::InvalidSample);
    }
    Ok(())
}

fn validate_playback_id(playback_id: u64) -> Result<(), PlaybackError> {
    if playback_id == 0 || playback_id > MAX_PLAYBACK_ID {
        Err(PlaybackError::InvalidPlaybackId)
    } else {
        Ok(())
    }
}

fn duration_ms(sample_count: usize, sample_rate_hz: u32) -> u64 {
    (u64::try_from(sample_count)
        .unwrap_or(u64::MAX)
        .saturating_mul(1_000)
        / u64::from(sample_rate_hz))
    .max(1)
}

fn output_drain_duration(buffer_size: &BufferSize, sample_rate_hz: u32) -> Duration {
    let buffer_duration = match buffer_size {
        BufferSize::Fixed(frames) => {
            let nanoseconds = u64::from(*frames)
                .saturating_mul(1_000_000_000)
                .saturating_add(u64::from(sample_rate_hz).saturating_sub(1))
                / u64::from(sample_rate_hz);
            Duration::from_nanos(nanoseconds)
        }
        BufferSize::Default => OUTPUT_BUFFER_FALLBACK,
    };
    buffer_duration.saturating_add(PLAYBACK_POLL_INTERVAL)
}

fn wait_for_output_drain(
    cancelled: &AtomicBool,
    output_failed: &AtomicBool,
    timeout_deadline: Instant,
    drain_duration: Duration,
) -> Result<(), PlaybackError> {
    let drain_deadline = Instant::now() + drain_duration;
    loop {
        if cancelled.load(Ordering::Acquire) {
            return Err(PlaybackError::Superseded);
        }
        if output_failed.load(Ordering::Acquire) {
            return Err(PlaybackError::OutputFailed(String::from(
                "the active output stream stopped unexpectedly",
            )));
        }
        let now = Instant::now();
        if now >= drain_deadline {
            return Ok(());
        }
        if now >= timeout_deadline {
            return Err(PlaybackError::TimedOut);
        }
        thread::sleep(PLAYBACK_POLL_INTERVAL.min(drain_deadline.saturating_duration_since(now)));
    }
}

struct RodioBackend;

struct RodioOutput {
    sink: MixerDeviceSink,
    failed: Arc<AtomicBool>,
}

impl PlaybackBackend for RodioBackend {
    fn start(
        &self,
        pcm_f32: Vec<f32>,
        sample_rate_hz: u32,
        linear_gain: f32,
    ) -> Result<Arc<dyn ActivePlayback>, PlaybackError> {
        let output = open_default_output()?;
        let channels = NonZeroU16::new(1).expect("one channel is non-zero");
        let sample_rate =
            NonZeroU32::new(sample_rate_hz).ok_or(PlaybackError::InvalidSampleRate)?;
        let drain_duration = output_drain_duration(
            output.sink.config().buffer_size(),
            output.sink.config().sample_rate().get(),
        );
        let player = Player::connect_new(output.sink.mixer());
        player.pause();
        player.set_volume(linear_gain);
        player.append(SamplesBuffer::new(channels, sample_rate, pcm_f32));

        Ok(Arc::new(RodioPlayback {
            player,
            _output_sink: output.sink,
            cancelled: AtomicBool::new(false),
            output_failed: output.failed,
            drain_duration,
        }))
    }

    fn release(&self) {}
}

fn open_default_output() -> Result<RodioOutput, PlaybackError> {
    let failed = Arc::new(AtomicBool::new(false));
    let failure_flag = Arc::clone(&failed);
    let mut sink = DeviceSinkBuilder::from_default_device()
        .map_err(|error| PlaybackError::OutputUnavailable(error.to_string()))?
        .with_error_callback(move |_| failure_flag.store(true, Ordering::Release))
        .open_sink_or_fallback()
        .map_err(|error| PlaybackError::OutputUnavailable(error.to_string()))?;
    sink.log_on_drop(false);
    Ok(RodioOutput { sink, failed })
}

struct RodioPlayback {
    player: Player,
    _output_sink: MixerDeviceSink,
    cancelled: AtomicBool,
    output_failed: Arc<AtomicBool>,
    drain_duration: Duration,
}

impl ActivePlayback for RodioPlayback {
    fn activate(&self) -> Result<(), PlaybackError> {
        if self.cancelled.load(Ordering::Acquire) {
            return Err(PlaybackError::Superseded);
        }
        if self.output_failed.load(Ordering::Acquire) {
            return Err(PlaybackError::OutputFailed(String::from(
                "the active output stream stopped unexpectedly",
            )));
        }
        self.player.play();
        Ok(())
    }

    fn stop(&self) {
        self.cancelled.store(true, Ordering::Release);
        self.player.stop();
    }

    fn wait(&self, timeout: Duration) -> Result<(), PlaybackError> {
        let deadline = Instant::now() + timeout;
        loop {
            if self.cancelled.load(Ordering::Acquire) {
                return Err(PlaybackError::Superseded);
            }
            if self.output_failed.load(Ordering::Acquire) {
                return Err(PlaybackError::OutputFailed(String::from(
                    "the active output stream stopped unexpectedly",
                )));
            }
            if self.player.empty() {
                let result = wait_for_output_drain(
                    &self.cancelled,
                    &self.output_failed,
                    deadline,
                    self.drain_duration,
                );
                if result == Err(PlaybackError::TimedOut) {
                    self.player.stop();
                }
                return result;
            }
            if Instant::now() >= deadline {
                self.player.stop();
                return Err(PlaybackError::TimedOut);
            }
            thread::sleep(PLAYBACK_POLL_INTERVAL);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        output_drain_duration, wait_for_output_drain, ActivePlayback, AudioPlaybackService,
        PlaybackBackend, PlaybackError, PlaybackRequest,
    };
    use rodio::cpal::BufferSize;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{mpsc, Arc, Condvar, Mutex};
    use std::thread;
    use std::time::{Duration, Instant};

    #[derive(Default)]
    struct FakeBackend {
        starts: AtomicUsize,
        releases: AtomicUsize,
        fail_start: AtomicBool,
        default_device_generation: AtomicUsize,
        last_linear_gain: Mutex<Option<f32>>,
        started_device_generations: Mutex<Vec<usize>>,
        players: Mutex<Vec<Arc<FakePlayback>>>,
    }

    impl PlaybackBackend for FakeBackend {
        fn start(
            &self,
            _pcm_f32: Vec<f32>,
            _sample_rate_hz: u32,
            linear_gain: f32,
        ) -> Result<Arc<dyn ActivePlayback>, PlaybackError> {
            self.starts.fetch_add(1, Ordering::AcqRel);
            self.started_device_generations
                .lock()
                .expect("device generations lock")
                .push(self.default_device_generation.load(Ordering::Acquire));
            *self.last_linear_gain.lock().expect("gain lock") = Some(linear_gain);
            if self.fail_start.load(Ordering::Acquire) {
                return Err(PlaybackError::OutputUnavailable(String::from(
                    "test output unavailable",
                )));
            }
            let playback = Arc::new(FakePlayback::default());
            self.players
                .lock()
                .expect("fake players lock")
                .push(Arc::clone(&playback));
            Ok(playback)
        }

        fn release(&self) {
            self.releases.fetch_add(1, Ordering::AcqRel);
        }
    }

    #[derive(Default)]
    struct FakePlayback {
        activated: AtomicBool,
        stopped: AtomicBool,
        completed: Mutex<bool>,
        changed: Condvar,
    }

    #[derive(Default)]
    struct BlockingBackend {
        start_entered: Mutex<bool>,
        start_entered_changed: Condvar,
        start_released: Mutex<bool>,
        start_released_changed: Condvar,
        releases: AtomicUsize,
        playback: Mutex<Option<Arc<FakePlayback>>>,
    }

    impl BlockingBackend {
        fn wait_for_start(&self) {
            let entered = self.start_entered.lock().expect("start entered lock");
            let _entered = self
                .start_entered_changed
                .wait_while(entered, |entered| !*entered)
                .expect("start entered wait");
        }

        fn release_start(&self) {
            *self.start_released.lock().expect("start released lock") = true;
            self.start_released_changed.notify_all();
        }

        fn prepared_playback(&self) -> Arc<FakePlayback> {
            self.playback
                .lock()
                .expect("blocking playback lock")
                .clone()
                .expect("blocking playback should be prepared")
        }
    }

    impl PlaybackBackend for BlockingBackend {
        fn start(
            &self,
            _pcm_f32: Vec<f32>,
            _sample_rate_hz: u32,
            _linear_gain: f32,
        ) -> Result<Arc<dyn ActivePlayback>, PlaybackError> {
            *self.start_entered.lock().expect("start entered lock") = true;
            self.start_entered_changed.notify_all();
            let released = self.start_released.lock().expect("start released lock");
            let _released = self
                .start_released_changed
                .wait_while(released, |released| !*released)
                .expect("start released wait");
            let playback = Arc::new(FakePlayback::default());
            *self.playback.lock().expect("blocking playback lock") = Some(Arc::clone(&playback));
            Ok(playback)
        }

        fn release(&self) {
            self.releases.fetch_add(1, Ordering::AcqRel);
        }
    }

    impl FakePlayback {
        fn complete(&self) {
            *self.completed.lock().expect("completion lock") = true;
            self.changed.notify_all();
        }
    }

    impl ActivePlayback for FakePlayback {
        fn activate(&self) -> Result<(), PlaybackError> {
            if self.stopped.load(Ordering::Acquire) {
                return Err(PlaybackError::Superseded);
            }
            self.activated.store(true, Ordering::Release);
            Ok(())
        }

        fn stop(&self) {
            self.stopped.store(true, Ordering::Release);
            self.changed.notify_all();
        }

        fn wait(&self, timeout: Duration) -> Result<(), PlaybackError> {
            let completed = self.completed.lock().expect("completion lock");
            let (completed, _) = self
                .changed
                .wait_timeout_while(completed, timeout, |completed| {
                    !*completed && !self.stopped.load(Ordering::Acquire)
                })
                .expect("completion wait");
            if self.stopped.load(Ordering::Acquire) {
                Err(PlaybackError::Superseded)
            } else if *completed {
                Ok(())
            } else {
                Err(PlaybackError::TimedOut)
            }
        }
    }

    fn request(playback_id: u64) -> PlaybackRequest {
        PlaybackRequest {
            playback_id,
            pcm_f32: vec![0.0; 22],
            sample_rate_hz: 22_000,
            gain_db: 3.0,
        }
    }

    fn wait_for_player(backend: &FakeBackend, index: usize) -> Arc<FakePlayback> {
        for _ in 0..100 {
            if let Some(player) = backend
                .players
                .lock()
                .expect("fake players lock")
                .get(index)
                .cloned()
            {
                return player;
            }
            thread::sleep(Duration::from_millis(1));
        }
        panic!("fake playback did not start");
    }

    #[test]
    fn validates_requests_before_opening_output() {
        let backend = Arc::new(FakeBackend::default());
        let service = AudioPlaybackService::with_backend(backend.clone());

        let mut invalid = request(1);
        invalid.pcm_f32.clear();
        assert_eq!(service.play(invalid), Err(PlaybackError::EmptyAudio));
        let mut invalid = request(1);
        invalid.pcm_f32[0] = f32::NAN;
        assert_eq!(service.play(invalid), Err(PlaybackError::InvalidSample));
        let mut invalid = request(1);
        invalid.gain_db = f32::INFINITY;
        assert_eq!(service.play(invalid), Err(PlaybackError::InvalidGain));
        assert_eq!(backend.starts.load(Ordering::Acquire), 0);
    }

    #[test]
    fn completes_playback_and_reports_duration() {
        let backend = Arc::new(FakeBackend::default());
        let service = Arc::new(AudioPlaybackService::with_backend(backend.clone()));
        let worker_service = Arc::clone(&service);
        let worker = thread::spawn(move || worker_service.play(request(1)));
        let player = wait_for_player(&backend, 0);

        player.complete();

        let completion = worker.join().expect("playback thread").expect("playback");
        assert_eq!(completion.playback_id, 1);
        assert_eq!(completion.duration_ms, 1);
        let gain = backend
            .last_linear_gain
            .lock()
            .expect("gain lock")
            .expect("gain should be captured");
        assert!((gain - 10_f32.powf(3.0 / 20.0)).abs() < 0.000_1);
    }

    #[test]
    fn output_open_failure_is_retryable_for_the_same_playback_id() {
        let backend = Arc::new(FakeBackend::default());
        backend.fail_start.store(true, Ordering::Release);
        let service = Arc::new(AudioPlaybackService::with_backend(backend.clone()));

        assert!(matches!(
            service.play(request(1)),
            Err(PlaybackError::OutputUnavailable(_))
        ));

        backend.fail_start.store(false, Ordering::Release);
        let worker_service = Arc::clone(&service);
        let worker = thread::spawn(move || worker_service.play(request(1)));
        let player = wait_for_player(&backend, 0);
        player.complete();
        assert!(worker.join().expect("playback thread").is_ok());
    }

    #[test]
    fn newer_playback_supersedes_and_stops_older_playback() {
        let backend = Arc::new(FakeBackend::default());
        let service = Arc::new(AudioPlaybackService::with_backend(backend.clone()));
        let first_service = Arc::clone(&service);
        let first = thread::spawn(move || first_service.play(request(1)));
        let first_player = wait_for_player(&backend, 0);
        let second_service = Arc::clone(&service);
        let second = thread::spawn(move || second_service.play(request(2)));
        let second_player = wait_for_player(&backend, 1);

        assert!(first_player.stopped.load(Ordering::Acquire));
        second_player.complete();

        assert_eq!(
            first.join().expect("first playback"),
            Err(PlaybackError::Superseded)
        );
        assert!(second.join().expect("second playback").is_ok());
    }

    #[test]
    fn duplicate_active_playback_id_is_rejected() {
        let backend = Arc::new(FakeBackend::default());
        let service = Arc::new(AudioPlaybackService::with_backend(backend.clone()));
        let worker_service = Arc::clone(&service);
        let worker = thread::spawn(move || worker_service.play(request(1)));
        let player = wait_for_player(&backend, 0);

        assert_eq!(service.play(request(1)), Err(PlaybackError::Superseded));
        assert_eq!(backend.starts.load(Ordering::Acquire), 1);

        player.complete();
        assert!(worker.join().expect("playback thread").is_ok());
    }

    #[test]
    fn stale_cancel_does_not_stop_newer_playback() {
        let backend = Arc::new(FakeBackend::default());
        let service = Arc::new(AudioPlaybackService::with_backend(backend.clone()));
        let worker_service = Arc::clone(&service);
        let worker = thread::spawn(move || worker_service.play(request(2)));
        let player = wait_for_player(&backend, 0);

        assert!(!service.cancel(1).expect("stale cancellation"));
        assert!(!player.stopped.load(Ordering::Acquire));
        player.complete();
        assert!(worker.join().expect("playback thread").is_ok());
    }

    #[test]
    fn matching_cancel_stops_playback_and_wakes_waiter() {
        let backend = Arc::new(FakeBackend::default());
        let service = Arc::new(AudioPlaybackService::with_backend(backend.clone()));
        let worker_service = Arc::clone(&service);
        let worker = thread::spawn(move || worker_service.play(request(3)));
        let player = wait_for_player(&backend, 0);

        assert!(service.cancel(3).expect("matching cancellation"));
        assert!(player.stopped.load(Ordering::Acquire));
        assert_eq!(
            worker.join().expect("playback thread"),
            Err(PlaybackError::Superseded)
        );
    }

    #[test]
    fn suspend_stops_current_playback_and_releases_backend() {
        let backend = Arc::new(FakeBackend::default());
        let service = Arc::new(AudioPlaybackService::with_backend(backend.clone()));
        let worker_service = Arc::clone(&service);
        let worker = thread::spawn(move || worker_service.play(request(4)));
        let player = wait_for_player(&backend, 0);

        service.suspend().expect("suspend output");

        assert!(player.stopped.load(Ordering::Acquire));
        assert_eq!(backend.releases.load(Ordering::Acquire), 1);
        assert_eq!(
            worker.join().expect("playback thread"),
            Err(PlaybackError::Superseded)
        );
    }

    #[test]
    fn shutdown_rejects_future_playback() {
        let backend = Arc::new(FakeBackend::default());
        let service = AudioPlaybackService::with_backend(backend.clone());

        service.shutdown();

        assert_eq!(service.play(request(5)), Err(PlaybackError::ShuttingDown));
        assert_eq!(backend.releases.load(Ordering::Acquire), 1);
    }

    #[test]
    fn shutdown_is_bounded_while_output_start_is_blocked() {
        let backend = Arc::new(BlockingBackend::default());
        let service = Arc::new(AudioPlaybackService::with_backend(backend.clone()));
        let playback_service = Arc::clone(&service);
        let playback = thread::spawn(move || playback_service.play(request(6)));
        backend.wait_for_start();
        let shutdown_service = Arc::clone(&service);
        let (shutdown_tx, shutdown_rx) = mpsc::channel();
        let shutdown = thread::spawn(move || {
            shutdown_service.shutdown();
            shutdown_tx.send(()).expect("report shutdown completion");
        });

        let bounded = shutdown_rx.recv_timeout(Duration::from_millis(50));
        backend.release_start();
        shutdown.join().expect("shutdown thread");
        let playback_result = playback.join().expect("playback thread");

        assert!(
            bounded.is_ok(),
            "shutdown waited for blocked output startup"
        );
        assert_eq!(playback_result, Err(PlaybackError::ShuttingDown));
        assert!(!backend
            .prepared_playback()
            .activated
            .load(Ordering::Acquire));
        assert_eq!(service.play(request(7)), Err(PlaybackError::ShuttingDown));
        assert_eq!(backend.releases.load(Ordering::Acquire), 1);
    }

    #[test]
    fn shutdown_waits_for_short_state_contention_and_stops_playback() {
        let backend = Arc::new(FakeBackend::default());
        let service = Arc::new(AudioPlaybackService::with_backend(backend.clone()));
        let playback_service = Arc::clone(&service);
        let playback = thread::spawn(move || playback_service.play(request(8)));
        let player = wait_for_player(&backend, 0);
        let state_guard = service.state.lock().expect("state lock");
        let shutdown_service = Arc::clone(&service);
        let (shutdown_tx, shutdown_rx) = mpsc::channel();
        let shutdown = thread::spawn(move || {
            shutdown_service.shutdown();
            shutdown_tx.send(()).expect("report shutdown completion");
        });

        let completed_while_contended = shutdown_rx.recv_timeout(Duration::from_millis(20));
        drop(state_guard);
        shutdown.join().expect("shutdown thread");

        assert!(completed_while_contended.is_err());
        assert!(player.stopped.load(Ordering::Acquire));
        assert_eq!(
            playback.join().expect("playback thread"),
            Err(PlaybackError::Superseded)
        );
    }

    #[test]
    fn suspend_rejects_delayed_and_pre_suspend_requests() {
        let backend = Arc::new(FakeBackend::default());
        let service = AudioPlaybackService::with_backend(backend.clone());

        service.cancel(8).expect("invalidate authorized playback");
        service.suspend().expect("suspend playback");
        assert_eq!(service.play(request(9)), Err(PlaybackError::Suspended));
        service.resume().expect("resume playback");
        assert_eq!(service.play(request(8)), Err(PlaybackError::Superseded));
        assert_eq!(backend.starts.load(Ordering::Acquire), 0);
    }

    #[test]
    fn delayed_start_stays_invalid_after_suspend_and_resume() {
        let backend = Arc::new(BlockingBackend::default());
        let service = Arc::new(AudioPlaybackService::with_backend(backend.clone()));
        let playback_service = Arc::clone(&service);
        let playback = thread::spawn(move || playback_service.play(request(10)));
        backend.wait_for_start();

        service.suspend().expect("suspend playback");
        service.cancel(10).expect("invalidate authorized playback");
        service.resume().expect("resume playback");
        backend.release_start();

        assert_eq!(
            playback.join().expect("playback thread"),
            Err(PlaybackError::Superseded)
        );
        assert!(!backend
            .prepared_playback()
            .activated
            .load(Ordering::Acquire));
    }

    #[test]
    fn each_utterance_resolves_the_current_default_output() {
        let backend = Arc::new(FakeBackend::default());
        backend
            .default_device_generation
            .store(1, Ordering::Release);
        let service = Arc::new(AudioPlaybackService::with_backend(backend.clone()));

        let first_service = Arc::clone(&service);
        let first = thread::spawn(move || first_service.play(request(11)));
        wait_for_player(&backend, 0).complete();
        first.join().expect("first playback").expect("first result");

        backend
            .default_device_generation
            .store(2, Ordering::Release);
        let second_service = Arc::clone(&service);
        let second = thread::spawn(move || second_service.play(request(12)));
        wait_for_player(&backend, 1).complete();
        second
            .join()
            .expect("second playback")
            .expect("second result");

        assert_eq!(
            *backend
                .started_device_generations
                .lock()
                .expect("device generations lock"),
            vec![1, 2]
        );
    }

    #[test]
    fn output_drain_uses_the_configured_buffer_interval() {
        assert_eq!(
            output_drain_duration(&BufferSize::Fixed(2_400), 48_000),
            Duration::from_millis(60)
        );
        assert_eq!(
            output_drain_duration(&BufferSize::Default, 48_000),
            Duration::from_millis(110)
        );
    }

    #[test]
    fn output_drain_retains_the_sink_and_observes_cancellation() {
        let cancelled = Arc::new(AtomicBool::new(false));
        let failed = AtomicBool::new(false);
        let start = Instant::now();
        wait_for_output_drain(
            &cancelled,
            &failed,
            start + Duration::from_secs(1),
            Duration::from_millis(10),
        )
        .expect("drain output");
        assert!(start.elapsed() >= Duration::from_millis(10));

        let cancellation = Arc::clone(&cancelled);
        let cancel = thread::spawn(move || {
            thread::sleep(Duration::from_millis(10));
            cancellation.store(true, Ordering::Release);
        });
        assert_eq!(
            wait_for_output_drain(
                &cancelled,
                &failed,
                Instant::now() + Duration::from_secs(1),
                Duration::from_millis(100),
            ),
            Err(PlaybackError::Superseded)
        );
        cancel.join().expect("cancellation thread");
    }
}
