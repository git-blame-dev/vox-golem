//! Bounded first-paragraph selection and one shared typed/voice reply-speech owner.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn early_clause_does_not_split_a_decimal_at_a_delta_boundary() {
        let mut owner = ReplySpeech::early_clauses();
        owner.begin("decimal", true);
        assert!(owner.text("decimal", "The value is 3.").speak.is_none());
        assert_eq!(
            owner.text("decimal", "14 units. ").speak.unwrap().text,
            "The value is 3.14 units."
        );
    }

    #[test]
    fn identical_truncated_canonical_does_not_replay_but_shorter_correction_does() {
        for unchanged in [true, false] {
            let mut owner = ReplySpeech::early_clauses();
            owner.begin("r", true);
            let opening = "one two three. ";
            let long = "synthetic ".repeat(16);
            let flood = "x".repeat(601);
            let first = owner.text("r", opening).speak.unwrap();
            let second = owner.text("r", &long).speak.unwrap();
            let overflow = owner.text("r", &flood);
            assert!(!overflow.cancel && overflow.speak.is_none());
            let accumulated = format!("{opening}{long}{flood}");
            let canonical = if unchanged {
                accumulated.as_str()
            } else {
                "one two three."
            };
            let decision = owner.settle("r", Some(canonical));
            if unchanged {
                assert!(!decision.cancel);
                assert!(decision.speak.is_none());
                assert!(owner.valid(first.generation));
            } else {
                assert!(decision.cancel);
                let replacement = decision.speak.unwrap();
                assert_eq!(replacement.text, "one two three.");
                assert!(!owner.valid(first.generation));
                assert!(first.text.len() + second.text.len() + replacement.text.len() <= MAX_BYTES);
                assert!(
                    first.text.split(' ').count()
                        + second.text.split(' ').count()
                        + replacement.text.split(' ').count()
                        <= MAX_WORDS
                );
            }
        }
    }

    #[test]
    fn initial_clause_requires_three_useful_words_but_close_flushes_short_answers() {
        let mut owner = ReplySpeech::early_clauses();
        owner.begin("r", true);
        assert!(owner.text("r", "Hello world. ").speak.is_none());
        assert!(owner.text("r", "!!! ").speak.is_none());
        assert_eq!(
            owner.text("r", "Again! ").speak.unwrap().text,
            "Hello world. !!! Again!"
        );
        assert_eq!(owner.text("r", "Yes. ").speak.unwrap().text, "Yes.");
        owner.begin("lf", true);
        assert!(owner.text("lf", "Yes. ").speak.is_none());
        assert_eq!(owner.text("lf", "\n").speak.unwrap().text, "Yes.");
        owner.begin("final", true);
        assert!(owner.text("final", "Yes. ").speak.is_none());
        assert_eq!(
            owner.settle("final", Some("Yes.")).speak.unwrap().text,
            "Yes."
        );
    }

    #[test]
    fn early_clauses_append_and_settle_without_prefix_replay() {
        let mut owner = ReplySpeech::early_clauses();
        owner.begin("r", true);
        assert!(owner.text("r", "Hello").speak.is_none());
        let first = owner.text("r", " synthetic world. ").speak.unwrap();
        assert_eq!(first.text, "Hello synthetic world.");
        assert_eq!(
            owner.text("r", "Next clause; ").speak.unwrap().text,
            "Next clause;"
        );
        let end = owner.settle("r", Some("Hello synthetic world. Next clause; tail"));
        assert_eq!(end.speak.unwrap().text, "tail");
        assert!(end.stream_closed);
        assert!(owner.valid(first.generation));
    }

    #[test]
    fn early_bounds_corrections_controls_and_stale_work() {
        let mut owner = ReplySpeech::early_clauses();
        owner.begin("r", true);
        let first = owner.text("r", "Same synthetic clause; ").speak.unwrap();
        assert!(owner.corrected("r").cancel);
        assert!(!owner.valid(first.generation));
        assert!(owner.text("r", "ignored. ").speak.is_none());
        assert_eq!(
            owner
                .settle("r", Some("Same synthetic clause; new tail"))
                .speak
                .unwrap()
                .text,
            "new tail"
        );
        owner.begin("flood", true);
        let mut bytes = 0;
        let mut words = 0;
        for _ in 0..10_000 {
            if let Some(chunk) = owner.text("flood", "é; ").speak {
                bytes += chunk.text.len();
                words += chunk.text.split_whitespace().count();
            }
            assert!(owner.paragraph.excerpt.len() + owner.paragraph.word.len() <= 1200);
        }
        assert!(bytes <= MAX_BYTES && words <= MAX_WORDS);
        assert!(bytes > 0);
        owner.begin("huge", true);
        assert!(owner.text("huge", &"é".repeat(100_000)).speak.is_none());
        owner.begin("bad", true);
        assert!(owner.text("bad", "bad\0clause; ").speak.is_none());
        owner.begin("valid", true);
        let chunk = owner.text("valid", "this is okay: ").speak.unwrap();
        owner.set_enabled(false);
        owner.set_enabled(true);
        assert!(!owner.valid(chunk.generation));
        assert!(owner.settle("valid", Some("okay: tail")).speak.is_none());
        owner.begin("new", true);
        assert!(!owner.cancel("valid").cancel);
        let chunk = owner.text("new", "this is new. ").speak.unwrap();
        owner.reset();
        assert!(!owner.valid(chunk.generation));
    }

    #[test]
    fn early_character_deltas_match_unicode_and_canonical_replacement() {
        let mut owner = ReplySpeech::early_clauses();
        owner.begin("r", true);
        let mut chunks = Vec::new();
        for c in "\u{feff}héllo\tsynthetic world; next! tail".chars() {
            if let Some(chunk) = owner.text("r", &c.to_string()).speak {
                chunks.push(chunk.text);
            }
        }
        assert_eq!(chunks, ["héllo synthetic world;", "next!"]);
        let decision = owner.settle("r", Some("different canonical"));
        assert!(decision.cancel && decision.stream_closed);
        assert_eq!(decision.speak.unwrap().text, "different canonical");
    }

    #[test]
    fn early_all_splits_preserve_selection_and_replacements_share_budget() {
        let value = "\u{feff}héllo\tworld; next! tail\nignored";
        for split in value.char_indices().map(|(index, _)| index) {
            let mut owner = ReplySpeech::early_clauses();
            owner.begin("r", true);
            let mut chunks = Vec::new();
            for delta in [&value[..split], &value[split..]] {
                if let Some(chunk) = owner.text("r", delta).speak {
                    chunks.push(chunk.text);
                }
            }
            assert_eq!(chunks.join(" "), "héllo world; next! tail");
            assert!(owner.settle("r", Some(value)).speak.is_none());
        }
        let mut owner = ReplySpeech::early_clauses();
        owner.begin("r", true);
        let first = owner
            .text("r", &format!("two words {}; ", "é".repeat(250)))
            .speak
            .unwrap();
        let replacement = owner.settle("r", Some(&vec!["new"; 100].join(" ")));
        assert!(replacement.cancel);
        let second = replacement.speak.unwrap();
        assert!(first.text.len() + second.text.len() <= MAX_BYTES);
        assert!(!owner.valid(first.generation));
        assert!(owner.valid(second.generation));
    }

    #[test]
    fn mobile_comma_and_long_clauses_release_without_replaying() {
        let mut owner = ReplySpeech::early_clauses();
        owner.begin("r", true);
        assert_eq!(
            owner
                .text("r", "one two three four five, ")
                .speak
                .unwrap()
                .text,
            "one two three four five,"
        );
        let long = vec!["synthetic"; 16].join(" ") + " ";
        let chunk = owner.text("r", &long).speak.unwrap();
        assert_eq!(chunk.text, long.trim());
        assert!(owner.text("r", "tail ").speak.is_none());
        let final_text = format!("one two three four five, {long}tail");
        assert_eq!(
            owner.settle("r", Some(&final_text)).speak.unwrap().text,
            "tail"
        );
    }

    fn excerpt(value: &str) -> Option<String> {
        first_paragraph(value)
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

fn valid_excerpt(text: &str) -> bool {
    !text.is_empty()
        && !text
            .chars()
            .any(|c| c < '\u{20}' || matches!(c, '\u{7f}' | '\u{2028}' | '\u{2029}'))
}

fn useful_words(text: &str) -> usize {
    text.split(' ')
        .filter(|word| word.chars().any(char::is_alphanumeric))
        .count()
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
    }

    fn finish(&mut self) -> Option<String> {
        self.complete();
        // Keep the bounded pre-fallback prefix for canonical reconciliation. The
        // selection still falls back to the last sentence, exactly as before.
        let selected = if self.truncated && self.sentence_end > 0 {
            &self.excerpt[..self.sentence_end]
        } else {
            &self.excerpt
        };
        valid_excerpt(selected).then(|| selected.to_owned())
    }
}

