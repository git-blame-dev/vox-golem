//! Loopback-only Neural TTS synthesis client. Audio playback belongs to the caller.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use rand::RngCore;
use serde::{Deserialize, Serialize};
use tokio::sync::Notify;

const CONTROL_BYTES: usize = 4096;
const AUDIO_FRAME_BYTES: usize = 65_536;
const MAX_AUDIO_BYTES: usize = 64 * 1024 * 1024;
const STARTUP_TIMEOUT: Duration = Duration::from_secs(180);

#[derive(Debug, Clone)]
pub struct NeuralTtsOptions {
    pub endpoint: String,
    pub start_command: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct Voice {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub aliases: Vec<String>,
    #[serde(default)]
    pub default: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Capabilities {
    pub protocol_version: u32,
    pub ready: bool,
    pub backend: String,
    pub default_voice: String,
    pub voices: Vec<Voice>,
    pub sample_rate_hz: u32,
    pub channels: u16,
    pub incremental_audio: bool,
}

impl Capabilities {
    fn validate(&self) -> Result<(), Error> {
        if self.protocol_version != 1
            || !valid_id(&self.backend)
            || !valid_id(&self.default_voice)
            || self.voices.is_empty()
            || self.voices.len() > 32
            || !(1..=96_000).contains(&self.sample_rate_hz)
            || self.channels != 1
        {
            return Err(Error::Protocol);
        }
        let mut ids = std::collections::HashSet::new();
        for voice in &self.voices {
            if !valid_id(&voice.id)
                || voice.name.is_empty()
                || voice.name.len() > 256
                || voice.name.chars().any(char::is_control)
                || voice.aliases.len() > 32
                || !ids.insert(voice.id.as_str())
            {
                return Err(Error::Protocol);
            }
            for alias in &voice.aliases {
                if !valid_id(alias) || !ids.insert(alias.as_str()) {
                    return Err(Error::Protocol);
                }
            }
        }
        if self.resolve_voice(&self.default_voice).is_none() {
            return Err(Error::Protocol);
        }
        Ok(())
    }

    pub fn resolve_voice(&self, requested: &str) -> Option<&Voice> {
        let requested = if requested.is_empty() {
            self.default_voice.as_str()
        } else {
            requested
        };
        self.voices.iter().find(|voice| {
            voice.id == requested || voice.aliases.iter().any(|alias| alias == requested)
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Configuration,
    Unavailable,
    Incompatible,
    VoiceUnavailable,
    Busy,
    Cancelled,
    Transport,
    Protocol,
    Synthesis,
    Output,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Configuration => "Neural TTS configuration is invalid",
            Self::Unavailable => "Neural TTS service is unavailable",
            Self::Incompatible => "Neural TTS service protocol is incompatible",
            Self::VoiceUnavailable => "The selected Neural TTS voice is unavailable",
            Self::Busy => "Neural TTS is busy",
            Self::Cancelled => "Neural TTS synthesis was cancelled",
            Self::Transport => "Neural TTS connection failed",
            Self::Protocol => "Neural TTS returned invalid or incomplete audio",
            Self::Synthesis => "Neural TTS synthesis failed",
            Self::Output => "Neural TTS audio output failed",
        })
    }
}
impl std::error::Error for Error {}

#[derive(Debug, Default)]
pub struct Cancellation {
    cancelled: AtomicBool,
    notify: Notify,
}
impl Cancellation {
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
        self.notify.notify_waiters();
    }
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }
    async fn notified(&self) {
        let notified = self.notify.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        if !self.is_cancelled() {
            notified.await;
        }
    }
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct AudioFormat {
    pub protocol_version: u32,
    pub job_id: String,
    pub sample_rate_hz: u32,
    pub channels: u16,
    pub format: String,
}
pub enum AudioEvent {
    Format(AudioFormat),
    Samples(Vec<f32>),
}
#[derive(Debug)]
pub struct Audio {
    pub samples: Vec<f32>,
    pub sample_rate_hz: u32,
}

pub struct NeuralTtsClient {
    http: reqwest::Client,
    base: reqwest::Url,
    session: std::sync::Mutex<String>,
    renewal: tokio::sync::Mutex<()>,
    capabilities: Capabilities,
    runtime: tokio::runtime::Handle,
    closed: AtomicBool,
}

impl std::fmt::Debug for NeuralTtsClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NeuralTtsClient")
            .field("backend", &self.capabilities.backend)
            .field("closed", &self.closed.load(Ordering::Acquire))
            .finish_non_exhaustive()
    }
}

