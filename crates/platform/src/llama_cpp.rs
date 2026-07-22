use crate::inference::{ActualInferenceProvider, InferencePolicy};
use crate::managed_process::{configure_owned, terminate_group};
use rand::random;
use serde::{Deserialize, Serialize};
use std::fmt::{Display, Formatter};
use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

#[cfg(windows)]
use std::os::windows::{io::AsRawHandle, process::CommandExt};

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x08000000;

const STARTUP_TIMEOUT: Duration = Duration::from_secs(120);
const HTTP_TIMEOUT: Duration = Duration::from_secs(120);
const HEALTH_TIMEOUT: Duration = Duration::from_millis(500);
const MAX_RESPONSE_HEADERS: usize = 16 * 1024;
const MAX_RESPONSE_BODY: usize = 8 * 1024 * 1024;
const MAX_DECODED_BODY: usize = 8 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LlamaCppServerSpec {
    executable_path: PathBuf,
    model_path: PathBuf,
    host: String,
    port: u16,
    alias: String,
    api_key: String,
}

impl LlamaCppServerSpec {
    pub fn new(
        executable_path: impl Into<PathBuf>,
        model_path: impl Into<PathBuf>,
        host: impl Into<String>,
        port: u16,
        alias: impl Into<String>,
    ) -> Self {
        Self {
            executable_path: executable_path.into(),
            model_path: model_path.into(),
            host: host.into(),
            port,
            alias: alias.into(),
            api_key: new_api_key(),
        }
    }

    /// Build a specification for an already-running authenticated server.
    /// Owned servers must continue to use `new`, which generates a fresh key.
    pub fn external_authenticated(
        executable_path: impl Into<PathBuf>,
        model_path: impl Into<PathBuf>,
        host: impl Into<String>,
        port: u16,
        alias: impl Into<String>,
        api_key: impl Into<String>,
    ) -> Self {
        let mut spec = Self::new(executable_path, model_path, host, port, alias);
        spec.api_key = api_key.into();
        spec
    }

    pub fn executable_path(&self) -> &Path {
        &self.executable_path
    }

    pub fn model_path(&self) -> &Path {
        &self.model_path
    }

    pub fn host(&self) -> &str {
        &self.host
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    pub fn alias(&self) -> &str {
        &self.alias
    }
}

#[derive(Debug)]
pub struct LlamaCppRuntime {
    spec: LlamaCppServerSpec,
    child: Option<Child>,
    #[cfg(windows)]
    process_job: Option<win32job::Job>,
    provider: ActualInferenceProvider,
}

#[derive(Clone, Debug, Default)]
pub struct LlamaCppStartupCancellation(Arc<AtomicBool>);

impl LlamaCppStartupCancellation {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }
    fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

#[derive(Clone, Debug)]
pub struct LlamaCppClient {
    spec: LlamaCppServerSpec,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LlamaCppPrompt {
    system_prompt: Option<String>,
    user_prompt: String,
    max_tokens: u16,
}

impl LlamaCppPrompt {
    pub fn new(user_prompt: impl Into<String>) -> Self {
        Self {
            system_prompt: None,
            user_prompt: user_prompt.into(),
            max_tokens: 256,
        }
    }

    pub fn with_system_prompt(mut self, system_prompt: impl Into<String>) -> Self {
        self.system_prompt = Some(system_prompt.into());
        self
    }

