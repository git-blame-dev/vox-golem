use std::collections::VecDeque;
use std::fmt;
use std::num::{NonZeroU16, NonZeroU32};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};

use rodio::buffer::SamplesBuffer;
use rodio::cpal::BufferSize;
use rodio::{DeviceSinkBuilder, MixerDeviceSink, Player, Source};

const MAX_PLAYBACK_ID: u64 = 9_007_199_254_740_991;
const PLAYBACK_POLL_INTERVAL: Duration = Duration::from_millis(10);
const PLAYBACK_DRAIN_GRACE: Duration = Duration::from_secs(5);
const OUTPUT_BUFFER_FALLBACK: Duration = Duration::from_millis(100);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaybackPurpose {
    Cue,
    Speech,
}

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
    AudioTooLarge,
    Superseded,
    Suspended,
    ShuttingDown,
    OutputUnavailable(String),
    OutputFailed(String),
    TimedOut,
    StatePoisoned,
    Busy,
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
            Self::AudioTooLarge => write!(formatter, "audio playback exceeds the buffer limit"),
            Self::Superseded => write!(formatter, "audio playback was cancelled or superseded"),
            Self::Suspended => write!(formatter, "audio playback service is suspended"),
            Self::ShuttingDown => write!(formatter, "audio playback service is shutting down"),
            Self::OutputUnavailable(details) => {
                write!(formatter, "audio output is unavailable: {details}")
            }
            Self::OutputFailed(details) => write!(formatter, "audio output failed: {details}"),
            Self::TimedOut => write!(formatter, "audio playback timed out"),
            Self::StatePoisoned => write!(formatter, "audio playback state lock is poisoned"),
            Self::Busy => write!(formatter, "audio playback admission is full"),
        }
    }
}

impl std::error::Error for PlaybackError {}

// At most one second/64 frames queued plus one consumer-owned frame (64 KiB).
const STREAM_FRAME_SAMPLES: usize = 65_536 / std::mem::size_of::<f32>();
const STREAM_MAX_QUEUED_FRAMES: usize = 64;
const STREAM_WAIT_LIMIT: Duration = Duration::from_secs(15);

/// Clones share one producer lifetime. Explicit finish/fail is recommended;
/// dropping the last handle aborts rather than silently accepting truncation.
#[derive(Clone)]
pub struct AudioPlaybackStream(Arc<StreamProducer>);

struct StreamProducer {
    owner: Arc<AudioPlaybackService>,
    playback_id: u64,
    queue: Arc<StreamQueue>,
}

impl AudioPlaybackStream {
    /// Frames are at most 64 KiB and at most one second at the declared rate.
    /// Bounded blocking backpressure; never call from an async/UI thread.
    /// Malformed frames abort even after partial output; late frames are rejected.
    pub fn append(&self, pcm_f32: Vec<f32>) -> Result<(), PlaybackError> {
        if !self
            .0
            .owner
            .owns(self.0.playback_id, PlaybackPurpose::Speech)?
        {
            self.0.queue.fail(PlaybackError::Superseded);
        }
        self.0.queue.append(pcm_f32)
    }

    /// Signals producer EOF and waits for native playback plus output-buffer drain.
    /// Success retains the shared default device for subsequent cues/speech.
    pub fn finish(&self) -> Result<PlaybackCompletion, PlaybackError> {
        self.0.queue.close()?;
        self.wait()
    }

    /// Reports a failed/truncated producer without logging provider payloads.
    /// Wait for `wait` to acknowledge the worker's actual retirement.
    pub fn fail(&self) {
        self.0
            .queue
            .fail(PlaybackError::OutputFailed("audio producer failed".into()));
    }

    /// Waits for retirement without marking producer EOF (also useful after cancel).
    pub fn wait(&self) -> Result<PlaybackCompletion, PlaybackError> {
        let mut state = self
            .0
            .queue
            .state
            .lock()
            .map_err(|_| PlaybackError::StatePoisoned)?;
        loop {
            if let Some(result) = &state.result {
                return result.clone();
            }
            state = self
                .0
                .queue
                .changed
                .wait(state)
                .map_err(|_| PlaybackError::StatePoisoned)?;
        }
    }
}

impl Drop for StreamProducer {
    fn drop(&mut self) {
        self.queue.fail(PlaybackError::Superseded);
        let _ = self
            .owner
            .cancel_owned(self.playback_id, PlaybackPurpose::Speech);
    }
}

struct StreamQueue {
    rate: u32,
    state: Mutex<StreamQueueState>,
    changed: Condvar,
    aborted: AtomicBool,
}

struct StreamQueueState {
    frames: VecDeque<Vec<f32>>,
    queued: usize,
    total: usize,
    closed: bool,
    error: Option<PlaybackError>,
    result: Option<Result<PlaybackCompletion, PlaybackError>>,
}

impl StreamQueue {
    fn new(rate: u32) -> Self {
        Self {
            rate,
            state: Mutex::new(StreamQueueState {
                frames: VecDeque::new(),
                queued: 0,
                total: 0,
                closed: false,
                error: None,
                result: None,
            }),
            changed: Condvar::new(),
            aborted: AtomicBool::new(false),
        }
    }

    fn append(&self, frame: Vec<f32>) -> Result<(), PlaybackError> {
        self.append_until(frame, Instant::now() + STREAM_WAIT_LIMIT)
    }

    fn append_until(&self, frame: Vec<f32>, deadline: Instant) -> Result<(), PlaybackError> {
        let validation = if frame.is_empty() {
            Err(PlaybackError::EmptyAudio)
        } else if frame.len() > STREAM_FRAME_SAMPLES || frame.len() > self.rate as usize {
            Err(PlaybackError::AudioTooLarge)
        } else if frame.iter().any(|sample| !sample.is_finite()) {
            Err(PlaybackError::InvalidSample)
        } else {
            Ok(())
        };
        if let Err(error) = validation {
            self.fail(error.clone());
            return Err(error);
        }
        // Caller-provided spare capacity must not inflate retained audio memory.
        let frame = frame.into_boxed_slice().into_vec();
        let mut state = self
            .state
            .lock()
            .map_err(|_| PlaybackError::StatePoisoned)?;
        loop {
            if let Some(error) = &state.error {
                return Err(error.clone());
            }
            if state.closed || state.result.is_some() {
                return Err(PlaybackError::Superseded);
            }
            if state.total.saturating_add(frame.len()) > self.rate as usize * 600 {
                drop(state);
                self.fail(PlaybackError::AudioTooLarge);
                return Err(PlaybackError::AudioTooLarge);
            }
            if state.queued + frame.len() <= self.rate as usize
                && state.frames.len() < STREAM_MAX_QUEUED_FRAMES
            {
                break;
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                drop(state);
                self.fail(PlaybackError::TimedOut);
                return Err(PlaybackError::TimedOut);
            }
            state = self
                .changed
                .wait_timeout(state, remaining)
                .map_err(|_| PlaybackError::StatePoisoned)?
                .0;
        }
        state.total += frame.len();
        state.queued += frame.len();
        state.frames.push_back(frame);
        self.changed.notify_all();
        Ok(())
    }

    fn close(&self) -> Result<(), PlaybackError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| PlaybackError::StatePoisoned)?;
        // Even a failed stream must wait for worker retirement in finish().
        if state.total == 0 && state.error.is_none() {
            drop(state);
            self.fail(PlaybackError::EmptyAudio);
            return Ok(());
        }
        state.closed = true;
        self.changed.notify_all();
        Ok(())
    }

    fn fail(&self, error: PlaybackError) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.result.is_some() {
            return;
        }
        state.error.get_or_insert(error);
        state.frames.clear();
        state.queued = 0;
        self.aborted.store(true, Ordering::Release);
        self.changed.notify_all();
    }

    fn error(&self) -> Option<PlaybackError> {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .error
            .clone()
    }

    fn complete(&self, result: Result<PlaybackCompletion, PlaybackError>) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if let Err(error) = &result {
            state.error.get_or_insert(error.clone());
        }
        state.frames.clear();
        state.queued = 0;
        state.result = Some(result);
        self.changed.notify_all();
    }
}

struct StreamSource {
    queue: Arc<StreamQueue>,
    frame: std::vec::IntoIter<f32>,
    silence: usize,
    empty_since: Instant,
}

impl StreamSource {
    fn new(queue: Arc<StreamQueue>) -> Self {
        Self {
            queue,
            frame: Vec::new().into_iter(),
            silence: 0,
            empty_since: Instant::now(),
        }
    }
}

impl Iterator for StreamSource {
    type Item = f32;
    fn next(&mut self) -> Option<f32> {
        if self.queue.aborted.load(Ordering::Acquire) {
            return None;
        }
        if let Some(sample) = self.frame.next() {
            return Some(sample);
        }
        if self.silence > 0 {
            self.silence -= 1;
            return Some(0.0);
        }
        // Never wait on the device callback: inspect once per frame/1ms silence.
        if let Ok(mut state) = self.queue.state.try_lock() {
            if let Some(frame) = state.frames.pop_front() {
                state.queued -= frame.len();
                self.queue.changed.notify_all();
                self.frame = frame.into_iter();
                self.empty_since = Instant::now();
                return self.frame.next();
            }
            if state.closed {
                return None;
            }
            if self.empty_since.elapsed() >= STREAM_WAIT_LIMIT {
                state.error.get_or_insert(PlaybackError::TimedOut);
                self.queue.aborted.store(true, Ordering::Release);
                self.queue.changed.notify_all();
                return None;
            }
        }
        self.silence = (self.queue.rate as usize / 1_000).max(1) - 1;
        Some(0.0)
    }
}