impl NeuralTtsClient {
    pub async fn connect(options: &NeuralTtsOptions) -> Result<Self, Error> {
        let base = validate_endpoint(&options.endpoint)?;
        let http = super::owned_loopback::client_builder()
            .connect_timeout(Duration::from_secs(3))
            .timeout(Duration::from_secs(450))
            .build()
            .map_err(|_| Error::Configuration)?;
        let started = Instant::now();
        let mut spawned = false;
        let mut session = None;
        let mut delay = Duration::from_millis(100);
        let capabilities = loop {
            match fetch_capabilities(&http, &base).await {
                Ok(capabilities) if capabilities.ready => break capabilities,
                Ok(_) | Err(Error::Busy) => {
                    if session.is_none() {
                        // Session acquisition is the service's explicit recovery gate.
                        // A dead worker cannot become ready merely by polling metadata.
                        session = Some(
                            open_session(
                                &http,
                                &base,
                                STARTUP_TIMEOUT.saturating_sub(started.elapsed()),
                            )
                            .await?,
                        );
                    }
                }
                Err(Error::Unavailable) if !spawned && !options.start_command.is_empty() => {
                    start_service(&options.start_command)?;
                    spawned = true;
                }
                Err(Error::Unavailable) if !spawned => return Err(Error::Unavailable),
                Err(Error::Unavailable) => {}
                Err(error) => return Err(error),
            }
            if started.elapsed() >= STARTUP_TIMEOUT {
                return Err(Error::Unavailable);
            }
            tokio::time::sleep(delay).await;
            delay = (delay * 2).min(Duration::from_secs(1));
        };
        let session = if let Some(session) = session {
            session
        } else {
            open_session(&http, &base, Duration::from_secs(10)).await?
        };
        Ok(Self {
            http,
            base,
            session: std::sync::Mutex::new(session),
            renewal: tokio::sync::Mutex::new(()),
            capabilities,
            runtime: tokio::runtime::Handle::current(),
            closed: AtomicBool::new(false),
        })
    }

    pub fn capabilities(&self) -> &Capabilities {
        &self.capabilities
    }

    /// Each callback runs on the async caller. Blocking native output must be offloaded.
    /// A cancelled/output-failed request drains the service terminal without delivering
    /// more PCM; cancelling the caller does not kill a shared model process.
    pub async fn synthesize<F, Fut>(
        &self,
        text: &str,
        voice: &str,
        cancellation: Arc<Cancellation>,
        output: F,
    ) -> Result<(), Error>
    where
        F: FnMut(AudioEvent) -> Fut,
        Fut: std::future::Future<Output = Result<(), Error>>,
    {
        self.synthesize_with_priority(text, voice, cancellation, false, output)
            .await
    }