    pub fn with_max_tokens(mut self, max_tokens: u16) -> Self {
        self.max_tokens = max_tokens;
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LlamaCppChatResponse {
    pub text: String,
}

#[derive(Debug)]
pub enum LlamaCppRuntimeError {
    MissingExecutableParent { path: PathBuf },
    SpawnFailed { details: String },
    ProcessJobFailed { details: String },
    StartupTimedOut { host: String, port: u16 },
    StartupCancelled,
    ServerExited { exit_code: Option<i32> },
    HttpFailed { details: String },
    InvalidHttpResponse { details: String },
    InvalidResponsePayload { details: String },
    EmptyAssistantMessage,
}

impl Display for LlamaCppRuntimeError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingExecutableParent { path } => {
                write!(
                    formatter,
                    "llama.cpp executable has no parent directory: {}",
                    path.display()
                )
            }
            Self::SpawnFailed { details } => {
                write!(formatter, "failed to start llama.cpp server: {details}")
            }
            Self::ProcessJobFailed { details } => {
                write!(
                    formatter,
                    "failed to supervise llama.cpp server process: {details}"
                )
            }
            Self::StartupTimedOut { host, port } => {
                write!(
                    formatter,
                    "timed out waiting for llama.cpp server at http://{host}:{port}"
                )
            }
            Self::StartupCancelled => write!(formatter, "llama.cpp startup was cancelled"),
            Self::ServerExited { exit_code } => match exit_code {
                Some(code) => write!(
                    formatter,
                    "llama.cpp server exited during startup with code {code}"
                ),
                None => write!(formatter, "llama.cpp server exited during startup"),
            },
            Self::HttpFailed { details } => {
                write!(formatter, "local llama.cpp request failed: {details}")
            }
            Self::InvalidHttpResponse { details } => {
                write!(
                    formatter,
                    "invalid HTTP response from llama.cpp server: {details}"
                )
            }
            Self::InvalidResponsePayload { details } => {
                write!(formatter, "invalid llama.cpp response payload: {details}")
            }
            Self::EmptyAssistantMessage => {
                write!(formatter, "llama.cpp returned an empty assistant message")
            }
        }
    }
}

impl std::error::Error for LlamaCppRuntimeError {}

impl LlamaCppRuntime {
    pub fn start(spec: LlamaCppServerSpec) -> Result<Self, LlamaCppRuntimeError> {
        if !is_loopback_host(spec.host()) {
            return Err(LlamaCppRuntimeError::HttpFailed {
                details: "llama.cpp host must be loopback".to_string(),
            });
        }
        if matches!(
            send_http_request_with_timeout(&spec, "GET", "/health", None, HEALTH_TIMEOUT),
            Ok(response) if response.status_code == 200
        ) {
            return Ok(Self::attach(spec));
        }

        Self::start_with_policy(spec, InferencePolicy::Auto)
    }

    pub fn start_with_policy(
        spec: LlamaCppServerSpec,
        policy: InferencePolicy,
    ) -> Result<Self, LlamaCppRuntimeError> {
        Self::start_with_policy_cancelled(spec, policy, None)
    }

    pub fn start_with_policy_cancellable(
        spec: LlamaCppServerSpec,
        policy: InferencePolicy,
    ) -> (
        LlamaCppStartupCancellation,
        thread::JoinHandle<Result<Self, LlamaCppRuntimeError>>,
    ) {
        let cancellation = LlamaCppStartupCancellation::default();
        let worker_cancellation = cancellation.clone();
        let worker = Self::start_with_policy_cancellation(spec, policy, worker_cancellation);
        (cancellation, worker)
    }

    pub fn start_with_policy_cancellation(
        spec: LlamaCppServerSpec,
        policy: InferencePolicy,
        cancellation: LlamaCppStartupCancellation,
    ) -> thread::JoinHandle<Result<Self, LlamaCppRuntimeError>> {
        thread::spawn(move || Self::start_with_policy_cancelled(spec, policy, Some(cancellation)))
    }

