use serde::{Deserialize, Serialize};
use std::fmt::{Display, Formatter};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

const STARTUP_TIMEOUT: Duration = Duration::from_secs(120);
const HTTP_TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LlamaCppServerSpec {
    executable_path: PathBuf,
    model_path: PathBuf,
    host: String,
    port: u16,
    alias: String,
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
        }
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
    StartupTimedOut { host: String, port: u16 },
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
            Self::StartupTimedOut { host, port } => {
                write!(
                    formatter,
                    "timed out waiting for llama.cpp server at http://{host}:{port}"
                )
            }
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
        let executable_parent = spec.executable_path().parent().ok_or_else(|| {
            LlamaCppRuntimeError::MissingExecutableParent {
                path: spec.executable_path().to_path_buf(),
            }
        })?;

        let child = Command::new(spec.executable_path())
            .current_dir(executable_parent)
            .args([
                "--host",
                spec.host(),
                "--port",
                &spec.port().to_string(),
                "-m",
                &spec.model_path().to_string_lossy(),
                "-a",
                spec.alias(),
                "-ngl",
                "all",
                "-c",
                "8192",
                "--no-webui",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| LlamaCppRuntimeError::SpawnFailed {
                details: error.to_string(),
            })?;

        let mut runtime = Self {
            spec,
            child: Some(child),
        };
        runtime.wait_until_ready()?;
        Ok(runtime)
    }

    pub fn attach(spec: LlamaCppServerSpec) -> Self {
        Self { spec, child: None }
    }

    pub fn chat(
        &mut self,
        prompt: &LlamaCppPrompt,
    ) -> Result<LlamaCppChatResponse, LlamaCppRuntimeError> {
        let request_body = serde_json::to_vec(&build_chat_completion_request(&self.spec, prompt))
            .map_err(|error| LlamaCppRuntimeError::InvalidResponsePayload {
            details: error.to_string(),
        })?;
        let response = send_http_request(
            self.spec.host(),
            self.spec.port(),
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

    fn wait_until_ready(&mut self) -> Result<(), LlamaCppRuntimeError> {
        let deadline = Instant::now() + STARTUP_TIMEOUT;

        loop {
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

            match send_http_request(self.spec.host(), self.spec.port(), "GET", "/health", None) {
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

impl Drop for LlamaCppRuntime {
    fn drop(&mut self) {
        if let Some(child) = self.child.as_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

#[derive(Debug)]
struct HttpResponse {
    status_code: u16,
    body: Vec<u8>,
}

fn send_http_request(
    host: &str,
    port: u16,
    method: &str,
    path: &str,
    body: Option<(&str, &[u8])>,
) -> Result<HttpResponse, LlamaCppRuntimeError> {
    let mut stream =
        TcpStream::connect((host, port)).map_err(|error| LlamaCppRuntimeError::HttpFailed {
            details: error.to_string(),
        })?;
    stream
        .set_read_timeout(Some(HTTP_TIMEOUT))
        .map_err(|error| LlamaCppRuntimeError::HttpFailed {
            details: error.to_string(),
        })?;
    stream
        .set_write_timeout(Some(HTTP_TIMEOUT))
        .map_err(|error| LlamaCppRuntimeError::HttpFailed {
            details: error.to_string(),
        })?;

    let mut request = format!(
        "{method} {path} HTTP/1.1\r\nHost: {host}:{port}\r\nConnection: close\r\nAccept: application/json\r\n",
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
    stream
        .read_to_end(&mut raw_response)
        .map_err(|error| LlamaCppRuntimeError::HttpFailed {
            details: error.to_string(),
        })?;

    parse_http_response(&raw_response)
}

fn parse_http_response(raw_response: &[u8]) -> Result<HttpResponse, LlamaCppRuntimeError> {
    let header_end = raw_response
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .ok_or_else(|| LlamaCppRuntimeError::InvalidHttpResponse {
            details: "missing header terminator".to_string(),
        })?;
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

        if remaining.len() < chunk_size + 2 {
            return Err(LlamaCppRuntimeError::InvalidHttpResponse {
                details: "chunk smaller than declared size".to_string(),
            });
        }

        decoded.extend_from_slice(&remaining[..chunk_size]);
        remaining = &remaining[(chunk_size + 2)..];
    }
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
        build_chat_completion_request, decode_chunked_body, parse_http_response, LlamaCppPrompt,
        LlamaCppServerSpec,
    };

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
}
