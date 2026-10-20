//! Desktop-local frame admission and worker lifetime. Inference and turn policy
//! belong to the caller; this module never interprets PCM or runtime phases.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Condvar, Mutex, Weak};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const QUEUE_CAPACITY: usize = 32;
const SAMPLE_RATE_HZ: u128 = 16_000;
const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;
const RECEIVE_INTERVAL: Duration = Duration::from_millis(10);
const RETIREMENT_TIMEOUT: Duration = Duration::from_millis(250);
const PENDING_RETIREMENT: &str = "Native voice worker retirement is pending";
const OVERFLOW: &str = "Native microphone processing fell behind";
// Producer health, not inference latency or acoustic silence. An installed
// stream gets longer to deliver its first frame than an established producer.
const FIRST_FRAME_GRACE: Duration = Duration::from_secs(5);
const PRODUCER_ARRIVAL_GAP: Duration = Duration::from_secs(2);
const PRODUCER_STALLED: &str = "Native microphone frame delivery stalled";

type MonotonicNow = Arc<dyn Fn() -> Instant + Send + Sync>;

#[derive(Clone, Copy)]
struct ProducerTimeouts {
    first_frame: Duration,
    arrival_gap: Duration,
}

struct ProducerProgress {
    started_at: Instant,
    last_arrival: Option<Instant>,
}

impl ProducerProgress {
    fn stalled(&self, now: Instant, timeouts: ProducerTimeouts) -> bool {
        let (since, timeout) = match self.last_arrival {
            Some(arrival) => (arrival, timeouts.arrival_gap),
            None => (self.started_at, timeouts.first_frame),
        };
        now.saturating_duration_since(since) >= timeout
    }
}

type FrameCallback = Arc<dyn Fn(VoiceFrame) -> Result<(), String> + Send + Sync>;
type FailureCallback = Arc<dyn Fn(u64, String) + Send + Sync>;

#[derive(Debug)]
pub struct VoiceFrame {
    pub capture_id: u64,
    pub epoch: u64,
    pub sample_end_ms: u64,
    pub samples: Vec<f32>,
}

/// Resampled 16-kHz mono audio time, independent of worker scheduling delay.
#[derive(Debug, Default)]
pub struct FrameSampleClock {
    samples_seen: u64,
    elapsed_ms: u64,
}

impl FrameSampleClock {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn advance(&mut self, sample_count: usize) -> Result<u64, String> {
        if sample_count == 0 {
            return Err(String::from("Native voice frame is empty"));
        }
        let count = u64::try_from(sample_count)
            .map_err(|_| String::from("Native voice sample clock exhausted"))?;
        let next = self
            .samples_seen
            .checked_add(count)
            .filter(|next| *next <= MAX_SAFE_INTEGER)
            .ok_or_else(|| String::from("Native voice sample clock exhausted"))?;
        self.samples_seen = next;
        self.elapsed_ms = ((u128::from(next) * 1_000) / SAMPLE_RATE_HZ) as u64;
        Ok(self.now_ms())
    }

    pub fn now_ms(&self) -> u64 {
        self.elapsed_ms
    }
}

struct QueuedFrame {
    epoch: u64,
    samples: Vec<f32>,
}

struct Generation {
    closed: AtomicBool,
    failure: Mutex<Option<String>>,
    retired: Mutex<bool>,
    retirement: Condvar,
    producer: Mutex<ProducerProgress>,
}

impl Generation {
    fn fail(&self, message: &str) {
        // Callers pass only categorical messages, never callback/provider text.
        if let Ok(mut failure) = self.failure.lock() {
            // The first terminal cause owns publication. In particular a late
            // queue/callback error cannot overwrite an intentional stop.
            if !self.closed.swap(true, Ordering::AcqRel) && failure.is_none() {
                *failure = Some(message.to_owned());
            }
        }
        self.closed.store(true, Ordering::Release);
    }
}

struct Worker {
    capture_id: u64,
    generation: Arc<Generation>,
    handle: Option<JoinHandle<()>>,
    joiner: Option<JoinHandle<()>>,
    thread_id: thread::ThreadId,
}

impl Worker {
    fn is_finished(&self) -> bool {
        self.handle
            .as_ref()
            .or(self.joiner.as_ref())
            .is_some_and(JoinHandle::is_finished)
    }

    fn join(self) {
        if let Some(handle) = self.handle {
            let _ = handle.join();
        }
        if let Some(joiner) = self.joiner {
            let _ = joiner.join();
        }
    }
}

#[derive(Default)]
struct Control {
    shutdown: bool,
    last_capture_id: u64,
    cancelled_through: u64,
    worker: Option<Worker>,
}

struct Inner {
    control: Mutex<Control>,
    epoch: AtomicU64,
    producer_timeouts: ProducerTimeouts,
    now: MonotonicNow,
}

/// One authoritative capture generation and one physically tracked worker.
pub struct DesktopVoiceRuntime {
    inner: Arc<Inner>,
}

#[derive(Clone)]
pub struct VoiceIngress {
    inner: Weak<Inner>,
    generation: Arc<Generation>,
    sender: SyncSender<QueuedFrame>,
}

impl VoiceIngress {
    /// Nonblocking queue admission; never waits for space or a frame callback.
    /// Failure publication always happens on the worker.
    pub fn try_push(&self, samples: Vec<f32>) -> Result<(), String> {
        let inner = self
            .inner
            .upgrade()
            .ok_or_else(|| String::from("Native voice runtime is unavailable"))?;
        // Each generation is closed permanently before replacement. No runtime
        // control lock is needed on capture admission; a concurrent close may
        // retire an already-admitted frame, but can never reopen this ingress.
        if self.generation.closed.load(Ordering::Acquire) {
            return Err(String::from("Native voice admission is closed"));
        }
        if samples.is_empty() {
            self.generation.fail("Native voice frame is empty");
            return Err(String::from("Native voice frame is empty"));
        }
        // Serialize only short producer bookkeeping and try_send with the stall
        // decision. No inference, callback, or runtime control lock is involved.
        let mut producer = self.generation.producer.lock().map_err(|_| {
            self.generation
                .fail("Native voice producer state is unavailable");
            String::from("Native voice producer state is unavailable")
        })?;
        if self.generation.closed.load(Ordering::Acquire) {
            return Err(String::from("Native voice admission is closed"));
        }
        let arrival = (inner.now)();
        if producer.stalled(arrival, inner.producer_timeouts) {
            self.generation.fail(PRODUCER_STALLED);
            return Err(String::from(PRODUCER_STALLED));
        }
        let frame = QueuedFrame {
            epoch: inner.epoch.load(Ordering::Acquire),
            samples,
        };
        match self.sender.try_send(frame) {
            Ok(()) => {
                producer.last_arrival = Some(arrival);
                Ok(())
            }
            Err(TrySendError::Full(_)) => {
                self.generation.fail(OVERFLOW);
                Err(String::from(OVERFLOW))
            }
            Err(TrySendError::Disconnected(_)) => {
                self.generation.fail("Native voice worker is unavailable");
                Err(String::from("Native voice worker is unavailable"))
            }
        }
    }
}

