use super::envelope::{checked_envelope_with_check, InputProjection};
use super::*;

#[derive(Clone, Debug, PartialEq)]
pub struct ToolCall {
    pub call_id: String,
    pub name: String,
    pub arguments: Value,
}

#[derive(Default)]
pub struct ToolResponseDecoder {
    pub output: Vec<Value>,
    pub calls: Vec<ToolCall>,
    text: OutputAccumulator,
    metadata: Option<ResponseMetadata>,
}

impl ToolResponseDecoder {
    pub fn event(&mut self, event: Value) -> Result<bool, CustomOpenAiError> {
        match event["type"].as_str() {
            Some("response.output_text.delta" | "response.refusal.delta") => {
                let delta = event["delta"]
                    .as_str()
                    .ok_or(CustomOpenAiError::MalformedStream)?;
                let kind = if event["type"] == "response.refusal.delta" {
                    CustomOpenAiContentType::Refusal
                } else {
                    CustomOpenAiContentType::OutputText
                };
                self.text.push(delta, kind)?;
            }
            Some("response.output_item.done") => self.item(event["item"].clone())?,
            Some("response.completed" | "response.done") => {
                let response = event
                    .get("response")
                    .ok_or(CustomOpenAiError::MalformedStream)?;
                if response["error"] != Value::Null
                    || response["incomplete_details"] != Value::Null
                    || response
                        .get("status")
                        .is_some_and(|status| status != "completed")
                {
                    return Err(CustomOpenAiError::Incomplete);
                }
                if let Some(output) = response.get("output") {
                    let output = output
                        .as_array()
                        .ok_or(CustomOpenAiError::MalformedStream)?;
                    let mut terminal = Self::default();
                    for item in output {
                        terminal.item(item.clone())?;
                    }
                    // Item completions are provisional until the terminal output confirms them.
                    // Check the entire set before handing any call to an external executor.
                    if self.calls.iter().any(|call| !terminal.calls.contains(call))
                        || terminal.calls.len()
                            != output
                                .iter()
                                .filter(|item| item["type"] == "function_call")
                                .count()
                    {
                        return Err(CustomOpenAiError::MalformedStream);
                    }
                    for item in &self.output {
                        if item["type"] == "function_call" && !terminal.output.contains(item) {
                            return Err(CustomOpenAiError::MalformedStream);
                        }
                    }
                    self.calls = terminal.calls;
                    self.output = terminal.output;
                } else if !self.calls.is_empty() {
                    return Err(CustomOpenAiError::MalformedStream);
                }
                if self.output.is_empty() && self.text.text.trim().is_empty() {
                    return Err(CustomOpenAiError::EmptyResponse);
                }
                self.metadata = Some(
                    serde_json::from_value(response.clone())
                        .map_err(|_| CustomOpenAiError::MalformedStream)?,
                );
                return Ok(true);
            }
            Some("response.failed" | "error") => return Err(CustomOpenAiError::Failed),
            Some("response.incomplete") => return Err(CustomOpenAiError::Incomplete),
            Some(_) => {}
            None => return Err(CustomOpenAiError::MalformedStream),
        }
        Ok(false)
    }

    fn item(&mut self, item: Value) -> Result<(), CustomOpenAiError> {
        if serde_json::to_vec(&item)
            .map_err(|_| CustomOpenAiError::MalformedStream)?
            .len()
            > MAX_EVENT_BYTES
        {
            return Err(CustomOpenAiError::TooLarge);
        }
        if item["type"] == "function_call" {
            if item
                .get("status")
                .is_some_and(|status| status != "completed")
            {
                return Err(CustomOpenAiError::Incomplete);
            }
            let call_id = item["call_id"]
                .as_str()
                .filter(|id| !id.is_empty() && id.len() <= 256)
                .ok_or(CustomOpenAiError::MalformedStream)?;
            let name = item["name"]
                .as_str()
                .filter(|name| !name.is_empty() && name.len() <= 128)
                .ok_or(CustomOpenAiError::MalformedStream)?;
            let raw = item["arguments"]
                .as_str()
                .ok_or(CustomOpenAiError::MalformedStream)?;
            if raw.len() > 8 * 1024 {
                return Err(CustomOpenAiError::TooLarge);
            }
            let call = ToolCall {
                call_id: call_id.into(),
                name: name.into(),
                arguments: serde_json::from_str(raw).unwrap_or_else(|_| Value::String(raw.into())),
            };
            if let Some(previous) = self
                .calls
                .iter()
                .find(|previous| previous.call_id == call_id)
            {
                if previous != &call {
                    return Err(CustomOpenAiError::MalformedStream);
                }
            } else {
                if self.calls.len() >= 16 {
                    return Err(CustomOpenAiError::TooLarge);
                }
                self.calls.push(call);
            }
        }
        if let Some(id) = item["id"].as_str() {
            if let Some(previous) = self.output.iter().find(|previous| previous["id"] == id) {
                if previous != &item {
                    return Err(CustomOpenAiError::MalformedStream);
                }
                return Ok(());
            }
        }
        if self.output.len() >= 64 {
            return Err(CustomOpenAiError::TooLarge);
        }
        self.output.push(item);
        Ok(())
    }

