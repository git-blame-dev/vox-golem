use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
#[cfg(unix)]
use std::thread;
use std::time::Duration;

use crate::managed_process::{
    configure_owned_tokio, terminate_tokio, terminate_tokio_on_drop, ProcessOwnership,
};
use futures_util::StreamExt;
use reqwest::{StatusCode, Url};
use serde::Deserialize;
use tokio::process::{Child, Command as TokioCommand};

const MAX_JSON_RESPONSE_BYTES: usize = 64 * 1024;
const MAX_SSE_FRAME_BYTES: usize = 256 * 1024;
const MAX_SSE_OUTPUT_BYTES: usize = 4 * 1024 * 1024;
const MAX_TRACKED_PART_BYTES: usize = 4 * 1024 * 1024;
const MAX_TRACKED_PARTS: usize = 1024;
const MAX_TOOL_EVIDENCE_BYTES: usize = 64 * 1024;
const STARTUP_HEALTH_PROBE_TIMEOUT: Duration = Duration::from_secs(1);
#[cfg(unix)]
const ETXTBSY_RETRY_COUNT: usize = 20;
#[cfg(unix)]
const ETXTBSY_RETRY_DELAY: Duration = Duration::from_millis(10);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpencodePrompt {
    text: String,
    message_id: Option<String>,
}

impl OpencodePrompt {
    pub fn new(text: impl Into<String>) -> Result<Self, OpencodePromptError> {
        let text = text.into();

        if text.trim().is_empty() {
            return Err(OpencodePromptError::EmptyPrompt);
        }

        Ok(Self {
            text,
            message_id: None,
        })
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn with_message_id(mut self, message_id: impl Into<String>) -> Self {
        self.message_id = Some(message_id.into());
        self
    }

    pub fn message_id(&self) -> Option<&str> {
        self.message_id.as_deref()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpencodePromptError {
    EmptyPrompt,
}

/// The model configurations approved for prompts sent by the platform.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpencodeModel {
    Gpt56SolHigh,
    Gpt56LunaLow,
}

impl OpencodeModel {
    fn request_fields(self) -> serde_json::Value {
        let model = match self {
            Self::Gpt56SolHigh => "gpt-5.6-sol",
            Self::Gpt56LunaLow => "gpt-5.6-luna",
        };
        serde_json::json!({
            "providerID": "openai",
            "modelID": model,
        })
    }

    fn variant(self) -> &'static str {
        match self {
            Self::Gpt56SolHigh => "high",
            Self::Gpt56LunaLow => "low",
        }
    }
}

/// Tool permissions are deny-by-default. The wildcard is intentional: tools
/// added by a future OpenCode version remain unavailable until explicitly
/// allowed here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpencodeToolPolicy {
    AnswerOnly,
    Research,
}