    fn start_with_policy_cancelled(
        spec: LlamaCppServerSpec,
        policy: InferencePolicy,
        cancellation: Option<LlamaCppStartupCancellation>,
    ) -> Result<Self, LlamaCppRuntimeError> {
        if cancellation
            .as_ref()
            .is_some_and(LlamaCppStartupCancellation::is_cancelled)
        {
            return Err(LlamaCppRuntimeError::StartupCancelled);
        }
        if !is_loopback_host(spec.host()) {
            return Err(LlamaCppRuntimeError::HttpFailed {
                details: "llama.cpp host must be loopback".to_string(),
            });
        }
        if matches!(send_http_request_with_timeout(&spec, "GET", "/health", None, HEALTH_TIMEOUT), Ok(response) if response.status_code == 200)
        {
            return Ok(Self::attach(spec));
        }
        let attempts = match policy {
            InferencePolicy::Auto => [Some(true), Some(false)],
            InferencePolicy::Cuda => [Some(true), None],
            InferencePolicy::Cpu => [Some(false), None],
        };
        let mut last_error = None;
        for (index, cuda) in attempts.into_iter().enumerate() {
            let Some(cuda) = cuda else { break };
            match Self::start_owned(spec.clone(), cuda, cancellation.as_ref()) {
                Ok(runtime) => return Ok(runtime),
                Err(error) => {
                    let retryable = matches!(
                        error,
                        LlamaCppRuntimeError::ServerExited { .. }
                            | LlamaCppRuntimeError::StartupTimedOut { .. }
                    );
                    last_error = Some(error);
                    if policy != InferencePolicy::Auto || index == 1 || !retryable {
                        break;
                    }
                }
            }
        }
        Err(last_error.expect("inference policy has an attempt"))
    }

