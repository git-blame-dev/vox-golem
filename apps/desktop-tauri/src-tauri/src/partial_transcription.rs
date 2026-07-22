//! A small, runtime-independent scheduler for live transcription snapshots.

use std::time::Duration;

pub type VoiceSessionId = u64;

#[derive(Clone, Debug, PartialEq)]
pub enum PartialTranscriptionAction {
    StartSnapshot {
        session_id: VoiceSessionId,
        revision: u64,
        samples: Vec<f32>,
    },
    PublishText {
        session_id: VoiceSessionId,
        revision: u64,
        text: String,
    },
    Ignore,
}

#[derive(Clone, Copy, Debug)]
pub struct PartialTranscriptionConfig {
    pub minimum_samples: usize,
    pub throttle: Duration,
    pub maximum_copied_samples: usize,
}

#[derive(Clone, Debug)]
struct Snapshot {
    session_id: VoiceSessionId,
    revision: u64,
    samples: Vec<f32>,
}

pub struct PartialTranscriptionScheduler {
    config: PartialTranscriptionConfig,
    session_id: Option<VoiceSessionId>,
    next_revision: u64,
    in_flight: Option<Snapshot>,
    pending: Option<Snapshot>,
    pending_updated_ms: Option<u64>,
    last_started_ms: Option<u64>,
}

impl PartialTranscriptionScheduler {
    pub fn new(config: PartialTranscriptionConfig) -> Self {
        assert!(config.maximum_copied_samples > 0);
        Self {
            config,
            session_id: None,
            next_revision: 0,
            in_flight: None,
            pending: None,
            pending_updated_ms: None,
            last_started_ms: None,
        }
    }

    pub fn start_session(&mut self, session_id: VoiceSessionId) {
        self.session_id = Some(session_id);
        self.pending = None;
        self.pending_updated_ms = None;
        self.last_started_ms = None;
    }

    pub fn reset(&mut self) {
        self.session_id = None;
        self.pending = None;
        self.pending_updated_ms = None;
    }

    pub fn finalize(&mut self) {
        self.reset();
    }

    pub fn request_snapshot(&mut self, now_ms: u64, samples: &[f32]) -> PartialTranscriptionAction {
        let Some(session_id) = self.session_id else {
            return PartialTranscriptionAction::Ignore;
        };
        if samples.len() < self.config.minimum_samples {
            return PartialTranscriptionAction::Ignore;
        }
        if self.pending.is_some() {
            let copy_throttle_started = if self.in_flight.is_some() {
                self.pending_updated_ms
            } else {
                self.last_started_ms
            };
            if copy_throttle_started.is_some_and(|started| {
                now_ms.saturating_sub(started) < self.config.throttle.as_millis() as u64
            }) {
                return PartialTranscriptionAction::Ignore;
            }
        }
        let snapshot = Snapshot {
            session_id,
            revision: self.next_revision.saturating_add(1),
            samples: samples[samples
                .len()
                .saturating_sub(self.config.maximum_copied_samples)..]
                .to_vec(),
        };
        self.next_revision = snapshot.revision;
        if self.in_flight.is_some() {
            self.pending = Some(snapshot);
            self.pending_updated_ms = Some(now_ms);
            return PartialTranscriptionAction::Ignore;
        }
        if self.last_started_ms.is_some_and(|started| {
            now_ms.saturating_sub(started) < self.config.throttle.as_millis() as u64
        }) {
            // Once the older request has completed, a post-throttle snapshot is
            // authoritative; it must not be followed by the stale pending one.
            self.pending = Some(snapshot);
            self.pending_updated_ms = Some(now_ms);
            return PartialTranscriptionAction::Ignore;
        }
        self.pending = None;
        self.pending_updated_ms = None;
        self.start(snapshot, now_ms)
    }

    pub fn complete(
        &mut self,
        now_ms: u64,
        session_id: VoiceSessionId,
        revision: u64,
        text: String,
    ) -> PartialTranscriptionAction {
        let Some(active) = self.in_flight.as_ref() else {
            return PartialTranscriptionAction::Ignore;
        };
        if active.session_id != session_id || active.revision != revision {
            return PartialTranscriptionAction::Ignore;
        }
        self.in_flight.take();
        if self.session_id != Some(session_id) {
            if self.pending.is_some()
                && now_ms.saturating_sub(self.last_started_ms.unwrap_or(0))
                    >= self.config.throttle.as_millis() as u64
            {
                let pending = self.pending.take().expect("pending snapshot exists");
                self.pending_updated_ms = None;
                return self.start(pending, now_ms);
            }
            return PartialTranscriptionAction::Ignore;
        }
        let publish = PartialTranscriptionAction::PublishText {
            session_id,
            revision,
            text,
        };
        if let Some(pending) = self.pending.as_ref() {
            if now_ms.saturating_sub(self.last_started_ms.unwrap_or(0))
                >= self.config.throttle.as_millis() as u64
            {
                let pending = pending.clone();
                self.pending.take();
                self.pending_updated_ms = None;
                let action = self.start(pending, now_ms);
                return match action {
                    PartialTranscriptionAction::StartSnapshot { .. } => action,
                    _ => publish,
                };
            }
            // A newer snapshot is pending; never publish the completed older revision.
            return PartialTranscriptionAction::Ignore;
        }
        publish
    }