impl OpencodeToolPolicy {
    fn request_tools(self) -> serde_json::Value {
        match self {
            Self::AnswerOnly => serde_json::json!({"*": false}),
            Self::Research => serde_json::json!({
                "*": false,
                "websearch": true,
                "webfetch": true,
                "shell": false,
                "file": false,
                "edit": false,
                "mutation": false,
            }),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OpencodePromptOptions {
    pub model: OpencodeModel,
    pub tool_policy: OpencodeToolPolicy,
}

impl OpencodePromptOptions {
    pub const fn new(model: OpencodeModel, tool_policy: OpencodeToolPolicy) -> Self {
        Self { model, tool_policy }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpencodeCommandSpec {
    executable_path: PathBuf,
    output_format: OpencodeOutputFormat,
    args: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpencodeRunResult {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: Option<i32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpencodeJsonRunResult {
    pub events: Vec<OpencodeJsonEvent>,
    pub stderr: String,
    pub exit_code: Option<i32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpencodeToolEvidence {
    pub session_id: String,
    pub message_id: Option<String>,
    pub tool: String,
    pub status: OpencodeToolUseStatus,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpencodeJsonEvent {
    Text {
        text: String,
    },
    Reasoning {
        text: String,
    },
    StepStart,
    StepFinish {
        reason: Option<String>,
    },
    Error {
        name: String,
        message: String,
    },
    ToolUse {
        tool: String,
        status: OpencodeToolUseStatus,
        detail: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpencodeToolUseStatus {
    Completed,
    Error,
}

#[derive(Debug)]
pub enum OpencodeJsonRunError {
    Io(std::io::Error),
    InvalidJsonLine { line_number: usize, details: String },
}

impl std::fmt::Display for OpencodeJsonRunError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "{error}"),
            Self::InvalidJsonLine {
                line_number,
                details,
            } => write!(
                formatter,
                "invalid OpenCode JSON event on line {line_number}: {details}"
            ),
        }
    }
}

impl std::error::Error for OpencodeJsonRunError {}

impl OpencodeRunResult {
    pub fn succeeded(&self) -> bool {
        self.exit_code == Some(0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpencodeOutputFormat {
    Default,
    Json,
}

impl OpencodeCommandSpec {
    pub fn new(executable_path: impl Into<PathBuf>, prompt: OpencodePrompt) -> Self {
        Self {
            executable_path: executable_path.into(),
            output_format: OpencodeOutputFormat::Default,
            args: vec![String::from("run"), prompt.text().to_string()],
        }
    }

    pub fn with_output_format(mut self, output_format: OpencodeOutputFormat) -> Self {
        self.output_format = output_format;
        self
    }

    pub fn executable_path(&self) -> &Path {
        &self.executable_path
    }

    pub fn args(&self) -> &[String] {
        &self.args
    }

    pub fn to_command(&self) -> Command {
        let mut command = Command::new(&self.executable_path);

        if self.output_format == OpencodeOutputFormat::Json {
            command.args(["run", "--format", "json"]);
            command.args(&self.args[1..]);
        } else {
            command.args(&self.args);
        }

        command
    }
}

pub fn run_opencode(spec: &OpencodeCommandSpec) -> std::io::Result<OpencodeRunResult> {
    let output = run_command(spec)?;

    Ok(OpencodeRunResult {
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        exit_code: output.status.code(),
    })
}

pub fn run_opencode_json(
    spec: &OpencodeCommandSpec,
) -> Result<OpencodeJsonRunResult, OpencodeJsonRunError> {
    let output = run_command(spec).map_err(OpencodeJsonRunError::Io)?;
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();

    Ok(OpencodeJsonRunResult {
        events: parse_json_events(&stdout)?,
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        exit_code: output.status.code(),
    })
}

fn run_command(spec: &OpencodeCommandSpec) -> std::io::Result<Output> {
    #[cfg(unix)]
    for attempt in 0..ETXTBSY_RETRY_COUNT {
        match spec.to_command().output() {
            Err(error) if is_executable_busy(&error) && attempt + 1 < ETXTBSY_RETRY_COUNT => {
                thread::sleep(ETXTBSY_RETRY_DELAY);
            }
            result => return result,
        }
    }

    #[cfg(not(unix))]
    return spec.to_command().output();

    #[cfg(unix)]
    unreachable!("the bounded command retry loop always returns")
}

#[cfg(unix)]
fn is_executable_busy(error: &std::io::Error) -> bool {
    error.raw_os_error() == Some(26)
}

fn parse_json_events(stdout: &str) -> Result<Vec<OpencodeJsonEvent>, OpencodeJsonRunError> {
    let mut events = Vec::new();

    for (line_index, line) in stdout.lines().enumerate() {
        let trimmed = line.trim();

        if trimmed.is_empty() {
            continue;
        }

        let raw_event = serde_json::from_str::<RawJsonEvent>(trimmed).map_err(|error| {
            OpencodeJsonRunError::InvalidJsonLine {
                line_number: line_index + 1,
                details: error.to_string(),
            }
        })?;

        match raw_event {
            RawJsonEvent::Text { part, .. } => {
                let text = part.text.trim();

                if !text.is_empty() {
                    events.push(OpencodeJsonEvent::Text {
                        text: text.to_string(),
                    });
                }
            }
            RawJsonEvent::Reasoning { part, .. } => {
                let text = part.text.trim();

                if !text.is_empty() {
                    events.push(OpencodeJsonEvent::Reasoning {
                        text: text.to_string(),
                    });
                }
            }
            RawJsonEvent::StepStart { .. } => {
                events.push(OpencodeJsonEvent::StepStart);
            }
            RawJsonEvent::StepFinish { part, .. } => {
                let reason = part.reason.and_then(|value| {
                    let trimmed = value.trim();

                    if trimmed.is_empty() {
                        None
                    } else {
                        Some(trimmed.to_string())
                    }
                });

                events.push(OpencodeJsonEvent::StepFinish { reason });
            }
            RawJsonEvent::Error { error, .. } => {
                let message = error
                    .data
                    .as_ref()
                    .and_then(|data| data.message.as_deref())
                    .unwrap_or(&error.name)
                    .to_string();

                events.push(OpencodeJsonEvent::Error {
                    name: error.name,
                    message,
                });
            }
            RawJsonEvent::ToolUse { part, .. } => match part.state {
                RawToolState::Completed { title, output } => {
                    let detail = if title.trim().is_empty() {
                        output.trim().to_string()
                    } else {
                        title
                    };

                    if !detail.is_empty() {
                        events.push(OpencodeJsonEvent::ToolUse {
                            tool: part.tool,
                            status: OpencodeToolUseStatus::Completed,
                            detail,
                        });
                    }
                }
                RawToolState::Error { error } => {
                    let detail = error.trim();

                    if !detail.is_empty() {
                        events.push(OpencodeJsonEvent::ToolUse {
                            tool: part.tool,
                            status: OpencodeToolUseStatus::Error,
                            detail: detail.to_string(),
                        });
                    }
                }
            },
            RawJsonEvent::Other => {}
        }
    }

    Ok(events)
}

/// Events emitted by the persistent server, intentionally independent of the wire schema.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpencodeEvent {
    Text(String),
    Reasoning(String),
    Status(String),
    Tool {
        name: String,
        status: OpencodeToolStatus,
        detail: String,
    },
    ToolEvidence(OpencodeToolEvidence),
    Error(String),
    Completed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpencodeToolStatus {
    Pending,
    Running,
    Completed,
    Error,
}

#[derive(Debug)]
pub enum OpencodeServerError {
    Io(std::io::Error),
    Http(reqwest::Error),
    Status(StatusCode),
    InvalidResponse(String),
    StartupTimeout,
    RequestTimeout,
    InvalidContentType,
    FrameTooLarge,
    InvalidUtf8,
    #[cfg(windows)]
    ProcessJobFailed {
        details: String,
    },
}

impl std::fmt::Display for OpencodeServerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "{e}"),
            Self::Http(e) => write!(f, "{e}"),
            Self::Status(s) => write!(f, "OpenCode HTTP status {s}"),
            Self::InvalidResponse(e) => write!(f, "{e}"),
            Self::StartupTimeout => write!(f, "OpenCode server startup timed out"),
            Self::RequestTimeout => write!(f, "OpenCode request timed out"),
            Self::InvalidContentType => write!(f, "OpenCode event stream is not text/event-stream"),
            Self::FrameTooLarge => write!(f, "OpenCode SSE frame exceeds the safety limit"),
            Self::InvalidUtf8 => write!(f, "OpenCode SSE frame is not valid UTF-8"),
            #[cfg(windows)]
            Self::ProcessJobFailed { details } => {
                write!(f, "failed to own OpenCode process tree: {details}")
            }
        }
    }
}
impl std::error::Error for OpencodeServerError {}
impl From<std::io::Error> for OpencodeServerError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}
impl From<reqwest::Error> for OpencodeServerError {
    fn from(e: reqwest::Error) -> Self {
        Self::Http(e)
    }
}

#[derive(Debug, Clone)]
pub struct OpencodeServerConfig {
    launch: OpencodeServerLaunch,
    pub startup_timeout: Duration,
    pub request_timeout: Duration,
}

#[derive(Debug, Clone)]
enum OpencodeServerLaunch {
    Native(PathBuf),
    Wsl(PathBuf),
}

impl OpencodeServerConfig {
    pub fn new(executable: impl Into<PathBuf>) -> Self {
        Self {
            launch: OpencodeServerLaunch::Native(executable.into()),
            startup_timeout: Duration::from_secs(30),
            request_timeout: Duration::from_secs(30),
        }
    }

    pub fn new_wsl(executable: impl Into<PathBuf>) -> Self {
        Self {
            launch: OpencodeServerLaunch::Wsl(executable.into()),
            startup_timeout: Duration::from_secs(30),
            request_timeout: Duration::from_secs(30),
        }
    }
}

pub struct OpencodeServer {
    client: reqwest::Client,
    process: Option<OwnedServerProcess>,
    base_url: String,
    session_id: String,
    password: String,
    request_timeout: Duration,
}

struct OwnedServerProcess {
    child: Option<Child>,
    wsl_owned: bool,
    #[cfg(windows)]
    process_job: Option<win32job::Job>,
}

#[derive(Clone)]
pub struct OpencodeClient {
    client: reqwest::Client,
    base_url: String,
    session_id: String,
    password: String,
    request_timeout: Duration,
}

impl OpencodeServer {
    pub async fn start(config: OpencodeServerConfig) -> Result<Self, OpencodeServerError> {
        let password = format!("voxgolem-{:032x}", rand::random::<u128>());
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0))?;
        let port = listener.local_addr()?.port();
        drop(listener);
        let wsl_owned = matches!(&config.launch, OpencodeServerLaunch::Wsl(_));
        let child = spawn_server_process(&config.launch, port, &password).await?;
        #[cfg(windows)]
        let mut process = match create_process_job(&child) {
            Ok(job) => OwnedServerProcess {
                child: Some(child),
                wsl_owned,
                process_job: Some(job),
            },
            Err(error) => {
                let mut process = OwnedServerProcess {
                    child: Some(child),
                    wsl_owned,
                    process_job: None,
                };
                process.terminate().await;
                return Err(error);
            }
        };
        #[cfg(not(windows))]
        let mut process = OwnedServerProcess {
            child: Some(child),
            wsl_owned,
        };
        let client = match reqwest::Client::builder()
            .connect_timeout(config.request_timeout)
            .build()
        {
            Ok(client) => client,
            Err(error) => {
                process.terminate().await;
                return Err(error.into());
            }
        };
        let base_url = format!("http://127.0.0.1:{port}");
        let deadline = tokio::time::Instant::now() + config.startup_timeout;
        loop {
            if tokio::time::Instant::now() >= deadline {
                process.terminate().await;
                return Err(OpencodeServerError::StartupTimeout);
            }
            let child_status = match process.child_mut().try_wait() {
                Ok(status) => status,
                Err(error) => {
                    process.terminate().await;
                    return Err(error.into());
                }
            };
            if let Some(status) = child_status {
                process.terminate().await;
                return Err(OpencodeServerError::InvalidResponse(format!(
                    "OpenCode exited during startup: {status}"
                )));
            }
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            let health_timeout = startup_health_probe_timeout(config.request_timeout, remaining);
            let health_request = client
                .get(format!("{base_url}/global/health"))
                .basic_auth("opencode", Some(&password))
                .send();
            let healthy = match tokio::time::timeout(health_timeout, health_request).await {
                Ok(Ok(response)) if response.status().is_success() => {
                    let body_timeout = config
                        .request_timeout
                        .min(deadline.saturating_duration_since(tokio::time::Instant::now()));
                    read_json_response(response, body_timeout)
                        .await
                        .map(|v| {
                            v.get("healthy").and_then(serde_json::Value::as_bool) == Some(true)
                        })
                        .unwrap_or(false)
                }
                _ => false,
            };
            if healthy {
                break;
            }
            tokio::time::sleep(
                Duration::from_millis(100)
                    .min(deadline.saturating_duration_since(tokio::time::Instant::now())),
            )
            .await;
        }
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            process.terminate().await;
            return Err(OpencodeServerError::StartupTimeout);
        }
        let session_id = match create_session_before_deadline(
            &client,
            &base_url,
            &password,
            config.request_timeout,
            deadline,
        )
        .await
        {
            Ok(session_id) => session_id,
            Err(error) => {
                process.terminate().await;
                return Err(error);
            }
        };
        Ok(Self {
            client,
            process: Some(process),
            base_url,
            session_id,
            password,
            request_timeout: config.request_timeout,
        })
    }
    pub fn client(&self) -> OpencodeClient {
        OpencodeClient {
            client: self.client.clone(),
            base_url: self.base_url.clone(),
            session_id: self.session_id.clone(),
            password: self.password.clone(),
            request_timeout: self.request_timeout,
        }
    }
    pub fn session_id(&self) -> &str {
        &self.session_id
    }
    pub async fn reset(&mut self) -> Result<(), OpencodeServerError> {
        self.reset_with_deadlines(
            tokio::time::Instant::now() + self.request_timeout,
            self.request_timeout,
        )
        .await
    }

    pub async fn reset_with_deadlines(
        &mut self,
        deadline: tokio::time::Instant,
        cleanup_timeout: Duration,
    ) -> Result<(), OpencodeServerError> {
        let old_client = self.client();
        let new_session_id = create_session_before_deadline(
            &self.client,
            &self.base_url,
            &self.password,
            self.request_timeout,
            deadline,
        )
        .await?;
        self.session_id = new_session_id;

        let abort_result = tokio::time::timeout(cleanup_timeout, old_client.abort()).await;
        let delete_result = tokio::time::timeout(cleanup_timeout, old_client.delete()).await;

        match abort_result {
            Ok(Ok(())) => delete_result
                .unwrap_or(Err(OpencodeServerError::RequestTimeout))
                .map(|_| ()),
            Ok(Err(error)) => Err(error),
            Err(_) => Err(OpencodeServerError::RequestTimeout),
        }
    }
    pub async fn shutdown(mut self) -> Result<(), OpencodeServerError> {
        let client = self.client();
        let _ = tokio::time::timeout(Duration::from_secs(3), async {
            let _ = client.abort().await;
            let _ = client.delete().await;
            let _ = client.dispose().await;
        })
        .await;
        if let Some(mut process) = self.process.take() {
            process.terminate().await;
        }
        Ok(())
    }
}

fn startup_health_probe_timeout(request_timeout: Duration, remaining: Duration) -> Duration {
    request_timeout
        .min(remaining)
        .min(STARTUP_HEALTH_PROBE_TIMEOUT)
}

fn server_command(launch: &OpencodeServerLaunch, port: u16, password: &str) -> TokioCommand {
    let port = port.to_string();
    let args = [
        "serve",
        "--pure",
        "--hostname",
        "127.0.0.1",
        "--port",
        &port,
    ];
    let mut command = match launch {
        OpencodeServerLaunch::Native(executable) => {
            let mut command = TokioCommand::new(executable);
            command.args(args).env("OPENCODE_SERVER_PASSWORD", password);
            command
        }
        OpencodeServerLaunch::Wsl(executable) => crate::wsl::WslRunner::default()
            .launch_opencode(executable, &args.map(str::to_owned))
            .to_tokio_command(Some(password)),
    };
    if matches!(launch, OpencodeServerLaunch::Wsl(_)) {
        command.stdin(std::process::Stdio::piped());
    } else {
        command.stdin(std::process::Stdio::null());
    }
    command
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    command.kill_on_drop(true);
    configure_owned_tokio(&mut command);
    command
}

async fn spawn_server_process(
    launch: &OpencodeServerLaunch,
    port: u16,
    password: &str,
) -> std::io::Result<Child> {
    #[cfg(unix)]
    for attempt in 0..ETXTBSY_RETRY_COUNT {
        match server_command(launch, port, password).spawn() {
            Err(error) if is_executable_busy(&error) && attempt + 1 < ETXTBSY_RETRY_COUNT => {
                tokio::time::sleep(ETXTBSY_RETRY_DELAY).await;
            }
            result => return result,
        }
    }

    #[cfg(not(unix))]
    return server_command(launch, port, password).spawn();

    #[cfg(unix)]
    unreachable!("the bounded server spawn retry loop always returns")
}

impl OpencodeClient {
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    pub async fn create_transient(&self) -> Result<Self, OpencodeServerError> {
        let session_id = create_session(
            &self.client,
            &self.base_url,
            &self.password,
            self.request_timeout,
        )
        .await?;
        Ok(Self {
            session_id,
            ..self.clone()
        })
    }
    pub async fn prompt(&self, prompt: &OpencodePrompt) -> Result<(), OpencodeServerError> {
        let mut body = prompt_body(prompt, None);
        if let Some(message_id) = prompt.message_id() {
            body["messageID"] = serde_json::Value::String(message_id.to_string());
        }
        self.send_prompt_body(body).await
    }

    pub async fn prompt_with_options(
        &self,
        prompt: &OpencodePrompt,
        options: OpencodePromptOptions,
    ) -> Result<(), OpencodeServerError> {
        let mut body = prompt_body(
            prompt,
            Some(serde_json::json!({
                "model": options.model.request_fields(),
                "variant": options.model.variant(),
                "tools": options.tool_policy.request_tools(),
            })),
        );
        if let Some(message_id) = prompt.message_id() {
            body["messageID"] = serde_json::Value::String(message_id.to_string());
        }
        self.send_prompt_body(body).await
    }

    async fn send_prompt_body(&self, body: serde_json::Value) -> Result<(), OpencodeServerError> {
        let response = send_with_timeout(
            self.client
                .post(format!(
                    "{}/session/{}/prompt_async",
                    self.base_url, self.session_id
                ))
                .basic_auth("opencode", Some(&self.password))
                .json(&body),
            self.request_timeout,
        )
        .await?;
        if response.status() == StatusCode::NO_CONTENT {
            Ok(())
        } else {
            Err(OpencodeServerError::Status(response.status()))
        }
    }
    pub async fn abort(&self) -> Result<(), OpencodeServerError> {
        let response = send_with_timeout(
            self.client
                .post(format!(
                    "{}/session/{}/abort",
                    self.base_url, self.session_id
                ))
                .basic_auth("opencode", Some(&self.password)),
            self.request_timeout,
        )
        .await?;
        expect_boolean_response(response, "abort session", false, self.request_timeout).await
    }
    pub async fn delete(&self) -> Result<(), OpencodeServerError> {
        let response = send_with_timeout(
            self.client
                .delete(format!("{}/session/{}", self.base_url, self.session_id))
                .basic_auth("opencode", Some(&self.password)),
            self.request_timeout,
        )
        .await?;
        expect_boolean_response(response, "delete session", true, self.request_timeout).await
    }
    async fn dispose(&self) -> Result<(), OpencodeServerError> {
        let response = send_with_timeout(
            self.client
                .post(format!("{}/instance/dispose", self.base_url))
                .basic_auth("opencode", Some(&self.password)),
            self.request_timeout,
        )
        .await?;
        expect_boolean_response(response, "dispose instance", true, self.request_timeout).await
    }
    pub async fn status(&self) -> Result<serde_json::Value, OpencodeServerError> {
        let response = send_with_timeout(
            self.client
                .get(format!("{}/session/status", self.base_url))
                .basic_auth("opencode", Some(&self.password)),
            self.request_timeout,
        )
        .await?;
        if !response.status().is_success() {
            return Err(OpencodeServerError::Status(response.status()));
        }
        read_json_response(response, self.request_timeout).await
    }
    pub async fn events(
        &self,
    ) -> Result<
        impl futures_util::Stream<Item = Result<OpencodeEvent, OpencodeServerError>>,
        OpencodeServerError,
    > {
        self.events_for_message_id(None).await
    }

    pub async fn events_for_message(
        &self,
        message_id: impl Into<String>,
    ) -> Result<
        impl futures_util::Stream<Item = Result<OpencodeEvent, OpencodeServerError>>,
        OpencodeServerError,
    > {
        self.events_for_message_id(Some(message_id.into())).await
    }

    async fn events_for_message_id(
        &self,
        message_id: Option<String>,
    ) -> Result<
        impl futures_util::Stream<Item = Result<OpencodeEvent, OpencodeServerError>>,
        OpencodeServerError,
    > {
        let response = send_with_timeout(
            self.client
                .get(format!("{}/event", self.base_url))
                .basic_auth("opencode", Some(&self.password)),
            self.request_timeout,
        )
        .await?;
        if !response.status().is_success() {
            return Err(OpencodeServerError::Status(response.status()));
        }
        if !response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.to_ascii_lowercase().starts_with("text/event-stream"))
        {
            return Err(OpencodeServerError::InvalidContentType);
        }
        let stream = futures_util::stream::unfold(
            (
                response.bytes_stream(),
                OpencodeSseDecoder::with_message(self.session_id.clone(), message_id),
            ),
            |(mut source, mut decoder)| async move {
                source.next().await.map(|chunk| {
                    let events = chunk
                        .map(|bytes| decoder.push(&bytes))
                        .unwrap_or_else(|e| vec![Err(OpencodeServerError::Http(e))]);
                    (events, (source, decoder))
                })
            },
        )
        .flat_map(futures_util::stream::iter);
        Ok(stream)
    }
}

fn prompt_body(prompt: &OpencodePrompt, options: Option<serde_json::Value>) -> serde_json::Value {
    let mut body = serde_json::json!({
        "parts": [{"type": "text", "text": prompt.text()}],
        "tools": {"*": false},
    });
    if let Some(options) = options {
        if let Some(object) = options.as_object() {
            for (key, value) in object {
                body[key] = value.clone();
            }
        }
    }
    body
}

impl OwnedServerProcess {
    fn child_mut(&mut self) -> &mut Child {
        self.child.as_mut().expect("owned server process")
    }

