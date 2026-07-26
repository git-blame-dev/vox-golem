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
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

thread_local! {
    static IN_CHAT_PUBLICATION_CALLBACK: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

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
const MAX_STREAMING_ERROR_BODY: usize = 4 * 1024;
const MAX_CHUNK_LINE: usize = 8 * 1024;
const MAX_CHUNK_TRAILER: usize = 8 * 1024;

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

#[derive(Debug)]
pub struct LlamaCppChatCancellation {
    cancelled: Arc<AtomicBool>,
    publication_gate: Arc<Mutex<()>>,
}

impl Default for LlamaCppChatCancellation {
    fn default() -> Self {
        Self {
            cancelled: Arc::new(AtomicBool::new(false)),
            publication_gate: Arc::new(Mutex::new(())),
        }
    }
}

impl Clone for LlamaCppChatCancellation {
    fn clone(&self) -> Self {
        Self {
            cancelled: Arc::clone(&self.cancelled),
            publication_gate: Arc::clone(&self.publication_gate),
        }
    }
}

impl LlamaCppChatCancellation {
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
        // Cancellation returns only after any in-flight publication has completed.
        // This makes the cancelled bit and the publication boundary one observable event.
        if !IN_CHAT_PUBLICATION_CALLBACK.with(std::cell::Cell::get) {
            let _gate = self
                .publication_gate
                .lock()
                .expect("chat publication gate should not be poisoned");
        }
    }

    fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
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
    ChatCancelled,
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
            Self::ChatCancelled => write!(formatter, "llama.cpp chat was cancelled"),
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
    pub fn chat_streaming<F>(
        &self,
        prompt: &LlamaCppPrompt,
        cancellation: &LlamaCppChatCancellation,
        mut on_delta: F,
    ) -> Result<LlamaCppChatResponse, LlamaCppRuntimeError>
    where
        F: FnMut(&str),
    {
        chat_streaming(&self.spec, prompt, cancellation, &mut on_delta)
    }
}

