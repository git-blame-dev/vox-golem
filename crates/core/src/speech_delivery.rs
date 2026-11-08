//! Ordered delivery of ReplySpeech decisions. No engines or physical-retirement claims.
use crate::reply_speech::{Decision, SpeechRequest, MAX_BYTES, MAX_WORDS};
use std::collections::VecDeque;

#[derive(Debug, PartialEq, Eq)]
pub enum DeliveryError {
    Capacity,
    Stale,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct DeliveryFacts {
    pub pending: usize,
    pub in_flight: usize,
    /// Bounded PCM accepted by output, awaiting the output session's drain.
    pub produced: usize,
    pub stream_closed: bool,
    /// Accepted terminal callbacks; adapters retain success/failure and retirement facts.
    pub completed: u64,
    /// Logical invalidation only; adapters must still retire their native resources.
    pub cancelled: u64,
}

/// Save `decision.cancel`, apply the selector decision, stop old output if requested, then drain
/// `take_ready()` under the same adapter lock. Acknowledge actual playback callbacks
/// with `complete(generation, chunk_id)`, then drain again. Desktop read-ahead is 1;
/// mobile is 2. Settlement/stream_closed is NOT a playback acknowledgement.
///
/// Pending storage is bounded to 600 bytes/100 requests, independently of the
/// selector's lifetime emission budget. Errors never replace or drop an early clause.
pub struct SpeechDelivery {
    pending: VecDeque<SpeechRequest>,
    in_flight: Vec<(u64, u64)>,
    produced: Vec<(u64, u64)>,
    pending_bytes: usize,
    read_ahead: usize,
    last: Option<(u64, u64)>,
    allow_generation_change: bool,
    facts: DeliveryFacts,
}
impl SpeechDelivery {
    pub fn new(read_ahead: usize) -> Self {
        assert!((1..=MAX_WORDS).contains(&read_ahead));
        Self {
            pending: VecDeque::new(),
            in_flight: Vec::with_capacity(read_ahead),
            produced: Vec::new(),
            pending_bytes: 0,
            read_ahead,
            last: None,
            allow_generation_change: true,
            facts: DeliveryFacts::default(),
        }
    }
    pub fn apply(&mut self, decision: Decision) -> Result<(), DeliveryError> {
        if let Some(request) = &decision.speak {
            let key = (request.generation, request.chunk_id);
            if self.last.is_some_and(|last| {
                key.0 < last.0
                    || (key.0 == last.0 && (self.allow_generation_change || key.1 <= last.1))
                    || (key.0 != last.0 && !self.allow_generation_change && !decision.cancel)
            }) {
                return Err(DeliveryError::Stale);
            }
        }
        if let Some(request) = &decision.speak {
            let pending_bytes = if decision.cancel {
                0
            } else {
                self.pending_bytes
            };
            if (!decision.cancel && self.pending.len() >= MAX_WORDS)
                || pending_bytes + request.text.len() > MAX_BYTES
            {
                return Err(DeliveryError::Capacity);
            }
        }
        if decision.cancel {
            self.cancel();
        }
        if let Some(request) = decision.speak {
            let key = (request.generation, request.chunk_id);
            self.pending_bytes += request.text.len();
            self.last = Some(key);
            self.allow_generation_change = false;
            self.pending.push_back(request);
        }
        self.facts.stream_closed |= decision.stream_closed;
        Ok(())
    }
    pub fn take_ready(&mut self) -> Option<SpeechRequest> {
        if self.in_flight.len() >= self.read_ahead {
            return None;
        }
        let request = self.pending.pop_front()?;
        self.pending_bytes -= request.text.len();
        self.in_flight.push((request.generation, request.chunk_id));
        Some(request)
    }
    /// Acknowledge a terminal playback callback, including failures. Reverse-order,
    /// duplicate and stale callbacks free only their exact reservation. This does
    /// not certify SDK shutdown; adapters retain failed native retirement leases.
    pub fn complete(&mut self, generation: u64, chunk_id: u64) -> bool {
        let key = (generation, chunk_id);
        if let Some(index) = self.in_flight.iter().position(|item| *item == key) {
            self.in_flight.swap_remove(index);
        } else if let Some(index) = self.produced.iter().position(|item| *item == key) {
            self.produced.swap_remove(index);
        } else {
            return false;
        }
        self.facts.completed += 1;
        true
    }

    /// Releases synthesis capacity when bounded native output accepts the PCM.
    /// It does not certify playback completion or native resource retirement.
    pub fn produced(&mut self, generation: u64, chunk_id: u64) -> bool {
        if self.produced.len() >= MAX_WORDS {
            return false;
        }
        let key = (generation, chunk_id);
        let Some(index) = self.in_flight.iter().position(|item| *item == key) else {
            return false;
        };
        self.in_flight.swap_remove(index);
        self.produced.push(key);
        true
    }

    /// Called only after a continuous native output session reaches its terminal
    /// drain/error callback. A stale session cannot acknowledge its replacement.
    pub fn complete_produced(&mut self, generation: u64) -> usize {
        let before = self.produced.len();
        self.produced.retain(|key| key.0 != generation);
        let count = before - self.produced.len();
        self.facts.completed += count as u64;
        count
    }