    async fn terminate(&mut self) {
        let wsl_owned = self.wsl_owned;
        if let Some(child) = self.child.as_mut() {
            terminate_child(child, wsl_owned).await;
        }
        self.child = None;
        #[cfg(windows)]
        {
            self.process_job = None;
        }
    }
}

impl Drop for OwnedServerProcess {
    fn drop(&mut self) {
        let Some(child) = self.child.take() else {
            return;
        };
        #[cfg(windows)]
        let process_guard = self.process_job.take();
        #[cfg(not(windows))]
        let process_guard = ();
        terminate_child_on_drop(child, self.wsl_owned, process_guard);
    }
}

impl Drop for OpencodeServer {
    fn drop(&mut self) {
        self.process = None;
    }
}

fn terminate_child_on_drop<G>(mut child: Child, wsl_owned: bool, process_guard: G)
where
    G: Send + 'static,
{
    if !wsl_owned {
        drop(process_guard);
        terminate_tokio_on_drop(child, ProcessOwnership::Owned);
        return;
    }
    drop(child.stdin.take());
    let _ = std::thread::Builder::new()
        .name(String::from("wsl-opencode-reaper"))
        .spawn(move || {
            let _process_guard = process_guard;
            let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            else {
                let _ = child.start_kill();
                return;
            };
            runtime.block_on(async move {
                if matches!(
                    tokio::time::timeout(Duration::from_secs(2), child.wait()).await,
                    Ok(Ok(_))
                ) {
                    return;
                }
                let _ =
                    terminate_tokio(&mut child, ProcessOwnership::Owned, Duration::from_secs(2))
                        .await;
            });
        });
}

async fn send_with_timeout(
    request: reqwest::RequestBuilder,
    timeout: Duration,
) -> Result<reqwest::Response, OpencodeServerError> {
    match tokio::time::timeout(timeout, request.send()).await {
        Ok(result) => result.map_err(OpencodeServerError::Http),
        Err(_) => Err(OpencodeServerError::RequestTimeout),
    }
}

async fn expect_boolean_response(
    response: reqwest::Response,
    action: &str,
    require_true: bool,
    request_timeout: Duration,
) -> Result<(), OpencodeServerError> {
    if !response.status().is_success() {
        return Err(OpencodeServerError::Status(response.status()));
    }
    let value = read_json_response(response, request_timeout)
        .await?
        .as_bool()
        .ok_or_else(|| {
            OpencodeServerError::InvalidResponse(format!(
                "OpenCode {action} response was not boolean"
            ))
        })?;
    if require_true && !value {
        return Err(OpencodeServerError::InvalidResponse(format!(
            "OpenCode could not {action}"
        )));
    }
    Ok(())
}

async fn create_session(
    client: &reqwest::Client,
    base_url: &str,
    password: &str,
    request_timeout: Duration,
) -> Result<String, OpencodeServerError> {
    let response = send_with_timeout(
        client
            .post(format!("{base_url}/session"))
            .basic_auth("opencode", Some(password))
            .json(&serde_json::json!({})),
        request_timeout,
    )
    .await?;
    if !response.status().is_success() {
        return Err(OpencodeServerError::Status(response.status()));
    }
    read_json_response(response, request_timeout)
        .await?
        .get("id")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| OpencodeServerError::InvalidResponse("session response has no id".into()))
}

async fn create_session_before_deadline(
    client: &reqwest::Client,
    base_url: &str,
    password: &str,
    request_timeout: Duration,
    deadline: tokio::time::Instant,
) -> Result<String, OpencodeServerError> {
    let send_timeout =
        request_timeout.min(deadline.saturating_duration_since(tokio::time::Instant::now()));
    if send_timeout.is_zero() {
        return Err(OpencodeServerError::StartupTimeout);
    }
    let response = send_with_timeout(
        client
            .post(format!("{base_url}/session"))
            .basic_auth("opencode", Some(password))
            .json(&serde_json::json!({})),
        send_timeout,
    )
    .await?;
    if !response.status().is_success() {
        return Err(OpencodeServerError::Status(response.status()));
    }
    let body_timeout =
        request_timeout.min(deadline.saturating_duration_since(tokio::time::Instant::now()));
    if body_timeout.is_zero() {
        return Err(OpencodeServerError::StartupTimeout);
    }
    read_json_response(response, body_timeout)
        .await?
        .get("id")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| OpencodeServerError::InvalidResponse("session response has no id".into()))
}

async fn read_json_response(
    response: reqwest::Response,
    request_timeout: Duration,
) -> Result<serde_json::Value, OpencodeServerError> {
    let bytes = tokio::time::timeout(request_timeout, async move {
        let mut stream = response.bytes_stream();
        let mut body = Vec::new();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            if body.len().saturating_add(chunk.len()) > MAX_JSON_RESPONSE_BYTES {
                return Err(OpencodeServerError::InvalidResponse(
                    "OpenCode JSON response exceeds the safety limit".into(),
                ));
            }
            body.extend_from_slice(&chunk);
        }
        Ok::<_, OpencodeServerError>(body)
    })
    .await
    .map_err(|_| OpencodeServerError::RequestTimeout)??;

    serde_json::from_slice(&bytes)
        .map_err(|error| OpencodeServerError::InvalidResponse(error.to_string()))
}

async fn terminate_child(child: &mut Child, wsl_owned: bool) {
    if wsl_owned {
        drop(child.stdin.take());
        if matches!(
            tokio::time::timeout(Duration::from_secs(2), child.wait()).await,
            Ok(Ok(_))
        ) {
            return;
        }
    }
    let _ = terminate_tokio(child, ProcessOwnership::Owned, Duration::from_secs(2)).await;
}

