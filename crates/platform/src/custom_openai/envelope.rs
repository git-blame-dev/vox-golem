use super::*;
use serde::ser::{Serialize, SerializeSeq, Serializer};
use std::io::{self, Write};

pub const MAX_REQUEST_BYTES: usize = 384 * 1024;

/// Only history supplied by the initial prompt is eligible for pair eviction.
/// Provider output and tool results are ordered, mandatory continuation data.
#[derive(Clone, Copy)]
pub(crate) struct InputProjection<'a> {
    pub history: &'a [CustomOpenAiMessage],
    pub current: Option<&'a str>,
    pub continuation: &'a [Value],
}

impl<'a> InputProjection<'a> {
    pub(crate) fn initial(prompt: &'a CustomOpenAiPrompt, continuation: &'a [Value]) -> Self {
        Self {
            history: &prompt.history,
            current: Some(&prompt.prompt),
            continuation,
        }
    }

    pub(crate) fn mandatory(input: &'a [Value]) -> Self {
        Self {
            history: &[],
            current: None,
            continuation: input,
        }
    }
}

fn optional_pair(history: &[CustomOpenAiMessage], start: usize) -> bool {
    history.get(start).is_some_and(|message| {
        message.role == CustomOpenAiRole::User
            && message.content_type == CustomOpenAiContentType::OutputText
    }) && history
        .get(start + 1)
        .is_some_and(|message| message.role == CustomOpenAiRole::Assistant)
}

#[derive(serde::Serialize)]
#[serde(tag = "type")]
enum ContentView<'a> {
    #[serde(rename = "input_text")]
    Input { text: &'a str },
    #[serde(rename = "output_text")]
    Output { text: &'a str },
    #[serde(rename = "refusal")]
    Refusal { refusal: &'a str },
}

#[derive(serde::Serialize)]
struct MessageView<'a> {
    role: &'static str,
    content: [ContentView<'a>; 1],
}

fn message_view(message: &CustomOpenAiMessage) -> MessageView<'_> {
    MessageView {
        role: message.role.as_str(),
        content: [match (message.role, message.content_type) {
            (CustomOpenAiRole::User, _) => ContentView::Input {
                text: &message.text,
            },
            (CustomOpenAiRole::Assistant, CustomOpenAiContentType::OutputText) => {
                ContentView::Output {
                    text: &message.text,
                }
            }
            (CustomOpenAiRole::Assistant, CustomOpenAiContentType::Refusal) => {
                ContentView::Refusal {
                    refusal: &message.text,
                }
            }
        }],
    }
}

fn current_view(text: &str) -> MessageView<'_> {
    MessageView {
        role: "user",
        content: [ContentView::Input { text }],
    }
}

struct SelectedInput<'a> {
    projection: InputProjection<'a>,
    optional_start: usize,
}

impl Serialize for SelectedInput<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut sequence = serializer.serialize_seq(None)?;
        let history = self.projection.history;
        let mut index = 0;
        while index < history.len() {
            if optional_pair(history, index) {
                if index >= self.optional_start {
                    sequence.serialize_element(&message_view(&history[index]))?;
                    sequence.serialize_element(&message_view(&history[index + 1]))?;
                }
                index += 2;
            } else {
                sequence.serialize_element(&message_view(&history[index]))?;
                index += 1;
            }
        }
        if let Some(current) = self.projection.current {
            sequence.serialize_element(&current_view(current))?;
        }
        for item in self.projection.continuation {
            sequence.serialize_element(item)?;
        }
        sequence.end()
    }
}

#[derive(serde::Serialize)]
struct ReasoningView {
    effort: &'static str,
}

#[derive(serde::Serialize)]
struct Envelope<'a> {
    model: &'static str,
    instructions: &'a str,
    input: SelectedInput<'a>,
    reasoning: ReasoningView,
    stream: bool,
    store: bool,
    prompt_cache_key: &'a str,
    parallel_tool_calls: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<&'a [Value]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    include: Option<[&'static str; 1]>,
}