impl Source for StreamSource {
    fn current_span_len(&self) -> Option<usize> {
        None
    }
    fn channels(&self) -> NonZeroU16 {
        NonZeroU16::new(1).expect("mono")
    }
    fn sample_rate(&self) -> NonZeroU32 {
        NonZeroU32::new(self.queue.rate).expect("validated rate")
    }
    fn total_duration(&self) -> Option<Duration> {
        None
    }
}

struct StreamingPlayback {
    playback: Arc<dyn ActivePlayback>,
    queue: Arc<StreamQueue>,
}

impl ActivePlayback for StreamingPlayback {
    fn activate(&self) -> Result<(), PlaybackError> {
        if let Some(error) = self.queue.error() {
            return Err(error);
        }
        self.playback.activate()
    }
    fn stop(&self) {
        self.queue.fail(PlaybackError::Superseded);
        self.playback.stop();
    }
    fn wait(&self, timeout: Duration) -> Result<(), PlaybackError> {
        let result = self.playback.wait(timeout);
        self.queue.error().map_or(result, Err)
    }
}

fn validate_metadata(id: u64, rate: u32, gain: f32) -> Result<(), PlaybackError> {
    validate_playback_id(id)?;
    if rate == 0 {
        return Err(PlaybackError::InvalidSampleRate);
    }
    if rate > 96_000 {
        return Err(PlaybackError::AudioTooLarge);
    }
    if !gain.is_finite() || !10_f32.powf(gain / 20.0).is_finite() {
        return Err(PlaybackError::InvalidGain);
    }
    Ok(())
}

pub struct AudioPlaybackService {
    operation_lock: Mutex<OutputOwnership>,
    state: Mutex<PlaybackState>,
    backend: Arc<dyn PlaybackBackend>,
    suspended: AtomicBool,
    shutting_down: AtomicBool,
    speech_enabled: AtomicBool,
    workers: AtomicUsize,
}

struct PlaybackState {
    current: Option<CurrentPlayback>,
    next_ticket: u64,
    cue_ticket: Option<u64>,
    speech_ticket: Option<u64>,
    pending_stream: Option<(u64, Arc<StreamQueue>)>,
    lifecycle_epoch: u64,
    retire_through: u64,
}

impl Default for PlaybackState {
    fn default() -> Self {
        Self {
            current: None,
            next_ticket: 0,
            cue_ticket: None,
            speech_ticket: None,
            pending_stream: None,
            lifecycle_epoch: 1,
            retire_through: 0,
        }
    }
}

#[derive(Default)]
struct OutputOwnership {
    epoch: Option<u64>,
}

/// A close request fences the output lifetime that existed when admission closed.
/// It cannot retire an output created by a later resume.
#[derive(Clone, Copy)]
pub struct OutputRetirement(u64);

struct CurrentPlayback {
    playback_id: u64,
    playback: Arc<dyn ActivePlayback>,
    purpose: PlaybackPurpose,
}

trait PlaybackBackend: Send + Sync {
    #[cfg(test)]
    fn before_admission(&self) {}
    fn start(
        &self,
        pcm_f32: Vec<f32>,
        sample_rate_hz: u32,
        linear_gain: f32,
    ) -> Result<Arc<dyn ActivePlayback>, PlaybackError>;

    fn start_stream(
        &self,
        source: StreamSource,
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
        Self::with_backend(Arc::new(RodioBackend::default()))
    }

    fn with_backend(backend: Arc<dyn PlaybackBackend>) -> Self {
        Self {
            operation_lock: Mutex::new(OutputOwnership::default()),
            state: Mutex::new(PlaybackState::default()),
            backend,
            suspended: AtomicBool::new(false),
            shutting_down: AtomicBool::new(false),
            speech_enabled: AtomicBool::new(true),
            workers: AtomicUsize::new(0),
        }
    }

    /// Reserves one of two bounded ownership slots. Speech explicitly retires cues;
    /// cues replace cues but never interrupt reserved or active speech.
    pub fn reserve(&self, purpose: PlaybackPurpose) -> Result<u64, PlaybackError> {
        self.check_available(purpose)?;
        let mut state = self
            .state
            .lock()
            .map_err(|_| PlaybackError::StatePoisoned)?;
        self.check_available(purpose)?;
        if purpose == PlaybackPurpose::Cue && state.speech_ticket.is_some() {
            return Err(PlaybackError::Superseded);
        }
        let ticket = state
            .next_ticket
            .checked_add(1)
            .filter(|ticket| *ticket <= MAX_PLAYBACK_ID)
            .ok_or(PlaybackError::InvalidPlaybackId)?;
        state.next_ticket = ticket;
        match purpose {
            PlaybackPurpose::Speech => {
                state.speech_ticket = Some(ticket);
                state.cue_ticket = None;
            }
            PlaybackPurpose::Cue => state.cue_ticket = Some(ticket),
        }
        if let Some((_, queue)) = state.pending_stream.take() {
            queue.fail(PlaybackError::Superseded);
        }
        if let Some(current) = state.current.take() {
            current.playback.stop();
        }
        Ok(ticket)
    }

    /// Wait for retirement of an owned operation. `activated` is a short historical
    /// notification after activation and after both service locks are released;
    /// it may reenter ownership/cancellation APIs. It must not block or panic.
    pub fn play_owned(
        &self,
        request: PlaybackRequest,
        purpose: PlaybackPurpose,
        activated: impl FnOnce(),
    ) -> Result<PlaybackCompletion, PlaybackError> {
        let ticket = request.playback_id;
        let result = self.play_inner(request, purpose, activated, None);
        if result.is_err() {
            let _ = self.retire_unstarted(ticket, purpose);
        }
        result
    }

    /// Opens a mono f32 speech stream. No device work occurs on the caller.
    /// Activation runs on the owned blocking worker and must be short/non-panicking.
    /// Append/finish/wait must run on blocking threads. Last-handle drop aborts.
    /// Limits: 600s audio, 615s active playback, 15s starvation/backpressure timeout.
    pub fn open_stream_owned(
        self: &Arc<Self>,
        playback_id: u64,
        sample_rate_hz: u32,
        gain_db: f32,
        on_activated: impl FnOnce() + Send + 'static,
    ) -> Result<AudioPlaybackStream, PlaybackError> {
        let result = self.open_stream_inner(playback_id, sample_rate_hz, gain_db, on_activated);
        if result.is_err() {
            let _ = self.retire_unstarted(playback_id, PlaybackPurpose::Speech);
        }
        result
    }