#[cfg(windows)]
fn create_process_job(child: &Child) -> Result<win32job::Job, OpencodeServerError> {
    let mut limits = win32job::ExtendedLimitInfo::new();
    limits.limit_kill_on_job_close();
    let job = win32job::Job::create_with_limit_info(&limits).map_err(|e| {
        OpencodeServerError::ProcessJobFailed {
            details: e.to_string(),
        }
    })?;
    let raw_handle = child
        .raw_handle()
        .ok_or_else(|| OpencodeServerError::ProcessJobFailed {
            details: String::from("OpenCode child process has no Windows handle"),
        })?;
    job.assign_process(raw_handle as isize)
        .map_err(|e| OpencodeServerError::ProcessJobFailed {
            details: e.to_string(),
        })?;
    Ok(job)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum StreamPartKind {
    Text,
    Reasoning,
}

struct StreamPart {
    kind: StreamPartKind,
    text: String,
}

fn parse_sse_event(
    frame: &str,
    active_session: Option<&str>,
    expected_user_message: Option<&str>,
    assistant_message: &mut Option<String>,
    request_started: &mut bool,
    stream_parts: &mut HashMap<String, StreamPart>,
) -> Result<Option<OpencodeEvent>, OpencodeServerError> {
    let data = frame
        .lines()
        .filter_map(|line| {
            line.strip_prefix("data:")
                .map(|data| data.strip_prefix(' ').unwrap_or(data))
        })
        .collect::<Vec<_>>()
        .join("\n");
    let value: serde_json::Value = serde_json::from_str(data.trim())
        .map_err(|e| OpencodeServerError::InvalidResponse(e.to_string()))?;
    let kind = value
        .get("type")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    let requires_session = matches!(
        kind,
        "message.part.updated"
            | "message.part.delta"
            | "message.updated"
            | "session.idle"
            | "session.status"
            | "session.error"
    );
    let props = value.get("properties").unwrap_or(&value);
    let part = props.get("part").unwrap_or(props);
    let session = part
        .get("sessionID")
        .and_then(serde_json::Value::as_str)
        .or_else(|| props.get("sessionID").and_then(serde_json::Value::as_str))
        .or_else(|| {
            props
                .get("info")
                .and_then(|info| info.get("sessionID"))
                .and_then(serde_json::Value::as_str)
        })
        .or_else(|| value.get("sessionID").and_then(serde_json::Value::as_str));
    if requires_session && active_session.is_some() && session.is_none() {
        return Ok(None);
    }
    if active_session.is_some_and(|active| session.is_some_and(|session| active != session)) {
        return Ok(None);
    }
    match kind {
        "message.part.updated" => {
            if expected_user_message.is_some() {
                let belongs_to_assistant = assistant_message.as_deref().is_some_and(|message_id| {
                    part.get("messageID").and_then(serde_json::Value::as_str) == Some(message_id)
                });
                if !belongs_to_assistant {
                    return Ok(None);
                }
            }
            match part.get("type").and_then(serde_json::Value::as_str) {
                Some("text") => {
                    map_stream_part_update(part, props, StreamPartKind::Text, stream_parts)
                }
                Some("reasoning") => {
                    map_stream_part_update(part, props, StreamPartKind::Reasoning, stream_parts)
                }
                Some("tool") => map_tool_event(part).map(Some),
                Some("step-start") => Ok(Some(OpencodeEvent::Status("step_start".into()))),
                Some("step-finish") => Ok(Some(OpencodeEvent::Status("step_finish".into()))),
                _ => Ok(None),
            }
        }
        "message.part.delta" => {
            if expected_user_message.is_some()
                && props.get("messageID").and_then(serde_json::Value::as_str)
                    != assistant_message.as_deref()
            {
                return Ok(None);
            }
            if props.get("field").and_then(serde_json::Value::as_str) != Some("text") {
                return Ok(None);
            }
            let Some(part_id) = props.get("partID").and_then(serde_json::Value::as_str) else {
                return Ok(None);
            };
            let Some(delta) = props.get("delta").and_then(serde_json::Value::as_str) else {
                return Ok(None);
            };
            let current = stream_parts.get(part_id).map_or(0, |part| part.text.len());
            if current.saturating_add(delta.len()) > MAX_TRACKED_PART_BYTES
                || tracked_part_bytes(stream_parts).saturating_add(delta.len())
                    > MAX_TRACKED_PART_BYTES
            {
                return Err(OpencodeServerError::FrameTooLarge);
            }
            let Some(stream_part) = stream_parts.get_mut(part_id) else {
                return Ok(None);
            };
            stream_part.text.push_str(delta);
            Ok(Some(match stream_part.kind {
                StreamPartKind::Text => OpencodeEvent::Text(delta.into()),
                StreamPartKind::Reasoning => OpencodeEvent::Reasoning(delta.into()),
            }))
        }
        "session.idle" => {
            if expected_user_message.is_some() && (!*request_started || assistant_message.is_none())
            {
                Ok(None)
            } else {
                Ok(Some(OpencodeEvent::Completed))
            }
        }
        "session.status" => match props
            .get("status")
            .and_then(|status| status.get("type"))
            .and_then(serde_json::Value::as_str)
        {
            Some("idle")
                if expected_user_message.is_some()
                    && (!*request_started || assistant_message.is_none()) =>
            {
                Ok(None)
            }
            Some("idle") => Ok(Some(OpencodeEvent::Completed)),
            Some("retry") => Ok(Some(OpencodeEvent::Status(
                props
                    .get("status")
                    .and_then(|status| status.get("message"))
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("retrying")
                    .into(),
            ))),
            Some(status) => Ok(Some(OpencodeEvent::Status(status.into()))),
            None => Ok(None),
        },
        "session.error" if expected_user_message.is_some() && !*request_started => Ok(None),
        "session.error" => Ok(Some(OpencodeEvent::Error(
            props
                .get("error")
                .and_then(|error| error.get("data"))
                .and_then(|data| data.get("message"))
                .and_then(serde_json::Value::as_str)
                .unwrap_or("OpenCode error")
                .into(),
        ))),
        "message.updated" => {
            let Some(info) = props.get("info") else {
                return Ok(None);
            };
            let mut assistant_completed = false;
            if let Some(expected_user_message) = expected_user_message {
                match info.get("role").and_then(serde_json::Value::as_str) {
                    Some("user")
                        if info.get("id").and_then(serde_json::Value::as_str)
                            == Some(expected_user_message) =>
                    {
                        *request_started = true;
                    }
                    Some("assistant")
                        if info.get("parentID").and_then(serde_json::Value::as_str)
                            == Some(expected_user_message) =>
                    {
                        *request_started = true;
                        let Some(message_id) = info.get("id").and_then(serde_json::Value::as_str)
                        else {
                            return Ok(None);
                        };
                        if assistant_message
                            .as_deref()
                            .is_some_and(|active_message| active_message != message_id)
                        {
                            return Ok(None);
                        }
                        if assistant_message.is_none() {
                            *assistant_message = Some(message_id.to_owned());
                        }
                        assistant_completed = info
                            .get("time")
                            .and_then(|time| time.get("completed"))
                            .and_then(serde_json::Value::as_u64)
                            .is_some();
                    }
                    _ => return Ok(None),
                }
            }
            let error_message = info
                .get("error")
                .and_then(|error| error.get("data"))
                .and_then(|data| data.get("message"))
                .and_then(serde_json::Value::as_str);
            if let Some(message) = error_message {
                Ok(Some(OpencodeEvent::Error(message.into())))
            } else if assistant_completed {
                Ok(Some(OpencodeEvent::Completed))
            } else {
                Ok(None)
            }
        }
        _ => Ok(None),
    }
}

fn map_stream_part_update(
    part: &serde_json::Value,
    properties: &serde_json::Value,
    kind: StreamPartKind,
    stream_parts: &mut HashMap<String, StreamPart>,
) -> Result<Option<OpencodeEvent>, OpencodeServerError> {
    if let Some(delta) = properties.get("delta").and_then(serde_json::Value::as_str) {
        if let Some(part_id) = part.get("id").and_then(serde_json::Value::as_str) {
            let current = stream_parts.get(part_id).map_or(0, |part| part.text.len());
            if stream_parts.get(part_id).is_none() && stream_parts.len() >= MAX_TRACKED_PARTS
                || current.saturating_add(delta.len()) > MAX_TRACKED_PART_BYTES
                || tracked_part_bytes(stream_parts).saturating_add(delta.len())
                    > MAX_TRACKED_PART_BYTES
            {
                return Err(OpencodeServerError::FrameTooLarge);
            }
            stream_parts
                .entry(part_id.to_string())
                .or_insert_with(|| StreamPart {
                    kind,
                    text: String::new(),
                })
                .text
                .push_str(delta);
        }
        return Ok(Some(match kind {
            StreamPartKind::Text => OpencodeEvent::Text(delta.into()),
            StreamPartKind::Reasoning => OpencodeEvent::Reasoning(delta.into()),
        }));
    }

    let Some(part_id) = part.get("id").and_then(serde_json::Value::as_str) else {
        return Ok(None);
    };
    let full_text = part
        .get("text")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    let delta = stream_parts
        .get(part_id)
        .filter(|tracked| tracked.kind == kind)
        .and_then(|tracked| full_text.strip_prefix(&tracked.text))
        .unwrap_or(full_text)
        .to_string();
    let previous_len = stream_parts.get(part_id).map_or(0, |part| part.text.len());
    if stream_parts.get(part_id).is_none() && stream_parts.len() >= MAX_TRACKED_PARTS
        || tracked_part_bytes(stream_parts)
            .saturating_sub(previous_len)
            .saturating_add(full_text.len())
            > MAX_TRACKED_PART_BYTES
    {
        return Err(OpencodeServerError::FrameTooLarge);
    }
    stream_parts.insert(
        part_id.to_string(),
        StreamPart {
            kind,
            text: full_text.to_string(),
        },
    );
    if delta.is_empty() {
        return Ok(None);
    }
    Ok(Some(match kind {
        StreamPartKind::Text => OpencodeEvent::Text(delta),
        StreamPartKind::Reasoning => OpencodeEvent::Reasoning(delta),
    }))
}

fn tracked_part_bytes(stream_parts: &HashMap<String, StreamPart>) -> usize {
    stream_parts
        .values()
        .fold(0, |total, part| total.saturating_add(part.text.len()))
}

fn map_tool_event(part: &serde_json::Value) -> Result<OpencodeEvent, OpencodeServerError> {
    let name = part
        .get("tool")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| OpencodeServerError::InvalidResponse("tool event has no name".into()))?;
    let state = part
        .get("state")
        .ok_or_else(|| OpencodeServerError::InvalidResponse("tool event has no state".into()))?;
    let status = state
        .get("status")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| OpencodeServerError::InvalidResponse("tool state has no status".into()))?;
    let (status, detail) = match status {
        "pending" => (OpencodeToolStatus::Pending, "pending"),
        "running" => (
            OpencodeToolStatus::Running,
            state
                .get("title")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("running"),
        ),
        "completed" => (
            OpencodeToolStatus::Completed,
            state
                .get("title")
                .and_then(serde_json::Value::as_str)
                .or_else(|| state.get("output").and_then(serde_json::Value::as_str))
                .unwrap_or("completed"),
        ),
        "error" => (
            OpencodeToolStatus::Error,
            state
                .get("error")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("tool failed"),
        ),
        status => {
            return Err(OpencodeServerError::InvalidResponse(format!(
                "unsupported tool status {status}"
            )));
        }
    };
    Ok(OpencodeEvent::Tool {
        name: name.into(),
        status,
        detail: detail.into(),
    })
}

/// Incremental SSE framing for arbitrary network chunk boundaries.
pub struct OpencodeSseDecoder {
    buffer: Vec<u8>,
    active_session: Option<String>,
    expected_user_message: Option<String>,
    assistant_message: Option<String>,
    request_started: bool,
    stream_parts: HashMap<String, StreamPart>,
    output_bytes: usize,
    poisoned: bool,
}
impl OpencodeSseDecoder {
    pub fn new(active_session: String) -> Self {
        Self::with_message(active_session, None)
    }

    pub fn for_message(active_session: String, message_id: String) -> Self {
        Self::with_message(active_session, Some(message_id))
    }

    fn with_message(active_session: String, expected_user_message: Option<String>) -> Self {
        Self {
            buffer: Vec::new(),
            active_session: Some(active_session),
            expected_user_message,
            assistant_message: None,
            request_started: false,
            stream_parts: HashMap::new(),
            output_bytes: 0,
            poisoned: false,
        }
    }
    pub fn push(&mut self, chunk: &[u8]) -> Vec<Result<OpencodeEvent, OpencodeServerError>> {
        if self.poisoned {
            return Vec::new();
        }
        self.buffer.extend_from_slice(chunk);
        let mut events = Vec::new();
        while let Some((end, delimiter_len)) = sse_frame_end(&self.buffer) {
            let frame = self.buffer.drain(..end + delimiter_len).collect::<Vec<_>>();
            if frame.len() > MAX_SSE_FRAME_BYTES {
                events.push(Err(OpencodeServerError::FrameTooLarge));
                continue;
            }
            if frame.contains(&b':') {
                match std::str::from_utf8(&frame) {
                    Ok(text) if text.lines().any(|line| line.starts_with("data:")) => {
                        match parse_sse_event(
                            text,
                            self.active_session.as_deref(),
                            self.expected_user_message.as_deref(),
                            &mut self.assistant_message,
                            &mut self.request_started,
                            &mut self.stream_parts,
                        ) {
                            Ok(Some(event)) => {
                                let mut evidence = Vec::new();
                                collect_tool_evidence(
                                    text,
                                    self.active_session.as_deref(),
                                    self.assistant_message.as_deref(),
                                    &mut evidence,
                                );
                                let event_bytes = match &event {
                                    OpencodeEvent::Text(text)
                                    | OpencodeEvent::Reasoning(text)
                                    | OpencodeEvent::Status(text)
                                    | OpencodeEvent::Error(text) => text.len(),
                                    OpencodeEvent::Tool { name, detail, .. } => {
                                        name.len().saturating_add(detail.len())
                                    }
                                    OpencodeEvent::ToolEvidence(evidence) => {
                                        evidence.tool.len().saturating_add(evidence.detail.len())
                                    }
                                    OpencodeEvent::Completed => 0,
                                };
                                if self.output_bytes.saturating_add(event_bytes)
                                    > MAX_SSE_OUTPUT_BYTES
                                {
                                    events.push(Err(OpencodeServerError::FrameTooLarge));
                                } else {
                                    self.output_bytes += event_bytes;
                                    events.push(Ok(event));
                                    if !evidence.is_empty() {
                                        let evidence_bytes =
                                            evidence.iter().fold(0_usize, |total, item| {
                                                total
                                                    .saturating_add(item.tool.len())
                                                    .saturating_add(item.detail.len())
                                            });
                                        if self.output_bytes.saturating_add(evidence_bytes)
                                            > MAX_SSE_OUTPUT_BYTES
                                        {
                                            events.push(Err(OpencodeServerError::FrameTooLarge));
                                        } else {
                                            self.output_bytes += evidence_bytes;
                                            events.extend(
                                                evidence
                                                    .into_iter()
                                                    .map(OpencodeEvent::ToolEvidence)
                                                    .map(Ok),
                                            );
                                        }
                                    }
                                }
                            }
                            Ok(None) => {}
                            Err(error) => {
                                if matches!(error, OpencodeServerError::FrameTooLarge) {
                                    self.poisoned = true;
                                    self.buffer.clear();
                                    self.stream_parts.clear();
                                }
                                events.push(Err(error));
                                if self.poisoned {
                                    break;
                                }
                            }
                        }
                    }
                    Err(_) => events.push(Err(OpencodeServerError::InvalidUtf8)),
                    _ => {}
                }
            }
        }
        if self.buffer.len() > MAX_SSE_FRAME_BYTES {
            self.buffer.clear();
            events.push(Err(OpencodeServerError::FrameTooLarge));
        }
        events
    }
}

