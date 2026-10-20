//! Bounded first-paragraph selection and one shared typed/voice reply-speech owner.

#[cfg(test)]
mod tests {
    use super::*;

    fn excerpt(value: &str) -> Option<String> {
        let mut paragraph = Paragraph::default();
        paragraph.push(value);
        paragraph.finish()
    }

    #[test]
    fn first_nonempty_line_and_ecmascript_whitespace() {
        assert_eq!(
            excerpt("\r\n \t\n hello\tworld\r\nignored"),
            Some("hello world".into())
        );
        assert_eq!(
            excerpt("\u{feff}hello\u{a0}world\u{feff}"),
            Some("hello world".into())
        );
        assert_eq!(excerpt("hello\u{85}world"), Some("hello\u{85}world".into()));
    }

    #[test]
    fn truncate_only_when_needed_and_prefer_sentence() {
        assert_eq!(
            excerpt(&format!("Done. {}", "x".repeat(601))),
            Some("Done.".into())
        );
        assert_eq!(excerpt("Done. more words"), Some("Done. more words".into()));
        assert_eq!(excerpt(&"é".repeat(301)), None);
        assert_eq!(excerpt(&"é".repeat(300)), Some("é".repeat(300)));
        assert_eq!(
            excerpt(&vec!["a"; 101].join(" ")),
            Some(vec!["a"; 100].join(" "))
        );
    }

    #[test]
    fn controls_are_rejected_after_extraction_not_sanitized() {
        assert_eq!(excerpt("bad\0text\nokay"), None);
        assert_eq!(excerpt("a\u{2028}b"), Some("a b".into()));
        assert_eq!(excerpt("okay\nignored\0"), Some("okay".into()));
    }

    #[test]
    fn streaming_waits_for_lf_and_is_chunk_independent() {
        let mut owner = ReplySpeech::default();
        owner.begin("r", true);
        assert!(owner.text("r", "\r\n hi\r").speak.is_none());
        let speech = owner.text("r", "\nsecond").speak.unwrap();
        assert_eq!(speech.text, "hi");
        assert!(owner.text("r", "\nthird").speak.is_none());
        assert!(owner.settle("r", Some("hi\nsecond")).speak.is_none());
        assert!(owner.valid(speech.generation));
    }

    #[test]
    fn generations_correction_canonical_and_stale_events() {
        let mut owner = ReplySpeech::default();
        owner.begin("old", true);
        let old = owner.text("old", "provisional\n").speak.unwrap();
        assert!(owner.corrected("old").cancel);
        assert!(!owner.valid(old.generation));
        assert!(owner.text("old", "not canonical\n").speak.is_none());
        let final_speech = owner.settle("old", Some("canonical")).speak.unwrap();
        assert_eq!(final_speech.text, "canonical");
        assert!(owner.settle("old", Some("canonical")).speak.is_none());
        owner.begin("new", true);
        assert!(!owner.valid(final_speech.generation));
        assert!(!owner.cancel("old").cancel);
        assert!(owner.settle("old", Some("stale")).speak.is_none());
        assert_eq!(
            owner
                .settle("new", Some("I cannot help."))
                .speak
                .unwrap()
                .text,
            "I cannot help."
        );
    }

    #[test]
    fn failure_disabled_toggle_and_changed_canonical() {
        let mut owner = ReplySpeech::default();
        owner.begin("r", true);
        let speech = owner.text("r", "early\n").speak.unwrap();
        let decision = owner.settle("r", Some("different"));
        assert!(decision.cancel);
        assert_eq!(decision.speak.unwrap().text, "different");
        assert!(!owner.valid(speech.generation));
        owner.begin("r2", true);
        let speech = owner.text("r2", "early\n").speak.unwrap();
        assert!(owner.settle("r2", None).cancel);
        assert!(!owner.valid(speech.generation));
        assert!(owner.settle("r2", Some("late")).speak.is_none());
        owner.begin("r3", false);
        assert!(owner.text("r3", "silent\n").speak.is_none());
        owner.set_enabled(true);
        assert!(owner.settle("r3", Some("silent")).speak.is_none());
        owner.begin("r4", true);
        owner.set_enabled(false);
        owner.set_enabled(true);
        assert!(owner.settle("r4", Some("silent")).speak.is_none());
    }

    #[test]
    fn bounded_even_for_unterminated_huge_words_and_blank_lines() {
        let mut p = Paragraph::default();
        for _ in 0..10_000 {
            p.push("     \r\n");
        }
        p.push("Done. ");
        for _ in 0..10_000 {
            p.push(&"x".repeat(1000));
        }
        assert!(p.excerpt.len() + p.word.len() <= 1200);
        assert_eq!(p.finish(), Some("Done.".into()));
    }

