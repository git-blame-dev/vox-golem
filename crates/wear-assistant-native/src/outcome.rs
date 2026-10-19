use crate::{request::MAX_OUTPUT_BYTES, ErrorCode};
use serde::Serialize;
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
        #[serde(rename = "speechText", skip_serializing_if = "Option::is_none")]
        speech_text: Option<String>,
        #[serde(rename = "searchMs", skip_serializing_if = "Option::is_none")]
        search_ms: Option<u64>,
        #[serde(rename = "searchStatus", skip_serializing_if = "Option::is_none")]
        search_status: Option<&'static str>,
        #[serde(rename = "fallbackUsed", skip_serializing_if = "Option::is_none")]
        fallback_used: Option<bool>,
        #[serde(rename = "completedMs", skip_serializing_if = "Option::is_none")]
        completed_ms: Option<u64>,
    },
    Cancelled,
    Error {
        code: ErrorCode,
        #[serde(rename = "authRevision", skip_serializing_if = "Option::is_none")]
        auth_revision: Option<String>,
    },
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

    pub fn with_search_telemetry(
        mut self,
        search_ms: u64,
        search_status: &'static str,
        fallback_used: bool,
        completed_ms: u64,
    ) -> Self {
        if let Self::Complete {
            search_ms: out_search_ms,
            search_status: out_search_status,
            fallback_used: out_fallback_used,
            completed_ms: out_completed_ms,
            ..
        } = &mut self
        {
            *out_search_ms = Some(search_ms);
            *out_search_status = Some(search_status);
            *out_fallback_used = Some(fallback_used);
            *out_completed_ms = Some(completed_ms);
        }
        self
    }

    pub fn from_response(result: Result<CustomOpenAiResponse, CustomOpenAiError>) -> Self {
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
            Ok(response) => Self::Complete {
                text: response.text,
                sources: Vec::new(),
                content_type: response.content_type.as_str(),
                returned_model: response.model,
                auth_revision: None,
                speech_text: None,
                search_ms: None,
                search_status: None,
                fallback_used: None,
                completed_ms: None,
            },
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
    ) -> Self {
        let speech_text = response.text.clone();
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
        let mut outcome = Self::from_response(Ok(response));
        if let Self::Complete {
            sources,
            speech_text: speech,
            ..
        } = &mut outcome
        {
            *sources = canonical_sources;
            *speech = Some(speech_text);
        }
        outcome
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use voxgolem_platform::custom_openai::{CustomOpenAiContentType, CustomOpenAiTimings};

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
        let outcome = Outcome::from_response(Ok(response(
            "Synthetic \"answer\"\n",
            CustomOpenAiContentType::Refusal,
        )));
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
            )
            .json(),
        )
        .unwrap();
        assert_eq!(value["text"], "Synthetic answer");
        assert_eq!(value["sources"], serde_json::json!([source]));
        assert_eq!(value["speechText"], "Synthetic answer");
        assert_eq!(
            Outcome::from_search_response(
                response(
                    &"x".repeat(MAX_OUTPUT_BYTES),
                    CustomOpenAiContentType::OutputText
                ),
                vec![source.clone()]
            )
            .json(),
            r#"{"status":"error","code":"too_large"}"#
        );
        let boundary = "🧪".repeat(MAX_OUTPUT_BYTES / 4);
        let value: serde_json::Value = serde_json::from_str(
            &Outcome::from_search_response(
                response(&boundary, CustomOpenAiContentType::OutputText),
                vec![],
            )
            .json(),
        )
        .unwrap();
        assert_eq!(value["text"], boundary);
        let refusal: serde_json::Value = serde_json::from_str(
            &Outcome::from_search_response(
                response("Synthetic refusal", CustomOpenAiContentType::Refusal),
                vec![source],
            )
            .json(),
        )
        .unwrap();
        assert_eq!(refusal["sources"], serde_json::json!([]));
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
            let value: serde_json::Value =
                serde_json::from_str(&Outcome::from_response(Err(error)).json()).unwrap();
            assert_eq!(value, serde_json::json!({"status": "error", "code": code}));
        }
    }

    #[test]
    fn output_envelope_remains_bounded_and_empty_output_is_not_success() {
        assert_eq!(
            Outcome::from_response(Ok(response(
                &"x".repeat(MAX_OUTPUT_BYTES + 1),
                CustomOpenAiContentType::OutputText
            )))
            .json(),
            r#"{"status":"error","code":"too_large"}"#
        );
        assert_eq!(
            Outcome::from_response(Ok(response(" ", CustomOpenAiContentType::OutputText))).json(),
            r#"{"status":"error","code":"invalid_response"}"#
        );
    }
}