    fn finish(self) -> Result<ModelRound, CustomOpenAiError> {
        let mut terminal = OutputAccumulator::default();
        for content in self
            .output
            .iter()
            .filter(|item| item["type"] == "message")
            .filter_map(|item| item["content"].as_array())
            .flatten()
        {
            let (text, kind) = match content["type"].as_str() {
                Some("output_text") => (
                    content["text"]
                        .as_str()
                        .ok_or(CustomOpenAiError::MalformedStream)?,
                    CustomOpenAiContentType::OutputText,
                ),
                Some("refusal") => (
                    content["refusal"]
                        .as_str()
                        .ok_or(CustomOpenAiError::MalformedStream)?,
                    CustomOpenAiContentType::Refusal,
                ),
                _ => continue,
            };
            terminal.push(text, kind)?;
        }
        if self
            .text
            .content_type
            .zip(terminal.content_type)
            .is_some_and(|(streamed, final_kind)| streamed != final_kind)
        {
            return Err(CustomOpenAiError::MixedContent);
        }
        let accumulator = if !terminal.text.is_empty() {
            terminal
        } else {
            self.text
        };
        let text = accumulator.text;
        let content_type = accumulator
            .content_type
            .unwrap_or(CustomOpenAiContentType::OutputText);
        if self.calls.is_empty() && text.trim().is_empty() {
            return Err(CustomOpenAiError::EmptyResponse);
        }
        if text.len() > MAX_OUTPUT_BYTES {
            return Err(CustomOpenAiError::TooLarge);
        }
        let metadata = self.metadata.ok_or(CustomOpenAiError::Incomplete)?;
        if metadata
            .model
            .as_ref()
            .is_some_and(|model| model.len() > 256 || model.trim().is_empty())
        {
            return Err(CustomOpenAiError::MalformedStream);
        }
        Ok(ModelRound {
            response: CustomOpenAiResponse {
                text,
                content_type,
                model: metadata.model,
                service_tier: metadata.service_tier,
                usage: metadata.usage.map(Into::into),
                timings: CustomOpenAiTimings::default(),
            },
            calls: self.calls,
            output: self.output,
            auth_revision: None,
        })
    }
}

pub struct ModelRound {
    pub response: CustomOpenAiResponse,
    pub calls: Vec<ToolCall>,
    pub output: Vec<Value>,
    pub auth_revision: Option<CustomOpenAiAuthRevision>,
}

pub fn continuation(
    input: &mut Vec<Value>,
    output: Vec<Value>,
    results: &[Value],
) -> Result<(), CustomOpenAiError> {
    let calls = output
        .iter()
        .filter(|item| item["type"] == "function_call")
        .collect::<Vec<_>>();
    if calls.len() != results.len()
        || calls.iter().any(|call| {
            results
                .iter()
                .filter(|result| {
                    result["type"] == "function_call_output"
                        && result["call_id"] == call["call_id"]
                        && result["output"].is_string()
                })
                .count()
                != 1
        })
    {
        return Err(CustomOpenAiError::MalformedStream);
    }
    input.extend(output);
    input.extend_from_slice(results);
    Ok(())
}

pub fn initial_input(config: &CustomOpenAiConfig, prompt: &CustomOpenAiPrompt) -> Vec<Value> {
    build_request(config, prompt, None)["input"]
        .as_array()
        .unwrap()
        .clone()
}

pub fn request_body(
    config: &CustomOpenAiConfig,
    session_id: &str,
    input: &[Value],
    instructions: &str,
    tools: &[Value],
    require_tool: bool,
) -> Value {
    envelope::tool_value(config, session_id, input, instructions, tools, require_tool)
}