    #[test]
    fn every_character_split_matches_completed_selection() {
        for value in [
            "\u{feff}\r\n\t\nanswer; more\twords\r\nignored".to_string(),
            format!("answer: {}\nignored", "é".repeat(400)),
            format!("{}\nignored", vec!["word"; 101].join(" ")),
            "bad\u{7f}word\nvalid later line".to_string(),
        ] {
            let expected = excerpt(&value);
            for split in value.char_indices().map(|(index, _)| index) {
                let mut p = Paragraph::default();
                p.push(&value[..split]);
                p.push(&value[split..]);
                assert_eq!(p.finish(), expected);
            }
        }
    }

    #[test]
    fn invalid_canonical_and_reset_retire_provisional_work() {
        let mut owner = ReplySpeech::default();
        owner.begin("r", true);
        let old = owner.text("r", "valid\n").speak.unwrap();
        let decision = owner.settle("r", Some("bad\0word"));
        assert!(decision.cancel);
        assert!(decision.speak.is_none());
        assert!(!owner.valid(old.generation));
        owner.begin("next", true);
        let speech = owner
            .settle("next", Some("final without newline"))
            .speak
            .unwrap();
        assert!(owner.valid(speech.generation));
        assert!(owner.reset().cancel);
        assert!(!owner.valid(speech.generation));
    }

    #[test]
    fn correction_does_not_replay_identical_first_paragraph() {
        let mut owner = ReplySpeech::default();
        owner.begin("r", true);
        let speech = owner
            .text("r", "same first line\nold detail")
            .speak
            .unwrap();
        owner.corrected("r");
        assert!(!owner.valid(speech.generation));
        assert!(owner
            .settle("r", Some("same first line\nnew detail"))
            .speak
            .is_none());
    }
}

const MAX_BYTES: usize = 600;
const MAX_WORDS: usize = 100;

// ECMAScript trim / \s, deliberately not Rust's Unicode White_Space (U+0085 differs).
fn whitespace(c: char) -> bool {
    matches!(c, '\u{0009}'..='\u{000d}' | ' ' | '\u{00a0}' | '\u{1680}'
        | '\u{2000}'..='\u{200a}' | '\u{2028}' | '\u{2029}' | '\u{202f}'
        | '\u{205f}' | '\u{3000}' | '\u{feff}')
}

#[derive(Default)]
struct Paragraph {
    excerpt: String,
    word: String,
    words: usize,
    sentence_end: usize,
    nonempty: bool,
    truncated: bool,
    complete: bool,
}

impl Paragraph {
    fn push(&mut self, value: &str) {
        if self.complete {
            return;
        }
        for c in value.chars() {
            if c == '\n' {
                if self.nonempty {
                    self.complete();
                    break;
                }
                continue;
            }
            if whitespace(c) {
                self.flush_word();
            } else {
                self.nonempty = true;
                if self.truncated {
                    continue;
                }
                let separator = usize::from(!self.excerpt.is_empty());
                if self.words >= MAX_WORDS
                    || self.excerpt.len() + separator + self.word.len() + c.len_utf8() > MAX_BYTES
                {
                    self.truncated = true;
                    self.word.clear();
                } else {
                    self.word.push(c);
                }
            }
        }
    }

    fn flush_word(&mut self) {
        if self.word.is_empty() || self.truncated {
            return;
        }
        if !self.excerpt.is_empty() {
            self.excerpt.push(' ');
        }
        self.excerpt.push_str(&self.word);
        self.words += 1;
        if self.word.ends_with(['.', '!', '?', ';', ':']) {
            self.sentence_end = self.excerpt.len();
        }
        self.word.clear();
    }

    fn complete(&mut self) {
        self.flush_word();
        self.complete = true;
        if self.truncated && self.sentence_end > 0 {
            self.excerpt.truncate(self.sentence_end);
        }
    }

    fn finish(&mut self) -> Option<String> {
        self.complete();
        let valid = !self.excerpt.is_empty()
            && !self
                .excerpt
                .chars()
                .any(|c| c < '\u{20}' || matches!(c, '\u{7f}' | '\u{2028}' | '\u{2029}'));
        valid.then(|| self.excerpt.clone())
    }
}