    fn start(&mut self, snapshot: Snapshot, now_ms: u64) -> PartialTranscriptionAction {
        let action = PartialTranscriptionAction::StartSnapshot {
            session_id: snapshot.session_id,
            revision: snapshot.revision,
            samples: snapshot.samples.clone(),
        };
        self.in_flight = Some(snapshot);
        self.pending_updated_ms = None;
        self.last_started_ms = Some(now_ms);
        action
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scheduler() -> PartialTranscriptionScheduler {
        PartialTranscriptionScheduler::new(PartialTranscriptionConfig {
            minimum_samples: 3,
            throttle: Duration::from_millis(100),
            maximum_copied_samples: 4,
        })
    }

    #[test]
    fn threshold_throttle_and_bounded_copy() {
        let mut s = scheduler();
        s.start_session(7);
        assert_eq!(
            s.request_snapshot(0, &[1.0, 2.0]),
            PartialTranscriptionAction::Ignore
        );
        assert_eq!(
            s.request_snapshot(0, &[1., 2., 3., 4., 5.]),
            PartialTranscriptionAction::StartSnapshot {
                session_id: 7,
                revision: 1,
                samples: vec![2., 3., 4., 5.]
            }
        );
        assert_eq!(
            s.request_snapshot(50, &[9., 8., 7.]),
            PartialTranscriptionAction::Ignore
        );
    }

    #[test]
    fn busy_requests_coalesce_to_latest() {
        let mut s = scheduler();
        s.start_session(1);
        let first = s.request_snapshot(0, &[1., 2., 3.]);
        assert_eq!(
            s.request_snapshot(1, &[4., 5., 6.]),
            PartialTranscriptionAction::Ignore
        );
        assert_eq!(
            s.complete(100, 1, 1, String::from("old")),
            PartialTranscriptionAction::StartSnapshot {
                session_id: 1,
                revision: 2,
                samples: vec![4., 5., 6.]
            }
        );
        assert!(matches!(
            first,
            PartialTranscriptionAction::StartSnapshot { .. }
        ));
    }

    #[test]
    fn completion_before_throttle_retains_pending_snapshot() {
        let mut s = scheduler();
        s.start_session(1);
        let _ = s.request_snapshot(0, &[1., 2., 3.]);
        let _ = s.request_snapshot(10, &[4., 5., 6.]);

        assert_eq!(
            s.complete(50, 1, 1, String::from("old")),
            PartialTranscriptionAction::Ignore
        );
        assert_eq!(
            s.pending.as_ref().map(|snapshot| snapshot.revision),
            Some(2)
        );
    }

    #[test]
    fn post_throttle_snapshot_supersedes_completed_pending_revision() {
        let mut s = scheduler();
        s.start_session(1);
        assert!(matches!(
            s.request_snapshot(0, &[1., 2., 3.]),
            PartialTranscriptionAction::StartSnapshot { revision: 1, .. }
        ));
        let _ = s.request_snapshot(10, &[4., 5., 6.]);
        assert_eq!(
            s.complete(50, 1, 1, "old".into()),
            PartialTranscriptionAction::Ignore
        );
        assert_eq!(
            s.request_snapshot(100, &[7., 8., 9.]),
            PartialTranscriptionAction::StartSnapshot {
                session_id: 1,
                revision: 3,
                samples: vec![7., 8., 9.]
            }
        );
        assert_eq!(
            s.complete(101, 1, 3, "new".into()),
            PartialTranscriptionAction::PublishText {
                session_id: 1,
                revision: 3,
                text: "new".into()
            }
        );
    }

    #[test]
    fn reset_and_finalize_reject_stale_results() {
        let mut s = scheduler();
        s.start_session(1);
        let _ = s.request_snapshot(0, &[1., 2., 3.]);
        s.reset();
        assert_eq!(
            s.complete(1, 1, 1, "stale".into()),
            PartialTranscriptionAction::Ignore
        );
        s.start_session(2);
        let _ = s.request_snapshot(2, &[1., 2., 3.]);
        s.finalize();
        assert_eq!(
            s.complete(200, 2, 1, "stale".into()),
            PartialTranscriptionAction::Ignore
        );
    }

    #[test]
    fn new_session_keeps_old_slot_and_stale_completion_starts_pending() {
        let mut s = scheduler();
        s.start_session(1);
        let _ = s.request_snapshot(0, &[1., 2., 3.]);
        s.reset();
        s.start_session(2);
        assert_eq!(
            s.request_snapshot(10, &[4., 5., 6.]),
            PartialTranscriptionAction::Ignore
        );
        assert_eq!(
            s.complete(100, 1, 1, "stale".into()),
            PartialTranscriptionAction::StartSnapshot {
                session_id: 2,
                revision: 2,
                samples: vec![4., 5., 6.]
            }
        );
        assert_eq!(s.in_flight.as_ref().map(|x| x.session_id), Some(2));
    }

    #[test]
    fn busy_requests_inside_throttle_do_not_copy_or_advance_revision() {
        let mut s = scheduler();
        s.start_session(1);
        let _ = s.request_snapshot(0, &[1., 2., 3.]);
        let _ = s.request_snapshot(10, &[4., 5., 6.]);
        let _ = s.request_snapshot(20, &[7., 8., 9.]);
        assert_eq!(s.next_revision, 2);
        assert_eq!(s.pending.as_ref().unwrap().samples, vec![4., 5., 6.]);
        assert_eq!(
            s.request_snapshot(110, &[7., 8., 9.]),
            PartialTranscriptionAction::Ignore
        );
        assert_eq!(s.next_revision, 3);
        assert_eq!(s.pending.as_ref().unwrap().samples, vec![7., 8., 9.]);
    }
}