    fn open_stream_inner(
        self: &Arc<Self>,
        playback_id: u64,
        sample_rate_hz: u32,
        gain_db: f32,
        on_activated: impl FnOnce() + Send + 'static,
    ) -> Result<AudioPlaybackStream, PlaybackError> {
        validate_metadata(playback_id, sample_rate_hz, gain_db)?;
        self.check_available(PlaybackPurpose::Speech)?;
        let queue = Arc::new(StreamQueue::new(sample_rate_hz));
        {
            let mut state = self
                .state
                .lock()
                .map_err(|_| PlaybackError::StatePoisoned)?;
            self.check_available(PlaybackPurpose::Speech)?;
            if !state.owns(PlaybackPurpose::Speech, playback_id)
                || state.pending_stream.is_some()
                || state
                    .current
                    .as_ref()
                    .is_some_and(|current| current.playback_id == playback_id)
            {
                return Err(PlaybackError::Superseded);
            }
            self.workers
                .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                    (n < 3).then_some(n + 1)
                })
                .map_err(|_| PlaybackError::Busy)?;
            state.pending_stream = Some((playback_id, queue.clone()));
        }
        let owner = self.clone();
        let worker_queue = queue.clone();
        let spawn = thread::Builder::new()
            .name("speech-output".into())
            .spawn(move || {
                let result = owner.play_inner(
                    PlaybackRequest {
                        playback_id,
                        pcm_f32: Vec::new(),
                        sample_rate_hz,
                        gain_db,
                    },
                    PlaybackPurpose::Speech,
                    on_activated,
                    Some(worker_queue.clone()),
                );
                {
                    let mut state = owner.state.lock().unwrap_or_else(|e| e.into_inner());
                    if state
                        .pending_stream
                        .as_ref()
                        .is_some_and(|(id, _)| *id == playback_id)
                    {
                        state.pending_stream = None;
                    }
                }
                let _ = owner.retire_unstarted(playback_id, PlaybackPurpose::Speech);
                worker_queue.complete(result);
            });
        if spawn.is_err() {
            self.workers.fetch_sub(1, Ordering::AcqRel);
            self.cancel_owned(playback_id, PlaybackPurpose::Speech)?;
            return Err(PlaybackError::OutputUnavailable(
                "playback worker could not start".into(),
            ));
        }
        Ok(AudioPlaybackStream(Arc::new(StreamProducer {
            owner: self.clone(),
            playback_id,
            queue,
        })))
    }

    fn check_available(&self, purpose: PlaybackPurpose) -> Result<(), PlaybackError> {
        if self.shutting_down.load(Ordering::Acquire) {
            return Err(PlaybackError::ShuttingDown);
        }
        if self.suspended.load(Ordering::Acquire)
            || (purpose == PlaybackPurpose::Speech && !self.speech_enabled.load(Ordering::Acquire))
        {
            return Err(PlaybackError::Suspended);
        }
        Ok(())
    }

    fn play_inner(
        &self,
        request: PlaybackRequest,
        purpose: PlaybackPurpose,
        activated: impl FnOnce(),
        stream: Option<Arc<StreamQueue>>,
    ) -> Result<PlaybackCompletion, PlaybackError> {
        // Streaming admission already holds a worker slot before thread creation.
        let _stream_worker = stream.as_ref().map(|_| PlaybackWorker(&self.workers));
        if stream.is_none() {
            validate_request(&request)?;
        }
        self.check_available(purpose)?;
        // One active waiter, one initializing replacement, one pending replacement.
        // Excess callers fail admission rather than building an unbounded wait queue.
        if stream.is_none() {
            self.workers
                .fetch_update(Ordering::AcqRel, Ordering::Acquire, |workers| {
                    (workers < 3).then_some(workers + 1)
                })
                .map_err(|_| PlaybackError::Busy)?;
        }
        let _worker = stream.is_none().then(|| PlaybackWorker(&self.workers));
        let duration_ms = duration_ms(request.pcm_f32.len(), request.sample_rate_hz);
        let timeout = if stream.is_some() {
            Duration::from_secs(615)
        } else {
            Duration::from_millis(duration_ms).saturating_add(PLAYBACK_DRAIN_GRACE)
        };
        let linear_gain = 10_f32.powf(request.gain_db / 20.0);

        let playback = {
            let mut operation = self.operation()?;
            #[cfg(test)]
            self.backend.before_admission();
            let (previous, epoch) = {
                let mut state = self
                    .state
                    .lock()
                    .map_err(|_| PlaybackError::StatePoisoned)?;
                self.check_available(purpose)?;
                if !state.owns(purpose, request.playback_id)
                    || (stream.is_none()
                        && state
                            .pending_stream
                            .as_ref()
                            .is_some_and(|(id, _)| *id == request.playback_id))
                    || state
                        .current
                        .as_ref()
                        .is_some_and(|current| current.playback_id == request.playback_id)
                {
                    return Err(PlaybackError::Superseded);
                }
                (state.current.take(), state.lifecycle_epoch)
            };
            if let Some(previous) = previous {
                previous.playback.stop();
            }

            operation.retire_pending()?;
            operation.guard.as_mut().expect("operation guard").epoch = Some(epoch);

            let started = if let Some(queue) = &stream {
                self.backend
                    .start_stream(StreamSource::new(queue.clone()), linear_gain)
                    .map(|playback| {
                        Arc::new(StreamingPlayback {
                            playback,
                            queue: queue.clone(),
                        }) as Arc<dyn ActivePlayback>
                    })
            } else {
                self.backend
                    .start(request.pcm_f32, request.sample_rate_hz, linear_gain)
            };
            let playback = match started {
                Ok(playback) => playback,
                Err(error) => {
                    operation.release();
                    return Err(error);
                }
            };
            if let Err(error) = self.check_available(purpose) {
                playback.stop();
                operation.release();
                return Err(error);
            }
            let mut state = self
                .state
                .lock()
                .map_err(|_| PlaybackError::StatePoisoned)?;
            if !state.owns(purpose, request.playback_id) {
                drop(state);
                playback.stop();
                operation.release();
                return Err(PlaybackError::Superseded);
            }
            state.current = Some(CurrentPlayback {
                playback_id: request.playback_id,
                playback: Arc::clone(&playback),
                purpose,
            });
            if self.shutting_down.load(Ordering::Acquire) {
                playback.stop();
                state.current = None;
                drop(state);
                operation.release();
                return Err(PlaybackError::ShuttingDown);
            }
            if let Err(error) = self.check_available(purpose) {
                playback.stop();
                state.current = None;
                drop(state);
                operation.release();
                return Err(error);
            }
            if let Err(error) = playback.activate() {
                playback.stop();
                state.current = None;
                drop(state);
                operation.release();
                return Err(error);
            }
            playback
        };

        // Historical activation notification, not current ownership, completion,
        // or acoustic output. Both locks are released: reentrant owners may cancel
        // or supersede this operation, and the waiter still retires its resources.
        activated();

        let wait_result = playback.wait(timeout);
        // A cancelled source does not own the newer initializer/output. Retire
        // its waiter promptly instead of queuing behind unrelated output startup.
        if wait_result.is_err() {
            let state = self
                .state
                .lock()
                .map_err(|_| PlaybackError::StatePoisoned)?;
            if state
                .current
                .as_ref()
                .is_none_or(|current| current.playback_id != request.playback_id)
            {
                return Err(PlaybackError::Superseded);
            }
        }
        let mut error_operation = if wait_result.is_err() {
            Some(self.operation()?)
        } else {
            None
        };
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
                state.clear_ticket(request.playback_id);
                true
            } else {
                false
            }
        };
        if !is_current {
            return Err(PlaybackError::Superseded);
        }
        if let Err(error) = wait_result {
            playback.stop();
            error_operation
                .as_mut()
                .expect("failed operation guard")
                .release();
            return Err(error);
        }

        Ok(PlaybackCompletion {
            playback_id: request.playback_id,
            duration_ms: stream.as_ref().map_or(duration_ms, |q| {
                let state = q.state.lock().unwrap_or_else(|e| e.into_inner());
                self::duration_ms(state.total, request.sample_rate_hz)
            }),
        })
    }

    pub fn cancel(&self, playback_id: u64) -> Result<bool, PlaybackError> {
        // The remaining desktop TTS-disable caller owns Speech tickets only.
        self.cancel_owned(playback_id, PlaybackPurpose::Speech)
    }

    pub fn cancel_owned(
        &self,
        playback_id: u64,
        purpose: PlaybackPurpose,
    ) -> Result<bool, PlaybackError> {
        validate_playback_id(playback_id)?;
        let mut state = self
            .state
            .lock()
            .map_err(|_| PlaybackError::StatePoisoned)?;
        if !state.owns(purpose, playback_id) {
            return Ok(false);
        }
        state.clear_ticket(playback_id);
        if state
            .pending_stream
            .as_ref()
            .is_some_and(|(id, _)| *id == playback_id)
        {
            if let Some((_, queue)) = state.pending_stream.take() {
                queue.fail(PlaybackError::Superseded);
            }
        }
        if state
            .current
            .as_ref()
            .is_some_and(|current| current.playback_id == playback_id)
        {
            if let Some(current) = state.current.take() {
                current.playback.stop();
            }
        }
        Ok(true)
    }

    pub fn owns(&self, playback_id: u64, purpose: PlaybackPurpose) -> Result<bool, PlaybackError> {
        Ok(self
            .state
            .lock()
            .map_err(|_| PlaybackError::StatePoisoned)?
            .owns(purpose, playback_id))
    }

    /// Failure cleanup must not let a duplicate request retire an active owner.
    pub fn retire_unstarted(
        &self,
        playback_id: u64,
        purpose: PlaybackPurpose,
    ) -> Result<(), PlaybackError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| PlaybackError::StatePoisoned)?;
        if state
            .current
            .as_ref()
            .is_none_or(|current| current.playback_id != playback_id)
            && state
                .pending_stream
                .as_ref()
                .is_none_or(|(id, _)| *id != playback_id)
            && state.owns(purpose, playback_id)
        {
            state.clear_ticket(playback_id);
        }
        Ok(())
    }

    /// Close admission synchronously without initializing or dropping an output.
    /// A durable close obligation is also consumed by any operation before unlock.
    pub fn request_suspend(&self) -> Result<OutputRetirement, PlaybackError> {
        let (current, token) = {
            let mut state = self
                .state
                .lock()
                .map_err(|_| PlaybackError::StatePoisoned)?;
            self.suspended.store(true, Ordering::Release);
            state.cue_ticket = None;
            state.speech_ticket = None;
            if let Some((_, queue)) = state.pending_stream.take() {
                queue.fail(PlaybackError::Superseded);
            }
            let epoch = state.lifecycle_epoch;
            state.retire_through = epoch;
            state.lifecycle_epoch = epoch.checked_add(1).unwrap_or_else(|| {
                self.shutting_down.store(true, Ordering::Release);
                epoch
            });
            (state.current.take(), OutputRetirement(epoch))
        };
        if let Some(current) = current {
            current.playback.stop();
        }
        Ok(token)
    }

    /// Blocking retirement acknowledgement. Call only on a blocking worker.
    /// Success means outputs through this token have been dropped, not acoustic
    /// silence, and not that a newer resumed output has been closed.
    pub fn retire_output(&self, token: OutputRetirement) -> Result<(), PlaybackError> {
        let mut operation = self.operation()?;
        operation.release_through(token.0);
        Ok(())
    }

    /// Non-waiting lifecycle request for blocking owners (including shutdown).
    /// An in-flight initializer inherits the durable retirement obligation.
    pub fn suspend(&self) -> Result<(), PlaybackError> {
        let token = self.request_suspend()?;
        self.try_retire_output(token);
        Ok(())
    }

    fn try_retire_output(&self, token: OutputRetirement) {
        if let Ok(guard) = self.operation_lock.try_lock() {
            let mut operation = PlaybackOperation {
                owner: self,
                guard: Some(guard),
            };
            operation.release_through(token.0);
        }
    }

    fn operation(&self) -> Result<PlaybackOperation<'_>, PlaybackError> {
        Ok(PlaybackOperation {
            owner: self,
            guard: Some(
                self.operation_lock
                    .lock()
                    .map_err(|_| PlaybackError::StatePoisoned)?,
            ),
        })
    }

    pub fn resume(&self) -> Result<(), PlaybackError> {
        let _state = self
            .state
            .lock()
            .map_err(|_| PlaybackError::StatePoisoned)?;
        if self.shutting_down.load(Ordering::Acquire) {
            return Err(PlaybackError::ShuttingDown);
        }
        self.suspended.store(false, Ordering::Release);
        Ok(())
    }

    /// TTS preference changes do not suspend cue output or the foreground owner.
    pub fn set_speech_enabled(&self, enabled: bool) -> Result<(), PlaybackError> {
        if self.shutting_down.load(Ordering::Acquire) {
            return Err(PlaybackError::ShuttingDown);
        }
        self.speech_enabled.store(enabled, Ordering::Release);
        if !enabled {
            let mut state = self
                .state
                .lock()
                .map_err(|_| PlaybackError::StatePoisoned)?;
            state.speech_ticket = None;
            if let Some((_, queue)) = state.pending_stream.take() {
                queue.fail(PlaybackError::Superseded);
            }
            if state
                .current
                .as_ref()
                .is_some_and(|current| current.purpose == PlaybackPurpose::Speech)
            {
                if let Some(current) = state.current.take() {
                    current.playback.stop();
                }
            }
        }
        Ok(())
    }

    pub fn shutdown(&self) {
        if self.shutting_down.swap(true, Ordering::AcqRel) {
            return;
        }
        if let Ok(token) = self.request_suspend() {
            self.try_retire_output(token);
        }
    }
}