fn chat_streaming<F>(
    spec: &LlamaCppServerSpec,
    prompt: &LlamaCppPrompt,
    cancellation: &LlamaCppChatCancellation,
    on_delta: &mut F,
) -> Result<LlamaCppChatResponse, LlamaCppRuntimeError>
where
    F: FnMut(&str),
{
    if cancellation.is_cancelled() {
        return Err(LlamaCppRuntimeError::ChatCancelled);
    }
    validate_api_key_header(&spec.api_key)?;
    let body = serde_json::to_vec(&build_chat_completion_request_streaming(spec, prompt)).map_err(
        |error| LlamaCppRuntimeError::InvalidResponsePayload {
            details: error.to_string(),
        },
    )?;
    let deadline = Instant::now() + HTTP_TIMEOUT;
    let mut stream = connect_http(spec, deadline, cancellation)?;
    let request = format!(
        "POST /v1/chat/completions HTTP/1.1\r\nHost: {}\r\nAuthorization: Bearer {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\nAccept: text/event-stream\r\n\r\n",
        http_host_authority(spec.host(), spec.port()), spec.api_key, body.len()
    );
    checked_write(&mut stream, request.as_bytes(), deadline, cancellation)?;
    checked_write(&mut stream, &body, deadline, cancellation)?;
    if cancellation.is_cancelled() {
        return Err(LlamaCppRuntimeError::ChatCancelled);
    }
    stream
        .set_write_timeout(Some(Duration::from_millis(50)))
        .map_err(|error| LlamaCppRuntimeError::HttpFailed {
            details: error.to_string(),
        })?;
    stream
        .flush()
        .map_err(|error| LlamaCppRuntimeError::HttpFailed {
            details: error.to_string(),
        })?;
    if cancellation.is_cancelled() {
        return Err(LlamaCppRuntimeError::ChatCancelled);
    }

    let mut raw = Vec::new();
    let mut pending_sse = Vec::new();
    let mut buffer = [0_u8; 8192];
    let mut header_parsed = false;
    let mut body_start = 0;
    let mut error_status = None;
    let mut chunked = None;
    let mut chunk_decoder = ChunkedDecoder::default();
    let mut error_body = Vec::new();
    let mut error_content_length = None;
    let mut sse_done = false;
    let mut output = String::new();
    loop {
        if cancellation.is_cancelled() {
            return Err(LlamaCppRuntimeError::ChatCancelled);
        }
        stream
            .set_read_timeout(Some(
                remaining_http_timeout(deadline)?.min(Duration::from_millis(50)),
            ))
            .map_err(|error| LlamaCppRuntimeError::HttpFailed {
                details: error.to_string(),
            })?;
        match stream.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => {
                if !header_parsed && raw.len() > MAX_RESPONSE_HEADERS + 4 - read {
                    return Err(LlamaCppRuntimeError::InvalidHttpResponse {
                        details: "response headers exceed size limit".into(),
                    });
                }
                if header_parsed && pending_sse.len() > MAX_RESPONSE_BODY - read {
                    return Err(LlamaCppRuntimeError::InvalidHttpResponse {
                        details: "streaming response exceeds size limit".into(),
                    });
                }
                raw.extend_from_slice(&buffer[..read]);
                if !header_parsed {
                    if let Some(end) = raw.windows(4).position(|w| w == b"\r\n\r\n") {
                        let (status_code, is_chunked, _, content_length) =
                            parse_http_headers(&raw[..end + 4])?;
                        header_parsed = true;
                        body_start = end + 4;
                        chunked = Some(is_chunked);
                        if status_code != 200 {
                            error_status = Some(status_code);
                            error_content_length = content_length;
                        }
                        raw.drain(..body_start);
                        body_start = 0;
                    }
                }
                if error_status.is_some() {
                    let body = &raw[body_start..];
                    let decoded = if chunked == Some(true) {
                        chunk_decoder.feed(body)?
                    } else {
                        body.to_vec()
                    };
                    let remaining = MAX_STREAMING_ERROR_BODY - error_body.len();
                    error_body.extend_from_slice(&decoded[..decoded.len().min(remaining)]);
                    if error_body.len() >= MAX_STREAMING_ERROR_BODY
                        || error_content_length.is_some_and(|length| error_body.len() >= length)
                        || chunked == Some(true) && chunk_decoder.is_finished()
                    {
                        break;
                    }
                    raw.drain(body_start..);
                    body_start = 0;
                    continue;
                }
                if header_parsed {
                    let body = &raw[body_start..];
                    let decoded = if chunked == Some(true) {
                        chunk_decoder.feed(body)?
                    } else {
                        body.to_vec()
                    };
                    if !decoded.is_empty() && !sse_done {
                        pending_sse.extend_from_slice(&decoded);
                        if pending_sse.len() > MAX_RESPONSE_BODY {
                            return Err(LlamaCppRuntimeError::InvalidHttpResponse {
                                details: "streaming event exceeds size limit".into(),
                            });
                        }
                        sse_done |= consume_sse_pending(
                            &mut pending_sse,
                            cancellation,
                            &mut output,
                            on_delta,
                        )?;
                        if sse_done {
                            pending_sse.clear();
                        }
                    }
                    raw.drain(body_start..);
                    body_start = 0;
                    if sse_done && (chunked != Some(true) || chunk_decoder.is_finished()) {
                        break;
                    }
                }
            }
            Err(error)
                if error.kind() == std::io::ErrorKind::WouldBlock
                    || error.kind() == std::io::ErrorKind::TimedOut =>
            {
                continue
            }
            Err(error)
                if error_status.is_some()
                    && error.kind() == std::io::ErrorKind::ConnectionReset =>
            {
                break
            }
            Err(error) => {
                return Err(LlamaCppRuntimeError::HttpFailed {
                    details: error.to_string(),
                })
            }
        }
    }
    if cancellation.is_cancelled() {
        return Err(LlamaCppRuntimeError::ChatCancelled);
    }
    if let Some(status_code) = error_status {
        let details =
            String::from_utf8_lossy(&error_body[..error_body.len().min(MAX_STREAMING_ERROR_BODY)]);
        return Err(LlamaCppRuntimeError::HttpFailed {
            details: format!("status {status_code}: {details}"),
        });
    }
    if !header_parsed
        || chunked == Some(true) && !chunk_decoder.is_finished()
        || !pending_sse.is_empty()
        || !sse_done
    {
        return Err(LlamaCppRuntimeError::InvalidHttpResponse {
            details: "truncated streaming response".into(),
        });
    }
    if output.trim().is_empty() {
        return Err(LlamaCppRuntimeError::EmptyAssistantMessage);
    }
    Ok(LlamaCppChatResponse { text: output })
}

