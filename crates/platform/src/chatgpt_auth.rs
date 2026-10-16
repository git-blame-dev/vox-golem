//! Native ChatGPT device-code ownership. Only the four public display fields in
//! `AuthStatus` may cross a UI bridge. `export_access` is for trusted native peers.
//!
//! The caller supplies a dedicated application-owned path, never an imported
//! OpenCode file. Unix storage uses 0600 atomic files in directories which are
//! not writable by other users (new directories are 0700).
//! On Windows the shell must supply its user-private application-data directory
//! (inherited per-user DACL); this module does not configure Windows ACLs.
//! Polling and refresh run on one continuously driven native runtime, independent
//! of UI/request future cancellation. No account requests occur until requested.

use crate::custom_openai::{CustomOpenAiClient, CustomOpenAiConfig, CustomOpenAiError};
use futures_util::StreamExt;
use reqwest::{Client, StatusCode};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::fmt::{Display, Formatter};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::task::AbortHandle;

mod storage;
use storage::{read_file, write_file, Credential, ManagedFile, MANAGED_VERSION};
#[cfg(test)]
#[path = "chatgpt_auth_tests.rs"]
mod tests;

const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const VERIFICATION_URL: &str = "https://auth.openai.com/codex/device";
const CALLBACK_URL: &str = "https://auth.openai.com/deviceauth/callback";
const MAX_PATHS: usize = 16;
const MAX_BODY: usize = 65_536;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);
const FLOW_TIMEOUT: Duration = Duration::from_secs(15 * 60);
const EXPIRY_MARGIN: u64 = 60_000;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    SignedOut,
    Starting,
    Pending,
    Connected,
    Error,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthStatus {
    pub phase: Phase,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verification_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl AuthStatus {
    fn phase(phase: Phase) -> Self {
        Self {
            phase,
            user_code: None,
            verification_url: None,
            error: None,
        }
    }

    fn pending(code: String) -> Self {
        Self {
            user_code: Some(code),
            verification_url: Some(VERIFICATION_URL.into()),
            ..Self::phase(Phase::Pending)
        }
    }

    fn failed(error: AuthError) -> Self {
        Self {
            error: Some(error.to_string()),
            ..Self::phase(Phase::Error)
        }
    }

    fn active(&self) -> bool {
        matches!(self.phase, Phase::Starting | Phase::Pending)
    }
}

/// Deliberately categorical: neither Display nor Debug contains provider bodies,
/// paths, account metadata, transport details, or credential material.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AuthError {
    InvalidAction,
    Storage,
    Conflict,
    Busy,
    Transport,
    Timeout,
    Rejected,
    ReconnectRequired,
    Unavailable,
    InvalidResponse,
    Cancelled,
}

impl Display for AuthError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::InvalidAction => "Unknown ChatGPT sign-in action",
            Self::Storage => "ChatGPT private credential storage is unavailable",
            Self::Conflict => "ChatGPT credentials changed or belong to another source",
            Self::Busy => "ChatGPT authentication is busy; try again",
            Self::Transport => "ChatGPT sign-in connection failed",
            Self::Timeout => "ChatGPT sign-in timed out; start again",
            Self::Rejected => {
                "ChatGPT sign-in was rejected; enable device-code access or sign in again"
            }
            Self::ReconnectRequired => "ChatGPT session expired; sign in again",
            Self::Unavailable => "ChatGPT is temporarily unavailable; try again",
            Self::InvalidResponse => "ChatGPT sign-in returned an invalid response",
            Self::Cancelled => "ChatGPT sign-in was cancelled",
        })
    }
}

impl std::error::Error for AuthError {}

struct State {
    generation: u64,
    status: AuthStatus,
    polling: Option<AbortHandle>,
}

struct Entry {
    path: PathBuf,
    state: Mutex<State>,
    refresh: tokio::sync::Mutex<Result<(), AuthError>>,
    refresh_serial: AtomicU64,
}

struct Engine {
    entries: Mutex<HashMap<PathBuf, Arc<Entry>>>,
    client: Client,
    issuer: String,
    revoke: String,
    poll_safety: Duration,
    flow_timeout: Duration,
}

