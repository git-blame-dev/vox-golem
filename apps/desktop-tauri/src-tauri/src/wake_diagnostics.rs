use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::time::Instant;

const SCHEMA_VERSION: u8 = 1;
const HISTORY_MS: u64 = 30_000;
const MAX_EVENTS: usize = 512;
const MODEL_NAME_MAX_BYTES: usize = 128;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WakeDiagnosticMarker {
    WakeAttempt,
    FalseWake,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WakeDiagnosticPhase {
    Sleeping,
    Listening,
    Processing,
    Executing,
    Initializing,
    Error,
    Stopped,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct WakeDiagnosticsSnapshot {
    pub schema_version: u8,
    pub enabled: bool,
    pub model: String,
    pub feature_profile: String,
    pub threshold: f32,
    pub sample_rate_hz: u32,
    pub window_samples: usize,
    pub hop_samples: usize,
    pub warmup_remaining_samples: usize,
    pub phase: WakeDiagnosticPhase,
    pub dropped_events: u64,
    pub events: Vec<WakeDiagnosticEvent>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WakeDiagnosticEvent {
    Score {
        sequence: u64,
        at_ms: u64,
        score: f32,
        sample_position: u64,
        rms_dbfs: Option<f32>,
        peak: f32,
        clipped_fraction: f32,
        inference_ms: f64,
    },
    Phase {
        sequence: u64,
        at_ms: u64,
        phase: WakeDiagnosticPhase,
    },
    Reset {
        sequence: u64,
        at_ms: u64,
        reason: String,
    },
    Marker {
        sequence: u64,
        at_ms: u64,
        marker: WakeDiagnosticMarker,
    },
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct InferenceDiagnostics {
    pub sample_position: u64,
    pub rms_dbfs: Option<f32>,
    pub peak: f32,
    pub clipped_fraction: f32,
    pub inference_ms: f64,
}

#[derive(Debug, Clone)]
pub(crate) struct WakeDiagnostics {
    model: String,
    threshold: f32,
    sample_rate_hz: u32,
    window_samples: usize,
    hop_samples: usize,
    enabled: bool,
    phase: WakeDiagnosticPhase,
    events: VecDeque<WakeDiagnosticEvent>,
    next_sequence: u64,
    dropped_events: u64,
    started_at: Instant,
}

impl WakeDiagnostics {
    pub(crate) fn new(
        model: &str,
        threshold: f32,
        sample_rate_hz: u32,
        window_samples: usize,
        hop_samples: usize,
    ) -> Self {
        Self {
            model: bounded_model_name(model),
            threshold,
            sample_rate_hz,
            window_samples,
            hop_samples,
            enabled: false,
            phase: WakeDiagnosticPhase::Sleeping,
            events: VecDeque::new(),
            next_sequence: 0,
            dropped_events: 0,
            started_at: Instant::now(),
        }
    }

    pub(crate) fn now_ms(&self) -> u64 {
        self.started_at
            .elapsed()
            .as_millis()
            .min(u128::from(u64::MAX)) as u64
    }

    pub(crate) fn set_enabled(&mut self, enabled: bool, at_ms: u64) {
        if self.enabled && !enabled {
            self.expire_before(at_ms);
        }
        if enabled && !self.enabled {
            self.events.clear();
            self.next_sequence = 0;
            self.dropped_events = 0;
        }
        self.enabled = enabled;
    }

    pub(crate) fn record_score(&mut self, at_ms: u64, score: f32, metrics: InferenceDiagnostics) {
        if !self.enabled {
            return;
        }
        self.push(at_ms, |sequence, at_ms| WakeDiagnosticEvent::Score {
            sequence,
            at_ms,
            score,
            sample_position: metrics.sample_position,
            rms_dbfs: metrics.rms_dbfs,
            peak: metrics.peak,
            clipped_fraction: metrics.clipped_fraction,
            inference_ms: metrics.inference_ms,
        });
    }

    pub(crate) fn record_reset(&mut self, at_ms: u64, reason: &'static str) {
        if self.enabled {
            self.push(at_ms, |sequence, at_ms| WakeDiagnosticEvent::Reset {
                sequence,
                at_ms,
                reason: reason.to_owned(),
            });
        }
    }

    pub(crate) fn record_phase(&mut self, phase: WakeDiagnosticPhase, at_ms: u64) {
        let changed = self.phase != phase;
        self.phase = phase;
        if self.enabled && changed {
            self.push(at_ms, |sequence, at_ms| WakeDiagnosticEvent::Phase {
                sequence,
                at_ms,
                phase,
            });
        }
    }

    pub(crate) fn mark(
        &mut self,
        marker: WakeDiagnosticMarker,
        at_ms: u64,
    ) -> WakeDiagnosticsSnapshot {
        if self.enabled {
            self.push(at_ms, |sequence, at_ms| WakeDiagnosticEvent::Marker {
                sequence,
                at_ms,
                marker,
            });
        }
        self.snapshot(0, at_ms)
    }

    pub(crate) fn snapshot(
        &self,
        warmup_remaining_samples: usize,
        at_ms: u64,
    ) -> WakeDiagnosticsSnapshot {
        let expired = if self.enabled {
            self.events
                .iter()
                .take_while(|event| at_ms.saturating_sub(event.at_ms()) > HISTORY_MS)
                .count() as u64
        } else {
            0
        };
        WakeDiagnosticsSnapshot {
            schema_version: SCHEMA_VERSION,
            enabled: self.enabled,
            model: self.model.clone(),
            feature_profile: String::from("normalized_f32"),
            threshold: self.threshold,
            sample_rate_hz: self.sample_rate_hz,
            window_samples: self.window_samples,
            hop_samples: self.hop_samples,
            warmup_remaining_samples,
            phase: self.phase,
            dropped_events: self.dropped_events.saturating_add(expired),
            events: self
                .events
                .iter()
                .filter(|event| !self.enabled || at_ms.saturating_sub(event.at_ms()) <= HISTORY_MS)
                .cloned()
                .collect(),
        }
    }

    fn push(&mut self, at_ms: u64, build: impl FnOnce(u64, u64) -> WakeDiagnosticEvent) {
        let sequence = self.next_sequence;
        self.next_sequence = self.next_sequence.saturating_add(1);
        self.events.push_back(build(sequence, at_ms));
        self.expire_before(at_ms);
        while self.events.len() > MAX_EVENTS {
            self.events.pop_front();
            self.dropped_events = self.dropped_events.saturating_add(1);
        }
    }

    fn expire_before(&mut self, at_ms: u64) {
        while self
            .events
            .front()
            .is_some_and(|event| at_ms.saturating_sub(event.at_ms()) > HISTORY_MS)
        {
            self.events.pop_front();
            self.dropped_events = self.dropped_events.saturating_add(1);
        }
    }
}

impl WakeDiagnosticEvent {
    fn at_ms(&self) -> u64 {
        match self {
            Self::Score { at_ms, .. }
            | Self::Phase { at_ms, .. }
            | Self::Reset { at_ms, .. }
            | Self::Marker { at_ms, .. } => *at_ms,
        }
    }
}

fn bounded_model_name(model: &str) -> String {
    let basename = model.rsplit(['/', '\\']).next().unwrap_or_default();
    let stem = basename.rsplit_once('.').map_or(basename, |(stem, _)| stem);
    stem.chars()
        .scan(0, |bytes, character| {
            let next = *bytes + character.len_utf8();
            if next > MODEL_NAME_MAX_BYTES {
                None
            } else {
                *bytes = next;
                Some(character)
            }
        })
        .collect()
}

pub(crate) fn signal_metrics(samples: &[f32]) -> (Option<f32>, f32, f32) {
    if samples.is_empty() {
        return (None, 0.0, 0.0);
    }
    let mut sum_squares = 0.0_f64;
    let mut peak = 0.0_f32;
    let mut clipped = 0_usize;
    for &sample in samples {
        let magnitude = sample.abs();
        peak = peak.max(magnitude);
        if magnitude >= 1.0 {
            clipped += 1;
        }
        sum_squares += f64::from(sample) * f64::from(sample);
    }
    let rms = (sum_squares / samples.len() as f64).sqrt();
    let rms_dbfs = (rms > 0.0).then(|| (20.0 * rms.log10()) as f32);
    (rms_dbfs, peak, clipped as f32 / samples.len() as f32)
}

#[cfg(test)]
mod tests {
    use super::{
        signal_metrics, WakeDiagnosticEvent, WakeDiagnosticMarker, WakeDiagnosticPhase,
        WakeDiagnostics,
    };

    fn diagnostics() -> WakeDiagnostics {
        WakeDiagnostics::new(
            "/private/models/hey_livekit.onnx",
            0.68,
            16_000,
            32_000,
            1_280,
        )
    }

    #[test]
    fn disabled_diagnostics_record_nothing_and_enable_clears_history() {
        let mut diagnostics = diagnostics();
        diagnostics.record_phase(WakeDiagnosticPhase::Listening, 0);
        assert!(diagnostics.snapshot(0, 0).events.is_empty());
        diagnostics.set_enabled(true, 1);
        diagnostics.record_phase(WakeDiagnosticPhase::Listening, 10);
        assert_eq!(diagnostics.snapshot(0, 10).events.len(), 0);
        diagnostics.record_phase(WakeDiagnosticPhase::Sleeping, 11);
        assert_eq!(diagnostics.snapshot(0, 11).events.len(), 1);
        diagnostics.set_enabled(false, 12);
        diagnostics.set_enabled(true, 13);
        assert!(diagnostics.snapshot(0, 13).events.is_empty());
    }

    #[test]
    fn history_is_bounded_by_time_and_event_count() {
        let mut diagnostics = diagnostics();
        diagnostics.set_enabled(true, 0);
        for index in 0..600_u64 {
            let phase = if index % 2 == 0 {
                WakeDiagnosticPhase::Listening
            } else {
                WakeDiagnosticPhase::Sleeping
            };
            diagnostics.record_phase(phase, 0);
        }
        let snapshot = diagnostics.snapshot(0, 0);
        assert_eq!(snapshot.events.len(), 512);
        assert_eq!(snapshot.dropped_events, 88);
        assert!(matches!(
            snapshot.events.first(),
            Some(WakeDiagnosticEvent::Phase { sequence: 88, .. })
        ));
        let snapshot = diagnostics.snapshot(0, 30_000);
        assert_eq!(snapshot.events.len(), 512);
        assert_eq!(snapshot.dropped_events, 88);
    }

    #[test]
    fn time_expiry_is_independent_of_the_event_count_limit() {
        let mut diagnostics = diagnostics();
        diagnostics.set_enabled(true, 0);
        diagnostics.record_phase(WakeDiagnosticPhase::Listening, 0);
        diagnostics.record_phase(WakeDiagnosticPhase::Sleeping, 30_000);
        let boundary = diagnostics.snapshot(0, 30_000);
        assert_eq!(boundary.events.len(), 2);
        let expired = diagnostics.snapshot(0, 30_001);
        assert_eq!(expired.events.len(), 1);
        assert_eq!(expired.dropped_events, 1);
    }

    #[test]
    fn markers_return_independent_metadata_only_snapshots_and_wire_schema() {
        let mut diagnostics = diagnostics();
        diagnostics.set_enabled(true, 0);
        let frozen = diagnostics.mark(WakeDiagnosticMarker::WakeAttempt, 40);
        diagnostics.record_phase(WakeDiagnosticPhase::Processing, 80);
        assert_eq!(frozen.events.len(), 1);
        assert_eq!(frozen.model, "hey_livekit");
        let json = serde_json::to_value(&frozen).unwrap();
        assert_eq!(json["schema_version"], 1);
        assert_eq!(json["feature_profile"], "normalized_f32");
        assert_eq!(json["events"][0]["kind"], "marker");
        assert_eq!(json["events"][0]["marker"], "wake_attempt");
        assert_eq!(
            serde_json::from_value::<WakeDiagnosticMarker>(json["events"][0]["marker"].clone())
                .unwrap(),
            WakeDiagnosticMarker::WakeAttempt
        );
        assert!(json.get("audio").is_none());
        assert!(json.to_string().find("/private").is_none());
    }

    #[test]
    fn signal_metrics_handle_silence_and_clipped_input() {
        assert_eq!(signal_metrics(&[0.0; 4]), (None, 0.0, 0.0));
        let (rms, peak, clipped) = signal_metrics(&[0.0, -0.5, 1.2, -1.0]);
        assert!((rms.unwrap() - -1.724).abs() < 0.01);
        assert_eq!(peak, 1.2);
        assert_eq!(clipped, 0.5);
    }
}