/// The counter and final buffer use the same serializer and enforce the same limit.
struct BoundedWriter {
    bytes: Option<Vec<u8>>,
    len: usize,
    limit: usize,
}

impl Write for BoundedWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let next = self
            .len
            .checked_add(bytes.len())
            .filter(|next| *next <= self.limit)
            .ok_or_else(|| io::Error::other("request budget exceeded"))?;
        if let Some(buffer) = &mut self.bytes {
            buffer.extend_from_slice(bytes);
        }
        self.len = next;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn encoded_len(value: &impl Serialize, limit: usize) -> Result<usize, CustomOpenAiError> {
    let mut writer = BoundedWriter {
        bytes: None,
        len: 0,
        limit,
    };
    serde_json::to_writer(&mut writer, value).map_err(|_| CustomOpenAiError::TooLarge)?;
    Ok(writer.len)
}

pub(crate) fn checked_envelope(
    config: &CustomOpenAiConfig,
    session_id: &str,
    projection: InputProjection<'_>,
    instructions: &str,
    tools: Option<&[Value]>,
    require_tool: bool,
) -> Result<Vec<u8>, CustomOpenAiError> {
    checked_envelope_with_check(
        config,
        session_id,
        projection,
        instructions,
        tools,
        require_tool,
        || Ok(()),
    )
}

pub(crate) fn checked_envelope_with_check(
    config: &CustomOpenAiConfig,
    session_id: &str,
    projection: InputProjection<'_>,
    instructions: &str,
    tools: Option<&[Value]>,
    require_tool: bool,
    mut check: impl FnMut() -> Result<(), CustomOpenAiError>,
) -> Result<Vec<u8>, CustomOpenAiError> {
    check()?;
    if instructions.len() > MAX_REQUEST_BYTES
        || session_id.len() > MAX_REQUEST_BYTES
        || projection
            .current
            .is_some_and(|text| text.len() > MAX_REQUEST_BYTES)
    {
        return Err(CustomOpenAiError::TooLarge);
    }
    if session_id.trim().is_empty()
        || projection
            .current
            .is_some_and(|text| text.trim().is_empty())
    {
        return Err(CustomOpenAiError::InvalidPrompt);
    }
    let instructions = voxgolem_core::response_style::compose_instructions(instructions);
    let mut envelope = envelope(
        config,
        session_id,
        InputProjection::mandatory(&[]),
        &instructions,
        tools,
        require_tool,
    );
    let mut total = encoded_len(&envelope, MAX_REQUEST_BYTES)?;
    let mut count = 0;
    let mut add = |value_len: usize| -> Result<(), CustomOpenAiError> {
        total = total
            .checked_add(value_len)
            .and_then(|total| total.checked_add(usize::from(count > 0)))
            .filter(|total| *total <= MAX_REQUEST_BYTES)
            .ok_or(CustomOpenAiError::TooLarge)?;
        count += 1;
        Ok(())
    };
    let mut index = 0;
    while index < projection.history.len() {
        check()?;
        if optional_pair(projection.history, index) {
            index += 2;
        } else {
            add(encoded_len(
                &message_view(&projection.history[index]),
                MAX_REQUEST_BYTES,
            )?)?;
            index += 1;
        }
    }
    if let Some(current) = projection.current {
        check()?;
        add(encoded_len(&current_view(current), MAX_REQUEST_BYTES)?)?;
    }
    for item in projection.continuation {
        check()?;
        add(encoded_len(item, MAX_REQUEST_BYTES)?)?;
    }
    // Select a contiguous newest suffix of optional groups; mandatory items keep
    // their original positions even when they occur between optional groups.
    let mut optional_start = projection.history.len();
    let mut end = projection.history.len();
    while end > 0 {
        check()?;
        if end >= 2 && optional_pair(projection.history, end - 2) {
            let start = end - 2;
            let remaining = MAX_REQUEST_BYTES - total;
            let pair_len = (|| {
                let first = encoded_len(&message_view(&projection.history[start]), remaining)?;
                let second = encoded_len(
                    &message_view(&projection.history[start + 1]),
                    remaining.saturating_sub(first),
                )?;
                first
                    .checked_add(second)
                    .and_then(|len| len.checked_add(1 + usize::from(count > 0)))
                    .filter(|len| *len <= remaining)
                    .ok_or(CustomOpenAiError::TooLarge)
            })();
            let Ok(pair_len) = pair_len else {
                break;
            };
            total += pair_len;
            count += 2;
            optional_start = start;
            end = start;
        } else {
            end -= 1;
        }
    }
    envelope.input = SelectedInput {
        projection,
        optional_start,
    };
    check()?;
    let mut writer = BoundedWriter {
        bytes: Some(Vec::with_capacity(total)),
        len: 0,
        limit: MAX_REQUEST_BYTES,
    };
    serde_json::to_writer(&mut writer, &envelope).map_err(|_| CustomOpenAiError::TooLarge)?;
    check()?;
    if writer.len != total {
        return Err(CustomOpenAiError::InvalidPrompt);
    }
    Ok(writer.bytes.expect("wire writer owns its buffer"))
}

