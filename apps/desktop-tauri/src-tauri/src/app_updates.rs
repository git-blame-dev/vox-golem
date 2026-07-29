use base64::Engine;
use futures_util::StreamExt;
use minisign_verify::{PublicKey, Signature};
use reqwest::header::{HeaderValue, ACCEPT};
use serde::{Deserialize, Serialize};
use std::future::Future;
use std::path::Path;
use std::sync::Mutex;
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::time::Instant;

const UPDATE_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const UPDATE_DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(30 * 60);
const MAX_UPDATE_METADATA_BYTES: u64 = 64 * 1024;
const MAX_LINUX_UPDATE_BYTES: u64 = 300_000_000;
const MAX_WINDOWS_UPDATE_BYTES: u64 = 250_000_000;
const MAX_UPDATE_NOTES_CHARS: usize = 2_048;
const UPDATE_CHANNEL_UNAVAILABLE_REASON: &str = "No published updates yet.";
const UPDATE_RELEASE_HOST: &str = "github.com";
const UPDATE_RELEASE_PATH_PREFIX: &str = "/git-blame-dev/vox-golem/releases/download";
const UPDATE_PROGRESS_INTERVAL: Duration = Duration::from_millis(100);
const UPDATE_PROGRESS_BYTES: u64 = 1024 * 1024;
#[cfg(any(target_os = "windows", test))]
const WINDOWS_STAGING_PREFIX: &str = "vox-golem-update-";
#[cfg(any(target_os = "windows", test))]
const WINDOWS_STAGING_MARKER: &str = ".vox-golem-update";
#[cfg(any(target_os = "windows", test))]
const WINDOWS_STAGING_MARKER_CONTENT: &str = "vox-golem-update-v1\n";
#[cfg(any(target_os = "windows", test))]
const WINDOWS_STAGING_MAX_SCAN: usize = 128;
#[cfg(target_os = "windows")]
const WINDOWS_STAGING_MAX_AGE: Duration = Duration::from_secs(24 * 60 * 60);
#[cfg(target_os = "windows")]
const WINDOWS_UPDATE_LOG_MAX_BYTES: u64 = 64 * 1024;

#[derive(Default)]
enum UpdateOperationState {
    #[default]
    Idle,
    Checking,
    Ready(Box<AvailableUpdate>),
    Downloading {
        version: String,
    },
    Replacing {
        version: String,
    },
    Installed {
        version: String,
    },
}

#[derive(Debug)]
enum UpdateChannelResult {
    Available(AvailableUpdate),
    UpToDate,
    Unavailable,
}