fn collect_tool_evidence(
    frame: &str,
    active_session: Option<&str>,
    expected_message: Option<&str>,
    evidence: &mut Vec<OpencodeToolEvidence>,
) {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(
        &frame
            .lines()
            .filter_map(|line| line.strip_prefix("data:").map(|v| v.trim_start()))
            .collect::<Vec<_>>()
            .join("\n"),
    ) else {
        return;
    };
    if value.get("type").and_then(serde_json::Value::as_str) != Some("message.part.updated") {
        return;
    }
    let props = value.get("properties").unwrap_or(&value);
    let part = props.get("part").unwrap_or(props);
    if part.get("type").and_then(serde_json::Value::as_str) != Some("tool")
        || part.get("tool").and_then(serde_json::Value::as_str) != Some("webfetch")
    {
        return;
    }
    let session = part
        .get("sessionID")
        .and_then(serde_json::Value::as_str)
        .or_else(|| props.get("sessionID").and_then(serde_json::Value::as_str));
    let Some(session) = session else { return };
    if active_session.is_some_and(|expected| expected != session) {
        return;
    }
    let message = part
        .get("messageID")
        .and_then(serde_json::Value::as_str)
        .or_else(|| props.get("messageID").and_then(serde_json::Value::as_str));
    if expected_message.is_some_and(|expected| message != Some(expected)) {
        return;
    }
    let Some(state) = part.get("state") else {
        return;
    };
    if state.get("status").and_then(serde_json::Value::as_str) != Some("completed") {
        return;
    }
    let Some(input_url) = state
        .get("input")
        .and_then(|input| input.get("url"))
        .and_then(serde_json::Value::as_str)
    else {
        return;
    };
    let Ok(url) = Url::parse(input_url) else {
        return;
    };
    if !matches!(url.scheme(), "http" | "https")
        || input_url.len() > MAX_TOOL_EVIDENCE_BYTES
        || input_url.chars().any(|c| c.is_control())
    {
        return;
    }
    evidence.push(OpencodeToolEvidence {
        session_id: session.into(),
        message_id: message.map(str::to_owned),
        tool: "webfetch".into(),
        status: OpencodeToolUseStatus::Completed,
        detail: input_url.into(),
    });
}

