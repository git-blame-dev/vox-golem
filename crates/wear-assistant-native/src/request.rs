use crate::ErrorCode;
use serde::Deserialize;
use voxgolem_core::search::Mode;
use voxgolem_platform::custom_openai::{
    CustomOpenAiContentType, CustomOpenAiMessage, CustomOpenAiModel, CustomOpenAiPrompt,
    CustomOpenAiRole,
};

pub(crate) const MAX_REQUEST_BYTES: usize = 384 * 1024;
pub(crate) const MAX_REQUEST_ID_BYTES: usize = 128;
pub(crate) const MAX_OUTPUT_BYTES: usize = 256 * 1024;
const MAX_INSTRUCTIONS_BYTES: usize = 4096;
const MAX_PROMPT_BYTES: usize = 32 * 1024;
const MAX_MESSAGES: usize = 41;
const MAX_TEXT_BYTES: usize = 256 * 1024;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WireRequest {
    request_id: String,
    #[serde(default = "default_model")]
    model: String,
    messages: Vec<WireMessage>,
    instructions: String,
    #[serde(default = "default_web_enabled")]
    web_enabled: bool,
    #[serde(default)]
    external_session_id: Option<String>,
    #[serde(default)]
    search_mode: Option<Mode>,
    #[serde(default)]
    conversation_id: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WireMessage {
    role: WireRole,
    content: String,
    #[serde(default)]
    content_type: WireContentType,
}

#[derive(Clone, Copy, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
enum WireRole {
    User,
    Assistant,
}

#[derive(Clone, Copy, Default, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
enum WireContentType {
    #[default]
    OutputText,
    Refusal,
}

fn default_model() -> String {
    "custom-sol-high".to_string()
}

fn default_web_enabled() -> bool {
    true
}

pub(crate) struct Request {
    pub request_id: String,
    pub model: CustomOpenAiModel,
    pub prompt: CustomOpenAiPrompt,
    pub instructions: String,
    pub web_enabled: bool,
    pub search_mode: Option<Mode>,
}

pub(crate) fn valid_request_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_REQUEST_ID_BYTES
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_.:".contains(&byte))
}

pub(crate) fn parse(input: &str) -> Result<Request, ErrorCode> {
    if input.len() > MAX_REQUEST_BYTES {
        return Err(ErrorCode::InvalidRequest);
    }
    let mut wire: WireRequest =
        serde_json::from_str(input).map_err(|_| ErrorCode::InvalidRequest)?;
    if !valid_request_id(&wire.request_id)
        || wire
            .external_session_id
            .as_ref()
            .is_some_and(|id| !valid_request_id(id))
        || wire
            .conversation_id
            .as_ref()
            .is_some_and(|id| !valid_conversation_id(id))
        || (wire.search_mode.is_some()
            && wire.conversation_id.is_none()
            && wire.external_session_id.is_none())
        || wire.instructions.trim().is_empty()
        || wire.instructions.len() > MAX_INSTRUCTIONS_BYTES
        || wire.messages.is_empty()
        || wire.messages.len() > MAX_MESSAGES
    {
        return Err(ErrorCode::InvalidRequest);
    }
    let model = match wire.model.as_str() {
        "custom-sol-high" => CustomOpenAiModel::SolHigh,
        "custom-sol-none" => CustomOpenAiModel::SolNone,
        "custom-luna-none" => CustomOpenAiModel::LunaNone,
        "custom-luna-low" => CustomOpenAiModel::LunaLow,
        "local-fast" => CustomOpenAiModel::LocalFast,
        _ => return Err(ErrorCode::InvalidRequest),
    };
    let text_bytes: usize = wire
        .messages
        .iter()
        .map(|message| message.content.len())
        .sum();
    if text_bytes > MAX_TEXT_BYTES
        || wire.messages.iter().any(|message| {
            message.content.trim().is_empty()
                || (message.role == WireRole::User
                    && (message.content.len() > MAX_PROMPT_BYTES
                        || message.content_type == WireContentType::Refusal))
        })
    {
        return Err(ErrorCode::InvalidRequest);
    }
    let current = wire.messages.pop().ok_or(ErrorCode::InvalidRequest)?;
    if current.role != WireRole::User {
        return Err(ErrorCode::InvalidRequest);
    }
    let history = wire
        .messages
        .into_iter()
        .map(|message| CustomOpenAiMessage {
            role: match message.role {
                WireRole::User => CustomOpenAiRole::User,
                WireRole::Assistant => CustomOpenAiRole::Assistant,
            },
            content_type: match message.content_type {
                WireContentType::OutputText => CustomOpenAiContentType::OutputText,
                WireContentType::Refusal => CustomOpenAiContentType::Refusal,
            },
            text: message.content,
        })
        .collect();
    Ok(Request {
        prompt: CustomOpenAiPrompt {
            session_id: voxgolem_core::external_correlation::external_id(
                wire.external_session_id
                    .as_deref()
                    .or(wire.conversation_id.as_deref())
                    .unwrap_or(&wire.request_id),
            ),
            prompt: current.content,
            history,
        },
        request_id: wire.request_id,
        model,
        instructions: wire.instructions,
        web_enabled: wire.web_enabled,
        search_mode: wire.search_mode,
    })
}