fn envelope<'a>(
    config: &CustomOpenAiConfig,
    session_id: &'a str,
    projection: InputProjection<'a>,
    instructions: &'a str,
    tools: Option<&'a [Value]>,
    require_tool: bool,
) -> Envelope<'a> {
    Envelope {
        model: config.model.model_id(),
        instructions,
        input: SelectedInput {
            projection,
            optional_start: 0,
        },
        reasoning: ReasoningView {
            effort: config.model.reasoning_effort(),
        },
        stream: true,
        store: false,
        prompt_cache_key: session_id,
        parallel_tool_calls: false,
        tools,
        tool_choice: tools.map(|tools| {
            if tools.is_empty() {
                "none"
            } else if require_tool {
                "required"
            } else {
                "auto"
            }
        }),
        include: tools.map(|_| ["reasoning.encrypted_content"]),
    }
}

// Compatibility value-building helpers share the wire shape, but transports
// always use checked_envelope. No optional provenance is inferred from Values.
#[cfg(any(test, feature = "web-search"))]
pub(crate) fn initial_value(
    config: &CustomOpenAiConfig,
    prompt: &CustomOpenAiPrompt,
    instructions: Option<&str>,
) -> Value {
    let instructions = voxgolem_core::response_style::compose_instructions(
        instructions.unwrap_or(DEFAULT_INSTRUCTIONS),
    );
    serde_json::to_value(envelope(
        config,
        &prompt.session_id,
        InputProjection::initial(prompt, &[]),
        &instructions,
        None,
        false,
    ))
    .expect("JSON view is serializable")
}

#[cfg(feature = "web-search")]
pub(crate) fn tool_value(
    config: &CustomOpenAiConfig,
    session_id: &str,
    input: &[Value],
    instructions: &str,
    tools: &[Value],
    require_tool: bool,
) -> Value {
    let instructions = voxgolem_core::response_style::compose_instructions(instructions);
    serde_json::to_value(envelope(
        config,
        session_id,
        InputProjection::mandatory(input),
        &instructions,
        Some(tools),
        require_tool,
    ))
    .expect("JSON view is serializable")
}

/// Production projection and checked bytes, also usable without credentials or HTTP.
pub fn checked_request(
    config: &CustomOpenAiConfig,
    prompt: &CustomOpenAiPrompt,
    instructions: Option<&str>,
) -> Result<Vec<u8>, CustomOpenAiError> {
    checked_envelope(
        config,
        &prompt.session_id,
        InputProjection::initial(prompt, &[]),
        instructions.unwrap_or(DEFAULT_INSTRUCTIONS),
        None,
        false,
    )
}