fn consume_sse_pending<F>(
    pending: &mut Vec<u8>,
    cancellation: &LlamaCppChatCancellation,
    output: &mut String,
    on_delta: &mut F,
) -> Result<bool, LlamaCppRuntimeError>
where
    F: FnMut(&str),
{
    let mut done = false;
    while let Some(end) = pending.windows(1).position(|window| window == b"\n") {
        let line = pending.drain(..=end).collect::<Vec<_>>();
        let line = line[..line.len() - 1]
            .strip_suffix(b"\r")
            .unwrap_or(&line[..line.len() - 1]);
        let Some(data) = line.strip_prefix(b"data: ") else {
            continue;
        };
        if data == b"[DONE]" {
            done = true;
            break;
        }
        let event: ChatCompletionStreamResponse =
            serde_json::from_slice(data).map_err(|error| {
                LlamaCppRuntimeError::InvalidResponsePayload {
                    details: error.to_string(),
                }
            })?;
        if let Some(delta) = event
            .choices
            .into_iter()
            .next()
            .and_then(|choice| choice.delta.content)
        {
            if delta.len() > MAX_RESPONSE_BODY - output.len() {
                return Err(LlamaCppRuntimeError::InvalidResponsePayload {
                    details: "generated text exceeds size limit".into(),
                });
            }
            let _publication_gate = cancellation
                .publication_gate
                .lock()
                .map_err(|_| LlamaCppRuntimeError::ChatCancelled)?;
            if cancellation.is_cancelled() {
                return Err(LlamaCppRuntimeError::ChatCancelled);
            }
            output.push_str(&delta);
            IN_CHAT_PUBLICATION_CALLBACK.with(|in_callback| {
                let previous = in_callback.replace(true);
                on_delta(&delta);
                in_callback.set(previous);
            });
        }
    }
    Ok(done)
}

fn connect_http(
    spec: &LlamaCppServerSpec,
    deadline: Instant,
    cancellation: &LlamaCppChatCancellation,
) -> Result<TcpStream, LlamaCppRuntimeError> {
    let addresses = (spec.host(), spec.port())
        .to_socket_addrs()
        .map_err(|error| LlamaCppRuntimeError::HttpFailed {
            details: error.to_string(),
        })?;
    for address in addresses {
        if cancellation.is_cancelled() {
            return Err(LlamaCppRuntimeError::ChatCancelled);
        }
        if cancellation.is_cancelled() {
            return Err(LlamaCppRuntimeError::ChatCancelled);
        }
        if let Ok(stream) = TcpStream::connect_timeout(
            &address,
            remaining_http_timeout(deadline)?.min(Duration::from_millis(50)),
        ) {
            return Ok(stream);
        }
    }
    Err(LlamaCppRuntimeError::HttpFailed {
        details: "could not connect to llama.cpp".into(),
    })
}

fn checked_write(
    stream: &mut TcpStream,
    bytes: &[u8],
    deadline: Instant,
    cancellation: &LlamaCppChatCancellation,
) -> Result<(), LlamaCppRuntimeError> {
    if cancellation.is_cancelled() {
        return Err(LlamaCppRuntimeError::ChatCancelled);
    }
    let mut written = 0;
    while written < bytes.len() {
        if cancellation.is_cancelled() {
            return Err(LlamaCppRuntimeError::ChatCancelled);
        }
        stream
            .set_write_timeout(Some(
                remaining_http_timeout(deadline)?.min(Duration::from_millis(50)),
            ))
            .map_err(|error| LlamaCppRuntimeError::HttpFailed {
                details: error.to_string(),
            })?;
        match stream.write(&bytes[written..]) {
            Ok(0) => {
                return Err(LlamaCppRuntimeError::HttpFailed {
                    details: "connection closed while writing request".into(),
                });
            }
            Ok(count) => written += count,
            Err(error)
                if error.kind() == std::io::ErrorKind::WouldBlock
                    || error.kind() == std::io::ErrorKind::TimedOut =>
            {
                continue
            }
            Err(error) => {
                return Err(LlamaCppRuntimeError::HttpFailed {
                    details: error.to_string(),
                });
            }
        }
    }
    Ok(())
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
    #[cfg(test)]
    body: Vec<u8>,
}

