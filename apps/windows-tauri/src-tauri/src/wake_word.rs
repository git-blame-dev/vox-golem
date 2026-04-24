use crate::livekit_wakeword::WakeWordModel;
use std::path::Path;

const DETECTOR_SAMPLE_RATE_HZ: u64 = 16_000;
const DETECTOR_INPUT_SAMPLE_RATE_HZ: u32 = 16_000;
const DETECTOR_CHUNK_SAMPLES: usize = 1_280;
const DETECTOR_WINDOW_SAMPLES: usize = 32_000;
const DETECTION_THRESHOLD: f32 = 0.68;
const DETECTION_REQUIRED_CONSECUTIVE_HITS: usize = 1;

pub struct WakeWordRuntime {
    inner: BufferedWakeWordRuntime<LiveKitDetector<Box<dyn WakeWordScorer + Send>>>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WakeWordDetection {
    pub detected_at_ms: u64,
    pub confidence: f32,
}

impl WakeWordRuntime {
    pub fn new(wake_word_model_path: &Path) -> Result<Self, String> {
        Ok(Self {
            inner: BufferedWakeWordRuntime::new(LiveKitDetector::new(Box::new(
                LiveKitScorer::new(wake_word_model_path)?,
            ))),
        })
    }

    pub fn process_sleeping_frame(
        &mut self,
        frame: &[f32],
    ) -> Result<Option<WakeWordDetection>, String> {
        self.inner.process_sleeping_frame(frame)
    }

    pub fn reset(&mut self) {
        self.inner.reset();
    }

    #[cfg(test)]
    pub(crate) fn new_failing_for_test() -> Self {
        Self {
            inner: BufferedWakeWordRuntime::new(LiveKitDetector::with_settings(
                Box::new(AlwaysFailScorer),
                4,
                8,
                DETECTION_THRESHOLD,
                1,
            )),
        }
    }
}

trait WakeWordDetector {
    fn samples_per_frame(&self) -> usize;
    fn process_samples(&mut self, samples: &[f32]) -> Result<Option<f32>, String>;
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

    fn process_sleeping_frame(
        &mut self,
        frame: &[f32],
    ) -> Result<Option<WakeWordDetection>, String> {
        self.pending_samples.extend_from_slice(frame);

        let samples_per_frame = self.detector.samples_per_frame();
        let mut consumed_samples = 0;

        while self.pending_samples.len().saturating_sub(consumed_samples) >= samples_per_frame {
            let frame_end = consumed_samples.saturating_add(samples_per_frame);
            let detection_score = match self
                .detector
                .process_samples(&self.pending_samples[consumed_samples..frame_end])
            {
                Ok(detection_score) => detection_score,
                Err(error) => {
                    self.pending_samples.drain(..frame_end);
                    self.detector.reset();
                    return Err(error);
                }
            };

            self.processed_samples = self
                .processed_samples
                .saturating_add(samples_per_frame as u64);
            consumed_samples = frame_end;

            if let Some(confidence) = detection_score {
                self.pending_samples.clear();
                self.detector.reset();
                return Ok(Some(WakeWordDetection {
                    detected_at_ms: samples_to_ms(self.processed_samples),
                    confidence,
                }));
            }
        }

        if consumed_samples > 0 {
            self.pending_samples.drain(..consumed_samples);
        }

        Ok(None)
    }

    fn reset(&mut self) {
        self.pending_samples.clear();
        self.detector.reset();
    }
}

trait WakeWordScorer: Send {
    fn score(&mut self, audio_chunk: &[i16]) -> Result<f32, String>;
}

impl WakeWordScorer for Box<dyn WakeWordScorer + Send> {
    fn score(&mut self, audio_chunk: &[i16]) -> Result<f32, String> {
        self.as_mut().score(audio_chunk)
    }
}

struct LiveKitScorer {
    model: WakeWordModel,
}

#[cfg(test)]
struct AlwaysFailScorer;

impl LiveKitScorer {
    fn new(wake_word_model_path: &Path) -> Result<Self, String> {
        let model = WakeWordModel::new(&[wake_word_model_path], DETECTOR_INPUT_SAMPLE_RATE_HZ)
            .map_err(|error| format!("failed to load wake word model: {error}"))?;

        Ok(Self { model })
    }
}

impl WakeWordScorer for LiveKitScorer {
    fn score(&mut self, audio_chunk: &[i16]) -> Result<f32, String> {
        let predictions = self
            .model
            .predict(audio_chunk)
            .map_err(|error| format!("wake word prediction failed: {error}"))?;

        Ok(predictions.values().copied().fold(0.0, f32::max))
    }
}

#[cfg(test)]
impl WakeWordScorer for AlwaysFailScorer {
    fn score(&mut self, _audio_chunk: &[i16]) -> Result<f32, String> {
        Err(String::from("synthetic wake word scorer failure"))
    }
}

struct LiveKitDetector<S> {
    scorer: S,
    rolling_samples: Vec<i16>,
    samples_per_frame: usize,
    window_samples: usize,
    detection_threshold: f32,
    required_consecutive_hits: usize,
    consecutive_hits: usize,
    consecutive_floor_score: Option<f32>,
}

impl<S: WakeWordScorer> LiveKitDetector<S> {
    fn new(scorer: S) -> Self {
        Self::with_settings(
            scorer,
            DETECTOR_CHUNK_SAMPLES,
            DETECTOR_WINDOW_SAMPLES,
            DETECTION_THRESHOLD,
            DETECTION_REQUIRED_CONSECUTIVE_HITS,
        )
    }

