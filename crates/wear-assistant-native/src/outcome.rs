use crate::{request::MAX_OUTPUT_BYTES, ErrorCode};
use serde::Serialize;
use voxgolem_core::durable_conversation::{validate_completed_content, Answer, DomainError};
use voxgolem_core::sources::{merge_sources, source_bytes, SourceLink};
use voxgolem_platform::custom_openai::{CustomOpenAiError, CustomOpenAiResponse};

#[derive(Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub(crate) enum Outcome {
    Complete {
        text: String,
        sources: Vec<SourceLink>,
        #[serde(rename = "contentType")]
        content_type: &'static str,
        #[serde(rename = "returnedModel", skip_serializing_if = "Option::is_none")]
        returned_model: Option<String>,
        #[serde(rename = "authRevision", skip_serializing_if = "Option::is_none")]
        auth_revision: Option<String>,
        #[serde(rename = "searchMs", skip_serializing_if = "Option::is_none")]
        search_ms: Option<u64>,
        #[serde(rename = "searchStatus", skip_serializing_if = "Option::is_none")]
        search_status: Option<&'static str>,
        #[serde(rename = "fallbackUsed", skip_serializing_if = "Option::is_none")]
        fallback_used: Option<bool>,
        #[serde(rename = "completedMs", skip_serializing_if = "Option::is_none")]
        completed_ms: Option<u64>,
        #[serde(rename = "rounds", skip_serializing_if = "Option::is_none")]
        rounds: Option<u64>,
        #[serde(rename = "searches", skip_serializing_if = "Option::is_none")]
        searches: Option<u64>,
        #[serde(rename = "reads", skip_serializing_if = "Option::is_none")]
        reads: Option<u64>,
    },
    Cancelled,
    Error {
        code: ErrorCode,
        #[serde(rename = "authRevision", skip_serializing_if = "Option::is_none")]
        auth_revision: Option<String>,
    },
}

pub(crate) struct SearchTelemetry {
    pub search_ms: u64,
    pub search_status: &'static str,
    pub fallback_used: bool,
    pub completed_ms: u64,
    pub rounds: u64,
    pub searches: u64,
    pub reads: u64,
}

impl Outcome {
    pub fn error(code: ErrorCode) -> Self {
        Self::Error {
            code,
            auth_revision: None,
        }
    }

    pub fn with_auth_revision(mut self, revision: Option<String>) -> Self {
        match &mut self {
            Self::Complete { auth_revision, .. } | Self::Error { auth_revision, .. } => {
                *auth_revision = revision
            }
            Self::Cancelled => {}
        }
        self
    }

    pub fn with_search_telemetry(mut self, telemetry: SearchTelemetry) -> Self {
        if let Self::Complete {
            search_ms: out_search_ms,
            search_status: out_search_status,
            fallback_used: out_fallback_used,
            completed_ms: out_completed_ms,
            rounds: out_rounds,
            searches: out_searches,
            reads: out_reads,
            ..
        } = &mut self
        {
            *out_search_ms = Some(telemetry.search_ms);
            *out_search_status = Some(telemetry.search_status);
            *out_fallback_used = Some(telemetry.fallback_used);
            *out_completed_ms = Some(telemetry.completed_ms);
            *out_rounds = Some(telemetry.rounds);
            *out_searches = Some(telemetry.searches);
            *out_reads = Some(telemetry.reads);
        }
        self
    }

    pub fn from_response(
        result: Result<CustomOpenAiResponse, CustomOpenAiError>,
        prompt: &str,
    ) -> Self {
        Self::convert_response(result, prompt, Vec::new())
    }

    fn convert_response(
        result: Result<CustomOpenAiResponse, CustomOpenAiError>,
        prompt: &str,
        sources: Vec<SourceLink>,
    ) -> Self {
        match result {
            Ok(response) if response.text.len() > MAX_OUTPUT_BYTES => {
                Self::error(ErrorCode::TooLarge)
            }
            Ok(response) if response.text.trim().is_empty() => {
                Self::error(ErrorCode::InvalidResponse)
            }
            Ok(response)
                if response
                    .model
                    .as_ref()
                    .is_some_and(|model| model.len() > 256 || model.trim().is_empty()) =>
            {
                Self::error(ErrorCode::InvalidResponse)
            }
            Ok(response) => {
                let answer = if response.content_type
                    == voxgolem_platform::custom_openai::CustomOpenAiContentType::Refusal
                {
                    Answer::Refusal {
                        reason: response.text,
                    }
                } else {
                    Answer::Text {
                        text: response.text,
                    }
                };
                if let Err(error) = validate_completed_content(prompt, &answer, &sources) {
                    return Self::error(match error {
                        DomainError::LimitExceeded { limit }
                            if matches!(
                                limit.as_str(),
                                "prompt" | "answer" | "turn_text_bytes"
                            ) =>
                        {
                            ErrorCode::TooLarge
                        }
                        _ => ErrorCode::InvalidResponse,
                    });
                }
                let text = match answer {
                    Answer::Text { text } => text,
                    Answer::Refusal { reason } => reason,
                };
                Self::Complete {
                    text,
                    sources,
                    content_type: response.content_type.as_str(),
                    returned_model: response.model,
                    auth_revision: None,
                    search_ms: None,
                    search_status: None,
                    fallback_used: None,
                    completed_ms: None,
                    rounds: None,
                    searches: None,
                    reads: None,
                }
            }
            Err(error) => Self::error(error.into()),
        }
    }