#[derive(Clone, Debug)]
struct AvailableUpdate {
    current_version: String,
    version: String,
    body: Option<String>,
    download_url: reqwest::Url,
    signature: String,
    max_bytes: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum UpdatePlatform {
    Linux,
    Windows,
}

fn current_update_platform() -> UpdatePlatform {
    #[cfg(target_os = "windows")]
    {
        UpdatePlatform::Windows
    }
    #[cfg(not(target_os = "windows"))]
    {
        UpdatePlatform::Linux
    }
}

fn update_platform(os: &str) -> Option<UpdatePlatform> {
    match os {
        "linux" => Some(UpdatePlatform::Linux),
        "windows" => Some(UpdatePlatform::Windows),
        _ => None,
    }
}

fn install_behavior(platform: UpdatePlatform) -> InstallBehavior {
    match platform {
        UpdatePlatform::Linux => InstallBehavior::InstallThenRestart,
        UpdatePlatform::Windows => InstallBehavior::InstallAndRestart,
    }
}

fn platform_manifest(
    platforms: &UpdatePlatforms,
    platform: UpdatePlatform,
) -> &UpdateManifestPlatform {
    match platform {
        UpdatePlatform::Linux => &platforms.linux_x86_64,
        UpdatePlatform::Windows => &platforms.windows_x86_64,
    }
}

fn platform_max_bytes(platform: UpdatePlatform) -> u64 {
    match platform {
        UpdatePlatform::Linux => MAX_LINUX_UPDATE_BYTES,
        UpdatePlatform::Windows => MAX_WINDOWS_UPDATE_BYTES,
    }
}

#[derive(Deserialize)]
struct UpdateManifest {
    version: String,
    notes: Option<String>,
    platforms: UpdatePlatforms,
}

#[derive(Deserialize)]
struct UpdatePlatforms {
    #[serde(rename = "linux-x86_64")]
    linux_x86_64: UpdateManifestPlatform,
    #[serde(rename = "windows-x86_64")]
    windows_x86_64: UpdateManifestPlatform,
}

#[derive(Deserialize)]
struct UpdateManifestPlatform {
    signature: String,
    url: reqwest::Url,
}

#[derive(Default)]
struct UpdaterLifecycle {
    state: UpdateOperationState,
    exit_committed: bool,
    exit_deferred: bool,
}

#[derive(Default)]
pub(crate) struct PendingUpdate {
    lifecycle: Mutex<UpdaterLifecycle>,
}

#[derive(Debug, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub(crate) enum UpdateCheckResult {
    Available {
        current_version: String,
        version: String,
        notes: Option<String>,
        install_behavior: InstallBehavior,
    },
    UpToDate {
        current_version: String,
    },
    Unavailable {
        current_version: String,
        reason: &'static str,
    },
    Unsupported {
        current_version: String,
        reason: &'static str,
    },
    Installing {
        current_version: String,
        version: String,
        install_behavior: InstallBehavior,
    },
    Installed {
        current_version: String,
        version: String,
        install_behavior: InstallBehavior,
    },
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum InstallBehavior {
    InstallThenRestart,
    InstallAndRestart,
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum UpdateProgressPhase {
    Started,
    Progress,
    Verifying,
    Installing,
}

#[derive(Clone, Debug, Serialize)]
struct UpdateProgress {
    version: String,
    phase: UpdateProgressPhase,
    downloaded_bytes: u64,
    total_bytes: Option<u64>,
}

#[derive(Debug, Serialize)]
pub(crate) struct InstallUpdateResult {
    version: String,
}

#[tauri::command]
pub(crate) async fn check_for_update(
    app: AppHandle,
    pending_update: State<'_, PendingUpdate>,
) -> Result<UpdateCheckResult, String> {
    let current_version = app.package_info().version.to_string();
    #[cfg(target_os = "linux")]
    let environment = app.env();
    #[cfg(target_os = "linux")]
    let appimage = environment.appimage.as_deref().map(Path::new);
    #[cfg(not(target_os = "linux"))]
    let appimage = None;
    let current_executable = std::env::current_exe().ok();
    #[cfg(target_os = "windows")]
    let local_data_directory = app.path().local_data_dir().ok();
    #[cfg(not(target_os = "windows"))]
    let local_data_directory: Option<std::path::PathBuf> = None;
    if let Err(reason) = update_installation_support(
        std::env::consts::OS,
        appimage,
        current_executable.as_deref(),
        &std::env::temp_dir(),
        local_data_directory.as_deref(),
    ) {
        return Ok(UpdateCheckResult::Unsupported {
            current_version,
            reason,
        });
    }

    if let Some(result) = begin_check_or_snapshot(&pending_update, &current_version)? {
        return Ok(result);
    }

    let endpoint = match updater_endpoint(&app) {
        Ok(endpoint) => endpoint,
        Err(error) => {
            reset_checking_state(&pending_update)?;
            return Err(error);
        }
    };
    let update = match fetch_update_channel(&endpoint, &current_version).await {
        Ok(UpdateChannelResult::Available(update)) => update,
        Ok(UpdateChannelResult::UpToDate) => {
            set_update_state(&pending_update, UpdateOperationState::Idle)?;
            return Ok(UpdateCheckResult::UpToDate { current_version });
        }
        Ok(UpdateChannelResult::Unavailable) => {
            set_update_state(&pending_update, UpdateOperationState::Idle)?;
            return Ok(UpdateCheckResult::Unavailable {
                current_version,
                reason: UPDATE_CHANNEL_UNAVAILABLE_REASON,
            });
        }
        Err(error) => {
            reset_checking_state(&pending_update)?;
            return Err(error);
        }
    };

    let result = available_result(&update);
    set_update_state(
        &pending_update,
        UpdateOperationState::Ready(Box::new(update)),
    )?;
    Ok(result)
}

#[tauri::command]
pub(crate) async fn install_update(
    app: AppHandle,
    pending_update: State<'_, PendingUpdate>,
    app_state: State<'_, crate::AppState>,
) -> Result<InstallUpdateResult, String> {
    crate::ensure_update_installation_is_idle(&app_state)?;
    #[cfg(target_os = "windows")]
    let update_logging_enabled = app_state
        .runtime_config
        .as_ref()
        .is_some_and(|config| config.logging.enabled);
    #[cfg(target_os = "windows")]
    log_windows_update_event(&app, update_logging_enabled, "download started");
    let public_key = updater_public_key(&app)?;
    let update = take_ready_update(&pending_update)?;
    let version = update.version.clone();
    #[cfg(target_os = "linux")]
    let appimage_path = std::path::PathBuf::from(
        app.env()
            .appimage
            .clone()
            .ok_or_else(|| String::from("Linux AppImage path is unavailable"))?,
    );
    #[cfg(not(target_os = "linux"))]
    let appimage_path = std::path::PathBuf::new();

    let bytes = match download_and_verify_update(&app, &update, public_key).await {
        Ok(bytes) => bytes,
        Err(error) => {
            set_update_state(
                &pending_update,
                UpdateOperationState::Ready(Box::new(update)),
            )?;
            return Err(format!("failed to download or verify update: {error}"));
        }
    };
    #[cfg(target_os = "windows")]
    log_windows_update_event(&app, update_logging_enabled, "download verified");

    let _installation_guard = match crate::begin_update_installation(&app_state) {
        Ok(guard) => guard,
        Err(error) => {
            return Err(restore_ready_after_prelaunch_rejection(
                &pending_update,
                update,
                error,
            ));
        }
    };
    if let Err(error) = crate::ensure_update_installation_is_idle(&app_state) {
        return Err(restore_ready_after_prelaunch_rejection(
            &pending_update,
            update,
            error,
        ));
    }
    if !begin_replacement(&pending_update, &version)? {
        return Err(String::from(
            "application exit won before update replacement",
        ));
    }
    emit_progress(
        &app,
        &version,
        UpdateProgressPhase::Installing,
        bytes.len() as u64,
        Some(bytes.len() as u64),
    );
    let install_result = match tauri::async_runtime::spawn_blocking(move || {
        #[cfg(target_os = "windows")]
        {
            stage_and_launch_windows_installer(&bytes)
        }
        #[cfg(not(target_os = "windows"))]
        {
            replace_appimage(&appimage_path, &bytes)
        }
    })
    .await
    {
        Ok(result) => result.map_err(|error| format!("failed to install update: {error}")),
        Err(error) => Err(format!("update installer task failed: {error}")),
    };
    #[cfg(target_os = "windows")]
    log_windows_update_event(
        &app,
        update_logging_enabled,
        if install_result.is_ok() {
            "installer launched"
        } else {
            "installer launch failed"
        },
    );

    let next = if install_result.is_ok() {
        UpdateOperationState::Installed {
            version: version.clone(),
        }
    } else {
        UpdateOperationState::Ready(Box::new(update))
    };
    let should_exit = finish_replacement(&pending_update, next)?;
    if should_exit || (cfg!(target_os = "windows") && install_result.is_ok()) {
        app.exit(0);
    }
    install_result?;
    Ok(InstallUpdateResult { version })
}

#[tauri::command]
pub(crate) fn restart_for_update(
    app: AppHandle,
    pending_update: State<'_, PendingUpdate>,
) -> Result<(), String> {
    request_installed_restart(&pending_update, || app.request_restart())
}

fn request_installed_restart(
    pending_update: &PendingUpdate,
    request_restart: impl FnOnce(),
) -> Result<(), String> {
    let lifecycle = pending_update
        .lifecycle
        .lock()
        .map_err(|_| String::from("updater state lock is poisoned"))?;
    if !matches!(lifecycle.state, UpdateOperationState::Installed { .. }) {
        return Err(String::from(
            "an installed update is required before restart",
        ));
    }
    drop(lifecycle);
    request_restart();
    Ok(())
}

impl PendingUpdate {
    pub(crate) fn handle_exit_request(&self) -> bool {
        let mut lifecycle = self
            .lifecycle
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if matches!(lifecycle.state, UpdateOperationState::Replacing { .. }) {
            lifecycle.exit_deferred = true;
            true
        } else {
            lifecycle.exit_committed = true;
            false
        }
    }
}

pub(crate) fn update_installation_support(
    operating_system: &str,
    appimage_path: Option<&Path>,
    current_executable: Option<&Path>,
    temporary_directory: &Path,
    local_data_directory: Option<&Path>,
) -> Result<(), &'static str> {
    if update_platform(operating_system) == Some(UpdatePlatform::Windows) {
        let canonical_executable = local_data_directory.map(|directory| {
            directory
                .join("Programs")
                .join("VoxGolem")
                .join("vox-golem.exe")
        });
        if !current_executable
            .zip(canonical_executable.as_deref())
            .is_some_and(|(current, canonical)| windows_paths_match(current, canonical))
        {
            return Err("Automatic updates require the installed Windows application.");
        }
        return Ok(());
    }
    if operating_system != "linux" {
        return Err("Automatic updates are currently supported only on Linux and Windows.");
    }
    if appimage_path.is_none_or(|path| path.as_os_str().is_empty())
        || !current_executable.is_some_and(|executable| {
            executable
                .strip_prefix(temporary_directory)
                .ok()
                .and_then(|path| path.components().next())
                .is_some_and(|component| {
                    component
                        .as_os_str()
                        .to_string_lossy()
                        .starts_with(".mount_")
                })
        })
    {
        return Err("Automatic updates require the Linux AppImage.");
    }
    Ok(())
}

fn windows_paths_match(left: &Path, right: &Path) -> bool {
    fn normalize(path: &Path) -> Option<String> {
        let path = path.to_str()?.replace('/', "\\");
        Some(path.trim_end_matches('\\').to_ascii_lowercase())
    }

    normalize(left)
        .zip(normalize(right))
        .is_some_and(|(left, right)| left == right)
}

fn updater_public_key(app: &AppHandle) -> Result<String, String> {
    app.config()
        .plugins
        .0
        .get("updater")
        .and_then(|config| config.get("pubkey"))
        .and_then(serde_json::Value::as_str)
        .filter(|public_key| !public_key.is_empty())
        .map(String::from)
        .ok_or_else(|| String::from("updater public key is not configured"))
}

fn updater_endpoint(app: &AppHandle) -> Result<reqwest::Url, String> {
    app.config()
        .plugins
        .0
        .get("updater")
        .and_then(|config| config.get("endpoints"))
        .and_then(serde_json::Value::as_array)
        .and_then(|endpoints| endpoints.first())
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| String::from("updater endpoint is not configured"))?
        .parse()
        .map_err(|error| format!("invalid updater endpoint: {error}"))
}

