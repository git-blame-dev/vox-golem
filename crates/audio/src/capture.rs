use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Condvar, Mutex, Weak};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use rodio::cpal;
use rodio::cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use rodio::cpal::Sample as _;

#[cfg(feature = "benchmark-input")]
mod benchmark;
#[cfg(feature = "benchmark-input")]
pub use benchmark::BenchmarkAudioBoundary;

const MAX_CAPTURE_ID: u64 = 9_007_199_254_740_991;
const SAMPLE_QUEUE_CAPACITY: usize = 32;
const WORKER_POLL_INTERVAL: Duration = Duration::from_millis(10);
const STOP_WAIT: Duration = Duration::from_millis(250);

pub type CaptureCallback = Box<dyn FnMut(Vec<f32>) + Send + 'static>;
pub type CaptureTerminalCallback = Box<dyn FnOnce(CaptureTerminal) + Send + 'static>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputDevice {
    pub id: String,
    pub label: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CaptureStart {
    pub fell_back_to_default: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureTerminal {
    pub capture_id: u64,
    pub error: CaptureError,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CaptureError {
    InvalidCaptureId,
    CaptureIdExhausted,
    InvalidSampleRate,
    InvalidFrameSize,
    InvalidSample,
    ChannelOverflow,
    Superseded,
    ShuttingDown,
    InputUnavailable(String),
    InputFailed(String),
    StatePoisoned,
    StopTimedOut,
}

impl fmt::Display for CaptureError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidCaptureId => write!(formatter, "audio capture id is invalid"),
            Self::CaptureIdExhausted => write!(formatter, "audio capture ids are exhausted"),
            Self::InvalidSampleRate => {
                write!(
                    formatter,
                    "audio capture sample rate must be greater than zero"
                )
            }
            Self::InvalidFrameSize => {
                write!(
                    formatter,
                    "audio capture frame size must be greater than zero"
                )
            }
            Self::InvalidSample => write!(formatter, "audio capture samples must be finite"),
            Self::ChannelOverflow => write!(formatter, "audio capture input fell behind"),
            Self::Superseded => write!(formatter, "audio capture was cancelled or superseded"),
            Self::ShuttingDown => write!(formatter, "audio capture service is shutting down"),
            Self::InputUnavailable(details) => {
                write!(formatter, "audio input is unavailable: {details}")
            }
            Self::InputFailed(details) => write!(formatter, "audio input failed: {details}"),
            Self::StatePoisoned => write!(formatter, "audio capture state lock is poisoned"),
            Self::StopTimedOut => write!(formatter, "audio capture worker did not stop in time"),
        }
    }
}

impl std::error::Error for CaptureError {}

pub struct AudioCaptureService {
    shared: Arc<Shared>,
    #[cfg(feature = "benchmark-input")]
    benchmark_input: Option<(u32, usize)>,
}

struct Shared {
    operation_lock: Mutex<()>,
    state: Mutex<State>,
    retired: Condvar,
    shutting_down: AtomicBool,
}

#[derive(Default)]
struct State {
    next_id: u64,
    latest_id: u64,
    cancelled_through: u64,
    current_id: Option<u64>,
    active: Option<ActiveCapture>,
    teardown_count: usize,
    start_in_progress: bool,
}

struct StartLease<'a>(&'a Shared);

impl<'a> StartLease<'a> {
    fn begin(shared: &'a Shared) -> Result<Self, CaptureError> {
        let mut state = shared
            .state
            .lock()
            .map_err(|_| CaptureError::StatePoisoned)?;
        if shared.shutting_down.load(Ordering::Acquire) {
            return Err(CaptureError::ShuttingDown);
        }
        state.start_in_progress = true;
        Ok(Self(shared))
    }
}

impl Drop for StartLease<'_> {
    fn drop(&mut self) {
        if let Ok(mut state) = self.0.state.lock() {
            state.start_in_progress = false;
        }
        self.0.retired.notify_all();
    }
}

fn fully_retired(state: &State) -> bool {
    state.current_id.is_none()
        && state.active.is_none()
        && state.teardown_count == 0
        && !state.start_in_progress
}

struct ActiveCapture {
    stream: SourceHandle,
    control: mpsc::Sender<WorkerControl>,
    worker: JoinHandle<()>,
}

enum SourceHandle {
    Native(cpal::Stream),
    #[cfg(feature = "benchmark-input")]
    Benchmark(benchmark::Producer),
    #[cfg(test)]
    Test(Box<dyn Send>),
}

impl SourceHandle {
    fn play(&self) -> Result<(), CaptureError> {
        match self {
            Self::Native(stream) => stream
                .play()
                .map_err(|error| CaptureError::InputFailed(error.to_string())),
            #[cfg(feature = "benchmark-input")]
            Self::Benchmark(_) => Ok(()),
            #[cfg(test)]
            Self::Test(_source) => Ok(()),
        }
    }

    fn activate(&self) {
        #[cfg(feature = "benchmark-input")]
        if let Self::Benchmark(producer) = self {
            producer.activate();
        }
    }
}

enum WorkerControl {
    Activate,
    Stop,
    Fail(CaptureError),
}

impl AudioCaptureService {
    pub fn new() -> Self {
        Self {
            #[cfg(feature = "benchmark-input")]
            benchmark_input: None,
            shared: Arc::new(Shared {
                operation_lock: Mutex::new(()),
                state: Mutex::new(State::default()),
                retired: Condvar::new(),
                shutting_down: AtomicBool::new(false),
            }),
        }
    }

    pub fn list_input_devices(&self) -> Result<Vec<InputDevice>, CaptureError> {
        #[cfg(feature = "benchmark-input")]
        if self.benchmark_input.is_some() {
            return Ok(Vec::new());
        }
        enumerate_input_devices()
    }

    pub fn is_capturing(&self) -> Result<bool, CaptureError> {
        self.shared
            .state
            .lock()
            .map(|state| state.current_id.is_some())
            .map_err(|_| CaptureError::StatePoisoned)
    }