    fn with_settings(
        scorer: S,
        samples_per_frame: usize,
        window_samples: usize,
        detection_threshold: f32,
        required_consecutive_hits: usize,
    ) -> Self {
        Self {
            scorer,
            rolling_samples: Vec::with_capacity(window_samples),
            samples_per_frame,
            window_samples,
            detection_threshold,
            required_consecutive_hits,
            consecutive_hits: 0,
            consecutive_floor_score: None,
        }
    }
}

impl<S: WakeWordScorer> WakeWordDetector for LiveKitDetector<S> {
    fn samples_per_frame(&self) -> usize {
        self.samples_per_frame
    }

    fn process_samples(&mut self, samples: &[f32]) -> Result<Option<f32>, String> {
        self.rolling_samples
            .extend(samples.iter().copied().map(normalize_sample_to_i16));

        if self.rolling_samples.len() > self.window_samples {
            let overflow = self.rolling_samples.len() - self.window_samples;
            self.rolling_samples.drain(..overflow);
        }

        if self.rolling_samples.len() < self.window_samples {
            return Ok(None);
        }

        let score = self.scorer.score(&self.rolling_samples)?;
        if score >= self.detection_threshold {
            self.consecutive_floor_score = Some(
                self.consecutive_floor_score
                    .map_or(score, |current_floor| current_floor.min(score)),
            );
            self.consecutive_hits = self.consecutive_hits.saturating_add(1);
            if self.consecutive_hits >= self.required_consecutive_hits {
                return Ok(Some(self.consecutive_floor_score.unwrap_or(score)));
            }

            return Ok(None);
        }

        self.consecutive_hits = 0;
        self.consecutive_floor_score = None;
        Ok(None)
    }

    fn reset(&mut self) {
        self.rolling_samples.clear();
        self.consecutive_hits = 0;
        self.consecutive_floor_score = None;
    }
}

fn normalize_sample_to_i16(sample: f32) -> i16 {
    if sample >= 1.0 {
        i16::MAX
    } else if sample <= -1.0 {
        i16::MIN
    } else {
        (sample * 32_768.0) as i16
    }
}

fn samples_to_ms(samples: u64) -> u64 {
    samples.saturating_mul(1_000) / DETECTOR_SAMPLE_RATE_HZ
}

#[cfg(test)]
mod tests {
    use super::{
        normalize_sample_to_i16, samples_to_ms, BufferedWakeWordRuntime, LiveKitDetector,
        WakeWordDetection, WakeWordDetector, WakeWordRuntime, WakeWordScorer, DETECTION_THRESHOLD,
    };
    use hound::WavReader;
    use std::path::{Path, PathBuf};

    struct FakeWakeWordDetector {
        samples_per_frame: usize,
        detect_on_call: Option<usize>,
        error_on_call: Option<usize>,
        call_count: usize,
        reset_count: usize,
    }

    impl WakeWordDetector for FakeWakeWordDetector {
        fn samples_per_frame(&self) -> usize {
            self.samples_per_frame
        }

        fn process_samples(&mut self, _samples: &[f32]) -> Result<Option<f32>, String> {
            self.call_count += 1;
            if self.error_on_call == Some(self.call_count) {
                return Err(String::from("synthetic detector failure"));
            }
            Ok((self.detect_on_call == Some(self.call_count)).then_some(0.91))
        }

        fn reset(&mut self) {
            self.call_count = 0;
            self.reset_count += 1;
        }
    }

    struct FakeScorer {
        scores: Vec<f32>,
        seen_chunks: Vec<Vec<i16>>,
    }