#[cfg(test)]
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
    validate_api_key_header(&spec.api_key)?;
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

fn validate_api_key_header(api_key: &str) -> Result<(), LlamaCppRuntimeError> {
    if api_key.bytes().any(|byte| byte <= 0x1f || byte == 0x7f) {
        return Err(LlamaCppRuntimeError::HttpFailed {
            details: "llama.cpp API key contains invalid HTTP header bytes".to_string(),
        });
    }
    Ok(())
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
    let (status_code, chunked, body_start, _) = parse_http_headers(raw_response)?;
    let body_bytes = &raw_response[body_start..];
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
    #[cfg(not(test))]
    let _ = body;
    Ok(HttpResponse {
        status_code,
        #[cfg(test)]
        body,
    })
}

fn parse_http_headers(
    raw_response: &[u8],
) -> Result<(u16, bool, usize, Option<usize>), LlamaCppRuntimeError> {
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
    let header_text = String::from_utf8_lossy(&raw_response[..header_end]);
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
    let mut content_length = None;
    let mut chunked = false;
    for line in header_lines {
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if let Some((name, value)) = trimmed.split_once(':') {
            if name.eq_ignore_ascii_case("Content-Length") {
                content_length = Some(value.trim().parse::<usize>().map_err(|error| {
                    LlamaCppRuntimeError::InvalidHttpResponse {
                        details: error.to_string(),
                    }
                })?);
            }
        }
        if trimmed.eq_ignore_ascii_case("Transfer-Encoding: chunked") {
            chunked = true;
        }
    }
    Ok((status_code, chunked, header_end + 4, content_length))
}

#[derive(Default)]
struct ChunkedDecoder {
    input: Vec<u8>,
    decoded: usize,
    expected: Option<usize>,
    terminal: bool,
    finished: bool,
}

impl ChunkedDecoder {
    fn is_finished(&self) -> bool {
        self.finished
    }

    fn feed(&mut self, bytes: &[u8]) -> Result<Vec<u8>, LlamaCppRuntimeError> {
        self.input.extend_from_slice(bytes);
        let mut output = Vec::new();
        loop {
            if self.finished {
                break;
            }
            if self.terminal {
                let Some(line_end) = self.input.windows(2).position(|window| window == b"\r\n")
                else {
                    if self.input.len() > MAX_CHUNK_TRAILER {
                        return Err(LlamaCppRuntimeError::InvalidHttpResponse {
                            details: "chunk trailers exceed size limit".into(),
                        });
                    }
                    break;
                };
                if line_end == 0 {
                    self.input.drain(..2);
                    self.finished = true;
                } else {
                    let trailer = &self.input[..line_end];
                    if trailer.len() > MAX_CHUNK_TRAILER {
                        return Err(LlamaCppRuntimeError::InvalidHttpResponse {
                            details: "chunk trailer exceeds size limit".into(),
                        });
                    }
                    if !trailer.contains(&b':') {
                        return Err(LlamaCppRuntimeError::InvalidHttpResponse {
                            details: "malformed chunk trailer".into(),
                        });
                    }
                    self.input.drain(..line_end + 2);
                }
                continue;
            }
            if let Some(size) = self.expected {
                if size > MAX_DECODED_BODY - self.decoded {
                    return Err(LlamaCppRuntimeError::InvalidHttpResponse {
                        details: "decoded body exceeds size limit".into(),
                    });
                }
                let required = size.checked_add(2).ok_or_else(|| {
                    LlamaCppRuntimeError::InvalidHttpResponse {
                        details: "chunk size overflows response limits".into(),
                    }
                })?;
                if self.input.len() < required {
                    break;
                }
                if &self.input[size..required] != b"\r\n" {
                    return Err(LlamaCppRuntimeError::InvalidHttpResponse {
                        details: "chunk missing terminator".into(),
                    });
                }
                output.extend_from_slice(&self.input[..size]);
                self.decoded += size;
                self.input.drain(..required);
                self.expected = None;
                continue;
            }
            let Some(line_end) = self.input.windows(2).position(|window| window == b"\r\n") else {
                if self.input.len() > MAX_CHUNK_LINE {
                    return Err(LlamaCppRuntimeError::InvalidHttpResponse {
                        details: "chunk size line exceeds size limit".into(),
                    });
                }
                break;
            };
            if line_end > MAX_CHUNK_LINE {
                return Err(LlamaCppRuntimeError::InvalidHttpResponse {
                    details: "chunk size line exceeds size limit".into(),
                });
            }
            let size_text = String::from_utf8_lossy(&self.input[..line_end]);
            let size_text = size_text.split(';').next().unwrap_or_default().trim();
            let size = usize::from_str_radix(size_text, 16).map_err(|error| {
                LlamaCppRuntimeError::InvalidHttpResponse {
                    details: error.to_string(),
                }
            })?;
            self.input.drain(..line_end + 2);
            if size == 0 {
                self.terminal = true;
                continue;
            }
            self.expected = Some(size);
        }
        Ok(output)
    }
}