impl Default for DesktopVoiceRuntime {
    fn default() -> Self {
        Self::new()
    }
}

impl DesktopVoiceRuntime {
    pub fn new() -> Self {
        Self::with_producer_clock(
            ProducerTimeouts {
                first_frame: FIRST_FRAME_GRACE,
                arrival_gap: PRODUCER_ARRIVAL_GAP,
            },
            Arc::new(Instant::now),
        )
    }

    fn with_producer_clock(producer_timeouts: ProducerTimeouts, now: MonotonicNow) -> Self {
        Self {
            inner: Arc::new(Inner {
                control: Mutex::new(Control::default()),
                epoch: AtomicU64::new(0),
                producer_timeouts,
                now,
            }),
        }
    }

    pub fn epoch(&self) -> u64 {
        self.inner.epoch.load(Ordering::Acquire)
    }

    pub fn is_current(&self, capture_id: u64, epoch: u64) -> bool {
        self.inner.control.lock().is_ok_and(|control| {
            !control.shutdown
                && self.epoch() == epoch
                && control.worker.as_ref().is_some_and(|worker| {
                    worker.capture_id == capture_id
                        && !worker.generation.closed.load(Ordering::Acquire)
                })
        })
    }

    pub fn current_capture_id(&self) -> Option<u64> {
        self.inner.control.lock().ok().and_then(|control| {
            control
                .worker
                .as_ref()
                .filter(|worker| {
                    !control.shutdown && !worker.generation.closed.load(Ordering::Acquire)
                })
                .map(|worker| worker.capture_id)
        })
    }

    /// Includes a closed worker until its thread has physically finished.
    pub fn pending_workers(&self) -> usize {
        self.inner.control.lock().map_or(1, |control| {
            usize::from(control.worker.as_ref().is_some_and(|worker| {
                !worker.is_finished() && !worker.generation.retired.lock().is_ok_and(|done| *done)
            }))
        })
    }

    pub fn invalidate(&self) -> Result<u64, String> {
        let control = self
            .inner
            .control
            .lock()
            .map_err(|_| String::from("Native voice control is unavailable"))?;
        if control.shutdown {
            return Err(String::from("Native voice runtime is shut down"));
        }
        match self.advance_epoch() {
            Ok(epoch) => Ok(epoch),
            Err(error) => {
                if let Some(worker) = &control.worker {
                    worker.generation.fail("Native voice epoch exhausted");
                }
                Err(error)
            }
        }
    }

    fn advance_epoch(&self) -> Result<u64, String> {
        // Every writer holds control; readers may inspect the epoch lock-free.
        let next = self
            .epoch()
            .checked_add(1)
            .filter(|next| *next <= MAX_SAFE_INTEGER)
            .ok_or_else(|| String::from("Native voice epoch exhausted"))?;
        self.inner.epoch.store(next, Ordering::Release);
        Ok(next)
    }

    pub fn start(
        &self,
        capture_id: u64,
        on_frame: FrameCallback,
        on_failure: FailureCallback,
    ) -> Result<VoiceIngress, String> {
        if capture_id == 0 || capture_id > MAX_SAFE_INTEGER {
            return Err(String::from("Native voice capture ID is invalid"));
        }
        let mut control = self
            .inner
            .control
            .lock()
            .map_err(|_| String::from("Native voice control is unavailable"))?;
        if control.shutdown {
            return Err(String::from("Native voice runtime is shut down"));
        }
        if capture_id <= control.last_capture_id || capture_id <= control.cancelled_through {
            return Err(String::from("Native voice capture ID is stale"));
        }
        if control
            .worker
            .as_ref()
            .is_some_and(|worker| !worker.is_finished())
        {
            return Err(String::from(PENDING_RETIREMENT));
        }
        if let Some(worker) = control.worker.take() {
            // is_finished above ensures this never waits on a callback.
            worker.join();
        }
        self.advance_epoch()?;
        let generation = Arc::new(Generation {
            closed: AtomicBool::new(false),
            failure: Mutex::new(None),
            retired: Mutex::new(false),
            retirement: Condvar::new(),
            producer: Mutex::new(ProducerProgress {
                started_at: (self.inner.now)(),
                last_arrival: None,
            }),
        });
        let (sender, receiver) = mpsc::sync_channel(QUEUE_CAPACITY);
        // The worker retains its control/handle owner through physical callback
        // retirement, even if the public runtime is dropped after a timed stop.
        let inner = Arc::clone(&self.inner);
        let worker_generation = Arc::clone(&generation);
        let handle = thread::Builder::new()
            .name(String::from("desktop-voice"))
            .spawn(move || {
                run_worker(
                    inner,
                    capture_id,
                    worker_generation,
                    receiver,
                    on_frame,
                    on_failure,
                )
            })
            .map_err(|_| String::from("Native voice worker could not start"))?;
        control.worker = Some(Worker {
            capture_id,
            generation: Arc::clone(&generation),
            thread_id: handle.thread().id(),
            handle: Some(handle),
            joiner: None,
        });
        control.last_capture_id = capture_id;
        Ok(VoiceIngress {
            inner: Arc::downgrade(&self.inner),
            generation,
            sender,
        })
    }