async fn fetch_update_channel(
    endpoint: &reqwest::Url,
    current_version: &str,
) -> Result<UpdateChannelResult, String> {
    let response = reqwest::Client::builder()
        .user_agent(concat!(
            env!("CARGO_PKG_NAME"),
            "/",
            env!("CARGO_PKG_VERSION")
        ))
        .timeout(UPDATE_REQUEST_TIMEOUT)
        .build()
        .map_err(|error| format!("failed to configure update metadata request: {error}"))?
        .get(endpoint.clone())
        .send()
        .await
        .map_err(|error| format!("failed to fetch update metadata: {error}"))?;
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(UpdateChannelResult::Unavailable);
    }
    if response.status() == reqwest::StatusCode::NO_CONTENT {
        return Ok(UpdateChannelResult::UpToDate);
    }
    if !response.status().is_success() {
        return Err(format!(
            "update metadata request failed with status {}",
            response.status()
        ));
    }

    let content_length = response.content_length();
    if content_length.is_some_and(|length| length > MAX_UPDATE_METADATA_BYTES) {
        return Err(String::from("update metadata exceeds the supported size"));
    }
    let mut body = Vec::new();
    let mut downloaded = 0_u64;
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|error| format!("failed to read update metadata: {error}"))?;
        downloaded = downloaded
            .checked_add(chunk.len() as u64)
            .filter(|total| *total <= MAX_UPDATE_METADATA_BYTES)
            .ok_or_else(|| String::from("update metadata exceeds the supported size"))?;
        body.try_reserve_exact(chunk.len())
            .map_err(|error| format!("insufficient memory for update metadata: {error}"))?;
        body.extend_from_slice(&chunk);
    }
    parse_update_manifest(&body, current_version)
}

fn parse_update_manifest(
    body: &[u8],
    current_version: &str,
) -> Result<UpdateChannelResult, String> {
    let manifest = serde_json::from_slice::<UpdateManifest>(body)
        .map_err(|error| format!("invalid update metadata JSON: {error}"))?;
    if manifest
        .notes
        .as_deref()
        .is_some_and(|notes| notes.chars().count() > MAX_UPDATE_NOTES_CHARS)
    {
        return Err(String::from("update notes exceed the supported length"));
    }
    let current_version = semver::Version::parse(current_version)
        .map_err(|error| format!("invalid current application version: {error}"))?;
    let manifest_version = semver::Version::parse(&manifest.version)
        .map_err(|error| format!("invalid update version: {error}"))?;
    if manifest_version <= current_version {
        return Ok(UpdateChannelResult::UpToDate);
    }

    let version = manifest_version.to_string();
    let platform = current_update_platform();
    let selected = platform_manifest(&manifest.platforms, platform);
    expected_update_artifact(&version, &selected.url, platform)?;
    Ok(UpdateChannelResult::Available(AvailableUpdate {
        current_version: current_version.to_string(),
        version,
        body: manifest.notes,
        download_url: selected.url.clone(),
        signature: selected.signature.clone(),
        max_bytes: platform_max_bytes(platform),
    }))
}

async fn download_and_verify_update(
    app: &AppHandle,
    update: &AvailableUpdate,
    public_key: String,
) -> Result<Vec<u8>, String> {
    let deadline = Instant::now() + UPDATE_DOWNLOAD_TIMEOUT;
    let platform = current_update_platform();
    let expected_artifact =
        expected_update_artifact(&update.version, &update.download_url, platform)?;
    emit_progress(app, &update.version, UpdateProgressPhase::Started, 0, None);
    let bytes =
        run_before_update_deadline(deadline, "download", download_update(app, update)).await?;
    let signature = update.signature.clone();
    emit_progress(
        app,
        &update.version,
        UpdateProgressPhase::Verifying,
        bytes.len() as u64,
        Some(bytes.len() as u64),
    );

    run_before_update_deadline(
        deadline,
        "signature verification",
        verify_update_signature(
            &bytes,
            &signature,
            &public_key,
            &expected_artifact,
            deadline,
        ),
    )
    .await?;
    Ok(bytes)
}

fn expected_update_artifact(
    version: &str,
    download_url: &reqwest::Url,
    platform: UpdatePlatform,
) -> Result<String, String> {
    let tag = format!("v{version}");
    let artifact = match platform {
        UpdatePlatform::Linux => format!("vox-golem-linux-x86_64-{tag}.AppImage"),
        UpdatePlatform::Windows => format!("vox-golem-windows-x86_64-{tag}-setup.exe"),
    };
    let expected_path = format!("{UPDATE_RELEASE_PATH_PREFIX}/{tag}/{artifact}");
    if download_url.scheme() != "https"
        || download_url.host_str() != Some(UPDATE_RELEASE_HOST)
        || download_url.port().is_some()
        || !download_url.username().is_empty()
        || download_url.password().is_some()
        || download_url.path() != expected_path
        || download_url.query().is_some()
        || download_url.fragment().is_some()
    {
        return Err(String::from(
            "update artifact URL does not match the expected release identity",
        ));
    }
    Ok(artifact)
}

async fn download_update(app: &AppHandle, update: &AvailableUpdate) -> Result<Vec<u8>, String> {
    let response = reqwest::Client::builder()
        .user_agent(concat!(
            env!("CARGO_PKG_NAME"),
            "/",
            env!("CARGO_PKG_VERSION")
        ))
        .build()
        .map_err(|error| format!("failed to configure update download: {error}"))?
        .get(update.download_url.clone())
        .header(ACCEPT, HeaderValue::from_static("application/octet-stream"))
        .send()
        .await
        .map_err(|error| format!("update download request failed: {error}"))?;
    if !response.status().is_success() {
        return Err(format!(
            "update download request failed with status {}",
            response.status()
        ));
    }

    let content_length = response.content_length();
    ensure_declared_update_size(content_length, update.max_bytes)?;
    let mut bytes = Vec::new();
    if let Some(content_length) = content_length {
        reserve_update_bytes(&mut bytes, content_length as usize)?;
    }
    let mut downloaded = 0_u64;
    let mut last_progress = Instant::now();
    let mut last_progress_bytes = 0_u64;
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|error| format!("update download failed: {error}"))?;
        downloaded = checked_update_download_size(downloaded, chunk.len(), update.max_bytes)?;
        reserve_update_bytes(&mut bytes, chunk.len())?;
        bytes.extend_from_slice(&chunk);
        if downloaded == bytes.len() as u64
            && (downloaded.saturating_sub(last_progress_bytes) >= UPDATE_PROGRESS_BYTES
                || last_progress.elapsed() >= UPDATE_PROGRESS_INTERVAL)
        {
            emit_progress(
                app,
                &update.version,
                UpdateProgressPhase::Progress,
                downloaded,
                content_length,
            );
            last_progress = Instant::now();
            last_progress_bytes = downloaded;
        }
    }
    emit_progress(
        app,
        &update.version,
        UpdateProgressPhase::Progress,
        downloaded,
        content_length,
    );
    Ok(bytes)
}

fn emit_progress(
    app: &AppHandle,
    version: &str,
    phase: UpdateProgressPhase,
    downloaded_bytes: u64,
    total_bytes: Option<u64>,
) {
    let _ = app.emit(
        "app-update-progress",
        UpdateProgress {
            version: String::from(version),
            phase,
            downloaded_bytes,
            total_bytes,
        },
    );
}

