//! Pure contracts and prompt construction for the Deep/Review pipeline.

use serde::{Deserialize, Serialize};
use std::fmt;

pub const MAX_TEXT_BYTES: usize = 128 * 1024;
pub const MAX_SOURCES: usize = 32;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HistoryEntry {
    pub role: String,
    pub content: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Source {
    pub url: String,
    pub title: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeepRequest {
    pub original_request: String,
    pub canonical_history: Vec<HistoryEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Timings {
    pub elapsed_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeepReport {
    pub complete_answer: String,
    pub voice_summary: String,
    pub sources: Vec<Source>,
    pub timings: Timings,
    pub outcome: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReviewDecision {
    Keep,
    Rewrite {
        replacement: String,
        correction: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewReport {
    pub decision: ReviewDecision,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContractError(pub String);
impl fmt::Display for ContractError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for ContractError {}

pub fn custom_deep_prompt(request: &DeepRequest) -> String {
    format!("You are Custom Deep reasoning-only. Do not browse, call tools, or mutate the workspace or shell. Return only strict JSON with complete_answer, a voice_summary of at most 12 words, and an empty sources array.\nOriginal request:\n{}\nCanonical history:\n{}", request.original_request, format_history(&request.canonical_history))
}

pub fn opencode_deep_prompt(request: &DeepRequest) -> String {
    format!("You are OpenCode Deep research. You may use only websearch and webfetch through the adapter. Never mutate the workspace or shell, and do not claim tool results you did not receive. Return only strict JSON with complete_answer, a voice_summary of at most 12 words, and sources as validated URL/title objects.\nOriginal request:\n{}\nCanonical history:\n{}", request.original_request, format_history(&request.canonical_history))
}

pub fn review_prompt(original_request: &str, answer: &str, deep: Option<&DeepReport>) -> String {
    format!("Review the supplied answer against the original request. Style-only differences are KEEP. Compare the supplied Instant answer{}; return strict JSON: {{\"decision\":\"keep\"}} or {{\"decision\":\"rewrite\",\"replacement\":\"complete answer\",\"correction\":\"Correction: concise factual fix\"}}.\nOriginal: {original_request}\nAnswer: {answer}\nDeep: {}", if deep.is_some() { " and optional Deep report" } else { "" }, deep.map(|d| d.complete_answer.as_str()).unwrap_or("(none)"))
}

fn format_history(history: &[HistoryEntry]) -> String {
    history
        .iter()
        .map(|h| format!("{}: {}", h.role, h.content))
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn validate_deep_report(report: &DeepReport) -> Result<(), ContractError> {
    if report.complete_answer.trim().is_empty()
        || report.complete_answer.len() > MAX_TEXT_BYTES
        || report.voice_summary.trim().is_empty()
        || report.voice_summary.len() > 160
        || report.voice_summary.split_whitespace().count() > 12
        || report.outcome != "completed"
    {
        return Err(ContractError("answer or summary length is invalid".into()));
    }
    if report.sources.len() > MAX_SOURCES {
        return Err(ContractError("too many sources".into()));
    }
    for source in &report.sources {
        validate_source(source)?;
    }
    Ok(())
}

fn validate_source(source: &Source) -> Result<(), ContractError> {
    let url = source.url.as_bytes();
    let remainder = source
        .url
        .strip_prefix("https://")
        .or_else(|| source.url.strip_prefix("http://"))
        .unwrap_or_default();
    let authority_end = remainder.find(['/', '?', '#']).unwrap_or(remainder.len());
    let authority = &remainder[..authority_end];
    if url.is_empty()
        || url.len() > 2048
        || source.title.trim().is_empty()
        || source.title.len() > 512
        || authority.is_empty()
        || source.url.contains('@')
        || source.url.chars().any(|c| c.is_whitespace())
        || !valid_http_authority(authority)
    {
        return Err(ContractError("invalid source provenance".into()));
    }
    Ok(())
}

fn valid_http_authority(authority: &str) -> bool {
    if let Some(ipv6) = authority.strip_prefix('[') {
        let Some((host, port)) = ipv6.split_once(']') else {
            return false;
        };
        return host.parse::<std::net::Ipv6Addr>().is_ok() && valid_optional_port(port);
    }
    let (host, port) = authority
        .rsplit_once(':')
        .map_or((authority, ""), |(host, port)| (host, port));
    !host.is_empty()
        && host.split('.').all(|label| {
            !label.is_empty()
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        })
        && (port.is_empty() || valid_port(port))
}

fn valid_optional_port(port: &str) -> bool {
    port.is_empty() || port.strip_prefix(':').is_some_and(valid_port)
}

fn valid_port(port: &str) -> bool {
    port.parse::<u16>().is_ok_and(|port| port != 0)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeepWireReport {
    pub complete_answer: String,
    pub voice_summary: String,
    pub sources: Vec<Source>,
}

pub fn validate_deep_wire(
    wire: DeepWireReport,
    elapsed_ms: u64,
    sources_allowed: bool,
) -> Result<DeepReport, ContractError> {
    if !sources_allowed && !wire.sources.is_empty() {
        return Err(ContractError(
            "reasoning-only Deep cannot return sources".into(),
        ));
    }
    let report = DeepReport {
        complete_answer: wire.complete_answer,
        voice_summary: wire.voice_summary,
        sources: wire.sources,
        timings: Timings { elapsed_ms },
        outcome: String::from("completed"),
    };
    validate_deep_report(&report)?;
    Ok(report)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewWireReport {
    pub decision: String,
    pub replacement: Option<String>,
    pub correction: Option<String>,
}

pub fn validate_review_wire(wire: ReviewWireReport) -> Result<ReviewReport, ContractError> {
    match wire.decision.as_str() {
        "keep" if wire.replacement.is_none() && wire.correction.is_none() => Ok(ReviewReport {
            decision: ReviewDecision::Keep,
        }),
        "rewrite" => {
            let r = wire
                .replacement
                .ok_or_else(|| ContractError("replacement required".into()))?;
            let c = wire
                .correction
                .ok_or_else(|| ContractError("correction required".into()))?;
            if r.trim().is_empty()
                || r.len() > MAX_TEXT_BYTES
                || !c.starts_with("Correction: ")
                || c["Correction: ".len()..].trim().is_empty()
                || c.split_whitespace().count() > 12
                || c.len() > 160
            {
                return Err(ContractError("invalid rewrite contract".into()));
            }
            Ok(ReviewReport {
                decision: ReviewDecision::Rewrite {
                    replacement: r,
                    correction: c,
                },
            })
        }
        _ => Err(ContractError("invalid decision or fields".into())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deep_json_validates_sources_and_injects_timings() {
        let report = validate_deep_wire(
            DeepWireReport {
                complete_answer: String::from("Answer, with detail."),
                voice_summary: String::from("Answer verified."),
                sources: vec![Source {
                    url: String::from("https://example.com/source"),
                    title: String::from("Primary source"),
                }],
            },
            42,
            true,
        )
        .expect("valid research report");
        assert_eq!(report.timings.elapsed_ms, 42);
        assert_eq!(report.sources.len(), 1);
    }

    #[test]
    fn reasoning_only_deep_rejects_claimed_sources() {
        assert!(validate_deep_wire(
            DeepWireReport {
                complete_answer: String::from("Answer"),
                voice_summary: String::from("Done."),
                sources: vec![Source {
                    url: String::from("https://example.com"),
                    title: String::from("Source"),
                }],
            },
            1,
            false,
        )
        .is_err());
    }

    #[test]
    fn deep_report_rejects_bad_provenance() {
        assert!(validate_deep_wire(
            DeepWireReport {
                complete_answer: String::from("Answer"),
                voice_summary: String::from("Done."),
                sources: vec![Source {
                    url: String::from("https://user@example.com"),
                    title: String::from("Source"),
                }],
            },
            1,
            true,
        )
        .is_err());
    }

    #[test]
    fn review_keep_has_exact_nullability() {
        assert_eq!(
            validate_review_wire(ReviewWireReport {
                decision: String::from("keep"),
                replacement: None,
                correction: None,
            })
            .unwrap(),
            ReviewReport {
                decision: ReviewDecision::Keep
            }
        );
        assert!(validate_review_wire(ReviewWireReport {
            decision: String::from("keep"),
            replacement: Some(String::from("unused")),
            correction: None,
        })
        .is_err());
    }

    #[test]
    fn review_rewrite_supports_normal_escaped_json() {
        let report = validate_review_wire(ReviewWireReport {
            decision: String::from("rewrite"),
            replacement: Some(String::from("Use \"quoted\" text, then continue.")),
            correction: Some(String::from("Correction: Use the verified value.")),
        })
        .expect("valid rewrite");
        assert!(matches!(
            report.decision,
            ReviewDecision::Rewrite { replacement, .. } if replacement.contains("quoted")
        ));
    }

    #[test]
    fn review_rewrite_rejects_missing_or_long_correction() {
        assert!(validate_review_wire(ReviewWireReport {
            decision: String::from("rewrite"),
            replacement: Some(String::from("Answer")),
            correction: None,
        })
        .is_err());
        assert!(validate_review_wire(ReviewWireReport {
            decision: String::from("rewrite"),
            replacement: Some(String::from("Answer")),
            correction: Some(String::from(
                "Correction: one two three four five six seven eight nine ten eleven twelve",
            )),
        })
        .is_err());
    }

    #[test]
    fn review_rejects_unknown_decisions() {
        assert!(validate_review_wire(ReviewWireReport {
            decision: String::from("maybe"),
            replacement: None,
            correction: None,
        })
        .is_err());
    }

    #[test]
    fn prompts_state_tool_and_materiality_boundaries() {
        let request = DeepRequest {
            original_request: String::from("question"),
            canonical_history: vec![],
        };
        assert!(custom_deep_prompt(&request).contains("Do not browse"));
        assert!(opencode_deep_prompt(&request).contains("only websearch and webfetch"));
        assert!(review_prompt("q", "a", None).contains("Style-only differences are KEEP"));
    }

    #[test]
    fn rejects_malformed_source_authorities() {
        for url in [
            "https://?query",
            "https://#fragment",
            "https://-bad.example",
            "https://bad..example",
            "https://example.test:bad/path",
            "https://[not-ipv6]/path",
        ] {
            assert!(validate_source(&Source {
                url: url.into(),
                title: "invalid".into(),
            })
            .is_err());
        }
    }
}