/// Operation unlock and close-request admission are serialized under `state`.
/// There is no unchecked gap between the final close check and releasing the
/// operation lock; output drop itself never holds the admission/state lock.
struct PlaybackOperation<'a> {
    owner: &'a AudioPlaybackService,
    guard: Option<MutexGuard<'a, OutputOwnership>>,
}

impl PlaybackOperation<'_> {
    fn release(&mut self) {
        if self
            .guard
            .as_mut()
            .expect("operation guard")
            .epoch
            .take()
            .is_some()
        {
            self.owner.backend.release();
        }
    }

    fn release_through(&mut self, epoch: u64) {
        if self
            .guard
            .as_ref()
            .expect("operation guard")
            .epoch
            .is_some_and(|output| output <= epoch)
        {
            self.release();
        }
    }

    fn retire_pending(&mut self) -> Result<(), PlaybackError> {
        let through = self
            .owner
            .state
            .lock()
            .map_err(|_| PlaybackError::StatePoisoned)?
            .retire_through;
        self.release_through(through);
        Ok(())
    }
}

impl Drop for PlaybackOperation<'_> {
    fn drop(&mut self) {
        loop {
            let state = self
                .owner
                .state
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if self
                .guard
                .as_ref()
                .expect("operation guard")
                .epoch
                .is_some_and(|epoch| epoch <= state.retire_through)
            {
                drop(state);
                self.release();
            } else {
                // Keep admission fenced until the operation mutex is unlocked.
                drop(self.guard.take());
                drop(state);
                break;
            }
        }
    }
}

struct PlaybackWorker<'a>(&'a AtomicUsize);

impl Drop for PlaybackWorker<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

impl PlaybackState {
    fn owns(&self, purpose: PlaybackPurpose, ticket: u64) -> bool {
        match purpose {
            PlaybackPurpose::Cue => self.cue_ticket == Some(ticket),
            PlaybackPurpose::Speech => self.speech_ticket == Some(ticket),
        }
    }

