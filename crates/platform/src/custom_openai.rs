use futures_util::StreamExt;
use reqwest::header::{HeaderValue, ACCEPT, AUTHORIZATION, CONTENT_TYPE};
use reqwest::{redirect, Client, StatusCode, Url};
use serde::{de, Deserialize, Deserializer};
use serde_json::Value;
use std::fmt::{Display, Formatter};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub(crate) mod envelope;
#[cfg(feature = "web-search")]
pub mod tools;
pub use envelope::{checked_request, MAX_REQUEST_BYTES};
#[cfg(test)]
mod envelope_tests;
#[cfg(test)]
mod loopback_tests;

const DEFAULT_ENDPOINT: &str = "https://chatgpt.com/backend-api/codex/responses";
const DEFAULT_INSTRUCTIONS: &str = "You have no tools and must not claim to have used tools.";
const MAX_EVENT_BYTES: usize = 1024 * 1024;
const MAX_STREAM_BYTES: usize = 16 * 1024 * 1024;
const MAX_OUTPUT_BYTES: usize = 4 * 1024 * 1024;
const AUTH_EXPIRY_MARGIN_MS: u64 = 60_000;
const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
pub const MAX_CUSTOM_OPENAI_AUTH_BYTES: u64 = 65_536;
const MAX_SAFE_JSON_INTEGER: u64 = 9_007_199_254_740_991;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CustomOpenAiModel {
    SolHigh,
    SolNone,
    LunaNone,
    LunaLow,
    LocalFast,
}