    fn start_owned(
        spec: LlamaCppServerSpec,
        cuda: bool,
        cancellation: Option<&LlamaCppStartupCancellation>,
    ) -> Result<Self, LlamaCppRuntimeError> {
        if cancellation.is_some_and(|token| token.is_cancelled()) {
            return Err(LlamaCppRuntimeError::StartupCancelled);
        }
        let executable_parent = spec.executable_path().parent().ok_or_else(|| {
            LlamaCppRuntimeError::MissingExecutableParent {
                path: spec.executable_path().to_path_buf(),
            }
        })?;

        let mut command = Command::new(spec.executable_path());
        command
            .current_dir(executable_parent)
            .args([
                "--host",
                spec.host(),
                "--port",
                &spec.port().to_string(),
                "--parallel",
                "1",
                "-m",
                &spec.model_path().to_string_lossy(),
                "-a",
                spec.alias(),
                "-ngl",
                if cuda { "all" } else { "0" },
                "-c",
                "8192",
                "--no-webui",
                "--api-key",
                &spec.api_key,
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        configure_llama_server_command(&mut command);
        configure_owned(&mut command);

        let mut child = command
            .spawn()
            .map_err(|error| LlamaCppRuntimeError::SpawnFailed {
                details: error.to_string(),
            })?;

        #[cfg(windows)]
        let process_job = create_owned_process_job(&child).inspect_err(|_| {
            let _ = child.kill();
            let _ = child.wait();
        })?;
        #[cfg(not(windows))]
        create_owned_process_job(&child).inspect_err(|_| {
            let _ = child.kill();
            let _ = child.wait();
        })?;

        let mut runtime = Self {
            spec,
            child: Some(child),
            #[cfg(windows)]
            process_job,
            provider: if cuda {
                ActualInferenceProvider::RequestedCuda
            } else {
                ActualInferenceProvider::Cpu
            },
        };
        if let Err(error) = runtime.wait_until_ready(cancellation) {
            runtime.shutdown_owned();
            return Err(error);
        }
        Ok(runtime)
    }

    pub fn attach(spec: LlamaCppServerSpec) -> Self {
        Self {
            spec,
            child: None,
            #[cfg(windows)]
            process_job: None,
            provider: ActualInferenceProvider::AttachedUnknown,
        }
    }

    pub fn attach_authenticated(spec: LlamaCppServerSpec, api_key: impl Into<String>) -> Self {
        let mut spec = spec;
        spec.api_key = api_key.into();
        Self::attach(spec)
    }

    pub fn is_owned(&self) -> bool {
        self.child.is_some()
    }

    pub fn actual_provider(&self) -> ActualInferenceProvider {
        self.provider
    }

    pub fn client(&self) -> LlamaCppClient {
        LlamaCppClient {
            spec: self.spec.clone(),
        }
    }

    pub fn shutdown_owned(&mut self) {
        if let Some(mut child) = self.child.take() {
            #[cfg(unix)]
            {
                let _ = terminate_group(child.id(), false);
                let deadline = Instant::now() + Duration::from_secs(2);
                while child.try_wait().ok().flatten().is_none() && Instant::now() < deadline {
                    thread::sleep(Duration::from_millis(10));
                }
                // Signal the group even when the leader exited during grace: descendants
                // remain owned by the group and must not outlive this runtime.
                let _ = terminate_group(child.id(), true);
            }
            #[cfg(windows)]
            let _ = child.kill();
            let _ = child.wait();
        }

        #[cfg(windows)]
        {
            self.process_job = None;
        }
    }

    pub fn chat(
        &self,
        prompt: &LlamaCppPrompt,
    ) -> Result<LlamaCppChatResponse, LlamaCppRuntimeError> {
        chat(&self.spec, prompt)
    }

    fn wait_until_ready(
        &mut self,
        cancellation: Option<&LlamaCppStartupCancellation>,
    ) -> Result<(), LlamaCppRuntimeError> {
        let deadline = Instant::now() + STARTUP_TIMEOUT;

        loop {
            if cancellation.is_some_and(|token| token.is_cancelled()) {
                return Err(LlamaCppRuntimeError::StartupCancelled);
            }
            if let Some(child) = self.child.as_mut() {
                if let Some(status) =
                    child
                        .try_wait()
                        .map_err(|error| LlamaCppRuntimeError::SpawnFailed {
                            details: error.to_string(),
                        })?
                {
                    return Err(LlamaCppRuntimeError::ServerExited {
                        exit_code: status.code(),
                    });
                }
            }

            match send_http_request_with_timeout(&self.spec, "GET", "/health", None, HEALTH_TIMEOUT)
            {
                Ok(response) if response.status_code == 200 => return Ok(()),
                Ok(_) | Err(_) if Instant::now() >= deadline => {
                    return Err(LlamaCppRuntimeError::StartupTimedOut {
                        host: self.spec.host().to_string(),
                        port: self.spec.port(),
                    });
                }
                Ok(_) | Err(_) => thread::sleep(Duration::from_millis(500)),
            }
        }
    }
}

impl LlamaCppClient {
    pub fn chat(
        &self,
        prompt: &LlamaCppPrompt,
    ) -> Result<LlamaCppChatResponse, LlamaCppRuntimeError> {
        chat(&self.spec, prompt)
    }
}

fn chat(
    spec: &LlamaCppServerSpec,
    prompt: &LlamaCppPrompt,
) -> Result<LlamaCppChatResponse, LlamaCppRuntimeError> {
    let request_body =
        serde_json::to_vec(&build_chat_completion_request(spec, prompt)).map_err(|error| {
            LlamaCppRuntimeError::InvalidResponsePayload {
                details: error.to_string(),
            }
        })?;
    let response = send_http_request(
        spec,
        "POST",
        "/v1/chat/completions",
        Some(("application/json", &request_body)),
    )?;

    if response.status_code != 200 {
        return Err(LlamaCppRuntimeError::HttpFailed {
            details: format!(
                "status {}: {}",
                response.status_code,
                String::from_utf8_lossy(&response.body)
            ),
        });
    }

    let payload =
        serde_json::from_slice::<ChatCompletionResponse>(&response.body).map_err(|error| {
            LlamaCppRuntimeError::InvalidResponsePayload {
                details: error.to_string(),
            }
        })?;
    let text = payload
        .choices
        .into_iter()
        .next()
        .and_then(|choice| choice.message.content)
        .map(|content| content.trim().to_string())
        .filter(|content| !content.is_empty())
        .ok_or(LlamaCppRuntimeError::EmptyAssistantMessage)?;

    Ok(LlamaCppChatResponse { text })
}

impl Drop for LlamaCppRuntime {
    fn drop(&mut self) {
        self.shutdown_owned();
    }
}

fn configure_llama_server_command(command: &mut Command) {
    #[cfg(windows)]
    {
        command.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(not(windows))]
    let _ = command;
}

#[cfg(windows)]
fn create_owned_process_job(child: &Child) -> Result<Option<win32job::Job>, LlamaCppRuntimeError> {
    let mut limit_info = win32job::ExtendedLimitInfo::new();
    limit_info.limit_kill_on_job_close();

    let job = win32job::Job::create_with_limit_info(&limit_info).map_err(|error| {
        LlamaCppRuntimeError::ProcessJobFailed {
            details: error.to_string(),
        }
    })?;
    job.assign_process(child.as_raw_handle() as isize)
        .map_err(|error| LlamaCppRuntimeError::ProcessJobFailed {
            details: error.to_string(),
        })?;

    Ok(Some(job))
}

#[cfg(not(windows))]
fn create_owned_process_job(_child: &Child) -> Result<(), LlamaCppRuntimeError> {
    Ok(())
}

#[derive(Debug)]
struct HttpResponse {
    status_code: u16,
    body: Vec<u8>,
}

fn send_http_request(
    spec: &LlamaCppServerSpec,
    method: &str,
    path: &str,
    body: Option<(&str, &[u8])>,
) -> Result<HttpResponse, LlamaCppRuntimeError> {
    send_http_request_with_timeout(spec, method, path, body, HTTP_TIMEOUT)
}

fn send_http_request_with_timeout(
    spec: &LlamaCppServerSpec,
    method: &str,
    path: &str,
    body: Option<(&str, &[u8])>,
    timeout: Duration,
) -> Result<HttpResponse, LlamaCppRuntimeError> {
    if spec
        .api_key
        .bytes()
        .any(|byte| byte <= 0x1f || byte == 0x7f)
    {
        return Err(LlamaCppRuntimeError::HttpFailed {
            details: "llama.cpp API key contains invalid HTTP header bytes".to_string(),
        });
    }
    let deadline = Instant::now() + timeout;
    let addresses = (spec.host(), spec.port())
        .to_socket_addrs()
        .map_err(|error| LlamaCppRuntimeError::HttpFailed {
            details: error.to_string(),
        })?;
    let mut last_error = None;
    let mut stream = None;
    for address in addresses {
        let remaining = remaining_http_timeout(deadline)?;
        match TcpStream::connect_timeout(&address, remaining) {
            Ok(connected) => {
                stream = Some(connected);
                break;
            }
            Err(error) => last_error = Some(error),
        }
    }
    let mut stream = stream.ok_or_else(|| LlamaCppRuntimeError::HttpFailed {
        details: last_error.map_or_else(
            || String::from("llama.cpp host did not resolve"),
            |error| error.to_string(),
        ),
    })?;
    stream
        .set_read_timeout(Some(remaining_http_timeout(deadline)?))
        .map_err(|error| LlamaCppRuntimeError::HttpFailed {
            details: error.to_string(),
        })?;
    stream
        .set_write_timeout(Some(remaining_http_timeout(deadline)?))
        .map_err(|error| LlamaCppRuntimeError::HttpFailed {
            details: error.to_string(),
        })?;

    let mut request = format!(
        "{method} {path} HTTP/1.1\r\nHost: {}\r\nAuthorization: Bearer {}\r\nConnection: close\r\nAccept: application/json\r\n",
        http_host_authority(spec.host(), spec.port()),
        spec.api_key,
    );
    if let Some((content_type, request_body)) = body {
        request.push_str(&format!(
            "Content-Type: {content_type}\r\nContent-Length: {}\r\n",
            request_body.len()
        ));
        request.push_str("\r\n");
        stream
            .write_all(request.as_bytes())
            .and_then(|_| stream.write_all(request_body))
            .and_then(|_| stream.flush())
            .map_err(|error| LlamaCppRuntimeError::HttpFailed {
                details: error.to_string(),
            })?;
    } else {
        request.push_str("\r\n");
        stream
            .write_all(request.as_bytes())
            .and_then(|_| stream.flush())
            .map_err(|error| LlamaCppRuntimeError::HttpFailed {
                details: error.to_string(),
            })?;
    }

    let mut raw_response = Vec::new();
    let mut buffer = [0_u8; 8192];
    loop {
        stream
            .set_read_timeout(Some(remaining_http_timeout(deadline)?))
            .map_err(|error| LlamaCppRuntimeError::HttpFailed {
                details: error.to_string(),
            })?;
        let read = stream
            .read(&mut buffer)
            .map_err(|error| LlamaCppRuntimeError::HttpFailed {
                details: error.to_string(),
            })?;
        if read == 0 {
            break;
        }
        if read > MAX_RESPONSE_HEADERS + MAX_RESPONSE_BODY - raw_response.len() {
            return Err(LlamaCppRuntimeError::InvalidHttpResponse {
                details: "response exceeds size limit".to_string(),
            });
        }
        raw_response.extend_from_slice(&buffer[..read]);
    }

    parse_http_response(&raw_response)
}

fn remaining_http_timeout(deadline: Instant) -> Result<Duration, LlamaCppRuntimeError> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|remaining| !remaining.is_zero())
        .ok_or_else(|| LlamaCppRuntimeError::HttpFailed {
            details: String::from("request timed out"),
        })
}

fn parse_http_response(raw_response: &[u8]) -> Result<HttpResponse, LlamaCppRuntimeError> {
    let header_end = raw_response
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .ok_or_else(|| LlamaCppRuntimeError::InvalidHttpResponse {
            details: "missing header terminator".to_string(),
        })?;
    if header_end > MAX_RESPONSE_HEADERS {
        return Err(LlamaCppRuntimeError::InvalidHttpResponse {
            details: "response headers exceed size limit".to_string(),
        });
    }
    let header_bytes = &raw_response[..header_end];
    let body_bytes = &raw_response[(header_end + 4)..];
    let header_text = String::from_utf8_lossy(header_bytes);
    let mut header_lines = header_text.lines();
    let status_line =
        header_lines
            .next()
            .ok_or_else(|| LlamaCppRuntimeError::InvalidHttpResponse {
                details: "missing status line".to_string(),
            })?;
    let status_code = status_line
        .split_whitespace()
        .nth(1)
        .ok_or_else(|| LlamaCppRuntimeError::InvalidHttpResponse {
            details: "missing status code".to_string(),
        })?
        .parse::<u16>()
        .map_err(|error| LlamaCppRuntimeError::InvalidHttpResponse {
            details: error.to_string(),
        })?;
    let chunked = header_lines.any(|line| {
        let trimmed = line.trim_end_matches(['\r', '\n']);
        trimmed.eq_ignore_ascii_case("Transfer-Encoding: chunked")
    });
    if body_bytes.len() > MAX_RESPONSE_BODY {
        return Err(LlamaCppRuntimeError::InvalidHttpResponse {
            details: "response body exceeds size limit".to_string(),
        });
    }
    let body = if chunked {
        decode_chunked_body(body_bytes)?
    } else {
        body_bytes.to_vec()
    };

    Ok(HttpResponse { status_code, body })
}

fn decode_chunked_body(body: &[u8]) -> Result<Vec<u8>, LlamaCppRuntimeError> {
    let mut remaining = body;
    let mut decoded = Vec::new();

    loop {
        let line_end = remaining
            .windows(2)
            .position(|window| window == b"\r\n")
            .ok_or_else(|| LlamaCppRuntimeError::InvalidHttpResponse {
                details: "missing chunk size terminator".to_string(),
            })?;
        let size_text = String::from_utf8_lossy(&remaining[..line_end]);
        let chunk_size = usize::from_str_radix(size_text.trim(), 16).map_err(|error| {
            LlamaCppRuntimeError::InvalidHttpResponse {
                details: error.to_string(),
            }
        })?;
        remaining = &remaining[(line_end + 2)..];

        if chunk_size == 0 {
            return Ok(decoded);
        }

        let required =
            chunk_size
                .checked_add(2)
                .ok_or_else(|| LlamaCppRuntimeError::InvalidHttpResponse {
                    details: "chunk size overflows response limits".to_string(),
                })?;
        if remaining.len() < required {
            return Err(LlamaCppRuntimeError::InvalidHttpResponse {
                details: "chunk smaller than declared size".to_string(),
            });
        }

        if chunk_size > MAX_DECODED_BODY - decoded.len() {
            return Err(LlamaCppRuntimeError::InvalidHttpResponse {
                details: "decoded body exceeds size limit".to_string(),
            });
        }
        decoded.extend_from_slice(&remaining[..chunk_size]);
        remaining = &remaining[(chunk_size + 2)..];
    }
}

fn is_loopback_host(host: &str) -> bool {
    matches!(host, "localhost" | "127.0.0.1" | "::1")
}

fn http_host_authority(host: &str, port: u16) -> String {
    let host = host.trim_start_matches('[').trim_end_matches(']');
    if host.contains(':') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    }
}

