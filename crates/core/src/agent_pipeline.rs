//! Pure contracts and prompt construction for the Deep/Review pipeline.

use crate::assistant::Content;
use serde::{Deserialize, Serialize};
use std::fmt;

pub const MAX_TEXT_BYTES: usize = 128 * 1024;
pub const MAX_SOURCES: usize = 32;
pub const MAX_REVIEW_PROMPT_BYTES: usize = 512 * 1024;
pub const MAX_HISTORY_ENTRIES: usize = 128;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HistoryEntry {
    pub role: String,
    pub content: Content,
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StageStatus<T> {
    Success(T),
    Failure(String),
}

pub type SourceEvidence = Source;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewInput {
    pub original_request: String,
    pub canonical_history: Vec<HistoryEntry>,
    pub instant: StageStatus<Content>,
    pub deep: StageStatus<Content>,
    pub materiality_policy: String,
    pub sources: Vec<SourceEvidence>,
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
        replacement: Content,
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

pub fn typed_review_prompt(input: &ReviewInput) -> String {
    format!(
        "Review only material factual defects; style-only differences are KEEP. Return exactly one strict JSON object with all three keys: decision, replacement, and correction. Accepted wire forms are {{\"decision\":\"keep\",\"replacement\":null,\"correction\":null}}, {{\"decision\":\"rewrite\",\"replacement\":{{\"type\":\"text\",\"content\":\"complete corrected answer\"}},\"correction\":\"Correction: concise factual fix\"}}, or {{\"decision\":\"rewrite\",\"replacement\":{{\"type\":\"refusal\",\"content\":\"concise refusal\"}},\"correction\":\"Correction: concise factual fix\"}}. KEEP requires replacement and correction to be null. REWRITE requires a replacement object with exactly type and content; type must be `text` or `refusal`, content must be non-empty and at most 128 KiB, and correction must start exactly with `Correction: `. correction must be concise: at most 12 words and 160 bytes; use concise factual correction text. Do not add fields.\nOriginal: {}\nCanonical history: {}\nInstant status: {}\nDeep status: {}\nMateriality policy: {}\nValidated source evidence: {:?}",
        input.original_request,
        format_history(&input.canonical_history),
        format_stage_status(&input.instant),
        format_stage_status(&input.deep),
        input.materiality_policy,
        input.sources
    )
}

fn format_stage_status(status: &StageStatus<Content>) -> String {
    match status {
        StageStatus::Success(content) => format!("Success({})", format_content(content)),
        StageStatus::Failure(error) => format!("Failure({error})"),
    }
}

pub fn validate_review_input(input: &ReviewInput) -> Result<(), ContractError> {
    if input.original_request.len() > MAX_TEXT_BYTES
        || input.materiality_policy.len() > 1024
        || input.canonical_history.len() > MAX_HISTORY_ENTRIES
        || input.sources.len() > MAX_SOURCES
    {
        return Err(ContractError("review input exceeds bounds".into()));
    }
    if let StageStatus::Success(content) = &input.instant {
        validate_content(content)?;
    }
    if let StageStatus::Success(content) = &input.deep {
        validate_content(content)?;
    }
    if let StageStatus::Failure(error) = &input.instant {
        validate_text(error, "instant failure")?;
    }
    if let StageStatus::Failure(error) = &input.deep {
        validate_text(error, "deep failure")?;
    }
    for entry in &input.canonical_history {
        validate_text(&entry.role, "history role")?;
        validate_content(&entry.content)?;
    }
    for source in &input.sources {
        validate_source(source)?;
    }
    if typed_review_prompt(input).len() > MAX_REVIEW_PROMPT_BYTES {
        return Err(ContractError("review prompt exceeds bounds".into()));
    }
    Ok(())
}

fn format_history(history: &[HistoryEntry]) -> String {
    history
        .iter()
        .map(|h| format!("{}: {}", h.role, format_content(&h.content)))
        .collect::<Vec<_>>()
        .join("\n")
}

fn format_content(content: &Content) -> String {
    match content {
        Content::Text(value) => format!("Text: {value}"),
        Content::Refusal(value) => format!("Refusal: {value}"),
    }
}

fn validate_content(content: &Content) -> Result<(), ContractError> {
    match content {
        Content::Text(value) => validate_text(value, "text"),
        Content::Refusal(value) => validate_text(value, "refusal"),
    }
}

fn validate_text(value: &str, field: &str) -> Result<(), ContractError> {
    if value.len() > MAX_TEXT_BYTES {
        return Err(ContractError(format!("{field} exceeds bounds")));
    }
    Ok(())
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
        || authority.contains('@')
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
    pub replacement: Option<ReviewReplacement>,
    pub correction: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "type", content = "content", deny_unknown_fields)]
pub enum ReviewReplacement {
    #[serde(rename = "text")]
    Text(String),
    #[serde(rename = "refusal")]
    Refusal(String),
}

pub fn validate_review_wire(wire: ReviewWireReport) -> Result<ReviewReport, ContractError> {
    match wire.decision.as_str() {
        "keep" if wire.replacement.is_none() && wire.correction.is_none() => Ok(ReviewReport {
            decision: ReviewDecision::Keep,
        }),
        "rewrite" => {
            let replacement = wire
                .replacement
                .ok_or_else(|| ContractError("replacement required".into()))?;
            let c = wire
                .correction
                .ok_or_else(|| ContractError("correction required".into()))?;
            let content = match replacement {
                ReviewReplacement::Text(content) => Content::Text(content),
                ReviewReplacement::Refusal(content) => Content::Refusal(content),
            };
            if matches!(&content, Content::Text(value) | Content::Refusal(value) if value.trim().is_empty() || value.len() > MAX_TEXT_BYTES)
                || !c.starts_with("Correction: ")
                || c["Correction: ".len()..].trim().is_empty()
                || c.split_whitespace().count() > 12
                || c.len() > 160
            {
                return Err(ContractError("invalid rewrite contract".into()));
            }
            Ok(ReviewReport {
                decision: ReviewDecision::Rewrite {
                    replacement: content,
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
            replacement: Some(ReviewReplacement::Text(String::from("unused"))),
            correction: None,
        })
        .is_err());
    }

    #[test]
    fn review_rewrite_supports_normal_escaped_json() {
        let report = validate_review_wire(ReviewWireReport {
            decision: String::from("rewrite"),
            replacement: Some(ReviewReplacement::Text(String::from(
                "Use \"quoted\" text, then continue.",
            ))),
            correction: Some(String::from("Correction: Use the verified value.")),
        })
        .expect("valid rewrite");
        assert!(matches!(
            report.decision,
            ReviewDecision::Rewrite { replacement: Content::Text(replacement), .. } if replacement.contains("quoted")
        ));
    }

    #[test]
    fn review_rewrite_rejects_missing_or_long_correction() {
        assert!(validate_review_wire(ReviewWireReport {
            decision: String::from("rewrite"),
            replacement: Some(ReviewReplacement::Text(String::from("Answer"))),
            correction: None,
        })
        .is_err());
        assert!(validate_review_wire(ReviewWireReport {
            decision: String::from("rewrite"),
            replacement: Some(ReviewReplacement::Text(String::from("Answer"))),
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
        assert!(typed_review_prompt(&ReviewInput {
            original_request: "q".into(),
            canonical_history: vec![],
            instant: StageStatus::Success(Content::Text("a".into())),
            deep: StageStatus::Failure("none".into()),
            materiality_policy: "factual defects only".into(),
            sources: vec![],
        })
        .contains("style-only differences are KEEP"));
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

    #[test]
    fn source_validation_allows_at_in_path_and_query_but_not_authority_userinfo() {
        for url in [
            "https://example.com/path/@user",
            "https://example.com/?q=@user",
        ] {
            assert!(validate_source(&Source {
                url: url.into(),
                title: "valid".into()
            })
            .is_ok());
        }
        assert!(validate_source(&Source {
            url: "https://user@example.com/path".into(),
            title: "invalid".into()
        })
        .is_err());
    }

    #[test]
    fn review_prompt_contains_typed_status_history_policy_and_sources() {
        let input = ReviewInput {
            original_request: "question".into(),
            canonical_history: vec![HistoryEntry {
                role: "user".into(),
                content: Content::Text("prior".into()),
            }],
            instant: StageStatus::Success(Content::Text("instant".into())),
            deep: StageStatus::Failure("timeout".into()),
            materiality_policy: "factual defects only".into(),
            sources: vec![SourceEvidence {
                url: "https://example.com".into(),
                title: "Evidence".into(),
            }],
        };
        let prompt = typed_review_prompt(&input);
        assert!(
            prompt.contains("prior") && prompt.contains("instant") && prompt.contains("timeout")
        );
        assert!(prompt.contains("factual defects only") && prompt.contains("https://example.com"));
    }

    #[test]
    fn typed_review_prompt_specifies_the_exact_review_wire_contract() {
        let input = ReviewInput {
            original_request: "question".into(),
            canonical_history: vec![],
            instant: StageStatus::Success(Content::Text("answer".into())),
            deep: StageStatus::Success(Content::Text("research".into())),
            materiality_policy: "factual defects only".into(),
            sources: vec![],
        };
        let prompt = typed_review_prompt(&input);

        for fragment in [
            r#"{"decision":"keep","replacement":null,"correction":null}"#,
            r#"{"decision":"rewrite","replacement":{"type":"text","content":"complete corrected answer"},"correction":"Correction: concise factual fix"}"#,
            "decision, replacement, and correction",
            "KEEP requires replacement and correction to be null",
            "REWRITE requires a replacement object with exactly type and content",
            "correction must start exactly with `Correction: `",
            "correction must be concise: at most 12 words and 160 bytes",
        ] {
            assert!(
                prompt.contains(fragment),
                "missing prompt contract: {fragment}"
            );
        }
    }

    #[test]
    fn provider_like_review_fixtures_accept_only_valid_wire_forms() {
        for (json, expected) in [
            (
                ReviewWireReport {
                    decision: "keep".into(),
                    replacement: None,
                    correction: None,
                },
                ReviewDecision::Keep,
            ),
            (
                ReviewWireReport {
                    decision: "rewrite".into(),
                    replacement: Some(ReviewReplacement::Text("Corrected answer.".into())),
                    correction: Some("Correction: Use the verified value.".into()),
                },
                ReviewDecision::Rewrite {
                    replacement: Content::Text("Corrected answer.".into()),
                    correction: "Correction: Use the verified value.".into(),
                },
            ),
        ] {
            assert_eq!(validate_review_wire(json).unwrap().decision, expected);
        }

        for wire in [
            ReviewWireReport {
                decision: "keep".into(),
                replacement: Some(ReviewReplacement::Text("unused".into())),
                correction: None,
            },
            ReviewWireReport {
                decision: "rewrite".into(),
                replacement: Some(ReviewReplacement::Text("Answer".into())),
                correction: None,
            },
            ReviewWireReport {
                decision: "rewrite".into(),
                replacement: Some(ReviewReplacement::Text("Answer".into())),
                correction: Some("Use the value.".into()),
            },
            ReviewWireReport {
                decision: "rewrite".into(),
                replacement: Some(ReviewReplacement::Text("Answer".into())),
                correction: Some(
                    "Correction: one two three four five six seven eight nine ten eleven twelve"
                        .into(),
                ),
            },
            ReviewWireReport {
                decision: "maybe".into(),
                replacement: None,
                correction: None,
            },
        ] {
            assert!(validate_review_wire(wire).is_err());
        }
    }

    fn review_input() -> ReviewInput {
        ReviewInput {
            original_request: "question".into(),
            canonical_history: vec![HistoryEntry {
                role: "user".into(),
                content: Content::Refusal("history refusal".into()),
            }],
            instant: StageStatus::Success(Content::Refusal("instant refusal".into())),
            deep: StageStatus::Success(Content::Text("deep answer".into())),
            materiality_policy: "factual defects only".into(),
            sources: vec![],
        }
    }

    #[test]
    fn typed_prompts_distinguish_text_and_refusal_content() {
        let input = review_input();
        let prompt = typed_review_prompt(&input);
        assert!(prompt.contains("Refusal: history refusal"));
        assert!(prompt.contains("Success(Refusal: instant refusal)"));
        assert!(prompt.contains("Success(Text: deep answer)"));
        assert!(custom_deep_prompt(&DeepRequest {
            original_request: "q".into(),
            canonical_history: input.canonical_history.clone(),
        })
        .contains("Refusal: history refusal"));
    }

    #[test]
    fn review_input_rejects_each_textual_boundary_and_prompt_aggregate() {
        let mut input = review_input();
        input.original_request = "x".repeat(MAX_TEXT_BYTES + 1);
        assert!(validate_review_input(&input).is_err());
        let mut input = review_input();
        input.canonical_history[0].role = "x".repeat(MAX_TEXT_BYTES + 1);
        assert!(validate_review_input(&input).is_err());
        let mut input = review_input();
        input.canonical_history[0].content = Content::Refusal("x".repeat(MAX_TEXT_BYTES + 1));
        assert!(validate_review_input(&input).is_err());
        let mut input = review_input();
        input.instant = StageStatus::Success(Content::Text("x".repeat(MAX_TEXT_BYTES + 1)));
        assert!(validate_review_input(&input).is_err());
        let mut input = review_input();
        input.deep = StageStatus::Failure("x".repeat(MAX_TEXT_BYTES + 1));
        assert!(validate_review_input(&input).is_err());
        let mut input = review_input();
        input.materiality_policy = "x".repeat(1025);
        assert!(validate_review_input(&input).is_err());

        let mut input = review_input();
        input.sources = vec![SourceEvidence {
            url: format!("https://{}.example", "x".repeat(2049)),
            title: "valid".into(),
        }];
        assert!(validate_review_input(&input).is_err());
        let mut input = review_input();
        input.sources = vec![SourceEvidence {
            url: "https://example.com".into(),
            title: "x".repeat(513),
        }];
        assert!(validate_review_input(&input).is_err());

        let mut input = review_input();
        input.original_request = "é".repeat(MAX_TEXT_BYTES / 2 + 1);
        assert!(validate_review_input(&input).is_err());

        let mut input = review_input();
        input.instant = StageStatus::Success(Content::Text("x".repeat(MAX_TEXT_BYTES)));
        assert!(validate_review_input(&input).is_ok());

        let mut input = review_input();
        input.canonical_history = (0..128)
            .map(|_| HistoryEntry {
                role: "user".into(),
                content: Content::Text("x".repeat(4096)),
            })
            .collect();
        assert!(validate_review_input(&input).is_err());
    }

    #[test]
    fn refusal_review_fixtures_keep_or_rewrite_without_losing_type() {
        let mut input = review_input();
        input.deep = StageStatus::Success(Content::Refusal("deep refusal".into()));
        let prompt = typed_review_prompt(&input);
        assert!(prompt.contains("Success(Refusal: deep refusal)"));
        assert!(validate_review_wire(ReviewWireReport {
            decision: "keep".into(),
            replacement: None,
            correction: None,
        })
        .is_ok());
        assert!(validate_review_wire(ReviewWireReport {
            decision: "rewrite".into(),
            replacement: Some(ReviewReplacement::Refusal("Corrected refusal".into())),
            correction: Some("Correction: Refusal was not warranted.".into()),
        })
        .is_ok());
    }

    #[test]
    fn refusal_rewrite_parses_as_refusal_and_commits_with_type() {
        let wire: ReviewWireReport = toml::from_str(
            r#"decision = "rewrite"
correction = "Correction: The request requires a refusal."

[replacement]
type = "refusal"
content = "I cannot provide that."
"#,
        )
        .expect("valid refusal rewrite JSON");
        let report = validate_review_wire(wire).expect("valid refusal rewrite");
        let ReviewDecision::Rewrite { replacement, .. } = report.decision else {
            panic!("expected rewrite");
        };
        assert_eq!(
            replacement,
            Content::Refusal("I cannot provide that.".into())
        );

        let mut coordinator =
            crate::assistant::AssistantCoordinator::new(crate::assistant::AssistantPreferences {
                review_enabled: true,
                ..Default::default()
            });
        let generation = coordinator.start("question").expect("generation");
        coordinator.accept(
            generation,
            crate::assistant::Stage::Instant,
            crate::assistant::StageResult::Instant(crate::assistant::InstantOutcome::Complete(
                Content::Text("answer".into()),
            )),
        );
        let result = coordinator.accept(
            generation,
            crate::assistant::Stage::Review,
            crate::assistant::StageResult::Review(crate::assistant::ReviewOutcome::Success(
                crate::assistant::ReviewDecision::Rewrite(replacement.clone()),
            )),
        );
        assert_eq!(
            result,
            crate::assistant::AcceptResult::Resolved(replacement.clone())
        );
        assert_eq!(coordinator.commit(generation), Some(replacement));
    }
}