pub struct RoundOptions {
    pub deadline: tokio::time::Instant,
    pub expected_auth_revision: Option<CustomOpenAiAuthRevision>,
    pub require_tool: bool,
}

pub struct RoundRequest<'a> {
    pub session_id: &'a str,
    pub input: &'a [Value],
    pub instructions: &'a str,
    pub tools: &'a [Value],
}

pub enum ModelEvent<'a> {
    AuthRevision(CustomOpenAiAuthRevision),
    RequestDispatched(Instant),
    TextDelta {
        text: &'a str,
        content_type: CustomOpenAiContentType,
    },
}

#[derive(Debug)]
pub enum RoundError {
    Provider(CustomOpenAiError),
    CallbackFailed,
}

impl From<CustomOpenAiError> for RoundError {
    fn from(error: CustomOpenAiError) -> Self {
        Self::Provider(error)
    }
}

impl CustomOpenAiClient {
    pub async fn tool_round(
        &self,
        session_id: &str,
        input: &[Value],
        instructions: &str,
        tools: &[Value],
        options: RoundOptions,
    ) -> Result<ModelRound, CustomOpenAiError> {
        self.tool_round_with_events(
            RoundRequest {
                session_id,
                input,
                instructions,
                tools,
            },
            options,
            |_| Ok(()),
        )
        .await
        .map_err(|error| match error {
            RoundError::Provider(error) => error,
            RoundError::CallbackFailed => CustomOpenAiError::Failed,
        })
    }

