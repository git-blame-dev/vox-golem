#![forbid(unsafe_code)]

//! 2D Intent Router: classifies incoming turns into one of 4 routes:
//! - DirectFast (F): low complexity, no external evidence needed.
//! - SearchFast (S): low complexity, needs single-round search lookup.
//! - DirectSmart (C): high complexity (coding, math, reasoning), no external evidence.
//! - ResearchSmart (R): high complexity, needs deep multi-step search/evidence.

use crate::router_weights::{BGE_SMALL_EMBEDDING_DIM, BGE_SMALL_EN_V15_RIDGE_W};
use crate::search::{has_readable_text, truncate_utf8, Mode, Source};
use serde::{Deserialize, Serialize};

pub const PARALLEL_MARGIN_THRESHOLD: f32 = 0.14;
pub const CANONICAL_SUPPLIED_TEXT_INTENT: &str =
    "Translate, summarize, rewrite, or fix typos in this provided text passage.";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Route2D {
    #[default]
    DirectFast, // F: index 0 in BGE_SMALL_EN_V15_RIDGE_W
    SearchFast,    // S: index 1 in BGE_SMALL_EN_V15_RIDGE_W
    DirectSmart,   // C: index 2 in BGE_SMALL_EN_V15_RIDGE_W
    ResearchSmart, // R: index 3 in BGE_SMALL_EN_V15_RIDGE_W
}

impl Route2D {
    pub const ALL: [Self; 4] = [
        Self::DirectFast,
        Self::SearchFast,
        Self::DirectSmart,
        Self::ResearchSmart,
    ];