fn ensure_declared_update_size(content_length: Option<u64>, max_bytes: u64) -> Result<(), String> {
    if content_length.is_some_and(|length| length > max_bytes) {
        return Err(format!(
            "update artifact exceeds the {max_bytes}-byte limit"
        ));
    }
    Ok(())
}

fn checked_update_download_size(
    downloaded: u64,
    chunk_size: usize,
    max_bytes: u64,
) -> Result<u64, String> {
    let total = downloaded
        .checked_add(chunk_size as u64)
        .filter(|total| *total <= max_bytes)
        .ok_or_else(|| format!("update artifact exceeds the {max_bytes}-byte limit"))?;
    Ok(total)
}

fn reserve_update_bytes(bytes: &mut Vec<u8>, additional: usize) -> Result<(), String> {
    let required_capacity = bytes.len().saturating_add(additional);
    if required_capacity > bytes.capacity() {
        bytes
            .try_reserve_exact(additional)
            .map_err(|error| format!("insufficient memory for update artifact: {error}"))?;
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn replace_appimage(path: &Path, bytes: &[u8]) -> Result<(), String> {
    use std::io::Write;

    if bytes.is_empty() {
        return Err(String::from("replacement AppImage is empty"));
    }
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|error| format!("failed to inspect current AppImage: {error}"))?;
    if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
        return Err(String::from("current AppImage path is not a regular file"));
    }
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or_else(|| String::from("current AppImage has no parent directory"))?;
    let mut replacement = tempfile::Builder::new()
        .prefix(".vox-golem-update-")
        .tempfile_in(parent)
        .map_err(|error| format!("failed to stage replacement AppImage: {error}"))?;
    replacement
        .write_all(bytes)
        .map_err(|error| format!("failed to write replacement AppImage: {error}"))?;
    replacement
        .as_file()
        .set_permissions(metadata.permissions())
        .map_err(|error| format!("failed to set replacement AppImage permissions: {error}"))?;
    replacement
        .as_file()
        .sync_all()
        .map_err(|error| format!("failed to sync replacement AppImage: {error}"))?;
    replacement
        .persist(path)
        .map_err(|error| format!("failed to replace current AppImage: {}", error.error))?;
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn replace_appimage(_path: &Path, _bytes: &[u8]) -> Result<(), String> {
    Err(String::from(
        "automatic update replacement is supported only on Linux",
    ))
}

#[cfg(any(target_os = "windows", test))]
fn windows_launch_args() -> [&'static str; 3] {
    ["/P", "/R", "/UPDATE"]
}

#[cfg(any(target_os = "windows", test))]
fn validate_staging_directory(path: &Path) -> Result<(), String> {
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|error| format!("failed to inspect update staging directory: {error}"))?;
    if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
        return Err(String::from(
            "update staging path is not a regular directory",
        ));
    }
    Ok(())
}

#[cfg(any(target_os = "windows", test))]
fn write_staging_marker(directory: &Path) -> Result<(), String> {
    use std::io::Write;

    let marker = directory.join(WINDOWS_STAGING_MARKER);
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&marker)
        .map_err(|error| format!("failed to create update staging marker: {error}"))?;
    file.write_all(WINDOWS_STAGING_MARKER_CONTENT.as_bytes())
        .map_err(|error| format!("failed to write update staging marker: {error}"))?;
    file.sync_all()
        .map_err(|error| format!("failed to sync update staging marker: {error}"))
}

#[cfg(any(target_os = "windows", test))]
fn cleanup_stale_windows_installers_in(root: &Path, minimum_age: Duration) -> usize {
    let Ok(entries) = std::fs::read_dir(root) else {
        return 0;
    };
    let mut removed = 0;
    for entry in entries
        .flatten()
        .filter(|entry| {
            entry
                .file_name()
                .to_str()
                .is_some_and(|name| name.starts_with(WINDOWS_STAGING_PREFIX))
        })
        .take(WINDOWS_STAGING_MAX_SCAN)
    {
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        debug_assert!(name.starts_with(WINDOWS_STAGING_PREFIX));
        let path = entry.path();
        let Ok(metadata) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
            continue;
        }
        let marker = path.join(WINDOWS_STAGING_MARKER);
        let Ok(marker_metadata) = std::fs::symlink_metadata(&marker) else {
            continue;
        };
        if !marker_metadata.file_type().is_file() || marker_metadata.file_type().is_symlink() {
            continue;
        }
        if !matches!(
            std::fs::read_to_string(&marker),
            Ok(content) if content == WINDOWS_STAGING_MARKER_CONTENT
        ) {
            continue;
        }
        let Ok(age) = metadata.modified().and_then(|modified| {
            std::time::SystemTime::now()
                .duration_since(modified)
                .map_err(std::io::Error::other)
        }) else {
            continue;
        };
        if age < minimum_age {
            continue;
        }
        if std::fs::remove_dir_all(&path).is_ok() {
            removed += 1;
        }
    }
    removed
}

#[cfg(target_os = "windows")]
pub(crate) fn cleanup_stale_windows_installers(app: &AppHandle, logging_enabled: bool) {
    let removed =
        cleanup_stale_windows_installers_in(&std::env::temp_dir(), WINDOWS_STAGING_MAX_AGE);
    if removed > 0 {
        log_windows_update_event(
            app,
            logging_enabled,
            &format!("removed {removed} stale staging directories"),
        );
    }
}

#[cfg(target_os = "windows")]
fn log_windows_update_event(app: &AppHandle, enabled: bool, event: &str) {
    use std::io::Write;

    if !enabled {
        return;
    }
    let Ok(directory) = app.path().app_log_dir() else {
        return;
    };
    if std::fs::create_dir_all(&directory).is_err() {
        return;
    }
    let path = directory.join("updates.log");
    let truncate = match std::fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.file_type().is_file() && !metadata.file_type().is_symlink() => {
            metadata.len() >= WINDOWS_UPDATE_LOG_MAX_BYTES
        }
        Ok(_) => return,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(_) => return,
    };
    let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(!truncate)
        .truncate(truncate)
        .write(true)
        .open(path)
    else {
        return;
    };
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    let safe_event: String = event
        .chars()
        .filter(|character| character.is_ascii_alphanumeric() || matches!(character, ' ' | '-'))
        .take(128)
        .collect();
    let _ = writeln!(file, "{timestamp} {safe_event}");
}

#[cfg(target_os = "windows")]
fn stage_and_launch_windows_installer(bytes: &[u8]) -> Result<(), String> {
    use std::fs::OpenOptions;
    use std::io::Write;
    use std::process::Command;

    if bytes.is_empty() {
        return Err(String::from("update installer is empty"));
    }
    let root = std::env::temp_dir();
    let directory = tempfile::Builder::new()
        .prefix(WINDOWS_STAGING_PREFIX)
        .tempdir_in(&root)
        .map_err(|error| format!("failed to create update staging directory: {error}"))?;
    validate_staging_directory(directory.path())?;
    write_staging_marker(directory.path())?;
    let installer = directory.path().join("vox-golem-update.exe");
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&installer)
        .map_err(|error| format!("failed to create staged installer: {error}"))?;
    file.write_all(bytes)
        .map_err(|error| format!("failed to stage installer: {error}"))?;
    file.sync_all()
        .map_err(|error| format!("failed to sync staged installer: {error}"))?;
    drop(file);
    Command::new(&installer)
        .args(windows_launch_args())
        .spawn()
        .map_err(|error| format!("failed to launch staged installer: {error}"))?;
    std::mem::forget(directory);
    Ok(())
}

