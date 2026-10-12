use serde::{Deserialize, Serialize};
use std::time::Duration;

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    Quick,
    Verify,
    Research,
}

#[derive(Clone, Copy, Debug)]
pub struct Policy {
    pub searches: usize,
    pub reads: usize,
    pub rounds: usize,
    pub tool_time: Duration,
    pub evidence_bytes: usize,
}

impl Mode {
    pub fn policy(self) -> Policy {
        let (searches, reads, rounds, seconds, evidence_bytes) = match self {
            Self::Quick => (2, 2, 4, 20, 24 * 1024),
            Self::Verify => (4, 4, 6, 45, 48 * 1024),
            Self::Research => (8, 8, 10, 90, 96 * 1024),
        };
        Policy {
            searches,
            reads,
            rounds,
            tool_time: Duration::from_secs(seconds),
            evidence_bytes,
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Provider {
    Parallel,
    Exa,
}

impl Provider {
    pub fn alternate(self) -> Self {
        match self {
            Self::Parallel => Self::Exa,
            Self::Exa => Self::Parallel,
        }
    }
    pub fn endpoint(self) -> &'static str {
        match self {
            Self::Parallel => "https://search.parallel.ai/mcp",
            Self::Exa => "https://mcp.exa.ai/mcp",
        }
    }
    pub fn tool(self, fetch: bool) -> &'static str {
        match (self, fetch) {
            (Self::Parallel, false) => "web_search",
            (Self::Parallel, true) => "web_fetch",
            (Self::Exa, false) => "web_search_exa",
            (Self::Exa, true) => "web_fetch_exa",
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Failure {
    InvalidArguments,
    DeniedUrl,
    Unsupported,
    IncompatibleSchema,
    RateLimited,
    Service,
    Transport,
    InvalidUpstream,
    Deadline,
    Cancelled,
    Budget,
}

impl Failure {
    pub fn fallback(self) -> bool {
        matches!(
            self,
            Self::RateLimited
                | Self::Service
                | Self::Transport
                | Self::InvalidUpstream
                | Self::Deadline
        )
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Source {
    pub id: String,
    pub url: String,
    pub title: String,
    pub provider: Provider,
    pub kind: EvidenceKind,
    pub text: String,
    pub retrieved_unix_ms: u128,
    pub publisher_date: Option<String>,
    pub truncated: bool,
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceKind {
    SearchExcerpt,
    PageRead,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct ToolResult {
    pub sources: Vec<Source>,
    pub failures: Vec<PageFailure>,
    pub fallback_used: bool,
    pub evidence_unavailable: bool,
    pub partial: bool,
    pub raw_text: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct PageFailure {
    pub url: String,
    pub failure: Failure,
}

pub fn truncate_utf8(text: &str, bytes: usize) -> String {
    let mut end = text.len().min(bytes);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_string()
}

pub fn has_readable_text(text: &str) -> bool {
    text.chars()
        .any(|character| !character.is_whitespace() && !character.is_control())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_wire_names_and_budgets_are_stable() {
        for (wire, mode, searches, reads, rounds, seconds, bytes) in [
            ("quick", Mode::Quick, 2, 2, 4, 20, 24 * 1024),
            ("verify", Mode::Verify, 4, 4, 6, 45, 48 * 1024),
            ("research", Mode::Research, 8, 8, 10, 90, 96 * 1024),
        ] {
            assert_eq!(serde_json::to_value(mode).unwrap(), wire);
            assert_eq!(
                serde_json::from_value::<Mode>(serde_json::json!(wire)).unwrap(),
                mode
            );
            let policy = mode.policy();
            assert_eq!(policy.searches, searches);
            assert_eq!(policy.reads, reads);
            assert_eq!(policy.rounds, rounds);
            assert_eq!(policy.tool_time, Duration::from_secs(seconds));
            assert_eq!(policy.evidence_bytes, bytes);
        }
    }

    #[test]
    fn truncation_keeps_a_valid_prefix_at_each_utf8_boundary() {
        let text = "Synthetic 🧪 evidence";
        for bytes in 0..=text.len() + 1 {
            let retained = truncate_utf8(text, bytes);
            assert!(retained.len() <= bytes);
            assert!(text.starts_with(&retained));
            if bytes >= text.len() {
                assert_eq!(retained, text);
            }
        }
    }

    #[test]
    fn whitespace_and_control_characters_are_not_readable_evidence() {
        for text in ["", " \t\r\n", "\0\u{7f}"] {
            assert!(!has_readable_text(text));
        }
        assert!(has_readable_text("Synthetic evidence"));
        assert!(has_readable_text("🧪"));
    }
}
