use crate::{request::MAX_OUTPUT_BYTES, ErrorCode};
use serde::Serialize;
use voxgolem_platform::custom_openai::{CustomOpenAiError, CustomOpenAiResponse};

#[derive(Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub(crate) enum Outcome {
    Complete {
        text: String,
        #[serde(rename = "contentType")]
        content_type: &'static str,
        #[serde(rename = "returnedModel", skip_serializing_if = "Option::is_none")]
        returned_model: Option<String>,
        #[serde(rename = "authRevision", skip_serializing_if = "Option::is_none")]
        auth_revision: Option<String>,
        #[serde(rename = "speechText", skip_serializing_if = "Option::is_none")]
        speech_text: Option<String>,
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
                content_type: response.content_type.as_str(),
                returned_model: response.model,
                auth_revision: None,
                speech_text: None,
            },
            Err(error) => Self::error(error.into()),
        }
    }

    pub fn json(&self) -> String {
        serde_json::to_string(self)
            .unwrap_or_else(|_| r#"{"status":"error","code":"internal_error"}"#.to_string())
    }

    pub fn from_search_response(response: CustomOpenAiResponse, persisted_text: String) -> Self {
        let speech_text = response.text.clone();
        let mut outcome = Self::from_response(Ok(response));
        if persisted_text.len() > MAX_OUTPUT_BYTES {
            return Self::error(ErrorCode::TooLarge);
        }
        if let Self::Complete {
            text,
            speech_text: speech,
            ..
        } = &mut outcome
        {
            *text = persisted_text;
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
                "returnedModel": "synthetic-model"
            })
        );
    }

    #[test]
    fn search_projection_bounds_persisted_sources_and_keeps_speech_answer_only() {
        let final_text = "Synthetic answer\n\nSources:\n- https://example.invalid/synthetic\n";
        let value: serde_json::Value = serde_json::from_str(
            &Outcome::from_search_response(
                response("Synthetic answer", CustomOpenAiContentType::OutputText),
                final_text.into(),
            )
            .json(),
        )
        .unwrap();
        assert_eq!(value["text"], final_text);
        assert_eq!(value["speechText"], "Synthetic answer");
        assert_eq!(
            Outcome::from_search_response(
                response("Synthetic answer", CustomOpenAiContentType::OutputText),
                "x".repeat(MAX_OUTPUT_BYTES + 1)
            )
            .json(),
            r#"{"status":"error","code":"too_large"}"#
        );
        let boundary = "🧪".repeat(MAX_OUTPUT_BYTES / 4);
        let value: serde_json::Value = serde_json::from_str(
            &Outcome::from_search_response(
                response("Synthetic answer", CustomOpenAiContentType::OutputText),
                boundary.clone(),
            )
            .json(),
        )
        .unwrap();
        assert_eq!(value["text"], boundary);
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