fn decode_chunked_body(body: &[u8]) -> Result<Vec<u8>, LlamaCppRuntimeError> {
    let mut decoder = ChunkedDecoder::default();
    let decoded = decoder.feed(body)?;
    if !decoder.is_finished() {
        return Err(LlamaCppRuntimeError::InvalidHttpResponse {
            details: "incomplete chunk trailers".into(),
        });
    }
    Ok(decoded)
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

fn build_chat_completion_request_streaming<'a>(
    spec: &'a LlamaCppServerSpec,
    prompt: &'a LlamaCppPrompt,
) -> ChatCompletionRequest<'a> {
    let mut request = build_chat_completion_request(spec, prompt);
    request.stream = true;
    request
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
struct ChatCompletionStreamResponse {
    choices: Vec<ChatCompletionStreamChoice>,
}

#[derive(Debug, Deserialize)]
struct ChatCompletionStreamChoice {
    delta: ChatCompletionDelta,
}

#[derive(Debug, Deserialize)]
struct ChatCompletionDelta {
    content: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::{
        build_chat_completion_request, decode_chunked_body, http_host_authority, is_loopback_host,
        parse_http_response, send_http_request, send_http_request_with_timeout, ChunkedDecoder,
        InferencePolicy, LlamaCppChatCancellation, LlamaCppPrompt, LlamaCppRuntime,
        LlamaCppRuntimeError, LlamaCppServerSpec,
    };
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::thread;
    use std::time::Duration;

    fn read_http_request(stream: &mut std::net::TcpStream) -> String {
        let mut request = Vec::new();
        let mut expected_len = None;
        loop {
            let mut chunk = [0_u8; 1024];
            let read = stream.read(&mut chunk).expect("test request should read");
            assert!(read > 0, "test request ended before its declared body");
            request.extend_from_slice(&chunk[..read]);
            assert!(request.len() <= 16 * 1024, "test request exceeded limit");

            if expected_len.is_none() {
                if let Some(header_end) = request.windows(4).position(|part| part == b"\r\n\r\n") {
                    let body_start = header_end + 4;
                    let headers =
                        std::str::from_utf8(&request[..header_end]).expect("request headers");
                    let content_len = headers
                        .lines()
                        .find_map(|line| {
                            line.split_once(':').and_then(|(name, value)| {
                                name.eq_ignore_ascii_case("content-length")
                                    .then(|| value.trim().parse::<usize>().expect("content length"))
                            })
                        })
                        .expect("request content length");
                    expected_len = Some(body_start + content_len);
                }
            }
            if expected_len.is_some_and(|expected| request.len() >= expected) {
                return String::from_utf8(request).expect("UTF-8 request");
            }
        }
    }

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
    fn chunk_decoder_requires_complete_terminal_trailers() {
        for (wire, expected) in [
            (b"0\r\n".as_slice(), false),
            (b"0\r\n\r\n".as_slice(), true),
            (b"0\r\nX-Test: yes\r\n\r\n".as_slice(), true),
        ] {
            let mut decoder = ChunkedDecoder::default();
            decoder.feed(wire).expect("valid framing should feed");
            assert_eq!(decoder.is_finished(), expected);
        }
        for wire in [
            b"0\r\nX-Test".as_slice(),
            b"0\r\ninvalid\r\n\r\n".as_slice(),
        ] {
            let mut decoder = ChunkedDecoder::default();
            let result = decoder.feed(wire);
            assert!(result.is_err() || !decoder.is_finished());
        }
    }