    pub fn reserve_id(&self) -> Result<u64, CaptureError> {
        if self.shared.shutting_down.load(Ordering::Acquire) {
            return Err(CaptureError::ShuttingDown);
        }
        let mut state = self
            .shared
            .state
            .lock()
            .map_err(|_| CaptureError::StatePoisoned)?;
        if self.shared.shutting_down.load(Ordering::Acquire) {
            return Err(CaptureError::ShuttingDown);
        }
        let next_id = state
            .next_id
            .max(state.latest_id)
            .max(state.cancelled_through)
            .checked_add(1)
            .filter(|next_id| *next_id <= MAX_CAPTURE_ID)
            .ok_or(CaptureError::CaptureIdExhausted)?;
        state.next_id = next_id;
        Ok(next_id)
    }

    pub fn start(
        &self,
        capture_id: u64,
        selected_device_id: Option<String>,
        target_rate_hz: u32,
        frame_size: usize,
        callback: CaptureCallback,
        terminal_callback: CaptureTerminalCallback,
    ) -> Result<CaptureStart, CaptureError> {
        validate(capture_id, target_rate_hz, frame_size)?;
        let _operation = self
            .shared
            .operation_lock
            .lock()
            .map_err(|_| CaptureError::StatePoisoned)?;
        if self.shared.shutting_down.load(Ordering::Acquire) {
            return Err(CaptureError::ShuttingDown);
        }
        let _starting = StartLease::begin(&self.shared)?;
        let previous = {
            let mut state = self
                .shared
                .state
                .lock()
                .map_err(|_| CaptureError::StatePoisoned)?;
            ensure_teardown_complete(&state)?;
            if capture_id <= state.latest_id {
                return Err(CaptureError::Superseded);
            }
            state.latest_id = capture_id;
            state.current_id = None;
            take_active_for_teardown(&mut state)
        };
        if let Some(previous) = previous {
            stop_active(previous, &self.shared)?;
        }

        #[cfg(feature = "benchmark-input")]
        let prepared = if let Some((rate, channels)) = self.benchmark_input {
            benchmark::prepare(
                Arc::downgrade(&self.shared),
                capture_id,
                rate,
                channels,
                target_rate_hz,
                frame_size,
                callback,
                terminal_callback,
            )
        } else {
            prepare_capture(
                Arc::downgrade(&self.shared),
                capture_id,
                selected_device_id.as_deref(),
                target_rate_hz,
                frame_size,
                callback,
                terminal_callback,
            )
        };
        #[cfg(not(feature = "benchmark-input"))]
        let prepared = prepare_capture(
            Arc::downgrade(&self.shared),
            capture_id,
            selected_device_id.as_deref(),
            target_rate_hz,
            frame_size,
            callback,
            terminal_callback,
        );
        let (active, fell_back_to_default) = prepared?;
        if let Err(error) = active.stream.play() {
            let _ = stop_uninstalled_active(active, &self.shared);
            return Err(error);
        }
        {
            let mut state = self
                .shared
                .state
                .lock()
                .map_err(|_| CaptureError::StatePoisoned)?;
            if let Err(error) = ensure_install_authorized(&self.shared, &state, capture_id) {
                begin_teardown(&mut state);
                drop(state);
                stop_active(active, &self.shared)?;
                return Err(error);
            }
            state.current_id = Some(capture_id);
            state.active = Some(active);
            if state
                .active
                .as_ref()
                .expect("active capture was just started")
                .control
                .send(WorkerControl::Activate)
                .is_err()
            {
                state.current_id = None;
                let active = take_active_for_teardown(&mut state);
                drop(state);
                if let Some(active) = active {
                    let _ = stop_active(active, &self.shared);
                }
                return Err(CaptureError::InputFailed(
                    "audio capture worker stopped during startup".to_string(),
                ));
            }
            state
                .active
                .as_ref()
                .expect("active capture was just started")
                .stream
                .activate();
        }

        Ok(CaptureStart {
            fell_back_to_default,
        })
    }