    async fn synthesize_with_priority<F, Fut>(
        &self,
        text: &str,
        voice: &str,
        cancellation: Arc<Cancellation>,
        background: bool,
        mut output: F,
    ) -> Result<(), Error>
    where
        F: FnMut(AudioEvent) -> Fut,
        Fut: std::future::Future<Output = Result<(), Error>>,
    {
        if self.closed.load(Ordering::Acquire) {
            return Err(Error::Unavailable);
        }
        if text.trim().is_empty() || text.len() > 4096 {
            return Err(Error::Configuration);
        }
        let voice = self
            .capabilities
            .resolve_voice(voice)
            .ok_or(Error::VoiceUnavailable)?;
        if cancellation.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let job = random_id();
        let mut session = self.session.lock().map_err(|_| Error::Unavailable)?.clone();
        let mut lease = JobLease {
            http: self.http.clone(),
            base: self.base.clone(),
            session: session.clone(),
            job: job.clone(),
            runtime: self.runtime.clone(),
            ended: false,
        };
        let mut renewed = false;
        let mut response = loop {
            let request = self
                .http
                .post(route(&self.base, "stream")?)
                .json(&serde_json::json!({
                    "session_id": session, "job_id": job, "text": text, "voice": voice.id,
                    "background": background,
                }))
                .send();
            tokio::pin!(request);
            let response = tokio::select! {
                _ = cancellation.notified() => return Err(Error::Cancelled),
                result = &mut request => result.map_err(|_| Error::Transport)?,
            };
            // The versioned service rejects an expired/missing session before job
            // admission. Renew once; never replay an accepted or ambiguous request.
            if response.status() == reqwest::StatusCode::NOT_FOUND && !renewed {
                let _renewal = self.renewal.lock().await;
                let current = self.session.lock().map_err(|_| Error::Unavailable)?.clone();
                session = if current != session {
                    current
                } else {
                    let next =
                        open_session(&self.http, &self.base, Duration::from_secs(10)).await?;
                    *self.session.lock().map_err(|_| Error::Unavailable)? = next.clone();
                    next
                };
                lease.session = session.clone();
                renewed = true;
                continue;
            }
            break response;
        };
        if !response.status().is_success() {
            return Err(status_error(response.status()));
        }
        if !response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.starts_with("application/vnd.neural-tts.pcm"))
        {
            self.cancel_job(&session, &job).await;
            return Err(Error::Protocol);
        }
        let mut decoder = Decoder::new(&job);
        let mut cancelled = false;
        let mut output_failure = None;
        loop {
            if cancellation.is_cancelled() && !cancelled {
                cancelled = true;
                self.cancel_job(&session, &job).await;
            }
            let chunk = if cancelled {
                response.chunk().await
            } else {
                tokio::select! {
                    _ = cancellation.notified() => continue,
                    chunk = response.chunk() => chunk,
                }
            }
            .map_err(|_| Error::Transport)?;
            let Some(chunk) = chunk else {
                break;
            };
            for frame in decoder.feed(&chunk)? {
                if !cancelled {
                    let event = match frame {
                        Frame::Format(value) => Some(AudioEvent::Format(value)),
                        Frame::Samples(value) => Some(AudioEvent::Samples(value)),
                        Frame::Terminal => None,
                    };
                    if let Some(event) = event {
                        if let Err(error) = output(event).await {
                            output_failure = Some(error);
                            cancelled = true;
                            self.cancel_job(&session, &job).await;
                        }
                    }
                }
            }
        }
        let terminal = decoder.finish()?;
        lease.ended = true;
        if let Some(error) = output_failure {
            return Err(error);
        }
        if cancelled || terminal != "completed" {
            return Err(if cancelled || terminal == "cancelled" {
                Error::Cancelled
            } else {
                Error::Synthesis
            });
        }
        Ok(())
    }

    pub async fn collect(
        &self,
        text: &str,
        voice: &str,
        cancellation: Arc<Cancellation>,
    ) -> Result<Audio, Error> {
        self.collect_with_priority(text, voice, cancellation, false)
            .await
    }

    /// Optional fixed cues wait behind interactive replies in the shared engine queue.
    pub async fn collect_background(
        &self,
        text: &str,
        voice: &str,
        cancellation: Arc<Cancellation>,
    ) -> Result<Audio, Error> {
        self.collect_with_priority(text, voice, cancellation, true)
            .await
    }

    async fn collect_with_priority(
        &self,
        text: &str,
        voice: &str,
        cancellation: Arc<Cancellation>,
        background: bool,
    ) -> Result<Audio, Error> {
        let mut samples = Vec::new();
        let mut rate = 0;
        self.synthesize_with_priority(text, voice, cancellation, background, |event| {
            match event {
                AudioEvent::Format(format) => rate = format.sample_rate_hz,
                AudioEvent::Samples(chunk) => samples.extend(chunk),
            }
            std::future::ready(Ok(()))
        })
        .await?;
        Ok(Audio {
            samples,
            sample_rate_hz: rate,
        })
    }

    async fn cancel_job(&self, session: &str, job: &str) {
        let _ = self
            .http
            .post(route(&self.base, "cancel").expect("validated synthesis route"))
            .json(&serde_json::json!({"session_id": session, "job_id": job}))
            .timeout(Duration::from_secs(5))
            .send()
            .await;
    }

    pub fn close(&self) {
        if !self.closed.swap(true, Ordering::AcqRel) {
            let http = self.http.clone();
            let base = self.base.clone();
            let Ok(session) = self.session.lock().map(|session| session.clone()) else {
                return;
            };
            self.runtime.spawn(async move {
                let _ = http
                    .post(route(&base, "release").expect("validated synthesis route"))
                    .json(&serde_json::json!({"session_id": session}))
                    .timeout(Duration::from_secs(5))
                    .send()
                    .await;
            });
        }
    }
}