    pub fn cancel(&mut self) {
        self.facts.cancelled +=
            (self.pending.len() + self.in_flight.len() + self.produced.len()) as u64;
        self.pending.clear();
        self.in_flight.clear();
        self.produced.clear();
        self.pending_bytes = 0;
        self.facts.stream_closed = false;
        self.allow_generation_change = true;
        // Keep last identity as a fence against replay after cancellation.
    }
    pub fn reset(&mut self) {
        self.cancel();
    }
    pub fn facts(&self) -> DeliveryFacts {
        DeliveryFacts {
            pending: self.pending.len(),
            in_flight: self.in_flight.len(),
            produced: self.produced.len(),
            stream_closed: self.facts.stream_closed,
            completed: self.facts.completed,
            cancelled: self.facts.cancelled,
        }
    }
    pub fn is_idle(&self) -> bool {
        self.pending.is_empty() && self.in_flight.is_empty() && self.produced.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reply_speech::ReplySpeech;
    #[test]
    fn pcm_submission_frees_capacity_without_claiming_output_drain() {
        let mut selector = ReplySpeech::early_clauses();
        let mut delivery = SpeechDelivery::new(1);
        delivery.apply(selector.begin("synthetic", true)).unwrap();
        delivery
            .apply(selector.text("synthetic", "One two three. "))
            .unwrap();
        delivery
            .apply(selector.text("synthetic", "Four five six. "))
            .unwrap();
        let first = delivery.take_ready().unwrap();
        assert!(delivery.take_ready().is_none());
        assert!(delivery.produced(first.generation, first.chunk_id));
        let second = delivery.take_ready().unwrap();
        assert!(delivery.produced(second.generation, second.chunk_id));
        assert!(!delivery.is_idle());
        assert_eq!(delivery.facts().completed, 0);
        assert_eq!(delivery.complete_produced(first.generation), 2);
        assert!(delivery.is_idle());
        assert_eq!(delivery.facts().completed, 2);
        assert!(!delivery.complete(first.generation, first.chunk_id));
    }
    #[test]
    fn blocked_consumer_keeps_every_clause_until_exact_ack() {
        let mut selection = ReplySpeech::early_clauses();
        selection.begin("synthetic", true);
        let mut delivery = SpeechDelivery::new(2);
        for text in [
            "One two three. ",
            "Four five six. ",
            "Seven eight nine. ",
            "Ten eleven twelve. ",
        ] {
            delivery.apply(selection.text("synthetic", text)).unwrap();
        }
        let a = delivery.take_ready().unwrap();
        let b = delivery.take_ready().unwrap();
        assert!(delivery.take_ready().is_none());
        delivery
            .apply(selection.settle(
                "synthetic",
                Some("One two three. Four five six. Seven eight nine. Ten eleven twelve."),
            ))
            .unwrap();
        assert!(!delivery.is_idle());
        assert!(delivery.complete(b.generation, b.chunk_id));
        assert!(!delivery.complete(b.generation, b.chunk_id));
        assert!(!delivery.complete(a.generation + 1, a.chunk_id));
        let c = delivery.take_ready().unwrap();
        assert_eq!(c.text, "Seven eight nine.");
        assert!(delivery.complete(a.generation, a.chunk_id));
        let d = delivery.take_ready().unwrap();
        assert_eq!(d.text, "Ten eleven twelve.");
        assert!(delivery.complete(d.generation, d.chunk_id));
        assert!(!delivery.is_idle());
        assert!(delivery.complete(c.generation, c.chunk_id));
        assert!(delivery.is_idle());
    }
    #[test]
    fn stale_cancelling_decision_cannot_delete_newer_work() {
        let mut d = SpeechDelivery::new(1);
        let decision = |generation, cancel| Decision {
            cancel,
            stream_closed: false,
            speak: Some(SpeechRequest {
                generation,
                chunk_id: 1,
                text: "Synthetic clause.".into(),
            }),
        };
        d.apply(decision(2, false)).unwrap();
        assert_eq!(d.apply(decision(1, true)), Err(DeliveryError::Stale));
        assert_eq!(d.facts().pending, 1);
    }

    #[test]
    fn capacity_is_explicit_and_correction_cancels_before_replacement() {
        let mut d = SpeechDelivery::new(1);
        let decision = |generation, chunk_id, text: &str, cancel| Decision {
            cancel,
            speak: Some(SpeechRequest {
                generation,
                chunk_id,
                text: text.into(),
            }),
            stream_closed: false,
        };
        d.apply(decision(1, 1, &"x".repeat(600), false)).unwrap();
        assert_eq!(
            d.apply(decision(1, 2, "overflow", false)),
            Err(DeliveryError::Capacity)
        );
        assert_eq!(
            d.apply(decision(2, 3, &"x".repeat(601), true)),
            Err(DeliveryError::Capacity)
        );
        assert_eq!(d.facts().pending, 1);
        let old = d.take_ready().unwrap();
        d.apply(decision(2, 3, "replacement", true)).unwrap();
        assert!(!d.complete(old.generation, old.chunk_id));
        assert_eq!(d.take_ready().unwrap().text, "replacement");
        d.reset();
        assert_eq!(
            d.apply(decision(2, 3, "replay", false)),
            Err(DeliveryError::Stale)
        );
        assert_eq!(
            d.apply(decision(2, 4, "late old generation", false)),
            Err(DeliveryError::Stale)
        );
        assert!(d.is_idle());
        assert_eq!(d.facts().cancelled, 2);
    }
}