    pub fn json(&self) -> String {
        serde_json::to_string(self)
            .unwrap_or_else(|_| r#"{"status":"error","code":"internal_error"}"#.to_string())
    }

    pub fn from_search_response(
        response: CustomOpenAiResponse,
        observed_sources: Vec<SourceLink>,
        prompt: &str,
    ) -> Self {
        let mut canonical_sources = Vec::new();
        if response.content_type
            != voxgolem_platform::custom_openai::CustomOpenAiContentType::Refusal
        {
            for source in observed_sources {
                canonical_sources = match merge_sources(&canonical_sources, &[source]) {
                    Ok(sources) => sources,
                    Err(_) => return Self::error(ErrorCode::InvalidResponse),
                };
            }
        }
        if response
            .text
            .len()
            .saturating_add(source_bytes(&canonical_sources))
            > MAX_OUTPUT_BYTES
        {
            return Self::error(ErrorCode::TooLarge);
        }
        Self::convert_response(Ok(response), prompt, canonical_sources)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use voxgolem_platform::custom_openai::{CustomOpenAiContentType, CustomOpenAiTimings};

    #[test]
    fn final_completion_reserves_the_admitted_prompt_budget() {
        for prompt in ["p".to_string(), "p".repeat(32 * 1024)] {
            let text = "x".repeat(256 * 1024 - prompt.len());
            let accepted = Outcome::from_response(
                Ok(response(&text, CustomOpenAiContentType::OutputText)),
                &prompt,
            );
            assert!(matches!(accepted, Outcome::Complete { .. }));
            let rejected = Outcome::from_response(
                Ok(response(&(text + "x"), CustomOpenAiContentType::OutputText)),
                &prompt,
            );
            assert!(matches!(
                rejected,
                Outcome::Error {
                    code: ErrorCode::TooLarge,
                    ..
                }
            ));
        }
    }

    #[test]
    fn canonical_completion_survives_the_actual_store_codec() {
        use voxgolem_core::durable_conversation::{
            Operation, OperationBody, OperationId, Provenance, TerminalStage, MAX_PROMPT_BYTES,
            MAX_TURN_TEXT_BYTES, SCHEMA_VERSION,
        };
        let source = SourceLink {
            url: "https://example.test/synthetic".into(),
            title: "Synthetic source".into(),
        };
        for prompt in ["p".to_string(), "🧪".repeat(MAX_PROMPT_BYTES / 4)] {
            for refusal in [false, true] {
                let sources = if refusal {
                    vec![]
                } else {
                    vec![source.clone()]
                };
                let budget = MAX_TURN_TEXT_BYTES - prompt.len() - source_bytes(&sources);
                let text = format!("{}{}", "🧪".repeat(budget / 4), "x".repeat(budget % 4));
                let kind = if refusal {
                    CustomOpenAiContentType::Refusal
                } else {
                    CustomOpenAiContentType::OutputText
                };
                let outcome = Outcome::from_search_response(
                    response(&text, kind),
                    vec![source.clone()],
                    &prompt,
                );
                let Outcome::Complete { text, sources, .. } = outcome else {
                    panic!("boundary must complete")
                };
                let id = |sequence: u64| OperationId {
                    replica: "00000000-0000-4000-8000-000000000001".into(),
                    sequence: sequence.to_string(),
                };
                let operation = Operation {
                    version: SCHEMA_VERSION,
                    id: id(3),
                    body: OperationBody::TurnCompleted {
                        conversation: id(1),
                        parent: None,
                        request: id(2),
                        prompt: prompt.clone(),
                        answer: if refusal {
                            Answer::Refusal {
                                reason: text.clone(),
                            }
                        } else {
                            Answer::Text { text: text.clone() }
                        },
                        sources,
                        terminal_stage: TerminalStage::Instant,
                        provenance: Provenance {
                            requested_model: "synthetic-model".into(),
                            returned_model: None,
                            policy_id: "synthetic".into(),
                            policy_version: "1".into(),
                            instruction_profile_id: "synthetic".into(),
                            instruction_profile_version: "1".into(),
                            context: vec![],
                        },
                    },
                };
                let raw = serde_json::to_string(&operation).unwrap();
                let record = voxgolem_conversation_store::codec::canonical_record(&raw).unwrap();
                record.validate().unwrap();
                assert!(matches!(
                    Outcome::from_search_response(
                        response(&(text + "x"), kind),
                        vec![source.clone()],
                        &prompt
                    ),
                    Outcome::Error {
                        code: ErrorCode::TooLarge,
                        ..
                    }
                ));
            }
        }
    }

    fn response(text: &str, content_type: CustomOpenAiContentType) -> CustomOpenAiResponse {
        CustomOpenAiResponse {
            text: text.into(),
            content_type,
            model: Some("synthetic-model".into()),
            service_tier: None,
            usage: None,
            timings: CustomOpenAiTimings::default(),
        }
    }

    #[test]
    fn completion_envelope_preserves_text_refusal_and_returned_model_without_sensitive_metadata() {
        let outcome = Outcome::from_response(
            Ok(response(
                "Synthetic \"answer\"\n",
                CustomOpenAiContentType::Refusal,
            )),
            "Synthetic prompt",
        );
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&outcome.json()).unwrap(),
            serde_json::json!({
                "status": "complete", "text": "Synthetic \"answer\"\n", "contentType": "refusal",
                "returnedModel": "synthetic-model", "sources": []
            })
        );
    }