    /// Closure-safe source failure. The worker owns eventual publication.
    /// The supplied text is deliberately discarded at this trust boundary.
    pub fn fail(&self, capture_id: u64, _message: String) -> Result<(), String> {
        let control = self
            .inner
            .control
            .lock()
            .map_err(|_| String::from("Native voice control is unavailable"))?;
        if let Some(worker) = &control.worker {
            if worker.capture_id == capture_id && !worker.generation.closed.load(Ordering::Acquire)
            {
                worker.generation.fail("Native microphone capture failed");
                self.advance_epoch()?;
            }
        }
        Ok(())
    }

    pub fn stop(&self, capture_id: Option<u64>) -> Result<(), String> {
        self.stop_inner(capture_id, false)
    }

    /// Close admission and fence commits without waiting for physical work.
    /// Returns true only when a matching open generation was newly closed.
    /// Reserved IDs are tombstoned even when no installed worker matches.
    pub fn request_stop(&self, capture_id: Option<u64>) -> Result<bool, String> {
        self.close_admission(capture_id, false)
            .map(|(_, closed)| closed)
    }

    pub fn shutdown(&self) -> Result<(), String> {
        self.stop_inner(None, true)
    }

    fn stop_inner(&self, capture_id: Option<u64>, shutdown: bool) -> Result<(), String> {
        let (generation, _) = self.close_admission(capture_id, shutdown)?;
        if let Some(generation) = generation {
            if !self.wait_for_generation_retirement(&generation, RETIREMENT_TIMEOUT) {
                return Err(String::from(PENDING_RETIREMENT));
            }
        }
        Ok(())
    }

    fn close_admission(
        &self,
        capture_id: Option<u64>,
        shutdown: bool,
    ) -> Result<(Option<Arc<Generation>>, bool), String> {
        if capture_id.is_some_and(|id| id > MAX_SAFE_INTEGER) {
            return Err(String::from("Native voice capture ID is invalid"));
        }
        let mut control = self
            .inner
            .control
            .lock()
            .map_err(|_| String::from("Native voice control is unavailable"))?;
        if shutdown {
            control.shutdown = true;
        }
        // A stop may arrive while a reserved capture is still starting.
        // Tombstone that ID even without a matching installed worker.
        control.cancelled_through = control
            .cancelled_through
            .max(capture_id.unwrap_or(control.last_capture_id));
        let Some(worker) = &control.worker else {
            return Ok((None, false));
        };
        if capture_id.is_some_and(|id| id != worker.capture_id) {
            return Ok((None, false));
        }
        let generation = Arc::clone(&worker.generation);
        let closed = !generation.closed.swap(true, Ordering::AcqRel);
        if closed {
            // Admission remains closed even if this final counter is exhausted.
            self.advance_epoch()?;
        }
        Ok((Some(generation), closed))
    }

    /// Wait after closing admission. A notification proves actual worker join,
    /// not merely the end of its callback. Timeout leaves ownership tracked.
    pub fn wait_for_retirement(&self, timeout: Duration) -> bool {
        let generation = match self.inner.control.lock() {
            Ok(control) => control
                .worker
                .as_ref()
                .map(|worker| Arc::clone(&worker.generation)),
            Err(_) => return false,
        };
        generation
            .is_none_or(|generation| self.wait_for_generation_retirement(&generation, timeout))
    }

    fn wait_for_generation_retirement(
        &self,
        generation: &Arc<Generation>,
        timeout: Duration,
    ) -> bool {
        {
            let Ok(mut control) = self.inner.control.lock() else {
                return false;
            };
            let Some(worker) = control.worker.as_mut() else {
                return true;
            };
            if !Arc::ptr_eq(&worker.generation, generation) {
                return true;
            }
            if worker.thread_id == thread::current().id() {
                return false;
            }
            if worker.is_finished() {
                control
                    .worker
                    .take()
                    .expect("finished worker checked")
                    .join();
                return true;
            }
            if worker.joiner.is_none() {
                // Preserve the original handle even if spawning the join owner
                // fails; no timeout path may silently detach physical work.
                let pending = Arc::new(Mutex::new(worker.handle.take()));
                let owned = Arc::clone(&pending);
                let retired = Arc::clone(generation);
                let owner = Arc::clone(&self.inner);
                let joiner = thread::Builder::new()
                    .name(String::from("voice-retirement"))
                    .spawn(move || {
                        let _owner = owner;
                        let handle = owned.lock().ok().and_then(|mut handle| handle.take());
                        if let Some(handle) = handle {
                            let _ = handle.join();
                            if let Ok(mut done) = retired.retired.lock() {
                                *done = true;
                                retired.retirement.notify_all();
                            }
                        }
                    });
                match joiner {
                    Ok(joiner) => worker.joiner = Some(joiner),
                    Err(_) => {
                        worker.handle = pending.lock().ok().and_then(|mut handle| handle.take());
                        return false;
                    }
                }
            }
        }
        let Ok(done) = generation.retired.lock() else {
            return false;
        };
        let Ok((done, _)) = generation
            .retirement
            .wait_timeout_while(done, timeout, |done| !*done)
        else {
            return false;
        };
        *done
    }
}

impl Drop for DesktopVoiceRuntime {
    fn drop(&mut self) {
        // Pending work retains Inner (and its tracked handle) until it exits.
        // No replacement can start through this permanently closed runtime.
        let _ = self.shutdown();
    }
}

