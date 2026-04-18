use rustpotter::{Rustpotter, RustpotterConfig, WakewordRef, WakewordRefBuildFromFiles};
use std::path::Path;

const DETECTOR_SAMPLE_RATE_HZ: u64 = 16_000;
const DETECTOR_MFCC_SIZE: u16 = 16;
const WAKE_WORD_KEY: &str = "configured-wake-word";

pub struct WakeWordRuntime {
    inner: BufferedWakeWordRuntime<RustpotterDetector>,
}

impl WakeWordRuntime {
    pub fn new(wake_word_wav: &Path) -> Result<Self, String> {
        Ok(Self {
            inner: BufferedWakeWordRuntime::new(RustpotterDetector::new(wake_word_wav)?),
        })
    }

    pub fn process_sleeping_frame(&mut self, frame: &[f32]) -> Option<u64> {
        self.inner.process_sleeping_frame(frame)
    }

    pub fn reset(&mut self) {
        self.inner.reset();
    }
}

trait WakeWordDetector {
    fn samples_per_frame(&self) -> usize;
    fn process_samples(&mut self, samples: Vec<f32>) -> bool;
    fn reset(&mut self);
}

struct BufferedWakeWordRuntime<D> {
    detector: D,
    pending_samples: Vec<f32>,
    processed_samples: u64,
}

impl<D: WakeWordDetector> BufferedWakeWordRuntime<D> {
    fn new(detector: D) -> Self {
        Self {
            detector,
            pending_samples: Vec::new(),
            processed_samples: 0,
        }
    }

    fn process_sleeping_frame(&mut self, frame: &[f32]) -> Option<u64> {
        self.pending_samples.extend_from_slice(frame);

        let samples_per_frame = self.detector.samples_per_frame();
        let mut consumed_samples = 0;

        while self.pending_samples.len().saturating_sub(consumed_samples) >= samples_per_frame {
            let frame_end = consumed_samples.saturating_add(samples_per_frame);
            let detected = self
                .detector
                .process_samples(self.pending_samples[consumed_samples..frame_end].to_vec());

            self.processed_samples = self
                .processed_samples
                .saturating_add(samples_per_frame as u64);
            consumed_samples = frame_end;

            if detected {
                self.pending_samples.clear();
                self.detector.reset();
                return Some(samples_to_ms(self.processed_samples));
            }
        }

        if consumed_samples > 0 {
            self.pending_samples.drain(..consumed_samples);
        }

        None
    }

    fn reset(&mut self) {
        self.pending_samples.clear();
        self.detector.reset();
    }
}

struct RustpotterDetector {
    rustpotter: Rustpotter,
    samples_per_frame: usize,
}

impl RustpotterDetector {
    fn new(wake_word_wav: &Path) -> Result<Self, String> {
        let mut rustpotter = Rustpotter::new(&RustpotterConfig::default())?;
        let wakeword = WakewordRef::new_from_sample_files(
            WAKE_WORD_KEY.to_string(),
            None,
            None,
            vec![wake_word_wav.to_string_lossy().into_owned()],
            DETECTOR_MFCC_SIZE,
        )?;
        rustpotter.add_wakeword_ref(WAKE_WORD_KEY, wakeword)?;
        let samples_per_frame = rustpotter.get_samples_per_frame();

        Ok(Self {
            rustpotter,
            samples_per_frame,
        })
    }
}

impl WakeWordDetector for RustpotterDetector {
    fn samples_per_frame(&self) -> usize {
        self.samples_per_frame
    }

    fn process_samples(&mut self, samples: Vec<f32>) -> bool {
        self.rustpotter.process_samples(samples).is_some()
    }

    fn reset(&mut self) {
        self.rustpotter.reset();
    }
}

fn samples_to_ms(samples: u64) -> u64 {
    samples.saturating_mul(1_000) / DETECTOR_SAMPLE_RATE_HZ
}

#[cfg(test)]
mod tests {
    use super::{samples_to_ms, BufferedWakeWordRuntime, WakeWordDetector};

    struct FakeWakeWordDetector {
        samples_per_frame: usize,
        detect_on_call: Option<usize>,
        call_count: usize,
        reset_count: usize,
    }

    impl WakeWordDetector for FakeWakeWordDetector {
        fn samples_per_frame(&self) -> usize {
            self.samples_per_frame
        }

        fn process_samples(&mut self, _samples: Vec<f32>) -> bool {
            self.call_count += 1;
            self.detect_on_call == Some(self.call_count)
        }

        fn reset(&mut self) {
            self.call_count = 0;
            self.reset_count += 1;
        }
    }

    #[test]
    fn process_sleeping_frame_buffers_until_detector_frame_size() {
        let detector = FakeWakeWordDetector {
            samples_per_frame: 480,
            detect_on_call: None,
            call_count: 0,
            reset_count: 0,
        };
        let mut runtime = BufferedWakeWordRuntime::new(detector);

        assert_eq!(runtime.process_sleeping_frame(&vec![0.0; 1_600]), None);
        assert_eq!(runtime.detector.call_count, 3);
        assert_eq!(runtime.pending_samples.len(), 160);
        assert_eq!(runtime.processed_samples, 1_440);
    }

    #[test]
    fn process_sleeping_frame_reports_detection_time_and_resets_state() {
        let detector = FakeWakeWordDetector {
            samples_per_frame: 480,
            detect_on_call: Some(2),
            call_count: 0,
            reset_count: 0,
        };
        let mut runtime = BufferedWakeWordRuntime::new(detector);

        assert_eq!(runtime.process_sleeping_frame(&vec![0.0; 1_600]), Some(60));
        assert!(runtime.pending_samples.is_empty());
        assert_eq!(runtime.processed_samples, 960);
        assert_eq!(runtime.detector.call_count, 0);
        assert_eq!(runtime.detector.reset_count, 1);
    }

    #[test]
    fn reset_clears_pending_samples_without_rewinding_processed_time() {
        let detector = FakeWakeWordDetector {
            samples_per_frame: 480,
            detect_on_call: None,
            call_count: 0,
            reset_count: 0,
        };
        let mut runtime = BufferedWakeWordRuntime::new(detector);

        assert_eq!(runtime.process_sleeping_frame(&vec![0.0; 1_000]), None);
        runtime.reset();

        assert!(runtime.pending_samples.is_empty());
        assert_eq!(runtime.processed_samples, 960);
        assert_eq!(runtime.detector.reset_count, 1);
    }

    #[test]
    fn samples_to_ms_uses_detector_sample_rate() {
        assert_eq!(samples_to_ms(480), 30);
        assert_eq!(samples_to_ms(960), 60);
    }
}