    #[test]
    fn chunk_decoder_rejects_maximum_declared_size_without_panicking() {
        let header = format!("{:x}\r\n", usize::MAX);
        for suffix in [b"".as_slice(), b"x\r\n".as_slice()] {
            let mut wire = header.as_bytes().to_vec();
            wire.extend_from_slice(suffix);
            let mut decoder = ChunkedDecoder::default();
            assert!(matches!(
                decoder.feed(&wire),
                Err(LlamaCppRuntimeError::InvalidHttpResponse { .. })
            ));
        }
    }

    #[test]
    fn chunk_decoder_bounds_unterminated_size_and_trailer_lines() {
        let mut decoder = ChunkedDecoder::default();
        assert!(matches!(
            decoder.feed(&vec![b'f'; super::MAX_CHUNK_LINE + 1]),
            Err(LlamaCppRuntimeError::InvalidHttpResponse { .. })
        ));

        let mut decoder = ChunkedDecoder::default();
        decoder.feed(b"0\r\n").expect("terminal chunk should parse");
        assert!(matches!(
            decoder.feed(&vec![b'x'; super::MAX_CHUNK_TRAILER + 1]),
            Err(LlamaCppRuntimeError::InvalidHttpResponse { .. })
        ));
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
    fn streaming_chat_preserves_prompt_and_emits_complete_nonempty_text() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("listener should bind");
        let port = listener.local_addr().expect("listener address").port();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("request should connect");
            let mut request = Vec::new();
            let mut buffer = [0_u8; 1024];
            loop {
                let read = stream.read(&mut buffer).expect("request should read");
                request.extend_from_slice(&buffer[..read]);
                if request.windows(4).any(|window| window == b"\r\n\r\n")
                    && request
                        .windows(13)
                        .any(|window| window == b"\"stream\":true")
                {
                    break;
                }
            }
            let request_text = String::from_utf8(request).expect("request should be utf8");
            assert!(request_text.starts_with("POST /v1/chat/completions HTTP/1.1"));
            let body = request_text.split("\r\n\r\n").nth(1).expect("body");
            assert!(body.contains("\"stream\":true"));
            assert!(body.contains("prompt with \\n spaces"));
            let response = concat!(
                "HTTP/1.1 200 OK\r\n",
                "Content-Type: text/event-stream\r\n",
                "Connection: close\r\n\r\n",
                "data: {\"choices\":[{\"delta\":{\"content\":\"hello \"}}]}\n\n",
                "data: {\"choices\":[{\"delta\":{\"content\":\"world\"}}]}\n\n",
                "data: [DONE]\n\n"
            );
            stream
                .write_all(response.as_bytes())
                .expect("response should write");
        });
        let client = LlamaCppRuntime::attach(LlamaCppServerSpec::new(
            "llama-server",
            "model.gguf",
            "127.0.0.1",
            port,
            "default",
        ))
        .client();
        let mut deltas = Vec::new();
        let response = client
            .chat_streaming(
                &LlamaCppPrompt::new("prompt with \n spaces"),
                &LlamaCppChatCancellation::default(),
                |delta| deltas.push(delta.to_string()),
            )
            .expect("stream should succeed");
        server.join().expect("server should stop");
        assert_eq!(deltas, ["hello ", "world"]);
        assert_eq!(response.text, "hello world");
    }

    #[test]
    fn streaming_chat_handles_bytewise_json_and_utf8_splits_exactly_once() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("listener should bind");
        let port = listener.local_addr().expect("listener address").port();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("request should connect");
            let request = read_http_request(&mut stream);
            assert!(request.contains("\"stream\":true"));
            let response = concat!(
                "HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n",
                "data: {\"choices\":[{\"delta\":{\"content\":\"é\"}}]}\n\n",
                "data: [DONE]\n\n",
                "data: {\"choices\":[{\"delta\":{\"content\":\"late\"}}]}\n\n"
            );
            for byte in response.as_bytes() {
                if stream.write_all(&[*byte]).is_err() {
                    break;
                }
            }
            thread::sleep(Duration::from_millis(200));
        });
        let client = LlamaCppRuntime::attach(LlamaCppServerSpec::new(
            "llama-server",
            "model.gguf",
            "127.0.0.1",
            port,
            "default",
        ))
        .client();
        let mut deltas = Vec::new();
        let response = client
            .chat_streaming(
                &LlamaCppPrompt::new("hello"),
                &LlamaCppChatCancellation::default(),
                |delta| {
                    deltas.push(delta.to_string());
                },
            )
            .expect("bytewise stream should succeed");
        server.join().expect("server should stop");
        assert_eq!(deltas, ["é"]);
        assert_eq!(response.text, "é");
    }

    #[test]
    fn chunked_done_settles_before_open_socket_eof() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("listener should bind");
        let port = listener.local_addr().expect("listener address").port();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("request should connect");
            let _ = read_http_request(&mut stream);
            let data =
                b"data: {\"choices\":[{\"delta\":{\"content\":\"ok\"}}]}\n\ndata: [DONE]\n\n";
            let wire = format!("{:x}\r\n", data.len()).into_bytes();
            let mut response = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n".to_vec();
            response.extend_from_slice(&wire);
            response.extend_from_slice(data);
            response.extend_from_slice(b"\r\n0\r\nTrailer: ok\r\n\r\n");
            stream.write_all(&response).expect("response should write");
            thread::sleep(Duration::from_millis(200));
        });
        let client = LlamaCppRuntime::attach(LlamaCppServerSpec::new(
            "llama-server",
            "model.gguf",
            "127.0.0.1",
            port,
            "default",
        ))
        .client();
        let started = std::time::Instant::now();
        let result = client.chat_streaming(
            &LlamaCppPrompt::new("hello"),
            &LlamaCppChatCancellation::default(),
            |_| {},
        );
        assert_eq!(result.expect("complete chunked stream").text, "ok");
        assert!(started.elapsed() < Duration::from_millis(150));
        server.join().expect("server should stop");
    }

    #[test]
    fn streaming_chat_rejects_eof_before_done_and_incomplete_chunk_framing() {
        for body in [
            b"38\r\ndata: {\"choices\":[{\"delta\":{\"content\":\"x\"}}]}\n\n\r\n0\r\n\r\n"
                .to_vec(),
            b"4\r\ndata\r\n3\r\n: x".to_vec(),
        ] {
            let listener = TcpListener::bind(("127.0.0.1", 0)).expect("listener should bind");
            let port = listener.local_addr().expect("listener address").port();
            let server = thread::spawn(move || {
                let (mut stream, _) = listener.accept().expect("request should connect");
                let mut request = [0_u8; 4096];
                let _ = stream.read(&mut request);
                stream
                    .write_all(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n")
                    .expect("headers should write");
                stream.write_all(&body).expect("body should write");
            });
            let client = LlamaCppRuntime::attach(LlamaCppServerSpec::new(
                "llama-server",
                "model.gguf",
                "127.0.0.1",
                port,
                "default",
            ))
            .client();
            let result = client.chat_streaming(
                &LlamaCppPrompt::new("hello"),
                &LlamaCppChatCancellation::default(),
                |_| {},
            );
            server.join().expect("server should stop");
            assert!(result.is_err());
        }
    }

    #[test]
    fn streaming_chat_cancellation_returns_distinct_error_without_late_callback() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("listener should bind");
        let port = listener.local_addr().expect("listener address").port();
        let server = thread::spawn(move || {
            listener
                .set_nonblocking(true)
                .expect("listener should become nonblocking");
            let accept_deadline = std::time::Instant::now() + Duration::from_secs(1);
            let connection = loop {
                match listener.accept() {
                    Ok(connection) => break connection,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        if std::time::Instant::now() > accept_deadline {
                            return;
                        }
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(error) => panic!("request should connect: {error}"),
                }
            };
            let (mut stream, _) = connection;
            let mut request = [0_u8; 4096];
            let _ = stream.read(&mut request);
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\ndata: {\"choices\":[{\"delta\":{\"content\":\"first\"}}]}\n\ndata: {\"choices\":[{\"delta\":{\"content\":\"late\"}}]}\n\ndata: [DONE]\n\n")
                .expect("first event should write");
            thread::sleep(Duration::from_millis(200));
        });
        let client = LlamaCppRuntime::attach(LlamaCppServerSpec::new(
            "llama-server",
            "model.gguf",
            "127.0.0.1",
            port,
            "default",
        ))
        .client();
        let cancellation = LlamaCppChatCancellation::default();
        let mut deltas = Vec::new();
        let result = client.chat_streaming(&LlamaCppPrompt::new("hello"), &cancellation, |delta| {
            deltas.push(delta.to_string());
            cancellation.cancel();
        });
        server.join().expect("server should stop");
        assert!(matches!(result, Err(LlamaCppRuntimeError::ChatCancelled)));
        assert_eq!(deltas, ["first"]);
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

    #[test]
    fn streaming_external_api_key_rejects_http_header_control_bytes() {
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
        let client = LlamaCppRuntime::attach(spec).client();

        let result = client.chat_streaming(
            &LlamaCppPrompt::new("hello"),
            &LlamaCppChatCancellation::default(),
            |_| {},
        );

        assert!(matches!(
            result,
            Err(LlamaCppRuntimeError::HttpFailed { details })
                if details == "llama.cpp API key contains invalid HTTP header bytes"
        ));
    }

    #[test]
    fn streaming_non_200_error_preserves_bounded_body() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("listener should bind");
        let port = listener.local_addr().expect("listener address").port();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("request should connect");
            let _ = read_http_request(&mut stream);
            stream
                .write_all(
                    b"HTTP/1.1 400 Bad Request\r\nContent-Length: 62\r\nConnection: close\r\n\r\n",
                )
                .expect("error headers should write");
            stream.flush().expect("error headers should flush");
            thread::sleep(Duration::from_millis(10));
            stream
                .write_all(b"{\"error\":{\"message\":\"context window exceeded for this prompt\"}}")
                .expect("error body should write");
        });
        let client = LlamaCppRuntime::attach(LlamaCppServerSpec::new(
            "llama-server",
            "model.gguf",
            "127.0.0.1",
            port,
            "default",
        ))
        .client();

        let result = client.chat_streaming(
            &LlamaCppPrompt::new("hello"),
            &LlamaCppChatCancellation::default(),
            |_| {},
        );

        server.join().expect("server should stop");
        assert!(
            matches!(
                &result,
                Err(LlamaCppRuntimeError::HttpFailed { details })
                    if details.contains("status 400")
                        && details.contains("context window exceeded")
            ),
            "unexpected streaming error: {result:?}"
        );
    }

    #[test]
    fn streaming_chunked_error_classifies_phrase_split_across_chunks() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("listener should bind");
        let port = listener.local_addr().expect("listener address").port();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("request should connect");
            let _ = read_http_request(&mut stream);
            stream
                .write_all(b"HTTP/1.1 413 Payload Too Large\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n")
                .expect("error headers should write");
            stream
                .write_all(b"23\r\n{\"error\":{\"message\":\"context window")
                .expect("first error chunk should write");
            stream
                .write_all(b"\r\n")
                .expect("first chunk should terminate");
            stream
                .write_all(b"1c\r\n exceeded for this prompt\"}}\r\n0\r\n\r\n")
                .expect("second error chunk should write");
            stream.flush().expect("error response should flush");
        });
        let client = LlamaCppRuntime::attach(LlamaCppServerSpec::new(
            "llama-server",
            "model.gguf",
            "127.0.0.1",
            port,
            "default",
        ))
        .client();

        let result = client.chat_streaming(
            &LlamaCppPrompt::new("hello"),
            &LlamaCppChatCancellation::default(),
            |_| {},
        );

        server.join().expect("server should stop");
        assert!(
            matches!(
                &result,
                Err(LlamaCppRuntimeError::HttpFailed { details })
                    if details.contains("status 413")
                        && details.contains("context window exceeded")
            ),
            "unexpected streaming error: {result:?}"
        );
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