fn run_worker(
    inner: Arc<Inner>,
    capture_id: u64,
    generation: Arc<Generation>,
    receiver: Receiver<QueuedFrame>,
    on_frame: FrameCallback,
    on_failure: FailureCallback,
) {
    let mut clock = FrameSampleClock::new();
    while !generation.closed.load(Ordering::Acquire) {
        if producer_stalled(&inner, &generation) {
            break;
        }
        let frame = match receiver.recv_timeout(RECEIVE_INTERVAL) {
            Ok(frame) => frame,
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        };
        let sample_end_ms = match clock.advance(frame.samples.len()) {
            Ok(now) => now,
            Err(_) => {
                generation.fail("Native voice sample clock exhausted");
                break;
            }
        };
        let current = inner.epoch.load(Ordering::Acquire) == frame.epoch
            && !generation.closed.load(Ordering::Acquire);
        if !current {
            continue;
        }
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            on_frame(VoiceFrame {
                capture_id,
                epoch: frame.epoch,
                sample_end_ms,
                samples: frame.samples,
            })
        }));
        if !matches!(result, Ok(Ok(()))) {
            // Serialize the failure fence with invalidate/stop. An obsolete
            // callback must not retire a usable newer epoch of this capture.
            let Ok(control) = inner.control.lock() else {
                generation.fail("Native voice control is unavailable");
                break;
            };
            if inner.epoch.load(Ordering::Acquire) == frame.epoch
                && !generation.closed.load(Ordering::Acquire)
                && control.worker.as_ref().is_some_and(|worker| {
                    worker.capture_id == capture_id && Arc::ptr_eq(&worker.generation, &generation)
                })
            {
                generation.fail("Native voice frame processing failed");
                break;
            }
        }
    }
    generation.closed.store(true, Ordering::Release);
    let failure = generation
        .failure
        .lock()
        .ok()
        .and_then(|mut failure| failure.take());
    if let Some(message) = failure {
        let authoritative = inner.control.lock().is_ok_and(|control| {
            control.worker.as_ref().is_some_and(|worker| {
                worker.capture_id == capture_id && Arc::ptr_eq(&worker.generation, &generation)
            })
        });
        if authoritative {
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                on_failure(capture_id, message)
            }));
        }
    }
}