    fn clear_ticket(&mut self, ticket: u64) {
        if self.cue_ticket == Some(ticket) {
            self.cue_ticket = None;
        }
        if self.speech_ticket == Some(ticket) {
            self.speech_ticket = None;
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
    if request.sample_rate_hz > 96_000
        || request.pcm_f32.len() > request.sample_rate_hz as usize * 600
    {
        return Err(PlaybackError::AudioTooLarge);
    }
    if !request.gain_db.is_finite() || !10_f32.powf(request.gain_db / 20.0).is_finite() {
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

#[derive(Default)]
struct RodioBackend {
    // cpal 0.17's stream and rodio's mixer are Send + Sync on supported desktop
    // targets. Initialization is called only by blocking workers, never the UI.
    output: Mutex<Option<RodioOutput>>,
}

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
        let channels = NonZeroU16::new(1).expect("one channel is non-zero");
        let sample_rate =
            NonZeroU32::new(sample_rate_hz).ok_or(PlaybackError::InvalidSampleRate)?;
        self.start_source(
            SamplesBuffer::new(channels, sample_rate, pcm_f32),
            linear_gain,
        )
    }

    fn start_stream(
        &self,
        source: StreamSource,
        linear_gain: f32,
    ) -> Result<Arc<dyn ActivePlayback>, PlaybackError> {
        self.start_source(source, linear_gain)
    }

    fn release(&self) {
        if let Ok(mut output) = self.output.lock() {
            *output = None;
        }
    }
}

impl RodioBackend {
    fn start_source(
        &self,
        source: impl Source + Send + 'static,
        linear_gain: f32,
    ) -> Result<Arc<dyn ActivePlayback>, PlaybackError> {
        let mut output_guard = self
            .output
            .lock()
            .map_err(|_| PlaybackError::StatePoisoned)?;
        if output_guard
            .as_ref()
            .is_some_and(|output| output.failed.load(Ordering::Acquire))
        {
            *output_guard = None;
        }
        // Resolve the default device once per foreground audio-owner lifetime.
        // Suspend/error/shutdown drops it; the next play resolves it again.
        if output_guard.is_none() {
            *output_guard = Some(open_default_output()?);
        }
        let output = output_guard.as_ref().expect("output was opened");
        let drain_duration = output_drain_duration(
            output.sink.config().buffer_size(),
            output.sink.config().sample_rate().get(),
        );
        let player = Player::connect_new(output.sink.mixer());
        player.pause();
        player.set_volume(linear_gain);
        player.append(source);

        Ok(Arc::new(RodioPlayback {
            player,
            cancelled: AtomicBool::new(false),
            output_failed: Arc::clone(&output.failed),
            drain_duration,
        }))
    }
}

fn open_default_output() -> Result<RodioOutput, PlaybackError> {
    let failed = Arc::new(AtomicBool::new(false));
    let failure_flag = Arc::clone(&failed);
    let mut sink = DeviceSinkBuilder::from_default_device()
        .map_err(|_| {
            PlaybackError::OutputUnavailable(String::from("default device is unavailable"))
        })?
        .with_error_callback(move |_| failure_flag.store(true, Ordering::Release))
        .open_sink_or_fallback()
        .map_err(|_| {
            PlaybackError::OutputUnavailable(String::from("default device could not be opened"))
        })?;
    sink.log_on_drop(false);
    Ok(RodioOutput { sink, failed })
}

struct RodioPlayback {
    player: Player,
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
        PlaybackBackend, PlaybackError, PlaybackPurpose, PlaybackRequest,
    };
    use rodio::cpal::BufferSize;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{mpsc, Arc, Condvar, Mutex};
    use std::thread;
    use std::time::{Duration, Instant};

    #[derive(Default)]
    struct FakeBackend {
        admission_barrier: Mutex<Option<Arc<(std::sync::Barrier, std::sync::Barrier)>>>,
        starts: AtomicUsize,
        releases: AtomicUsize,
        fail_start: AtomicBool,
        default_device_generation: AtomicUsize,
        last_linear_gain: Mutex<Option<f32>>,
        started_device_generations: Mutex<Vec<usize>>,
        open_device_generation: Mutex<Option<usize>>,
        players: Mutex<Vec<Arc<FakePlayback>>>,
    }

    impl PlaybackBackend for FakeBackend {
        fn before_admission(&self) {
            let barrier = self.admission_barrier.lock().unwrap().take();
            if let Some(barrier) = barrier {
                barrier.0.wait();
                barrier.1.wait();
            }
        }
        fn start(
            &self,
            _pcm_f32: Vec<f32>,
            _sample_rate_hz: u32,
            linear_gain: f32,
        ) -> Result<Arc<dyn ActivePlayback>, PlaybackError> {
            self.starts.fetch_add(1, Ordering::AcqRel);
            let generation = *self
                .open_device_generation
                .lock()
                .unwrap()
                .get_or_insert_with(|| self.default_device_generation.load(Ordering::Acquire));
            self.started_device_generations
                .lock()
                .expect("device generations lock")
                .push(generation);
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

        fn start_stream(
            &self,
            source: super::StreamSource,
            gain: f32,
        ) -> Result<Arc<dyn ActivePlayback>, PlaybackError> {
            self.start(Vec::new(), source.queue.rate, gain)?;
            let player = self.players.lock().unwrap().last().unwrap().clone();
            *player.source.lock().unwrap() = Some(source);
            Ok(player)
        }

        fn release(&self) {
            self.releases.fetch_add(1, Ordering::AcqRel);
            *self.open_device_generation.lock().unwrap() = None;
        }
    }

    #[derive(Default)]
    struct FakePlayback {
        stop_barrier: Mutex<Option<Arc<(std::sync::Barrier, std::sync::Barrier)>>>,
        activated: AtomicBool,
        stopped: AtomicBool,
        completed: Mutex<bool>,
        changed: Condvar,
        source: Mutex<Option<super::StreamSource>>,
    }

    #[derive(Default)]
    struct BlockingBackend {
        complete_before_return: AtomicBool,
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
            if self.complete_before_return.load(Ordering::Acquire) {
                playback.complete();
            }
            *self.playback.lock().expect("blocking playback lock") = Some(Arc::clone(&playback));
            Ok(playback)
        }

        fn start_stream(
            &self,
            source: super::StreamSource,
            gain: f32,
        ) -> Result<Arc<dyn ActivePlayback>, PlaybackError> {
            self.start(Vec::new(), source.queue.rate, gain)
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
            let barrier = self.stop_barrier.lock().unwrap().take();
            if let Some(barrier) = barrier {
                barrier.0.wait();
                barrier.1.wait();
            }
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

    fn speech_request(service: &AudioPlaybackService) -> PlaybackRequest {
        request(
            service
                .reserve(PlaybackPurpose::Speech)
                .expect("reserve speech"),
        )
    }

    fn speech_play(
        service: &AudioPlaybackService,
        request: PlaybackRequest,
    ) -> Result<super::PlaybackCompletion, PlaybackError> {
        service.play_owned(request, PlaybackPurpose::Speech, || {})
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
    fn streaming_source_is_bounded_and_reports_producer_failure() {
        let queue = Arc::new(super::StreamQueue::new(24_000));
        queue.append(vec![0.25; 100]).unwrap();
        let mut source = super::StreamSource::new(queue.clone());
        assert_eq!(source.next(), Some(0.25));
        queue.fail(PlaybackError::InvalidSample);
        // Failure cuts off even a partially consumed frame.
        assert_eq!(source.next(), None);
        assert_eq!(queue.error(), Some(PlaybackError::InvalidSample));
    }

    #[test]
    fn streaming_source_finish_drains_in_order_without_early_eof() {
        let queue = Arc::new(super::StreamQueue::new(24_000));
        let mut source = super::StreamSource::new(queue.clone());
        assert_eq!(source.next(), Some(0.0)); // Temporary starvation is not EOF.
        queue.append(vec![0.25, 0.5]).unwrap();
        queue.close().unwrap();
        let samples: Vec<_> = source.collect();
        assert!(samples.ends_with(&[0.25, 0.5]));
        assert_eq!(queue.error(), None);
    }

    #[test]
    fn streaming_finish_waits_for_source_and_native_drain() {
        let backend = Arc::new(FakeBackend::default());
        let owner = Arc::new(AudioPlaybackService::with_backend(backend.clone()));
        let id = owner.reserve(PlaybackPurpose::Speech).unwrap();
        let stream = owner.open_stream_owned(id, 24_000, 0.0, || {}).unwrap();
        stream.append(vec![0.25; 240]).unwrap();
        let player = wait_for_player(&backend, 0);
        let finishing = stream.clone();
        let finish = thread::spawn(move || finishing.finish());
        // Drive the real source, but keep the simulated native drain outstanding.
        let mut source = loop {
            if let Some(source) = player.source.lock().unwrap().take() {
                break source;
            }
            thread::yield_now();
        };
        let mut nonzero = 0;
        for sample in source.by_ref() {
            if sample != 0.0 {
                nonzero += 1;
            }
        }
        assert_eq!(nonzero, 240);
        assert!(!finish.is_finished());
        assert!(owner.owns(id, PlaybackPurpose::Speech).unwrap());
        player.complete();
        assert_eq!(finish.join().unwrap().unwrap().duration_ms, 10);
        assert_eq!(owner.workers.load(Ordering::Acquire), 0);
        assert!(!owner.owns(id, PlaybackPurpose::Speech).unwrap());
    }

    #[test]
    fn streaming_cancel_during_init_wakes_full_queue_and_fences_late_frames() {
        let backend = Arc::new(BlockingBackend::default());
        let owner = Arc::new(AudioPlaybackService::with_backend(backend.clone()));
        let id = owner.reserve(PlaybackPurpose::Speech).unwrap();
        let stream = owner
            .open_stream_owned(id, 100, 0.0, || panic!("late activation"))
            .unwrap();
        backend.wait_for_start();
        stream.append(vec![0.25; 100]).unwrap();
        let writing = stream.clone();
        let (sent, received) = mpsc::sync_channel(1);
        let append = thread::spawn(move || {
            let result = writing.append(vec![0.5]);
            sent.send(result.clone()).unwrap();
            result
        });
        assert_eq!(
            received.recv_timeout(Duration::from_millis(20)),
            Err(mpsc::RecvTimeoutError::Timeout)
        );
        assert!(owner.cancel_owned(id, PlaybackPurpose::Speech).unwrap());
        assert_eq!(append.join().unwrap(), Err(PlaybackError::Superseded));
        assert!(
            stream.0.queue.state.lock().unwrap().result.is_none(),
            "cancel is not initialization retirement"
        );
        let newer = owner.reserve(PlaybackPurpose::Speech).unwrap();
        assert_eq!(stream.append(vec![0.5]), Err(PlaybackError::Superseded));
        backend.release_start();
        assert_eq!(stream.wait(), Err(PlaybackError::Superseded));
        assert!(!backend
            .prepared_playback()
            .activated
            .load(Ordering::Acquire));
        assert!(owner.owns(newer, PlaybackPurpose::Speech).unwrap());
        assert_eq!(owner.workers.load(Ordering::Acquire), 0);
    }

    #[test]
    fn streaming_producer_failure_and_drop_stop_partial_output() {
        for drop_producer in [false, true] {
            let backend = Arc::new(FakeBackend::default());
            let owner = Arc::new(AudioPlaybackService::with_backend(backend.clone()));
            let id = owner.reserve(PlaybackPurpose::Speech).unwrap();
            let stream = owner.open_stream_owned(id, 24_000, 0.0, || {}).unwrap();
            stream.append(vec![0.25; 240]).unwrap();
            let queue = stream.0.queue.clone();
            let player = wait_for_player(&backend, 0);
            let mut source = loop {
                if let Some(source) = player.source.lock().unwrap().take() {
                    break source;
                }
                thread::yield_now();
            };
            assert_eq!(source.next(), Some(0.25));
            if drop_producer {
                drop(stream);
            } else {
                stream.fail();
            }
            assert_eq!(source.next(), None);
            // Fake backend's output/drain completion is explicitly controlled.
            player.complete();
            let mut state = queue.state.lock().unwrap();
            while state.result.is_none() {
                state = queue.changed.wait(state).unwrap();
            }
            assert!(state.result.as_ref().unwrap().is_err());
            assert_eq!(owner.workers.load(Ordering::Acquire), 0);
        }
    }

    #[test]
    fn streaming_starvation_and_validation_fail_closed() {
        let queue = Arc::new(super::StreamQueue::new(24_000));
        let mut source = super::StreamSource::new(queue.clone());
        source.empty_since = Instant::now() - super::STREAM_WAIT_LIMIT;
        assert_eq!(source.next(), None);
        assert_eq!(queue.error(), Some(PlaybackError::TimedOut));
        for (frame, expected) in [
            (vec![], PlaybackError::EmptyAudio),
            (vec![f32::NAN], PlaybackError::InvalidSample),
            (
                vec![0.0; super::STREAM_FRAME_SAMPLES + 1],
                PlaybackError::AudioTooLarge,
            ),
        ] {
            let queue = Arc::new(super::StreamQueue::new(24_000));
            assert_eq!(queue.append(frame), Err(expected.clone()));
            assert_eq!(queue.error(), Some(expected));
        }
        let queue = Arc::new(super::StreamQueue::new(24_000));
        queue.state.lock().unwrap().total = 24_000 * 600;
        assert_eq!(queue.append(vec![0.0]), Err(PlaybackError::AudioTooLarge));
    }

    #[test]
    fn streaming_full_queue_times_out_and_consumer_never_waits_for_producer_lock() {
        let queue = Arc::new(super::StreamQueue::new(100));
        queue.append(vec![0.25; 100]).unwrap();
        assert_eq!(
            queue.append_until(vec![0.5], Instant::now()),
            Err(PlaybackError::TimedOut)
        );
        assert!(queue.aborted.load(Ordering::Acquire));

        let queue = Arc::new(super::StreamQueue::new(24_000));
        queue.append(vec![0.25; 100]).unwrap();
        let mut source = super::StreamSource::new(queue.clone());
        {
            let _producer_lock = queue.state.lock().unwrap();
            assert_eq!(source.next(), Some(0.0));
        }
        source.silence = 0;
        assert_eq!(source.next(), Some(0.25));
        let _producer_lock = queue.state.lock().unwrap();
        for _ in 0..99 {
            assert_eq!(source.next(), Some(0.25));
        }
    }

    #[test]
    fn streaming_full_queue_unblocks_when_consumer_takes_a_frame() {
        let queue = Arc::new(super::StreamQueue::new(100));
        queue.append(vec![0.25; 100]).unwrap();
        let writing = queue.clone();
        let (sent, received) = mpsc::sync_channel(1);
        let append = thread::spawn(move || {
            sent.send(writing.append(vec![0.5])).unwrap();
        });
        assert_eq!(
            received.recv_timeout(Duration::from_millis(20)),
            Err(mpsc::RecvTimeoutError::Timeout)
        );
        let mut source = super::StreamSource::new(queue.clone());
        assert_eq!(source.next(), Some(0.25));
        assert_eq!(
            received.recv_timeout(Duration::from_secs(1)).unwrap(),
            Ok(())
        );
        append.join().unwrap();
        queue.close().unwrap();
        assert_eq!(source.last(), Some(0.5));
    }

    #[test]
    fn streaming_metadata_and_empty_producer_do_not_activate_output() {
        let backend = Arc::new(FakeBackend::default());
        let owner = Arc::new(AudioPlaybackService::with_backend(backend.clone()));
        let id = owner.reserve(PlaybackPurpose::Speech).unwrap();
        for (rate, gain, error) in [
            (0, 0.0, PlaybackError::InvalidSampleRate),
            (96_001, 0.0, PlaybackError::AudioTooLarge),
            (24_000, f32::NAN, PlaybackError::InvalidGain),
            (24_000, f32::MAX, PlaybackError::InvalidGain),
        ] {
            assert!(matches!(owner.open_stream_owned(id, rate, gain, || {}), Err(e) if e == error));
        }
        assert_eq!(backend.starts.load(Ordering::Acquire), 0);
        let queue = Arc::new(super::StreamQueue::new(24_000));
        queue.close().unwrap();
        assert_eq!(queue.error(), Some(PlaybackError::EmptyAudio));
        assert_eq!(super::StreamSource::new(queue).next(), None);
    }

    #[test]
    fn streaming_replacement_suspend_and_duplicate_preserve_ticket_ownership() {
        let backend = Arc::new(BlockingBackend::default());
        let owner = Arc::new(AudioPlaybackService::with_backend(backend.clone()));
        let id = owner.reserve(PlaybackPurpose::Speech).unwrap();
        let stream = owner
            .open_stream_owned(id, 24_000, 0.0, || panic!("late activation"))
            .unwrap();
        backend.wait_for_start();
        assert!(matches!(
            owner.open_stream_owned(id, 24_000, 0.0, || {}),
            Err(PlaybackError::Superseded)
        ));
        owner.retire_unstarted(id, PlaybackPurpose::Speech).unwrap();
        assert!(owner.owns(id, PlaybackPurpose::Speech).unwrap());
        owner.suspend().unwrap();
        owner.resume().unwrap();
        let newer = owner.reserve(PlaybackPurpose::Speech).unwrap();
        backend.release_start();
        assert_eq!(stream.wait(), Err(PlaybackError::Superseded));
        assert!(owner.owns(newer, PlaybackPurpose::Speech).unwrap());
        assert_eq!(backend.releases.load(Ordering::Acquire), 1);
    }

    #[test]
    fn validates_requests_before_opening_output() {
        let backend = Arc::new(FakeBackend::default());
        let service = AudioPlaybackService::with_backend(backend.clone());

        let mut invalid = speech_request(&service);
        invalid.pcm_f32.clear();
        assert_eq!(
            speech_play(&service, invalid),
            Err(PlaybackError::EmptyAudio)
        );
        let mut invalid = speech_request(&service);
        invalid.pcm_f32[0] = f32::NAN;
        assert_eq!(
            speech_play(&service, invalid),
            Err(PlaybackError::InvalidSample)
        );
        let mut invalid = speech_request(&service);
        invalid.gain_db = f32::INFINITY;
        assert_eq!(
            speech_play(&service, invalid),
            Err(PlaybackError::InvalidGain)
        );
        let mut invalid = speech_request(&service);
        invalid.sample_rate_hz = 1;
        invalid.pcm_f32 = vec![0.0; 601];
        assert_eq!(
            speech_play(&service, invalid),
            Err(PlaybackError::AudioTooLarge)
        );
        assert_eq!(backend.starts.load(Ordering::Acquire), 0);
    }

    #[test]
    fn completes_playback_and_reports_duration() {
        let backend = Arc::new(FakeBackend::default());
        let service = Arc::new(AudioPlaybackService::with_backend(backend.clone()));
        let worker_service = Arc::clone(&service);
        let req = speech_request(&service);
        let id = req.playback_id;
        let worker = thread::spawn(move || speech_play(&worker_service, req));
        let player = wait_for_player(&backend, 0);

        player.complete();

        let completion = worker.join().expect("playback thread").expect("playback");
        assert_eq!(completion.playback_id, id);
        assert_eq!(completion.duration_ms, 1);
        let gain = backend
            .last_linear_gain
            .lock()
            .expect("gain lock")
            .expect("gain should be captured");
        assert!((gain - 10_f32.powf(3.0 / 20.0)).abs() < 0.000_1);
    }

    #[test]
    fn output_open_failure_is_retryable_with_a_fresh_owned_ticket() {
        let backend = Arc::new(FakeBackend::default());
        backend.fail_start.store(true, Ordering::Release);
        let service = Arc::new(AudioPlaybackService::with_backend(backend.clone()));

        assert!(matches!(
            speech_play(&service, speech_request(&service)),
            Err(PlaybackError::OutputUnavailable(_))
        ));

        backend.fail_start.store(false, Ordering::Release);
        let worker_service = Arc::clone(&service);
        let req = speech_request(&service);
        let worker = thread::spawn(move || speech_play(&worker_service, req));
        let player = wait_for_player(&backend, 0);
        player.complete();
        assert!(worker.join().expect("playback thread").is_ok());
    }

    #[test]
    fn newer_playback_supersedes_and_stops_older_playback() {
        let backend = Arc::new(FakeBackend::default());
        let service = Arc::new(AudioPlaybackService::with_backend(backend.clone()));
        let first_service = Arc::clone(&service);
        let req = speech_request(&service);
        let first = thread::spawn(move || speech_play(&first_service, req));
        let first_player = wait_for_player(&backend, 0);
        let second_service = Arc::clone(&service);
        let req = speech_request(&service);
        let second = thread::spawn(move || speech_play(&second_service, req));
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
        let req = speech_request(&service);
        let duplicate = req.clone();
        let worker = thread::spawn(move || speech_play(&worker_service, req));
        let player = wait_for_player(&backend, 0);

        assert_eq!(
            speech_play(&service, duplicate),
            Err(PlaybackError::Superseded)
        );
        assert_eq!(backend.starts.load(Ordering::Acquire), 1);

        player.complete();
        assert!(worker.join().expect("playback thread").is_ok());
    }

    #[test]
    fn stale_cancel_does_not_stop_newer_playback() {
        let backend = Arc::new(FakeBackend::default());
        let service = Arc::new(AudioPlaybackService::with_backend(backend.clone()));
        let worker_service = Arc::clone(&service);
        let old = speech_request(&service).playback_id;
        let req = speech_request(&service);
        let worker = thread::spawn(move || speech_play(&worker_service, req));
        let player = wait_for_player(&backend, 0);

        assert!(!service
            .cancel_owned(old, PlaybackPurpose::Speech)
            .expect("stale cancellation"));
        assert!(!player.stopped.load(Ordering::Acquire));
        player.complete();
        assert!(worker.join().expect("playback thread").is_ok());
    }

    #[test]
    fn matching_cancel_stops_playback_and_wakes_waiter() {
        let backend = Arc::new(FakeBackend::default());
        let service = Arc::new(AudioPlaybackService::with_backend(backend.clone()));
        let worker_service = Arc::clone(&service);
        let req = speech_request(&service);
        let id = req.playback_id;
        let worker = thread::spawn(move || speech_play(&worker_service, req));
        let player = wait_for_player(&backend, 0);

        assert!(service
            .cancel_owned(id, PlaybackPurpose::Speech)
            .expect("matching cancellation"));
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
        let req = speech_request(&service);
        let worker = thread::spawn(move || speech_play(&worker_service, req));
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

        let req = speech_request(&service);
        service.shutdown();

        assert_eq!(speech_play(&service, req), Err(PlaybackError::ShuttingDown));
        assert_eq!(
            service.set_speech_enabled(true),
            Err(PlaybackError::ShuttingDown)
        );
        assert_eq!(backend.releases.load(Ordering::Acquire), 0);
    }

    #[test]
    fn shutdown_is_bounded_while_output_start_is_blocked() {
        let backend = Arc::new(BlockingBackend::default());
        let service = Arc::new(AudioPlaybackService::with_backend(backend.clone()));
        let playback_service = Arc::clone(&service);
        let req = speech_request(&service);
        let delayed = req.clone();
        let playback = thread::spawn(move || speech_play(&playback_service, req));
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
        assert_eq!(
            speech_play(&service, delayed),
            Err(PlaybackError::ShuttingDown)
        );
        assert_eq!(backend.releases.load(Ordering::Acquire), 1);
    }

    #[test]
    fn shutdown_waits_for_short_state_contention_and_stops_playback() {
        let backend = Arc::new(FakeBackend::default());
        let service = Arc::new(AudioPlaybackService::with_backend(backend.clone()));
        let playback_service = Arc::clone(&service);
        let req = speech_request(&service);
        let (tx, rx) = mpsc::sync_channel(1);
        let playback = thread::spawn(move || {
            playback_service.play_owned(req, PlaybackPurpose::Speech, || {
                tx.send(()).unwrap();
            })
        });
        rx.recv_timeout(Duration::from_secs(1)).unwrap();
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

        let req = speech_request(&service);
        service.suspend().expect("suspend playback");
        assert_eq!(
            speech_play(&service, req.clone()),
            Err(PlaybackError::Suspended)
        );
        service.resume().expect("resume playback");
        assert_eq!(speech_play(&service, req), Err(PlaybackError::Superseded));
        assert_eq!(backend.starts.load(Ordering::Acquire), 0);
    }

    #[test]
    fn delayed_start_stays_invalid_after_suspend_and_resume() {
        let backend = Arc::new(BlockingBackend::default());
        let service = Arc::new(AudioPlaybackService::with_backend(backend.clone()));
        let playback_service = Arc::clone(&service);
        let req = speech_request(&service);
        let playback = thread::spawn(move || speech_play(&playback_service, req));
        backend.wait_for_start();

        service.suspend().expect("suspend playback");
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
    fn output_lifetime_contract_reuses_default_until_foreground_retirement() {
        let backend = Arc::new(FakeBackend::default());
        backend
            .default_device_generation
            .store(1, Ordering::Release);
        let service = Arc::new(AudioPlaybackService::with_backend(backend.clone()));

        let first_service = Arc::clone(&service);
        let req = speech_request(&service);
        let first = thread::spawn(move || speech_play(&first_service, req));
        wait_for_player(&backend, 0).complete();
        first.join().expect("first playback").expect("first result");

        backend
            .default_device_generation
            .store(2, Ordering::Release);
        let second_service = Arc::clone(&service);
        let req = speech_request(&service);
        let second = thread::spawn(move || speech_play(&second_service, req));
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
            vec![1, 1]
        );
        service.suspend().unwrap();
        service.resume().unwrap();
        let owner = service.clone();
        let req = speech_request(&service);
        let third = thread::spawn(move || speech_play(&owner, req));
        wait_for_player(&backend, 2).complete();
        third.join().unwrap().unwrap();
        assert_eq!(
            *backend.started_device_generations.lock().unwrap(),
            vec![1, 1, 2]
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

    #[test]
    fn tts_disabled_cues_activate_before_completion_and_keep_ownership() {
        let backend = Arc::new(FakeBackend::default());
        let service = Arc::new(AudioPlaybackService::with_backend(backend.clone()));
        service.set_speech_enabled(false).unwrap();
        assert_eq!(
            service.reserve(PlaybackPurpose::Speech),
            Err(PlaybackError::Suspended)
        );
        let cue = service.reserve(PlaybackPurpose::Cue).unwrap();
        let (tx, rx) = mpsc::sync_channel(1);
        let owner = service.clone();
        let worker = thread::spawn(move || {
            owner.play_owned(request(cue), PlaybackPurpose::Cue, || {
                tx.send(()).unwrap();
            })
        });
        rx.recv_timeout(Duration::from_secs(1))
            .expect("activation receipt");
        assert!(!worker.is_finished(), "activation must not mean completion");
        assert!(service.owns(cue, PlaybackPurpose::Cue).unwrap());
        // Enabling/disabling speech cannot stop active cues.
        service.set_speech_enabled(true).unwrap();
        service.set_speech_enabled(false).unwrap();
        let player = wait_for_player(&backend, 0);
        assert!(!player.stopped.load(Ordering::Acquire));
        player.complete();
        assert!(worker.join().unwrap().is_ok());
        assert!(!service.owns(cue, PlaybackPurpose::Cue).unwrap());
    }

    #[test]
    fn purpose_policy_and_old_cue_stop_cannot_cancel_new_speech() {
        let backend = Arc::new(FakeBackend::default());
        let service = Arc::new(AudioPlaybackService::with_backend(backend.clone()));
        let old_cue = service.reserve(PlaybackPurpose::Cue).unwrap();
        let newer_cue = service.reserve(PlaybackPurpose::Cue).unwrap();
        assert!(newer_cue > old_cue);
        assert_eq!(
            service.play_owned(request(old_cue), PlaybackPurpose::Cue, || panic!(
                "stale activation"
            )),
            Err(PlaybackError::Superseded)
        );
        let speech = service.reserve(PlaybackPurpose::Speech).unwrap();
        assert!(speech > newer_cue);
        assert_eq!(
            service.reserve(PlaybackPurpose::Cue),
            Err(PlaybackError::Superseded)
        );
        let owner = service.clone();
        let worker = thread::spawn(move || {
            owner.play_owned(request(speech), PlaybackPurpose::Speech, || {})
        });
        let player = wait_for_player(&backend, 0);
        assert!(!service.cancel_owned(old_cue, PlaybackPurpose::Cue).unwrap());
        assert!(!service
            .cancel_owned(newer_cue, PlaybackPurpose::Cue)
            .unwrap());
        assert!(!service.cancel_owned(speech, PlaybackPurpose::Cue).unwrap());
        assert!(!player.stopped.load(Ordering::Acquire));
        player.complete();
        assert!(worker.join().unwrap().is_ok());
    }

    #[test]
    fn blocked_cue_initialization_cannot_activate_after_owned_stop() {
        let backend = Arc::new(BlockingBackend::default());
        let service = Arc::new(AudioPlaybackService::with_backend(backend.clone()));
        let cue = service.reserve(PlaybackPurpose::Cue).unwrap();
        let owner = service.clone();
        let worker = thread::spawn(move || {
            owner.play_owned(request(cue), PlaybackPurpose::Cue, || {
                panic!("late activation")
            })
        });
        backend.wait_for_start();
        assert!(service.cancel_owned(cue, PlaybackPurpose::Cue).unwrap());
        backend.release_start();
        assert_eq!(worker.join().unwrap(), Err(PlaybackError::Superseded));
        assert!(!backend
            .prepared_playback()
            .activated
            .load(Ordering::Acquire));
        assert!(backend.prepared_playback().stopped.load(Ordering::Acquire));
    }

    #[test]
    fn speech_reservation_supersedes_blocked_cue_without_late_activation() {
        let backend = Arc::new(BlockingBackend::default());
        let service = Arc::new(AudioPlaybackService::with_backend(backend.clone()));
        let cue = service.reserve(PlaybackPurpose::Cue).unwrap();
        let owner = service.clone();
        let worker = thread::spawn(move || {
            owner.play_owned(request(cue), PlaybackPurpose::Cue, || {
                panic!("late activation")
            })
        });
        backend.wait_for_start();
        let speech = service.reserve(PlaybackPurpose::Speech).unwrap();
        backend.release_start();
        assert_eq!(worker.join().unwrap(), Err(PlaybackError::Superseded));
        assert!(service.owns(speech, PlaybackPurpose::Speech).unwrap());
        assert!(!backend
            .prepared_playback()
            .activated
            .load(Ordering::Acquire));
    }

    #[test]
    fn foreground_retirement_invalidates_blocked_cue_even_after_resume() {
        let backend = Arc::new(BlockingBackend::default());
        let service = Arc::new(AudioPlaybackService::with_backend(backend.clone()));
        let cue = service.reserve(PlaybackPurpose::Cue).unwrap();
        let owner = service.clone();
        let worker = thread::spawn(move || {
            owner.play_owned(request(cue), PlaybackPurpose::Cue, || {
                panic!("late activation")
            })
        });
        backend.wait_for_start();
        service.suspend().unwrap();
        service.resume().unwrap();
        backend.release_start();
        assert_eq!(worker.join().unwrap(), Err(PlaybackError::Superseded));
        assert!(!backend
            .prepared_playback()
            .activated
            .load(Ordering::Acquire));
        assert_eq!(backend.releases.load(Ordering::Acquire), 1);
    }

    #[test]
    fn shutdown_retires_blocked_owned_cue_without_activation() {
        let backend = Arc::new(BlockingBackend::default());
        let service = Arc::new(AudioPlaybackService::with_backend(backend.clone()));
        let cue = service.reserve(PlaybackPurpose::Cue).unwrap();
        let owner = service.clone();
        let worker = thread::spawn(move || {
            owner.play_owned(request(cue), PlaybackPurpose::Cue, || {
                panic!("late activation")
            })
        });
        backend.wait_for_start();
        service.shutdown();
        backend.release_start();
        assert_eq!(worker.join().unwrap(), Err(PlaybackError::ShuttingDown));
        assert!(!backend
            .prepared_playback()
            .activated
            .load(Ordering::Acquire));
    }

    #[test]
    fn disabling_then_enabling_speech_cannot_reactivate_blocked_speech_or_retire_a_cue() {
        let backend = Arc::new(BlockingBackend::default());
        let service = Arc::new(AudioPlaybackService::with_backend(backend.clone()));
        let speech = service.reserve(PlaybackPurpose::Speech).unwrap();
        let owner = service.clone();
        let worker = thread::spawn(move || {
            owner.play_owned(request(speech), PlaybackPurpose::Speech, || {
                panic!("late speech activation")
            })
        });
        backend.wait_for_start();
        service.set_speech_enabled(false).unwrap();
        let cue = service.reserve(PlaybackPurpose::Cue).unwrap();
        service.set_speech_enabled(true).unwrap();
        backend.release_start();
        assert_eq!(worker.join().unwrap(), Err(PlaybackError::Superseded));
        assert!(service.owns(cue, PlaybackPurpose::Cue).unwrap());
        assert!(!backend
            .prepared_playback()
            .activated
            .load(Ordering::Acquire));
    }

    #[test]
    fn failed_owned_start_retires_ticket_and_is_retryable_with_new_ticket() {
        let backend = Arc::new(FakeBackend::default());
        backend.fail_start.store(true, Ordering::Release);
        let service = AudioPlaybackService::with_backend(backend);
        let cue = service.reserve(PlaybackPurpose::Cue).unwrap();
        assert!(matches!(
            service.play_owned(request(cue), PlaybackPurpose::Cue, || panic!(
                "failed activation"
            )),
            Err(PlaybackError::OutputUnavailable(_))
        ));
        assert!(!service.owns(cue, PlaybackPurpose::Cue).unwrap());
        assert!(service.reserve(PlaybackPurpose::Cue).unwrap() > cue);
    }

    #[test]
    fn duplicate_owned_activation_does_not_retire_the_active_owner() {
        let backend = Arc::new(FakeBackend::default());
        let service = Arc::new(AudioPlaybackService::with_backend(backend.clone()));
        let cue = service.reserve(PlaybackPurpose::Cue).unwrap();
        let (tx, rx) = mpsc::sync_channel(1);
        let owner = service.clone();
        let worker = thread::spawn(move || {
            owner.play_owned(request(cue), PlaybackPurpose::Cue, || {
                tx.send(()).unwrap();
            })
        });
        rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert_eq!(
            service.play_owned(request(cue), PlaybackPurpose::Cue, || panic!(
                "duplicate activation"
            )),
            Err(PlaybackError::Superseded)
        );
        assert!(service.owns(cue, PlaybackPurpose::Cue).unwrap());
        let player = wait_for_player(&backend, 0);
        assert!(!player.stopped.load(Ordering::Acquire));
        player.complete();
        assert!(worker.join().unwrap().is_ok());
    }

    #[test]
    fn excess_workers_fail_admission_without_waiting_for_output_initialization() {
        let backend = Arc::new(BlockingBackend::default());
        let service = Arc::new(AudioPlaybackService::with_backend(backend.clone()));
        let first_owner = service.clone();
        let req = speech_request(&service);
        let first = thread::spawn(move || speech_play(&first_owner, req));
        backend.wait_for_start();
        // Simulate the two other admitted operations, without creating scheduling
        // dependencies or sleeping to observe blocked worker counts.
        service.workers.store(3, Ordering::Release);
        assert_eq!(
            speech_play(&service, speech_request(&service)),
            Err(PlaybackError::Busy)
        );
        service.workers.store(1, Ordering::Release);
        service.shutdown();
        backend.release_start();
        assert_eq!(first.join().unwrap(), Err(PlaybackError::ShuttingDown));
        assert_eq!(service.workers.load(Ordering::Acquire), 0);
    }

    #[test]
    fn global_owned_tickets_never_wrap_or_exceed_javascript_safe_integers() {
        let service = AudioPlaybackService::with_backend(Arc::new(FakeBackend::default()));
        service.state.lock().unwrap().next_ticket = super::MAX_PLAYBACK_ID - 1;
        assert_eq!(
            service.reserve(PlaybackPurpose::Cue).unwrap(),
            super::MAX_PLAYBACK_ID
        );
        assert_eq!(
            service.reserve(PlaybackPurpose::Speech),
            Err(PlaybackError::InvalidPlaybackId)
        );
    }

    #[test]
    fn cached_output_is_retired_when_operation_exits_at_early_admission_check() {
        for shutdown in [false, true] {
            let backend = Arc::new(FakeBackend::default());
            let service = Arc::new(AudioPlaybackService::with_backend(backend.clone()));
            let first = service.reserve(PlaybackPurpose::Cue).unwrap();
            let owner = service.clone();
            let worker = thread::spawn(move || {
                owner.play_owned(request(first), PlaybackPurpose::Cue, || {})
            });
            wait_for_player(&backend, 0).complete();
            worker.join().unwrap().unwrap();
            let next = service.reserve(PlaybackPurpose::Cue).unwrap();
            let barrier = Arc::new((std::sync::Barrier::new(2), std::sync::Barrier::new(2)));
            *backend.admission_barrier.lock().unwrap() = Some(barrier.clone());
            let owner = service.clone();
            let worker = thread::spawn(move || {
                owner.play_owned(request(next), PlaybackPurpose::Cue, || {
                    panic!("late activation")
                })
            });
            barrier.0.wait(); // Operation lock held; replacement has not acquired state.
            if shutdown {
                service.shutdown();
            } else {
                service.suspend().unwrap();
            }
            barrier.1.wait();
            let result = worker.join().unwrap();
            assert_eq!(
                result,
                Err(if shutdown {
                    PlaybackError::ShuttingDown
                } else {
                    PlaybackError::Suspended
                })
            );
            assert!(
                backend.open_device_generation.lock().unwrap().is_none(),
                "cached output leaked after early return"
            );
        }
    }

    #[test]
    fn old_slow_suspend_cannot_release_resumed_output() {
        let backend = Arc::new(FakeBackend::default());
        let service = Arc::new(AudioPlaybackService::with_backend(backend.clone()));
        let cue = service.reserve(PlaybackPurpose::Cue).unwrap();
        let owner = service.clone();
        let first =
            thread::spawn(move || owner.play_owned(request(cue), PlaybackPurpose::Cue, || {}));
        let player = wait_for_player(&backend, 0);
        let barrier = Arc::new((std::sync::Barrier::new(2), std::sync::Barrier::new(2)));
        *player.stop_barrier.lock().unwrap() = Some(barrier.clone());
        let owner = service.clone();
        let suspend = thread::spawn(move || owner.suspend());
        barrier.0.wait(); // Close admission/state cleanup finished; stop is delayed.
        service.resume().unwrap();
        backend
            .default_device_generation
            .store(2, Ordering::Release);
        let cue = service.reserve(PlaybackPurpose::Cue).unwrap();
        let owner = service.clone();
        let (tx, rx) = mpsc::sync_channel(1);
        let second = thread::spawn(move || {
            owner.play_owned(request(cue), PlaybackPurpose::Cue, || {
                tx.send(()).unwrap();
            })
        });
        rx.recv_timeout(Duration::from_secs(1)).unwrap();
        barrier.1.wait();
        suspend.join().unwrap().unwrap();
        let still_open = backend.open_device_generation.lock().unwrap().is_some();
        wait_for_player(&backend, 1).complete();
        first.join().unwrap().unwrap_err();
        second.join().unwrap().unwrap();
        assert!(still_open, "old suspend closed resumed output");
    }

    #[test]
    fn blocked_old_startup_cannot_activate_after_new_speech_reservation() {
        let backend = Arc::new(BlockingBackend::default());
        backend
            .complete_before_return
            .store(true, Ordering::Release);
        let service = Arc::new(AudioPlaybackService::with_backend(backend.clone()));
        let owner = service.clone();
        let req = speech_request(&service);
        let old = thread::spawn(move || speech_play(&owner, req));
        backend.wait_for_start();
        let newer = service.reserve(PlaybackPurpose::Speech).unwrap();
        backend.release_start();
        assert_eq!(old.join().unwrap(), Err(PlaybackError::Superseded));
        assert!(!backend
            .prepared_playback()
            .activated
            .load(Ordering::Acquire));
        assert!(service.owns(newer, PlaybackPurpose::Speech).unwrap());
    }

    #[test]
    fn activation_notification_can_reenter_ownership_and_cancellation() {
        let backend = Arc::new(FakeBackend::default());
        let service = Arc::new(AudioPlaybackService::with_backend(backend));
        let cue = service.reserve(PlaybackPurpose::Cue).unwrap();
        let owner = service.clone();
        let callback_owner = service.clone();
        let (tx, rx) = mpsc::sync_channel(1);
        let worker = thread::spawn(move || {
            owner.play_owned(request(cue), PlaybackPurpose::Cue, || {
                assert!(callback_owner.owns(cue, PlaybackPurpose::Cue).unwrap());
                assert!(callback_owner
                    .cancel_owned(cue, PlaybackPurpose::Cue)
                    .unwrap());
                let newer = callback_owner.reserve(PlaybackPurpose::Speech).unwrap();
                tx.send(newer).unwrap();
            })
        });
        let newer = rx
            .recv_timeout(Duration::from_secs(1))
            .expect("callback reentry must not deadlock");
        assert_eq!(worker.join().unwrap(), Err(PlaybackError::Superseded));
        assert!(service.owns(newer, PlaybackPurpose::Speech).unwrap());
    }

    #[test]
    fn explicit_old_retirement_ack_does_not_close_a_new_output() {
        let backend = Arc::new(FakeBackend::default());
        let service = Arc::new(AudioPlaybackService::with_backend(backend.clone()));
        let req = speech_request(&service);
        let owner = service.clone();
        let first = thread::spawn(move || speech_play(&owner, req));
        wait_for_player(&backend, 0).complete();
        first.join().unwrap().unwrap();
        let token = service.request_suspend().unwrap();
        assert_eq!(
            service.reserve(PlaybackPurpose::Cue),
            Err(PlaybackError::Suspended)
        );
        assert!(
            backend.open_device_generation.lock().unwrap().is_some(),
            "quick admission close must not drop output"
        );
        service.resume().unwrap();
        let req = speech_request(&service);
        let owner = service.clone();
        let (tx, rx) = mpsc::sync_channel(1);
        let second = thread::spawn(move || {
            owner.play_owned(req, PlaybackPurpose::Speech, || {
                tx.send(()).unwrap();
            })
        });
        rx.recv_timeout(Duration::from_secs(1)).unwrap();
        service.retire_output(token).unwrap();
        assert!(backend.open_device_generation.lock().unwrap().is_some());
        wait_for_player(&backend, 1).complete();
        second.join().unwrap().unwrap();
        assert_eq!(backend.releases.load(Ordering::Acquire), 1);
    }
}