fn new_api_key() -> String {
    format!("vox-{:032x}", random::<u128>())
}

fn build_chat_completion_request<'a>(
    spec: &'a LlamaCppServerSpec,
    prompt: &'a LlamaCppPrompt,
) -> ChatCompletionRequest<'a> {
    let mut messages = Vec::new();
    if let Some(system_prompt) = prompt.system_prompt.as_deref() {
        messages.push(ChatCompletionMessage {
            role: "system",
            content: system_prompt,
        });
    }
    messages.push(ChatCompletionMessage {
        role: "user",
        content: &prompt.user_prompt,
    });

    ChatCompletionRequest {
        model: spec.alias(),
        messages,
        max_tokens: prompt.max_tokens,
        temperature: 0.35,
        stream: false,
    }
}

#[derive(Debug, Serialize)]
struct ChatCompletionRequest<'a> {
    model: &'a str,
    messages: Vec<ChatCompletionMessage<'a>>,
    max_tokens: u16,
    temperature: f32,
    stream: bool,
}

#[derive(Debug, Serialize)]
struct ChatCompletionMessage<'a> {
    role: &'a str,
    content: &'a str,
}

#[derive(Debug, Deserialize)]
struct ChatCompletionResponse {
    choices: Vec<ChatCompletionChoice>,
}