fn producer_stalled(inner: &Inner, generation: &Generation) -> bool {
    match generation.producer.lock() {
        Ok(producer) => {
            if generation.closed.load(Ordering::Acquire) {
                return true;
            }
            if producer.stalled((inner.now)(), inner.producer_timeouts) {
                generation.fail(PRODUCER_STALLED);
                return true;
            }
            false
        }
        Err(_) => {
            generation.fail("Native voice producer state is unavailable");
            true
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    const TEST_TIMEOUT: Duration = Duration::from_secs(3);

    fn join_worker(runtime: &DesktopVoiceRuntime) {
        let worker = runtime.inner.control.lock().unwrap().worker.take();
        if let Some(worker) = worker {
            worker.generation.closed.store(true, Ordering::Release);
            worker.join();
        }
    }

    fn ignore_failure() -> FailureCallback {
        Arc::new(|_, _| {})
    }

    #[test]
    fn obsolete_callback_error_does_not_close_reset_epoch() {
        let runtime = DesktopVoiceRuntime::new();
        let (frames_tx, frames_rx) = mpsc::channel();
        let (failure_tx, failure_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let release_rx = Mutex::new(release_rx);
        let calls = AtomicUsize::new(0);
        let ingress = runtime
            .start(
                1,
                Arc::new(move |frame| {
                    let first = calls.fetch_add(1, Ordering::SeqCst) == 0;
                    frames_tx.send((frame.epoch, frame.sample_end_ms)).unwrap();
                    if first {
                        let _ = release_rx.lock().unwrap().recv_timeout(TEST_TIMEOUT);
                        Err("Synthetic obsolete inference failure".into())
                    } else {
                        Ok(())
                    }
                }),
                Arc::new(move |_, message| {
                    failure_tx.send(message).unwrap();
                }),
            )
            .unwrap();
        let old_epoch = runtime.epoch();
        ingress.try_push(vec![0.0; 480]).unwrap();
        assert_eq!(
            frames_rx.recv_timeout(TEST_TIMEOUT).unwrap(),
            (old_epoch, 30)
        );
        let new_epoch = runtime.invalidate().unwrap();
        ingress.try_push(vec![0.0; 480]).unwrap();
        release_tx.send(()).unwrap();
        assert_eq!(
            frames_rx.recv_timeout(TEST_TIMEOUT).unwrap(),
            (new_epoch, 60)
        );
        assert!(runtime.is_current(1, new_epoch));
        assert!(failure_rx.try_recv().is_err());
        join_worker(&runtime);
        assert!(failure_rx.try_recv().is_err());
    }

    #[test]
    fn request_stop_closes_admission_without_waiting_for_blocked_callback() {
        let runtime = DesktopVoiceRuntime::new();
        let (entered_tx, entered_rx) = mpsc::channel();
        let (failure_tx, failure_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let release_rx = Mutex::new(release_rx);
        let ingress = runtime
            .start(
                1,
                Arc::new(move |_| {
                    entered_tx.send(()).unwrap();
                    let _ = release_rx.lock().unwrap().recv_timeout(TEST_TIMEOUT);
                    Ok(())
                }),
                Arc::new(move |_, message| {
                    failure_tx.send(message).unwrap();
                }),
            )
            .unwrap();
        ingress.try_push(vec![0.0; 480]).unwrap();
        entered_rx.recv_timeout(TEST_TIMEOUT).unwrap();
        ingress.try_push(vec![0.0; 480]).unwrap();
        let epoch = runtime.epoch();
        assert!(runtime.request_stop(Some(1)).unwrap());
        assert_eq!(runtime.current_capture_id(), None);
        assert!(runtime.epoch() > epoch);
        assert_eq!(runtime.pending_workers(), 1);
        assert!(ingress.try_push(vec![0.0; 480]).is_err());
        assert!(!runtime.wait_for_retirement(Duration::ZERO));
        // An admission attempt already in flight may observe its queue error
        // after intentional closure. It must not turn Mic Off into failure.
        runtime
            .inner
            .control
            .lock()
            .unwrap()
            .worker
            .as_ref()
            .unwrap()
            .generation
            .fail(OVERFLOW);
        release_tx.send(()).unwrap();
        assert!(runtime.wait_for_retirement(TEST_TIMEOUT));
        assert!(entered_rx.try_recv().is_err());
        assert!(failure_rx.try_recv().is_err());
    }

    #[test]
    fn request_stop_tombstones_reserved_ids_without_closing_other_captures_or_shutdown() {
        let runtime = DesktopVoiceRuntime::new();
        assert!(!runtime.request_stop(Some(2)).unwrap());
        assert!(runtime
            .start(2, Arc::new(|_| Ok(())), ignore_failure())
            .is_err());
        let _ingress = runtime
            .start(3, Arc::new(|_| Ok(())), ignore_failure())
            .unwrap();
        let epoch = runtime.epoch();
        assert!(!runtime.request_stop(Some(1)).unwrap());
        assert!(!runtime.request_stop(Some(4)).unwrap());
        assert_eq!(runtime.epoch(), epoch);
        assert!(runtime.is_current(3, epoch));
        assert!(runtime.request_stop(None).unwrap());
        let stopped_epoch = runtime.epoch();
        assert!(!runtime.request_stop(Some(3)).unwrap());
        assert_eq!(runtime.epoch(), stopped_epoch);
        assert_eq!(runtime.current_capture_id(), None);
        assert!(runtime.wait_for_retirement(TEST_TIMEOUT));
        join_worker(&runtime);
        assert!(runtime
            .start(4, Arc::new(|_| Ok(())), ignore_failure())
            .is_err());
        let _replacement = runtime
            .start(5, Arc::new(|_| Ok(())), ignore_failure())
            .unwrap();
        assert!(!runtime.request_stop(Some(3)).unwrap());
        assert_eq!(runtime.current_capture_id(), Some(5));
        assert!(runtime.request_stop(None).unwrap());
        assert!(runtime.wait_for_retirement(TEST_TIMEOUT));
    }

    #[test]
    fn source_failure_still_retires_capture_after_obsolete_callback_error() {
        let runtime = DesktopVoiceRuntime::new();
        let (entered_tx, entered_rx) = mpsc::channel();
        let (failure_tx, failure_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let release_rx = Mutex::new(release_rx);
        let ingress = runtime
            .start(
                1,
                Arc::new(move |_| {
                    entered_tx.send(()).unwrap();
                    let _ = release_rx.lock().unwrap().recv_timeout(TEST_TIMEOUT);
                    Err("Synthetic obsolete callback error".into())
                }),
                Arc::new(move |_, message| {
                    failure_tx.send(message).unwrap();
                }),
            )
            .unwrap();
        ingress.try_push(vec![0.0; 480]).unwrap();
        entered_rx.recv_timeout(TEST_TIMEOUT).unwrap();
        runtime.invalidate().unwrap();
        runtime.fail(1, "Synthetic source failure".into()).unwrap();
        release_tx.send(()).unwrap();
        assert_eq!(
            failure_rx.recv_timeout(TEST_TIMEOUT).unwrap(),
            "Native microphone capture failed"
        );
        assert!(ingress.try_push(vec![0.0; 480]).is_err());
        assert!(runtime.wait_for_retirement(TEST_TIMEOUT));
        assert!(failure_rx.try_recv().is_err());
    }

    #[test]
    fn retirement_wait_tracks_blocked_callback_then_actual_join() {
        let runtime = DesktopVoiceRuntime::new();
        assert!(runtime.wait_for_retirement(Duration::ZERO));
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let release_rx = Mutex::new(release_rx);
        let ingress = runtime
            .start(
                1,
                Arc::new(move |_| {
                    entered_tx.send(()).unwrap();
                    let _ = release_rx.lock().unwrap().recv_timeout(TEST_TIMEOUT);
                    Ok(())
                }),
                ignore_failure(),
            )
            .unwrap();
        ingress.try_push(vec![0.0; 480]).unwrap();
        entered_rx.recv_timeout(TEST_TIMEOUT).unwrap();
        assert_eq!(runtime.stop(Some(1)).unwrap_err(), PENDING_RETIREMENT);
        assert!(!runtime.wait_for_retirement(Duration::ZERO));
        assert_eq!(runtime.pending_workers(), 1);
        release_tx.send(()).unwrap();
        assert!(runtime.wait_for_retirement(TEST_TIMEOUT));
        assert_eq!(runtime.pending_workers(), 0);
        assert!(runtime
            .start(1, Arc::new(|_| Ok(())), ignore_failure())
            .is_err());
    }

    fn fake_producer_clock() -> (DesktopVoiceRuntime, Arc<AtomicU64>) {
        let elapsed_ms = Arc::new(AtomicU64::new(0));
        let clock = Arc::clone(&elapsed_ms);
        let origin = Instant::now();
        (
            DesktopVoiceRuntime::with_producer_clock(
                ProducerTimeouts {
                    first_frame: FIRST_FRAME_GRACE,
                    arrival_gap: PRODUCER_ARRIVAL_GAP,
                },
                Arc::new(move || origin + Duration::from_millis(clock.load(Ordering::Acquire))),
            ),
            elapsed_ms,
        )
    }

    #[test]
    fn producer_without_first_frame_fails_once_and_rejects_resumption() {
        let (runtime, elapsed_ms) = fake_producer_clock();
        let (failure_tx, failure_rx) = mpsc::channel();
        let ingress = runtime
            .start(
                1,
                Arc::new(|_| Ok(())),
                Arc::new(move |_, message| {
                    failure_tx.send(message).unwrap();
                }),
            )
            .unwrap();
        elapsed_ms.store(5_000, Ordering::Release);
        assert_eq!(
            failure_rx.recv_timeout(TEST_TIMEOUT).unwrap(),
            PRODUCER_STALLED
        );
        assert!(ingress.try_push(vec![0.0; 480]).is_err());
        join_worker(&runtime);
        assert!(failure_rx.try_recv().is_err());
    }

    #[test]
    fn stopping_reserved_future_capture_tombstones_its_start_without_stopping_current() {
        let runtime = DesktopVoiceRuntime::new();
        runtime.stop(Some(2)).unwrap();
        assert!(runtime
            .start(1, Arc::new(|_| Ok(())), ignore_failure())
            .is_err());
        assert!(runtime
            .start(2, Arc::new(|_| Ok(())), ignore_failure())
            .is_err());
        let ingress = runtime
            .start(3, Arc::new(|_| Ok(())), ignore_failure())
            .unwrap();
        let epoch = runtime.epoch();
        runtime.stop(Some(4)).unwrap();
        runtime.stop(Some(1)).unwrap();
        assert!(runtime.is_current(3, epoch));
        ingress.try_push(vec![0.0; 480]).unwrap();
        join_worker(&runtime);
        assert!(runtime
            .start(4, Arc::new(|_| Ok(())), ignore_failure())
            .is_err());
        let _replacement = runtime
            .start(5, Arc::new(|_| Ok(())), ignore_failure())
            .unwrap();
        join_worker(&runtime);
    }

    #[test]
    fn producer_deadlines_use_arrival_instants_not_pcm_content_or_sample_time() {
        let origin = Instant::now();
        let timeouts = ProducerTimeouts {
            first_frame: FIRST_FRAME_GRACE,
            arrival_gap: PRODUCER_ARRIVAL_GAP,
        };
        let mut progress = ProducerProgress {
            started_at: origin,
            last_arrival: None,
        };
        assert!(!progress.stalled(origin + Duration::from_millis(4_999), timeouts));
        assert!(progress.stalled(origin + FIRST_FRAME_GRACE, timeouts));
        progress.last_arrival = Some(origin + Duration::from_millis(4_999));
        assert!(!progress.stalled(origin + Duration::from_millis(6_998), timeouts));
        assert!(progress.stalled(origin + Duration::from_millis(6_999), timeouts));
    }

    #[test]
    fn zero_pcm_refreshes_producer_progress_then_missing_delivery_fails() {
        let (runtime, elapsed_ms) = fake_producer_clock();
        let (frames_tx, frames_rx) = mpsc::channel();
        let (failure_tx, failure_rx) = mpsc::channel();
        let ingress = runtime
            .start(
                1,
                Arc::new(move |frame| {
                    assert!(frame.samples.iter().all(|sample| *sample == 0.0));
                    frames_tx.send(frame.sample_end_ms).unwrap();
                    Ok(())
                }),
                Arc::new(move |_, message| {
                    failure_tx.send(message).unwrap();
                }),
            )
            .unwrap();
        for (arrival_ms, sample_end_ms) in [(1_000, 30), (2_500, 60), (4_400, 90)] {
            elapsed_ms.store(arrival_ms, Ordering::Release);
            ingress.try_push(vec![0.0; 480]).unwrap();
            assert_eq!(frames_rx.recv_timeout(TEST_TIMEOUT).unwrap(), sample_end_ms);
            assert!(failure_rx.try_recv().is_err());
        }
        elapsed_ms.store(6_400, Ordering::Release);
        assert_eq!(
            failure_rx.recv_timeout(TEST_TIMEOUT).unwrap(),
            PRODUCER_STALLED
        );
        assert!(ingress.try_push(vec![0.0; 480]).is_err());
        join_worker(&runtime);
        assert!(failure_rx.try_recv().is_err());
    }

    #[test]
    fn producer_progress_while_callback_is_blocked_is_not_a_stall() {
        let (runtime, elapsed_ms) = fake_producer_clock();
        let (frames_tx, frames_rx) = mpsc::channel();
        let (failure_tx, failure_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let release_rx = Mutex::new(release_rx);
        let calls = AtomicUsize::new(0);
        let ingress = runtime
            .start(
                1,
                Arc::new(move |frame| {
                    let first = calls.fetch_add(1, Ordering::SeqCst) == 0;
                    frames_tx.send(frame.sample_end_ms).unwrap();
                    if first {
                        let _ = release_rx.lock().unwrap().recv_timeout(TEST_TIMEOUT);
                    }
                    Ok(())
                }),
                Arc::new(move |_, message| {
                    failure_tx.send(message).unwrap();
                }),
            )
            .unwrap();
        ingress.try_push(vec![0.0; 480]).unwrap();
        assert_eq!(frames_rx.recv_timeout(TEST_TIMEOUT).unwrap(), 30);
        for arrival in [1_500, 3_000, 4_500] {
            elapsed_ms.store(arrival, Ordering::Release);
            ingress.try_push(vec![0.0; 480]).unwrap();
        }
        let generation = Arc::clone(
            &runtime
                .inner
                .control
                .lock()
                .unwrap()
                .worker
                .as_ref()
                .unwrap()
                .generation,
        );
        assert!(!producer_stalled(&runtime.inner, &generation));
        release_tx.send(()).unwrap();
        for end_ms in [60, 90, 120] {
            assert_eq!(frames_rx.recv_timeout(TEST_TIMEOUT).unwrap(), end_ms);
        }
        assert!(failure_rx.try_recv().is_err());
        join_worker(&runtime);
    }

    #[test]
    fn late_resumption_during_blocked_callback_fails_instead_of_splicing_audio() {
        let (runtime, elapsed_ms) = fake_producer_clock();
        let (entered_tx, entered_rx) = mpsc::channel();
        let (failure_tx, failure_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let release_rx = Mutex::new(release_rx);
        let ingress = runtime
            .start(
                1,
                Arc::new(move |_| {
                    entered_tx.send(()).unwrap();
                    let _ = release_rx.lock().unwrap().recv_timeout(TEST_TIMEOUT);
                    Ok(())
                }),
                Arc::new(move |_, message| {
                    failure_tx.send(message).unwrap();
                }),
            )
            .unwrap();
        ingress.try_push(vec![0.0; 480]).unwrap();
        entered_rx.recv_timeout(TEST_TIMEOUT).unwrap();
        elapsed_ms.store(2_000, Ordering::Release);
        assert_eq!(
            ingress.try_push(vec![0.0; 480]).unwrap_err(),
            PRODUCER_STALLED
        );
        assert!(failure_rx.try_recv().is_err());
        release_tx.send(()).unwrap();
        assert_eq!(
            failure_rx.recv_timeout(TEST_TIMEOUT).unwrap(),
            PRODUCER_STALLED
        );
        join_worker(&runtime);
        assert!(entered_rx.try_recv().is_err());
        assert!(failure_rx.try_recv().is_err());
    }

    #[test]
    fn clock_accumulates_samples_before_rounding() {
        let mut clock = FrameSampleClock::new();
        for _ in 0..15 {
            assert_eq!(clock.advance(1).unwrap(), 0);
        }
        assert_eq!(clock.advance(1).unwrap(), 1);
        assert_eq!(clock.advance(480).unwrap(), 31);
    }

    #[test]
    fn clock_rejects_empty_and_exhaustion_without_mutation() {
        let mut clock = FrameSampleClock::new();
        assert!(clock.advance(0).is_err());
        assert_eq!(clock.now_ms(), 0);
        clock.samples_seen = MAX_SAFE_INTEGER - 1;
        assert_eq!(
            clock.advance(1).unwrap(),
            ((u128::from(MAX_SAFE_INTEGER) * 1_000) / SAMPLE_RATE_HZ) as u64
        );
        let before = clock.now_ms();
        assert!(clock.advance(1).is_err());
        assert_eq!(clock.samples_seen, MAX_SAFE_INTEGER);
        assert_eq!(clock.now_ms(), before);
    }

    #[test]
    fn idle_source_failure_is_published_without_an_audio_frame() {
        let runtime = DesktopVoiceRuntime::new();
        let (failure_tx, failure_rx) = mpsc::channel();
        let ingress = runtime
            .start(
                1,
                Arc::new(|_| panic!("no frame was admitted")),
                Arc::new(move |id, message| {
                    failure_tx.send((id, message)).unwrap();
                }),
            )
            .unwrap();
        let epoch = runtime.epoch();
        runtime
            .fail(1, String::from("synthetic source detail"))
            .unwrap();
        assert!(runtime.epoch() > epoch);
        assert!(!runtime.is_current(1, epoch));
        assert!(ingress.try_push(vec![0.0; 480]).is_err());
        assert_eq!(
            failure_rx.recv_timeout(TEST_TIMEOUT).unwrap(),
            (1, String::from("Native microphone capture failed"))
        );
        join_worker(&runtime);
        assert!(failure_rx.try_recv().is_err());
    }

    #[test]
    fn admission_does_not_wait_for_the_control_mutex() {
        let runtime = DesktopVoiceRuntime::new();
        let (frame_tx, frame_rx) = mpsc::channel();
        let ingress = runtime
            .start(
                1,
                Arc::new(move |frame| {
                    frame_tx.send(frame.samples.len()).unwrap();
                    Ok(())
                }),
                ignore_failure(),
            )
            .unwrap();
        let control = runtime.inner.control.lock().unwrap();
        ingress.try_push(vec![0.0; 480]).unwrap();
        assert_eq!(frame_rx.recv_timeout(TEST_TIMEOUT).unwrap(), 480);
        drop(control);
        join_worker(&runtime);
    }

    #[test]
    fn empty_admission_and_epoch_exhaustion_fail_closed() {
        for exhaust_epoch in [false, true] {
            let runtime = DesktopVoiceRuntime::new();
            let (failure_tx, failure_rx) = mpsc::channel();
            let ingress = runtime
                .start(
                    1,
                    Arc::new(|_| Ok(())),
                    Arc::new(move |_, message| {
                        failure_tx.send(message).unwrap();
                    }),
                )
                .unwrap();
            if exhaust_epoch {
                runtime
                    .inner
                    .epoch
                    .store(MAX_SAFE_INTEGER, Ordering::Release);
                assert!(runtime.invalidate().is_err());
                assert_eq!(runtime.epoch(), MAX_SAFE_INTEGER);
            } else {
                assert!(ingress.try_push(Vec::new()).is_err());
            }
            assert!(ingress.try_push(vec![0.0; 480]).is_err());
            assert_eq!(runtime.current_capture_id(), None);
            assert_eq!(
                failure_rx.recv_timeout(TEST_TIMEOUT).unwrap(),
                if exhaust_epoch {
                    "Native voice epoch exhausted"
                } else {
                    "Native voice frame is empty"
                }
            );
            join_worker(&runtime);
            assert!(failure_rx.try_recv().is_err());
        }
    }

    #[test]
    fn final_drop_keeps_the_worker_handle_owned_until_callback_retirement() {
        let runtime = DesktopVoiceRuntime::new();
        let inner = Arc::downgrade(&runtime.inner);
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let release_rx = Mutex::new(release_rx);
        let ingress = runtime
            .start(
                1,
                Arc::new(move |_| {
                    entered_tx.send(()).unwrap();
                    let _ = release_rx.lock().unwrap().recv();
                    Ok(())
                }),
                ignore_failure(),
            )
            .unwrap();
        ingress.try_push(vec![0.0; 480]).unwrap();
        entered_rx.recv_timeout(TEST_TIMEOUT).unwrap();
        drop(runtime);
        assert!(ingress.try_push(vec![0.0; 480]).is_err());
        let retained = inner.upgrade().expect("worker retains the control owner");
        assert!(retained.control.lock().unwrap().shutdown);
        assert!(retained.control.lock().unwrap().worker.is_some());
        release_tx.send(()).unwrap();
        let worker = retained.control.lock().unwrap().worker.take().unwrap();
        worker.join();
        drop(retained);
        assert!(inner.upgrade().is_none());
    }

    #[test]
    fn stale_queued_epochs_still_advance_the_capture_sample_clock() {
        let runtime = Arc::new(DesktopVoiceRuntime::new());
        let (frames_tx, frames_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let release_rx = Mutex::new(release_rx);
        let calls = AtomicUsize::new(0);
        let weak = Arc::downgrade(&runtime);
        let ingress = runtime
            .start(
                1,
                Arc::new(move |frame| {
                    let first = calls.fetch_add(1, Ordering::SeqCst) == 0;
                    frames_tx
                        .send((frame.capture_id, frame.epoch, frame.sample_end_ms))
                        .unwrap();
                    if first {
                        let _ = release_rx.lock().unwrap().recv();
                    } else {
                        // Callback reentry must not deadlock behind the control lock.
                        weak.upgrade().unwrap().invalidate()?;
                    }
                    Ok(())
                }),
                ignore_failure(),
            )
            .unwrap();
        let old_epoch = runtime.epoch();
        ingress.try_push(vec![0.0; 480]).unwrap();
        assert_eq!(
            frames_rx.recv_timeout(TEST_TIMEOUT).unwrap(),
            (1, old_epoch, 30)
        );
        ingress.try_push(vec![0.0; 480]).unwrap();
        let new_epoch = runtime.invalidate().unwrap();
        assert!(!runtime.is_current(1, old_epoch));
        assert!(runtime.is_current(1, new_epoch));
        ingress.try_push(vec![0.0; 480]).unwrap();
        release_tx.send(()).unwrap();
        assert_eq!(
            frames_rx.recv_timeout(TEST_TIMEOUT).unwrap(),
            (1, new_epoch, 90)
        );
        join_worker(&runtime);
        assert!(frames_rx.try_recv().is_err());
    }

    #[test]
    fn overflow_closes_admission_and_publishes_once_on_worker() {
        let runtime = DesktopVoiceRuntime::new();
        let caller = thread::current().id();
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let release_rx = Mutex::new(release_rx);
        let (failure_tx, failure_rx) = mpsc::channel();
        let ingress = runtime
            .start(
                1,
                Arc::new(move |_| {
                    entered_tx.send(thread::current().id()).unwrap();
                    let _ = release_rx.lock().unwrap().recv();
                    Ok(())
                }),
                Arc::new(move |id, message| {
                    failure_tx
                        .send((id, message, thread::current().id()))
                        .unwrap();
                }),
            )
            .unwrap();
        ingress.try_push(vec![0.0; 480]).unwrap();
        let worker_thread = entered_rx.recv_timeout(TEST_TIMEOUT).unwrap();
        for _ in 0..QUEUE_CAPACITY {
            ingress.clone().try_push(vec![0.0; 480]).unwrap();
        }
        assert_eq!(ingress.try_push(vec![0.0; 480]).unwrap_err(), OVERFLOW);
        assert!(ingress.try_push(vec![0.0; 480]).is_err());
        assert_eq!(runtime.current_capture_id(), None);
        assert!(failure_rx.try_recv().is_err());
        release_tx.send(()).unwrap();
        let (id, message, published_thread) = failure_rx.recv_timeout(TEST_TIMEOUT).unwrap();
        assert_eq!(id, 1);
        assert_eq!(message, OVERFLOW);
        assert_eq!(published_thread, worker_thread);
        assert_ne!(published_thread, caller);
        join_worker(&runtime);
        assert!(failure_rx.try_recv().is_err());
        assert!(entered_rx.try_recv().is_err());
    }

    #[test]
    fn stop_tracks_pending_retirement_and_stale_controls_cannot_stop_replacement() {
        let runtime = DesktopVoiceRuntime::new();
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let release_rx = Mutex::new(release_rx);
        let ingress = runtime
            .start(
                1,
                Arc::new(move |_| {
                    entered_tx.send(()).unwrap();
                    let _ = release_rx.lock().unwrap().recv();
                    Ok(())
                }),
                ignore_failure(),
            )
            .unwrap();
        ingress.try_push(vec![0.0; 480]).unwrap();
        entered_rx.recv_timeout(TEST_TIMEOUT).unwrap();
        let epoch = runtime.epoch();
        assert_eq!(runtime.stop(Some(1)).unwrap_err(), PENDING_RETIREMENT);
        assert!(runtime.epoch() > epoch);
        assert!(!runtime.is_current(1, epoch));
        assert_eq!(runtime.pending_workers(), 1);
        assert!(ingress.try_push(vec![0.0; 480]).is_err());
        assert!(runtime
            .start(2, Arc::new(|_| Ok(())), ignore_failure())
            .is_err());
        release_tx.send(()).unwrap();
        join_worker(&runtime);
        let (frame_tx, frame_rx) = mpsc::channel();
        let (failure_tx, failure_rx) = mpsc::channel();
        let replacement = runtime
            .start(
                2,
                Arc::new(move |frame| {
                    frame_tx.send(frame.sample_end_ms).unwrap();
                    Ok(())
                }),
                Arc::new(move |_, message| {
                    failure_tx.send(message).unwrap();
                }),
            )
            .unwrap();
        let replacement_epoch = runtime.epoch();
        runtime.stop(Some(1)).unwrap();
        runtime
            .fail(1, String::from("synthetic untrusted detail"))
            .unwrap();
        assert_eq!(runtime.epoch(), replacement_epoch);
        assert!(runtime.is_current(2, replacement_epoch));
        replacement.try_push(vec![0.0; 480]).unwrap();
        assert_eq!(frame_rx.recv_timeout(TEST_TIMEOUT).unwrap(), 30);
        join_worker(&runtime);
        assert!(failure_rx.try_recv().is_err());
        assert!(runtime
            .start(2, Arc::new(|_| Ok(())), ignore_failure())
            .is_err());
        runtime.shutdown().unwrap();
        assert!(runtime
            .start(3, Arc::new(|_| Ok(())), ignore_failure())
            .is_err());
    }

    #[test]
    fn source_and_processing_failures_are_categorical_and_self_stop_never_joins() {
        for source_failure in [false, true] {
            let runtime = Arc::new(DesktopVoiceRuntime::new());
            let (failure_tx, failure_rx) = mpsc::channel();
            let weak = Arc::downgrade(&runtime);
            let failure_runtime = Arc::downgrade(&runtime);
            let ingress = runtime
                .start(
                    1,
                    Arc::new(move |_| {
                        if source_failure {
                            weak.upgrade()
                                .unwrap()
                                .fail(1, String::from("synthetic private source detail"))?;
                            Ok(())
                        } else {
                            Err(String::from("synthetic private inference detail"))
                        }
                    }),
                    Arc::new(move |_, message| {
                        assert_eq!(
                            failure_runtime
                                .upgrade()
                                .unwrap()
                                .stop(Some(1))
                                .unwrap_err(),
                            PENDING_RETIREMENT
                        );
                        failure_tx.send(message).unwrap();
                    }),
                )
                .unwrap();
            ingress.try_push(vec![0.0; 480]).unwrap();
            assert_eq!(
                failure_rx.recv_timeout(TEST_TIMEOUT).unwrap(),
                if source_failure {
                    "Native microphone capture failed"
                } else {
                    "Native voice frame processing failed"
                }
            );
            join_worker(&runtime);
        }
    }
}