    struct OneShotErrorDetector {
        samples_per_frame: usize,
        call_count: usize,
        reset_count: usize,
        errored: bool,
    }

    impl WakeWordScorer for FakeScorer {
        fn score(&mut self, audio_chunk: &[i16]) -> Result<f32, String> {
            self.seen_chunks.push(audio_chunk.to_vec());
            Ok(self.scores.remove(0))
        }
    }

    impl WakeWordDetector for OneShotErrorDetector {
        fn samples_per_frame(&self) -> usize {
            self.samples_per_frame
        }

        fn process_samples(&mut self, _samples: &[f32]) -> Result<Option<f32>, String> {
            self.call_count += 1;
            if !self.errored && self.call_count == 2 {
                self.errored = true;
                return Err(String::from("synthetic detector failure"));
            }

            Ok(None)
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
            error_on_call: None,
            call_count: 0,
            reset_count: 0,
        };
        let mut runtime = BufferedWakeWordRuntime::new(detector);

        assert_eq!(runtime.process_sleeping_frame(&vec![0.0; 1_600]), Ok(None));
        assert_eq!(runtime.detector.call_count, 3);
        assert_eq!(runtime.pending_samples.len(), 160);
        assert_eq!(runtime.processed_samples, 1_440);
    }

    #[test]
    fn process_sleeping_frame_reports_detection_time_and_resets_state() {
        let detector = FakeWakeWordDetector {
            samples_per_frame: 480,
            detect_on_call: Some(2),
            error_on_call: None,
            call_count: 0,
            reset_count: 0,
        };
        let mut runtime = BufferedWakeWordRuntime::new(detector);

        assert_eq!(
            runtime.process_sleeping_frame(&vec![0.0; 1_600]),
            Ok(Some(WakeWordDetection {
                detected_at_ms: 60,
                confidence: 0.91,
            }))
        );
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
            error_on_call: None,
            call_count: 0,
            reset_count: 0,
        };
        let mut runtime = BufferedWakeWordRuntime::new(detector);

        assert_eq!(runtime.process_sleeping_frame(&vec![0.0; 1_000]), Ok(None));
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

    #[test]
    fn livekit_detector_waits_for_full_window_before_scoring() {
        let scorer = FakeScorer {
            scores: vec![0.8],
            seen_chunks: Vec::new(),
        };
        let mut detector = LiveKitDetector::with_settings(scorer, 4, 8, 0.5, 1);

        assert_eq!(detector.process_samples(&[0.1, 0.2, 0.3, 0.4]), Ok(None));
        assert!(detector.scorer.seen_chunks.is_empty());

        assert_eq!(
            detector.process_samples(&[0.5, 0.6, 0.7, 0.8]),
            Ok(Some(0.8))
        );
        assert_eq!(detector.scorer.seen_chunks.len(), 1);
        assert_eq!(detector.scorer.seen_chunks[0].len(), 8);
    }

    #[test]
    fn livekit_detector_keeps_only_most_recent_window() {
        let scorer = FakeScorer {
            scores: vec![0.2, 0.2],
            seen_chunks: Vec::new(),
        };
        let mut detector = LiveKitDetector::with_settings(scorer, 4, 8, 0.5, 1);

        assert_eq!(detector.process_samples(&[0.0, 0.1, 0.2, 0.3]), Ok(None));
        assert_eq!(detector.process_samples(&[0.4, 0.5, 0.6, 0.7]), Ok(None));
        assert_eq!(detector.process_samples(&[0.8, 0.9, 1.0, -1.0]), Ok(None));

        assert_eq!(detector.scorer.seen_chunks.len(), 2);
        assert_eq!(
            detector.scorer.seen_chunks[1],
            vec![
                normalize_sample_to_i16(0.4),
                normalize_sample_to_i16(0.5),
                normalize_sample_to_i16(0.6),
                normalize_sample_to_i16(0.7),
                normalize_sample_to_i16(0.8),
                normalize_sample_to_i16(0.9),
                i16::MAX,
                i16::MIN,
            ]
        );
    }

    #[test]
    fn normalize_sample_to_i16_clamps_endpoints() {
        assert_eq!(normalize_sample_to_i16(1.0), i16::MAX);
        assert_eq!(normalize_sample_to_i16(-1.0), i16::MIN);
        assert_eq!(normalize_sample_to_i16(0.0), 0);
    }

    #[test]
    fn process_sleeping_frame_propagates_detector_errors() {
        let mut runtime = WakeWordRuntime::new_failing_for_test();

        assert_eq!(
            runtime.process_sleeping_frame(&[0.0; 8]),
            Err(String::from("synthetic wake word scorer failure"))
        );
    }

