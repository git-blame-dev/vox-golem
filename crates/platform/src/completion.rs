use std::ffi::{OsStr, OsString};
use std::net::TcpListener;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;
trait ContextExt<T> {
    fn context(self, message: impl Into<String>) -> Result<T>;
    fn with_context(self, message: impl FnOnce() -> String) -> Result<T>;
}
impl<T, E: std::error::Error + Send + Sync + 'static> ContextExt<T> for std::result::Result<T, E> {
    fn context(self, message: impl Into<String>) -> Result<T> {
        self.map_err(|e| format!("{}: {e}", message.into()).into())
    }
    fn with_context(self, message: impl FnOnce() -> String) -> Result<T> {
        self.map_err(|e| format!("{}: {e}", message()).into())
    }
}
impl<T> ContextExt<T> for Option<T> {
    fn context(self, message: impl Into<String>) -> Result<T> {
        self.ok_or_else(|| message.into().into())
    }
    fn with_context(self, message: impl FnOnce() -> String) -> Result<T> {
        self.ok_or_else(|| message().into())
    }
}
macro_rules! bail {
    ($($message:tt)*) => {
        return Err(format!($($message)*).into())
    };
}
use crate::inference::{ActualInferenceProvider, InferencePolicy};
use crate::managed_process::{
    configure_owned_tokio, terminate_tokio, terminate_tokio_on_drop, ProcessOwnership,
};
use futures_util::StreamExt;
use reqwest::{redirect, Client};
use serde::{Deserialize, Serialize};
use tokio::process::{Child, Command};
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tokio::time::{sleep, timeout};

const STARTUP_TIMEOUT: Duration = Duration::from_secs(120);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(2);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);
const DEBOUNCE: Duration = Duration::from_millis(35);
const RETRY_DELAY: Duration = Duration::from_millis(200);
const MAX_SUFFIX_BYTES: usize = 128;
const MAX_STABLE_PROMPT_BYTES: usize = 384;
const MAX_ACTIVE_PREFIX_BYTES: usize = 64;
const MAX_PREDICT_TOKENS: u8 = 6;
const MAX_REQUEST_ATTEMPTS: u8 = 3;
const MAX_RESPONSE_BODY_BYTES: usize = 16 * 1024;

pub struct CompletionRuntime {
    server: Option<Child>,
    client: CompletionClient,
    provider: ActualInferenceProvider,
}

#[derive(Clone)]
pub struct CompletionClient {
    client: Client,
    endpoint: String,
    api_key: String,
}

pub struct CompletionPredictor {
    drafts: watch::Sender<Option<CompletionDraft>>,
    results: watch::Receiver<Option<CompletionPrediction>>,
    worker: JoinHandle<()>,
}

#[derive(Clone)]
pub struct CompletionRequestHandle {
    drafts: watch::Sender<Option<CompletionDraft>>,
}

#[derive(Clone, PartialEq, Eq)]
struct CompletionDraft {
    revision: u64,
    prompt: String,
}

#[derive(Clone)]
pub struct CompletionPrediction {
    pub revision: u64,
    pub prompt: String,
    pub suffix: Option<String>,
}