struct Owner {
    runtime: tokio::runtime::Runtime,
    engine: Arc<Engine>,
}

fn owner() -> Result<&'static Owner, AuthError> {
    static OWNER: OnceLock<Result<Owner, AuthError>> = OnceLock::new();
    OWNER
        .get_or_init(|| {
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(1)
                .max_blocking_threads(2)
                .thread_name("chatgpt-auth")
                .enable_all()
                .build()
                .map_err(|_| AuthError::Transport)?;
            let client = CustomOpenAiClient::transport(&CustomOpenAiConfig::default())
                .map_err(|_| AuthError::Transport)?;
            Ok(Owner {
                runtime,
                engine: Arc::new(Engine {
                    entries: Mutex::new(HashMap::new()),
                    client,
                    issuer: "https://auth.openai.com".into(),
                    // Published OpenID discovery revocation_endpoint; do not guess /oauth/revoke.
                    revoke: "https://auth.openai.com/api/accounts/oauth/revoke".into(),
                    poll_safety: Duration::from_secs(3),
                    flow_timeout: FLOW_TIMEOUT,
                }),
            })
        })
        .as_ref()
        .map_err(|error| *error)
}

/// `start` is idempotent while starting/pending/connected and returns immediately.
/// `status` recovers connected state from the managed file after process restart.
/// `cancel` stops only pending authorization, not a shared token refresh.
/// `sign_out` fences pending writes, clears only owned storage, then revokes best effort.
pub async fn command(path: &Path, action: &str) -> Result<AuthStatus, AuthError> {
    if !matches!(action, "start" | "status" | "cancel" | "sign_out") {
        return Err(AuthError::InvalidAction);
    }
    let owner = owner()?;
    let engine = owner.engine.clone();
    let path = path.to_owned();
    let action = action.to_owned();
    owner
        .runtime
        .spawn(async move { engine.command(&path, &action).await })
        .await
        .map_err(|_| AuthError::Transport)?
}

pub fn is_managed(path: &Path) -> bool {
    read_file(path)
        .ok()
        .flatten()
        .is_some_and(|bytes| storage::owned(&bytes))
}

pub fn can_refresh(path: &Path) -> bool {
    read_file(path)
        .ok()
        .flatten()
        .is_some_and(|bytes| renewable_contents(&bytes))
}

pub(crate) fn renewable_contents(contents: &[u8]) -> bool {
    storage::parse(contents).is_ok_and(|file| file.openai.refresh.is_some())
}

pub(crate) fn marked_contents(contents: &[u8]) -> bool {
    storage::marked(contents)
}

/// Refresh only app-managed credentials. Legacy/imported files are never changed.
/// Refresh is single-flight per normalized path; dropping one request's future
/// cannot cancel refresh needed by another request or native peer.
pub async fn ensure_fresh(path: &Path) -> Result<(), CustomOpenAiError> {
    fresh(path).await.map_err(|error| match error {
        AuthError::Timeout => CustomOpenAiError::Timeout,
        AuthError::Transport => CustomOpenAiError::Transport,
        AuthError::Rejected | AuthError::ReconnectRequired => CustomOpenAiError::Unauthorized,
        AuthError::Unavailable => CustomOpenAiError::Transport,
        _ => CustomOpenAiError::AuthUnavailable,
    })
}

async fn fresh(path: &Path) -> Result<(), AuthError> {
    let owner = owner()?;
    let engine = owner.engine.clone();
    let path = path.to_owned();
    owner
        .runtime
        .spawn(async move { engine.ensure_fresh(&path).await })
        .await
        .map_err(|_| AuthError::Transport)?
}