    pub fn code(self) -> &'static str {
        match self {
            Self::DirectFast => "F",
            Self::SearchFast => "S",
            Self::DirectSmart => "C",
            Self::ResearchSmart => "R",
        }
    }

    pub fn needs_search(self) -> bool {
        matches!(self, Self::SearchFast | Self::ResearchSmart)
    }

    pub fn needs_smart(self) -> bool {
        matches!(self, Self::DirectSmart | Self::ResearchSmart)
    }

    pub fn effective_search_mode(self, requested: Mode) -> Option<Mode> {
        if requested != Mode::Quick {
            Some(requested)
        } else {
            match self {
                Self::DirectFast | Self::DirectSmart => None,
                Self::SearchFast => Some(Mode::Quick),
                Self::ResearchSmart => Some(Mode::Research),
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct RoutingDecision {
    pub primary: Route2D,
    pub secondary: Route2D,
    pub margin: f32,
    pub parallel_fast_and_smart: bool,
    pub normalized_input: String,
}

impl RoutingDecision {
    pub fn needs_search(&self) -> bool {
        self.primary.needs_search()
    }

    pub fn needs_smart(&self) -> bool {
        self.primary.needs_smart()
    }
}

fn strip_leading_fillers(mut text: &str) -> &str {
    loop {
        text = text.trim_start();
        let lower = text.to_lowercase();
        let mut stripped = false;
        for filler in ["um", "uh", "er", "ah", "like"] {
            if lower.starts_with(filler) {
                let remainder = &text[filler.len()..];
                if remainder.is_empty() {
                    text = "";
                    stripped = true;
                    break;
                }
                let first_char = remainder.chars().next().unwrap();
                if first_char.is_whitespace() || first_char.is_ascii_punctuation() {
                    let mut trimmed_rem = remainder;
                    while let Some(c) = trimmed_rem.chars().next() {
                        if c.is_whitespace() || c.is_ascii_punctuation() {
                            trimmed_rem = &trimmed_rem[c.len_utf8()..];
                        } else {
                            break;
                        }
                    }
                    text = trimmed_rem;
                    stripped = true;
                    break;
                }
            }
        }
        if !stripped {
            break;
        }
    }
    text
}

fn tail_utf8(text: &str, bytes: usize) -> &str {
    if text.len() <= bytes {
        return text;
    }
    let mut start = text.len() - bytes;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    &text[start..]
}

fn match_prefix_colon_supplied_text(text: &str) -> bool {
    let Some(colon_pos) = text.find(": ") else {
        return false;
    };
    if colon_pos > 80 {
        return false;
    }
    let payload = &text[colon_pos + 2..];
    if payload.trim().is_empty() {
        return false;
    }

    let prefix = &text[..colon_pos];
    let mut p = prefix.trim().to_lowercase();

    loop {
        p = p.trim().to_string();
        let mut stripped = false;
        const GREETINGS: &[&str] = &[
            "um",
            "uh",
            "yo",
            "hello",
            "hey",
            "hi",
            "can you",
            "please",
            "could you",
        ];
        for greeting in GREETINGS {
            if let Some(rem) = p.strip_prefix(greeting) {
                if rem.is_empty() {
                    p.clear();
                    stripped = true;
                    break;
                }
                let first = rem.chars().next().unwrap();
                if first.is_whitespace() || first.is_ascii_punctuation() {
                    let mut rem_trim = rem;
                    while let Some(c) = rem_trim.chars().next() {
                        if c.is_whitespace() || c.is_ascii_punctuation() {
                            rem_trim = &rem_trim[c.len_utf8()..];
                        } else {
                            break;
                        }
                    }
                    p = rem_trim.to_string();
                    stripped = true;
                    break;
                }
            }
        }
        if !stripped {
            break;
        }
    }

    const PREFIX_VERBS: &[&str] = &[
        "translate",
        "summarize",
        "summarise",
        "rewrite",
        "proofread",
        "fix typos",
        "fix the typos",
        "fix grammar",
        "fix the grammar",
        "fix spelling",
        "fix the spelling",
        "extract",
    ];

    PREFIX_VERBS.iter().any(|v| p.starts_with(v))
}

fn match_tail_supplied_text_instruction(text: &str) -> bool {
    if text.len() <= 180 {
        return false;
    }
    let tail = tail_utf8(text, 240);
    let tail_lower = tail.to_lowercase();

    const VERBS: &[&str] = &[
        "summarize",
        "summarise",
        "translate",
        "rewrite",
        "proofread",
        "paraphrase",
        "simplify",
        "fix the typos",
        "fix the grammar",
        "fix the spelling",
    ];

    const TARGETS: &[&str] = &[
        "above",
        "this",
        "preceding",
        "following",
        "text",
        "passage",
        "article",
    ];

    for verb in VERBS {
        let mut search_from = 0;
        while let Some(pos) = tail_lower[search_from..].find(verb) {
            let actual_pos = search_from + pos;
            let valid_start = actual_pos == 0
                || !tail_lower[..actual_pos]
                    .chars()
                    .next_back()
                    .unwrap()
                    .is_alphanumeric();
            let after_verb = &tail_lower[actual_pos + verb.len()..];
            if valid_start && (after_verb.starts_with(' ') || after_verb.starts_with('\t')) {
                let rem = after_verb.trim_start();
                let rem_after_the = if let Some(stripped) = rem.strip_prefix("the ") {
                    stripped.trim_start()
                } else {
                    rem
                };
                for target in TARGETS {
                    if let Some(after_target) = rem_after_the.strip_prefix(target) {
                        if after_target.is_empty()
                            || after_target.chars().next().unwrap().is_ascii_punctuation()
                            || after_target.chars().next().unwrap().is_whitespace()
                        {
                            return true;
                        }
                    }
                }
            }
            search_from = actual_pos + verb.len();
        }
    }
    false
}

fn strip_leading_discourse_markers(mut lower: &str) -> &str {
    const DISCOURSE_MARKERS: &[&str] = &[
        "wait", "okay", "ok", "so", "well", "hmm", "oh", "hey", "actually", "also", "and", "but",
        "then",
    ];
    loop {
        lower = lower.trim_start();
        let mut stripped = false;
        for marker in DISCOURSE_MARKERS {
            if let Some(rem) = lower.strip_prefix(marker) {
                if rem.is_empty() {
                    return "";
                }
                let first = rem.chars().next().unwrap();
                if first.is_whitespace() || first.is_ascii_punctuation() {
                    let mut trimmed = rem;
                    while let Some(c) = trimmed.chars().next() {
                        if c.is_whitespace() || c.is_ascii_punctuation() {
                            trimmed = &trimmed[c.len_utf8()..];
                        } else {
                            break;
                        }
                    }
                    lower = trimmed;
                    stripped = true;
                    break;
                }
            }
        }
        if !stripped {
            break;
        }
    }
    lower
}

fn has_anaphoric_follow_up_cue(lower: &str) -> bool {
    const ANAPHORIC_PHRASES: &[&str] = &[
        "its first",
        "its second",
        "its third",
        "the first ability",
        "the second ability",
        "the third ability",
        "first ability",
        "second ability",
        "third ability",
        "chapter one",
        "chapter two",
        "chapter three",
        "chapter 1",
        "chapter 2",
        "chapter 3",
        "does that trigger",
        "does it trigger",
        "when it enters",
        "when it leaves",
        "when it dies",
        "if it's",
        "if it is",
        "while it's",
        "while it is",
        "on the stack",
        "in play",
        "on the battlefield",
    ];
    ANAPHORIC_PHRASES
        .iter()
        .any(|phrase| lower.contains(phrase))
}

const ELLIPTICAL_PREFIXES: &[&str] = &[
    "what about",
    "and what about",
    "how about",
    "and how about",
    "what if",
    "and if",
    "if i",
    "if it",
    "if they",
    "if there",
    "if you",
    "and what",
    "what was the",
    "what is its",
    "what are its",
    "what does it",
    "what do they",
    "what happens when",
    "what happens if",
    "what happens",
    "are there any",
    "is there any",
    "why has it",
    "why is it",
    "why does it",
    "why did it",
    "how does that",
    "how does it",
    "does the",
    "does that",
    "does it",
    "do the",
    "do they",
    "can the",
    "can i",
    "can it",
    "can that",
    "could it",
    "could that",
    "should i",
    "should it",
    "which one",
    "which of",
    "so if",
    "so does",
    "so is",
    "even if",
    "or if",
    "and does",
    "and can",
    "and is",
    "would that",
    "would it",
    "will that",
    "will it",
    "is that",
    "is it",
    "when it",
    "when that",
    "on chapter",
    "for the first",
    "for the second",
    "for the third",
    "that still",
    "now modify",
    "now change",
    "now find",
    "now make",
    "do we need",
];

const SMART_KEYWORDS: &[&str] = &[
    "rust",
    "python",
    "javascript",
    "typescript",
    "c++",
    "golang",
    "function",
    "fn ",
    "def ",
    "class ",
    "struct ",
    "compile",
    "compiler",
    "error",
    "exception",
    "panic",
    "debug",
    "bug",
    "traceback",
    "stack trace",
    "regex",
    "algorithm",
    "complexity",
    "big o",
    "sql",
    "database schema",
    "architecture",
    "refactor",
    "equation",
    "derivative",
    "integral",
    "solve for",
    "calculus",
    "linear algebra",
    "proof",
    "matrix multiplication",
];

fn starts_with_elliptical_prefix(core_lower: &str) -> bool {
    ELLIPTICAL_PREFIXES.iter().any(|prefix| {
        if let Some(rem) = core_lower.strip_prefix(prefix) {
            rem.is_empty()
                || rem.chars().next().unwrap().is_whitespace()
                || rem.chars().next().unwrap().is_ascii_punctuation()
        } else {
            false
        }
    })
}

fn has_interrogative_lead(core_lower: &str) -> bool {
    const INTERROGATIVE_STARTS: &[&str] = &[
        "what ", "who ", "where ", "when ", "which ", "why ", "how ", "does ", "did ", "do ",
        "is ", "are ", "was ", "were ", "can ", "could ", "should ", "would ", "will ", "compare ",
    ];
    INTERROGATIVE_STARTS
        .iter()
        .any(|prefix| core_lower.starts_with(prefix))
}

fn is_self_identity(lower: &str) -> bool {
    let trimmed = lower.trim_start();
    trimmed.starts_with("what model are you") || trimmed.starts_with("who are you")
}

fn has_referential_continuation_cue(lower: &str) -> bool {
    if lower.contains("the ") {
        return true;
    }
    const REFERENTIAL_WORDS: &[&str] = &[
        "the", "that", "this", "it", "its", "they", "their", "them", "instead", "too", "also",
        "either", "both", "first", "second", "third", "other", "same",
    ];
    lower
        .split(|c: char| !c.is_alphanumeric())
        .any(|word| REFERENTIAL_WORDS.contains(&word))
}

fn strip_sources_footer(text: &str) -> &str {
    match text.split_once("\n\nSources:\n") {
        Some((before, _)) => before.trim(),
        None => text.trim(),
    }
}

fn is_elliptical_or_anaphoric_turn(text: &str) -> bool {
    let lower = strip_leading_fillers(text.trim()).to_lowercase();
    let core_lower = strip_leading_discourse_markers(&lower);
    starts_with_elliptical_prefix(core_lower)
        || (text.len() <= 160 && has_anaphoric_follow_up_cue(&lower))
}

pub fn fold_user_history<'a>(
    turns: impl IntoIterator<Item = &'a str>,
    current_prompt: &str,
) -> Option<String> {
    let current_clean = strip_sources_footer(current_prompt);
    let mut prior: Vec<&str> = turns
        .into_iter()
        .map(strip_sources_footer)
        .filter(|t| !t.is_empty())
        .collect();
    if prior
        .last()
        .is_some_and(|last| !current_clean.is_empty() && *last == current_clean)
    {
        prior.pop();
    }
    match prior.len() {
        0 => None,
        1 => Some(truncate_utf8(prior[0], 200)),
        _ => {
            let prev = *prior.last().unwrap();
            let window_start = prior.len().saturating_sub(3);
            let anchor = prior[..prior.len() - 1]
                .iter()
                .rev()
                .copied()
                .find(|t| !is_elliptical_or_anaphoric_turn(t))
                .unwrap_or(prior[window_start]);
            if anchor == prev {
                Some(truncate_utf8(prev, 200))
            } else {
                Some(format!(
                    "{} {}",
                    truncate_utf8(anchor, 120),
                    truncate_utf8(prev, 100)
                ))
            }
        }
    }
}

fn apply_elliptical_follow_up(text: &str, prev_user_turn: Option<&str>) -> String {
    let Some(prev) = prev_user_turn else {
        return text.to_string();
    };
    let prev_trimmed = prev.trim();
    if prev_trimmed.is_empty() {
        return text.to_string();
    }

    let lower = text.to_lowercase();

    const DE_ESCALATING_PHRASES: &[&str] = &[
        "summarize",
        "summarise",
        "translate",
        "rewrite",
        "shorter",
        "make it shorter",
        "shorter version",
        "thanks",
        "thank you",
        "awesome",
    ];

    if DE_ESCALATING_PHRASES
        .iter()
        .any(|phrase| lower.contains(phrase))
    {
        return text.to_string();
    }

    let core_lower = strip_leading_discourse_markers(&lower);
    let starts_elliptical = starts_with_elliptical_prefix(core_lower);
    let is_short_anaphoric = text.len() <= 160 && has_anaphoric_follow_up_cue(&lower);
    let is_contextual_search_follow_up = !starts_elliptical
        && !is_short_anaphoric
        && text.len() <= 140
        && (text.contains('?') || has_interrogative_lead(core_lower))
        && has_referential_continuation_cue(&lower)
        && !SMART_KEYWORDS.iter().any(|k| lower.contains(k))
        && !is_self_identity(&lower)
        && !is_pure_arithmetic_or_greeting(&lower)
        && !classify_lexical_fallback(text).needs_search()
        && classify_lexical_fallback(prev_trimmed).needs_search();

    if !starts_elliptical && !is_short_anaphoric && !is_contextual_search_follow_up {
        return text.to_string();
    }

    format!("{} {}", truncate_utf8(prev_trimmed, 200), text)
}

fn isolate_long_paste_instruction(t: &str) -> String {
    if t.len() <= 260 {
        return t.to_string();
    }

    if t.contains("\n\n") {
        let parts: Vec<&str> = t
            .split("\n\n")
            .map(str::trim)
            .filter(|part| !part.is_empty())
            .collect();
        if parts.len() >= 2 && parts.last().unwrap().len() < 260 {
            return parts.last().unwrap().to_string();
        }
    }

    if let Some(first_nl) = t.find('\n') {
        if (15..160).contains(&first_nl) {
            return t[..first_nl].trim().to_string();
        }
    }

    format!("{} ... {}", truncate_utf8(t, 110), tail_utf8(t, 140))
}

pub fn normalize_for_routing(prompt: &str, prev_user_turn: Option<&str>) -> String {
    let t = strip_leading_fillers(prompt.trim());

    if match_prefix_colon_supplied_text(t) {
        return CANONICAL_SUPPLIED_TEXT_INTENT.to_string();
    }

    if match_tail_supplied_text_instruction(t) {
        return CANONICAL_SUPPLIED_TEXT_INTENT.to_string();
    }

    if t.len() > 260 {
        return isolate_long_paste_instruction(t);
    }

    apply_elliptical_follow_up(t, prev_user_turn)
}

pub fn classify_bge_small_embedding(
    embedding: &[f32],
    normalized_input: String,
) -> Option<RoutingDecision> {
    if embedding.len() != BGE_SMALL_EMBEDDING_DIM {
        return None;
    }
    let sum_sq: f32 = embedding.iter().map(|&x| x * x).sum();
    let norm = sum_sq.sqrt();
    if !norm.is_finite() || norm <= 1e-12 {
        return None;
    }
    let inv_norm = 1.0 / norm;

    let mut logits = [0.0_f32; 4];
    for (r, row) in BGE_SMALL_EN_V15_RIDGE_W.iter().enumerate() {
        let val = embedding[r] * inv_norm;
        logits[0] += val * row[0];
        logits[1] += val * row[1];
        logits[2] += val * row[2];
        logits[3] += val * row[3];
    }

    if logits.iter().any(|l| !l.is_finite()) {
        return None;
    }

    let mut indices = [0, 1, 2, 3];
    indices.sort_by(|&a, &b| {
        logits[b]
            .partial_cmp(&logits[a])
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let top1 = indices[0];
    let top2 = indices[1];
    let primary = Route2D::ALL[top1];
    let secondary = Route2D::ALL[top2];
    let margin = logits[top1] - logits[top2];
    let parallel_fast_and_smart =
        margin < PARALLEL_MARGIN_THRESHOLD || primary == Route2D::ResearchSmart;

    Some(RoutingDecision {
        primary,
        secondary,
        margin,
        parallel_fast_and_smart,
        normalized_input,
    })
}

fn is_pure_arithmetic_or_greeting(lower: &str) -> bool {
    let trimmed = lower.trim_matches(|c: char| c.is_whitespace() || c.is_ascii_punctuation());
    if matches!(
        trimmed,
        "hi" | "hello"
            | "hey"
            | "yo"
            | "thanks"
            | "thank you"
            | "good morning"
            | "good afternoon"
            | "good evening"
            | "how are you"
    ) {
        return true;
    }

    let has_digits = lower.chars().any(|c| c.is_ascii_digit());
    if !has_digits {
        return false;
    }
    let has_math_op = lower.contains('+')
        || lower.contains('*')
        || lower.contains(" + ")
        || lower.contains(" - ")
        || lower.contains(" / ")
        || lower.contains(" times ")
        || lower.contains(" plus ")
        || lower.contains(" minus ")
        || lower.contains(" multiplied by ")
        || lower.contains(" divided by ");
    if !has_math_op {
        return false;
    }
    let has_non_math_topic = [
        "stock", "price", "weather", "year", "version", "release", "magic", "chapter", "level",
        "ability", "damage", "mana", "card", "rule",
    ]
    .iter()
    .any(|w| lower.contains(w));
    !has_non_math_topic
}

fn has_structural_factual_or_entity_signal(normalized: &str, lower: &str) -> bool {
    let trimmed = normalized.trim();
    if trimmed.is_empty() {
        return false;
    }
    let core_lower = strip_leading_discourse_markers(lower);
    if (core_lower.starts_with("what about ") || core_lower.starts_with("how about "))
        && trimmed.split_whitespace().count() <= 3
    {
        return false;
    }
    let is_question = trimmed.contains('?') || has_interrogative_lead(core_lower);
    if !is_question {
        return false;
    }

    // Check for proper nouns (capitalized words after the first word of a sentence)
    // or quoted entity names.
    let has_quoted_entity = trimmed.contains('"') || trimmed.contains('\u{201c}');
    let mut has_proper_noun = false;
    for (idx, raw_word) in trimmed.split_whitespace().enumerate() {
        if idx == 0 {
            continue;
        }
        let clean = raw_word.trim_matches(|c: char| !c.is_alphanumeric());
        if clean.len() >= 2 && clean != "I" && clean != "I'm" && clean != "I've" {
            if let Some(first) = clean.chars().next() {
                if first.is_ascii_uppercase() {
                    has_proper_noun = true;
                    break;
                }
            }
        }
    }

    has_proper_noun || has_quoted_entity
}

pub fn classify_lexical_fallback(normalized: &str) -> RoutingDecision {
    if normalized == CANONICAL_SUPPLIED_TEXT_INTENT {
        return RoutingDecision {
            primary: Route2D::DirectFast,
            secondary: Route2D::DirectSmart,
            margin: 1.0,
            parallel_fast_and_smart: false,
            normalized_input: normalized.to_string(),
        };
    }

    let lower = normalized.to_lowercase();

    // 1. Multi-source comparison / research keywords -> ResearchSmart
    const RESEARCH_KEYWORDS: &[&str] = &[
        "compare and contrast",
        "deep dive",
        "comprehensive comparison",
        "comprehensive overview",
        "literature review",
        "state of the art",
        "research the",
        "in-depth analysis",
        "pros and cons of",
        "differences between",
    ];
    if RESEARCH_KEYWORDS.iter().any(|k| lower.contains(k)) {
        return RoutingDecision {
            primary: Route2D::ResearchSmart,
            secondary: Route2D::SearchFast,
            margin: 0.25,
            parallel_fast_and_smart: true,
            normalized_input: normalized.to_string(),
        };
    }

    // 2. Self-referential identity questions & pure arithmetic / greetings -> DirectFast
    if is_self_identity(&lower) || is_pure_arithmetic_or_greeting(&lower) {
        return RoutingDecision {
            primary: Route2D::DirectFast,
            secondary: Route2D::DirectSmart,
            margin: 0.50,
            parallel_fast_and_smart: false,
            normalized_input: normalized.to_string(),
        };
    }

    // 3. Explicit live/temporal/lookup & domain/game-rules markers -> SearchFast
    // (checked BEFORE SMART_KEYWORDS so technical or rules queries with "stack",
    // "function", "class", or live/temporal intent still trigger search).
    const LIVE_AND_RULES_OVERRIDE_KEYWORDS: &[&str] = &[
        "search",
        "google",
        "look up",
        "lookup",
        "weather",
        "forecast",
        "temperature",
        "news",
        "headline",
        "headlines",
        "stock",
        "price",
        "score",
        "scores",
        "standings",
        "who won",
        "today",
        "today's",
        "tonight",
        "yesterday",
        "tomorrow",
        "latest",
        "current",
        "currently",
        "right now",
        "recent",
        "recently",
        "2025",
        "2026",
        "2027",
        "release date",
        "latest version",
        "current version",
        "outage",
        "check online",
        "search online",
        "official docs",
        "officially",
        "unofficially",
        // Game rules, TCG (Magic: The Gathering, etc.), tabletop RPGs, and card/ability interactions:
        "magic: the gathering",
        "magic the gathering",
        "in magic",
        "mtg",
        "dungeons & dragons",
        "dungeons and dragons",
        "d&d",
        "dnd",
        "pathfinder",
        "warhammer",
        "yu-gi-oh",
        "yugioh",
        "lorcana",
        "flesh and blood",
        "scryfall",
        "gatherer",
        "oracle text",
        "comprehensive rules",
        "state-based",
        "mana value",
        "converted mana cost",
        "planeswalker",
        "battlefield",
        "graveyard",
        "command zone",
        "commander",
        "combat damage",
        "trample",
        "deathtouch",
        "lifelink",
        "hexproof",
        "indestructible",
        "flashback",
        "prowess",
        "cascade",
        "saga",
        "lore counter",
        "chapter one",
        "chapter two",
        "chapter three",
        "chapter 1",
        "chapter 2",
        "chapter 3",
        "triggered ability",
        "activated ability",
        "replacement effect",
        "continuous effect",
        "first ability",
        "second ability",
        "third ability",
        "saving throw",
        "spell slot",
        "cantrip",
        "armor class",
        "sneak attack",
        "opportunity attack",
        "banlist",
        "ruling",
        "rulings",
        "interact with",
        "interacts with",
        "interaction between",
        "what happens if i cast",
        "what happens when i cast",
        "what happens if you cast",
        "does that trigger",
        "does it trigger",
        "when it enters",
    ];
    if LIVE_AND_RULES_OVERRIDE_KEYWORDS
        .iter()
        .any(|k| lower.contains(k))
    {
        return RoutingDecision {
            primary: Route2D::SearchFast,
            secondary: Route2D::DirectFast,
            margin: 0.35,
            parallel_fast_and_smart: false,
            normalized_input: normalized.to_string(),
        };
    }

    // 4. Coding / math / debugging / architecture keywords -> DirectSmart
    if SMART_KEYWORDS.iter().any(|k| lower.contains(k)) {
        return RoutingDecision {
            primary: Route2D::DirectSmart,
            secondary: Route2D::DirectFast,
            margin: 0.35,
            parallel_fast_and_smart: false,
            normalized_input: normalized.to_string(),
        };
    }

    // 5. General factual / world-knowledge / lookup keywords & structural entity questions -> SearchFast
    const SEARCH_KEYWORDS: &[&str] = &[
        "search",
        "google",
        "look up",
        "lookup",
        "weather",
        "forecast",
        "temperature",
        "news",
        "headline",
        "headlines",
        "stock",
        "stock price",
        "stock of",
        "crypto price",
        "trading at",
        "market cap",
        "price",
        "score",
        "scores",
        "standings",
        "who won",
        "latest score",
        "game score",
        "today",
        "today's",
        "tonight",
        "yesterday",
        "tomorrow",
        "latest",
        "current",
        "currently",
        "right now",
        "recent",
        "recently",
        "2025",
        "2026",
        "2027",
        "schedule",
        "release",
        "release date of",
        "version",
        "outage",
        "ceo of",
        "current ceo",
        "president of",
        "prime minister",
        "current population",
        "who is the current",
        "what is the current",
        "when does",
        "when will",
        "how much is",
        "what time is it",
        "who is",
        "who was",
        "who directed",
        "who wrote",
        "who invented",
        "who founded",
        "what is the",
        "what are the",
        "what was the",
        "what happens",
        "what happened",
        "what does",
        "how does",
        "when is",
        "when did",
        "when was",
        "where is",
        "where was",
        "where did",
        "how much",
        "how many",
        "official",
        "unofficial",
        "community",
        "online",
        "internet",
        "reddit",
        "github",
        "docs",
        "documentation",
        "specs",
        "specifications",
        "update",
        "status",
        "worth",
        "cost",
        "rate",
        "winner",
        "champion",
        "leader",
        "minister",
        "governor",
        "mayor",
        "population",
        "capital of",
        "distance",
        "calories",
        "recipe",
        "ingredients",
        "symptoms",
        "treatment",
        "law",
        "policy",
        "ability",
        "abilities",
        "trigger",
        "triggers",
        "interact",
        "interaction",
    ];
    if SEARCH_KEYWORDS.iter().any(|k| lower.contains(k))
        || has_structural_factual_or_entity_signal(normalized, &lower)
    {
        return RoutingDecision {
            primary: Route2D::SearchFast,
            secondary: Route2D::DirectFast,
            margin: 0.35,
            parallel_fast_and_smart: false,
            normalized_input: normalized.to_string(),
        };
    }

    // 6. Casual / arithmetic / conversion / default -> DirectFast
    RoutingDecision {
        primary: Route2D::DirectFast,
        secondary: Route2D::DirectSmart,
        margin: 0.50,
        parallel_fast_and_smart: false,
        normalized_input: normalized.to_string(),
    }
}

pub fn classify_prompt_2d(
    prompt: &str,
    prev_user_turn: Option<&str>,
    bge_embedding: Option<&[f32]>,
) -> RoutingDecision {
    let normalized = normalize_for_routing(prompt, prev_user_turn);
    if let Some(embedding) = bge_embedding {
        if let Some(decision) = classify_bge_small_embedding(embedding, normalized.clone()) {
            return decision;
        }
    }
    classify_lexical_fallback(&normalized)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EscalationReason {
    EmptySources,
    StubOrPaywall,
    MissingNumericFact,
    ReaderAbstentionOrConflict,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SnippetSufficiency {
    Sufficient,
    NeedsEscalation(EscalationReason),
}

pub fn check_search_snippets_sufficiency(query: &str, sources: &[Source]) -> SnippetSufficiency {
    if !sources.iter().any(|s| has_readable_text(&s.text)) {
        return SnippetSufficiency::NeedsEscalation(EscalationReason::EmptySources);
    }

    let combined: String = sources
        .iter()
        .map(|s| s.text.to_lowercase())
        .collect::<Vec<_>>()
        .join(" ");

    let readable_chars = combined
        .chars()
        .filter(|c| !c.is_whitespace() && !c.is_control())
        .count();

    const STUB_PHRASES: &[&str] = &[
        "click to view full",
        "see full chart on page",
        "subscribe to read",
        "sign in to view",
        "enable javascript to view",
    ];

    let has_stub_phrase = STUB_PHRASES.iter().any(|phrase| combined.contains(phrase));
    if readable_chars < 120 && has_stub_phrase {
        return SnippetSufficiency::NeedsEscalation(EscalationReason::StubOrPaywall);
    }

    if (combined.contains("click to view full") || combined.contains("see full chart on page"))
        && !combined.chars().any(|c| c.is_ascii_digit())
    {
        return SnippetSufficiency::NeedsEscalation(EscalationReason::StubOrPaywall);
    }

    const QUANTITATIVE_PHRASES: &[&str] = &[
        "how much",
        "what price",
        "stock price",
        "trading for",
        "market cap",
        "exchange rate",
        "cash rate",
        "interest rate",
        "score",
        "what time",
        "temperature",
        "air quality",
        "snow depth",
        "dividend",
        "per ounce",
        "per kwh",
    ];

    let query_lower = query.to_lowercase();
    let is_verification = query_lower.contains("is it true")
        || query_lower.contains("fact check")
        || query_lower.contains("fact-check")
        || query_lower.contains("verify whether")
        || query_lower.contains("verify if")
        || (query_lower.contains("did ") && query_lower.contains(" really "));
    if is_verification {
        return SnippetSufficiency::NeedsEscalation(EscalationReason::StubOrPaywall);
    }

    let is_quantitative = QUANTITATIVE_PHRASES.iter().any(|q| query_lower.contains(q));
    if is_quantitative && !combined.chars().any(|c| c.is_ascii_digit()) {
        return SnippetSufficiency::NeedsEscalation(EscalationReason::MissingNumericFact);
    }

    SnippetSufficiency::Sufficient
}

pub fn is_natural_abstention(reader_output: &str) -> bool {
    const ABSTENTION_PHRASES: &[&str] = &[
        "not provided in the",
        "not mentioned in the",
        "snippets do not",
        "search results do not",
        "cannot determine from",
        "does not specify",
        "conflicting",
        "contradict",
    ];
    let lower = reader_output.to_lowercase();
    ABSTENTION_PHRASES
        .iter()
        .any(|phrase| lower.contains(phrase))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn route2d_helpers_and_effective_search_mode() {
        assert_eq!(Route2D::DirectFast.code(), "F");
        assert_eq!(Route2D::SearchFast.code(), "S");
        assert_eq!(Route2D::DirectSmart.code(), "C");
        assert_eq!(Route2D::ResearchSmart.code(), "R");

        assert!(!Route2D::DirectFast.needs_search());
        assert!(Route2D::SearchFast.needs_search());
        assert!(!Route2D::DirectSmart.needs_search());
        assert!(Route2D::ResearchSmart.needs_search());

        assert!(!Route2D::DirectFast.needs_smart());
        assert!(!Route2D::SearchFast.needs_smart());
        assert!(Route2D::DirectSmart.needs_smart());
        assert!(Route2D::ResearchSmart.needs_smart());

        // When requested is not Quick (e.g., Verify or Research), preserve requested mode
        assert_eq!(
            Route2D::DirectFast.effective_search_mode(Mode::Verify),
            Some(Mode::Verify)
        );
        assert_eq!(
            Route2D::SearchFast.effective_search_mode(Mode::Research),
            Some(Mode::Research)
        );

        // When requested is Quick:
        assert_eq!(Route2D::DirectFast.effective_search_mode(Mode::Quick), None);
        assert_eq!(
            Route2D::DirectSmart.effective_search_mode(Mode::Quick),
            None
        );
        assert_eq!(
            Route2D::SearchFast.effective_search_mode(Mode::Quick),
            Some(Mode::Quick)
        );
        assert_eq!(
            Route2D::ResearchSmart.effective_search_mode(Mode::Quick),
            Some(Mode::Research)
        );
    }

    #[test]
    fn normalizer_strips_leading_fillers() {
        assert_eq!(
            normalize_for_routing("   um, uh like what is the capital?", None),
            "what is the capital?"
        );
        assert_eq!(
            normalize_for_routing("umbrella weather forecast", None),
            "umbrella weather forecast"
        );
        assert_eq!(
            normalize_for_routing("er: what is 2+2?", None),
            "what is 2+2?"
        );
    }

    #[test]
    fn normalizer_handles_prefix_colon_supplied_text() {
        assert_eq!(
            normalize_for_routing("Translate: Hello world!", None),
            CANONICAL_SUPPLIED_TEXT_INTENT
        );
        assert_eq!(
            normalize_for_routing(
                "Hey, can you please summarize: The quick brown fox jumps over the lazy dog.",
                None
            ),
            CANONICAL_SUPPLIED_TEXT_INTENT
        );
        assert_eq!(
            normalize_for_routing("Fix typos: teh quick brwon fxo", None),
            CANONICAL_SUPPLIED_TEXT_INTENT
        );
        // Empty payload should NOT match
        assert_ne!(
            normalize_for_routing("Translate:   ", None),
            CANONICAL_SUPPLIED_TEXT_INTENT
        );
        // Colon after 80 bytes should NOT match
        let long_prefix = "a".repeat(85) + ": hello";
        assert_ne!(
            normalize_for_routing(&long_prefix, None),
            CANONICAL_SUPPLIED_TEXT_INTENT
        );
    }

    #[test]
    fn normalizer_handles_tail_supplied_text_on_long_paste() {
        let long_body = "The US economy recorded moderate growth in the final quarter of the year, driven by consumer spending and mining exports. Unemployment remained steady while inflation eased slightly toward target bands. ";
        let prompt = format!("{long_body}Please summarize the above article.");
        assert!(prompt.len() > 180);
        assert_eq!(
            normalize_for_routing(&prompt, None),
            CANONICAL_SUPPLIED_TEXT_INTENT
        );

        let prompt2 = format!("{long_body}Translate this passage into Spanish.");
        assert_eq!(
            normalize_for_routing(&prompt2, None),
            CANONICAL_SUPPLIED_TEXT_INTENT
        );
    }

    #[test]
    fn normalizer_handles_multi_turn_elliptical_follow_up() {
        assert_eq!(
            normalize_for_routing("What about Nvidia?", Some("Tell me about Apple stock.")),
            "Tell me about Apple stock. What about Nvidia?"
        );
        assert_eq!(
            normalize_for_routing("How about Tesla?", Some("Compare Microsoft and Google")),
            "Compare Microsoft and Google How about Tesla?"
        );

        // De-escalating phrase prevents prepend
        assert_eq!(
            normalize_for_routing(
                "What about Nvidia? Can you summarize that in one short sentence?",
                Some("Tell me about Apple stock.")
            ),
            "What about Nvidia? Can you summarize that in one short sentence?"
        );
        assert_eq!(
            normalize_for_routing(
                "What about Nvidia? Shorter please.",
                Some("Tell me about Apple stock.")
            ),
            "What about Nvidia? Shorter please."
        );
    }

    #[test]
    fn normalizer_handles_long_paste_instruction_isolation() {
        // Multi-paragraph with short instruction at the end
        let p1 = "A".repeat(150);
        let p2 = "B".repeat(150);
        let instruction = "Extract the three main themes.";
        let text = format!("{p1}\n\n{p2}\n\n{instruction}");
        assert!(text.len() > 260);
        assert_eq!(normalize_for_routing(&text, None), instruction);

        // First newline in 15..160
        let heading = "Log snippet analysis";
        let body = "X".repeat(300);
        let text2 = format!("{heading}\n{body}");
        assert!(text2.len() > 260);
        assert_eq!(normalize_for_routing(&text2, None), heading);

        // Single paragraph with no newline: truncate head ... tail
        let text3 = "C".repeat(300);
        let normalized = normalize_for_routing(&text3, None);
        assert!(normalized.contains(" ... "));
        assert_eq!(normalized.len(), 110 + 5 + 140);
    }

    #[test]
    fn classify_bge_small_embedding_validates_and_computes() {
        // Invalid length
        assert!(classify_bge_small_embedding(&[1.0; 100], "test".into()).is_none());

        // Zero / subnormal norm
        assert!(
            classify_bge_small_embedding(&[0.0; BGE_SMALL_EMBEDDING_DIM], "test".into()).is_none()
        );

        // Valid mock embedding
        let mut emb = [0.0_f32; BGE_SMALL_EMBEDDING_DIM];
        emb[0] = 1.0;
        let decision = classify_bge_small_embedding(&emb, "test".into()).expect("classification");
        assert!(Route2D::ALL.contains(&decision.primary));
        assert!(Route2D::ALL.contains(&decision.secondary));
        assert!(decision.margin >= 0.0);
        assert_eq!(decision.normalized_input, "test");
    }

    #[test]
    fn classify_lexical_fallback_routes_properly() {
        let supplied = classify_lexical_fallback(CANONICAL_SUPPLIED_TEXT_INTENT);
        assert_eq!(supplied.primary, Route2D::DirectFast);
        assert!(!supplied.parallel_fast_and_smart);

        let coding =
            classify_lexical_fallback("how do I write a rust macro with pattern matching?");
        assert_eq!(coding.primary, Route2D::DirectSmart);
        assert!(coding.needs_smart());
        assert!(!coding.needs_search());

        let live_rust = classify_lexical_fallback("What is the latest version of Rust in 2026?");
        assert_eq!(live_rust.primary, Route2D::SearchFast);
        assert!(live_rust.needs_search());

        let live_exa = classify_lexical_fallback(
            "double check online how exa works officially and what community can do unofficially",
        );
        assert_eq!(live_exa.primary, Route2D::SearchFast);
        assert!(live_exa.needs_search());

        let research =
            classify_lexical_fallback("compare and contrast react vs vue for large teams");
        assert_eq!(research.primary, Route2D::ResearchSmart);
        assert!(research.parallel_fast_and_smart);
        assert!(research.needs_smart());
        assert!(research.needs_search());

        let search =
            classify_lexical_fallback("what is the current weather forecast in Chicago?");
        assert_eq!(search.primary, Route2D::SearchFast);
        assert!(search.needs_search());
        assert!(!search.needs_smart());

        let factual_pm = classify_lexical_fallback("Who is the President of the United States?");
        assert_eq!(factual_pm.primary, Route2D::SearchFast);
        assert!(factual_pm.needs_search());

        let factual_specs = classify_lexical_fallback("What are the specs of the Pixel 8?");
        assert_eq!(factual_specs.primary, Route2D::SearchFast);
        assert!(factual_specs.needs_search());

        let casual = classify_lexical_fallback("hello! what is 12 times 15?");
        assert_eq!(casual.primary, Route2D::DirectFast);
        assert!(!casual.needs_search());
        assert!(!casual.needs_smart());

        let identity = classify_lexical_fallback("What model are you?");
        assert_eq!(identity.primary, Route2D::DirectFast);
        assert!(!identity.needs_search());
    }

    #[test]
    fn classify_mtg_rules_and_multi_turn_follow_ups_route_to_search() {
        for query in [
            "In Magic: The Gathering, what happens if I cast Blood Moon while Urza's Saga is on the battlefield?",
            "How does Dress Down interact with Urza's Saga?",
            "What happens when the third lore counter is placed on a Saga?",
            "Does that trigger prowess?",
            "In D&D 5e, can a Rogue use Sneak Attack on an opportunity attack?",
            "Who directed Oppenheimer?",
        ] {
            let decision = classify_prompt_2d(query, None, None);
            assert!(
                decision.needs_search(),
                "expected needs_search() == true for query: {query}, got {:?}",
                decision.primary
            );
        }

        let prev_mtg = "In Magic: The Gathering, how does Urza's Saga work with Blood Moon?";
        for follow_up in [
            "Wait what about its second ability?",
            "And if I have Dress Down in play?",
            "What if it's on chapter two?",
            "Does that trigger constellation?",
        ] {
            let decision = classify_prompt_2d(follow_up, Some(prev_mtg), None);
            assert!(
                decision.needs_search(),
                "expected needs_search() == true for follow-up: {follow_up}, got {:?}",
                decision.primary
            );
            assert!(
                decision.normalized_input.contains("Magic: The Gathering"),
                "expected normalized_input to inherit previous turn context for: {follow_up}, got: {}",
                decision.normalized_input
            );
        }
    }

    #[test]
    fn fold_user_history_preserves_turn_one_anchor_across_three_turns() {
        let turn1 = "In Magic: The Gathering, how does Urza's Saga work?\n\nSources:\n- https://example.com/urza";
        let turn2 = "What about its second ability?";
        for turn3 in [
            "And what if it gets destroyed?",
            "Does the token keep its ability?",
            "What happens if it loses all abilities before chapter three?",
        ] {
            let folded = fold_user_history([turn1, turn2, turn3], turn3)
                .expect("folded prior user history should be present");
            assert!(
                folded.contains("Urza's Saga"),
                "expected folded history to preserve Turn 1 anchor, got: {folded}"
            );
            assert!(
                !folded.contains("Sources:"),
                "expected folded history to strip Sources footer, got: {folded}"
            );
            let decision = classify_prompt_2d(turn3, Some(&folded), None);
            assert_eq!(
                decision.primary,
                Route2D::SearchFast,
                "expected SearchFast for Turn 3 {turn3}, got {:?}",
                decision.primary
            );
            assert!(decision.needs_search());
            assert!(
                decision.normalized_input.contains("Urza's Saga"),
                "expected normalized_input to preserve Urza's Saga for Turn 3 {turn3}, got: {}",
                decision.normalized_input
            );
        }
    }

    #[test]
    fn classify_prompt_2d_falls_back_when_no_embedding() {
        let dec = classify_prompt_2d("how to compile rust code?", None, None);
        assert_eq!(dec.primary, Route2D::DirectSmart);
    }

    #[test]
    fn snippet_sufficiency_and_escalation() {
        fn make_source(text: &str) -> Source {
            Source {
                id: "test".into(),
                url: "https://example.com".into(),
                title: "Test".into(),
                provider: crate::search::Provider::Parallel,
                kind: crate::search::EvidenceKind::SearchExcerpt,
                text: text.into(),
                retrieved_unix_ms: 0,
                publisher_date: None,
                truncated: false,
            }
        }

        // Empty sources
        assert_eq!(
            check_search_snippets_sufficiency("query", &[]),
            SnippetSufficiency::NeedsEscalation(EscalationReason::EmptySources)
        );
        assert_eq!(
            check_search_snippets_sufficiency("query", &[make_source("   \n\t  ")]),
            SnippetSufficiency::NeedsEscalation(EscalationReason::EmptySources)
        );

        // Stub / paywall with <120 chars
        assert_eq!(
            check_search_snippets_sufficiency(
                "query",
                &[make_source(
                    "Short preview. Subscribe to read full article."
                )]
            ),
            SnippetSufficiency::NeedsEscalation(EscalationReason::StubOrPaywall)
        );

        // Click to view full without digits
        assert_eq!(
            check_search_snippets_sufficiency(
                "query",
                &[make_source(
                    "Here is an overview of the topic. Click to view full chart on our site."
                )]
            ),
            SnippetSufficiency::NeedsEscalation(EscalationReason::StubOrPaywall)
        );

        // Quantitative query missing numeric facts
        assert_eq!(
            check_search_snippets_sufficiency(
                "what is the stock price of Apple?",
                &[make_source(
                    "Apple stock traded today on the Nasdaq exchange during regular market hours."
                )]
            ),
            SnippetSufficiency::NeedsEscalation(EscalationReason::MissingNumericFact)
        );

        // Quantitative query with numeric fact
        assert_eq!(
            check_search_snippets_sufficiency(
                "what is the stock price of Apple?",
                &[make_source(
                    "Apple stock traded today at $235.40 on the Nasdaq exchange."
                )]
            ),
            SnippetSufficiency::Sufficient
        );

        // Qualitative query with sufficient text
        assert_eq!(
            check_search_snippets_sufficiency(
                "history of the Eiffel Tower",
                &[make_source("The Eiffel Tower was constructed from 1887 to 1889 as the centerpiece of the 1889 World's Fair in Paris.")]
            ),
            SnippetSufficiency::Sufficient
        );

        // Explicit verification / fact-check queries escalate to web_fetch
        for verify_query in [
            "Is it true that the Eiffel Tower was built in 1889?",
            "Fact check whether the company announced a buyout",
            "Please fact-check this claim about solar efficiency",
            "Verify whether the API supports WebSockets",
            "Verify if the release happened in 2026",
            "Did Napoleon really ban that book in 1805?",
        ] {
            assert_eq!(
                check_search_snippets_sufficiency(
                    verify_query,
                    &[make_source("The Eiffel Tower was constructed from 1887 to 1889 as the centerpiece of the 1889 World's Fair in Paris.")]
                ),
                SnippetSufficiency::NeedsEscalation(EscalationReason::StubOrPaywall),
                "query should escalate: {verify_query}"
            );
        }
    }

    #[test]
    fn natural_abstention_detection() {
        assert!(is_natural_abstention(
            "The exact release date is not provided in the search results."
        ));
        assert!(is_natural_abstention(
            "The snippets do not mention the CEO's salary."
        ));
        assert!(is_natural_abstention(
            "I cannot determine from the sources provided."
        ));
        assert!(is_natural_abstention(
            "The sources have conflicting statements."
        ));
        assert!(!is_natural_abstention(
            "The Eiffel Tower was built in 1889."
        ));
    }
}