#[derive(Debug, Deserialize)]
struct ChatCompletionChoice {
    message: ChatCompletionAssistantMessage,
}

#[derive(Debug, Deserialize)]
struct ChatCompletionAssistantMessage {
    content: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::{
        build_chat_completion_request, decode_chunked_body, http_host_authority, is_loopback_host,
        parse_http_response, send_http_request, send_http_request_with_timeout, InferencePolicy,
        LlamaCppPrompt, LlamaCppRuntime, LlamaCppRuntimeError, LlamaCppServerSpec,
    };
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::thread;
    use std::time::Duration;

    #[test]
    fn parse_http_response_reads_content_length_body() {
        let response =
            parse_http_response(b"HTTP/1.1 200 OK\r\nContent-Length: 12\r\n\r\nhello world!")
                .expect("response should parse");

        assert_eq!(response.status_code, 200);
        assert_eq!(response.body, b"hello world!");
    }

    #[test]
    fn decode_chunked_body_concatenates_chunks() {
        let body = decode_chunked_body(b"5\r\nhello\r\n6\r\n world\r\n0\r\n\r\n")
            .expect("chunked body should decode");

        assert_eq!(body, b"hello world");
    }

    #[test]
    fn ipv6_host_authority_is_bracketed_without_accepting_bracketed_config() {
        assert_eq!(http_host_authority("::1", 11435), "[::1]:11435");
        assert!(is_loopback_host("::1"));
        assert!(!is_loopback_host("[::1]"));
    }