async fn run_before_update_deadline<T, F>(
    deadline: Instant,
    operation: &'static str,
    future: F,
) -> Result<T, String>
where
    F: Future<Output = Result<T, String>>,
{
    tokio::time::timeout_at(deadline, future)
        .await
        .map_err(|_| format!("update {operation} timed out"))?
}

async fn verify_update_signature(
    bytes: &[u8],
    encoded_signature: &str,
    encoded_public_key: &str,
    expected_artifact: &str,
    deadline: Instant,
) -> Result<(), String> {
    ensure_verification_before_deadline(deadline)?;
    let public_key = PublicKey::decode(&decode_base64_text(encoded_public_key, "public key")?)
        .map_err(|error| format!("invalid updater public key: {error}"))?;
    let signature = Signature::decode(&decode_base64_text(encoded_signature, "signature")?)
        .map_err(|error| format!("invalid update signature: {error}"))?;
    let trusted_comment = signature.trusted_comment();
    let Some((timestamp, signed_artifact)) = trusted_comment
        .strip_prefix("timestamp:")
        .and_then(|comment| comment.split_once("\tfile:"))
    else {
        return Err(String::from(
            "update signature does not contain a trusted artifact identity",
        ));
    };
    if timestamp.is_empty()
        || !timestamp.bytes().all(|byte| byte.is_ascii_digit())
        || signed_artifact != expected_artifact
    {
        return Err(String::from(
            "update signature artifact identity does not match metadata",
        ));
    }
    let mut verifier = public_key
        .verify_stream(&signature)
        .map_err(|error| format!("unsupported update signature: {error}"))?;
    for chunk in bytes.chunks(64 * 1024) {
        ensure_verification_before_deadline(deadline)?;
        verifier.update(chunk);
        tokio::task::yield_now().await;
    }
    ensure_verification_before_deadline(deadline)?;
    verifier
        .finalize()
        .map_err(|error| format!("update signature verification failed: {error}"))
}

fn ensure_verification_before_deadline(deadline: Instant) -> Result<(), String> {
    if Instant::now() >= deadline {
        return Err(String::from("update signature verification timed out"));
    }
    Ok(())
}

fn decode_base64_text(encoded: &str, name: &str) -> Result<String, String> {
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .map_err(|error| format!("invalid base64 {name}: {error}"))?;
    String::from_utf8(decoded).map_err(|error| format!("non-UTF-8 {name}: {error}"))
}

fn begin_check_or_snapshot(
    pending_update: &PendingUpdate,
    current_version: &str,
) -> Result<Option<UpdateCheckResult>, String> {
    let mut lifecycle = pending_update
        .lifecycle
        .lock()
        .map_err(|_| String::from("updater state lock is poisoned"))?;
    if lifecycle.exit_committed {
        return Err(String::from("application exit is already committed"));
    }
    let result = match &lifecycle.state {
        UpdateOperationState::Idle => {
            lifecycle.state = UpdateOperationState::Checking;
            None
        }
        UpdateOperationState::Checking => {
            return Err(String::from("an update check is already in progress"));
        }
        UpdateOperationState::Ready(update) => Some(available_result(update)),
        UpdateOperationState::Downloading { version }
        | UpdateOperationState::Replacing { version } => Some(UpdateCheckResult::Installing {
            current_version: String::from(current_version),
            version: version.clone(),
            install_behavior: install_behavior(current_update_platform()),
        }),
        UpdateOperationState::Installed { version } => Some(UpdateCheckResult::Installed {
            current_version: String::from(current_version),
            version: version.clone(),
            install_behavior: install_behavior(current_update_platform()),
        }),
    };
    Ok(result)
}

fn take_ready_update(pending_update: &PendingUpdate) -> Result<AvailableUpdate, String> {
    let mut lifecycle = pending_update
        .lifecycle
        .lock()
        .map_err(|_| String::from("updater state lock is poisoned"))?;
    if lifecycle.exit_committed {
        return Err(String::from("application exit is already committed"));
    }
    let previous = std::mem::take(&mut lifecycle.state);
    match previous {
        UpdateOperationState::Ready(update) => {
            lifecycle.state = UpdateOperationState::Downloading {
                version: update.version.clone(),
            };
            Ok(*update)
        }
        UpdateOperationState::Downloading { version } => {
            lifecycle.state = UpdateOperationState::Downloading { version };
            Err(String::from(
                "an update installation is already in progress",
            ))
        }
        UpdateOperationState::Replacing { version } => {
            lifecycle.state = UpdateOperationState::Replacing { version };
            Err(String::from(
                "an update installation is already in progress",
            ))
        }
        UpdateOperationState::Installed { version } => {
            lifecycle.state = UpdateOperationState::Installed { version };
            Err(String::from("the update is installed and awaiting restart"))
        }
        other => {
            lifecycle.state = other;
            Err(String::from("there is no checked update to install"))
        }
    }
}

fn restore_ready_after_prelaunch_rejection(
    pending_update: &PendingUpdate,
    update: AvailableUpdate,
    error: String,
) -> String {
    match set_update_state(
        pending_update,
        UpdateOperationState::Ready(Box::new(update)),
    ) {
        Ok(()) => error,
        Err(state_error) => format!("{error}; failed to restore update state: {state_error}"),
    }
}

fn begin_replacement(pending_update: &PendingUpdate, version: &str) -> Result<bool, String> {
    let mut lifecycle = pending_update
        .lifecycle
        .lock()
        .map_err(|_| String::from("updater state lock is poisoned"))?;
    if lifecycle.exit_committed {
        lifecycle.state = UpdateOperationState::Idle;
        return Ok(false);
    }
    if !matches!(lifecycle.state, UpdateOperationState::Downloading { .. }) {
        return Err(String::from(
            "updater was not downloading before replacement",
        ));
    }
    lifecycle.state = UpdateOperationState::Replacing {
        version: String::from(version),
    };
    Ok(true)
}

fn available_result(update: &AvailableUpdate) -> UpdateCheckResult {
    UpdateCheckResult::Available {
        current_version: update.current_version.clone(),
        version: update.version.clone(),
        notes: update.body.clone(),
        install_behavior: install_behavior(current_update_platform()),
    }
}

fn finish_replacement(
    pending_update: &PendingUpdate,
    next: UpdateOperationState,
) -> Result<bool, String> {
    let mut lifecycle = pending_update
        .lifecycle
        .lock()
        .map_err(|_| String::from("updater state lock is poisoned"))?;
    if !matches!(lifecycle.state, UpdateOperationState::Replacing { .. }) {
        return Err(String::from("updater replacement was not active"));
    }
    lifecycle.state = next;
    Ok(std::mem::take(&mut lifecycle.exit_deferred))
}

fn reset_checking_state(pending_update: &PendingUpdate) -> Result<(), String> {
    let mut lifecycle = pending_update
        .lifecycle
        .lock()
        .map_err(|_| String::from("updater state lock is poisoned"))?;
    if matches!(lifecycle.state, UpdateOperationState::Checking) {
        lifecycle.state = UpdateOperationState::Idle;
    }
    Ok(())
}