fn sse_frame_end(buffer: &[u8]) -> Option<(usize, usize)> {
    let lf = buffer.windows(2).position(|window| window == b"\n\n");
    let crlf = buffer.windows(4).position(|window| window == b"\r\n\r\n");
    match (lf, crlf) {
        (Some(lf), Some(crlf)) if lf <= crlf => Some((lf, 2)),
        (Some(_), Some(crlf)) => Some((crlf, 4)),
        (Some(lf), None) => Some((lf, 2)),
        (None, Some(crlf)) => Some((crlf, 4)),
        (None, None) => None,
    }
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
enum RawJsonEvent {
    #[serde(rename = "text")]
    Text {
        part: RawTextPart,
        #[serde(rename = "timestamp")]
        _timestamp: u64,
        #[serde(rename = "sessionID")]
        _session_id: String,
    },
    #[serde(rename = "error")]
    Error {
        error: RawErrorPayload,
        #[serde(rename = "timestamp")]
        _timestamp: u64,
        #[serde(rename = "sessionID")]
        _session_id: String,
    },
    #[serde(rename = "reasoning")]
    Reasoning {
        part: RawTextPart,
        #[serde(rename = "timestamp")]
        _timestamp: u64,
        #[serde(rename = "sessionID")]
        _session_id: String,
    },
    #[serde(rename = "step_start")]
    StepStart {
        #[serde(rename = "timestamp")]
        _timestamp: u64,
        #[serde(rename = "sessionID")]
        _session_id: String,
    },
    #[serde(rename = "step_finish")]
    StepFinish {
        part: RawStepFinishPart,
        #[serde(rename = "timestamp")]
        _timestamp: u64,
        #[serde(rename = "sessionID")]
        _session_id: String,
    },
    #[serde(rename = "tool_use")]
    ToolUse {
        part: RawToolUsePart,
        #[serde(rename = "timestamp")]
        _timestamp: u64,
        #[serde(rename = "sessionID")]
        _session_id: String,
    },
    #[serde(other)]
    Other,
}

#[derive(Debug, Deserialize)]
struct RawTextPart {
    text: String,
}

#[derive(Debug, Deserialize)]
struct RawErrorPayload {
    name: String,
    data: Option<RawErrorData>,
}

#[derive(Debug, Deserialize)]
struct RawErrorData {
    message: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawToolUsePart {
    tool: String,
    state: RawToolState,
}

#[derive(Debug, Deserialize)]
struct RawStepFinishPart {
    reason: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "status")]
enum RawToolState {
    #[serde(rename = "completed")]
    Completed { title: String, output: String },
    #[serde(rename = "error")]
    Error { error: String },
}

#[cfg(test)]
mod tests {
    #[cfg(target_os = "linux")]
    use super::terminate_child_on_drop;
    use super::{
        prompt_body, run_opencode, run_opencode_json, server_command, startup_health_probe_timeout,
        OpencodeCommandSpec, OpencodeEvent, OpencodeJsonEvent, OpencodeJsonRunError, OpencodeModel,
        OpencodeOutputFormat, OpencodePrompt, OpencodePromptError, OpencodeServer,
        OpencodeServerConfig, OpencodeServerError, OpencodeSseDecoder, OpencodeToolPolicy,
        OpencodeToolStatus, OpencodeToolUseStatus, MAX_TRACKED_PART_BYTES,
    };
    use std::ffi::OsStr;
    use std::fs;
    use std::path::Path;
    use std::path::PathBuf;
    #[cfg(target_os = "linux")]
    use std::sync::atomic::AtomicBool;
    use std::sync::atomic::{AtomicU64, Ordering};
    #[cfg(target_os = "linux")]
    use std::sync::Arc;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    static TEMP_DIR_SEQUENCE: AtomicU64 = AtomicU64::new(0);

    #[cfg(target_os = "linux")]
    struct DropProbe(Arc<AtomicBool>);

    #[cfg(target_os = "linux")]
    impl Drop for DropProbe {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn drop_fallback_reaps_the_owned_child() {
        let mut command = tokio::process::Command::new("sh");
        command.args(["-c", "exec sleep 30"]).kill_on_drop(true);
        crate::managed_process::configure_owned_tokio(&mut command);
        let child = command.spawn().expect("test child should start");
        let pid = child.id().expect("test child should expose its pid");

        terminate_child_on_drop(child, false, ());

        tokio::time::timeout(Duration::from_secs(2), async {
            while Path::new(&format!("/proc/{pid}")).exists() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("drop fallback should reap the child");
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn wsl_drop_fallback_closes_the_owned_control_pipe() {
        let mut command = tokio::process::Command::new("sh");
        command
            .args(["-c", "read _ || true; sleep 0.2"])
            .stdin(std::process::Stdio::piped())
            .kill_on_drop(true);
        crate::managed_process::configure_owned_tokio(&mut command);
        let child = command.spawn().expect("test child should start");
        let pid = child.id().expect("test child should expose its pid");
        let guard_dropped = Arc::new(AtomicBool::new(false));

        terminate_child_on_drop(child, true, DropProbe(Arc::clone(&guard_dropped)));
        assert!(!guard_dropped.load(Ordering::SeqCst));

        tokio::time::timeout(Duration::from_secs(2), async {
            while Path::new(&format!("/proc/{pid}")).exists()
                || !guard_dropped.load(Ordering::SeqCst)
            {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("WSL drop fallback should close the control pipe and reap the child");
        assert!(guard_dropped.load(Ordering::SeqCst));
    }

    #[test]
    fn startup_health_probes_are_shorter_than_the_overall_deadline() {
        assert_eq!(
            startup_health_probe_timeout(Duration::from_secs(30), Duration::from_secs(20)),
            Duration::from_secs(1)
        );
        assert_eq!(
            startup_health_probe_timeout(Duration::from_secs(30), Duration::from_millis(250)),
            Duration::from_millis(250)
        );
        assert_eq!(
            OpencodeServerConfig::new("opencode").startup_timeout,
            Duration::from_secs(30)
        );
    }

    #[test]
    fn wsl_server_command_keeps_password_out_of_arguments() {
        let config = OpencodeServerConfig::new_wsl("/home/user/.opencode/bin/opencode");
        let command = server_command(&config.launch, 4096, "secret");
        let command = command.as_std();
        assert_eq!(command.get_program(), "wsl.exe");
        let args = command
            .get_args()
            .map(|argument| argument.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert_eq!(&args[..3], ["--exec", "sh", "-c"]);
        assert!(!args[3].is_empty());
        assert_eq!(args[4], "quiet");
        assert_eq!(
            &args[5..],
            [
                "/home/user/.opencode/bin/opencode",
                "serve",
                "--pure",
                "--hostname",
                "127.0.0.1",
                "--port",
                "4096",
            ]
        );
        assert!(!command
            .get_args()
            .any(|argument| argument == OsStr::new("secret")));
        assert_eq!(
            command
                .get_envs()
                .find(|(name, _)| *name == "OPENCODE_SERVER_PASSWORD")
                .and_then(|(_, value)| value),
            Some(OsStr::new("secret"))
        );
        assert!(command
            .get_envs()
            .find(|(name, _)| *name == "WSLENV")
            .and_then(|(_, value)| value)
            .is_some_and(|value| value
                .to_string_lossy()
                .split(':')
                .any(|entry| entry == "OPENCODE_SERVER_PASSWORD")));
    }

    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new() -> Self {
            let stamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system time should be after unix epoch")
                .as_nanos();
            let sequence = TEMP_DIR_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "voxgolem-opencode-tests-{}-{stamp}-{sequence}",
                std::process::id()
            ));

            fs::create_dir(&path).expect("temporary test directory should be creatable");

            Self { path }
        }

        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    #[test]
    fn rejects_blank_prompts() {
        assert_eq!(
            OpencodePrompt::new("   \n\t "),
            Err(OpencodePromptError::EmptyPrompt)
        );
    }

    #[test]
    fn preserves_non_empty_prompt_text() {
        let prompt = OpencodePrompt::new("summarize the latest transcript")
            .expect("non-empty prompt should be accepted");

        assert_eq!(prompt.text(), "summarize the latest transcript");
    }

    #[test]
    fn default_prompt_body_denies_all_tools() {
        let prompt = OpencodePrompt::new("answer briefly").expect("prompt should be valid");
        assert_eq!(
            prompt_body(&prompt, None)["tools"],
            serde_json::json!({"*": false})
        );
    }

    #[test]
    fn preserves_approved_model_identity_and_variant() {
        assert_eq!(
            OpencodeModel::Gpt56SolHigh.request_fields(),
            serde_json::json!({
                "providerID": "openai",
                "modelID": "gpt-5.6-sol",
            })
        );
        assert_eq!(OpencodeModel::Gpt56SolHigh.variant(), "high");
        assert_eq!(
            OpencodeModel::Gpt56LunaLow.request_fields(),
            serde_json::json!({
                "providerID": "openai",
                "modelID": "gpt-5.6-luna",
            })
        );
        assert_eq!(OpencodeModel::Gpt56LunaLow.variant(), "low");
    }

    #[test]
    fn builds_opencode_1184_prompt_body_with_top_level_variant_and_tools() {
        let prompt = OpencodePrompt::new("answer briefly").expect("prompt should be valid");
        let body = prompt_body(
            &prompt,
            Some(serde_json::json!({
                "model": OpencodeModel::Gpt56LunaLow.request_fields(),
                "variant": "low",
                "tools": OpencodeToolPolicy::Research.request_tools(),
            })),
        );

        assert_eq!(
            body,
            serde_json::json!({
                "parts": [{"type": "text", "text": "answer briefly"}],
                "model": {"providerID": "openai", "modelID": "gpt-5.6-luna"},
                "variant": "low",
                "tools": {
                    "*": false,
                    "websearch": true,
                    "webfetch": true,
                    "shell": false,
                    "file": false,
                    "edit": false,
                    "mutation": false,
                },
            })
        );
    }

    #[test]
    fn tool_policies_are_deny_by_default() {
        assert_eq!(
            OpencodeToolPolicy::AnswerOnly.request_tools(),
            serde_json::json!({"*": false})
        );
        assert_eq!(
            OpencodeToolPolicy::Research.request_tools(),
            serde_json::json!({
                "*": false,
                "websearch": true,
                "webfetch": true,
                "shell": false,
                "file": false,
                "edit": false,
                "mutation": false,
            })
        );
    }

    #[test]
    fn builds_run_command_spec() {
        let prompt = OpencodePrompt::new("open the release checklist")
            .expect("non-empty prompt should be accepted");
        let spec = OpencodeCommandSpec::new("C:/Program Files/OpenCode/opencode.exe", prompt);

        assert_eq!(
            spec.executable_path(),
            Path::new("C:/Program Files/OpenCode/opencode.exe")
        );
        assert_eq!(
            spec.args(),
            &[
                String::from("run"),
                String::from("open the release checklist")
            ]
        );
    }

    #[test]
    fn keeps_shell_like_characters_inside_single_argument() {
        let prompt = OpencodePrompt::new("say hello && remove nothing")
            .expect("shell-like prompt text should still be accepted");
        let spec = OpencodeCommandSpec::new("opencode.exe", prompt);
        let command = spec.to_command();

        assert_eq!(command.get_program(), OsStr::new("opencode.exe"));
        assert_eq!(
            command.get_args().collect::<Vec<_>>(),
            vec![OsStr::new("run"), OsStr::new("say hello && remove nothing")]
        );
    }

    #[test]
    fn can_request_json_output_for_programmatic_runs() {
        let prompt = OpencodePrompt::new("summarize the transcript")
            .expect("non-empty prompt should be accepted");
        let spec = OpencodeCommandSpec::new("opencode.exe", prompt)
            .with_output_format(OpencodeOutputFormat::Json);
        let command = spec.to_command();

        assert_eq!(
            command.get_args().collect::<Vec<_>>(),
            vec![
                OsStr::new("run"),
                OsStr::new("--format"),
                OsStr::new("json"),
                OsStr::new("summarize the transcript"),
            ]
        );
    }

    #[test]
    fn captures_stdout_stderr_and_exit_code_from_process() {
        let temp = TempDir::new();
        let executable = create_fake_opencode(temp.path(), successful_run_script());
        let prompt = OpencodePrompt::new("summarize the transcript")
            .expect("non-empty prompt should be accepted");
        let spec = OpencodeCommandSpec::new(executable, prompt);

        let result = run_opencode(&spec).expect("fake executable should run");

        assert_eq!(
            result.stdout,
            format!("stdout:run|summarize the transcript{}", platform_newline())
        );
        assert_eq!(result.stderr, format!("stderr:run{}", platform_newline()));
        assert_eq!(result.exit_code, Some(0));
        assert!(result.succeeded());
    }

    #[test]
    fn preserves_non_zero_exit_codes() {
        let temp = TempDir::new();
        let executable = create_fake_opencode(temp.path(), failing_run_script());
        let prompt = OpencodePrompt::new("summarize the transcript")
            .expect("non-empty prompt should be accepted");
        let spec = OpencodeCommandSpec::new(executable, prompt);

        let result = run_opencode(&spec).expect("fake executable should run");

        assert_eq!(result.stdout, "");
        assert_eq!(result.stderr.trim_end_matches(['\r', '\n']), "bad prompt");
        assert_eq!(result.exit_code, Some(7));
        assert!(!result.succeeded());
    }

    #[test]
    fn parses_minimal_json_events_and_ignores_other_event_types() {
        let temp = TempDir::new();
        let executable = create_fake_opencode(temp.path(), json_events_script());
        let prompt = OpencodePrompt::new("summarize the transcript")
            .expect("non-empty prompt should be accepted");
        let spec = OpencodeCommandSpec::new(executable, prompt)
            .with_output_format(OpencodeOutputFormat::Json);

        let result = run_opencode_json(&spec).expect("fake json executable should run");

        assert_eq!(
            result.events,
            vec![
                OpencodeJsonEvent::Text {
                    text: "Hello from OpenCode".to_string(),
                },
                OpencodeJsonEvent::Reasoning {
                    text: "Need to inspect the repo state first".to_string(),
                },
                OpencodeJsonEvent::StepStart,
                OpencodeJsonEvent::StepFinish {
                    reason: Some("stop".to_string()),
                },
                OpencodeJsonEvent::ToolUse {
                    tool: "bash".to_string(),
                    status: OpencodeToolUseStatus::Completed,
                    detail: "Shows working tree status".to_string(),
                },
                OpencodeJsonEvent::Error {
                    name: "APIError".to_string(),
                    message: "Provider failed".to_string(),
                },
            ]
        );
        assert_eq!(result.stderr, "");
        assert_eq!(result.exit_code, Some(0));
    }

    #[test]
    fn reports_invalid_json_lines() {
        let temp = TempDir::new();
        let executable = create_fake_opencode(temp.path(), invalid_json_script());
        let prompt = OpencodePrompt::new("summarize the transcript")
            .expect("non-empty prompt should be accepted");
        let spec = OpencodeCommandSpec::new(executable, prompt)
            .with_output_format(OpencodeOutputFormat::Json);

        let result = run_opencode_json(&spec);

        assert!(matches!(
            result,
            Err(OpencodeJsonRunError::InvalidJsonLine { line_number: 1, .. })
        ));
    }

    #[test]
    fn parses_tool_use_error_events() {
        let temp = TempDir::new();
        let executable = create_fake_opencode(temp.path(), tool_error_json_script());
        let prompt = OpencodePrompt::new("summarize the transcript")
            .expect("non-empty prompt should be accepted");
        let spec = OpencodeCommandSpec::new(executable, prompt)
            .with_output_format(OpencodeOutputFormat::Json);

        let result = run_opencode_json(&spec).expect("fake json executable should run");

        assert_eq!(
            result.events,
            vec![OpencodeJsonEvent::ToolUse {
                tool: "bash".to_string(),
                status: OpencodeToolUseStatus::Error,
                detail: "command failed".to_string(),
            }]
        );
    }

    #[test]
    fn parses_step_finish_without_reason() {
        let temp = TempDir::new();
        let executable = create_fake_opencode(temp.path(), step_finish_without_reason_script());
        let prompt = OpencodePrompt::new("summarize the transcript")
            .expect("non-empty prompt should be accepted");
        let spec = OpencodeCommandSpec::new(executable, prompt)
            .with_output_format(OpencodeOutputFormat::Json);

        let result = run_opencode_json(&spec).expect("fake json executable should run");

        assert_eq!(
            result.events,
            vec![OpencodeJsonEvent::StepFinish { reason: None }]
        );
    }

    #[test]
    fn incrementally_frames_sse_across_network_chunks() {
        let mut decoder = OpencodeSseDecoder::new("ses_1".into());
        assert!(decoder
            .push(b"data: {\"type\":\"message.part.updated\",\"properties\":{\"part\":{\"type\":\"text\",\"sessionID\":\"ses_1\"},\"delta\":\"hel")
            .is_empty());
        let events = decoder.push(b"lo\"}}\n\n");
        assert!(matches!(events.as_slice(), [Ok(OpencodeEvent::Text(text))] if text == "hello"));
    }

    #[test]
    fn decodes_multiple_crlf_frames_and_ignores_comments() {
        let mut decoder = OpencodeSseDecoder::new("ses_1".into());
        let events = decoder
            .push(
                b": connected\r\n\r\ndata:{\"type\":\"message.part.updated\",\"properties\":{\"part\":{\"type\":\"reasoning\",\"sessionID\":\"ses_1\"},\"delta\":\"think\"}}\r\n\r\ndata: {\"type\":\"session.idle\",\"properties\":{\"sessionID\":\"ses_1\"}}\r\n\r\n",
            )
            .into_iter()
            .collect::<Result<Vec<_>, _>>()
            .expect("valid frames should decode");
        assert_eq!(
            events,
            vec![
                OpencodeEvent::Reasoning("think".into()),
                OpencodeEvent::Completed,
            ]
        );
    }

    #[test]
    fn suppresses_events_for_other_sessions() {
        let mut decoder = OpencodeSseDecoder::new("ses_active".into());
        let events = decoder.push(
            b"data: {\"type\":\"message.part.updated\",\"properties\":{\"part\":{\"type\":\"text\",\"sessionID\":\"ses_other\"},\"delta\":\"wrong\"}}\n\n",
        );
        assert!(events.is_empty());
    }

    async fn fake_reset_server(abort_delay: Duration, delete_delay: Duration) -> String {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("fake server should bind");
        let address = listener
            .local_addr()
            .expect("fake server should expose its address");
        tokio::spawn(async move {
            for _ in 0..3 {
                let (mut stream, _) = listener.accept().await.expect("request should connect");
                let mut request = Vec::new();
                let mut chunk = [0; 1024];
                loop {
                    let count = stream
                        .read(&mut chunk)
                        .await
                        .expect("request should be readable");
                    if count == 0 {
                        break;
                    }
                    request.extend_from_slice(&chunk[..count]);
                    if request.windows(4).any(|window| window == b"\r\n\r\n") {
                        break;
                    }
                }
                let request = String::from_utf8_lossy(&request);
                let (delay, body) = if request.contains("POST /session/") {
                    (abort_delay, "true")
                } else if request.contains("DELETE /session/") {
                    (delete_delay, "true")
                } else {
                    (Duration::ZERO, "{\"id\":\"ses_new\"}")
                };
                tokio::time::sleep(delay).await;
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                stream
                    .write_all(response.as_bytes())
                    .await
                    .expect("response should be writable");
            }
        });
        format!("http://{address}")
    }

    #[tokio::test]
    async fn reset_installs_replacement_before_independently_bounded_cleanup() {
        let base_url =
            fake_reset_server(Duration::from_millis(100), Duration::from_millis(100)).await;
        let mut client = OpencodeServer {
            client: reqwest::Client::new(),
            base_url,
            session_id: "ses_old".into(),
            password: "password".into(),
            request_timeout: Duration::from_secs(1),
            process: None,
        };

        let result = client
            .reset_with_deadlines(
                tokio::time::Instant::now() + Duration::from_secs(1),
                Duration::from_millis(10),
            )
            .await;

        assert_eq!(client.session_id(), "ses_new");
        assert!(matches!(result, Err(OpencodeServerError::RequestTimeout)));
    }

    #[tokio::test]
    async fn reset_cancellation_after_installation_retains_replacement() {
        let base_url = fake_reset_server(Duration::from_millis(100), Duration::ZERO).await;
        let mut client = OpencodeServer {
            client: reqwest::Client::new(),
            base_url,
            session_id: "ses_old".into(),
            password: "password".into(),
            request_timeout: Duration::from_secs(1),
            process: None,
        };

        {
            let reset = client.reset_with_deadlines(
                tokio::time::Instant::now() + Duration::from_secs(1),
                Duration::from_secs(1),
            );
            tokio::pin!(reset);
            tokio::select! {
                _ = &mut reset => panic!("cleanup should still be stalled"),
                _ = tokio::time::sleep(Duration::from_millis(20)) => {}
            }
        }

        assert_eq!(client.session_id(), "ses_new");
    }

    #[test]
    fn exposes_only_scoped_successful_webfetch_output_as_evidence() {
        let mut decoder = OpencodeSseDecoder::for_message("ses_active".into(), "msg_user".into());
        let frames = decoder.push(b"data: {\"type\":\"message.updated\",\"properties\":{\"info\":{\"id\":\"msg_user\",\"role\":\"user\",\"sessionID\":\"ses_active\"}}}\n\ndata: {\"type\":\"message.updated\",\"properties\":{\"info\":{\"id\":\"msg_assistant\",\"role\":\"assistant\",\"parentID\":\"msg_user\",\"sessionID\":\"ses_active\"}}}\n\ndata: {\"type\":\"message.part.updated\",\"properties\":{\"part\":{\"type\":\"tool\",\"tool\":\"webfetch\",\"sessionID\":\"ses_active\",\"messageID\":\"msg_assistant\",\"state\":{\"status\":\"completed\",\"title\":\"Fetched\",\"input\":{\"url\":\"https://example.test/page\"},\"output\":\"The page says the service is healthy.\"}}}}\n\n");
        assert!(frames.iter().all(Result::is_ok));
        assert!(frames.iter().any(
            |event| matches!(event, Ok(OpencodeEvent::Tool { name, .. }) if name == "webfetch")
        ));
        assert!(frames.iter().any(|event| matches!(event, Ok(OpencodeEvent::ToolEvidence(evidence)) if evidence.detail == "https://example.test/page")));

        let mut decoder = OpencodeSseDecoder::new("ses_active".into());
        let rejected = decoder.push(b"data: {\"type\":\"message.part.updated\",\"properties\":{\"part\":{\"type\":\"tool\",\"tool\":\"websearch\",\"sessionID\":\"ses_active\",\"state\":{\"status\":\"completed\",\"output\":\"https://example.test/search\"}}}}\n\ndata: {\"type\":\"message.part.updated\",\"properties\":{\"part\":{\"type\":\"tool\",\"tool\":\"webfetch\",\"sessionID\":\"ses_other\",\"state\":{\"status\":\"completed\",\"input\":{\"url\":\"https://example.test/other\"},\"output\":\"other session\"}}}}\n\ndata: {\"type\":\"message.part.updated\",\"properties\":{\"part\":{\"type\":\"tool\",\"tool\":\"webfetch\",\"sessionID\":\"ses_active\",\"state\":{\"status\":\"error\",\"input\":{\"url\":\"https://example.test/error\"},\"output\":\"error\"}}}}\n\ndata: {\"type\":\"message.part.updated\",\"properties\":{\"part\":{\"type\":\"tool\",\"tool\":\"webfetch\",\"sessionID\":\"ses_active\",\"state\":{\"status\":\"completed\",\"input\":{\"url\":\"not a URL\"},\"output\":\"https://user@example.test/private\"}}}}\n\ndata: {\"type\":\"message.part.updated\",\"properties\":{\"part\":{\"type\":\"tool\",\"tool\":\"webfetch\",\"sessionID\":\"ses_active\",\"state\":{\"status\":\"completed\",\"output\":\"https://output-only.example/page\"}}}}\n\n");
        assert!(rejected.iter().all(Result::is_ok));
    }

    #[test]
    fn rejects_unscoped_session_and_message_events() {
        let mut decoder = OpencodeSseDecoder::for_message("ses_active".into(), "msg_user".into());
        assert!(decoder
            .push(b"data: {\"type\":\"session.idle\",\"properties\":{}}\n\n")
            .is_empty());
        assert!(decoder
            .push(b"data: {\"type\":\"message.updated\",\"properties\":{\"info\":{\"id\":\"msg_user\",\"role\":\"user\"}}}\n\n")
            .is_empty());

        let events = decoder
            .push(
                b"data: {\"type\":\"message.updated\",\"sessionID\":\"ses_active\",\"properties\":{\"info\":{\"id\":\"msg_user\",\"role\":\"user\"}}}\n\ndata: {\"type\":\"message.updated\",\"sessionID\":\"ses_active\",\"properties\":{\"info\":{\"id\":\"msg_assistant\",\"role\":\"assistant\",\"parentID\":\"msg_user\"}}}\n\ndata: {\"type\":\"session.idle\",\"sessionID\":\"ses_active\",\"properties\":{}}\n\n",
            )
            .into_iter()
            .collect::<Result<Vec<_>, _>>()
            .expect("scoped frames should decode");
        assert_eq!(events, vec![OpencodeEvent::Completed]);
    }

    #[test]
    fn correlates_parts_and_completion_to_expected_message() {
        let mut decoder = OpencodeSseDecoder::for_message("ses_1".into(), "msg_user_1".into());

        assert!(decoder
            .push(
                b"data: {\"type\":\"session.idle\",\"properties\":{\"sessionID\":\"ses_1\"}}\n\n",
            )
            .is_empty());
        assert!(decoder
            .push(
                b"data: {\"type\":\"message.updated\",\"properties\":{\"info\":{\"id\":\"msg_other\",\"sessionID\":\"ses_1\",\"role\":\"assistant\",\"parentID\":\"msg_old\",\"error\":{\"data\":{\"message\":\"stale\"}}}}}\n\n",
            )
            .is_empty());
        assert!(decoder
            .push(
                b"data: {\"type\":\"message.updated\",\"properties\":{\"info\":{\"id\":\"msg_user_1\",\"sessionID\":\"ses_1\",\"role\":\"user\"}}}\n\ndata: {\"type\":\"message.updated\",\"properties\":{\"info\":{\"id\":\"msg_assistant_1\",\"sessionID\":\"ses_1\",\"role\":\"assistant\",\"parentID\":\"msg_user_1\"}}}\n\n",
            )
            .is_empty());

        let events = decoder
            .push(
                b"data: {\"type\":\"message.part.updated\",\"properties\":{\"sessionID\":\"ses_1\",\"part\":{\"id\":\"part_1\",\"type\":\"text\",\"text\":\"\",\"sessionID\":\"ses_1\",\"messageID\":\"msg_assistant_1\"}}}\n\ndata: {\"type\":\"message.part.delta\",\"properties\":{\"sessionID\":\"ses_1\",\"messageID\":\"msg_assistant_1\",\"partID\":\"part_1\",\"field\":\"text\",\"delta\":\"answer\"}}\n\ndata: {\"type\":\"session.idle\",\"properties\":{\"sessionID\":\"ses_1\"}}\n\n",
            )
            .into_iter()
            .collect::<Result<Vec<_>, _>>()
            .expect("correlated frames should decode");

        assert_eq!(
            events,
            vec![
                OpencodeEvent::Text("answer".into()),
                OpencodeEvent::Completed,
            ]
        );
    }

    #[test]
    fn completes_when_the_matching_assistant_message_finishes() {
        let mut decoder = OpencodeSseDecoder::for_message("ses_1".into(), "msg_user_1".into());
        let events = decoder
            .push(
                b"data: {\"type\":\"message.updated\",\"properties\":{\"sessionID\":\"ses_1\",\"info\":{\"id\":\"msg_assistant_1\",\"parentID\":\"msg_user_1\",\"role\":\"assistant\",\"sessionID\":\"ses_1\",\"time\":{\"created\":1}}}}\n\ndata: {\"type\":\"message.part.updated\",\"properties\":{\"sessionID\":\"ses_1\",\"part\":{\"id\":\"part_1\",\"type\":\"text\",\"text\":\"\",\"sessionID\":\"ses_1\",\"messageID\":\"msg_assistant_1\"}}}\n\ndata: {\"type\":\"message.part.delta\",\"properties\":{\"sessionID\":\"ses_1\",\"messageID\":\"msg_assistant_1\",\"partID\":\"part_1\",\"field\":\"text\",\"delta\":\"OK\"}}\n\ndata: {\"type\":\"message.updated\",\"properties\":{\"sessionID\":\"ses_1\",\"info\":{\"id\":\"msg_assistant_1\",\"parentID\":\"msg_user_1\",\"role\":\"assistant\",\"sessionID\":\"ses_1\",\"time\":{\"created\":1,\"completed\":2},\"finish\":\"stop\"}}}\n\n",
            )
            .into_iter()
            .collect::<Result<Vec<_>, _>>()
            .expect("real OpenCode completion frames should decode");

        assert_eq!(
            events,
            vec![OpencodeEvent::Text("OK".into()), OpencodeEvent::Completed]
        );
    }

    #[test]
    fn ignores_later_assistant_siblings_for_the_same_user_message() {
        let mut decoder = OpencodeSseDecoder::for_message("ses_1".into(), "msg_user_1".into());
        assert!(decoder
            .push(
                b"data: {\"type\":\"message.updated\",\"properties\":{\"sessionID\":\"ses_1\",\"info\":{\"id\":\"msg_assistant_1\",\"parentID\":\"msg_user_1\",\"role\":\"assistant\",\"sessionID\":\"ses_1\",\"time\":{\"created\":1}}}}\n\n",
            )
            .is_empty());
        assert!(decoder
            .push(
                b"data: {\"type\":\"message.updated\",\"properties\":{\"sessionID\":\"ses_1\",\"info\":{\"id\":\"msg_assistant_2\",\"parentID\":\"msg_user_1\",\"role\":\"assistant\",\"sessionID\":\"ses_1\",\"time\":{\"created\":2,\"completed\":3},\"error\":{\"data\":{\"message\":\"wrong sibling\"}}}}}\n\ndata: {\"type\":\"message.part.updated\",\"properties\":{\"sessionID\":\"ses_1\",\"part\":{\"id\":\"part_2\",\"type\":\"text\",\"text\":\"wrong sibling\",\"sessionID\":\"ses_1\",\"messageID\":\"msg_assistant_2\"}}}\n\n",
            )
            .is_empty());

        let events = decoder
            .push(
                b"data: {\"type\":\"message.updated\",\"properties\":{\"sessionID\":\"ses_1\",\"info\":{\"id\":\"msg_assistant_1\",\"parentID\":\"msg_user_1\",\"role\":\"assistant\",\"sessionID\":\"ses_1\",\"time\":{\"created\":1,\"completed\":4},\"finish\":\"stop\"}}}\n\n",
            )
            .into_iter()
            .collect::<Result<Vec<_>, _>>()
            .expect("matching assistant completion should decode");

        assert_eq!(events, vec![OpencodeEvent::Completed]);
    }

    #[test]
    fn filters_message_updated_errors_by_nested_session() {
        let mut decoder = OpencodeSseDecoder::new("ses_active".into());
        let events = decoder.push(
            b"data: {\"type\":\"message.updated\",\"properties\":{\"info\":{\"id\":\"msg_1\",\"sessionID\":\"ses_other\",\"role\":\"assistant\",\"error\":{\"data\":{\"message\":\"wrong session\"}}}}}\n\n",
        );
        assert!(events.is_empty());
    }

    #[test]
    fn maps_tool_updates_and_nested_session_errors() {
        let mut decoder = OpencodeSseDecoder::new("ses_1".into());
        let events = decoder
            .push(
                b"data: {\"type\":\"message.part.updated\",\"properties\":{\"part\":{\"type\":\"tool\",\"sessionID\":\"ses_1\",\"tool\":\"bash\",\"state\":{\"status\":\"completed\",\"title\":\"Checked status\",\"output\":\"clean\"}}}}\n\ndata: {\"type\":\"session.error\",\"properties\":{\"sessionID\":\"ses_1\",\"error\":{\"name\":\"APIError\",\"data\":{\"message\":\"provider failed\"}}}}\n\n",
            )
            .into_iter()
            .collect::<Result<Vec<_>, _>>()
            .expect("valid frames should decode");
        assert_eq!(
            events,
            vec![
                OpencodeEvent::Tool {
                    name: "bash".into(),
                    status: OpencodeToolStatus::Completed,
                    detail: "Checked status".into(),
                },
                OpencodeEvent::Error("provider failed".into()),
            ]
        );
    }

    #[test]
    fn maps_idle_status_to_completion_and_retry_to_status() {
        let mut decoder = OpencodeSseDecoder::new("ses_1".into());
        let events = decoder
            .push(
                b"data: {\"type\":\"session.status\",\"properties\":{\"sessionID\":\"ses_1\",\"status\":{\"type\":\"retry\",\"message\":\"rate limited\"}}}\n\ndata: {\"type\":\"session.status\",\"properties\":{\"sessionID\":\"ses_1\",\"status\":{\"type\":\"idle\"}}}\n\n",
            )
            .into_iter()
            .collect::<Result<Vec<_>, _>>()
            .expect("valid frames should decode");
        assert_eq!(
            events,
            vec![
                OpencodeEvent::Status("rate limited".into()),
                OpencodeEvent::Completed,
            ]
        );
    }

    #[test]
    fn reports_invalid_utf8_and_recovers_after_oversized_partial_frame() {
        let mut decoder = OpencodeSseDecoder::new("ses_1".into());
        assert!(matches!(
            decoder.push(b"data: \xff\n\n").as_slice(),
            [Err(OpencodeServerError::InvalidUtf8)]
        ));
        assert!(matches!(
            decoder.push(&vec![b'x'; 256 * 1024 + 1]).as_slice(),
            [Err(OpencodeServerError::FrameTooLarge)]
        ));
        assert!(decoder.push(b": recovered\n\n").is_empty());
    }

    #[test]
    fn reports_malformed_json_and_frame_limit() {
        let mut decoder = OpencodeSseDecoder::new("ses_1".into());
        assert!(matches!(
            decoder.push(b"data: nope\n\n").as_slice(),
            [Err(OpencodeServerError::InvalidResponse(_))]
        ));
        assert!(matches!(
            decoder.push(&vec![b'x'; 256 * 1024 + 1]).as_slice(),
            [Err(OpencodeServerError::FrameTooLarge)]
        ));
    }

    #[test]
    fn bounds_aggregate_sse_output() {
        let mut decoder = OpencodeSseDecoder::new("ses_1".into());
        let delta = "x".repeat(64 * 1024);
        for _ in 0..64 {
            let frame = format!(
                "data: {{\"type\":\"message.part.updated\",\"properties\":{{\"part\":{{\"type\":\"text\",\"sessionID\":\"ses_1\"}},\"delta\":\"{delta}\"}}}}\n\n"
            );
            assert!(decoder.push(frame.as_bytes()).iter().all(Result::is_ok));
        }
        let frame = format!(
            "data: {{\"type\":\"message.part.updated\",\"properties\":{{\"part\":{{\"type\":\"text\",\"sessionID\":\"ses_1\"}},\"delta\":\"{delta}\"}}}}\n\n"
        );
        assert!(matches!(
            decoder.push(frame.as_bytes()).as_slice(),
            [Err(OpencodeServerError::FrameTooLarge)]
        ));
    }

    #[test]
    fn bounds_tracked_part_state() {
        let mut decoder = OpencodeSseDecoder::new("ses_1".into());
        let delta = "x".repeat(64 * 1024);
        for _ in 0..64 {
            let frame = format!(
                "data: {{\"type\":\"message.part.updated\",\"properties\":{{\"part\":{{\"id\":\"part_1\",\"type\":\"text\",\"sessionID\":\"ses_1\"}},\"delta\":\"{delta}\"}}}}\n\n"
            );
            assert!(decoder.push(frame.as_bytes()).iter().all(Result::is_ok));
        }
        let frame = format!(
            "data: {{\"type\":\"message.part.updated\",\"properties\":{{\"part\":{{\"id\":\"part_1\",\"type\":\"text\",\"sessionID\":\"ses_1\"}},\"delta\":\"{delta}\"}}}}\n\n"
        );
        assert!(matches!(
            decoder.push(frame.as_bytes()).as_slice(),
            [Err(OpencodeServerError::FrameTooLarge)]
        ));
    }

    #[test]
    fn rejects_repeated_deltas_before_mutating_tracked_state() {
        let mut decoder = OpencodeSseDecoder::new("ses_1".into());
        let delta = "x".repeat(4096);
        let frame = |delta: &str| {
            format!(
                "data: {{\"type\":\"message.part.updated\",\"properties\":{{\"part\":{{\"id\":\"part_1\",\"type\":\"text\",\"sessionID\":\"ses_1\"}},\"delta\":\"{delta}\"}}}}\n\n"
            )
        };
        for _ in 0..(MAX_TRACKED_PART_BYTES / delta.len()) {
            assert!(decoder
                .push(frame(&delta).as_bytes())
                .iter()
                .all(Result::is_ok));
        }
        assert!(matches!(
            decoder.push(frame(&delta).as_bytes()).as_slice(),
            [Err(OpencodeServerError::FrameTooLarge)]
        ));
        assert!(decoder.push(frame("safe").as_bytes()).is_empty());
    }

    fn create_fake_opencode(directory: &Path, body: &str) -> PathBuf {
        #[cfg(windows)]
        let executable = directory.join("fake-opencode.cmd");
        #[cfg(not(windows))]
        let executable = directory.join("fake-opencode.sh");
        let staged_executable = executable.with_extension("staged");

        #[cfg(windows)]
        let script = format!("@echo off\r\n{body}\r\n");
        #[cfg(not(windows))]
        let script = format!("#!/bin/sh\n{body}\n");

        {
            use std::io::Write;

            let mut file =
                fs::File::create(&staged_executable).expect("fake executable should be created");
            file.write_all(script.as_bytes())
                .expect("fake executable should be written");
            file.sync_all()
                .expect("fake executable should be flushed before execution");
        }

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            let permissions = fs::Permissions::from_mode(0o755);
            fs::set_permissions(&staged_executable, permissions)
                .expect("fake executable should be marked executable");
        }

        fs::rename(&staged_executable, &executable)
            .expect("closed fake executable should be installed atomically");

        executable
    }

    #[cfg(windows)]
    fn platform_newline() -> &'static str {
        "\r\n"
    }

    #[cfg(not(windows))]
    fn platform_newline() -> &'static str {
        "\n"
    }

    #[cfg(windows)]
    fn successful_run_script() -> &'static str {
        "echo stdout:%1^|%~2\n1>&2 echo stderr:%1"
    }

    #[cfg(not(windows))]
    fn successful_run_script() -> &'static str {
        "printf 'stdout:%s|%s\\n' \"$1\" \"$2\"; printf 'stderr:%s\\n' \"$1\" 1>&2"
    }

    #[cfg(windows)]
    fn failing_run_script() -> &'static str {
        "1>&2 echo bad prompt\nexit /b 7"
    }

    #[cfg(not(windows))]
    fn failing_run_script() -> &'static str {
        "printf 'bad prompt' 1>&2; exit 7"
    }

    #[cfg(windows)]
    fn json_events_script() -> &'static str {
        "echo {\"type\":\"text\",\"timestamp\":1,\"sessionID\":\"ses_1\",\"part\":{\"text\":\"Hello from OpenCode\"}}\necho {\"type\":\"reasoning\",\"timestamp\":2,\"sessionID\":\"ses_1\",\"part\":{\"text\":\"Need to inspect the repo state first\"}}\necho {\"type\":\"step_start\",\"timestamp\":3,\"sessionID\":\"ses_1\"}\necho {\"type\":\"step_finish\",\"timestamp\":4,\"sessionID\":\"ses_1\",\"part\":{\"reason\":\"stop\"}}\necho {\"type\":\"tool_use\",\"timestamp\":5,\"sessionID\":\"ses_1\",\"part\":{\"tool\":\"bash\",\"state\":{\"status\":\"completed\",\"title\":\"Shows working tree status\",\"output\":\"On branch main\"}}}\necho {\"type\":\"error\",\"timestamp\":6,\"sessionID\":\"ses_1\",\"error\":{\"name\":\"APIError\",\"data\":{\"message\":\"Provider failed\"}}}"
    }

    #[cfg(not(windows))]
    fn json_events_script() -> &'static str {
        "printf '%s\\n' '{\"type\":\"text\",\"timestamp\":1,\"sessionID\":\"ses_1\",\"part\":{\"text\":\"Hello from OpenCode\"}}' '{\"type\":\"reasoning\",\"timestamp\":2,\"sessionID\":\"ses_1\",\"part\":{\"text\":\"Need to inspect the repo state first\"}}' '{\"type\":\"step_start\",\"timestamp\":3,\"sessionID\":\"ses_1\"}' '{\"type\":\"step_finish\",\"timestamp\":4,\"sessionID\":\"ses_1\",\"part\":{\"reason\":\"stop\"}}' '{\"type\":\"tool_use\",\"timestamp\":5,\"sessionID\":\"ses_1\",\"part\":{\"tool\":\"bash\",\"state\":{\"status\":\"completed\",\"title\":\"Shows working tree status\",\"output\":\"On branch main\"}}}' '{\"type\":\"error\",\"timestamp\":6,\"sessionID\":\"ses_1\",\"error\":{\"name\":\"APIError\",\"data\":{\"message\":\"Provider failed\"}}}'"
    }

    #[cfg(windows)]
    fn invalid_json_script() -> &'static str {
        "echo not json at all"
    }

    #[cfg(not(windows))]
    fn invalid_json_script() -> &'static str {
        "printf '%s\\n' 'not json at all'"
    }

    #[cfg(windows)]
    fn tool_error_json_script() -> &'static str {
        "echo {\"type\":\"tool_use\",\"timestamp\":1,\"sessionID\":\"ses_1\",\"part\":{\"tool\":\"bash\",\"state\":{\"status\":\"error\",\"error\":\"command failed\"}}}"
    }

    #[cfg(not(windows))]
    fn tool_error_json_script() -> &'static str {
        "printf '%s\\n' '{\"type\":\"tool_use\",\"timestamp\":1,\"sessionID\":\"ses_1\",\"part\":{\"tool\":\"bash\",\"state\":{\"status\":\"error\",\"error\":\"command failed\"}}}'"
    }

    #[cfg(windows)]
    fn step_finish_without_reason_script() -> &'static str {
        "echo {\"type\":\"step_finish\",\"timestamp\":1,\"sessionID\":\"ses_1\",\"part\":{}}"
    }

    #[cfg(not(windows))]
    fn step_finish_without_reason_script() -> &'static str {
        "printf '%s\\n' '{\"type\":\"step_finish\",\"timestamp\":1,\"sessionID\":\"ses_1\",\"part\":{}}'"
    }
}