    #[test]
    fn build_chat_completion_request_includes_system_prompt_when_present() {
        let spec = LlamaCppServerSpec::new(
            "llama-server.exe",
            "model.gguf",
            "127.0.0.1",
            11435,
            "default",
        );
        let prompt = LlamaCppPrompt::new("hello")
            .with_system_prompt("be concise")
            .with_max_tokens(42);

        let request = build_chat_completion_request(&spec, &prompt);

        assert_eq!(request.model, "default");
        assert_eq!(request.max_tokens, 42);
        assert_eq!(request.messages.len(), 2);
        assert_eq!(request.messages[0].role, "system");
        assert_eq!(request.messages[0].content, "be concise");
        assert_eq!(request.messages[1].role, "user");
        assert_eq!(request.messages[1].content, "hello");
    }

    #[test]
    fn start_reuses_running_server_and_does_not_stop_external_server_behavior() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("listener should bind");
        let port = listener
            .local_addr()
            .expect("listener should expose local addr")
            .port();
        listener
            .set_nonblocking(true)
            .expect("listener should allow nonblocking mode");

        let shutdown = Arc::new(AtomicBool::new(false));
        let shutdown_for_thread = Arc::clone(&shutdown);
        let server_thread = thread::spawn(move || {
            while !shutdown_for_thread.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let mut request_buffer = [0_u8; 1024];
                        let _ = stream.read(&mut request_buffer);
                        let _ = stream.write_all(
                            b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok",
                        );
                        let _ = stream.flush();
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(10));
                    }
                    Err(error) => panic!("test server accept failed: {error}"),
                }
            }
        });

        let spec = LlamaCppServerSpec::new(
            "llama-server.exe",
            "model.gguf",
            "127.0.0.1",
            port,
            "default",
        );

        let runtime =
            LlamaCppRuntime::start(spec).expect("runtime should attach to running server");
        assert!(!runtime.is_owned());
        let client_spec = runtime.spec.clone();
        drop(runtime);

        let response = send_http_request(&client_spec, "GET", "/health", None)
            .expect("health request should succeed after runtime drop");
        assert_eq!(response.status_code, 200);

        shutdown.store(true, Ordering::SeqCst);
        server_thread
            .join()
            .expect("server thread should exit cleanly");
    }

    #[test]
    fn health_request_enforces_absolute_deadline_against_trickled_bytes() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("listener should bind");
        let port = listener.local_addr().expect("listener address").port();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("health request should connect");
            let mut request = [0_u8; 1024];
            let _ = stream.read(&mut request);
            for byte in b"HTTP/1.1 200 OK\r\nContent-Length: 1\r\n\r\nx" {
                if stream.write_all(&[*byte]).is_err() {
                    break;
                }
                thread::sleep(Duration::from_millis(40));
            }
        });
        let spec =
            LlamaCppServerSpec::new("llama-server", "model.gguf", "127.0.0.1", port, "default");
        let started = std::time::Instant::now();

        let result = send_http_request_with_timeout(
            &spec,
            "GET",
            "/health",
            None,
            Duration::from_millis(150),
        );

        assert!(result.is_err());
        assert!(started.elapsed() < Duration::from_secs(1));
        server
            .join()
            .expect("server should stop after client timeout");
    }

    #[test]
    fn external_api_key_rejects_http_header_control_bytes() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("listener should bind");
        let port = listener.local_addr().expect("listener address").port();
        let spec = LlamaCppServerSpec::external_authenticated(
            "llama-server",
            "model.gguf",
            "127.0.0.1",
            port,
            "default",
            "valid\r\nX-Injected: true",
        );

        let result = send_http_request_with_timeout(
            &spec,
            "GET",
            "/health",
            None,
            Duration::from_millis(20),
        );

        assert!(matches!(
            result,
            Err(LlamaCppRuntimeError::HttpFailed { details })
                if details == "llama.cpp API key contains invalid HTTP header bytes"
        ));
    }

    #[cfg(unix)]
    #[test]
    fn cancellable_startup_reaps_owned_child_when_cancelled_during_readiness() {
        use std::os::unix::fs::PermissionsExt;
        let path =
            std::env::temp_dir().join(format!("vox-golem-llama-stub-{}", std::process::id()));
        std::fs::write(&path, b"#!/bin/sh\nsleep 30\n").expect("stub should be written");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))
            .expect("stub should be executable");
        let spec = LlamaCppServerSpec::new(&path, "missing.gguf", "127.0.0.1", 1, "test");
        let cancel = super::LlamaCppStartupCancellation::default();
        let worker = LlamaCppRuntime::start_with_policy_cancellation(
            spec,
            InferencePolicy::Cpu,
            cancel.clone(),
        );
        std::thread::sleep(Duration::from_millis(100));
        cancel.cancel();
        let result = worker.join().expect("startup worker should join");
        assert!(matches!(
            result,
            Err(LlamaCppRuntimeError::StartupCancelled)
        ));
        std::fs::remove_file(path).expect("stub should be removed");
    }
}