#[derive(Serialize)]
struct CompletionRequest<'a> {
    prompt: &'a str,
    n_predict: u8,
    temperature: u8,
    samplers: [&'static str; 1],
    stop: [&'static str; 1],
    stream: bool,
    cache_prompt: bool,
    id_slot: u8,
    response_fields: [&'static str; 3],
    #[serde(skip_serializing_if = "Option::is_none")]
    grammar: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CompletionResponse {
    content: String,
    stop_type: String,
    tokens_predicted: usize,
}

impl CompletionRuntime {
    pub async fn start(server: &Path, model: &Path) -> Result<Self> {
        Self::start_with_policy(server, model, InferencePolicy::Auto).await
    }

    pub async fn start_with_policy(
        server: &Path,
        model: &Path,
        policy: InferencePolicy,
    ) -> Result<Self> {
        let result = Self::start_once(server, model, policy).await;
        if policy == InferencePolicy::Auto
            && result.as_ref().is_err_and(|error| {
                let text = error.to_string();
                !text.contains("failed to start")
                    && !text.contains("was not found")
                    && !text.contains("path did not have")
                    && !text.contains("path is not valid")
            })
        {
            return Self::start_once(server, model, InferencePolicy::Cpu).await;
        }
        result
    }

    async fn start_once(server: &Path, model: &Path, policy: InferencePolicy) -> Result<Self> {
        if !server.is_file() {
            bail!("llama-server was not found at {}", server.display());
        }
        if !model.is_file() {
            bail!("completion model was not found at {}", model.display());
        }

        let port = reserve_loopback_port()?;
        let api_key = transient_api_key()?;
        let client = Client::builder()
            .no_proxy()
            .redirect(redirect::Policy::none())
            .connect_timeout(Duration::from_millis(500))
            .pool_idle_timeout(Duration::from_secs(30))
            .build()
            .context("failed to configure the completion client")?;
        let server_dir = server
            .parent()
            .context("completion server path did not have a parent directory")?;
        let gpu_layers = match policy {
            InferencePolicy::Cpu => "0",
            _ => "all",
        };
        let mut command = Command::new(server);
        command
            .args([
                "--model",
                model
                    .to_str()
                    .context("completion model path is not valid UTF-8")?,
                "--offline",
                "--host",
                "127.0.0.1",
                "--port",
                &port.to_string(),
                "--api-key",
                &api_key,
                "--no-ui",
                "--no-slots",
                "--parallel",
                "1",
                "--ctx-size",
                "512",
                "--n-gpu-layers",
                gpu_layers,
                "--cache-ram",
                "0",
            ])
            .envs(library_path_environment(server_dir.as_os_str()))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        configure_owned_tokio(&mut command);
        let mut child = command
            .spawn()
            .with_context(|| format!("failed to start {}", server.display()))?;
        // This server is owned by the runtime; descendants must share its
        // Unix process group (Windows keeps tokio's existing behavior).
        let base_url = format!("http://127.0.0.1:{port}");

        if let Err(error) = wait_until_ready(&client, &base_url, &mut child).await {
            let _ = stop_server(&mut child).await;
            return Err(error);
        }

        Ok(Self {
            server: Some(child),
            client: CompletionClient {
                client,
                endpoint: format!("{base_url}/completion"),
                api_key,
            },
            provider: if policy == InferencePolicy::Cpu {
                ActualInferenceProvider::Cpu
            } else {
                ActualInferenceProvider::RequestedCuda
            },
        })
    }

    pub fn client(&self) -> CompletionClient {
        self.client.clone()
    }

    pub fn actual_provider(&self) -> ActualInferenceProvider {
        self.provider
    }

    pub async fn shutdown(&mut self) -> Result<()> {
        let Some(mut server) = self.server.take() else {
            return Ok(());
        };
        stop_server(&mut server).await
    }
}

impl Drop for CompletionRuntime {
    fn drop(&mut self) {
        if let Some(server) = self.server.take() {
            terminate_tokio_on_drop(server, ProcessOwnership::Owned);
        }
    }
}

#[cfg(target_os = "linux")]
fn library_path_environment(directory: &OsStr) -> [(OsString, OsString); 1] {
    let mut value = directory.to_os_string();
    if let Some(existing) = std::env::var_os("LD_LIBRARY_PATH").filter(|path| !path.is_empty()) {
        value.push(":");
        value.push(existing);
    }
    [(OsString::from("LD_LIBRARY_PATH"), value)]
}

#[cfg(not(target_os = "linux"))]
fn library_path_environment(_directory: &OsStr) -> [(OsString, OsString); 0] {
    []
}

impl CompletionClient {
    pub fn predictor(&self) -> CompletionPredictor {
        let (drafts, draft_receiver) = watch::channel(None);
        let (result_sender, results) = watch::channel(None);
        let client = self.clone();
        let worker = tokio::spawn(run_predictor(client, draft_receiver, result_sender));
        CompletionPredictor {
            drafts,
            results,
            worker,
        }
    }

    async fn predict(&self, prompt: &str) -> Result<Option<String>> {
        let Some((stable_prompt, active_prefix)) = completion_input(prompt) else {
            return Ok(None);
        };
        let grammar = (!active_prefix.is_empty()).then(|| completion_grammar(active_prefix));
        let request = CompletionRequest {
            prompt: stable_prompt,
            n_predict: MAX_PREDICT_TOKENS,
            temperature: 0,
            samplers: ["temperature"],
            stop: ["\n"],
            stream: false,
            cache_prompt: true,
            id_slot: 0,
            response_fields: ["content", "stop_type", "tokens_predicted"],
            grammar,
        };
        let response = self
            .client
            .post(&self.endpoint)
            .bearer_auth(&self.api_key)
            .timeout(REQUEST_TIMEOUT)
            .json(&request)
            .send()
            .await
            .context("completion request failed")?
            .error_for_status()
            .context("completion server returned an error")?;
        let response = read_completion_response(response).await?;
        validate_response(response, active_prefix)
    }
}

async fn read_completion_response(response: reqwest::Response) -> Result<CompletionResponse> {
    let mut body = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.context("completion response body failed")?;
        if chunk.len() > MAX_RESPONSE_BODY_BYTES - body.len() {
            bail!("completion response exceeded size limit");
        }
        body.extend_from_slice(&chunk);
    }
    decode_completion_response(&body)
}

fn decode_completion_response(body: &[u8]) -> Result<CompletionResponse> {
    if body.len() > MAX_RESPONSE_BODY_BYTES {
        bail!("completion response exceeded size limit");
    }
    serde_json::from_slice(body).context("completion response was invalid")
}

impl CompletionPredictor {
    pub fn request_handle(&self) -> CompletionRequestHandle {
        CompletionRequestHandle {
            drafts: self.drafts.clone(),
        }
    }

    pub fn request(&self, revision: u64, prompt: String) {
        self.request_handle().request(revision, prompt);
    }

    pub fn clear(&self) {
        self.request_handle().clear();
    }

    pub async fn next(&mut self) -> Option<CompletionPrediction> {
        if self.results.changed().await.is_err() {
            return None;
        }
        self.results.borrow_and_update().clone()
    }
}

impl CompletionRequestHandle {
    pub fn request(&self, revision: u64, prompt: String) {
        self.drafts
            .send_replace(Some(CompletionDraft { revision, prompt }));
    }

    pub fn clear(&self) {
        self.drafts.send_replace(None);
    }
}

impl Drop for CompletionPredictor {
    fn drop(&mut self) {
        self.worker.abort();
    }
}

async fn run_predictor(
    client: CompletionClient,
    mut drafts: watch::Receiver<Option<CompletionDraft>>,
    results: watch::Sender<Option<CompletionPrediction>>,
) {
    let mut completed_draft = None;
    let mut attempted_draft = None;
    let mut request_attempts = 0_u8;
    loop {
        let Some(draft) = drafts.borrow_and_update().clone() else {
            if drafts.changed().await.is_err() {
                return;
            }
            continue;
        };
        if completed_draft.as_ref() == Some(&draft) {
            if drafts.changed().await.is_err() {
                return;
            }
            continue;
        }
        if attempted_draft.as_ref() != Some(&draft) {
            attempted_draft = Some(draft.clone());
            request_attempts = 0;
        }

        let request_delay = if request_attempts == 0 {
            DEBOUNCE
        } else {
            RETRY_DELAY
        };
        tokio::select! {
            changed = drafts.changed() => {
                if changed.is_err() {
                    return;
                }
                continue;
            }
            _ = sleep(request_delay) => {}
        }

        let prediction = client.predict(&draft.prompt);
        tokio::pin!(prediction);
        let suffix = tokio::select! {
            changed = drafts.changed() => {
                if changed.is_err() {
                    return;
                }
                continue;
            }
            result = &mut prediction => match result {
                Ok(Some(suffix)) => Some(suffix),
                Ok(None) => {
                    request_attempts += 1;
                    if request_attempts < MAX_REQUEST_ATTEMPTS {
                        continue;
                    }
                    None
                }
                Err(_) => {
                    request_attempts += 1;
                    if request_attempts >= MAX_REQUEST_ATTEMPTS {
                        // Exhaustion belongs to this revision only.  Emit a
                        // terminal empty result and keep serving newer drafts.
                        None
                    } else {
                        continue;
                    }
                }
            },
        };
        completed_draft = Some(draft.clone());
        results.send_replace(Some(CompletionPrediction {
            revision: draft.revision,
            prompt: draft.prompt.clone(),
            suffix,
        }));
    }
}

fn validate_response(response: CompletionResponse, active_prefix: &str) -> Result<Option<String>> {
    if response.tokens_predicted > usize::from(MAX_PREDICT_TOKENS)
        || !matches!(response.stop_type.as_str(), "eos" | "limit" | "word")
    {
        bail!("completion server returned inconsistent metadata");
    }
    let suffix = if active_prefix.is_empty() {
        response.content
    } else {
        let Some(suffix) = response.content.strip_prefix(active_prefix) else {
            return Ok(None);
        };
        suffix.to_string()
    };
    if suffix.is_empty()
        || suffix.len() > MAX_SUFFIX_BYTES
        || !suffix.is_ascii()
        || suffix.chars().any(char::is_control)
        || suffix.chars().all(char::is_whitespace)
    {
        return Ok(None);
    }
    Ok(Some(suffix))
}

fn completion_input(prompt: &str) -> Option<(&str, &str)> {
    let (stable_prompt, active_prefix) = split_active_prefix(prompt);
    if active_prefix.len() > MAX_ACTIVE_PREFIX_BYTES {
        return None;
    }
    let mut start = stable_prompt.len().saturating_sub(MAX_STABLE_PROMPT_BYTES);
    while !stable_prompt.is_char_boundary(start) {
        start += 1;
    }
    Some((&stable_prompt[start..], active_prefix))
}

fn split_active_prefix(text: &str) -> (&str, &str) {
    let start = text
        .char_indices()
        .rev()
        .find_map(|(index, character)| {
            (!character.is_ascii_alphabetic() && character != '\'')
                .then_some(index + character.len_utf8())
        })
        .unwrap_or(0);
    (&text[..start], &text[start..])
}

fn completion_grammar(active_prefix: &str) -> String {
    let literal = serde_json::to_string(active_prefix).expect("string serialization cannot fail");
    format!("root ::= {literal} [A-Za-z']* (\" \" [A-Za-z0-9_,'!?;:.-]+)*")
}

fn reserve_loopback_port() -> Result<u16> {
    let listener = TcpListener::bind(("127.0.0.1", 0))?;
    Ok(listener.local_addr()?.port())
}

fn transient_api_key() -> Result<String> {
    let random: [u8; 32] = rand::random();
    let mut api_key = String::with_capacity(64);
    const HEX: &[u8; 16] = b"0123456789abcdef";
    for byte in random {
        api_key.push(char::from(HEX[usize::from(byte >> 4)]));
        api_key.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    Ok(api_key)
}

async fn wait_until_ready(client: &Client, base_url: &str, child: &mut Child) -> Result<()> {
    let health_url = format!("{base_url}/health");
    timeout(STARTUP_TIMEOUT, async {
        loop {
            if let Some(status) = child
                .try_wait()
                .context("failed to inspect llama-server status")?
            {
                bail!("llama-server exited during startup with {status}");
            }
            if client
                .get(&health_url)
                .timeout(Duration::from_millis(500))
                .send()
                .await
                .is_ok_and(|response| response.status().is_success())
            {
                return Ok(());
            }
            sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .context("llama-server did not become ready within 120 seconds")?
}

async fn stop_server(child: &mut Child) -> Result<()> {
    terminate_tokio(child, ProcessOwnership::Owned, SHUTDOWN_TIMEOUT)
        .await
        .context("failed to stop llama-server")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{ErrorKind, Read, Write};
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::thread;

    struct FakeCompletionServer {
        endpoint: String,
        requests: Arc<AtomicUsize>,
        captured: Arc<Mutex<Vec<String>>>,
        release_first: Option<Arc<AtomicBool>>,
        first_cancelled: Arc<AtomicBool>,
        stop: Arc<AtomicBool>,
        handlers: Arc<Mutex<Vec<thread::JoinHandle<()>>>>,
        worker: Option<thread::JoinHandle<()>>,
    }

    impl FakeCompletionServer {
        fn start(status: &str) -> Self {
            Self::start_inner(status, false, 0)
        }

        fn start_paused(status: &str) -> Self {
            Self::start_inner(status, true, 0)
        }

        fn start_flaky(failures_before_success: usize) -> Self {
            Self::start_inner("200 OK", false, failures_before_success)
        }

        fn start_inner(status: &str, pause_first: bool, failures_before_success: usize) -> Self {
            let listener = TcpListener::bind(("127.0.0.1", 0)).expect("fake server bind");
            listener
                .set_nonblocking(true)
                .expect("fake server nonblocking mode");
            let endpoint = format!(
                "http://{}/completion",
                listener.local_addr().expect("fake server address")
            );
            let requests = Arc::new(AtomicUsize::new(0));
            let captured = Arc::new(Mutex::new(Vec::new()));
            let release_first = pause_first.then(|| Arc::new(AtomicBool::new(false)));
            let first_cancelled = Arc::new(AtomicBool::new(false));
            let stop = Arc::new(AtomicBool::new(false));
            let handlers = Arc::new(Mutex::new(Vec::new()));
            let worker_requests = Arc::clone(&requests);
            let worker_captured = Arc::clone(&captured);
            let worker_release_first = release_first.clone();
            let worker_first_cancelled = Arc::clone(&first_cancelled);
            let worker_stop = Arc::clone(&stop);
            let worker_handlers = Arc::clone(&handlers);
            let status = status.to_string();
            let worker = thread::spawn(move || {
                while !worker_stop.load(Ordering::SeqCst) {
                    match listener.accept() {
                        Ok((mut stream, _)) => {
                            let request_number = worker_requests.fetch_add(1, Ordering::SeqCst) + 1;
                            let captured = Arc::clone(&worker_captured);
                            let release_first = worker_release_first.clone();
                            let first_cancelled = Arc::clone(&worker_first_cancelled);
                            let stop = Arc::clone(&worker_stop);
                            let status = status.clone();
                            let handler = thread::spawn(move || {
                                stream
                                    .set_read_timeout(Some(Duration::from_secs(1)))
                                    .expect("fake server read timeout");
                                let Some(request) = read_http_request(&mut stream) else {
                                    if request_number == 1 && release_first.is_some() {
                                        first_cancelled.store(true, Ordering::SeqCst);
                                        return;
                                    }
                                    panic!("request ended before its body was complete");
                                };
                                captured.lock().expect("captured requests").push(request);
                                if request_number == 1 {
                                    if let Some(release) = release_first.as_ref() {
                                        stream
                                            .set_nonblocking(true)
                                            .expect("fake server cancellation probe");
                                        while !release.load(Ordering::SeqCst)
                                            && !stop.load(Ordering::SeqCst)
                                        {
                                            match stream.peek(&mut [0_u8; 1]) {
                                                Ok(0) => {
                                                    first_cancelled.store(true, Ordering::SeqCst);
                                                    return;
                                                }
                                                Ok(_) => {}
                                                Err(error)
                                                    if error.kind() == ErrorKind::WouldBlock => {}
                                                Err(error) => {
                                                    panic!(
                                                        "fake server cancellation probe failed: {error}"
                                                    )
                                                }
                                            }
                                            thread::sleep(Duration::from_millis(2));
                                        }
                                    }
                                }
                                let status = if request_number <= failures_before_success {
                                    "503 Service Unavailable"
                                } else {
                                    &status
                                };
                                let body = if status == "200 OK" {
                                    r#"{"content":"next","stop_type":"limit","tokens_predicted":1}"#
                                } else {
                                    r#"{"error":"unavailable"}"#
                                };
                                let response = format!(
                                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                                    body.len()
                                );
                                let _ = stream.write_all(response.as_bytes());
                            });
                            worker_handlers
                                .lock()
                                .expect("fake server handlers")
                                .push(handler);
                        }
                        Err(error) if error.kind() == ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(2));
                        }
                        Err(error) => panic!("fake server accept failed: {error}"),
                    }
                }
            });
            Self {
                endpoint,
                requests,
                captured,
                release_first,
                first_cancelled,
                stop,
                handlers,
                worker: Some(worker),
            }
        }

        fn client(&self) -> CompletionClient {
            CompletionClient {
                client: Client::builder().no_proxy().build().expect("test client"),
                endpoint: self.endpoint.clone(),
                api_key: "test-key".to_string(),
            }
        }

        fn release_first(&self) {
            self.release_first
                .as_ref()
                .expect("paused fake server")
                .store(true, Ordering::SeqCst);
        }

        async fn wait_for_requests(&self, count: usize) {
            timeout(Duration::from_secs(1), async {
                while self.requests.load(Ordering::SeqCst) < count {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("fake server request timeout");
        }

        async fn wait_for_first_cancellation(&self) {
            timeout(Duration::from_secs(1), async {
                while !self.first_cancelled.load(Ordering::SeqCst) {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("fake server cancellation timeout");
        }
    }

    impl Drop for FakeCompletionServer {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::SeqCst);
            if let Some(release) = self.release_first.as_ref() {
                release.store(true, Ordering::SeqCst);
            }
            self.worker
                .take()
                .expect("fake server worker")
                .join()
                .expect("fake server shutdown");
            for handler in self
                .handlers
                .lock()
                .expect("fake server handlers")
                .drain(..)
            {
                handler.join().expect("fake server handler shutdown");
            }
        }
    }

    fn read_http_request(stream: &mut std::net::TcpStream) -> Option<String> {
        let mut request = Vec::new();
        let mut expected_len = None;
        loop {
            let mut chunk = [0_u8; 1024];
            let read = stream.read(&mut chunk).expect("fake server request");
            if read == 0 {
                return None;
            }
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
                return Some(String::from_utf8(request).expect("UTF-8 request"));
            }
        }
    }

    #[test]
    fn completion_response_decoder_rejects_oversized_bodies() {
        let body = vec![b'x'; MAX_RESPONSE_BODY_BYTES + 1];
        let error = decode_completion_response(&body).expect_err("oversized body must fail");
        assert!(error.to_string().contains("exceeded size limit"));
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn runtime_drop_reaps_owned_server() {
        let mut command = Command::new("sh");
        command.args(["-c", "exec sleep 30"]).kill_on_drop(true);
        configure_owned_tokio(&mut command);
        let child = command.spawn().expect("test child should start");
        let pid = child.id().expect("test child should expose its pid");
        let runtime = CompletionRuntime {
            server: Some(child),
            client: CompletionClient {
                client: Client::builder().no_proxy().build().expect("test client"),
                endpoint: String::from("http://127.0.0.1:1/completion"),
                api_key: String::from("test-key"),
            },
            provider: ActualInferenceProvider::Cpu,
        };

        drop(runtime);

        timeout(Duration::from_secs(2), async {
            while Path::new(&format!("/proc/{pid}")).exists() {
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("drop fallback should reap the child");
    }

    #[test]
    fn active_prefix_replay_is_stable() {
        assert_eq!(split_active_prefix("are you worki"), ("are you ", "worki"));
        assert_eq!(split_active_prefix("hello "), ("hello ", ""));
        assert_eq!(
            completion_grammar("worki"),
            "root ::= \"worki\" [A-Za-z']* (\" \" [A-Za-z0-9_,'!?;:.-]+)*"
        );
    }

    #[test]
    fn completion_input_bounds_context_and_partial_word_replay() {
        let prompt = format!("{} are you worki", "é".repeat(300));
        let (stable, prefix) = completion_input(&prompt).expect("bounded input");
        assert!(stable.len() <= MAX_STABLE_PROMPT_BYTES);
        assert!(stable.is_char_boundary(0));
        assert!(stable.ends_with(" are you "));
        assert_eq!(prefix, "worki");

        let long_word = "a".repeat(MAX_ACTIVE_PREFIX_BYTES + 1);
        assert!(completion_input(&long_word).is_none());
    }

    #[test]
    fn response_validation_strips_replay_and_rejects_unsafe_text() {
        let response = CompletionResponse {
            content: "working on this".to_string(),
            stop_type: "limit".to_string(),
            tokens_predicted: 4,
        };
        assert_eq!(
            validate_response(response, "worki").expect("valid completion"),
            Some("ng on this".to_string())
        );

        let unsafe_response = CompletionResponse {
            content: "hello\u{1b}".to_string(),
            stop_type: "limit".to_string(),
            tokens_predicted: 2,
        };
        assert_eq!(
            validate_response(unsafe_response, "").expect("unsafe candidate is ignored"),
            None
        );

        let whitespace_response = CompletionResponse {
            content: "   ".to_string(),
            stop_type: "limit".to_string(),
            tokens_predicted: 1,
        };
        assert_eq!(
            validate_response(whitespace_response, "").expect("blank candidate is ignored"),
            None
        );
    }

    #[tokio::test]
    async fn predictor_coalesces_drafts_clears_work_and_does_not_repeat() {
        let server = FakeCompletionServer::start("200 OK");
        let mut predictor = server.client().predictor();

        predictor.request(1, "old ".to_string());
        sleep(Duration::from_millis(10)).await;
        predictor.request(2, "new ".to_string());
        let result = timeout(Duration::from_secs(1), predictor.next())
            .await
            .expect("prediction timeout")
            .expect("prediction channel");
        assert_eq!(result.revision, 2);
        assert_eq!(result.prompt, "new ");
        assert_eq!(result.suffix.as_deref(), Some("next"));

        sleep(Duration::from_millis(80)).await;
        assert_eq!(server.requests.load(Ordering::SeqCst), 1);

        predictor.request(3, "clear ".to_string());
        predictor.clear();
        sleep(Duration::from_millis(80)).await;
        assert_eq!(server.requests.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn predictor_accepts_a_changed_prompt_with_a_reused_revision() {
        let server = FakeCompletionServer::start("200 OK");
        let mut predictor = server.client().predictor();
        predictor.request(1, "first ".to_string());
        let first = timeout(Duration::from_secs(1), predictor.next())
            .await
            .expect("first prediction timeout")
            .expect("first prediction");
        assert_eq!(first.prompt, "first ");

        predictor.request(1, "second ".to_string());
        let second = timeout(Duration::from_secs(1), predictor.next())
            .await
            .expect("second prediction timeout")
            .expect("second prediction");

        assert_eq!(second.prompt, "second ");
        assert_eq!(server.requests.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn predictor_stops_after_a_server_failure() {
        let server = FakeCompletionServer::start("500 Internal Server Error");
        let mut predictor = server.client().predictor();
        predictor.request(1, "hello ".to_string());

        let prediction = timeout(Duration::from_secs(1), predictor.next())
            .await
            .expect("prediction timeout")
            .expect("terminal prediction should be emitted");
        assert_eq!(prediction.revision, 1);
        assert_eq!(prediction.suffix, None);
        assert_eq!(
            server.requests.load(Ordering::SeqCst),
            usize::from(MAX_REQUEST_ATTEMPTS)
        );
    }

    #[tokio::test]
    async fn predictor_recovers_after_exhaustion_for_a_new_revision() {
        let server = FakeCompletionServer::start_flaky(3);
        let mut predictor = server.client().predictor();
        predictor.request(1, "first ".to_string());

        let exhausted = timeout(Duration::from_secs(1), predictor.next())
            .await
            .expect("exhaustion result timeout")
            .expect("prediction channel should remain open");
        assert_eq!(exhausted.revision, 1);
        assert_eq!(exhausted.suffix, None);

        predictor.request(2, "second ".to_string());
        let recovered = timeout(Duration::from_secs(1), predictor.next())
            .await
            .expect("recovery result timeout")
            .expect("prediction channel should remain open");
        assert_eq!(recovered.revision, 2);
        assert_eq!(recovered.suffix.as_deref(), Some("next"));
    }

    #[tokio::test]
    async fn predictor_retries_a_transient_failure_for_the_latest_draft() {
        let server = FakeCompletionServer::start_flaky(1);
        let mut predictor = server.client().predictor();
        predictor.request(1, "hello ".to_string());

        let result = timeout(Duration::from_secs(1), predictor.next())
            .await
            .expect("prediction retry timeout")
            .expect("prediction channel");
        assert_eq!(result.revision, 1);
        assert_eq!(result.suffix.as_deref(), Some("next"));
        assert_eq!(server.requests.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn predictor_supersedes_and_clears_in_flight_requests() {
        let server = FakeCompletionServer::start_paused("200 OK");
        let mut predictor = server.client().predictor();
        predictor.request(1, "first ".to_string());
        server.wait_for_requests(1).await;

        predictor.request(2, "second ".to_string());
        server.wait_for_requests(2).await;
        let result = timeout(Duration::from_secs(1), predictor.next())
            .await
            .expect("latest prediction timeout")
            .expect("latest prediction channel");
        assert_eq!(result.revision, 2);
        assert_eq!(result.prompt, "second ");
        server.release_first();

        let server = FakeCompletionServer::start_paused("200 OK");
        let mut predictor = server.client().predictor();
        predictor.request(1, "clear ".to_string());
        server.wait_for_requests(1).await;
        predictor.clear();
        server.wait_for_first_cancellation().await;
        server.release_first();
        assert!(timeout(Duration::from_millis(100), predictor.next())
            .await
            .is_err());
    }

    #[tokio::test]
    async fn cloned_request_handles_coalesce_drafts_and_clear_before_debounce() {
        let server = FakeCompletionServer::start("200 OK");
        let predictor = server.client().predictor();
        let first_handle = predictor.request_handle();
        let second_handle = first_handle.clone();

        first_handle.request(1, "old ".to_string());
        second_handle.request(2, "new ".to_string());
        let mut results = predictor;
        let result = timeout(Duration::from_secs(1), results.next())
            .await
            .expect("prediction timeout")
            .expect("prediction channel");
        assert_eq!(result.revision, 2);

        second_handle.clear();
        first_handle.request(3, "stale ".to_string());
        first_handle.clear();
        sleep(Duration::from_millis(80)).await;
        assert_eq!(server.requests.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn completion_request_matches_the_pinned_server_contract() {
        let server = FakeCompletionServer::start("200 OK");
        assert_eq!(
            server
                .client()
                .predict("are you worki")
                .await
                .expect("completion request"),
            None
        );
        let captured = server.captured.lock().expect("captured request");
        let request = captured.first().expect("request capture");
        let (headers, body) = request.split_once("\r\n\r\n").expect("HTTP request");
        assert!(headers.contains("authorization: Bearer test-key"));
        let body: serde_json::Value = serde_json::from_str(body).expect("request JSON");
        assert_eq!(body["prompt"], "are you ");
        assert_eq!(body["n_predict"], MAX_PREDICT_TOKENS);
        assert_eq!(body["temperature"], 0);
        assert_eq!(body["samplers"], serde_json::json!(["temperature"]));
        assert_eq!(body["stop"], serde_json::json!(["\n"]));
        assert_eq!(body["stream"], false);
        assert_eq!(body["cache_prompt"], true);
        assert_eq!(body["id_slot"], 0);
        assert_eq!(
            body["response_fields"],
            serde_json::json!(["content", "stop_type", "tokens_predicted"])
        );
        assert_eq!(
            body["grammar"],
            "root ::= \"worki\" [A-Za-z']* (\" \" [A-Za-z0-9_,'!?;:.-]+)*"
        );
    }
}