    pub fn stop(&self, capture_id: u64) -> Result<bool, CaptureError> {
        validate_id(capture_id)?;
        let active = {
            let mut state = self
                .shared
                .state
                .lock()
                .map_err(|_| CaptureError::StatePoisoned)?;
            state.latest_id = state.latest_id.max(capture_id);
            state.cancelled_through = state.cancelled_through.max(capture_id);
            if state.current_id == Some(capture_id) {
                state.current_id = None;
                let active = take_active_for_teardown(&mut state);
                self.shared.retired.notify_all();
                active
            } else {
                None
            }
        };
        if let Some(active) = active {
            stop_active(active, &self.shared)?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    pub fn shutdown(&self) {
        if self.shared.shutting_down.swap(true, Ordering::AcqRel) {
            return;
        }
        let active = self.shared.state.lock().ok().and_then(|mut state| {
            state.current_id = None;
            let active = take_active_for_teardown(&mut state);
            self.shared.retired.notify_all();
            active
        });
        if let Some(active) = active {
            let _ = stop_active(active, &self.shared);
        }
    }

    /// Stop the physical owner's current source, independently of downstream
    /// processing state. All IDs reserved before this operation are tombstoned;
    /// a later reservation can restart after actual retirement. The caller must
    /// close application-level start admission first when installing an update.
    pub fn stop_current(&self) -> Result<bool, CaptureError> {
        let _operation = self
            .shared
            .operation_lock
            .lock()
            .map_err(|_| CaptureError::StatePoisoned)?;
        let (active, had_current) = {
            let mut state = self
                .shared
                .state
                .lock()
                .map_err(|_| CaptureError::StatePoisoned)?;
            let through = state
                .next_id
                .max(state.latest_id)
                .max(state.current_id.unwrap_or_default());
            state.latest_id = through;
            state.cancelled_through = state.cancelled_through.max(through);
            let had_current = state.current_id.take().is_some();
            let active = take_active_for_teardown(&mut state);
            self.shared.retired.notify_all();
            (active, had_current)
        };
        let had_active = active.is_some();
        if let Some(active) = active {
            stop_active(active, &self.shared)?;
        }
        let state = self
            .shared
            .state
            .lock()
            .map_err(|_| CaptureError::StatePoisoned)?;
        if !fully_retired(&state) {
            return Err(CaptureError::StopTimedOut);
        }
        Ok(had_current || had_active)
    }

    /// Wait for physical stream drop and capture-worker exit, not merely a
    /// cleared current ID. Admission must already be closed by the caller.
    /// A start preparing an uninstalled backend is also still physical activity.
    pub fn wait_for_retirement(&self, timeout: Duration) -> bool {
        if self.shared.operation_lock.is_poisoned() {
            return false;
        }
        let Ok(state) = self.shared.state.lock() else {
            return false;
        };
        match self
            .shared
            .retired
            .wait_timeout_while(state, timeout, |state| !fully_retired(state))
        {
            Ok((state, _)) => fully_retired(&state) && !self.shared.operation_lock.is_poisoned(),
            Err(_) => false,
        }
    }
}

impl Default for AudioCaptureService {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for AudioCaptureService {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn enumerate_input_devices() -> Result<Vec<InputDevice>, CaptureError> {
    let devices = cpal::default_host()
        .input_devices()
        .map_err(|error| CaptureError::InputUnavailable(error.to_string()))?
        .filter_map(|device| {
            let description = device.description().ok()?;
            let id = device.id().ok()?;
            (description.driver() != Some("null"))
                .then(|| input_device_descriptor(&id, &description, cfg!(windows)))
        })
        .collect();
    Ok(devices)
}

fn input_device_descriptor(
    id: &cpal::DeviceId,
    description: &cpal::DeviceDescription,
    prefer_friendly_name: bool,
) -> InputDevice {
    let label = prefer_friendly_name
        .then(|| {
            description
                .extended()
                .iter()
                .find(|name| !name.trim().is_empty())
                .map(String::as_str)
        })
        .flatten()
        .unwrap_or_else(|| description.name());
    InputDevice {
        id: id.to_string(),
        label: label.to_string(),
    }
}

fn validate(id: u64, rate: u32, frame_size: usize) -> Result<(), CaptureError> {
    validate_id(id)?;
    if rate == 0 {
        return Err(CaptureError::InvalidSampleRate);
    }
    if frame_size == 0 {
        return Err(CaptureError::InvalidFrameSize);
    }
    Ok(())
}

fn validate_id(id: u64) -> Result<(), CaptureError> {
    if id == 0 || id > MAX_CAPTURE_ID {
        Err(CaptureError::InvalidCaptureId)
    } else {
        Ok(())
    }
}

fn ensure_teardown_complete(state: &State) -> Result<(), CaptureError> {
    if state.teardown_count == 0 {
        Ok(())
    } else {
        Err(CaptureError::StopTimedOut)
    }
}

fn ensure_install_authorized(
    shared: &Shared,
    state: &State,
    capture_id: u64,
) -> Result<(), CaptureError> {
    if shared.shutting_down.load(Ordering::Acquire) {
        return Err(CaptureError::ShuttingDown);
    }
    ensure_teardown_complete(state)?;
    if state.latest_id != capture_id || capture_id <= state.cancelled_through {
        return Err(CaptureError::Superseded);
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn prepare_capture(
    shared: Weak<Shared>,
    capture_id: u64,
    selected_device_id: Option<&str>,
    target_rate_hz: u32,
    frame_size: usize,
    mut callback: CaptureCallback,
    terminal_callback: CaptureTerminalCallback,
) -> Result<(ActiveCapture, bool), CaptureError> {
    let host = cpal::default_host();
    let selected = selected_device_id
        .and_then(|wanted| wanted.parse::<cpal::DeviceId>().ok())
        .and_then(|id| host.device_by_id(&id))
        .filter(DeviceTrait::supports_input);
    let fell_back_to_default = selected_device_id.is_some() && selected.is_none();
    let device = selected
        .or_else(|| host.default_input_device())
        .ok_or_else(|| CaptureError::InputUnavailable("no default input device".to_string()))?;
    let supported_config = device
        .default_input_config()
        .map_err(|error| CaptureError::InputFailed(error.to_string()))?;
    let channels = usize::from(supported_config.channels());
    let input_rate_hz = supported_config.sample_rate();
    let sample_format = supported_config.sample_format();
    let stream_config = supported_config.into();
    let (sample_sender, sample_receiver) = mpsc::sync_channel(SAMPLE_QUEUE_CAPACITY);
    let (control_sender, control_receiver) = mpsc::channel();
    let worker_control = control_sender.clone();
    let stream = build_stream(
        &device,
        &stream_config,
        sample_format,
        sample_sender,
        control_sender,
    )?;
    let worker = thread::spawn(move || {
        let terminal_error = worker_loop(
            input_rate_hz,
            channels,
            target_rate_hz,
            frame_size,
            sample_receiver,
            control_receiver,
            &mut callback,
        );
        if let Some(error) = terminal_error {
            publish_terminal(shared, capture_id, error, terminal_callback);
        }
    });
    Ok((
        ActiveCapture {
            stream: SourceHandle::Native(stream),
            control: worker_control,
            worker,
        },
        fell_back_to_default,
    ))
}

fn build_stream(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    sample_format: cpal::SampleFormat,
    sample_sender: mpsc::SyncSender<Vec<f32>>,
    control_sender: mpsc::Sender<WorkerControl>,
) -> Result<cpal::Stream, CaptureError> {
    macro_rules! build {
        ($sample:ty, $convert:expr) => {{
            let failed = Arc::new(AtomicBool::new(false));
            let callback_failed = Arc::clone(&failed);
            let callback_control = control_sender.clone();
            let error_control = control_sender.clone();
            device.build_input_stream(
                config,
                move |data: &[$sample], _| {
                    if callback_failed.load(Ordering::Acquire) {
                        return;
                    }
                    let samples = data.iter().map($convert).collect();
                    if sample_sender.try_send(samples).is_err() {
                        send_failure_once(
                            &callback_failed,
                            &callback_control,
                            CaptureError::ChannelOverflow,
                        );
                    }
                },
                move |error| {
                    send_failure_once(
                        &failed,
                        &error_control,
                        CaptureError::InputFailed(error.to_string()),
                    );
                },
                None,
            )
        }};
    }

    let stream = match sample_format {
        cpal::SampleFormat::I8 => build!(i8, sample_to_f32),
        cpal::SampleFormat::I16 => build!(i16, sample_to_f32),
        cpal::SampleFormat::I24 => build!(cpal::I24, sample_to_f32),
        cpal::SampleFormat::I32 => build!(i32, sample_to_f32),
        cpal::SampleFormat::I64 => build!(i64, sample_to_f32),
        cpal::SampleFormat::U8 => build!(u8, sample_to_f32),
        cpal::SampleFormat::U16 => build!(u16, sample_to_f32),
        cpal::SampleFormat::U24 => build!(cpal::U24, sample_to_f32),
        cpal::SampleFormat::U32 => build!(u32, sample_to_f32),
        cpal::SampleFormat::U64 => build!(u64, sample_to_f32),
        cpal::SampleFormat::F32 => build!(f32, sample_to_f32),
        cpal::SampleFormat::F64 => build!(f64, sample_to_f32),
        _ => {
            return Err(CaptureError::InputFailed(
                "unsupported input sample format".to_string(),
            ));
        }
    };
    stream.map_err(|error| CaptureError::InputFailed(error.to_string()))
}

fn sample_to_f32<S>(sample: &S) -> f32
where
    S: cpal::Sample,
    f32: cpal::FromSample<S>,
{
    f32::from_sample(*sample)
}

fn send_failure_once(
    failed: &AtomicBool,
    control: &mpsc::Sender<WorkerControl>,
    error: CaptureError,
) {
    if !failed.swap(true, Ordering::AcqRel) {
        let _ = control.send(WorkerControl::Fail(error));
    }
}

fn take_active_for_teardown(state: &mut State) -> Option<ActiveCapture> {
    let active = state.active.take();
    if active.is_some() {
        begin_teardown(state);
    }
    active
}

fn begin_teardown(state: &mut State) {
    state.teardown_count = state.teardown_count.saturating_add(1);
}

fn finish_teardown(shared: &Shared) {
    if let Ok(mut state) = shared.state.lock() {
        state.teardown_count = state.teardown_count.saturating_sub(1);
    }
    shared.retired.notify_all();
}

fn stop_uninstalled_active(
    active: ActiveCapture,
    shared: &Arc<Shared>,
) -> Result<(), CaptureError> {
    {
        let mut state = shared
            .state
            .lock()
            .map_err(|_| CaptureError::StatePoisoned)?;
        begin_teardown(&mut state);
    }
    stop_active(active, shared)
}

fn stop_active(active: ActiveCapture, shared: &Arc<Shared>) -> Result<(), CaptureError> {
    let teardown_shared = Arc::clone(shared);
    let (done_sender, done_receiver) = mpsc::sync_channel(1);
    thread::spawn(move || {
        let ActiveCapture {
            stream,
            control,
            worker,
        } = active;
        let _ = control.send(WorkerControl::Stop);
        drop(stream);
        let _ = worker.join();
        finish_teardown(&teardown_shared);
        let _ = done_sender.send(());
    });
    done_receiver
        .recv_timeout(STOP_WAIT)
        .map_err(|_| CaptureError::StopTimedOut)
}

fn worker_loop(
    input_rate_hz: u32,
    channels: usize,
    output_rate_hz: u32,
    frame_size: usize,
    samples: mpsc::Receiver<Vec<f32>>,
    control: mpsc::Receiver<WorkerControl>,
    callback: &mut CaptureCallback,
) -> Option<CaptureError> {
    match await_activation(&control) {
        WorkerActivation::Run => {}
        WorkerActivation::Fail(error) => return Some(error),
        WorkerActivation::Stop => return None,
    }
    let mut resampler = Resampler::new(input_rate_hz, output_rate_hz, channels);
    loop {
        match control.try_recv() {
            Ok(WorkerControl::Activate) => {}
            Ok(WorkerControl::Stop) => return None,
            Ok(WorkerControl::Fail(error)) => return Some(error),
            Err(mpsc::TryRecvError::Disconnected) => return None,
            Err(mpsc::TryRecvError::Empty) => {}
        }
        match samples.recv_timeout(WORKER_POLL_INTERVAL) {
            Ok(samples) => {
                if samples.iter().any(|sample| !sample.is_finite()) {
                    return Some(CaptureError::InvalidSample);
                }
                resampler.push(&samples, callback, frame_size);
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Some(CaptureError::InputFailed(
                    "audio input stream ended unexpectedly".to_string(),
                ));
            }
        }
    }
}

enum WorkerActivation {
    Run,
    Fail(CaptureError),
    Stop,
}

fn await_activation(control: &mpsc::Receiver<WorkerControl>) -> WorkerActivation {
    let mut failure = None;
    loop {
        match control.recv() {
            Ok(WorkerControl::Activate) => {
                return failure.map_or(WorkerActivation::Run, WorkerActivation::Fail);
            }
            Ok(WorkerControl::Fail(error)) => failure = Some(error),
            Ok(WorkerControl::Stop) | Err(_) => return WorkerActivation::Stop,
        }
    }
}

fn publish_terminal(
    shared: Weak<Shared>,
    capture_id: u64,
    error: CaptureError,
    terminal_callback: CaptureTerminalCallback,
) {
    let Some(shared) = shared.upgrade() else {
        return;
    };
    let (authoritative, active) = shared.state.lock().ok().map_or((false, None), |mut state| {
        if state.current_id != Some(capture_id) {
            return (false, None);
        }
        state.current_id = None;
        let active = take_active_for_teardown(&mut state);
        shared.retired.notify_all();
        (true, active)
    });
    if let Some(active) = active {
        let ActiveCapture { stream, worker, .. } = active;
        let teardown_shared = Arc::clone(&shared);
        thread::spawn(move || {
            drop(stream);
            let _ = worker.join();
            finish_teardown(&teardown_shared);
        });
    }
    if authoritative {
        terminal_callback(CaptureTerminal { capture_id, error });
    }
}

struct Resampler {
    input_rate_hz: u32,
    output_rate_hz: u32,
    channels: usize,
    channel_group: Vec<f32>,
    previous_mono: Option<f32>,
    position: f64,
    frame: Vec<f32>,
}

impl Resampler {
    fn new(input_rate_hz: u32, output_rate_hz: u32, channels: usize) -> Self {
        Self {
            input_rate_hz,
            output_rate_hz,
            channels,
            channel_group: Vec::with_capacity(channels),
            previous_mono: None,
            position: 0.0,
            frame: Vec::new(),
        }
    }

    fn push(&mut self, samples: &[f32], callback: &mut CaptureCallback, frame_size: usize) {
        for sample in samples {
            self.channel_group.push(*sample);
            if self.channel_group.len() != self.channels {
                continue;
            }
            let mono = self.channel_group.iter().sum::<f32>() / self.channels as f32;
            self.channel_group.clear();
            self.add_mono(mono, callback, frame_size);
        }
    }

    fn add_mono(&mut self, sample: f32, callback: &mut CaptureCallback, frame_size: usize) {
        if let Some(previous) = self.previous_mono.replace(sample) {
            let step = f64::from(self.input_rate_hz) / f64::from(self.output_rate_hz);
            while self.position < 1.0 {
                self.frame
                    .push(previous + (sample - previous) * self.position as f32);
                self.position += step;
                if self.frame.len() == frame_size {
                    callback(std::mem::take(&mut self.frame));
                }
            }
            self.position -= 1.0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Barrier;

    struct TestSourceDrop {
        entered: mpsc::Sender<()>,
        release: mpsc::Receiver<()>,
        dropped: mpsc::Sender<()>,
    }

    impl Drop for TestSourceDrop {
        fn drop(&mut self) {
            let _ = self.entered.send(());
            let _ = self.release.recv_timeout(Duration::from_secs(5));
            let _ = self.dropped.send(());
        }
    }

    struct SourceRetirementProbe {
        entered: mpsc::Receiver<()>,
        release: mpsc::Sender<()>,
        dropped: mpsc::Receiver<()>,
    }

    fn test_source() -> (SourceHandle, SourceRetirementProbe) {
        let (entered_tx, entered) = mpsc::channel();
        let (release, release_rx) = mpsc::channel();
        let (dropped_tx, dropped) = mpsc::channel();
        (
            SourceHandle::Test(Box::new(TestSourceDrop {
                entered: entered_tx,
                release: release_rx,
                dropped: dropped_tx,
            })),
            SourceRetirementProbe {
                entered,
                release,
                dropped,
            },
        )
    }

    struct CaptureRetirementProbe {
        source: SourceRetirementProbe,
        worker_stopping: mpsc::Receiver<()>,
        release_worker: mpsc::Sender<()>,
    }

    fn install_test_capture(service: &AudioCaptureService, id: u64) -> CaptureRetirementProbe {
        let (stream, source) = test_source();
        let (control, controls) = mpsc::channel();
        let (stopping_tx, worker_stopping) = mpsc::channel();
        let (release_worker, release_rx) = mpsc::channel();
        let worker = thread::spawn(move || {
            if matches!(
                controls.recv_timeout(Duration::from_secs(5)),
                Ok(WorkerControl::Stop)
            ) {
                let _ = stopping_tx.send(());
                let _ = release_rx.recv_timeout(Duration::from_secs(5));
            }
        });
        let mut state = service.shared.state.lock().unwrap();
        assert!(fully_retired(&state));
        state.latest_id = id;
        state.next_id = id;
        state.current_id = Some(id);
        state.active = Some(ActiveCapture {
            stream,
            control,
            worker,
        });
        CaptureRetirementProbe {
            source,
            worker_stopping,
            release_worker,
        }
    }

    #[test]
    fn capture_status_tracks_the_authoritative_capture_not_reserved_ids() {
        let service = AudioCaptureService::new();
        let id = service.reserve_id().unwrap();
        assert_eq!(service.is_capturing(), Ok(false));
        service.shared.state.lock().unwrap().current_id = Some(id);
        assert_eq!(service.is_capturing(), Ok(true));
        service.stop(id).unwrap();
        assert_eq!(service.is_capturing(), Ok(false));
    }

    #[test]
    fn reordered_duplicate_labels_preserve_stable_device_ids() {
        let host_id = cpal::default_host().id();
        let first_id = cpal::DeviceId(host_id, "stable-first".to_string());
        let second_id = cpal::DeviceId(host_id, "stable-second".to_string());
        let generic = cpal::DeviceDescriptionBuilder::new("Microphone").build();
        let listed = [
            input_device_descriptor(&first_id, &generic, false),
            input_device_descriptor(&second_id, &generic, false),
        ];
        let reordered = [
            input_device_descriptor(&second_id, &generic, false),
            input_device_descriptor(&first_id, &generic, false),
        ];

        assert_eq!(listed[0].id, reordered[1].id);
        assert_eq!(listed[1].id, reordered[0].id);
        assert_ne!(listed[0].id, listed[1].id);
    }

    #[test]
    fn full_windows_friendly_names_distinguish_generic_microphones() {
        let host_id = cpal::default_host().id();
        let first_id = cpal::DeviceId(host_id, "synthetic-usb".to_string());
        let second_id = cpal::DeviceId(host_id, "synthetic-virtual".to_string());
        let usb = cpal::DeviceDescriptionBuilder::new("Microphone")
            .add_extended_line("Microphone (Example USB)")
            .build();
        let virtual_mic = cpal::DeviceDescriptionBuilder::new("Microphone")
            .add_extended_line("Microphone (Example Virtual)")
            .build();

        let first = input_device_descriptor(&first_id, &usb, true);
        let second = input_device_descriptor(&second_id, &virtual_mic, true);

        assert_eq!(first.label, "Microphone (Example USB)");
        assert_eq!(second.label, "Microphone (Example Virtual)");
        assert_ne!(first.id, second.id);
        assert_eq!(
            input_device_descriptor(&first_id, &usb, false).label,
            "Microphone"
        );
    }

    #[test]
    fn validation_rejects_invalid_contract_values() {
        assert_eq!(validate(0, 1, 1), Err(CaptureError::InvalidCaptureId));
        assert_eq!(validate(1, 0, 1), Err(CaptureError::InvalidSampleRate));
        assert_eq!(validate(1, 1, 0), Err(CaptureError::InvalidFrameSize));
    }

    #[test]
    fn converts_every_pcm_input_format_to_f32() {
        let i24_zero = cpal::I24::new(0).unwrap();
        let u24_midpoint = cpal::U24::new(1 << 23).unwrap();
        for sample in [
            sample_to_f32(&0_i8),
            sample_to_f32(&0_i16),
            sample_to_f32(&i24_zero),
            sample_to_f32(&0_i32),
            sample_to_f32(&0_i64),
            sample_to_f32(&128_u8),
            sample_to_f32(&32_768_u16),
            sample_to_f32(&u24_midpoint),
            sample_to_f32(&(1_u32 << 31)),
            sample_to_f32(&(1_u64 << 63)),
            sample_to_f32(&0.0_f32),
            sample_to_f32(&0.0_f64),
        ] {
            assert!(sample.abs() <= f32::EPSILON);
        }
    }

    #[test]
    fn pending_teardown_blocks_replacement() {
        let state = State {
            teardown_count: 1,
            ..State::default()
        };
        assert_eq!(
            ensure_teardown_complete(&state),
            Err(CaptureError::StopTimedOut)
        );
    }

    #[test]
    fn stop_current_does_not_report_idle_while_a_detached_teardown_is_pending() {
        let service = AudioCaptureService::new();
        service.shared.state.lock().unwrap().teardown_count = 1;
        assert_eq!(service.stop_current(), Err(CaptureError::StopTimedOut));
        assert!(!service.wait_for_retirement(Duration::ZERO));
        finish_teardown(&service.shared);
        assert!(service.wait_for_retirement(Duration::ZERO));
    }

    #[test]
    fn stop_current_retires_physical_source_then_worker_and_allows_a_fresh_id() {
        let service = Arc::new(AudioCaptureService::new());
        let probe = install_test_capture(&service, 7);
        let reserved_before_stop = service.reserve_id().unwrap();
        let stopping = Arc::clone(&service);
        let stop = thread::spawn(move || stopping.stop_current());
        probe
            .worker_stopping
            .recv_timeout(Duration::from_secs(5))
            .unwrap();
        probe
            .source
            .entered
            .recv_timeout(Duration::from_secs(5))
            .unwrap();
        assert_eq!(stop.join().unwrap(), Err(CaptureError::StopTimedOut));
        assert!(!service.is_capturing().unwrap());
        assert_eq!(service.stop_current(), Err(CaptureError::StopTimedOut));
        assert!(!service.wait_for_retirement(Duration::ZERO));
        probe.source.release.send(()).unwrap();
        probe
            .source
            .dropped
            .recv_timeout(Duration::from_secs(5))
            .unwrap();
        assert!(
            !service.wait_for_retirement(Duration::from_millis(10)),
            "worker is still held after source drop"
        );
        assert_eq!(service.shared.state.lock().unwrap().teardown_count, 1);
        let waiting = Arc::clone(&service);
        let waiter = thread::spawn(move || waiting.wait_for_retirement(Duration::from_secs(5)));
        probe.release_worker.send(()).unwrap();
        assert!(waiter.join().unwrap());
        assert!(service.wait_for_retirement(Duration::ZERO));
        assert!(!service.stop_current().unwrap());
        let next = service.reserve_id().unwrap();
        assert!(next > reserved_before_stop);
        let replacement = install_test_capture(&service, next);
        assert!(
            !service.stop(7).unwrap(),
            "stale ID stop must not retire replacement"
        );
        assert_eq!(service.shared.state.lock().unwrap().current_id, Some(next));
        replacement.source.release.send(()).unwrap();
        replacement.release_worker.send(()).unwrap();
        assert!(service.stop_current().unwrap());
        assert!(service.wait_for_retirement(Duration::ZERO));
    }

    #[test]
    fn terminal_retirement_includes_the_publishing_worker_callback() {
        let service = Arc::new(AudioCaptureService::new());
        let (stream, source) = test_source();
        let (control, _controls) = mpsc::channel();
        let (start_tx, start_rx) = mpsc::channel();
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let shared = Arc::downgrade(&service.shared);
        let worker = thread::spawn(move || {
            start_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            publish_terminal(
                shared,
                9,
                CaptureError::InvalidSample,
                Box::new(move |_| {
                    let _ = entered_tx.send(());
                    let _ = release_rx.recv_timeout(Duration::from_secs(5));
                }),
            );
        });
        {
            let mut state = service.shared.state.lock().unwrap();
            state.latest_id = 9;
            state.current_id = Some(9);
            state.active = Some(ActiveCapture {
                stream,
                control,
                worker,
            });
        }
        start_tx.send(()).unwrap();
        entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        source.entered.recv_timeout(Duration::from_secs(5)).unwrap();
        source.release.send(()).unwrap();
        source.dropped.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(service.stop_current(), Err(CaptureError::StopTimedOut));
        assert!(
            !service.wait_for_retirement(Duration::from_millis(10)),
            "terminal publication is not worker exit"
        );
        release_tx.send(()).unwrap();
        assert!(service.wait_for_retirement(Duration::from_secs(5)));
    }

    #[test]
    fn stop_current_serializes_with_start_and_tombstones_reserved_installations() {
        let service = Arc::new(AudioCaptureService::new());
        let operation = service.shared.operation_lock.lock().unwrap();
        let starting = StartLease::begin(&service.shared).unwrap();
        let reserved = service.reserve_id().unwrap();
        let stopping = Arc::clone(&service);
        let (done_tx, done_rx) = mpsc::channel();
        let worker = thread::spawn(move || done_tx.send(stopping.stop_current()).unwrap());
        assert!(matches!(
            done_rx.recv_timeout(Duration::from_millis(10)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ));
        assert!(!service.wait_for_retirement(Duration::ZERO));
        drop(starting);
        drop(operation);
        assert_eq!(done_rx.recv_timeout(Duration::from_secs(5)), Ok(Ok(false)));
        worker.join().unwrap();
        let state = service.shared.state.lock().unwrap();
        assert_eq!(state.cancelled_through, reserved);
        assert_eq!(
            ensure_install_authorized(&service.shared, &state, reserved),
            Err(CaptureError::Superseded)
        );
        drop(state);
        assert!(service.reserve_id().unwrap() > reserved);
    }

    #[cfg(feature = "benchmark-input")]
    #[test]
    fn stop_current_allows_restart_through_the_software_capture_backend() {
        let service = AudioCaptureService::new_benchmark(16_000, 1).unwrap();
        let first = service.reserve_id().unwrap();
        service
            .start(first, None, 16_000, 480, Box::new(|_| {}), Box::new(|_| {}))
            .unwrap();
        assert!(service.stop_current().unwrap());
        assert!(service.wait_for_retirement(Duration::ZERO));
        let second = service.reserve_id().unwrap();
        assert!(second > first);
        service
            .start(
                second,
                None,
                16_000,
                480,
                Box::new(|_| {}),
                Box::new(|_| {}),
            )
            .unwrap();
        assert!(!service.stop(first).unwrap());
        assert!(service.is_capturing().unwrap());
        service.shutdown();
        assert!(service.wait_for_retirement(Duration::from_secs(5)));
    }

    #[test]
    fn shutdown_waits_for_uninstalled_start_and_poison_fails_closed() {
        let service = AudioCaptureService::new();
        let starting = StartLease::begin(&service.shared).unwrap();
        service.shutdown();
        assert!(!service.wait_for_retirement(Duration::ZERO));
        drop(starting);
        assert!(service.wait_for_retirement(Duration::ZERO));
        assert_eq!(service.reserve_id(), Err(CaptureError::ShuttingDown));

        let poisoned = AudioCaptureService::new();
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _state = poisoned.shared.state.lock().unwrap();
            panic!("synthetic capture state poison");
        }));
        assert!(!poisoned.wait_for_retirement(Duration::ZERO));
        assert_eq!(poisoned.stop_current(), Err(CaptureError::StatePoisoned));
    }

    #[test]
    fn terminal_teardown_publication_precedes_replacement_eligibility() {
        let state = Arc::new(Mutex::new(State::default()));
        let teardown_published = Arc::new(Barrier::new(2));
        let terminal_state = Arc::clone(&state);
        let terminal_barrier = Arc::clone(&teardown_published);
        let terminal = thread::spawn(move || {
            let mut state = terminal_state.lock().unwrap();
            begin_teardown(&mut state);
            terminal_barrier.wait();
        });

        teardown_published.wait();
        assert_eq!(
            ensure_teardown_complete(&state.lock().unwrap()),
            Err(CaptureError::StopTimedOut)
        );
        terminal.join().unwrap();
    }

    #[test]
    fn pre_activation_failure_waits_for_authoritative_installation() {
        let (_sample_sender, sample_receiver) = mpsc::sync_channel(1);
        let (control_sender, control_receiver) = mpsc::channel();
        let (result_sender, result_receiver) = mpsc::sync_channel(1);
        let mut callback: CaptureCallback = Box::new(|_| {});
        thread::spawn(move || {
            let result = worker_loop(
                48_000,
                1,
                16_000,
                480,
                sample_receiver,
                control_receiver,
                &mut callback,
            );
            result_sender.send(result).unwrap();
        });

        control_sender
            .send(WorkerControl::Fail(CaptureError::InvalidSample))
            .unwrap();
        assert!(matches!(
            result_receiver.recv_timeout(Duration::from_millis(10)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ));
        control_sender.send(WorkerControl::Activate).unwrap();
        assert_eq!(
            result_receiver.recv_timeout(Duration::from_millis(100)),
            Ok(Some(CaptureError::InvalidSample))
        );
    }

    #[test]
    fn repeated_stream_failures_publish_only_once() {
        let failed = AtomicBool::new(false);
        let (control_sender, control_receiver) = mpsc::channel();
        for _ in 0..100 {
            send_failure_once(
                &failed,
                &control_sender,
                CaptureError::InputFailed("device removed".to_string()),
            );
        }

        assert!(matches!(
            control_receiver.try_recv(),
            Ok(WorkerControl::Fail(CaptureError::InputFailed(message)))
                if message == "device removed"
        ));
        assert!(matches!(
            control_receiver.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
    }

    #[test]
    fn stopping_an_id_does_not_supersede_the_next_id() {
        let service = AudioCaptureService::new();
        assert!(!service.stop(1).unwrap());
        let state = service.shared.state.lock().unwrap();
        assert_eq!(state.latest_id, 1);
        assert_eq!(state.cancelled_through, 1);
    }

    #[test]
    fn native_id_reservations_advance_past_stopped_renderer_sessions() {
        let service = AudioCaptureService::new();
        let first = service.reserve_id().expect("reserve first capture");
        assert!(!service.stop(first).expect("stop first capture"));

        let second = service.reserve_id().expect("reserve replacement capture");

        assert!(second > first);
        service.shared.state.lock().unwrap().current_id = Some(second);
        assert!(!service.stop(first).expect("repeat stale stop"));
        let state = service.shared.state.lock().unwrap();
        assert_eq!(state.cancelled_through, first);
        assert_eq!(state.current_id, Some(second));
    }

    #[test]
    fn concurrent_native_id_reservations_are_unique_and_bounded() {
        let service = Arc::new(AudioCaptureService::new());
        let (ready_sender, ready_receiver) = mpsc::channel();
        let (result_sender, result_receiver) = mpsc::channel();
        let mut starts = Vec::new();
        let workers = (0..32)
            .map(|_| {
                let service = Arc::clone(&service);
                let ready_sender = ready_sender.clone();
                let result_sender = result_sender.clone();
                let (start_sender, start_receiver) = mpsc::channel();
                starts.push(start_sender);
                thread::spawn(move || {
                    if ready_sender.send(()).is_err()
                        || start_receiver.recv_timeout(Duration::from_secs(5)).is_err()
                    {
                        return;
                    }
                    let _ = result_sender.send(service.reserve_id());
                })
            })
            .collect::<Vec<_>>();
        drop(ready_sender);
        drop(result_sender);
        for _ in 0..32 {
            ready_receiver
                .recv_timeout(Duration::from_secs(5))
                .expect("all reservation workers should reach the coordinated start");
        }
        for start in starts {
            start.send(()).expect("reservation worker should be ready");
        }
        let mut reservations = (0..32)
            .map(|_| {
                result_receiver
                    .recv_timeout(Duration::from_secs(5))
                    .expect("reservation worker should finish within the bound")
                    .expect("reserve capture")
            })
            .collect::<Vec<_>>();
        for worker in workers {
            worker.join().expect("reservation worker");
        }
        reservations.sort_unstable();
        assert_eq!(reservations, (1..=32).collect::<Vec<_>>());

        service.shared.state.lock().unwrap().next_id = MAX_CAPTURE_ID;
        assert_eq!(service.reserve_id(), Err(CaptureError::CaptureIdExhausted));
    }

    #[test]
    fn stop_does_not_wait_for_an_in_flight_start_operation() {
        let service = Arc::new(AudioCaptureService::new());
        let operation = service.shared.operation_lock.lock().unwrap();
        let stopping_service = Arc::clone(&service);
        let (result_sender, result_receiver) = mpsc::sync_channel(1);
        thread::spawn(move || {
            result_sender.send(stopping_service.stop(1)).unwrap();
        });

        assert_eq!(
            result_receiver.recv_timeout(Duration::from_millis(50)),
            Ok(Ok(false))
        );
        drop(operation);
    }

    #[test]
    fn shutdown_invalidates_delayed_installation_without_the_operation_lock() {
        let service = AudioCaptureService::new();
        let operation = service.shared.operation_lock.lock().unwrap();
        service.shared.state.lock().unwrap().latest_id = 1;

        service.shutdown();

        let state = service.shared.state.lock().unwrap();
        assert_eq!(
            ensure_install_authorized(&service.shared, &state, 1),
            Err(CaptureError::ShuttingDown)
        );
        assert!(state.active.is_none());
        drop(state);
        drop(operation);
    }

    #[test]
    fn resamples_48k_to_16k_into_exact_frames() {
        let mut resampler = Resampler::new(48_000, 16_000, 1);
        let frames = Arc::new(Mutex::new(Vec::new()));
        let output = Arc::clone(&frames);
        let mut callback: CaptureCallback =
            Box::new(move |frame| output.lock().unwrap().push(frame));
        let input: Vec<_> = (0..4_800).map(|sample| sample as f32).collect();
        resampler.push(&input, &mut callback, 160);
        let frames = frames.lock().unwrap();
        assert_eq!(frames.len(), 10);
        assert!(frames.iter().all(|frame| frame.len() == 160));
        assert!(resampler.channel_group.is_empty());
        assert!(resampler.frame.len() < 160);
    }

    #[test]
    fn long_multichannel_input_keeps_internal_buffers_bounded() {
        let mut resampler = Resampler::new(48_000, 16_000, 8);
        let mut callback: CaptureCallback = Box::new(|_| {});
        for _ in 0..100_000 {
            resampler.push(&[0.0; 8], &mut callback, 160);
        }
        assert!(resampler.channel_group.len() < 8);
        assert!(resampler.frame.len() < 160);
    }

    #[test]
    fn stalled_worker_stops_without_waiting_for_samples() {
        let (_sample_sender, sample_receiver) = mpsc::sync_channel(1);
        let (control_sender, control_receiver) = mpsc::channel();
        let mut callback: CaptureCallback = Box::new(|_| {});
        let worker = thread::spawn(move || {
            worker_loop(
                48_000,
                1,
                16_000,
                480,
                sample_receiver,
                control_receiver,
                &mut callback,
            )
        });
        control_sender.send(WorkerControl::Stop).unwrap();
        assert_eq!(worker.join().unwrap(), None);
    }

    #[test]
    fn terminal_cleanup_is_scoped_to_the_authoritative_capture() {
        let shared = Arc::new(Shared {
            operation_lock: Mutex::new(()),
            retired: Condvar::new(),
            state: Mutex::new(State {
                latest_id: 2,
                current_id: Some(2),
                ..State::default()
            }),
            shutting_down: AtomicBool::new(false),
        });
        let stale_called = Arc::new(AtomicBool::new(false));
        let stale_result = Arc::clone(&stale_called);
        publish_terminal(
            Arc::downgrade(&shared),
            1,
            CaptureError::InvalidSample,
            Box::new(move |_| stale_result.store(true, Ordering::Release)),
        );
        assert!(!stale_called.load(Ordering::Acquire));
        assert_eq!(shared.state.lock().unwrap().current_id, Some(2));

        let current_called = Arc::new(AtomicBool::new(false));
        let current_result = Arc::clone(&current_called);
        publish_terminal(
            Arc::downgrade(&shared),
            2,
            CaptureError::InvalidSample,
            Box::new(move |_| current_result.store(true, Ordering::Release)),
        );
        assert!(current_called.load(Ordering::Acquire));
        assert_eq!(shared.state.lock().unwrap().current_id, None);
    }
}