fn valid_conversation_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 256
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_.:".contains(&byte))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use voxgolem_platform::custom_openai::{CustomOpenAiContentType, CustomOpenAiRole};

    fn fixture() -> serde_json::Value {
        json!({
            "requestId": "synthetic-request-1",
            "instructions": "Answer briefly for a watch. No tools.",
            "messages": [
                {"role": "user", "content": "Synthetic earlier question"},
                {"role": "assistant", "content": "Synthetic earlier answer"},
                {"role": "user", "content": "Synthetic current question"}
            ]
        })
    }

    #[test]
    fn search_requires_valid_mode_and_stable_conversation_identity() {
        for mode in ["quick", "verify", "research"] {
            let mut value = fixture();
            value["searchMode"] = json!(mode);
            assert!(parse(&value.to_string()).is_err());
            value["conversationId"] = json!("synthetic-replica:1");
            let first = parse(&value.to_string()).expect("valid search request");
            assert!(!first.prompt.session_id.contains("synthetic"));
            value["requestId"] = json!("synthetic-request-2");
            let second = parse(&value.to_string()).unwrap();
            assert_eq!(first.prompt.session_id, second.prompt.session_id);
        }
        for (field, replacement) in [
            ("searchMode", json!("unknown")),
            ("searchMode", json!("Quick")),
            ("conversationId", json!("")),
            ("conversationId", json!("synthetic identity with spaces")),
            ("conversationId", json!("x".repeat(257))),
        ] {
            let mut value = fixture();
            value["searchMode"] = json!("quick");
            value["conversationId"] = json!("synthetic-replica:1");
            value[field] = replacement;
            assert!(matches!(
                parse(&value.to_string()),
                Err(ErrorCode::InvalidRequest)
            ));
        }
    }

    #[test]
    fn maps_messages_to_existing_provider_prompt_without_protocol_duplication() {
        let request = parse(&fixture().to_string()).unwrap();
        assert_eq!(request.request_id, "synthetic-request-1");
        assert_eq!(request.model, CustomOpenAiModel::SolHigh);
        assert_ne!(request.prompt.session_id, request.request_id);
        assert_eq!(request.prompt.prompt, "Synthetic current question");
        assert_eq!(request.prompt.history.len(), 2);
        assert_eq!(request.prompt.history[0].role, CustomOpenAiRole::User);
        assert_eq!(request.prompt.history[1].role, CustomOpenAiRole::Assistant);
        assert_eq!(
            request.instructions,
            "Answer briefly for a watch. No tools."
        );
    }

    #[test]
    fn native_admission_captures_web_permission_and_opaque_external_lifetime() {
        let legacy = parse(&fixture().to_string()).unwrap();
        assert!(legacy.web_enabled);
        let mut value = fixture();
        value["webEnabled"] = json!(false);
        value["externalSessionId"] = json!("synthetic-process-session-1");
        value["searchMode"] = json!("research");
        let first = parse(&value.to_string()).unwrap();
        assert!(!first.web_enabled);
        value["requestId"] = json!("synthetic-request-2");
        let second = parse(&value.to_string()).unwrap();
        assert_eq!(first.prompt.session_id, second.prompt.session_id);
        value["externalSessionId"] = json!("synthetic-process-session-2");
        assert_ne!(
            first.prompt.session_id,
            parse(&value.to_string()).unwrap().prompt.session_id
        );
        for invalid in [json!("false"), json!(null), json!(1)] {
            value["webEnabled"] = invalid;
            assert!(parse(&value.to_string()).is_err());
        }
    }

    #[test]
    fn preserves_explicit_model_and_refusal_history() {
        let mut value = fixture();
        value["model"] = json!("custom-luna-low");
        value["messages"][1]["contentType"] = json!("refusal");
        let request = parse(&value.to_string()).unwrap();
        assert_eq!(request.model, CustomOpenAiModel::LunaLow);
        assert_eq!(
            request.prompt.history[1].content_type,
            CustomOpenAiContentType::Refusal
        );
    }

    #[test]
    fn rejects_invalid_ids_models_roles_and_missing_trusted_instructions() {
        for (field, replacement) in [
            ("requestId", json!("")),
            ("requestId", json!("x".repeat(129))),
            ("model", json!("unconfigured-provider")),
            ("instructions", json!("")),
            ("instructions", json!("x".repeat(4097))),
            ("messages", json!([])),
            (
                "messages",
                json!([{"role": "system", "content": "Synthetic"}]),
            ),
            (
                "messages",
                json!([{"role": "assistant", "content": "Synthetic"}]),
            ),
            ("messages", json!([{"role": "user", "content": " "}])),
            (
                "messages",
                json!([{"role": "user", "content": "x".repeat(32769)}]),
            ),
        ] {
            let mut value = fixture();
            value[field] = replacement;
            assert!(matches!(
                parse(&value.to_string()),
                Err(ErrorCode::InvalidRequest)
            ));
        }
        assert!(matches!(parse("not JSON"), Err(ErrorCode::InvalidRequest)));
        let mut value = fixture();
        value["endpoint"] = json!("https://synthetic.invalid");
        assert!(matches!(
            parse(&value.to_string()),
            Err(ErrorCode::InvalidRequest)
        ));
    }

    #[test]
    fn enforces_raw_json_message_count_and_aggregate_utf8_limits() {
        assert!(parse(&" ".repeat(MAX_REQUEST_BYTES + 1)).is_err());
        let mut value = fixture();
        value["messages"] = json!((0..MAX_MESSAGES + 1)
            .map(|_| json!({"role": "user", "content": "Synthetic"}))
            .collect::<Vec<_>>());
        assert!(parse(&value.to_string()).is_err());
        value["messages"] = json!([
            {"role": "assistant", "content": "x".repeat(MAX_TEXT_BYTES)},
            {"role": "user", "content": "Synthetic"}
        ]);
        assert!(parse(&value.to_string()).is_err());
        value["messages"] = json!([{"role": "user", "content": "🧪".repeat(MAX_PROMPT_BYTES / 4)}]);
        assert!(parse(&value.to_string()).is_ok());
        value["messages"][0]["content"] = json!("🧪".repeat(MAX_PROMPT_BYTES / 4 + 1));
        assert!(parse(&value.to_string()).is_err());
        value["messages"] =
            json!([{"role": "user", "content": "Synthetic", "contentType": "refusal"}]);
        assert!(parse(&value.to_string()).is_err());
    }
}