impl CustomOpenAiModel {
    pub fn model_id(self) -> &'static str {
        match self {
            Self::SolHigh | Self::SolNone => "gpt-6.1-sol",
            Self::LunaNone => "gpt-6-luna",
            Self::LunaLow => "gpt-5.6-luna",
            Self::LocalFast => "local-fast",
        }
    }

    pub fn reasoning_effort(self) -> &'static str {
        match self {
            Self::SolHigh => "high",
            Self::SolNone | Self::LunaNone | Self::LocalFast => "none",
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
            connect_timeout: DEFAULT_CONNECT_TIMEOUT,
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

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CustomOpenAiAuthRevision {
    len: u64,
    modified_ns: u128,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CustomOpenAiAuthObservation {
    pub revision: Option<CustomOpenAiAuthRevision>,
    pub status: Result<(), CustomOpenAiError>,
}

/// Inspect access using the same bounded read, parser and expiry policy as a request.
/// No HTTP client is constructed and no credentials or observations are retained.
pub fn observe_custom_openai_auth(path: &Path) -> CustomOpenAiAuthObservation {
    match read_credential_file(path) {
        Ok((contents, revision)) => CustomOpenAiAuthObservation {
            revision: Some(revision),
            status: parse_request_credential(&contents, revision).map(|_| ()),
        },
        Err(error) => CustomOpenAiAuthObservation {
            revision: None,
            status: Err(error),
        },
    }
}

/// A fresh path revision for fencing observations against external replacement.
pub fn custom_openai_auth_revision(path: &Path) -> Option<CustomOpenAiAuthRevision> {
    path_auth_revision(path)
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
            Self::TooLarge => "custom provider request or response exceeded safety limits",
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
        let config = Self::validated_config(config)?;
        let client = Self::transport(&config)?;
        Ok(Self { config, client })
    }

    /// Reuse this owner's pool with fresh request configuration and credentials.
    /// The owner must keep the pool on the same continuously driven Tokio runtime.
    pub fn with_reused_transport(
        &self,
        config: CustomOpenAiConfig,
    ) -> Result<Self, CustomOpenAiError> {
        let config = Self::validated_config(config)?;
        if config.endpoint != self.config.endpoint
            || config.connect_timeout != self.config.connect_timeout
        {
            return Err(CustomOpenAiError::InvalidConfig);
        }
        Ok(Self {
            config,
            client: self.client.clone(),
        })
    }

    pub fn config(&self) -> &CustomOpenAiConfig {
        &self.config
    }

    fn validated_config(
        config: CustomOpenAiConfig,
    ) -> Result<CustomOpenAiConfig, CustomOpenAiError> {
        let endpoint = config.endpoint.trim().to_string();
        if !valid_endpoint(&endpoint)
            || config.connect_timeout.is_zero()
            || config.idle_timeout.is_zero()
            || config.total_timeout.is_zero()
        {
            return Err(CustomOpenAiError::InvalidConfig);
        }
        Ok(CustomOpenAiConfig { endpoint, ..config })
    }

    fn transport(config: &CustomOpenAiConfig) -> Result<Client, CustomOpenAiError> {
        let endpoint =
            Url::parse(&config.endpoint).map_err(|_| CustomOpenAiError::InvalidConfig)?;
        #[allow(unused_mut)]
        let mut client_builder = if endpoint.scheme() == "http" {
            // validated_config admits HTTP only for numeric loopback endpoints.
            crate::owned_loopback::client_builder()
        } else {
            Client::builder()
        };
        #[cfg(all(feature = "custom-openai-webpki", not(feature = "custom-openai")))]
        {
            let root_store = rustls::RootCertStore {
                roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
            };
            let tls_config = rustls::ClientConfig::builder_with_provider(std::sync::Arc::new(
                rustls::crypto::ring::default_provider(),
            ))
            .with_safe_default_protocol_versions()
            .map_err(|_| CustomOpenAiError::Transport)?
            .with_root_certificates(root_store)
            .with_no_client_auth();
            client_builder = client_builder.use_preconfigured_tls(tls_config);
        }
        #[cfg(not(all(feature = "custom-openai-webpki", not(feature = "custom-openai"))))]
        {
            client_builder = client_builder.use_rustls_tls();
        }
        client_builder
            .redirect(redirect::Policy::none())
            .connect_timeout(config.connect_timeout)
            .tcp_keepalive(None)
            .build()
            .map_err(|_| CustomOpenAiError::Transport)
    }

    pub async fn respond<F>(
        &self,
        prompt: &CustomOpenAiPrompt,
        on_delta: F,
    ) -> Result<CustomOpenAiResponse, CustomOpenAiError>
    where
        F: FnMut(&str),
    {
        self.respond_with_instructions(prompt, None, on_delta).await
    }

    pub fn check_auth(&self) -> Result<(), CustomOpenAiError> {
        if self.config.model == CustomOpenAiModel::LocalFast {
            return Ok(());
        }
        self.observe_auth().status
    }

    pub fn auth_revision(&self) -> Option<CustomOpenAiAuthRevision> {
        if self.config.model == CustomOpenAiModel::LocalFast {
            return Some(CustomOpenAiAuthRevision::default());
        }
        custom_openai_auth_revision(&self.config.auth_path)
    }

    pub fn observe_auth(&self) -> CustomOpenAiAuthObservation {
        if self.config.model == CustomOpenAiModel::LocalFast {
            return CustomOpenAiAuthObservation {
                revision: Some(CustomOpenAiAuthRevision::default()),
                status: Ok(()),
            };
        }
        observe_custom_openai_auth(&self.config.auth_path)
    }

    pub async fn respond_with_instructions<F>(
        &self,
        prompt: &CustomOpenAiPrompt,
        instructions: Option<&str>,
        on_delta: F,
    ) -> Result<CustomOpenAiResponse, CustomOpenAiError>
    where
        F: FnMut(&str),
    {
        self.respond_with_auth_revision(prompt, instructions, on_delta, |_| {})
            .await
    }

    pub async fn respond_with_auth_revision<F, R>(
        &self,
        prompt: &CustomOpenAiPrompt,
        instructions: Option<&str>,
        on_delta: F,
        on_auth_revision: R,
    ) -> Result<CustomOpenAiResponse, CustomOpenAiError>
    where
        F: FnMut(&str),
        R: FnOnce(CustomOpenAiAuthRevision),
    {
        self.respond_with_auth_revision_and_dispatch(
            prompt,
            instructions,
            on_delta,
            on_auth_revision,
            |_| {},
        )
        .await
    }

    /// Observe dispatch after body preparation, without changing legacy timing origins.
    pub async fn respond_with_auth_revision_and_dispatch<F, R, D>(
        &self,
        prompt: &CustomOpenAiPrompt,
        instructions: Option<&str>,
        mut on_delta: F,
        on_auth_revision: R,
        on_dispatch: D,
    ) -> Result<CustomOpenAiResponse, CustomOpenAiError>
    where
        F: FnMut(&str),
        R: FnOnce(CustomOpenAiAuthRevision),
        D: FnOnce(Instant),
    {
        if prompt.session_id.len() > MAX_REQUEST_BYTES || prompt.prompt.len() > MAX_REQUEST_BYTES {
            return Err(CustomOpenAiError::TooLarge);
        }
        if prompt.session_id.trim().is_empty() || prompt.prompt.trim().is_empty() {
            return Err(CustomOpenAiError::InvalidPrompt);
        }
        let started = Instant::now();
        let deadline = tokio::time::Instant::now() + self.config.total_timeout;
        let request = checked_request(&self.config, prompt, instructions)?;
        if tokio::time::Instant::now() >= deadline {
            return Err(CustomOpenAiError::Timeout);
        }
        let credential = if self.config.model == CustomOpenAiModel::LocalFast {
            load_request_credential(&self.config.auth_path).unwrap_or_else(|_| RequestCredential {
                authorization: HeaderValue::from_static("Bearer local"),
                account_id: None,
                revision: CustomOpenAiAuthRevision::default(),
            })
        } else {
            load_request_credential(&self.config.auth_path)?
        };
        on_auth_revision(credential.revision);
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return Err(CustomOpenAiError::Timeout);
        }
        let mut request_builder = self
            .client
            .post(&self.config.endpoint)
            .header(ACCEPT, "text/event-stream")
            .header(AUTHORIZATION, credential.authorization)
            .header(CONTENT_TYPE, "application/json")
            .header("Originator", "opencode")
            .header("session-id", &prompt.session_id)
            .timeout(remaining);
        if let Some(account_id) = credential.account_id {
            request_builder = request_builder.header("ChatGPT-Account-Id", account_id);
        }
        let request_builder = request_builder.body(request);
        on_dispatch(Instant::now());
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return Err(CustomOpenAiError::Timeout);
        }
        let response = request_builder
            .timeout(remaining)
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
                            record_first_visible_text(&mut first_text_at, started, delta);
                            on_delta(delta);
                        }
                    }
                    "response.refusal.delta" => {
                        if let Some(delta) =
                            event.delta.as_deref().filter(|delta| !delta.is_empty())
                        {
                            accumulator.push(delta, CustomOpenAiContentType::Refusal)?;
                            record_first_visible_text(&mut first_text_at, started, delta);
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

fn record_first_visible_text(first_text_at: &mut Option<Duration>, started: Instant, delta: &str) {
    if first_text_at.is_none() && delta.chars().any(|character| !character.is_whitespace()) {
        *first_text_at = Some(started.elapsed());
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

#[cfg(any(test, feature = "web-search"))]
fn build_request(
    config: &CustomOpenAiConfig,
    prompt: &CustomOpenAiPrompt,
    instructions: Option<&str>,
) -> Value {
    envelope::initial_value(config, prompt, instructions)
}

#[derive(Deserialize)]
struct AuthFile {
    openai: OAuthCredential,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct OAuthCredential {
    access: String,
    account_id: Option<String>,
    #[serde(deserialize_with = "deserialize_expiry")]
    expires: u64,
}

fn deserialize_expiry<'de, D>(deserializer: D) -> Result<u64, D::Error>
where
    D: Deserializer<'de>,
{
    struct ExpiryVisitor;
    impl de::Visitor<'_> for ExpiryVisitor {
        type Value = u64;

        fn expecting(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("a non-negative safe integer")
        }
        fn visit_u64<E: de::Error>(self, value: u64) -> Result<u64, E> {
            if value <= MAX_SAFE_JSON_INTEGER {
                Ok(value)
            } else {
                Err(E::custom("expiry exceeds the safe integer range"))
            }
        }
        fn visit_i64<E: de::Error>(self, value: i64) -> Result<u64, E> {
            let value = u64::try_from(value).map_err(|_| E::custom("expiry is negative"))?;
            self.visit_u64(value)
        }
        fn visit_f64<E: de::Error>(self, value: f64) -> Result<u64, E> {
            if value.is_finite()
                && value >= 0.0
                && value.fract() == 0.0
                && value <= MAX_SAFE_JSON_INTEGER as f64
            {
                Ok(value as u64)
            } else {
                Err(E::custom("expiry is not a safe integer"))
            }
        }
    }
    deserializer.deserialize_any(ExpiryVisitor)
}

struct RequestCredential {
    authorization: HeaderValue,
    account_id: Option<HeaderValue>,
    revision: CustomOpenAiAuthRevision,
}

fn metadata_auth_revision(metadata: &std::fs::Metadata) -> Option<CustomOpenAiAuthRevision> {
    if !metadata.is_file() {
        return None;
    }
    let modified_ns = metadata
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()?
        .as_nanos();
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Some(CustomOpenAiAuthRevision {
            len: metadata.len(),
            modified_ns,
            device: metadata.dev(),
            inode: metadata.ino(),
        })
    }
    #[cfg(not(unix))]
    {
        Some(CustomOpenAiAuthRevision {
            len: metadata.len(),
            modified_ns,
        })
    }
}

fn path_auth_revision(path: &Path) -> Option<CustomOpenAiAuthRevision> {
    let metadata = std::fs::symlink_metadata(path).ok()?;
    if metadata.file_type().is_symlink() {
        return None;
    }
    metadata_auth_revision(&metadata)
}

fn read_credential_file(
    path: &Path,
) -> Result<(Vec<u8>, CustomOpenAiAuthRevision), CustomOpenAiError> {
    let file = std::fs::File::open(path).map_err(|_| CustomOpenAiError::AuthUnavailable)?;
    let metadata = file
        .metadata()
        .map_err(|_| CustomOpenAiError::AuthUnavailable)?;
    let revision = metadata_auth_revision(&metadata).ok_or(CustomOpenAiError::AuthUnavailable)?;
    if revision.len > MAX_CUSTOM_OPENAI_AUTH_BYTES {
        return Err(CustomOpenAiError::AuthUnavailable);
    }
    let mut contents = Vec::new();
    file.take(MAX_CUSTOM_OPENAI_AUTH_BYTES + 1)
        .read_to_end(&mut contents)
        .map_err(|_| CustomOpenAiError::AuthUnavailable)?;
    if contents.len() as u64 > MAX_CUSTOM_OPENAI_AUTH_BYTES {
        return Err(CustomOpenAiError::AuthUnavailable);
    }
    Ok((contents, revision))
}

fn parse_request_credential(
    contents: &[u8],
    revision: CustomOpenAiAuthRevision,
) -> Result<RequestCredential, CustomOpenAiError> {
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| CustomOpenAiError::AuthUnavailable)?
        .as_millis() as u64;
    let credential = parse_credential(contents, now_ms)?;
    let mut authorization = HeaderValue::from_str(&format!("Bearer {}", credential.access))
        .map_err(|_| CustomOpenAiError::AuthUnavailable)?;
    authorization.set_sensitive(true);
    let account_id = credential
        .account_id
        .map(|value| {
            let mut header =
                HeaderValue::from_str(&value).map_err(|_| CustomOpenAiError::AuthUnavailable)?;
            header.set_sensitive(true);
            Ok(header)
        })
        .transpose()?;
    Ok(RequestCredential {
        authorization,
        account_id,
        revision,
    })
}

fn load_request_credential(path: &Path) -> Result<RequestCredential, CustomOpenAiError> {
    let (contents, revision) = read_credential_file(path)?;
    parse_request_credential(&contents, revision)
}

fn parse_credential(contents: &[u8], now_ms: u64) -> Result<OAuthCredential, CustomOpenAiError> {
    let auth: AuthFile =
        serde_json::from_slice(contents).map_err(|_| CustomOpenAiError::AuthUnavailable)?;
    if auth.openai.access.is_empty()
        || auth
            .openai
            .account_id
            .as_ref()
            .is_some_and(String::is_empty)
    {
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
pub(crate) struct SseDecoder {
    buffer: Vec<u8>,
}

impl SseDecoder {
    pub(crate) fn push(&mut self, chunk: &[u8]) {
        self.buffer.extend_from_slice(chunk);
    }

    pub(crate) fn next_data(&mut self) -> Result<Option<String>, CustomOpenAiError> {
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
        fixture_with_account(status, content_type, chunks, idle, Some("test-account")).await
    }

    async fn fixture_with_account(
        status: u16,
        content_type: &str,
        chunks: Vec<Vec<u8>>,
        idle: Duration,
        account_id: Option<&str>,
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
        let account_id = account_id
            .map(|value| format!(r#","accountId":"{value}""#))
            .unwrap_or_default();
        std::fs::write(
            &path,
            format!(r#"{{"openai":{{"access":"test-access"{account_id},"expires":{expiry}}}}}"#),
        )
        .unwrap();
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

    #[test]
    fn first_visible_text_clock_ignores_leading_whitespace_deltas() {
        let started = Instant::now();
        let mut first = None;
        super::record_first_visible_text(&mut first, started, " \n");
        assert_eq!(first, None);
        std::thread::sleep(Duration::from_millis(8));
        super::record_first_visible_text(&mut first, started, "Ready");
        let visible_at = first.expect("visible text should start the clock");
        assert!(visible_at >= Duration::from_millis(8));
        super::record_first_visible_text(&mut first, started, " again");
        assert_eq!(first, Some(visible_at));
    }

    async fn pool_server(
        listener: TcpListener,
        accepted: std::sync::Arc<AtomicU64>,
        observed: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    ) {
        let mut workers = tokio::task::JoinSet::new();
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            accepted.fetch_add(1, Ordering::Relaxed);
            let observed = observed.clone();
            workers.spawn(async move {
                loop {
                    let mut request = Vec::new();
                    let mut buffer = [0_u8; 4096];
                    loop {
                        let count = socket.read(&mut buffer).await.unwrap();
                        if count == 0 {
                            return;
                        }
                        request.extend_from_slice(&buffer[..count]);
                        if let Some(end) = request.windows(4).position(|part| part == b"\r\n\r\n") {
                            let headers = String::from_utf8_lossy(&request[..end]).to_ascii_lowercase();
                            let length = headers.lines().find_map(|line| {
                                line.strip_prefix("content-length:")?.trim().parse::<usize>().ok()
                            }).unwrap();
                            if request.len() >= end + 4 + length {
                                observed.lock().unwrap().push(headers.lines().find(|line| {
                                    line.starts_with("authorization:")
                                }).unwrap().to_string());
                                break;
                            }
                        }
                    }
                    let body = concat!(
                        "data: {\"type\":\"response.output_text.delta\",\"delta\":\"Synthetic answer\"}\n\n",
                        "data: {\"type\":\"response.completed\",\"response\":{}}\n\n"
                    );
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\n\r\n{}",
                        body.len(), body
                    );
                    if socket.write_all(response.as_bytes()).await.is_err() {
                        return;
                    }
                }
            });
        }
    }

    #[test]
    fn independent_clients_do_not_share_idle_connections_across_suspended_runtimes() {
        let connections = std::sync::Arc::new(AtomicU64::new(0));
        let accepted = connections.clone();
        let (address_tx, address_rx) = std::sync::mpsc::sync_channel(1);
        let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
        // The server keeps running independently while client runtime A is suspended.
        let server = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            runtime.block_on(async {
                let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
                address_tx.send(listener.local_addr().unwrap()).unwrap();
                let task = tokio::spawn(pool_server(listener, accepted, Default::default()));
                let _ = stop_rx.await;
                task.abort();
                let _ = task.await;
            });
        });
        let endpoint = format!("http://{}", address_rx.recv().unwrap());
        let id = AUTH_FILE_ID.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "vox-synthetic-runtime-auth-{}-{id}.json",
            std::process::id()
        ));
        std::fs::write(
            &path,
            r#"{"openai":{"access":"synthetic-runtime","expires":4102444800}}"#,
        )
        .unwrap();
        let config = CustomOpenAiConfig {
            endpoint,
            auth_path: path.clone(),
            total_timeout: Duration::from_secs(1),
            ..Default::default()
        };
        let runtime_a = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let client_a = CustomOpenAiClient::new(config.clone()).unwrap();
        let first = runtime_a.block_on(async {
            let response = client_a.respond(&prompt(), |_| {}).await;
            tokio::task::yield_now().await;
            response
        });
        assert!(
            first.is_ok(),
            "runtime A must establish an idle connection: {first:?}"
        );
        let runtime_b = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let client_b = CustomOpenAiClient::new(config).unwrap();
        let second = runtime_b.block_on(client_b.respond(&prompt(), |_| {}));
        stop_tx.send(()).unwrap();
        server.join().unwrap();
        std::fs::remove_file(path).unwrap();
        assert!(
            second.is_ok(),
            "runtime B must not depend on suspended runtime A: {second:?}"
        );
        assert_eq!(
            connections.load(Ordering::Relaxed),
            2,
            "independent clients must own separate pools"
        );
        drop(client_a);
        drop(runtime_a);
    }

    #[tokio::test]
    async fn explicitly_owned_clients_reuse_connection_without_reusing_request_credentials() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let connections = std::sync::Arc::new(AtomicU64::new(0));
        let authorizations = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let server = tokio::spawn(pool_server(
            listener,
            connections.clone(),
            authorizations.clone(),
        ));
        let id = AUTH_FILE_ID.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "vox-synthetic-pool-auth-{}-{id}.json",
            std::process::id()
        ));
        let config = CustomOpenAiConfig {
            endpoint,
            auth_path: path.clone(),
            total_timeout: Duration::from_secs(3),
            ..Default::default()
        };
        let owner = CustomOpenAiClient::new(config.clone()).unwrap();
        let mut revisions = Vec::new();
        for access in ["synthetic-first", "synthetic-second"] {
            std::fs::write(
                &path,
                format!(r#"{{"openai":{{"access":"{access}","expires":4102444800}}}}"#),
            )
            .unwrap();
            let client = owner.with_reused_transport(config.clone()).unwrap();
            assert_eq!(
                client
                    .respond_with_auth_revision(
                        &prompt(),
                        None,
                        |_| {},
                        |revision| revisions.push(revision)
                    )
                    .await
                    .unwrap()
                    .text,
                "Synthetic answer"
            );
        }
        std::fs::remove_file(path).unwrap();
        server.abort();
        let _ = server.await;
        assert_eq!(
            *authorizations.lock().unwrap(),
            vec![
                "authorization: bearer synthetic-first",
                "authorization: bearer synthetic-second"
            ]
        );
        assert_eq!(
            connections.load(Ordering::Relaxed),
            1,
            "sequential requests should retain the existing HTTP connection pool"
        );
        assert_ne!(revisions[0], revisions[1]);
    }

    #[test]
    fn explicit_reuse_validates_policy_but_accepts_new_request_configuration() {
        let config = CustomOpenAiConfig {
            connect_timeout: Duration::from_secs(2),
            ..Default::default()
        };
        let owner = CustomOpenAiClient::new(config.clone()).unwrap();
        let mut request_config = config.clone();
        request_config.endpoint = format!(" {} ", request_config.endpoint);
        request_config.auth_path = PathBuf::from("/synthetic/new-auth.json");
        request_config.model = CustomOpenAiModel::LunaNone;
        request_config.idle_timeout = Duration::from_secs(4);
        request_config.total_timeout = Duration::from_secs(6);
        let request_client = owner.with_reused_transport(request_config).unwrap();
        assert_eq!(
            request_client.config.auth_path,
            PathBuf::from("/synthetic/new-auth.json")
        );
        assert_eq!(request_client.config.model, CustomOpenAiModel::LunaNone);
        assert_eq!(request_client.config.idle_timeout, Duration::from_secs(4));
        assert_eq!(request_client.config.total_timeout, Duration::from_secs(6));
        for invalid in [
            CustomOpenAiConfig {
                endpoint: "http://127.0.0.1:8080".into(),
                ..config.clone()
            },
            CustomOpenAiConfig {
                endpoint: "https://example.test/responses".into(),
                ..config.clone()
            },
            CustomOpenAiConfig {
                connect_timeout: DEFAULT_CONNECT_TIMEOUT,
                ..config.clone()
            },
            CustomOpenAiConfig {
                connect_timeout: Duration::ZERO,
                ..config.clone()
            },
            CustomOpenAiConfig {
                idle_timeout: Duration::ZERO,
                ..config.clone()
            },
            CustomOpenAiConfig {
                total_timeout: Duration::ZERO,
                ..config.clone()
            },
        ] {
            assert!(matches!(
                owner.with_reused_transport(invalid),
                Err(CustomOpenAiError::InvalidConfig)
            ));
        }
        assert!(
            owner.with_reused_transport(config).is_ok(),
            "a rejected configuration must not poison the owner"
        );
    }

    #[cfg(feature = "web-search")]
    #[tokio::test]
    async fn dispatch_observers_exclude_preparation_but_preserve_legacy_timing_origins() {
        use std::cell::Cell;
        use tools::{ModelEvent, RoundOptions, RoundRequest};
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let observed = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(pool_server(listener, Default::default(), observed.clone()));
        let id = AUTH_FILE_ID.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "vox-synthetic-dispatch-{}-{id}.json",
            std::process::id()
        ));
        std::fs::write(
            &path,
            r#"{"openai":{"access":"synthetic-dispatch","expires":4102444800}}"#,
        )
        .unwrap();
        let config = CustomOpenAiConfig {
            endpoint,
            auth_path: path.clone(),
            total_timeout: Duration::from_secs(3),
            ..Default::default()
        };
        let client = CustomOpenAiClient::new(config.clone()).unwrap();
        let mut prompt = prompt();
        prompt.prompt = "Synthetic preparation ".repeat(8000);
        for tool_route in [false, true] {
            let dispatch = Cell::new(None);
            let visible = Cell::new(None);
            let prepared = Cell::new(None);
            let before = observed.lock().unwrap().len();
            let prepare = || {
                std::thread::sleep(Duration::from_millis(80));
                prepared.set(Some(Instant::now()));
            };
            let on_dispatch = |instant: Instant| {
                assert!(dispatch.replace(Some(instant)).is_none());
                assert!(instant >= prepared.get().unwrap());
                assert_eq!(
                    observed.lock().unwrap().len(),
                    before,
                    "send must not be polled before dispatch"
                );
            };
            let on_text = |text: &str| {
                if !text.trim().is_empty() && visible.get().is_none() {
                    visible.set(Some(
                        dispatch.get().expect("dispatch before text").elapsed(),
                    ));
                }
            };
            let response = if tool_route {
                let input = tools::initial_input(&config, &prompt);
                client
                    .tool_round_with_events(
                        RoundRequest {
                            session_id: &prompt.session_id,
                            input: &input,
                            instructions: "Synthetic policy",
                            tools: &[],
                        },
                        RoundOptions {
                            deadline: tokio::time::Instant::now() + Duration::from_secs(3),
                            expected_auth_revision: None,
                            require_tool: false,
                        },
                        |event| {
                            match event {
                                ModelEvent::AuthRevision(_) => prepare(),
                                ModelEvent::RequestDispatched(instant) => on_dispatch(instant),
                                ModelEvent::TextDelta { text, .. } => on_text(text),
                            }
                            Ok(())
                        },
                    )
                    .await
                    .unwrap()
                    .response
            } else {
                client
                    .respond_with_auth_revision_and_dispatch(
                        &prompt,
                        None,
                        on_text,
                        |_| prepare(),
                        on_dispatch,
                    )
                    .await
                    .unwrap()
            };
            assert!(
                response.timings.first_text_at.unwrap()
                    >= visible.get().unwrap() + Duration::from_millis(80),
                "legacy clock must still include preparation"
            );
            assert!(response.timings.headers_at >= Duration::from_millis(80));
            assert!(response.timings.completed_at >= response.timings.first_text_at.unwrap());
        }
        std::fs::remove_file(path).unwrap();
        server.abort();
        let _ = server.await;
    }

    #[tokio::test]
    async fn respond_streams_text_and_sends_sensitive_compatible_request() {
        let events = sse(&[
            r#"{"type":"response.output_text.delta","delta":"hel"}"#,
            r#"{"type":"response.output_text.delta","delta":"lo"}"#,
            r#"{"type":"response.completed","response":{"model":"gpt-6.1-sol","service_tier":"default","usage":{"input_tokens":10,"output_tokens":2,"total_tokens":12,"input_tokens_details":{"cached_tokens":3}}}}"#,
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
        assert_eq!(body["model"], "gpt-6.1-sol");
        assert_eq!(body["reasoning"]["effort"], "high");
        assert_eq!(body["store"], false);
        assert!(body.get("tools").is_none());
        let response = result.expect("response");
        assert_eq!(response.text, "hello");
        assert_eq!(deltas, "hello");
        assert_eq!(response.usage.unwrap().cached_input_tokens, Some(3));
        assert!(response.timings.first_text_at.is_some());
        assert!(response.timings.headers_at <= response.timings.first_text_at.unwrap());
        assert!(response.timings.first_text_at.unwrap() <= response.timings.completed_at);
    }

    #[tokio::test]
    async fn respond_omits_account_header_when_oauth_record_has_no_account() {
        let (request, _, result) = fixture_with_account(
            200,
            "text/event-stream",
            sse(&[
                r#"{"type":"response.output_text.delta","delta":"ok"}"#,
                r#"{"type":"response.completed","response":{"model":"gpt-5.6-sol"}}"#,
            ]),
            Duration::from_secs(1),
            None,
        )
        .await;
        let request = String::from_utf8_lossy(&request).to_ascii_lowercase();
        assert!(request.contains("authorization: bearer test-access"));
        assert!(!request.contains("chatgpt-account-id:"));
        assert!(result.is_ok());
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
        assert_eq!(CustomOpenAiModel::SolHigh.model_id(), "gpt-6.1-sol");
        assert_eq!(CustomOpenAiModel::SolHigh.reasoning_effort(), "high");
        assert_eq!(CustomOpenAiModel::SolNone.model_id(), "gpt-6.1-sol");
        assert_eq!(CustomOpenAiModel::SolNone.reasoning_effort(), "none");
        assert_eq!(CustomOpenAiModel::LunaLow.model_id(), "gpt-5.6-luna");
        assert_eq!(CustomOpenAiModel::LunaLow.reasoning_effort(), "low");
    }

    #[test]
    fn direct_sol_without_reasoning_preserves_the_custom_request_contract() {
        let config = CustomOpenAiConfig {
            model: CustomOpenAiModel::SolNone,
            ..CustomOpenAiConfig::default()
        };
        let request = build_request(&config, &prompt(), None);
        assert_eq!(request["model"], "gpt-6.1-sol");
        assert_eq!(request["reasoning"]["effort"], "none");
        assert_eq!(request["store"], false);
        assert!(request.get("tools").is_none());
    }

    #[test]
    fn direct_gpt6_luna_without_reasoning_sends_exact_model_and_effort() {
        let config = CustomOpenAiConfig {
            model: CustomOpenAiModel::LunaNone,
            ..CustomOpenAiConfig::default()
        };
        let request = build_request(&config, &prompt(), None);
        assert_eq!(request["model"], "gpt-6-luna");
        assert_eq!(request["reasoning"]["effort"], "none");
        assert_eq!(request["store"], false);
        assert!(request.get("tools").is_none());
    }

    #[test]
    fn request_contains_history_reasoning_and_no_tools() {
        let config = CustomOpenAiConfig {
            model: CustomOpenAiModel::LunaLow,
            ..CustomOpenAiConfig::default()
        };
        let request = build_request(&config, &prompt(), None);

        assert_eq!(request["model"], "gpt-5.6-luna");
        assert_eq!(request["reasoning"]["effort"], "low");
        assert_eq!(request["stream"], true);
        assert_eq!(request["store"], false);
        assert!(request.get("tools").is_none());
        assert_eq!(
            request["instructions"],
            voxgolem_core::response_style::compose_instructions(DEFAULT_INSTRUCTIONS)
        );
        assert_eq!(request["input"][0]["content"][0]["type"], "refusal");
        assert_eq!(request["input"][1]["content"][0]["text"], "Say hello");
    }

    #[test]
    fn stage_instructions_replace_instant_defaults_without_inheriting_prose() {
        let config = CustomOpenAiConfig::default();
        let request = build_request(
            &config,
            &prompt(),
            Some("Return strict JSON only: {\"answer\": string}"),
        );
        assert_eq!(
            request["instructions"],
            voxgolem_core::response_style::compose_instructions(
                "Return strict JSON only: {\"answer\": string}"
            )
        );
        assert!(!request["instructions"]
            .as_str()
            .unwrap()
            .contains("First line"));
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
        let request = build_request(&config, &prompt, None);
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
        assert_eq!(credential.account_id.as_deref(), Some("account-id"));
        assert!(matches!(
            parse_credential(valid, 999_999_940_000),
            Err(CustomOpenAiError::AuthExpired)
        ));
        for expiry in ["4102444800.0", "4.1024448e9"] {
            let value = format!(
                r#"{{"openai":{{"access":"secret","accountId":"account-id","expires":{expiry}}}}}"#
            );
            assert!(parse_credential(value.as_bytes(), 1).is_ok());
        }
        assert!(parse_credential(
            br#"{"other":{"type":"api"},"openai":{"type":"oauth","refresh":"refresh-secret","access":"secret","expires":4102444800,"accountId":"account-id"}}"#,
            1,
        )
        .is_ok());
        assert!(parse_credential(
            br#"{"openai":{"type":"oauth","refresh":"refresh-secret","access":"secret","expires":4102444800}}"#,
            1,
        )
        .is_ok());
    }

    #[test]
    fn transport_independent_auth_observation_reads_replacements_and_bounds() {
        let id = AUTH_FILE_ID.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("vox-synthetic-auth-observation-{id}.json"));
        let absent = observe_custom_openai_auth(&path);
        assert_eq!(absent.status, Err(CustomOpenAiError::AuthUnavailable));
        assert_eq!(absent.revision, None);
        std::fs::write(
            &path,
            r#"{"openai":{"access":"synthetic-access","expires":4102444800000}}"#,
        )
        .unwrap();
        let ready = observe_custom_openai_auth(&path);
        assert_eq!(ready.status, Ok(()));
        assert_eq!(ready.revision, custom_openai_auth_revision(&path));
        assert_eq!(
            load_request_credential(&path).unwrap().revision,
            ready.revision.unwrap()
        );
        let replacement = path.with_extension("replacement");
        std::fs::write(
            &replacement,
            r#"{"openai":{"access":"synthetic-expired-access","expires":1}}"#,
        )
        .unwrap();
        std::fs::rename(&replacement, &path).unwrap();
        let expired = observe_custom_openai_auth(&path);
        assert_eq!(expired.status, Err(CustomOpenAiError::AuthExpired));
        assert_ne!(expired.revision, ready.revision);
        assert!(matches!(
            load_request_credential(&path),
            Err(CustomOpenAiError::AuthExpired)
        ));
        for invalid in [
            br#"{"openai":{"access":"synthetic\ninvalid-header","expires":4102444800000}}"#
                .to_vec(),
            br#"{"_assistant":{"generation":"0"}}"#.to_vec(),
            vec![b'x'; MAX_CUSTOM_OPENAI_AUTH_BYTES as usize + 1],
        ] {
            std::fs::write(&path, invalid).unwrap();
            assert_eq!(
                observe_custom_openai_auth(&path).status,
                Err(CustomOpenAiError::AuthUnavailable)
            );
        }
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn check_auth_reports_valid_missing_and_expired_records() {
        let id = AUTH_FILE_ID.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("vox-golem-check-auth-{id}.json"));
        let config = CustomOpenAiConfig {
            auth_path: path.clone(),
            ..Default::default()
        };
        let client = CustomOpenAiClient::new(config).expect("client");

        assert_eq!(client.check_auth(), Err(CustomOpenAiError::AuthUnavailable));
        let expiry = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
            + 3600;
        std::fs::write(
            &path,
            format!(
                r#"{{"openai":{{"access":"secret","accountId":"account","expires":{expiry}}}}}"#
            ),
        )
        .unwrap();
        assert_eq!(client.check_auth(), Ok(()));
        for invalid in [
            format!(
                r#"{{"openai":{{"access":"bad\nvalue","accountId":"account","expires":{expiry}}}}}"#
            ),
            format!(
                r#"{{"openai":{{"access":"secret","accountId":"bad\rvalue","expires":{expiry}}}}}"#
            ),
        ] {
            std::fs::write(&path, invalid).unwrap();
            assert_eq!(client.check_auth(), Err(CustomOpenAiError::AuthUnavailable));
        }
        std::fs::write(
            &path,
            r#"{"openai":{"access":"secret","accountId":"account","expires":1}}"#,
        )
        .unwrap();
        assert_eq!(client.check_auth(), Err(CustomOpenAiError::AuthExpired));
        std::fs::write(
            &path,
            vec![b'x'; (MAX_CUSTOM_OPENAI_AUTH_BYTES + 1) as usize],
        )
        .unwrap();
        assert_eq!(client.check_auth(), Err(CustomOpenAiError::AuthUnavailable));
        std::fs::remove_file(path).ok();
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