fn set_update_state(
    pending_update: &PendingUpdate,
    next: UpdateOperationState,
) -> Result<(), String> {
    pending_update
        .lifecycle
        .lock()
        .map_err(|_| String::from("updater state lock is poisoned"))?
        .state = next;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        begin_check_or_snapshot, begin_replacement, finish_replacement, take_ready_update,
        update_installation_support, PendingUpdate, UpdateCheckResult, UpdateOperationState,
        UpdaterLifecycle, MAX_LINUX_UPDATE_BYTES, UPDATE_CHANNEL_UNAVAILABLE_REASON,
        UPDATE_DOWNLOAD_TIMEOUT,
    };
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::path::Path;
    use std::sync::Mutex;
    use std::thread;
    use std::time::Duration;

    const FIXTURE_PUBLIC_KEY: &str = "dW50cnVzdGVkIGNvbW1lbnQ6IG1pbmlzaWduIHB1YmxpYyBrZXk6IEMwNjVCODhEQUZCNUI2NgpSV1JtVy92YWlGc0dEQm94ZG10c0ZUVllqTDR2N0ZkUGZ1RlJldVRUazM2Mit1Q252UG40SmU1Kwo=";
    const FIXTURE_SIGNATURE: &str = "dW50cnVzdGVkIGNvbW1lbnQ6IHNpZ25hdHVyZSBmcm9tIHRhdXJpIHNlY3JldCBrZXkKUlVSbVcvdmFpRnNHRExwb3RYV1VBeTk5WFdFcXhlWktYaW8wbFZ3UEdJd01hL1hQdEhGU2NhOHRqNkRwL3FlU0o2TWJHNVp2UDBneVVzNXRSQVZIRmRvcFdkZDNMMkY2Znc0PQp0cnVzdGVkIGNvbW1lbnQ6IHRpbWVzdGFtcDoxNzg1MTI0MzQwCWZpbGU6dGVzdC1maXh0dXJlLnR4dApFL09EalArMGFwNUR1VWFpa2dRWkdGWkdBck0vQ2phZmhnRE0zTk92c2xwdnA4Snd3NE9kUVdoaEVHdisxVDNoV21tRGpUTitZQTFaTVhqc0YzdE1CZz09Cg==";
    const FIXTURE_MESSAGE: &[u8] = b"VoxGolem updater verifier fixture\n";

    #[test]
    fn automatic_updates_require_the_platform_installation_identity() {
        assert_eq!(
            update_installation_support(
                "linux",
                Some(Path::new("/opt/VoxGolem.AppImage")),
                Some(Path::new("/tmp/.mount_vox/usr/bin/vox-golem")),
                Path::new("/tmp"),
                None,
            ),
            Ok(())
        );
        assert_eq!(
            update_installation_support(
                "linux",
                None,
                Some(Path::new("/tmp/.mount_vox/usr/bin/vox-golem")),
                Path::new("/tmp"),
                None,
            ),
            Err("Automatic updates require the Linux AppImage.")
        );
        assert_eq!(
            update_installation_support(
                "linux",
                Some(Path::new("/tmp/forged.AppImage")),
                Some(Path::new("/usr/bin/vox-golem")),
                Path::new("/tmp"),
                None,
            ),
            Err("Automatic updates require the Linux AppImage.")
        );
        assert_eq!(
            update_installation_support(
                "windows",
                Some(Path::new("ignored")),
                Some(Path::new(
                    "C:/Users/Test/AppData/Local/Programs/VoxGolem/vox-golem.exe"
                )),
                Path::new("ignored"),
                Some(Path::new("c:\\users\\test\\appdata\\local")),
            ),
            Ok(())
        );
        assert_eq!(
            update_installation_support(
                "windows",
                None,
                Some(Path::new(
                    "C:/Users/Test/AppData/Local/VoxGolem/vox-golem.exe",
                )),
                Path::new("ignored"),
                Some(Path::new("C:/Users/Test/AppData/Local")),
            ),
            Err("Automatic updates require the installed Windows application.")
        );
        assert_eq!(
            update_installation_support(
                "windows",
                None,
                Some(Path::new("C:/portable/vox-golem.exe")),
                Path::new("ignored"),
                Some(Path::new("C:/Users/Test/AppData/Local")),
            ),
            Err("Automatic updates require the installed Windows application.")
        );
        assert_eq!(
            update_installation_support("macos", None, None, Path::new("ignored"), None),
            Err("Automatic updates are currently supported only on Linux and Windows.")
        );
    }

    #[test]
    fn update_results_have_a_stable_tagged_contract() {
        let available = UpdateCheckResult::Available {
            current_version: String::from("0.1.0"),
            version: String::from("2026.7.27-12"),
            notes: Some(String::from("Safer updates")),
            install_behavior: super::InstallBehavior::InstallThenRestart,
        };
        assert_eq!(
            serde_json::to_value(available).expect("serialize update result"),
            serde_json::json!({
                "status": "available",
                "current_version": "0.1.0",
                "version": "2026.7.27-12",
                "notes": "Safer updates",
                "install_behavior": "install_then_restart"
            })
        );

        let unavailable = UpdateCheckResult::Unavailable {
            current_version: String::from("0.1.0"),
            reason: UPDATE_CHANNEL_UNAVAILABLE_REASON,
        };
        assert_eq!(
            serde_json::to_value(unavailable).expect("serialize unavailable result"),
            serde_json::json!({
                "status": "unavailable",
                "current_version": "0.1.0",
                "reason": "No published updates yet."
            })
        );
    }

    #[tokio::test]
    async fn metadata_fetch_is_single_bounded_and_distinguishes_real_failures() {
        assert!(matches!(
            fetch_response("404 Not Found", "missing").await,
            Ok(super::UpdateChannelResult::Unavailable)
        ));
        assert!(matches!(
            fetch_response("204 No Content", "").await,
            Ok(super::UpdateChannelResult::UpToDate)
        ));
        assert!(matches!(
            fetch_response("200 OK", &manifest_json("0.1.0")).await,
            Ok(super::UpdateChannelResult::UpToDate)
        ));
        assert!(matches!(
            fetch_response("200 OK", &manifest_json("2026.7.27-43")).await,
            Ok(super::UpdateChannelResult::Available(update))
                if update.version == "2026.7.27-43"
        ));
        for status in [
            "403 Forbidden",
            "429 Too Many Requests",
            "500 Internal Server Error",
        ] {
            let error = fetch_response(status, "failure")
                .await
                .expect_err("non-404 failure must remain visible");
            assert!(error.contains(status.split_once(' ').expect("status").0));
        }
        assert!(fetch_response("200 OK", "not json")
            .await
            .expect_err("malformed JSON must fail")
            .contains("invalid update metadata JSON"));

        let listener = TcpListener::bind("127.0.0.1:0").expect("reserve address");
        let endpoint: reqwest::Url = format!(
            "http://{}/latest.json",
            listener.local_addr().expect("address")
        )
        .parse()
        .expect("endpoint URL");
        drop(listener);
        assert!(super::fetch_update_channel(&endpoint, "0.1.0")
            .await
            .expect_err("transport failure must remain visible")
            .contains("failed to fetch update metadata"));
    }

    #[tokio::test]
    async fn oversized_declared_metadata_response_is_rejected() {
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            super::MAX_UPDATE_METADATA_BYTES + 1
        );
        assert_eq!(
            fetch_raw_response(response.into_bytes())
                .await
                .expect_err("oversized declared metadata must fail"),
            "update metadata exceeds the supported size"
        );
    }

    #[tokio::test]
    async fn oversized_chunked_metadata_response_is_rejected() {
        let bounded_chunk = vec![b'x'; super::MAX_UPDATE_METADATA_BYTES as usize];
        let mut response =
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n".to_vec();
        write!(response, "{:X}\r\n", bounded_chunk.len()).expect("write chunk size");
        response.extend_from_slice(&bounded_chunk);
        response.extend_from_slice(b"\r\n1\r\nx\r\n0\r\n\r\n");

        assert_eq!(
            fetch_raw_response(response)
                .await
                .expect_err("oversized streamed metadata must fail"),
            "update metadata exceeds the supported size"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn appimage_replacement_preserves_mode_and_rejects_symlinks() {
        use std::os::unix::fs::{symlink, PermissionsExt};

        let temp = tempfile::tempdir().expect("temporary AppImage directory");
        let appimage = temp.path().join("VoxGolem.AppImage");
        std::fs::write(&appimage, b"old AppImage").expect("write old AppImage");
        std::fs::set_permissions(&appimage, std::fs::Permissions::from_mode(0o751))
            .expect("set old AppImage mode");

        super::replace_appimage(&appimage, b"new signed AppImage")
            .expect("replace AppImage atomically");
        assert_eq!(
            std::fs::read(&appimage).expect("read replaced AppImage"),
            b"new signed AppImage"
        );
        assert_eq!(
            std::fs::metadata(&appimage)
                .expect("replacement metadata")
                .permissions()
                .mode()
                & 0o777,
            0o751
        );

        let target = temp.path().join("target.AppImage");
        let linked = temp.path().join("linked.AppImage");
        std::fs::write(&target, b"do not replace").expect("write symlink target");
        symlink(&target, &linked).expect("create AppImage symlink");
        assert!(super::replace_appimage(&linked, b"replacement").is_err());
        assert_eq!(
            std::fs::read(&target).expect("read unchanged symlink target"),
            b"do not replace"
        );
    }

    #[test]
    fn check_during_download_reports_progress_without_starting_another_check() {
        let pending = pending_in(UpdateOperationState::Downloading {
            version: String::from("2026.7.27-12"),
        });
        let result = begin_check_or_snapshot(&pending, "0.1.0")
            .expect("snapshot updater state")
            .expect("installing snapshot");
        assert!(matches!(
            result,
            UpdateCheckResult::Installing { version, .. } if version == "2026.7.27-12"
        ));
        assert!(matches!(
            pending.lifecycle.lock().expect("state lock").state,
            UpdateOperationState::Downloading { .. }
        ));
    }

    #[test]
    fn second_install_is_rejected_without_changing_the_first_install() {
        let pending = pending_in(UpdateOperationState::Downloading {
            version: String::from("2026.7.27-12"),
        });
        let error = match take_ready_update(&pending) {
            Ok(_) => panic!("second install must fail"),
            Err(error) => error,
        };
        assert_eq!(error, "an update installation is already in progress");
        assert!(matches!(
            &pending.lifecycle.lock().expect("state lock").state,
            UpdateOperationState::Downloading { version } if version == "2026.7.27-12"
        ));
    }

    #[test]
    fn busy_prelaunch_rejection_restores_the_available_update() {
        let pending = pending_in(UpdateOperationState::Downloading {
            version: String::from("2026.7.27-12"),
        });
        let update = super::AvailableUpdate {
            current_version: String::from("0.1.0"),
            version: String::from("2026.7.27-12"),
            body: None,
            download_url: "https://github.com/git-blame-dev/vox-golem/releases/download/v2026.7.27-12/vox-golem-windows-x86_64-v2026.7.27-12-setup.exe"
                .parse()
                .expect("update URL"),
            signature: String::from("signature"),
            max_bytes: super::MAX_WINDOWS_UPDATE_BYTES,
        };
        assert_eq!(
            super::restore_ready_after_prelaunch_rejection(
                &pending,
                update,
                String::from("Update installation requires VoxGolem to be idle."),
            ),
            "Update installation requires VoxGolem to be idle."
        );
        assert!(matches!(
            begin_check_or_snapshot(&pending, "0.1.0").expect("available snapshot"),
            Some(UpdateCheckResult::Available { version, .. }) if version == "2026.7.27-12"
        ));
    }

    #[test]
    fn exit_committed_during_download_prevents_replacement() {
        let pending = pending_in(UpdateOperationState::Downloading {
            version: String::from("2026.7.27-12"),
        });
        assert!(!pending.handle_exit_request());
        assert!(!begin_replacement(&pending, "2026.7.27-12").expect("try replacement"));
        let lifecycle = pending.lifecycle.lock().expect("lifecycle lock");
        assert!(lifecycle.exit_committed);
        assert!(matches!(lifecycle.state, UpdateOperationState::Idle));
    }

    #[test]
    fn replacement_started_before_exit_defers_exit_until_completion() {
        let pending = pending_in(UpdateOperationState::Downloading {
            version: String::from("2026.7.27-12"),
        });
        assert!(begin_replacement(&pending, "2026.7.27-12").expect("begin replacement"));
        assert!(pending.handle_exit_request());
        assert!(finish_replacement(
            &pending,
            UpdateOperationState::Installed {
                version: String::from("2026.7.27-12"),
            },
        )
        .expect("finish replacement"));
        let lifecycle = pending.lifecycle.lock().expect("lifecycle lock");
        assert!(!lifecycle.exit_deferred);
        assert!(!lifecycle.exit_committed);
        assert!(matches!(
            lifecycle.state,
            UpdateOperationState::Installed { .. }
        ));
    }

    #[test]
    fn restart_uses_graceful_request_only_after_installation() {
        let pending = pending_in(UpdateOperationState::Installed {
            version: String::from("2026.7.27-12"),
        });
        let mut requested = false;
        super::request_installed_restart(&pending, || requested = true)
            .expect("request graceful restart");
        assert!(requested);

        let pending = pending_in(UpdateOperationState::Idle);
        let mut requested = false;
        assert!(super::request_installed_restart(&pending, || requested = true).is_err());
        assert!(!requested);
    }

    #[test]
    fn artifact_download_has_a_bounded_long_running_timeout() {
        assert_eq!(UPDATE_DOWNLOAD_TIMEOUT, Duration::from_secs(30 * 60));
    }

    #[test]
    fn oversized_declared_and_streamed_updates_are_rejected() {
        assert!(super::ensure_declared_update_size(
            Some(MAX_LINUX_UPDATE_BYTES),
            MAX_LINUX_UPDATE_BYTES
        )
        .is_ok());
        assert!(super::ensure_declared_update_size(
            Some(MAX_LINUX_UPDATE_BYTES + 1),
            MAX_LINUX_UPDATE_BYTES
        )
        .is_err());
        assert_eq!(
            super::checked_update_download_size(
                MAX_LINUX_UPDATE_BYTES - 1,
                1,
                MAX_LINUX_UPDATE_BYTES
            ),
            Ok(MAX_LINUX_UPDATE_BYTES)
        );
        assert!(super::checked_update_download_size(
            MAX_LINUX_UPDATE_BYTES - 1,
            2,
            MAX_LINUX_UPDATE_BYTES
        )
        .is_err());
    }

    #[tokio::test(start_paused = true)]
    async fn download_and_verification_share_one_absolute_deadline() {
        let deadline = tokio::time::Instant::now() + UPDATE_DOWNLOAD_TIMEOUT;
        super::run_before_update_deadline(deadline, "download", async {
            tokio::time::sleep(UPDATE_DOWNLOAD_TIMEOUT - Duration::from_secs(1)).await;
            Ok(())
        })
        .await
        .expect("download before deadline");

        let error = super::run_before_update_deadline(deadline, "signature verification", async {
            tokio::time::sleep(Duration::from_secs(2)).await;
            Ok(())
        })
        .await
        .expect_err("verification must use the remaining deadline");
        assert_eq!(error, "update signature verification timed out");
    }

    #[tokio::test(start_paused = true)]
    async fn expired_verification_stops_before_signature_processing() {
        let error = super::verify_update_signature(
            b"artifact",
            "not a signature",
            "not a public key",
            "artifact.AppImage",
            tokio::time::Instant::now(),
        )
        .await
        .expect_err("expired verification must stop");
        assert_eq!(error, "update signature verification timed out");
    }

    #[tokio::test]
    async fn streamed_verifier_accepts_tauri_signature_fixture() {
        super::verify_update_signature(
            FIXTURE_MESSAGE,
            FIXTURE_SIGNATURE,
            FIXTURE_PUBLIC_KEY,
            "test-fixture.txt",
            tokio::time::Instant::now() + Duration::from_secs(1),
        )
        .await
        .expect("verify Tauri signature fixture");
    }

    #[tokio::test]
    async fn streamed_verifier_rejects_replayed_signature_identity() {
        super::verify_update_signature(
            FIXTURE_MESSAGE,
            FIXTURE_SIGNATURE,
            FIXTURE_PUBLIC_KEY,
            "vox-golem-linux-x86_64-v2026.7.27-43.AppImage",
            tokio::time::Instant::now() + Duration::from_secs(1),
        )
        .await
        .expect_err("old signed artifact identity must not satisfy newer metadata");
    }

    #[test]
    fn update_artifact_identity_binds_target_version_and_release_url() {
        let version = "2026.7.27-43";
        let artifact = "vox-golem-linux-x86_64-v2026.7.27-43.AppImage";
        let valid_url = format!(
            "https://github.com/git-blame-dev/vox-golem/releases/download/v{version}/{artifact}"
        )
        .parse()
        .expect("valid update URL");
        assert_eq!(
            super::expected_update_artifact(version, &valid_url, super::UpdatePlatform::Linux),
            Ok(String::from(artifact))
        );

        for invalid_url in [
            format!(
                "https://github.com/git-blame-dev/vox-golem/releases/download/v2026.7.26-42/{artifact}"
            ),
            format!(
                "https://github.com/attacker/vox-golem/releases/download/v{version}/{artifact}"
            ),
            format!(
                "http://github.com/git-blame-dev/vox-golem/releases/download/v{version}/{artifact}"
            ),
            format!(
                "https://github.com/git-blame-dev/vox-golem/releases/download/v{version}/{artifact}?replacement=1"
            ),
        ] {
            assert!(super::expected_update_artifact(
                version,
                &invalid_url.parse().expect("invalid-case URL still parses"),
                super::UpdatePlatform::Linux,
            )
            .is_err());
        }
    }

    #[test]
    fn platform_descriptors_have_distinct_exact_artifacts_and_bounds() {
        let version = "2026.7.27-43";
        let linux = format!("https://github.com/git-blame-dev/vox-golem/releases/download/v{version}/vox-golem-linux-x86_64-v{version}.AppImage").parse().expect("linux URL");
        let windows = format!("https://github.com/git-blame-dev/vox-golem/releases/download/v{version}/vox-golem-windows-x86_64-v{version}-setup.exe").parse().expect("windows URL");
        assert_eq!(
            super::expected_update_artifact(version, &linux, super::UpdatePlatform::Linux).unwrap(),
            format!("vox-golem-linux-x86_64-v{version}.AppImage")
        );
        assert_eq!(
            super::expected_update_artifact(version, &windows, super::UpdatePlatform::Windows)
                .unwrap(),
            format!("vox-golem-windows-x86_64-v{version}-setup.exe")
        );
        assert_eq!(
            super::platform_max_bytes(super::UpdatePlatform::Linux),
            300_000_000
        );
        assert_eq!(
            super::platform_max_bytes(super::UpdatePlatform::Windows),
            250_000_000
        );
    }

    #[test]
    fn windows_launch_and_staging_seams_are_host_testable() {
        assert_eq!(super::windows_launch_args(), ["/P", "/R", "/UPDATE"]);
        let directory = tempfile::tempdir().expect("staging directory");
        super::validate_staging_directory(directory.path()).expect("regular directory");
        let link = directory.path().join("link");
        #[cfg(unix)]
        std::os::unix::fs::symlink(directory.path(), &link).expect("symlink");
        #[cfg(unix)]
        assert!(super::validate_staging_directory(&link).is_err());

        let marked = directory.path().join("vox-golem-update-marked");
        let unmarked = directory.path().join("vox-golem-update-unmarked");
        for index in 0..=super::WINDOWS_STAGING_MAX_SCAN {
            std::fs::create_dir(directory.path().join(format!("unrelated-{index}")))
                .expect("unrelated directory");
        }
        std::fs::create_dir(&marked).expect("marked directory");
        std::fs::create_dir(&unmarked).expect("unmarked directory");
        super::write_staging_marker(&marked).expect("staging marker");
        assert_eq!(
            super::cleanup_stale_windows_installers_in(directory.path(), Duration::ZERO),
            1
        );
        assert!(!marked.exists());
        assert!(unmarked.exists());
    }

    fn pending_in(state: UpdateOperationState) -> PendingUpdate {
        PendingUpdate {
            lifecycle: Mutex::new(UpdaterLifecycle {
                state,
                ..UpdaterLifecycle::default()
            }),
        }
    }

    async fn fetch_response(
        status: &str,
        body: &str,
    ) -> Result<super::UpdateChannelResult, String> {
        fetch_raw_response(
            format!(
                "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .into_bytes(),
        )
        .await
    }

    async fn fetch_raw_response(response: Vec<u8>) -> Result<super::UpdateChannelResult, String> {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind metadata server");
        let address = listener.local_addr().expect("metadata server address");
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept metadata request");
            let mut request = [0_u8; 2_048];
            let _ = stream.read(&mut request).expect("read metadata request");
            stream
                .write_all(&response)
                .expect("write metadata response");
            listener
                .set_nonblocking(true)
                .expect("set metadata listener nonblocking");
            let deadline = std::time::Instant::now() + Duration::from_millis(100);
            let mut requests = 1;
            while std::time::Instant::now() < deadline {
                match listener.accept() {
                    Ok(_) => requests += 1,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(error) => panic!("accept another metadata request: {error}"),
                }
            }
            requests
        });
        let endpoint = format!("http://{address}/latest.json")
            .parse()
            .expect("metadata URL");
        let result = super::fetch_update_channel(&endpoint, "0.1.0").await;
        assert_eq!(server.join().expect("metadata server thread"), 1);
        result
    }

    fn manifest_json(version: &str) -> String {
        let artifact = format!("vox-golem-linux-x86_64-v{version}.AppImage");
        serde_json::json!({
            "version": version,
            "platforms": {
                "linux-x86_64": {
                    "signature": "signed artifact",
                    "url": format!(
                        "https://github.com/git-blame-dev/vox-golem/releases/download/v{version}/{artifact}"
                    )
                },
                "windows-x86_64": {
                    "signature": "signed artifact",
                    "url": format!(
                        "https://github.com/git-blame-dev/vox-golem/releases/download/v{version}/vox-golem-windows-x86_64-v{version}-setup.exe"
                    )
                }
            }
        })
        .to_string()
    }
}