/// Access-only legacy OpenAI envelope for an authorized native peer. Never expose
/// this result to a WebView. No refresh or ID token is included, even when fresh.
pub async fn export_access(path: &Path) -> Result<Option<String>, AuthError> {
    fresh(path).await?;
    let path = path.to_owned();
    disk(move || {
        let Some(bytes) = read_file(&path)? else {
            return Ok(None);
        };
        let Ok(file) = storage::parse(&bytes) else {
            return Ok(None);
        };
        if file.openai.expires <= now_ms()?.saturating_add(EXPIRY_MARGIN) {
            return Err(AuthError::Rejected);
        }
        let mut value =
            json!({"type":"oauth", "access":file.openai.access, "expires":file.openai.expires});
        if let Some(account) = file.openai.account_id {
            value["accountId"] = json!(account);
        }
        Ok(Some(
            json!({"voxgolemChatgpt":MANAGED_VERSION,"openai":value}).to_string(),
        ))
    })
    .await
}

async fn disk<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, AuthError> + Send + 'static,
) -> Result<T, AuthError> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|_| AuthError::Storage)?
}

async fn path_lock(path: &Path) -> Result<std::fs::File, AuthError> {
    let deadline = tokio::time::Instant::now() + REQUEST_TIMEOUT + Duration::from_secs(10);
    loop {
        let path = path.to_owned();
        if let Some(file) = disk(move || storage::try_lock(&path)).await? {
            return Ok(file);
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(AuthError::Busy);
        }
        // Never block either an executor thread or the bounded blocking pool on
        // another process's network operation. A blocking File::lock here can
        // starve the pool required to finish that operation's own persistence.
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

async fn refresh_snapshot(
    entry: Arc<Entry>,
) -> Result<Option<(u64, Vec<u8>, ManagedFile)>, AuthError> {
    disk(move || {
        let state = entry.state.lock().map_err(|_| AuthError::Storage)?;
        let Some(bytes) = storage::refresh_file(&entry.path)? else {
            return Ok(None);
        };
        let Ok(file) = storage::parse(&bytes) else {
            return Ok(None);
        };
        if file.openai.refresh.is_none()
            || file.openai.expires > now_ms()?.saturating_add(EXPIRY_MARGIN)
        {
            return Ok(None);
        }
        Ok(Some((state.generation, bytes, file)))
    })
    .await
}

fn fence(state: &mut State) {
    state.generation = state.generation.wrapping_add(1);
    if let Some(task) = state.polling.take() {
        task.abort();
    }
    state.status = AuthStatus::phase(Phase::SignedOut);
}

impl Engine {
    async fn entry(self: &Arc<Self>, path: &Path) -> Result<Arc<Entry>, AuthError> {
        let engine = self.clone();
        let path = path.to_owned();
        disk(move || {
            let path = storage::normalized_path(&path)?;
            let mut entries = engine.entries.lock().map_err(|_| AuthError::Storage)?;
            if let Some(entry) = entries.get(&path) {
                return Ok(entry.clone());
            }
            // Entries with work in flight have other strong owners and are never evicted.
            if entries.len() >= MAX_PATHS {
                entries.retain(|_, entry| Arc::strong_count(entry) > 1);
            }
            if entries.len() >= MAX_PATHS {
                return Err(AuthError::Busy);
            }
            let entry = Arc::new(Entry {
                path: path.clone(),
                state: Mutex::new(State {
                    generation: 0,
                    status: AuthStatus::phase(Phase::SignedOut),
                    polling: None,
                }),
                refresh: tokio::sync::Mutex::new(Ok(())),
                refresh_serial: AtomicU64::new(0),
            });
            entries.insert(path, entry.clone());
            Ok(entry)
        })
        .await
    }

    async fn command(self: &Arc<Self>, path: &Path, action: &str) -> Result<AuthStatus, AuthError> {
        let entry = self.entry(path).await?;
        if action == "sign_out" {
            // Fence this process immediately, even if another process owns an
            // in-flight refresh. The durable sign-out completes under its lock.
            let cancelling = entry.clone();
            disk(move || {
                let mut state = cancelling.state.lock().map_err(|_| AuthError::Storage)?;
                if read_file(&cancelling.path)?
                    .as_deref()
                    .is_some_and(|bytes| !storage::owned(bytes))
                {
                    return Err(AuthError::Conflict);
                }
                fence(&mut state);
                Ok(())
            })
            .await?;
            let file_lock = path_lock(&entry.path).await?;
            let cleared = entry.clone();
            let refresh = disk(move || {
                let _file_lock = file_lock;
                let mut state = cleared.state.lock().map_err(|_| AuthError::Storage)?;
                fence(&mut state);
                let bytes = read_file(&cleared.path)?;
                if bytes.as_deref().is_some_and(|bytes| !storage::owned(bytes)) {
                    return Err(AuthError::Conflict);
                }
                let refresh = bytes
                    .as_deref()
                    .and_then(|bytes| storage::parse(bytes).ok())
                    .and_then(|file| file.openai.refresh);
                storage::write_tombstone(&cleared.path)?;
                Ok(refresh)
            })
            .await?;
            if let Some(refresh) = refresh {
                let engine = self.clone();
                tokio::spawn(async move {
                    engine.revoke(&refresh).await;
                });
            }
            return Ok(AuthStatus::phase(Phase::SignedOut));
        }
        let engine = self.clone();
        let action = action.to_owned();
        let runtime = tokio::runtime::Handle::current();
        disk(move || {
            let mut state = entry.state.lock().map_err(|_| AuthError::Storage)?;
            if action == "cancel" && state.status.active() {
                fence(&mut state);
            }
            if state.status.active() {
                return Ok(state.status.clone());
            }
            let existing = read_file(&entry.path)?;
            let connected = existing.as_deref().is_some_and(|bytes| {
                storage::parse(bytes).is_ok_and(|file| {
                    file.openai.refresh.is_some()
                        || now_ms().is_ok_and(|now| {
                            file.openai.expires > now.saturating_add(EXPIRY_MARGIN)
                        })
                })
            });
            if connected {
                return Ok(AuthStatus::phase(Phase::Connected));
            }
            if action != "start" {
                if state.status.phase == Phase::Connected {
                    state.status = AuthStatus::phase(Phase::SignedOut);
                }
                return Ok(state.status.clone());
            }
            if existing
                .as_deref()
                .is_some_and(|bytes| !storage::tombstone(bytes))
            {
                return Err(AuthError::Conflict);
            }
            state.generation = state.generation.wrapping_add(1);
            let generation = state.generation;
            state.status = AuthStatus::phase(Phase::Starting);
            let task_entry = entry.clone();
            let task_engine = engine.clone();
            let task = runtime.spawn(async move {
                task_engine.run_flow(task_entry, generation, existing).await;
            });
            state.polling = Some(task.abort_handle());
            Ok(state.status.clone())
        })
        .await
    }

    async fn run_flow(
        self: Arc<Self>,
        entry: Arc<Entry>,
        generation: u64,
        original: Option<Vec<u8>>,
    ) {
        let deadline = tokio::time::Instant::now() + self.flow_timeout;
        let result = tokio::time::timeout_at(
            deadline,
            self.flow(entry.clone(), generation, original, deadline),
        )
        .await
        .unwrap_or(Err(AuthError::Timeout));
        let _ = disk(move || {
            let mut state = entry.state.lock().map_err(|_| AuthError::Storage)?;
            if state.generation == generation {
                state.polling = None;
                if result.is_err() {
                    // timeout/cancellation can drop a spawn_blocking join future;
                    // fence any publication which has not acquired this lock yet.
                    state.generation = state.generation.wrapping_add(1);
                }
                state.status = match result {
                    Ok(()) => AuthStatus::phase(Phase::Connected),
                    Err(error) => AuthStatus::failed(error),
                };
            }
            Ok(())
        })
        .await;
    }

    async fn flow(
        &self,
        entry: Arc<Entry>,
        generation: u64,
        original: Option<Vec<u8>>,
        deadline: tokio::time::Instant,
    ) -> Result<(), AuthError> {
        let device: DeviceCode = self
            .json(
                self.client
                    .post(format!("{}/api/accounts/deviceauth/usercode", self.issuer))
                    .json(&json!({"client_id":CLIENT_ID})),
            )
            .await?;
        if !safe_opaque(&device.device_auth_id, 4096)
            || device.user_code.is_empty()
            || device.user_code.len() > 64
            || !device
                .user_code
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        {
            return Err(AuthError::InvalidResponse);
        }
        let interval = interval(&device.interval)? + self.poll_safety;
        {
            let mut state = entry.state.lock().map_err(|_| AuthError::Storage)?;
            if state.generation != generation {
                return Err(AuthError::Cancelled);
            }
            state.status = AuthStatus::pending(device.user_code.clone());
        }
        let code: Authorization = loop {
            // Respect the server interval on the first poll too; no busy polling.
            tokio::time::sleep(interval).await;
            let response = self
                .client
                .post(format!("{}/api/accounts/deviceauth/token", self.issuer))
                .timeout(REQUEST_TIMEOUT)
                .json(&json!({"device_auth_id":device.device_auth_id,"user_code":device.user_code}))
                .send()
                .await
                .map_err(transport_error)?;
            if matches!(
                response.status(),
                StatusCode::FORBIDDEN | StatusCode::NOT_FOUND
            ) {
                continue;
            }
            break response_json(response).await?;
        };
        if !safe_opaque(&code.authorization_code, 4096) || !safe_opaque(&code.code_verifier, 4096) {
            return Err(AuthError::InvalidResponse);
        }
        let tokens: Tokens = self
            .json(form(
                self.client.post(format!("{}/oauth/token", self.issuer)),
                &[
                    ("grant_type", "authorization_code"),
                    ("client_id", CLIENT_ID),
                    ("code", code.authorization_code.as_str()),
                    ("code_verifier", code.code_verifier.as_str()),
                    ("redirect_uri", CALLBACK_URL),
                ],
            )?)
            .await?;
        let file = tokens.credential(None)?;
        let file_lock = path_lock(&entry.path).await?;
        disk(move || {
            let _file_lock = file_lock;
            let state = entry.state.lock().map_err(|_| AuthError::Storage)?;
            if tokio::time::Instant::now() >= deadline {
                return Err(AuthError::Timeout);
            }
            if state.generation != generation {
                return Err(AuthError::Cancelled);
            }
            if read_file(&entry.path)? != original {
                return Err(AuthError::Conflict);
            }
            write_file(&entry.path, &file)
        })
        .await
    }

    async fn ensure_fresh(self: &Arc<Self>, path: &Path) -> Result<(), AuthError> {
        let entry = self.entry(path).await?;
        let admitted = entry.refresh_serial.load(Ordering::Acquire);
        let mut outcome = entry.refresh.lock().await;
        if entry.refresh_serial.load(Ordering::Acquire) != admitted {
            return *outcome;
        }
        // Waiters share failure as well as success. In particular, cancelled
        // prompt waiters must not each become another background HTTP retry.
        let result = self.refresh_once(entry.clone()).await;
        *outcome = result;
        entry.refresh_serial.fetch_add(1, Ordering::Release);
        result
    }

    async fn refresh_once(&self, entry: Arc<Entry>) -> Result<(), AuthError> {
        if refresh_snapshot(entry.clone()).await?.is_none() {
            return Ok(());
        }
        // One OS lock spans re-read, remote rotation and durable commit. This
        // serializes independent VoxGolem processes, not just Tokio callers.
        let file_lock = path_lock(&entry.path).await?;
        let Some((generation, original, old)) = refresh_snapshot(entry.clone()).await? else {
            return Ok(());
        };
        let result = self
            .refresh_tokens(form(
                self.client.post(format!("{}/oauth/token", self.issuer)),
                &[
                    ("grant_type", "refresh_token"),
                    ("client_id", CLIENT_ID),
                    (
                        "refresh_token",
                        old.openai
                            .refresh
                            .as_deref()
                            .ok_or(AuthError::InvalidResponse)?,
                    ),
                ],
            )?)
            .await;
        let tokens = match result {
            Ok(tokens) => tokens,
            Err(AuthError::ReconnectRequired) => {
                let entry = entry.clone();
                disk(move || {
                    let _file_lock = file_lock;
                    let mut state = entry.state.lock().map_err(|_| AuthError::Storage)?;
                    if state.generation != generation {
                        return Err(AuthError::Cancelled);
                    }
                    if read_file(&entry.path)?.as_deref() != Some(original.as_slice()) {
                        return Err(AuthError::Conflict);
                    }
                    storage::write_tombstone(&entry.path)?;
                    fence(&mut state);
                    state.status = AuthStatus::failed(AuthError::ReconnectRequired);
                    Ok(())
                })
                .await?;
                return Err(AuthError::ReconnectRequired);
            }
            Err(error) => return Err(error),
        };
        let file = tokens.credential(Some(old.openai))?;
        disk(move || {
            let _file_lock = file_lock;
            let state = entry.state.lock().map_err(|_| AuthError::Storage)?;
            if state.generation != generation {
                return Err(AuthError::Cancelled);
            }
            if read_file(&entry.path)?.as_deref() != Some(original.as_slice()) {
                return Err(AuthError::Conflict);
            }
            write_file(&entry.path, &file)
        })
        .await
    }

    async fn json<T: DeserializeOwned>(
        &self,
        request: reqwest::RequestBuilder,
    ) -> Result<T, AuthError> {
        response_json(
            request
                .timeout(REQUEST_TIMEOUT)
                .send()
                .await
                .map_err(transport_error)?,
        )
        .await
    }

    async fn refresh_tokens(&self, request: reqwest::RequestBuilder) -> Result<Tokens, AuthError> {
        let response = request
            .timeout(REQUEST_TIMEOUT)
            .send()
            .await
            .map_err(transport_error)?;
        let status = response.status();
        if status.is_success() {
            return response_json(response).await;
        }
        if status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error() {
            return Err(AuthError::Unavailable);
        }
        if matches!(
            status,
            StatusCode::BAD_REQUEST | StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN
        ) {
            let bytes = response_bytes(response).await?;
            if terminal_refresh(&bytes) {
                return Err(AuthError::ReconnectRequired);
            }
        }
        Err(AuthError::Rejected)
    }

    async fn revoke(&self, refresh: &str) {
        // Local sign-out is already complete. Never surface raw remote errors or
        // delay sign-out for this best-effort RFC 7009 operation.
        let request = form(
            self.client
                .post(&self.revoke)
                .timeout(Duration::from_secs(3)),
            &[
                ("client_id", CLIENT_ID),
                ("token", refresh),
                ("token_type_hint", "refresh_token"),
            ],
        );
        if let Ok(request) = request {
            let _ = request.send().await;
        }
    }
}

fn form(
    request: reqwest::RequestBuilder,
    pairs: &[(&str, &str)],
) -> Result<reqwest::RequestBuilder, AuthError> {
    // Reuse URL's standard form-urlencoder without enabling an additional
    // serialization dependency. This temporary URL is never requested or logged.
    let encoded = reqwest::Url::parse_with_params("https://auth.openai.com", pairs)
        .map_err(|_| AuthError::InvalidResponse)?;
    Ok(request
        .header(
            reqwest::header::CONTENT_TYPE,
            "application/x-www-form-urlencoded",
        )
        .body(encoded.query().unwrap_or_default().to_owned()))
}

async fn response_json<T: DeserializeOwned>(response: reqwest::Response) -> Result<T, AuthError> {
    if !response.status().is_success() {
        return Err(AuthError::Rejected);
    }
    let bytes = response_bytes(response).await?;
    serde_json::from_slice(&bytes).map_err(|_| AuthError::InvalidResponse)
}

async fn response_bytes(response: reqwest::Response) -> Result<Vec<u8>, AuthError> {
    if response
        .content_length()
        .is_some_and(|length| length > MAX_BODY as u64)
    {
        return Err(AuthError::InvalidResponse);
    }
    let mut bytes = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(transport_error)?;
        if chunk.len() > MAX_BODY.saturating_sub(bytes.len()) {
            return Err(AuthError::InvalidResponse);
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

fn terminal_refresh(bytes: &[u8]) -> bool {
    let Ok(value) = serde_json::from_slice::<Value>(bytes) else {
        return false;
    };
    // Only exact public error codes affect durable state. Never inspect or expose
    // error_description/message, and never infer expiry from arbitrary prose.
    let code = value
        .get("error")
        .and_then(Value::as_str)
        .or_else(|| value.get("error")?.get("code")?.as_str())
        .or_else(|| value.get("code")?.as_str());
    matches!(
        code,
        Some(
            "invalid_grant"
                | "invalid_refresh_token"
                | "token_expired"
                | "refresh_token_expired"
                | "refresh_token_invalidated"
                | "refresh_token_reused"
        )
    )
}

fn transport_error(error: reqwest::Error) -> AuthError {
    if error.is_timeout() {
        AuthError::Timeout
    } else {
        AuthError::Transport
    }
}

fn now_ms() -> Result<u64, AuthError> {
    u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| AuthError::Storage)?
            .as_millis(),
    )
    .map_err(|_| AuthError::Storage)
}

fn safe_opaque(value: &str, max: usize) -> bool {
    !value.is_empty() && value.len() <= max && value.bytes().all(|byte| byte.is_ascii_graphic())
}

#[derive(Deserialize)]
struct DeviceCode {
    device_auth_id: String,
    #[serde(alias = "usercode")]
    user_code: String,
    #[serde(default)]
    interval: Value,
}

fn interval(value: &Value) -> Result<Duration, AuthError> {
    let seconds = match value {
        Value::Null => 5,
        Value::String(value) => value
            .trim()
            .parse::<u64>()
            .map_err(|_| AuthError::InvalidResponse)?,
        _ => value.as_u64().ok_or(AuthError::InvalidResponse)?,
    };
    if seconds > 900 {
        return Err(AuthError::InvalidResponse);
    }
    Ok(Duration::from_secs(seconds.max(1)))
}

#[derive(Deserialize)]
struct Authorization {
    authorization_code: String,
    code_verifier: String,
}

#[derive(Deserialize)]
struct Tokens {
    access_token: String,
    refresh_token: Option<String>,
    id_token: Option<String>,
    expires_in: Option<u64>,
}

impl Tokens {
    fn credential(self, old: Option<Credential>) -> Result<ManagedFile, AuthError> {
        let refresh = self
            .refresh_token
            .or_else(|| old.as_ref().and_then(|old| old.refresh.clone()))
            .ok_or(AuthError::InvalidResponse)?;
        let expires_in = self.expires_in.unwrap_or(3600);
        if !safe_opaque(&self.access_token, 24_000)
            || !safe_opaque(&refresh, 24_000)
            || !(61..=30 * 24 * 3600).contains(&expires_in)
        {
            return Err(AuthError::InvalidResponse);
        }
        // Claims are unverified routing metadata, NEVER an authorization decision.
        // Only the documented account claim is retained; ID tokens are discarded.
        let account_id = self
            .id_token
            .as_deref()
            .and_then(account_claim)
            .or_else(|| account_claim(&self.access_token))
            .or_else(|| old.and_then(|old| old.account_id));
        Ok(ManagedFile {
            voxgolem_chatgpt: MANAGED_VERSION,
            openai: Credential {
                access: self.access_token,
                refresh: Some(refresh),
                expires: now_ms()?.saturating_add(expires_in * 1000),
                account_id,
            },
        })
    }
}

fn account_claim(token: &str) -> Option<String> {
    let payload = token.split('.').nth(1)?;
    if payload.len() > 32_768 {
        return None;
    }
    // Bounded base64url decoding for optional routing metadata only. No new
    // dependency is needed for this non-cryptographic operation.
    let mut decoded = Vec::with_capacity(payload.len() * 3 / 4);
    let mut bits = 0_u32;
    let mut count = 0;
    for byte in payload.bytes() {
        let value = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'-' => 62,
            b'_' => 63,
            _ => return None,
        };
        bits = (bits << 6) | u32::from(value);
        count += 6;
        if count >= 8 {
            count -= 8;
            decoded.push((bits >> count) as u8);
        }
    }
    let claims: Value = serde_json::from_slice(&decoded).ok()?;
    let account = claims
        .get("chatgpt_account_id")
        .or_else(|| {
            claims
                .get("https://api.openai.com/auth")?
                .get("chatgpt_account_id")
        })?
        .as_str()?;
    safe_opaque(account, 256).then(|| account.to_owned())
}