/// Pure decisions only: no engine, queue, playback, canonical mutation, or logging.
/// Serialize calls under the parent's scheduler lock for BOTH typed and voice requests.
#[derive(Default)]
pub struct ReplySpeech {
    generation: u64,
    request_id: Option<String>,
    enabled: bool,
    active: bool,
    settled: bool,
    corrected: bool,
    paragraph: Paragraph,
    scheduled: Option<String>,
    last_emitted: Option<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct SpeechRequest {
    pub generation: u64,
    pub text: String,
}

/// Apply `cancel` BEFORE `speak`: retire obsolete pending/output work. Invalidation is
/// not physical retirement. Parent must retain physical work until it actually returns,
/// keep at most one replaceable pending synthesis, and fence reserve/start/finish/error
/// using `valid` under its admission synchronization (not a check-then-start race).
#[derive(Default, Debug, PartialEq, Eq)]
pub struct Decision {
    pub cancel: bool,
    pub speak: Option<SpeechRequest>,
}

impl ReplySpeech {
    fn invalidate(&mut self) -> Decision {
        // Never reuse a generation, including at integer exhaustion.
        self.generation = self
            .generation
            .checked_add(1)
            .expect("speech generation exhausted");
        self.scheduled = None;
        Decision {
            cancel: true,
            speak: None,
        }
    }

    fn matches(&self, id: &str) -> bool {
        self.active && self.request_id.as_deref() == Some(id)
    }

    /// New canonical request admission, not observer attachment. Always invalidates old work.
    pub fn begin(&mut self, id: &str, enabled: bool) -> Decision {
        let decision = self.invalidate();
        self.request_id = Some(id.into());
        self.enabled = enabled;
        self.active = enabled;
        self.settled = false;
        self.corrected = false;
        self.paragraph = Paragraph::default();
        self.last_emitted = None;
        decision
    }

    fn schedule(&mut self, text: Option<String>) -> Decision {
        if self.scheduled.is_some() {
            return Decision::default();
        }
        let speak = text.map(|text| {
            self.scheduled = Some(text.clone());
            self.last_emitted = Some(text.clone());
            SpeechRequest {
                generation: self.generation,
                text,
            }
        });
        Decision {
            cancel: false,
            speak,
        }
    }

    /// Append-only delta. A completed first nonempty LF-terminated line may speak
    /// provisionally. Later lines are neither stored nor scanned.
    pub fn text(&mut self, id: &str, delta: &str) -> Decision {
        if !self.matches(id) || self.settled || self.corrected || self.paragraph.complete {
            return Decision::default();
        }
        self.paragraph.push(delta);
        if self.paragraph.complete {
            let text = self.paragraph.finish();
            self.schedule(text)
        } else {
            Decision::default()
        }
    }

    /// Any canonical correction retires provisional speech immediately. Do not speak
    /// replacement deltas: only committed canonical settlement may resume speech.
    pub fn corrected(&mut self, id: &str) -> Decision {
        if !self.matches(id) || self.settled {
            return Decision::default();
        }
        self.corrected = true;
        self.paragraph = Paragraph::default();
        self.invalidate()
    }

    /// Call AFTER canonical commit with the committed answer, including refusals.
    /// None means failed/cancelled/uncommitted: permanently retire this request.
    /// Canonical text is borrowed, never retained beyond its bounded first paragraph.
    pub fn settle(&mut self, id: &str, canonical: Option<&str>) -> Decision {
        if !self.matches(id) || self.settled {
            return Decision::default();
        }
        let Some(canonical) = canonical else {
            return self.cancel(id);
        };
        self.settled = true;
        let mut paragraph = Paragraph::default();
        paragraph.push(canonical);
        let text = paragraph.finish();
        if self.last_emitted == text {
            return Decision::default();
        }
        let cancel = self.scheduled.is_some();
        if cancel {
            self.invalidate();
        }
        let mut decision = self.schedule(text);
        decision.cancel = cancel;
        decision
    }

    /// Request-specific failure/Stop. Stale request events cannot cancel newer work.
    pub fn cancel(&mut self, id: &str) -> Decision {
        if !self.matches(id) {
            return Decision::default();
        }
        self.reset()
    }

    /// Unconditional Stop/reset/shutdown fence, also valid after successful settlement.
    pub fn reset(&mut self) -> Decision {
        self.active = false;
        self.request_id = None;
        self.paragraph = Paragraph::default();
        self.last_emitted = None;
        self.invalidate()
    }

    /// Off permanently suppresses this request; turning on never replays old answers.
    /// The next begin uses the current preference supplied by the parent.
    pub fn set_enabled(&mut self, enabled: bool) -> Decision {
        self.enabled = enabled;
        if !enabled {
            self.reset()
        } else {
            Decision::default()
        }
    }

    pub fn valid(&self, generation: u64) -> bool {
        self.active && self.enabled && self.scheduled.is_some() && self.generation == generation
    }
}
