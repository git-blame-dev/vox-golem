use futures_util::StreamExt;
use reqwest::header::{HeaderValue, ACCEPT, AUTHORIZATION, CONTENT_TYPE};
use reqwest::{redirect, Client, StatusCode, Url};
use serde::Deserialize;
use serde_json::{json, Value};
use std::fmt::{Display, Formatter};
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const DEFAULT_ENDPOINT: &str = "https://chatgpt.com/backend-api/codex/responses";
const DEFAULT_INSTRUCTIONS: &str = "Answer directly. First line: give the answer naturally using the fewest words possible, never more than 12 words, with no Markdown or label. Then a blank line. Then give the complete answer with any needed detail. Do not repeat the first line verbatim. You have no tools and must not claim to have used tools.";
const MAX_EVENT_BYTES: usize = 1024 * 1024;
const MAX_STREAM_BYTES: usize = 16 * 1024 * 1024;
const MAX_OUTPUT_BYTES: usize = 4 * 1024 * 1024;
const AUTH_EXPIRY_MARGIN_MS: u64 = 60_000;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CustomOpenAiModel {
    SolHigh,
    LunaLow,
}

impl CustomOpenAiModel {
    pub fn model_id(self) -> &'static str {
        match self {
            Self::SolHigh => "gpt-5.6-sol",
            Self::LunaLow => "gpt-5.6-luna",
        }
    }

    pub fn reasoning_effort(self) -> &'static str {
        match self {
            Self::SolHigh => "high",
            Self::LunaLow => "low",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CustomOpenAiContentType {
    OutputText,
    Refusal,
}

impl CustomOpenAiContentType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::OutputText => "output_text",
            Self::Refusal => "refusal",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CustomOpenAiRole {
    User,
    Assistant,
}

impl CustomOpenAiRole {
    fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Assistant => "assistant",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CustomOpenAiMessage {
    pub role: CustomOpenAiRole,
    pub content_type: CustomOpenAiContentType,
    pub text: String,
}

#[derive(Clone, Debug)]
pub struct CustomOpenAiConfig {
    pub endpoint: String,
    pub auth_path: PathBuf,
    pub model: CustomOpenAiModel,
    pub connect_timeout: Duration,
    pub idle_timeout: Duration,
    pub total_timeout: Duration,
}

impl Default for CustomOpenAiConfig {
    fn default() -> Self {
        Self {
            endpoint: DEFAULT_ENDPOINT.to_string(),
            auth_path: default_auth_path(),
            model: CustomOpenAiModel::SolHigh,
            connect_timeout: Duration::from_secs(10),
            idle_timeout: Duration::from_secs(30),
            total_timeout: Duration::from_secs(180),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CustomOpenAiPrompt {
    pub session_id: String,
    pub prompt: String,
    pub history: Vec<CustomOpenAiMessage>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CustomOpenAiUsage {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
    pub cached_input_tokens: Option<u64>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CustomOpenAiTimings {
    pub headers_at: Duration,
    pub first_text_at: Option<Duration>,
    pub completed_at: Duration,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CustomOpenAiResponse {
    pub text: String,
    pub content_type: CustomOpenAiContentType,
    pub model: Option<String>,
    pub service_tier: Option<String>,
    pub usage: Option<CustomOpenAiUsage>,
    pub timings: CustomOpenAiTimings,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CustomOpenAiError {
    InvalidConfig,
    InvalidPrompt,
    AuthUnavailable,
    AuthExpired,
    Unauthorized,
    Forbidden,
    RateLimited,
    Http(u16),
    Transport,
    Timeout,
    UnexpectedContentType,
    MalformedStream,
    MixedContent,
    EmptyResponse,
    Incomplete,
    Failed,
    TooLarge,
}

impl Display for CustomOpenAiError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        let message = match self {
            Self::InvalidConfig => "custom provider configuration is invalid",
            Self::InvalidPrompt => "custom provider prompt is invalid",
            Self::AuthUnavailable => "OpenCode authentication is unavailable",
            Self::AuthExpired => "OpenCode authentication has expired",
            Self::Unauthorized => "OpenCode authentication was rejected",
            Self::Forbidden => "the selected model is unavailable",
            Self::RateLimited => "the custom provider rate limit was reached",
            Self::Http(status) => {
                return write!(formatter, "custom provider returned HTTP {status}")
            }
            Self::Transport => "custom provider transport failed",
            Self::Timeout => "custom provider request timed out",
            Self::UnexpectedContentType => "custom provider returned an unexpected content type",
            Self::MalformedStream => "custom provider returned a malformed stream",
            Self::MixedContent => "custom provider mixed incompatible output types",
            Self::EmptyResponse => "custom provider completed without visible text",
            Self::Incomplete => "custom provider response was incomplete",
            Self::Failed => "custom provider response failed",
            Self::TooLarge => "custom provider response exceeded safety limits",
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for CustomOpenAiError {}

pub struct CustomOpenAiClient {
    config: CustomOpenAiConfig,
    client: Client,
}

impl CustomOpenAiClient {
    pub fn new(config: CustomOpenAiConfig) -> Result<Self, CustomOpenAiError> {
        let endpoint = config.endpoint.trim().to_string();
        if !valid_endpoint(&endpoint)
            || config.connect_timeout.is_zero()
            || config.idle_timeout.is_zero()
            || config.total_timeout.is_zero()
        {
            return Err(CustomOpenAiError::InvalidConfig);
        }
        let client = Client::builder()
            .use_rustls_tls()
            .redirect(redirect::Policy::none())
            .connect_timeout(config.connect_timeout)
            .build()
            .map_err(|_| CustomOpenAiError::Transport)?;
        Ok(Self {
            config: CustomOpenAiConfig { endpoint, ..config },
            client,
        })
    }

    pub async fn respond<F>(
        &self,
        prompt: &CustomOpenAiPrompt,
        mut on_delta: F,
    ) -> Result<CustomOpenAiResponse, CustomOpenAiError>
    where
        F: FnMut(&str),
    {
        if prompt.session_id.trim().is_empty() || prompt.prompt.trim().is_empty() {
            return Err(CustomOpenAiError::InvalidPrompt);
        }
        let started = Instant::now();
        let credential = load_credential(&self.config.auth_path)?;
        let mut authorization = HeaderValue::from_str(&format!("Bearer {}", credential.access))
            .map_err(|_| CustomOpenAiError::AuthUnavailable)?;
        authorization.set_sensitive(true);
        let mut account_id = HeaderValue::from_str(&credential.account_id)
            .map_err(|_| CustomOpenAiError::AuthUnavailable)?;
        account_id.set_sensitive(true);
        let request = build_request(&self.config, prompt);
        let response = self
            .client
            .post(&self.config.endpoint)
            .header(ACCEPT, "text/event-stream")
            .header(AUTHORIZATION, authorization)
            .header("ChatGPT-Account-Id", account_id)
            .header(CONTENT_TYPE, "application/json")
            .header("Originator", "opencode")
            .header("session-id", &prompt.session_id)
            .timeout(self.config.total_timeout)
            .json(&request)
            .send()
            .await
            .map_err(map_transport_error)?;
        let headers_at = started.elapsed();
        if !response.status().is_success() {
            return Err(map_http_status(response.status()));
        }
        if !supported_stream_content_type(response.headers().get(CONTENT_TYPE)) {
            return Err(CustomOpenAiError::UnexpectedContentType);
        }

        let mut stream = response.bytes_stream();
        let mut decoder = SseDecoder::default();
        let mut stream_bytes = 0_usize;
        let mut accumulator = OutputAccumulator::default();
        let mut first_text_at = None;

        let (returned_model, service_tier, usage, completed_at) = 'response: loop {
            if started.elapsed() > self.config.total_timeout {
                return Err(CustomOpenAiError::Timeout);
            }
            let next = tokio::time::timeout(self.config.idle_timeout, stream.next())
                .await
                .map_err(|_| CustomOpenAiError::Timeout)?;
            let Some(chunk) = next else {
                return Err(CustomOpenAiError::Incomplete);
            };
            let chunk = chunk.map_err(map_transport_error)?;
            stream_bytes = stream_bytes
                .checked_add(chunk.len())
                .ok_or(CustomOpenAiError::TooLarge)?;
            if stream_bytes > MAX_STREAM_BYTES {
                return Err(CustomOpenAiError::TooLarge);
            }
            decoder.push(&chunk);
            while let Some(data) = decoder.next_data()? {
                if data.is_empty() || data == "[DONE]" {
                    continue;
                }
                let event: WireEvent =
                    serde_json::from_str(&data).map_err(|_| CustomOpenAiError::MalformedStream)?;
                match event.kind.as_str() {
                    "response.output_text.delta" => {
                        if let Some(delta) =
                            event.delta.as_deref().filter(|delta| !delta.is_empty())
                        {
                            accumulator.push(delta, CustomOpenAiContentType::OutputText)?;
                            first_text_at.get_or_insert_with(|| started.elapsed());
                            on_delta(delta);
                        }
                    }
                    "response.refusal.delta" => {
                        if let Some(delta) =
                            event.delta.as_deref().filter(|delta| !delta.is_empty())
                        {
                            accumulator.push(delta, CustomOpenAiContentType::Refusal)?;
                            first_text_at.get_or_insert_with(|| started.elapsed());
                            on_delta(delta);
                        }
                    }
                    "response.completed" | "response.done" => {
                        let metadata = event.response.ok_or(CustomOpenAiError::MalformedStream)?;
                        if metadata.error.is_some() || metadata.incomplete_details.is_some() {
                            return Err(CustomOpenAiError::Incomplete);
                        }
                        break 'response (
                            metadata.model,
                            metadata.service_tier,
                            metadata.usage.map(Into::into),
                            started.elapsed(),
                        );
                    }
                    "response.failed" | "error" => return Err(CustomOpenAiError::Failed),
                    "response.incomplete" => return Err(CustomOpenAiError::Incomplete),
                    _ => {}
                }
            }
        };

        let (text, content_type) = accumulator.finish()?;
        Ok(CustomOpenAiResponse {
            text,
            content_type,
            model: returned_model,
            service_tier,
            usage,
            timings: CustomOpenAiTimings {
                headers_at,
                first_text_at,
                completed_at,
            },
        })
    }
}

fn default_auth_path() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default()
        .join(".local/share/opencode/auth.json")
}

fn valid_endpoint(endpoint: &str) -> bool {
    let Ok(url) = Url::parse(endpoint.trim()) else {
        return false;
    };
    if !url.username().is_empty() || url.password().is_some() {
        return false;
    }
    match url.scheme() {
        "https" => {
            url.host_str()
                .is_some_and(|host| host.eq_ignore_ascii_case("chatgpt.com"))
                && url.port_or_known_default() == Some(443)
                && url.path() == "/backend-api/codex/responses"
                && url.query().is_none()
                && url.fragment().is_none()
        }
        "http" => url.host_str().is_some_and(|host| {
            host.trim_matches(['[', ']'])
                .parse::<std::net::IpAddr>()
                .is_ok_and(|address| address.is_loopback())
        }),
        _ => false,
    }
}

fn build_request(config: &CustomOpenAiConfig, prompt: &CustomOpenAiPrompt) -> Value {
    let input = prompt
        .history
        .iter()
        .map(|message| {
            let content = match (message.role, message.content_type) {
                (CustomOpenAiRole::User, _) => {
                    json!({ "type": "input_text", "text": message.text })
                }
                (CustomOpenAiRole::Assistant, CustomOpenAiContentType::OutputText) => {
                    json!({ "type": "output_text", "text": message.text })
                }
                (CustomOpenAiRole::Assistant, CustomOpenAiContentType::Refusal) => {
                    json!({ "type": "refusal", "refusal": message.text })
                }
            };
            json!({ "role": message.role.as_str(), "content": [content] })
        })
        .chain(std::iter::once(json!({
            "role": "user",
            "content": [{ "type": "input_text", "text": prompt.prompt }]
        })))
        .collect::<Vec<_>>();
    json!({
        "model": config.model.model_id(),
        "instructions": DEFAULT_INSTRUCTIONS,
        "input": input,
        "reasoning": { "effort": config.model.reasoning_effort() },
        "stream": true,
        "store": false,
        "prompt_cache_key": prompt.session_id,
        "parallel_tool_calls": false
    })
}

#[derive(Deserialize)]
struct AuthFile {
    openai: OAuthCredential,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct OAuthCredential {
    access: String,
    account_id: String,
    expires: u64,
}

fn load_credential(path: &PathBuf) -> Result<OAuthCredential, CustomOpenAiError> {
    let contents = std::fs::read(path).map_err(|_| CustomOpenAiError::AuthUnavailable)?;
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| CustomOpenAiError::AuthUnavailable)?
        .as_millis() as u64;
    parse_credential(&contents, now_ms)
}

fn parse_credential(contents: &[u8], now_ms: u64) -> Result<OAuthCredential, CustomOpenAiError> {
    let auth: AuthFile =
        serde_json::from_slice(contents).map_err(|_| CustomOpenAiError::AuthUnavailable)?;
    if auth.openai.access.is_empty() || auth.openai.account_id.is_empty() {
        return Err(CustomOpenAiError::AuthUnavailable);
    }
    let expires_ms = if auth.openai.expires < 10_000_000_000 {
        auth.openai.expires.saturating_mul(1000)
    } else {
        auth.openai.expires
    };
    if expires_ms <= now_ms.saturating_add(AUTH_EXPIRY_MARGIN_MS) {
        return Err(CustomOpenAiError::AuthExpired);
    }
    Ok(auth.openai)
}

#[derive(Deserialize)]
struct WireEvent {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    delta: Option<String>,
    #[serde(default)]
    response: Option<ResponseMetadata>,
}

#[derive(Deserialize)]
struct ResponseMetadata {
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    service_tier: Option<String>,
    #[serde(default)]
    usage: Option<TokenUsage>,
    #[serde(default)]
    error: Option<Value>,
    #[serde(default)]
    incomplete_details: Option<Value>,
}

#[derive(Clone, Copy, Deserialize)]
struct TokenUsage {
    #[serde(default)]
    input_tokens: Option<u64>,
    #[serde(default)]
    output_tokens: Option<u64>,
    #[serde(default)]
    total_tokens: Option<u64>,
    #[serde(default)]
    input_tokens_details: Option<InputTokenDetails>,
}

#[derive(Clone, Copy, Deserialize)]
struct InputTokenDetails {
    #[serde(default)]
    cached_tokens: Option<u64>,
}

impl From<TokenUsage> for CustomOpenAiUsage {
    fn from(usage: TokenUsage) -> Self {
        Self {
            input_tokens: usage.input_tokens,
            output_tokens: usage.output_tokens,
            total_tokens: usage.total_tokens,
            cached_input_tokens: usage
                .input_tokens_details
                .and_then(|details| details.cached_tokens),
        }
    }
}

#[derive(Default)]
struct OutputAccumulator {
    text: String,
    content_type: Option<CustomOpenAiContentType>,
}

impl OutputAccumulator {
    fn push(
        &mut self,
        delta: &str,
        content_type: CustomOpenAiContentType,
    ) -> Result<(), CustomOpenAiError> {
        if self
            .content_type
            .is_some_and(|existing| existing != content_type)
        {
            return Err(CustomOpenAiError::MixedContent);
        }
        self.content_type.get_or_insert(content_type);
        let next_len = self
            .text
            .len()
            .checked_add(delta.len())
            .ok_or(CustomOpenAiError::TooLarge)?;
        if next_len > MAX_OUTPUT_BYTES {
            return Err(CustomOpenAiError::TooLarge);
        }
        self.text.push_str(delta);
        Ok(())
    }

    fn finish(self) -> Result<(String, CustomOpenAiContentType), CustomOpenAiError> {
        if self.text.trim().is_empty() {
            return Err(CustomOpenAiError::EmptyResponse);
        }
        Ok((
            self.text,
            self.content_type.ok_or(CustomOpenAiError::EmptyResponse)?,
        ))
    }
}

#[derive(Default)]
struct SseDecoder {
    buffer: Vec<u8>,
}

impl SseDecoder {
    fn push(&mut self, chunk: &[u8]) {
        self.buffer.extend_from_slice(chunk);
    }

    fn next_data(&mut self) -> Result<Option<String>, CustomOpenAiError> {
        let Some((position, separator_len)) = find_sse_separator(&self.buffer) else {
            if self.buffer.len() > MAX_EVENT_BYTES {
                return Err(CustomOpenAiError::TooLarge);
            }
            return Ok(None);
        };
        if position > MAX_EVENT_BYTES {
            return Err(CustomOpenAiError::TooLarge);
        }
        let record = self.buffer[..position].to_vec();
        self.buffer.drain(..position + separator_len);
        let record = String::from_utf8(record).map_err(|_| CustomOpenAiError::MalformedStream)?;
        let record = record.strip_prefix('\u{feff}').unwrap_or(&record);
        let normalized = record.replace("\r\n", "\n").replace('\r', "\n");
        let data = normalized
            .lines()
            .filter_map(|line| line.strip_prefix("data:"))
            .map(|line| line.strip_prefix(' ').unwrap_or(line))
            .collect::<Vec<_>>()
            .join("\n");
        Ok(Some(data))
    }
}

fn find_sse_separator(buffer: &[u8]) -> Option<(usize, usize)> {
    buffer
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|position| (position, 4))
        .or_else(|| {
            buffer
                .windows(2)
                .position(|window| window == b"\n\n" || window == b"\r\r")
                .map(|position| (position, 2))
        })
}

fn supported_stream_content_type(value: Option<&HeaderValue>) -> bool {
    value.is_none_or(|value| {
        value.to_str().ok().is_some_and(|content_type| {
            content_type.split(';').next().is_some_and(|media_type| {
                media_type.trim().eq_ignore_ascii_case("text/event-stream")
            })
        })
    })
}

fn map_http_status(status: StatusCode) -> CustomOpenAiError {
    match status {
        StatusCode::UNAUTHORIZED => CustomOpenAiError::Unauthorized,
        StatusCode::FORBIDDEN => CustomOpenAiError::Forbidden,
        StatusCode::TOO_MANY_REQUESTS => CustomOpenAiError::RateLimited,
        _ => CustomOpenAiError::Http(status.as_u16()),
    }
}

fn map_transport_error(error: reqwest::Error) -> CustomOpenAiError {
    if error.is_timeout() {
        CustomOpenAiError::Timeout
    } else {
        CustomOpenAiError::Transport
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    static AUTH_FILE_ID: AtomicU64 = AtomicU64::new(0);

    fn prompt() -> CustomOpenAiPrompt {
        CustomOpenAiPrompt {
            session_id: "session-1".to_string(),
            prompt: "Say hello".to_string(),
            history: vec![CustomOpenAiMessage {
                role: CustomOpenAiRole::Assistant,
                content_type: CustomOpenAiContentType::Refusal,
                text: "No.".to_string(),
            }],
        }
    }

    async fn fixture(
        status: u16,
        content_type: &str,
        chunks: Vec<Vec<u8>>,
        idle: Duration,
    ) -> (
        Vec<u8>,
        String,
        Result<CustomOpenAiResponse, CustomOpenAiError>,
    ) {
        let content_type = content_type.to_string();
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.expect("listener");
        let address = listener.local_addr().expect("address");
        let request = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("connection");
            let mut bytes = Vec::new();
            let mut buffer = [0; 4096];
            loop {
                let count = socket.read(&mut buffer).await.expect("request");
                bytes.extend_from_slice(&buffer[..count]);
                if bytes.windows(4).any(|window| window == b"\r\n\r\n") {
                    let headers_end = bytes
                        .windows(4)
                        .position(|window| window == b"\r\n\r\n")
                        .unwrap()
                        + 4;
                    let headers = String::from_utf8_lossy(&bytes[..headers_end]);
                    let length = headers
                        .lines()
                        .find_map(|line| {
                            line.strip_prefix("Content-Length:")?
                                .trim()
                                .parse::<usize>()
                                .ok()
                        })
                        .unwrap_or(0);
                    if bytes.len() >= headers_end + length {
                        break;
                    }
                }
            }
            let content_type = if content_type.is_empty() {
                String::new()
            } else {
                format!("Content-Type: {content_type}\r\n")
            };
            let response =
                format!("HTTP/1.1 {status} Test\r\n{content_type}Connection: close\r\n\r\n");
            socket
                .write_all(response.as_bytes())
                .await
                .expect("response headers");
            for chunk in chunks {
                if socket.write_all(&chunk).await.is_err() {
                    break;
                }
                if socket.flush().await.is_err() {
                    break;
                }
                tokio::task::yield_now().await;
            }
            bytes
        });
        let id = AUTH_FILE_ID.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("vox-golem-auth-{}-{id}.json", std::process::id()));
        let expiry = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
            + 3600;
        std::fs::write(&path, format!(r#"{{"openai":{{"access":"test-access","accountId":"test-account","expires":{expiry}}}}}"#)).unwrap();
        let config = CustomOpenAiConfig {
            endpoint: format!("http://{address}"),
            auth_path: path.clone(),
            idle_timeout: idle,
            total_timeout: Duration::from_secs(5),
            ..Default::default()
        };
        let client = CustomOpenAiClient::new(config).expect("client");
        let deltas = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
        let seen = deltas.clone();
        let result = client
            .respond(&prompt(), |delta| seen.lock().unwrap().push_str(delta))
            .await;
        std::fs::remove_file(path).ok();
        let request = request.await.expect("server");
        let captured_deltas = deltas.lock().unwrap().clone();
        (request, captured_deltas, result)
    }

    fn sse(events: &[&str]) -> Vec<Vec<u8>> {
        events
            .iter()
            .flat_map(|event| {
                let bytes = format!("data: {event}\n\n").into_bytes();
                bytes.chunks(3).map(Vec::from).collect::<Vec<_>>()
            })
            .collect()
    }

    #[tokio::test]
    async fn respond_streams_text_and_sends_sensitive_compatible_request() {
        let events = sse(&[
            r#"{"type":"response.output_text.delta","delta":"hel"}"#,
            r#"{"type":"response.output_text.delta","delta":"lo"}"#,
            r#"{"type":"response.completed","response":{"model":"gpt-5.6-sol","service_tier":"default","usage":{"input_tokens":10,"output_tokens":2,"total_tokens":12,"input_tokens_details":{"cached_tokens":3}}}}"#,
        ]);
        let (request, deltas, result) = fixture(
            200,
            "text/event-stream; charset=utf-8",
            events,
            Duration::from_secs(1),
        )
        .await;
        let request_text = String::from_utf8_lossy(&request);
        let request_text = request_text.to_ascii_lowercase();
        assert!(request_text.contains("authorization: bearer test-access"));
        assert!(request_text.contains("chatgpt-account-id: test-account"));
        assert!(request_text.contains("originator: opencode"));
        let body = request_text.split("\r\n\r\n").nth(1).unwrap();
        let body: Value = serde_json::from_str(body).unwrap();
        assert_eq!(body["model"], "gpt-5.6-sol");
        assert_eq!(body["reasoning"]["effort"], "high");
        assert_eq!(body["store"], false);
        assert!(body.get("tools").is_none());
        let response = result.expect("response");
        assert_eq!(response.text, "hello");
        assert_eq!(deltas, "hello");
        assert_eq!(response.usage.unwrap().cached_input_tokens, Some(3));
    }

    #[tokio::test]
    async fn respond_covers_refusal_and_terminal_errors() {
        let refusal = sse(&[
            r#"{"type":"response.refusal.delta","delta":"no"}"#,
            r#"{"type":"response.done","response":{"model":"m"}}"#,
        ]);
        let (_, deltas, result) =
            fixture(200, "text/event-stream", refusal, Duration::from_secs(1)).await;
        assert_eq!(
            result.unwrap().content_type,
            CustomOpenAiContentType::Refusal
        );
        assert_eq!(deltas, "no");
        for (event, expected) in [
            ("response.failed", CustomOpenAiError::Failed),
            ("response.incomplete", CustomOpenAiError::Incomplete),
        ] {
            let (_, _, result) = fixture(
                200,
                "text/event-stream",
                sse(&[&format!(r#"{{"type":"{event}"}}"#)]),
                Duration::from_secs(1),
            )
            .await;
            assert_eq!(result, Err(expected));
        }
    }

    #[tokio::test]
    async fn respond_maps_http_content_and_stream_failures() {
        for (status, expected) in [
            (401, CustomOpenAiError::Unauthorized),
            (403, CustomOpenAiError::Forbidden),
            (429, CustomOpenAiError::RateLimited),
        ] {
            let (_, _, result) =
                fixture(status, "text/event-stream", vec![], Duration::from_secs(1)).await;
            assert_eq!(result, Err(expected));
        }
        let (_, _, result) = fixture(200, "application/json", vec![], Duration::from_secs(1)).await;
        assert_eq!(result, Err(CustomOpenAiError::UnexpectedContentType));
        let (_, _, result) = fixture(
            200,
            "text/event-stream",
            sse(&[r#"{"type":"bad""#]),
            Duration::from_secs(1),
        )
        .await;
        assert_eq!(result, Err(CustomOpenAiError::MalformedStream));
        let (_, _, result) = fixture(
            200,
            "text/event-stream",
            vec![b"data: {\"type\":\"response.output_text.delta\",\"delta\":\"x\"}\n\n".to_vec()],
            Duration::from_millis(20),
        )
        .await;
        assert_eq!(result, Err(CustomOpenAiError::Incomplete));
    }

    #[tokio::test]
    async fn respond_accepts_valid_sse_without_content_type() {
        let (_, _, result) = fixture(
            200,
            "",
            sse(&[
                r#"{"type":"response.output_text.delta","delta":"hello"}"#,
                r#"{"type":"response.completed","response":{}}"#,
            ]),
            Duration::from_secs(1),
        )
        .await;
        assert_eq!(result.unwrap().text, "hello");
    }

    #[tokio::test]
    async fn respond_rejects_non_sse_body_without_content_type() {
        let (_, _, result) = fixture(
            200,
            "",
            vec![br#"{"error":"not an event stream"}"#.to_vec()],
            Duration::from_secs(1),
        )
        .await;
        assert_eq!(result, Err(CustomOpenAiError::Incomplete));
    }

    #[test]
    fn maps_sol_and_luna_without_substitution() {
        assert_eq!(CustomOpenAiModel::SolHigh.model_id(), "gpt-5.6-sol");
        assert_eq!(CustomOpenAiModel::SolHigh.reasoning_effort(), "high");
        assert_eq!(CustomOpenAiModel::LunaLow.model_id(), "gpt-5.6-luna");
        assert_eq!(CustomOpenAiModel::LunaLow.reasoning_effort(), "low");
    }

    #[test]
    fn request_contains_history_reasoning_and_no_tools() {
        let config = CustomOpenAiConfig {
            model: CustomOpenAiModel::LunaLow,
            ..CustomOpenAiConfig::default()
        };
        let request = build_request(&config, &prompt());

        assert_eq!(request["model"], "gpt-5.6-luna");
        assert_eq!(request["reasoning"]["effort"], "low");
        assert_eq!(request["stream"], true);
        assert_eq!(request["store"], false);
        assert!(request.get("tools").is_none());
        assert_eq!(request["input"][0]["content"][0]["type"], "refusal");
        assert_eq!(request["input"][1]["content"][0]["text"], "Say hello");
    }

    #[test]
    fn historical_user_messages_are_input_text() {
        let config = CustomOpenAiConfig::default();
        let mut prompt = prompt();
        prompt.history = vec![CustomOpenAiMessage {
            role: CustomOpenAiRole::User,
            content_type: CustomOpenAiContentType::Refusal,
            text: "Earlier question".to_string(),
        }];
        let request = build_request(&config, &prompt);
        assert_eq!(request["input"][0]["content"][0]["type"], "input_text");
        assert_eq!(
            request["input"][0]["content"][0]["text"],
            "Earlier question"
        );
        assert!(request["input"][0]["content"][0].get("refusal").is_none());
    }

    #[test]
    fn endpoint_validation_restricts_oauth_to_chatgpt_or_loopback() {
        for endpoint in [
            "https://chatgpt.com/backend-api/codex/responses",
            "http://127.0.0.1:8080/responses",
            "http://[::1]:8080/responses",
        ] {
            assert!(
                valid_endpoint(endpoint),
                "expected valid endpoint: {endpoint}"
            );
        }
        for endpoint in [
            "",
            "not a url",
            "file:///tmp/credentials",
            "ftp://example.test/responses",
            "http://localhost:8080/responses",
            "http://example.test/responses",
            "https://api.example.test/responses",
            "https://chatgpt.com/other",
            "https://chatgpt.com/backend-api/codex/responses?redirect=1",
            "https://example.test:bad/responses",
            "http://user:password@127.0.0.1:8080/responses",
            "https://user@example.test/responses",
        ] {
            assert!(
                !valid_endpoint(endpoint),
                "expected invalid endpoint: {endpoint}"
            );
        }
    }

    #[test]
    fn client_stores_normalized_endpoint() {
        let mut config = CustomOpenAiConfig::default();
        config.endpoint = format!("  {}  ", config.endpoint);

        let client = CustomOpenAiClient::new(config).expect("trimmed endpoint should be valid");

        assert_eq!(client.config.endpoint, DEFAULT_ENDPOINT);
    }

    #[test]
    fn stream_content_type_may_be_absent_but_present_type_must_be_event_stream() {
        assert!(supported_stream_content_type(None));
        assert!(supported_stream_content_type(Some(
            &HeaderValue::from_static("text/event-stream; charset=utf-8")
        )));
        assert!(!supported_stream_content_type(Some(
            &HeaderValue::from_static("application/json")
        )));
        assert!(!supported_stream_content_type(Some(
            &HeaderValue::from_static("text/event-streamx")
        )));
    }

    #[test]
    fn parses_camel_case_auth_and_enforces_expiry_margin() {
        let valid =
            br#"{"openai":{"access":"secret","accountId":"account-id","expires":1000000000000}}"#;
        let credential = parse_credential(valid, 1).expect("credential should be valid");
        assert_eq!(credential.account_id, "account-id");
        assert!(matches!(
            parse_credential(valid, 999_999_940_000),
            Err(CustomOpenAiError::AuthExpired)
        ));
    }

    #[test]
    fn error_messages_never_include_credentials() {
        for error in [
            CustomOpenAiError::AuthUnavailable,
            CustomOpenAiError::AuthExpired,
            CustomOpenAiError::Unauthorized,
            CustomOpenAiError::Forbidden,
            CustomOpenAiError::Transport,
        ] {
            let message = error.to_string();
            assert!(!message.contains("secret"));
            assert!(!message.contains("account-id"));
        }
    }

    #[test]
    fn decoder_frames_utf8_crlf_and_multiple_data_lines() {
        let mut decoder = SseDecoder::default();
        decoder.push(b"data: {\"delta\":\"h");
        assert_eq!(decoder.next_data(), Ok(None));
        decoder.push(b"e\"}\r\ndata: second\r\n\r\n");
        assert_eq!(
            decoder.next_data(),
            Ok(Some("{\"delta\":\"he\"}\nsecond".to_string()))
        );
    }

    #[test]
    fn accumulator_rejects_mixed_text_and_refusal() {
        let mut output = OutputAccumulator::default();
        output
            .push("answer", CustomOpenAiContentType::OutputText)
            .expect("first output should be accepted");
        assert_eq!(
            output.push("refusal", CustomOpenAiContentType::Refusal),
            Err(CustomOpenAiError::MixedContent)
        );
    }

    #[test]
    fn completion_metadata_maps_usage_and_cached_tokens() {
        let event: WireEvent = serde_json::from_str(
            r#"{"type":"response.completed","response":{"model":"gpt-5.6-luna","service_tier":"default","usage":{"input_tokens":10,"output_tokens":4,"total_tokens":14,"input_tokens_details":{"cached_tokens":3}}}}"#,
        )
        .expect("completion event");
        let metadata = event.response.expect("metadata");
        let usage: CustomOpenAiUsage = metadata.usage.expect("usage").into();
        assert_eq!(metadata.model.as_deref(), Some("gpt-5.6-luna"));
        assert_eq!(usage.input_tokens, Some(10));
        assert_eq!(usage.output_tokens, Some(4));
        assert_eq!(usage.total_tokens, Some(14));
        assert_eq!(usage.cached_input_tokens, Some(3));
    }

    #[test]
    fn decoder_rejects_oversized_unterminated_record() {
        let mut decoder = SseDecoder::default();
        decoder.push(&vec![b'x'; MAX_EVENT_BYTES + 1]);
        assert_eq!(decoder.next_data(), Err(CustomOpenAiError::TooLarge));
    }

    #[test]
    fn maps_http_status_without_response_body() {
        assert_eq!(
            map_http_status(StatusCode::UNAUTHORIZED),
            CustomOpenAiError::Unauthorized
        );
        assert_eq!(
            map_http_status(StatusCode::FORBIDDEN),
            CustomOpenAiError::Forbidden
        );
        assert_eq!(
            map_http_status(StatusCode::TOO_MANY_REQUESTS),
            CustomOpenAiError::RateLimited
        );
        assert_eq!(
            map_http_status(StatusCode::BAD_GATEWAY),
            CustomOpenAiError::Http(502)
        );
    }
}
