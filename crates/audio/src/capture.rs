use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex, Weak};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use rodio::cpal;
use rodio::cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use rodio::cpal::Sample as _;

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
}

struct Shared {
    operation_lock: Mutex<()>,
    state: Mutex<State>,
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
}

struct ActiveCapture {
    stream: cpal::Stream,
    control: mpsc::Sender<WorkerControl>,
    worker: JoinHandle<()>,
}

enum WorkerControl {
    Activate,
    Stop,
    Fail(CaptureError),
}

impl AudioCaptureService {
    pub fn new() -> Self {
        Self {
            shared: Arc::new(Shared {
                operation_lock: Mutex::new(()),
                state: Mutex::new(State::default()),
                shutting_down: AtomicBool::new(false),
            }),
        }
    }

    pub fn list_input_devices(&self) -> Result<Vec<InputDevice>, CaptureError> {
        enumerate_input_devices()
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

        let (active, fell_back_to_default) = prepare_capture(
            Arc::downgrade(&self.shared),
            capture_id,
            selected_device_id.as_deref(),
            target_rate_hz,
            frame_size,
            callback,
            terminal_callback,
        )?;
        if let Err(error) = active.stream.play() {
            let _ = stop_uninstalled_active(active, &self.shared);
            return Err(CaptureError::InputFailed(error.to_string()));
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
                take_active_for_teardown(&mut state)
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
            take_active_for_teardown(&mut state)
        });
        if let Some(active) = active {
            let _ = stop_active(active, &self.shared);
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
                .then(|| input_device_descriptor(&id, description.name()))
        })
        .collect();
    Ok(devices)
}

fn input_device_descriptor(id: &cpal::DeviceId, label: &str) -> InputDevice {
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
            stream,
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
        (true, active)
    });
    if let Some(active) = active {
        let ActiveCapture { stream, worker, .. } = active;
        drop(worker);
        let teardown_shared = Arc::clone(&shared);
        thread::spawn(move || {
            drop(stream);
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

    #[test]
    fn reordered_duplicate_labels_preserve_stable_device_ids() {
        let host_id = cpal::default_host().id();
        let first_id = cpal::DeviceId(host_id, "stable-first".to_string());
        let second_id = cpal::DeviceId(host_id, "stable-second".to_string());
        let listed = [
            input_device_descriptor(&first_id, "Microphone"),
            input_device_descriptor(&second_id, "Microphone"),
        ];
        let reordered = [
            input_device_descriptor(&second_id, "Microphone"),
            input_device_descriptor(&first_id, "Microphone"),
        ];

        assert_eq!(listed[0].id, reordered[1].id);
        assert_eq!(listed[1].id, reordered[0].id);
        assert_ne!(listed[0].id, listed[1].id);
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
        let mut reservations = (0..32)
            .map(|_| {
                let service = Arc::clone(&service);
                thread::spawn(move || service.reserve_id().expect("reserve capture"))
            })
            .map(|worker| worker.join().expect("reservation worker"))
            .collect::<Vec<_>>();
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