    #[test]
    fn process_sleeping_frame_does_not_replay_consumed_chunks_after_error() {
        let detector = OneShotErrorDetector {
            samples_per_frame: 480,
            call_count: 0,
            reset_count: 0,
            errored: false,
        };
        let mut runtime = BufferedWakeWordRuntime::new(detector);

        assert_eq!(
            runtime.process_sleeping_frame(&vec![0.0; 1_600]),
            Err(String::from("synthetic detector failure"))
        );
        assert_eq!(runtime.pending_samples.len(), 640);
        assert_eq!(runtime.detector.reset_count, 1);

        assert_eq!(runtime.process_sleeping_frame(&vec![0.0; 320]), Ok(None));
        assert_eq!(runtime.detector.call_count, 2);
    }

    #[test]
    fn real_runtime_detects_positive_fixture_with_framed_audio() {
        let mut runtime = WakeWordRuntime::new(&fixtures_dir().join("hey_livekit.onnx"))
            .expect("official livekit classifier should load");
        let mut samples = read_wav_f32(&fixtures_dir().join("positive.wav"));
        samples.extend(vec![0.0; 1_440]);

        let mut detected = None;
        for chunk in samples.chunks(480) {
            detected = runtime
                .process_sleeping_frame(chunk)
                .expect("positive fixture should score successfully");
            if detected.is_some() {
                break;
            }
        }

        assert!(detected.is_some());
        assert!(
            detected.map(|result| result.confidence).unwrap_or_default() >= DETECTION_THRESHOLD
        );
    }

    #[test]
    fn real_runtime_ignores_negative_fixture_with_framed_audio() {
        let mut runtime = WakeWordRuntime::new(&fixtures_dir().join("hey_livekit.onnx"))
            .expect("official livekit classifier should load");
        let mut samples = read_wav_f32(&fixtures_dir().join("negative.wav"));
        samples.extend(vec![0.0; 1_440]);

        let mut detected = None;
        for chunk in samples.chunks(480) {
            detected = runtime
                .process_sleeping_frame(chunk)
                .expect("negative fixture should score successfully");
            if detected.is_some() {
                break;
            }
        }

        assert_eq!(detected, None);
    }

    #[test]
    fn behavior_detector_requires_three_consecutive_hits() {
        let scorer = FakeScorer {
            scores: vec![0.7, 0.4, 0.72, 0.75, 0.8],
            seen_chunks: Vec::new(),
        };
        let mut detector = LiveKitDetector::with_settings(scorer, 4, 8, 0.62, 3);

        assert_eq!(detector.process_samples(&[0.1, 0.2, 0.3, 0.4]), Ok(None));
        assert_eq!(detector.process_samples(&[0.5, 0.6, 0.7, 0.8]), Ok(None));
        assert_eq!(detector.process_samples(&[0.1, 0.2, 0.3, 0.4]), Ok(None));
        assert_eq!(detector.process_samples(&[0.5, 0.6, 0.7, 0.8]), Ok(None));
        assert_eq!(detector.process_samples(&[0.1, 0.2, 0.3, 0.4]), Ok(None));
        assert_eq!(
            detector.process_samples(&[0.5, 0.6, 0.7, 0.8]),
            Ok(Some(0.72))
        );
    }

    #[test]
    fn behavior_detector_reports_floor_score_across_triggering_streak() {
        let scorer = FakeScorer {
            scores: vec![0.95, 0.7, 0.99],
            seen_chunks: Vec::new(),
        };
        let mut detector = LiveKitDetector::with_settings(scorer, 4, 8, 0.62, 3);

        assert_eq!(detector.process_samples(&[0.1, 0.2, 0.3, 0.4]), Ok(None));
        assert_eq!(detector.process_samples(&[0.5, 0.6, 0.7, 0.8]), Ok(None));
        assert_eq!(detector.process_samples(&[0.1, 0.2, 0.3, 0.4]), Ok(None));
        assert_eq!(
            detector.process_samples(&[0.5, 0.6, 0.7, 0.8]),
            Ok(Some(0.7))
        );
    }

    fn fixtures_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
    }

    fn read_wav_f32(path: &Path) -> Vec<f32> {
        let mut reader = WavReader::open(path)
            .unwrap_or_else(|error| panic!("open {}: {error}", path.display()));
        reader
            .samples::<i16>()
            .map(|sample| sample.expect("wav sample") as f32 / 32_768.0)
            .collect()
    }
}