    #[test]
    fn search_projection_keeps_plain_answer_and_authoritative_source_links() {
        let source = SourceLink {
            url: "https://example.test/synthetic".into(),
            title: "Synthetic source".into(),
        };
        let value: serde_json::Value = serde_json::from_str(
            &Outcome::from_search_response(
                response("Synthetic answer", CustomOpenAiContentType::OutputText),
                vec![source.clone(), source.clone()],
                "Synthetic prompt",
            )
            .json(),
        )
        .unwrap();
        assert_eq!(value["text"], "Synthetic answer");
        assert_eq!(value["sources"], serde_json::json!([source]));
        assert!(value.get("speechText").is_none());
        assert_eq!(
            Outcome::from_search_response(
                response(
                    &"x".repeat(MAX_OUTPUT_BYTES),
                    CustomOpenAiContentType::OutputText
                ),
                vec![source.clone()],
                "Synthetic prompt"
            )
            .json(),
            r#"{"status":"error","code":"too_large"}"#
        );
        let boundary = "🧪".repeat((MAX_OUTPUT_BYTES - 4) / 4);
        let value: serde_json::Value = serde_json::from_str(
            &Outcome::from_search_response(
                response(&boundary, CustomOpenAiContentType::OutputText),
                vec![],
                "test",
            )
            .json(),
        )
        .unwrap();
        assert_eq!(value["text"], boundary);
        let refusal: serde_json::Value = serde_json::from_str(
            &Outcome::from_search_response(
                response("Synthetic refusal", CustomOpenAiContentType::Refusal),
                vec![source],
                "Synthetic prompt",
            )
            .json(),
        )
        .unwrap();
        assert_eq!(refusal["sources"], serde_json::json!([]));
        let telemetry: serde_json::Value = serde_json::from_str(
            &Outcome::from_search_response(
                response("Synthetic answer", CustomOpenAiContentType::OutputText),
                vec![],
                "Synthetic prompt",
            )
            .with_search_telemetry(SearchTelemetry {
                search_ms: 120,
                search_status: "ready",
                fallback_used: false,
                completed_ms: 340,
                rounds: 2,
                searches: 1,
                reads: 0,
            })
            .json(),
        )
        .unwrap();
        assert_eq!(telemetry["searchMs"], serde_json::json!(120));
        assert_eq!(telemetry["searchStatus"], serde_json::json!("ready"));
        assert_eq!(telemetry["fallbackUsed"], serde_json::json!(false));
        assert_eq!(telemetry["completedMs"], serde_json::json!(340));
        assert_eq!(telemetry["rounds"], serde_json::json!(2));
        assert_eq!(telemetry["searches"], serde_json::json!(1));
        assert_eq!(telemetry["reads"], serde_json::json!(0));
    }

    #[test]
    fn errors_are_static_codes_and_expiry_is_not_retried_or_refreshed() {
        for (error, code) in [
            (CustomOpenAiError::AuthUnavailable, "auth_unavailable"),
            (CustomOpenAiError::AuthExpired, "auth_expired"),
            (CustomOpenAiError::Unauthorized, "unauthorized"),
            (CustomOpenAiError::Forbidden, "forbidden"),
            (CustomOpenAiError::Http(500), "provider_http_error"),
            (CustomOpenAiError::MalformedStream, "invalid_response"),
        ] {
            let value: serde_json::Value = serde_json::from_str(
                &Outcome::from_response(Err(error), "Synthetic prompt").json(),
            )
            .unwrap();
            assert_eq!(value, serde_json::json!({"status": "error", "code": code}));
        }
    }

    #[test]
    fn output_envelope_remains_bounded_and_empty_output_is_not_success() {
        assert_eq!(
            Outcome::from_response(
                Ok(response(
                    &"x".repeat(MAX_OUTPUT_BYTES + 1),
                    CustomOpenAiContentType::OutputText
                )),
                "Synthetic prompt"
            )
            .json(),
            r#"{"status":"error","code":"too_large"}"#
        );
        assert_eq!(
            Outcome::from_response(
                Ok(response(" ", CustomOpenAiContentType::OutputText)),
                "Synthetic prompt"
            )
            .json(),
            r#"{"status":"error","code":"invalid_response"}"#
        );
    }
}