    pub async fn tool_round_with_events(
        &self,
        request: RoundRequest<'_>,
        options: RoundOptions,
        on_event: impl FnMut(ModelEvent<'_>) -> Result<(), ()>,
    ) -> Result<ModelRound, RoundError> {
        let RoundRequest {
            session_id,
            input,
            instructions,
            tools,
        } = request;
        self.projected_round_with_events(
            session_id,
            InputProjection::mandatory(input),
            instructions,
            tools,
            options,
            || Ok(()),
            on_event,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn projected_round_with_events(
        &self,
        session_id: &str,
        input: InputProjection<'_>,
        instructions: &str,
        tools: &[Value],
        options: RoundOptions,
        mut preparation_check: impl FnMut() -> Result<(), CustomOpenAiError>,
        mut on_event: impl FnMut(ModelEvent<'_>) -> Result<(), ()>,
    ) -> Result<ModelRound, RoundError> {
        let started = Instant::now();
        let deadline = options.deadline;
        if tokio::time::Instant::now() >= deadline {
            return Err(CustomOpenAiError::Timeout.into());
        }
        let request = checked_envelope_with_check(
            &self.config,
            session_id,
            input,
            instructions,
            Some(tools),
            options.require_tool,
            &mut preparation_check,
        )?;
        if tokio::time::Instant::now() >= deadline {
            return Err(CustomOpenAiError::Timeout.into());
        }
        let credential = load_request_credential(&self.config.auth_path)?;
        let auth_revision = credential.revision;
        if options
            .expected_auth_revision
            .is_some_and(|expected| expected != auth_revision)
        {
            return Err(CustomOpenAiError::AuthUnavailable.into());
        }
        on_event(ModelEvent::AuthRevision(auth_revision))
            .map_err(|_| RoundError::CallbackFailed)?;
        preparation_check()?;
        if tokio::time::Instant::now() >= deadline {
            return Err(CustomOpenAiError::Timeout.into());
        }
        let mut builder = self
            .client
            .post(&self.config.endpoint)
            .header(ACCEPT, "text/event-stream")
            .header(AUTHORIZATION, credential.authorization)
            .header(CONTENT_TYPE, "application/json")
            .header("Originator", "opencode")
            .header("session-id", session_id)
            .timeout(deadline.saturating_duration_since(tokio::time::Instant::now()));
        if let Some(account_id) = credential.account_id {
            builder = builder.header("ChatGPT-Account-Id", account_id);
        }
        tokio::time::timeout_at(deadline, async {
            let builder = builder.body(request);
            on_event(ModelEvent::RequestDispatched(Instant::now()))
                .map_err(|_| RoundError::CallbackFailed)?;
            preparation_check()?;
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                return Err(CustomOpenAiError::Timeout.into());
            }
            let response = builder
                .timeout(remaining)
                .send()
                .await
                .map_err(map_transport_error)?;
            let headers_at = started.elapsed();
            if !response.status().is_success() {
                return Err(map_http_status(response.status()).into());
            }
            if !supported_stream_content_type(response.headers().get(CONTENT_TYPE)) {
                return Err(CustomOpenAiError::UnexpectedContentType.into());
            }
            let mut stream = response.bytes_stream();
            let mut framing = SseDecoder::default();
            let mut decoder = ToolResponseDecoder::default();
            let mut total = 0;
            let mut first_text_at = None;
            let mut streamed_text = false;
            loop {
                let chunk = tokio::time::timeout(self.config.idle_timeout, stream.next())
                    .await
                    .map_err(|_| CustomOpenAiError::Timeout)?
                    .ok_or(CustomOpenAiError::Incomplete)?
                    .map_err(map_transport_error)?;
                total += chunk.len();
                if total > MAX_STREAM_BYTES {
                    return Err(CustomOpenAiError::TooLarge.into());
                }
                framing.push(&chunk);
                while let Some(data) = framing.next_data()? {
                    if data.is_empty() || data == "[DONE]" {
                        continue;
                    }
                    let event: Value = serde_json::from_str(&data)
                        .map_err(|_| CustomOpenAiError::MalformedStream)?;
                    let delta = match event["type"].as_str() {
                        Some("response.output_text.delta") => event["delta"]
                            .as_str()
                            .map(|text| (text.to_string(), CustomOpenAiContentType::OutputText)),
                        Some("response.refusal.delta") => event["delta"]
                            .as_str()
                            .map(|text| (text.to_string(), CustomOpenAiContentType::Refusal)),
                        _ => None,
                    };
                    let completed = decoder.event(event)?;
                    if let Some((text, kind)) = delta {
                        if !text.is_empty() {
                            streamed_text = true;
                            record_first_visible_text(&mut first_text_at, started, &text);
                            on_event(ModelEvent::TextDelta {
                                text: &text,
                                content_type: kind,
                            })
                            .map_err(|_| RoundError::CallbackFailed)?;
                        }
                    }
                    if completed {
                        let mut round = decoder.finish()?;
                        round.auth_revision = Some(auth_revision);
                        if !streamed_text && !round.response.text.is_empty() {
                            record_first_visible_text(
                                &mut first_text_at,
                                started,
                                &round.response.text,
                            );
                            on_event(ModelEvent::TextDelta {
                                text: &round.response.text,
                                content_type: round.response.content_type,
                            })
                            .map_err(|_| RoundError::CallbackFailed)?;
                        }
                        round.response.timings = CustomOpenAiTimings {
                            headers_at,
                            first_text_at,
                            completed_at: started.elapsed(),
                        };
                        return Ok(round);
                    }
                }
            }
        })
        .await
        .map_err(|_| RoundError::Provider(CustomOpenAiError::Timeout))?
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn failed_dispatch_callback_prevents_the_http_request() {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let id = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "vox-synthetic-dispatch-failure-{}-{id}.json",
            std::process::id()
        ));
        std::fs::write(
            &path,
            r#"{"openai":{"access":"synthetic-dispatch","expires":4102444800}}"#,
        )
        .unwrap();
        let client = CustomOpenAiClient::new(CustomOpenAiConfig {
            endpoint: format!("http://{}", listener.local_addr().unwrap()),
            auth_path: path.clone(),
            ..Default::default()
        })
        .unwrap();
        let result = client
            .tool_round_with_events(
                RoundRequest {
                    session_id: "synthetic-dispatch",
                    input: &[],
                    instructions: "Synthetic",
                    tools: &[],
                },
                RoundOptions {
                    deadline: tokio::time::Instant::now() + Duration::from_secs(1),
                    expected_auth_revision: None,
                    require_tool: false,
                },
                |event| {
                    if matches!(event, ModelEvent::RequestDispatched(_)) {
                        Err(())
                    } else {
                        Ok(())
                    }
                },
            )
            .await;
        std::fs::remove_file(path).unwrap();
        assert!(matches!(result, Err(RoundError::CallbackFailed)));
        assert!(
            tokio::time::timeout(Duration::from_millis(20), listener.accept())
                .await
                .is_err()
        );
    }

    #[test]
    fn terminal_only_refusal_retains_the_current_native_response_kind() {
        let mut decoder = ToolResponseDecoder::default();
        decoder.event(json!({"type":"response.completed","response":{"status":"completed","output":[{"type":"message","id":"synthetic-refusal","content":[{"type":"refusal","refusal":"Synthetic refusal"}]}]}})).unwrap();
        let round = decoder.finish().unwrap();
        assert_eq!(
            round.response.content_type,
            CustomOpenAiContentType::Refusal
        );
        assert_eq!(round.response.text, "Synthetic refusal");
    }

    #[test]
    fn terminal_only_mixed_content_obeys_the_existing_accumulator_contract() {
        let mut decoder = ToolResponseDecoder::default();
        decoder.event(json!({"type":"response.completed","response":{"status":"completed","output":[{"type":"message","id":"synthetic-mixed","content":[{"type":"output_text","text":"Synthetic text"},{"type":"refusal","refusal":"Synthetic refusal"}]}]}})).unwrap();
        assert!(matches!(
            decoder.finish(),
            Err(CustomOpenAiError::MixedContent)
        ));
    }

    #[test]
    fn completed_tool_path_retains_existing_response_metadata() {
        let mut decoder = ToolResponseDecoder::default();
        decoder
            .event(json!({"type":"response.output_text.delta","delta":"Synthetic answer"}))
            .unwrap();
        decoder.event(json!({"type":"response.completed","response":{"model":"synthetic-returned-model","service_tier":"synthetic-tier","usage":{"input_tokens":10,"output_tokens":4,"total_tokens":14,"input_tokens_details":{"cached_tokens":3}}}})).unwrap();
        let round = decoder.finish().unwrap();
        assert_eq!(
            round.response.model.as_deref(),
            Some("synthetic-returned-model")
        );
        assert_eq!(
            round.response.service_tier.as_deref(),
            Some("synthetic-tier")
        );
        assert_eq!(round.response.usage.unwrap().cached_input_tokens, Some(3));
    }

    #[test]
    fn arguments_never_execute_before_a_completed_call_item() {
        let mut decoder = ToolResponseDecoder::default();
        decoder.event(json!({"type":"response.function_call_arguments.delta", "item_id":"synthetic-item", "delta":"{\"objective\":"})).unwrap();
        decoder.event(json!({"type":"response.function_call_arguments.done", "item_id":"synthetic-item", "arguments":"{\"objective\":\"Synthetic\"}"})).unwrap();
        assert!(decoder.calls.is_empty());
        decoder.event(json!({"type":"response.output_item.done", "item":{
            "type":"function_call", "id":"synthetic-item", "call_id":"synthetic-call",
            "name":"web_search", "arguments":"{\"objective\":\"Synthetic\"}", "status":"completed"
        }})).unwrap();
        assert_eq!(
            decoder.calls,
            vec![ToolCall {
                call_id: "synthetic-call".into(),
                name: "web_search".into(),
                arguments: json!({"objective":"Synthetic"})
            }]
        );
    }

    #[test]
    fn duplicate_completion_is_deduplicated_but_conflicting_call_id_fails() {
        let mut decoder = ToolResponseDecoder::default();
        let event = json!({"type":"response.output_item.done", "item":{
            "type":"function_call", "id":"synthetic-item", "call_id":"synthetic-call", "name":"web_search", "arguments":"{}", "status":"completed"
        }});
        decoder.event(event.clone()).unwrap();
        decoder.event(event.clone()).unwrap();
        assert_eq!(decoder.calls.len(), 1);
        let mut conflict = event;
        conflict["item"]["arguments"] = json!("{\"objective\":\"different\"}");
        assert_eq!(
            decoder.event(conflict),
            Err(CustomOpenAiError::MalformedStream)
        );
    }

    #[test]
    fn tool_only_terminal_preserves_opaque_reasoning_and_call_linked_output() {
        let mut decoder = ToolResponseDecoder::default();
        let output = json!([
            {"type":"reasoning", "id":"synthetic-reasoning", "summary":[], "encrypted_content":"synthetic-opaque"},
            {"type":"function_call", "id":"synthetic-item", "call_id":"synthetic-call", "name":"web_search", "arguments":"{}", "status":"completed"}
        ]);
        assert!(decoder.event(json!({"type":"response.completed", "response":{"status":"completed", "output":output}})).unwrap());
        assert_eq!(decoder.calls.len(), 1);
        let mut input = vec![
            json!({"role":"user", "content":[{"type":"input_text", "text":"Synthetic question"}]}),
        ];
        continuation(&mut input, decoder.output, &[json!({"type":"function_call_output", "call_id":"synthetic-call", "output":"Synthetic result"})]).unwrap();
        assert_eq!(input[1]["encrypted_content"], "synthetic-opaque");
        assert_eq!(input[2]["call_id"], input[3]["call_id"]);
        assert!(input[3].get("id").is_none());
    }
}