/// Dropping a network future or rejecting its protocol still cancels its own job.
/// The server retains physical inference ownership until actual engine retirement.
struct JobLease {
    http: reqwest::Client,
    base: reqwest::Url,
    session: String,
    job: String,
    runtime: tokio::runtime::Handle,
    ended: bool,
}
impl Drop for JobLease {
    fn drop(&mut self) {
        if self.ended {
            return;
        }
        let http = self.http.clone();
        let base = self.base.clone();
        let session = self.session.clone();
        let job = self.job.clone();
        self.runtime.spawn(async move {
            let _ = http
                .post(route(&base, "cancel").expect("validated synthesis route"))
                .json(&serde_json::json!({"session_id": session, "job_id": job}))
                .timeout(Duration::from_secs(5))
                .send()
                .await;
        });
    }
}
impl Drop for NeuralTtsClient {
    fn drop(&mut self) {
        self.close();
    }
}

fn start_service(command: &[String]) -> Result<(), Error> {
    if command.len() > 32
        || command
            .iter()
            .any(|arg| arg.is_empty() || arg.len() > 4096 || arg.contains('\0'))
    {
        return Err(Error::Configuration);
    }
    let mut child = std::process::Command::new(&command[0])
        .args(&command[1..])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|_| Error::Unavailable)?;
    // Reap only our launch handle. Service lifetime is shared and not tied to Drop.
    std::thread::Builder::new()
        .name("neural-tts-launch".into())
        .spawn(move || {
            let _ = child.wait();
        })
        .map_err(|_| Error::Unavailable)?;
    Ok(())
}

fn random_id() -> String {
    let mut bytes = [0_u8; 24];
    rand::rng().fill_bytes(&mut bytes);
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
fn valid_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte))
}
fn validate_endpoint(value: &str) -> Result<reqwest::Url, Error> {
    let mut url = reqwest::Url::parse(value).map_err(|_| Error::Configuration)?;
    let host = url.host_str().ok_or(Error::Configuration)?;
    let allowed = host == "localhost"
        || host
            .trim_matches(['[', ']'])
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback());
    if url.scheme() != "http"
        || !allowed
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || !matches!(url.path(), "" | "/")
    {
        return Err(Error::Configuration);
    }
    url.set_path("/");
    Ok(url)
}
fn route(base: &reqwest::Url, operation: &str) -> Result<reqwest::Url, Error> {
    base.join(&format!("v1/synthesis/{operation}"))
        .map_err(|_| Error::Configuration)
}
fn status_error(status: reqwest::StatusCode) -> Error {
    match status.as_u16() {
        429 => Error::Busy,
        503 => Error::Unavailable,
        404 | 405 => Error::Incompatible,
        _ => Error::Protocol,
    }
}
async fn bounded_json<T: serde::de::DeserializeOwned>(
    mut response: reqwest::Response,
) -> Result<T, Error> {
    if !response.status().is_success() {
        return Err(status_error(response.status()));
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| Error::Transport)? {
        if bytes.len().saturating_add(chunk.len()) > 32 * 1024 {
            return Err(Error::Protocol);
        }
        bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&bytes).map_err(|_| Error::Protocol)
}
async fn fetch_capabilities(
    http: &reqwest::Client,
    base: &reqwest::Url,
) -> Result<Capabilities, Error> {
    let response = http
        .get(route(base, "capabilities")?)
        .timeout(Duration::from_secs(5))
        .send()
        .await
        .map_err(|error| {
            if error.is_timeout() {
                Error::Busy
            } else {
                Error::Unavailable
            }
        })?;
    // A warming synthesis service still reports its bounded descriptor.
    if response.status() == reqwest::StatusCode::SERVICE_UNAVAILABLE {
        return Err(Error::Busy);
    }
    let wire: serde_json::Value = bounded_json(response).await?;
    if wire
        .get("protocol_version")
        .and_then(serde_json::Value::as_u64)
        != Some(1)
    {
        return Err(Error::Incompatible);
    }
    if wire.get("ready").and_then(serde_json::Value::as_bool) == Some(false) {
        return Err(Error::Busy);
    }
    let value: Capabilities = serde_json::from_value(wire).map_err(|_| Error::Protocol)?;
    value.validate()?;
    Ok(value)
}