/// Extract a normalized, validated, bounded first paragraph without creating an owner.
pub fn first_paragraph(text: &str) -> Option<String> {
    let mut paragraph = Paragraph::default();
    paragraph.push(text);
    paragraph.finish()
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
    early: bool,
    emitted_bytes: usize,
    emitted_words: usize,
    chunk_id: u64,
}

#[derive(Debug, PartialEq, Eq)]
pub struct SpeechRequest {
    pub generation: u64,
    /// Monotonic within an admission, including canonical replacements.
    pub chunk_id: u64,
    pub text: String,
}

/// Apply `cancel` BEFORE `speak`: retire obsolete pending/output work. Invalidation is
/// not physical retirement. Parent must retain physical work until it actually returns,
/// and fence reserve/start/finish/error using `valid` under its admission synchronization
/// (not a check-then-start race). Desktop keeps at most one replaceable pending
/// synthesis; early-mode adapters own a bounded effect queue for ordered chunks.
#[derive(Default, Debug, PartialEq, Eq)]
pub struct Decision {
    pub cancel: bool,
    pub speak: Option<SpeechRequest>,
    /// Early mode: the bounded append stream is closed, not playback completed.
    /// Canonical settlement may still replace it (with cancellation). Cancellation
    /// itself always retires old work, independently of this flag.
    pub stream_closed: bool,
}

