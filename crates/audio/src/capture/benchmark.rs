use super::*;
use std::time::Instant;

/// Receipt of the first/last clip samples by the raw sample queue, not acoustic
/// output or frontend/VAD consumption. Callbacks must remain nonblocking.
#[derive(Debug, Clone, Copy)]
pub enum BenchmarkAudioBoundary {
    Started(Instant),
    Completed(Instant),
}

struct Clip {
    samples: Vec<f32>,
    callback: Box<dyn FnMut(BenchmarkAudioBoundary) + Send>,
    offset: usize,
}

enum Command {
    Activate,
    Stop,
}

pub(super) struct Producer {
    commands: mpsc::Sender<Command>,
    clip: Arc<Mutex<Option<Clip>>>,
    busy: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl Producer {
    pub(super) fn activate(&self) {
        let _ = self.commands.send(Command::Activate);
    }
}

impl Drop for Producer {
    fn drop(&mut self) {
        let _ = self.commands.send(Command::Stop);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

impl AudioCaptureService {
    /// Snapshot of the normal capture owner for a benchmark-configured service.
    /// Queue admission revalidates this ID, so a concurrent stop/start is safe.
    pub fn benchmark_capture_id(&self) -> Result<Option<u64>, CaptureError> {
        if self.benchmark_input.is_none() {
            return Err(CaptureError::InputUnavailable(
                "benchmark input is not configured".into(),
            ));
        }
        self.shared
            .state
            .lock()
            .map(|state| state.current_id)
            .map_err(|_| CaptureError::StatePoisoned)
    }

    pub fn new_benchmark(input_rate_hz: u32, channels: usize) -> Result<Self, CaptureError> {
        // Explicit finite budgets also bound per-tick allocation and clip size.
        if input_rate_hz == 0 || input_rate_hz > 192_000 {
            return Err(CaptureError::InvalidSampleRate);
        }
        if channels == 0 || channels > 8 {
            return Err(CaptureError::InvalidFrameSize);
        }
        let mut service = Self::new();
        service.benchmark_input = Some((input_rate_hz, channels));
        Ok(service)
    }

    pub fn queue_benchmark_clip(
        &self,
        capture_id: u64,
        samples: Vec<f32>,
        callback: Box<dyn FnMut(BenchmarkAudioBoundary) + Send>,
    ) -> Result<(), CaptureError> {
        validate_id(capture_id)?;
        let (rate, channels) = self.benchmark_input.ok_or_else(|| {
            CaptureError::InputUnavailable("benchmark input is not configured".into())
        })?;
        if samples.is_empty() || !samples.len().is_multiple_of(channels) {
            return Err(CaptureError::InvalidFrameSize);
        }
        if samples.len() > rate as usize * channels * 60 {
            return Err(CaptureError::ChannelOverflow);
        }
        if samples.iter().any(|sample| !sample.is_finite()) {
            return Err(CaptureError::InvalidSample);
        }
        let state = self
            .shared
            .state
            .lock()
            .map_err(|_| CaptureError::StatePoisoned)?;
        if self.shared.shutting_down.load(Ordering::Acquire) {
            return Err(CaptureError::ShuttingDown);
        }
        if state.current_id != Some(capture_id) {
            return Err(CaptureError::Superseded);
        }
        let Some(ActiveCapture {
            stream: SourceHandle::Benchmark(producer),
            ..
        }) = &state.active
        else {
            return Err(CaptureError::Superseded);
        };
        let mut slot = producer
            .clip
            .lock()
            .map_err(|_| CaptureError::StatePoisoned)?;
        if producer.busy.swap(true, Ordering::AcqRel) {
            return Err(CaptureError::ChannelOverflow);
        }
        *slot = Some(Clip {
            samples,
            callback,
            offset: 0,
        });
        Ok(())
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn prepare(
    shared: Weak<Shared>,
    capture_id: u64,
    rate: u32,
    channels: usize,
    target_rate: u32,
    frame_size: usize,
    mut callback: CaptureCallback,
    terminal_callback: CaptureTerminalCallback,
) -> Result<(ActiveCapture, bool), CaptureError> {
    let (samples_tx, samples_rx) = mpsc::sync_channel(SAMPLE_QUEUE_CAPACITY);
    let (control_tx, control_rx) = mpsc::channel();
    let (commands_tx, commands_rx) = mpsc::channel();
    let clip = Arc::new(Mutex::new(None));
    let busy = Arc::new(AtomicBool::new(false));
    let producer_clip = Arc::clone(&clip);
    let producer_busy = Arc::clone(&busy);
    let producer_control = control_tx.clone();
    let producer = thread::spawn(move || {
        produce(
            rate,
            channels,
            samples_tx,
            producer_control,
            commands_rx,
            producer_clip,
            producer_busy,
        );
    });
    let worker = thread::spawn(move || {
        if let Some(error) = worker_loop(
            rate,
            channels,
            target_rate,
            frame_size,
            samples_rx,
            control_rx,
            &mut callback,
        ) {
            publish_terminal(shared, capture_id, error, terminal_callback);
        }
    });
    Ok((
        ActiveCapture {
            stream: SourceHandle::Benchmark(Producer {
                commands: commands_tx,
                clip,
                busy,
                worker: Some(producer),
            }),
            control: control_tx,
            worker,
        },
        false,
    ))
}

fn produce(
    rate: u32,
    channels: usize,
    samples: mpsc::SyncSender<Vec<f32>>,
    control: mpsc::Sender<WorkerControl>,
    commands: mpsc::Receiver<Command>,
    slot: Arc<Mutex<Option<Clip>>>,
    busy: Arc<AtomicBool>,
) {
    if !matches!(commands.recv(), Ok(Command::Activate)) {
        return;
    }
    let origin = Instant::now();
    let chunk_frames = (rate / 100).max(1) as usize;
    let mut frames_sent = 0_u64;
    let mut current: Option<Clip> = None;
    loop {
        let deadline = origin + Duration::from_secs_f64(frames_sent as f64 / f64::from(rate));
        match commands.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok(Command::Stop) | Err(mpsc::RecvTimeoutError::Disconnected) => return,
            Ok(Command::Activate) => continue,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        if current.is_none() {
            let Ok(mut slot) = slot.lock() else { return };
            current = slot.take();
        }
        let mut data = vec![0.0; chunk_frames * channels];
        let mut boundaries = None;
        if let Some(clip) = current.as_mut() {
            let count = data.len().min(clip.samples.len() - clip.offset);
            data[..count].copy_from_slice(&clip.samples[clip.offset..clip.offset + count]);
            let first = clip.offset == 0;
            clip.offset += count;
            boundaries = Some((first, clip.offset == clip.samples.len()));
        }
        if samples.try_send(data).is_err() {
            let _ = control.send(WorkerControl::Fail(CaptureError::ChannelOverflow));
            return;
        }
        let admitted = Instant::now();
        if let Some((first, last)) = boundaries {
            let clip = current.as_mut().expect("clip boundaries require a clip");
            if first {
                (clip.callback)(BenchmarkAudioBoundary::Started(admitted));
            }
            if last {
                (clip.callback)(BenchmarkAudioBoundary::Completed(admitted));
                current = None;
                busy.store(false, Ordering::Release);
            }
        }
        frames_sent += chunk_frames as u64;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn start(service: &AudioCaptureService, id: u64) -> mpsc::Receiver<Vec<f32>> {
        let (tx, rx) = mpsc::channel();
        let result = service
            .start(
                id,
                Some("synthetic-nonexistent-device".into()),
                16_000,
                160,
                Box::new(move |frame| {
                    let _ = tx.send(frame);
                }),
                Box::new(|_| panic!("unexpected terminal")),
            )
            .unwrap();
        assert!(!result.fell_back_to_default);
        rx
    }

    #[test]
    fn clip_uses_real_downmix_resampler_then_continues_silence_and_stops() {
        let service = AudioCaptureService::new_benchmark(48_000, 2).unwrap();
        assert!(service.list_input_devices().unwrap().is_empty());
        let id = service.reserve_id().unwrap();
        let frames = start(&service, id);
        assert!(frames
            .recv_timeout(Duration::from_secs(1))
            .unwrap()
            .iter()
            .all(|x| *x == 0.0));
        let (tx, rx) = mpsc::channel();
        service
            .queue_benchmark_clip(
                id,
                [0.25, 0.75].repeat(4_800),
                Box::new(move |boundary| {
                    tx.send(boundary).unwrap();
                }),
            )
            .unwrap();
        let BenchmarkAudioBoundary::Started(first) =
            rx.recv_timeout(Duration::from_secs(1)).unwrap()
        else {
            panic!("start boundary")
        };
        let BenchmarkAudioBoundary::Completed(last) =
            rx.recv_timeout(Duration::from_secs(1)).unwrap()
        else {
            panic!("completion boundary")
        };
        assert!(last.duration_since(first) >= Duration::from_millis(50));
        let mut saw_clip = false;
        for _ in 0..100 {
            let frame = frames.recv_timeout(Duration::from_secs(1)).unwrap();
            saw_clip |= frame.contains(&0.5);
            if saw_clip && frame.iter().all(|x| *x == 0.0) {
                break;
            }
        }
        assert!(saw_clip);
        // More silence after the clip, rather than EOF.
        assert!(frames
            .recv_timeout(Duration::from_secs(1))
            .unwrap()
            .iter()
            .all(|x| *x == 0.0));
        assert!(service.stop(id).unwrap());
        while frames.try_recv().is_ok() {}
        assert!(matches!(
            frames.recv_timeout(Duration::from_millis(30)),
            Err(mpsc::RecvTimeoutError::Disconnected)
        ));
        assert!(!service.is_capturing().unwrap());
    }

    #[test]
    fn rejects_native_inactive_stale_invalid_and_busy_clips() {
        let native = AudioCaptureService::new();
        assert!(matches!(
            native.benchmark_capture_id(),
            Err(CaptureError::InputUnavailable(_))
        ));
        assert!(matches!(
            native.queue_benchmark_clip(1, vec![0.0], Box::new(|_| {})),
            Err(CaptureError::InputUnavailable(_))
        ));
        let service = AudioCaptureService::new_benchmark(16_000, 1).unwrap();
        assert_eq!(service.benchmark_capture_id(), Ok(None));
        assert_eq!(
            service.queue_benchmark_clip(1, vec![0.0], Box::new(|_| {})),
            Err(CaptureError::Superseded)
        );
        let frames = start(&service, 1);
        assert_eq!(service.benchmark_capture_id(), Ok(Some(1)));
        assert_eq!(
            service.queue_benchmark_clip(2, vec![0.0], Box::new(|_| {})),
            Err(CaptureError::Superseded)
        );
        assert_eq!(
            service.queue_benchmark_clip(1, vec![0.0; 960_001], Box::new(|_| {})),
            Err(CaptureError::ChannelOverflow)
        );
        assert_eq!(
            service.queue_benchmark_clip(1, vec![f32::NAN], Box::new(|_| {})),
            Err(CaptureError::InvalidSample)
        );
        assert_eq!(
            service.queue_benchmark_clip(1, vec![], Box::new(|_| {})),
            Err(CaptureError::InvalidFrameSize)
        );
        service
            .queue_benchmark_clip(1, vec![0.1; 16_000], Box::new(|_| {}))
            .unwrap();
        assert_eq!(
            service.queue_benchmark_clip(1, vec![0.0], Box::new(|_| {})),
            Err(CaptureError::ChannelOverflow)
        );
        service.stop(1).unwrap();
        assert_eq!(service.benchmark_capture_id(), Ok(None));
        assert_eq!(
            service.start(1, None, 16_000, 160, Box::new(|_| {}), Box::new(|_| {})),
            Err(CaptureError::Superseded)
        );
        drop(frames);
        let _frames = start(&service, 2);
        assert_eq!(service.benchmark_capture_id(), Ok(Some(2)));
        assert_eq!(
            service.queue_benchmark_clip(1, vec![0.0], Box::new(|_| {})),
            Err(CaptureError::Superseded)
        );
        service.shutdown();
        assert_eq!(service.benchmark_capture_id(), Ok(None));
        assert_eq!(
            service.queue_benchmark_clip(2, vec![0.0], Box::new(|_| {})),
            Err(CaptureError::ShuttingDown)
        );
    }

    #[test]
    fn producer_waits_for_activation_and_overflow_is_bounded() {
        let (samples_tx, samples_rx) = mpsc::sync_channel(1);
        let (control_tx, control_rx) = mpsc::channel();
        let (commands_tx, commands_rx) = mpsc::channel();
        let worker = thread::spawn(move || {
            produce(
                16_000,
                1,
                samples_tx,
                control_tx,
                commands_rx,
                Arc::new(Mutex::new(None)),
                Arc::new(AtomicBool::new(false)),
            )
        });
        assert!(matches!(
            samples_rx.recv_timeout(Duration::from_millis(30)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ));
        commands_tx.send(Command::Activate).unwrap();
        assert!(matches!(
            control_rx.recv_timeout(Duration::from_secs(1)),
            Ok(WorkerControl::Fail(CaptureError::ChannelOverflow))
        ));
        assert_eq!(samples_rx.try_iter().count(), 1);
        worker.join().unwrap();
    }

    #[test]
    fn unactivated_producer_is_joined_on_drop() {
        let service = AudioCaptureService::new_benchmark(16_000, 1).unwrap();
        let (active, _) = prepare(
            Arc::downgrade(&service.shared),
            1,
            16_000,
            1,
            16_000,
            160,
            Box::new(|_| panic!("output before activation")),
            Box::new(|_| {}),
        )
        .unwrap();
        active.stream.play().unwrap();
        stop_uninstalled_active(active, &service.shared).unwrap();
        assert_eq!(service.shared.state.lock().unwrap().teardown_count, 0);
    }
}