async fn open_session(
    http: &reqwest::Client,
    base: &reqwest::Url,
    timeout: Duration,
) -> Result<String, Error> {
    #[derive(Deserialize)]
    struct Session {
        session_id: String,
    }
    let response = http
        .post(route(base, "session")?)
        .json(&serde_json::json!({}))
        .timeout(timeout.max(Duration::from_millis(1)))
        .send()
        .await
        .map_err(|_| Error::Unavailable)?;
    let session: Session = bounded_json(response).await?;
    if session.session_id.len() < 32 || !valid_id(&session.session_id) {
        return Err(Error::Protocol);
    }
    Ok(session.session_id)
}

enum Frame {
    Format(AudioFormat),
    Samples(Vec<f32>),
    Terminal,
}
struct Decoder {
    job: String,
    pending: Vec<u8>,
    format: Option<AudioFormat>,
    terminal: Option<String>,
    samples: usize,
}
impl Decoder {
    fn new(job: &str) -> Self {
        Self {
            job: job.into(),
            pending: Vec::new(),
            format: None,
            terminal: None,
            samples: 0,
        }
    }
    fn feed(&mut self, mut input: &[u8]) -> Result<Vec<Frame>, Error> {
        let mut frames = Vec::new();
        while !input.is_empty() {
            let count = input.len().min(AUDIO_FRAME_BYTES + 5 - self.pending.len());
            if count == 0 {
                return Err(Error::Protocol);
            }
            self.pending.extend_from_slice(&input[..count]);
            input = &input[count..];
            while self.pending.len() >= 5 {
                let kind = self.pending[0];
                let len = u32::from_le_bytes(self.pending[1..5].try_into().unwrap()) as usize;
                let limit = if kind == 2 {
                    AUDIO_FRAME_BYTES
                } else {
                    CONTROL_BYTES
                };
                if len == 0 || len > limit || !matches!(kind, 1..=3) || self.terminal.is_some() {
                    return Err(Error::Protocol);
                }
                if self.pending.len() < len + 5 {
                    break;
                }
                let payload = &self.pending[5..len + 5];
                let frame = match kind {
                    1 if self.format.is_none() => {
                        let format: AudioFormat =
                            serde_json::from_slice(payload).map_err(|_| Error::Protocol)?;
                        if format.protocol_version != 1
                            || format.job_id != self.job
                            || !(1..=96_000).contains(&format.sample_rate_hz)
                            || format.channels != 1
                            || format.format != "f32le"
                        {
                            return Err(Error::Protocol);
                        }
                        self.format = Some(format.clone());
                        Frame::Format(format)
                    }
                    2 if self.format.is_some() && len.is_multiple_of(4) => {
                        let samples: Vec<f32> = payload
                            .chunks_exact(4)
                            .map(|bytes| f32::from_le_bytes(bytes.try_into().unwrap()))
                            .collect();
                        self.samples = self
                            .samples
                            .checked_add(samples.len())
                            .ok_or(Error::Protocol)?;
                        let rate = self.format.as_ref().unwrap().sample_rate_hz as usize;
                        if self.samples > MAX_AUDIO_BYTES / 4
                            || self.samples > rate * 360
                            || samples.iter().any(|sample| !sample.is_finite())
                        {
                            return Err(Error::Protocol);
                        }
                        Frame::Samples(samples)
                    }
                    3 if self.format.is_some() => {
                        #[derive(Deserialize)]
                        struct Terminal {
                            status: String,
                            samples: usize,
                        }
                        let terminal: Terminal =
                            serde_json::from_slice(payload).map_err(|_| Error::Protocol)?;
                        if !matches!(
                            terminal.status.as_str(),
                            "completed" | "cancelled" | "failed"
                        ) || terminal.samples != self.samples
                            || (terminal.status == "completed" && self.samples == 0)
                        {
                            return Err(Error::Protocol);
                        }
                        self.terminal = Some(terminal.status.clone());
                        Frame::Terminal
                    }
                    _ => return Err(Error::Protocol),
                };
                self.pending.drain(..len + 5);
                frames.push(frame);
            }
        }
        Ok(frames)
    }
    fn finish(self) -> Result<String, Error> {
        if !self.pending.is_empty() {
            return Err(Error::Protocol);
        }
        self.terminal.ok_or(Error::Protocol)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[derive(Default)]
    struct FixtureState {
        ready: AtomicBool,
        sessions: AtomicUsize,
        attempts: AtomicUsize,
        accepted: AtomicUsize,
        cancelled: Notify,
        released: Notify,
    }

    async fn fixture_service(
        warming: bool,
        expire_first: bool,
        truncate: bool,
    ) -> (String, Arc<FixtureState>, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let state = Arc::new(FixtureState::default());
        state.ready.store(!warming, Ordering::Release);
        let observed = state.clone();
        let server = tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    return;
                };
                let state = observed.clone();
                tokio::spawn(async move {
                    let mut request = Vec::new();
                    let (headers_end, length) = loop {
                        let mut bytes = [0_u8; 1024];
                        let count = socket.read(&mut bytes).await.unwrap();
                        if count == 0 {
                            return;
                        }
                        request.extend_from_slice(&bytes[..count]);
                        assert!(request.len() <= 16_384);
                        if let Some(end) = request.windows(4).position(|s| s == b"\r\n\r\n") {
                            let headers = String::from_utf8_lossy(&request[..end]);
                            let length = headers
                                .lines()
                                .find_map(|line| {
                                    line.to_ascii_lowercase()
                                        .strip_prefix("content-length:")
                                        .map(|value| value.trim().parse::<usize>().unwrap())
                                })
                                .unwrap_or(0);
                            break (end + 4, length);
                        }
                    };
                    while request.len() < headers_end + length {
                        let mut bytes = [0_u8; 1024];
                        let count = socket.read(&mut bytes).await.unwrap();
                        if count == 0 {
                            return;
                        }
                        request.extend_from_slice(&bytes[..count]);
                        assert!(request.len() <= 16_384);
                    }
                    let path = String::from_utf8_lossy(&request[..headers_end])
                        .split_whitespace()
                        .nth(1)
                        .unwrap()
                        .to_owned();
                    let body: serde_json::Value = if length == 0 {
                        serde_json::json!({})
                    } else {
                        serde_json::from_slice(&request[headers_end..headers_end + length]).unwrap()
                    };
                    let mut status = "200 OK";
                    let mut content_type = "application/json";
                    let output = match path.as_str() {
                        "/v1/synthesis/capabilities" => {
                            if !state.ready.load(Ordering::Acquire) {
                                serde_json::to_vec(&serde_json::json!({"protocol_version":1,"ready":false,"default_voice":null})).unwrap()
                            } else {
                                serde_json::to_vec(&serde_json::json!({"protocol_version":1,"ready":true,"backend":"synthetic","default_voice":"synthetic-a","voices":[{"id":"synthetic-a","name":"Synthetic A","aliases":[],"default":true}],"sample_rate_hz":24000,"channels":1,"incremental_audio":false})).unwrap()
                            }
                        }
                        "/v1/synthesis/session" => {
                            let count = state.sessions.fetch_add(1, Ordering::AcqRel) + 1;
                            state.ready.store(true, Ordering::Release);
                            serde_json::to_vec(
                                &serde_json::json!({"session_id":format!("{count:064x}")}),
                            )
                            .unwrap()
                        }
                        "/v1/synthesis/stream" => {
                            let attempt = state.attempts.fetch_add(1, Ordering::AcqRel);
                            if expire_first && attempt == 0 {
                                status = "404 Not Found";
                                b"{}".to_vec()
                            } else {
                                state.accepted.fetch_add(1, Ordering::AcqRel);
                                content_type = "application/vnd.neural-tts.pcm";
                                let mut output = frame(1, &serde_json::to_vec(&serde_json::json!({"protocol_version":1,"job_id":body["job_id"],"sample_rate_hz":24000,"channels":1,"format":"f32le"})).unwrap());
                                output.extend(frame(2, &0.25_f32.to_le_bytes()));
                                if !truncate {
                                    output
                                        .extend(frame(3, br#"{"status":"completed","samples":1}"#));
                                }
                                output
                            }
                        }
                        "/v1/synthesis/cancel" => {
                            state.cancelled.notify_one();
                            b"{}".to_vec()
                        }
                        "/v1/synthesis/release" => {
                            state.released.notify_one();
                            b"{}".to_vec()
                        }
                        _ => {
                            status = "404 Not Found";
                            b"{}".to_vec()
                        }
                    };
                    let header = format!(
                        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        output.len()
                    );
                    let _ = socket.write_all(header.as_bytes()).await;
                    let _ = socket.write_all(&output).await;
                });
            }
        });
        (endpoint, state, server)
    }

    #[tokio::test]
    async fn complete_client_handshake_recovers_warming_and_expired_sessions_without_replaying_jobs(
    ) {
        let (endpoint, state, server) = fixture_service(true, true, false).await;
        let client = NeuralTtsClient::connect(&NeuralTtsOptions {
            endpoint,
            start_command: vec![],
        })
        .await
        .unwrap();
        let audio = client
            .collect("Synthetic fixture.", "", Arc::new(Cancellation::default()))
            .await
            .unwrap();
        assert_eq!(audio.samples, [0.25]);
        assert_eq!(audio.sample_rate_hz, 24000);
        assert_eq!(state.sessions.load(Ordering::Acquire), 2);
        assert_eq!(state.attempts.load(Ordering::Acquire), 2);
        assert_eq!(state.accepted.load(Ordering::Acquire), 1);
        client.close();
        tokio::time::timeout(Duration::from_secs(1), state.released.notified())
            .await
            .unwrap();
        server.abort();
    }

    #[tokio::test]
    async fn truncated_real_http_body_cancels_owned_job_and_never_reports_success() {
        let (endpoint, state, server) = fixture_service(false, false, true).await;
        let client = NeuralTtsClient::connect(&NeuralTtsOptions {
            endpoint,
            start_command: vec![],
        })
        .await
        .unwrap();
        assert!(matches!(
            client
                .collect("Synthetic fixture.", "", Arc::new(Cancellation::default()))
                .await,
            Err(Error::Protocol)
        ));
        tokio::time::timeout(Duration::from_secs(1), state.cancelled.notified())
            .await
            .unwrap();
        client.close();
        tokio::time::timeout(Duration::from_secs(1), state.released.notified())
            .await
            .unwrap();
        server.abort();
    }

    #[tokio::test]
    #[ignore = "requires explicitly authorized running local Neural TTS service"]
    async fn real_service_synthetic_qualification() {
        let endpoint = std::env::var("NEURAL_TTS_TEST_ENDPOINT").expect("explicit local endpoint");
        let client = NeuralTtsClient::connect(&NeuralTtsOptions {
            endpoint,
            start_command: vec![],
        })
        .await
        .unwrap();
        assert!(!client.capabilities().voices.is_empty());
        let audio = client
            .collect(
                "This is a synthetic speech qualification. One plus one equals two.",
                "",
                Arc::new(Cancellation::default()),
            )
            .await
            .unwrap();
        assert_eq!(audio.sample_rate_hz, client.capabilities().sample_rate_hz);
        assert!(audio.samples.iter().all(|sample| sample.is_finite()));
        assert!(audio.samples.iter().any(|sample| *sample != 0.0));
        eprintln!(
            "synthetic neural qualification passed: voices={} rate={} samples={}",
            client.capabilities().voices.len(),
            audio.sample_rate_hz,
            audio.samples.len()
        );
        client.close();
    }
    fn frame(kind: u8, data: &[u8]) -> Vec<u8> {
        let mut bytes = vec![kind];
        bytes.extend_from_slice(&(data.len() as u32).to_le_bytes());
        bytes.extend_from_slice(data);
        bytes
    }
    fn fixture() -> Vec<u8> {
        let mut bytes = frame(1, br#"{"protocol_version":1,"job_id":"synthetic","sample_rate_hz":24000,"channels":1,"format":"f32le"}"#);
        bytes.extend(frame(2, &0.25_f32.to_le_bytes()));
        bytes.extend(frame(3, br#"{"status":"completed","samples":1}"#));
        bytes
    }
    #[test]
    fn every_network_split_preserves_format_audio_and_terminal() {
        let bytes = fixture();
        for split in 0..=bytes.len() {
            let mut decoder = Decoder::new("synthetic");
            let mut count = decoder.feed(&bytes[..split]).unwrap().len();
            count += decoder.feed(&bytes[split..]).unwrap().len();
            assert_eq!(count, 3);
            assert_eq!(decoder.finish().unwrap(), "completed");
        }
    }
    #[test]
    fn truncation_wrong_identity_nonfinite_and_post_terminal_are_rejected() {
        let bytes = fixture();
        for end in 0..bytes.len() {
            let mut decoder = Decoder::new("synthetic");
            assert!(decoder.feed(&bytes[..end]).is_err() || decoder.finish().is_err());
        }
        assert!(Decoder::new("another").feed(&bytes).is_err());
        let metadata = frame(1, br#"{"protocol_version":1,"job_id":"synthetic","sample_rate_hz":24000,"channels":1,"format":"f32le"}"#);
        let mut decoder = Decoder::new("synthetic");
        decoder.feed(&metadata).unwrap();
        assert!(decoder.feed(&frame(2, &f32::NAN.to_le_bytes())).is_err());
        let mut decoder = Decoder::new("synthetic");
        decoder.feed(&bytes).unwrap();
        assert!(decoder.feed(&frame(2, &0.0_f32.to_le_bytes())).is_err());
    }
    #[test]
    fn endpoints_cannot_escape_loopback_or_hide_credentials_and_routes() {
        for endpoint in [
            "http://127.0.0.1:8765",
            "http://localhost:8765/",
            "http://[::1]:8765",
        ] {
            assert!(validate_endpoint(endpoint).is_ok(), "{endpoint}");
        }
        for endpoint in [
            "https://localhost:8765",
            "http://example.invalid",
            "http://127.0.0.1/path",
            "http://user@localhost",
            "http://localhost?key=value",
            "http://localhost#fragment",
        ] {
            assert_eq!(validate_endpoint(endpoint), Err(Error::Configuration));
        }
    }
    #[tokio::test]
    async fn cancellation_before_notifier_registration_is_not_lost() {
        let cancel = Cancellation::default();
        cancel.cancel();
        tokio::time::timeout(Duration::from_millis(50), cancel.notified())
            .await
            .unwrap();
    }
}