impl ReplySpeech {
    /// Opt-in mobile clause release. Default remains desktop LF release.
    /// Feed append-only deltas and settle with committed canonical text. Apply
    /// cancellation before chunks; queue by (generation, chunk_id), fence callbacks
    /// with `valid`, and bound native pending work. No acknowledgement is needed by
    /// this pure owner: a valid generation does not imply physical completion.
    /// Release completed words at `. ! ? ; :`, commas after five pending words,
    /// or 140 pending UTF-8 bytes. Word completion waits for whitespace, avoiding
    /// a chunk-edge period being mistaken for the end of a decimal or URL.
    /// Initial release requires three words containing an alphanumeric character;
    /// subsequent releases require one. LF/final settlement flushes short answers.
    /// All issued chunks together, including replacements, consume at most 600
    /// UTF-8 bytes and 100 words per begin. LF still ends the first paragraph.
    pub fn early_clauses() -> Self {
        Self {
            early: true,
            ..Self::default()
        }
    }
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
            stream_closed: false,
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
        self.emitted_bytes = 0;
        self.emitted_words = 0;
        self.chunk_id = 0;
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
                chunk_id: 0,
                text,
            }
        });
        Decision {
            cancel: false,
            speak,
            stream_closed: false,
        }
    }

    /// Append-only delta. A completed first nonempty LF-terminated line may speak
    /// provisionally. Later lines are neither stored nor scanned.
    pub fn text(&mut self, id: &str, delta: &str) -> Decision {
        if !self.matches(id) || self.settled || self.corrected || self.paragraph.complete {
            return Decision::default();
        }
        self.paragraph.push(delta);
        if self.early {
            let closed = self.paragraph.complete || self.paragraph.truncated;
            let text = if closed {
                let text = self.paragraph.finish();
                if self.paragraph.truncated
                    && text.as_deref().is_some_and(|text| {
                        self.last_emitted
                            .as_deref()
                            .is_some_and(|previous| previous.starts_with(text))
                    })
                {
                    self.last_emitted.clone()
                } else {
                    text
                }
            } else {
                let previous = self.last_emitted.as_deref().unwrap_or("");
                let pending = self
                    .paragraph
                    .excerpt
                    .strip_prefix(previous)
                    .unwrap_or("")
                    .trim_start_matches(' ');
                // Reuse normalized complete words for mobile comma/long clauses.
                let clause =
                    pending.len() >= 140 || (pending.ends_with(',') && useful_words(pending) >= 5);
                let end = if clause {
                    self.paragraph.excerpt.len()
                } else {
                    self.paragraph.sentence_end.max(previous.len())
                };
                let prefix = &self.paragraph.excerpt[..end];
                let useful_words = useful_words(prefix.strip_prefix(previous).unwrap_or(""));
                let minimum = if self.chunk_id == 0 { 3 } else { 1 };
                if useful_words >= minimum && valid_excerpt(prefix) {
                    Some(prefix.to_owned())
                } else {
                    self.last_emitted.clone()
                }
            };
            return self.schedule_early(text, closed);
        }
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
        if self.early {
            // An overflowing unfinished word can make sentence fallback shorter
            // than clauses already issued. Preserve those clauses only when the
            // canonical bounded pre-fallback prefix actually contains them. A
            // genuinely shorter canonical answer does not have this evidence.
            if paragraph.truncated
                && self.last_emitted.as_deref().is_some_and(|previous| {
                    paragraph
                        .excerpt
                        .strip_prefix(previous)
                        .is_some_and(|suffix| suffix.is_empty() || suffix.starts_with(' '))
                        && text
                            .as_deref()
                            .is_some_and(|selected| previous.starts_with(selected))
                })
            {
                return self.schedule_early(self.last_emitted.clone(), true);
            }
            return self.schedule_early(text, true);
        }
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

    fn schedule_early(&mut self, text: Option<String>, closed: bool) -> Decision {
        let previous = self.last_emitted.as_deref().unwrap_or("");
        let current = text.as_deref().unwrap_or("");
        let suffix = current
            .strip_prefix(previous)
            .filter(|suffix| previous.is_empty() || suffix.is_empty() || suffix.starts_with(' '));
        let changed = suffix.is_none();
        let candidate = suffix.unwrap_or(current).trim_start_matches(' ');
        // Current and previous are already normalized/validated by Paragraph.
        // Budget counts all issued chunks, even cancelled or replaced ones.
        let mut chunk = String::new();
        let mut words = 0;
        for word in candidate.split(' ') {
            if word.is_empty() {
                continue;
            }
            let separator = usize::from(!chunk.is_empty());
            if self.emitted_words + words >= MAX_WORDS
                || self.emitted_bytes + chunk.len() + separator + word.len() > MAX_BYTES
            {
                break;
            }
            if separator != 0 {
                chunk.push(' ');
            }
            chunk.push_str(word);
            words += 1;
        }
        let cancel = changed && self.scheduled.is_some();
        if cancel {
            self.invalidate();
        }
        let speak = if chunk.is_empty() {
            None
        } else {
            self.emitted_bytes += chunk.len();
            self.emitted_words += words;
            self.chunk_id += 1;
            self.scheduled = Some(current.to_owned());
            self.last_emitted = Some(current.to_owned());
            Some(SpeechRequest {
                generation: self.generation,
                chunk_id: self.chunk_id,
                text: chunk,
            })
        };
        Decision {
            cancel,
            speak,
            stream_closed: closed
                || self.emitted_bytes >= MAX_BYTES
                || self.emitted_words >= MAX_WORDS,
        }
    }
}
