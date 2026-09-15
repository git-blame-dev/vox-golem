use serde::Serialize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::Duration;
use tauri::utils::config::BundleType;
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_updater::{Error as UpdaterError, Update, UpdaterExt};

const UPDATE_CHECK_TIMEOUT: Duration = Duration::from_secs(30);
const UPDATE_DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(10 * 60);
const UPDATE_SNAPSHOT_EVENT: &str = "app-update-state";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum UpdatePhase {
    Idle,
    Checking,
    UpToDate,
    Unavailable,
    Unsupported,
    Available,
    Downloading,
    Ready,
    Installing,
    RestartRequired,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum UpdateOperation {
    Check,
    Download,
    Install,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BackgroundWork {
    None,
    Check,
    Download,
}

fn select_background_work<U>(session: &UpdaterSession<U>, enabled: bool) -> BackgroundWork {
    if session.suppressed_for_session
        || session.active_operation_id.is_some()
        || session.snapshot.phase == UpdatePhase::RestartRequired
    {
        return BackgroundWork::None;
    }
    match session.snapshot.phase {
        UpdatePhase::Available if enabled => BackgroundWork::Download,
        UpdatePhase::Available => BackgroundWork::None,
        UpdatePhase::Ready | UpdatePhase::Downloading | UpdatePhase::Installing => {
            BackgroundWork::None
        }
        UpdatePhase::Idle
        | UpdatePhase::Checking
        | UpdatePhase::UpToDate
        | UpdatePhase::Unavailable
        | UpdatePhase::Unsupported => BackgroundWork::Check,
        UpdatePhase::RestartRequired => BackgroundWork::None,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum UpdateProgressPhase {
    Downloading,
    Verifying,
    Installing,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct UpdateSnapshot {
    revision: u64,
    phase: UpdatePhase,
    operation: Option<UpdateOperation>,
    current_version: String,
    version: Option<String>,
    notes: Option<String>,
    progress_phase: Option<UpdateProgressPhase>,
    downloaded_bytes: u64,
    total_bytes: Option<u64>,
    error: Option<String>,
    reason: Option<&'static str>,
    auto_download_enabled: bool,
}

#[derive(Clone, Debug)]
struct UpdateMetadata {
    current_version: String,
    version: String,
    notes: Option<String>,
}

impl UpdateMetadata {
    fn from_update(update: &Update) -> Self {
        Self {
            current_version: update.current_version.clone(),
            version: update.version.clone(),
            notes: update.body.clone(),
        }
    }
}

enum UpdateResource<U> {
    Available {
        update: U,
        metadata: UpdateMetadata,
    },
    Ready {
        update: U,
        bytes: Vec<u8>,
        metadata: UpdateMetadata,
    },
}

#[derive(Default)]
struct InstallExitCoordinator {
    phase: InstallExitPhase,
}

#[derive(Default)]
enum InstallExitPhase {
    #[default]
    Open,
    InstallationReserved,
    ExitCommitted,
}

impl InstallExitCoordinator {
    fn reserve_installation(&mut self) -> Result<(), &'static str> {
        match self.phase {
            InstallExitPhase::Open => {
                self.phase = InstallExitPhase::InstallationReserved;
                Ok(())
            }
            InstallExitPhase::InstallationReserved => {
                Err("update installation is already reserved")
            }
            InstallExitPhase::ExitCommitted => Err("application exit is already committed"),
        }
    }

    fn release_installation(&mut self) {
        if matches!(self.phase, InstallExitPhase::InstallationReserved) {
            self.phase = InstallExitPhase::Open;
        }
    }

    fn handle_exit_request(&mut self) -> bool {
        match self.phase {
            InstallExitPhase::InstallationReserved => true,
            InstallExitPhase::Open | InstallExitPhase::ExitCommitted => {
                self.phase = InstallExitPhase::ExitCommitted;
                false
            }
        }
    }
}

struct UpdaterSession<U> {
    snapshot: UpdateSnapshot,
    resource: Option<UpdateResource<U>>,
    suppressed_for_session: bool,
    next_operation_id: u64,
    active_operation_id: Option<u64>,
    install_exit: InstallExitCoordinator,
    #[cfg(target_os = "windows")]
    installation_guard: Option<tokio::sync::OwnedRwLockWriteGuard<()>>,
}

impl<U> UpdaterSession<U> {
    fn new(current_version: String, auto_download_enabled: bool) -> Self {
        Self {
            snapshot: UpdateSnapshot {
                revision: 0,
                phase: UpdatePhase::Idle,
                operation: None,
                current_version,
                version: None,
                notes: None,
                progress_phase: None,
                downloaded_bytes: 0,
                total_bytes: None,
                error: None,
                reason: None,
                auto_download_enabled,
            },
            resource: None,
            suppressed_for_session: false,
            next_operation_id: 0,
            active_operation_id: None,
            install_exit: InstallExitCoordinator::default(),
            #[cfg(target_os = "windows")]
            installation_guard: None,
        }
    }

    fn reserve(&mut self, operation: UpdateOperation, manual: bool) -> Result<u64, String> {
        if self.snapshot.phase == UpdatePhase::RestartRequired {
            return Err(String::from(
                "restart is required before another update operation",
            ));
        }
        if self.active_operation_id.is_some() {
            return Err(String::from("another update operation is already running"));
        }
        match operation {
            UpdateOperation::Check if !manual && self.suppressed_for_session => {
                return Err(String::from(
                    "automatic update checks are deferred for this session",
                ));
            }
            UpdateOperation::Check if self.resource.is_some() => {
                return Err(String::from(
                    "a downloaded or available update is already retained",
                ));
            }
            UpdateOperation::Download
                if !matches!(self.resource, Some(UpdateResource::Available { .. })) =>
            {
                return Err(String::from("there is no update available to download"));
            }
            UpdateOperation::Install
                if !matches!(self.resource, Some(UpdateResource::Ready { .. })) =>
            {
                return Err(String::from("there is no verified update ready to install"));
            }
            _ => {}
        }
        if operation == UpdateOperation::Check && manual {
            self.suppressed_for_session = false;
        }
        self.next_operation_id = self.next_operation_id.saturating_add(1);
        self.active_operation_id = Some(self.next_operation_id);
        self.snapshot.operation = Some(operation);
        self.snapshot.error = None;
        self.snapshot.reason = None;
        match operation {
            UpdateOperation::Check => self.snapshot.phase = UpdatePhase::Checking,
            UpdateOperation::Download => {
                self.snapshot.phase = UpdatePhase::Downloading;
                self.snapshot.progress_phase = Some(UpdateProgressPhase::Downloading);
                self.snapshot.downloaded_bytes = 0;
                self.snapshot.total_bytes = None;
            }
            UpdateOperation::Install => {
                self.snapshot.phase = UpdatePhase::Installing;
                self.snapshot.progress_phase = Some(UpdateProgressPhase::Installing);
            }
        }
        self.bump();
        Ok(self.next_operation_id)
    }

    fn finish_check(&mut self, id: u64, update: Option<U>, metadata: Option<UpdateMetadata>) {
        if !self.finish(id) {
            return;
        }
        self.resource = update
            .zip(metadata.clone())
            .map(|(update, metadata)| UpdateResource::Available { update, metadata });
        if let Some(metadata) = metadata {
            self.snapshot.phase = UpdatePhase::Available;
            self.set_metadata(&metadata);
        } else {
            self.snapshot.phase = UpdatePhase::UpToDate;
            self.clear_update_fields();
        }
    }

    fn finish_unsupported(&mut self, reason: &'static str) {
        self.snapshot.phase = UpdatePhase::Unsupported;
        self.snapshot.operation = None;
        self.snapshot.reason = Some(reason);
        self.resource = None;
        self.clear_update_fields();
        self.bump();
    }

    fn finish_unavailable(&mut self, id: u64) {
        if !self.finish(id) {
            return;
        }
        self.snapshot.phase = UpdatePhase::Unavailable;
        self.snapshot.reason = Some("No updater-enabled release is published yet.");
        self.snapshot.error = None;
        self.resource = None;
        self.clear_update_fields();
    }

    fn progress(&mut self, id: u64, chunk: usize, total: Option<u64>) -> bool {
        if self.active_operation_id != Some(id)
            || self.snapshot.operation != Some(UpdateOperation::Download)
        {
            return false;
        }
        self.snapshot.downloaded_bytes =
            self.snapshot.downloaded_bytes.saturating_add(chunk as u64);
        self.snapshot.total_bytes = total;
        self.snapshot.progress_phase = Some(UpdateProgressPhase::Downloading);
        self.bump();
        true
    }

    fn verifying(&mut self, id: u64) -> bool {
        if self.active_operation_id != Some(id) {
            return false;
        }
        self.snapshot.progress_phase = Some(UpdateProgressPhase::Verifying);
        self.bump();
        true
    }

    fn finish_download(&mut self, id: u64, bytes: Vec<u8>) {
        if !self.finish(id) {
            return;
        }
        if let Some(UpdateResource::Available { update, metadata }) = self.resource.take() {
            self.snapshot.phase = UpdatePhase::Ready;
            self.snapshot.progress_phase = None;
            self.resource = Some(UpdateResource::Ready {
                update,
                bytes,
                metadata,
            });
        }
    }

    fn finish_failure(&mut self, id: u64, message: String) {
        if !self.finish(id) {
            return;
        }
        self.snapshot.phase = match self.resource {
            Some(UpdateResource::Available { .. }) => UpdatePhase::Available,
            Some(UpdateResource::Ready { .. }) => UpdatePhase::Ready,
            None => UpdatePhase::Idle,
        };
        self.snapshot.progress_phase = None;
        self.snapshot.error = Some(message);
    }

    fn defer(&mut self) -> Result<(), String> {
        if self.active_operation_id.is_some() {
            return Err(String::from("another update operation is already running"));
        }
        if !matches!(self.resource, Some(UpdateResource::Ready { .. })) {
            return Err(String::from("there is no verified update ready to defer"));
        }
        self.resource = None;
        self.suppressed_for_session = true;
        self.snapshot.phase = UpdatePhase::Idle;
        self.snapshot.error = None;
        self.snapshot.reason = None;
        self.clear_update_fields();
        self.bump();
        Ok(())
    }

    fn set_auto_download(&mut self, enabled: bool) {
        self.snapshot.auto_download_enabled = enabled;
        self.bump();
    }

    fn take_ready(&mut self, id: u64) -> Option<(U, Vec<u8>, UpdateMetadata)> {
        if self.active_operation_id != Some(id) {
            return None;
        }
        match self.resource.take()? {
            UpdateResource::Ready {
                update,
                bytes,
                metadata,
            } => Some((update, bytes, metadata)),
            available => {
                self.resource = Some(available);
                None
            }
        }
    }

    fn restore_ready(
        &mut self,
        id: u64,
        update: U,
        bytes: Vec<u8>,
        metadata: UpdateMetadata,
        error: String,
    ) {
        self.resource = Some(UpdateResource::Ready {
            update,
            bytes,
            metadata,
        });
        self.finish_failure(id, error);
    }

    #[cfg(any(not(target_os = "windows"), test))]
    fn finish_unrecoverable_install_failure(&mut self, id: u64, error: String) {
        self.install_exit.release_installation();
        self.resource = None;
        self.finish_failure(id, error);
    }

    #[cfg(target_os = "windows")]
    fn finish_restart_required(
        &mut self,
        id: u64,
        guard: tokio::sync::OwnedRwLockWriteGuard<()>,
        message: String,
    ) {
        if self.finish(id) {
            retain_recovery_state(
                &mut self.snapshot,
                &mut self.resource,
                &mut self.installation_guard,
                guard,
                message,
            );
        }
    }

    fn finish(&mut self, id: u64) -> bool {
        if self.active_operation_id != Some(id) {
            return false;
        }
        self.active_operation_id = None;
        self.snapshot.operation = None;
        self.bump();
        true
    }

    fn set_metadata(&mut self, metadata: &UpdateMetadata) {
        self.snapshot.current_version = metadata.current_version.clone();
        self.snapshot.version = Some(metadata.version.clone());
        self.snapshot.notes = metadata.notes.clone();
        self.snapshot.downloaded_bytes = 0;
        self.snapshot.total_bytes = None;
        self.snapshot.progress_phase = None;
    }

    fn clear_update_fields(&mut self) {
        self.snapshot.version = None;
        self.snapshot.notes = None;
        self.snapshot.progress_phase = None;
        self.snapshot.downloaded_bytes = 0;
        self.snapshot.total_bytes = None;
    }

    fn bump(&mut self) {
        self.snapshot.revision = self.snapshot.revision.saturating_add(1);
    }
}

#[cfg(any(target_os = "windows", test))]
fn retain_recovery_state<U, G>(
    snapshot: &mut UpdateSnapshot,
    resource: &mut Option<UpdateResource<U>>,
    retained_guard: &mut Option<G>,
    guard: G,
    message: String,
) {
    *resource = None;
    snapshot.phase = UpdatePhase::RestartRequired;
    snapshot.progress_phase = None;
    snapshot.error = Some(message);
    snapshot.reason =
        Some("The installer could not be started. Restart VoxGolem before using it again.");
    *retained_guard = Some(guard);
}

pub(crate) struct PendingUpdate {
    session: Mutex<UpdaterSession<Update>>,
    auto_download_enabled: AtomicBool,
}

impl Default for PendingUpdate {
    fn default() -> Self {
        Self {
            session: Mutex::new(UpdaterSession::new(String::new(), true)),
            auto_download_enabled: AtomicBool::new(true),
        }
    }
}

impl PendingUpdate {
    pub(crate) fn handle_exit_request(&self) -> bool {
        self.session
            .lock()
            .map(|mut session| session.install_exit.handle_exit_request())
            .unwrap_or(true)
    }
}

#[tauri::command]
pub(crate) fn get_update_snapshot(
    app: AppHandle,
    pending: State<'_, PendingUpdate>,
) -> Result<UpdateSnapshot, String> {
    let mut session = lock_session(&pending)?;
    initialize_version(&mut session, &app);
    Ok(session.snapshot.clone())
}

#[tauri::command]
pub(crate) async fn check_for_update(
    app: AppHandle,
    pending: State<'_, PendingUpdate>,
) -> Result<UpdateSnapshot, String> {
    perform_check(&app, &pending, true).await
}

#[tauri::command]
pub(crate) async fn download_update(
    app: AppHandle,
    pending: State<'_, PendingUpdate>,
) -> Result<UpdateSnapshot, String> {
    perform_download(&app, &pending).await
}

#[tauri::command]
pub(crate) fn discard_update(
    app: AppHandle,
    pending: State<'_, PendingUpdate>,
) -> Result<UpdateSnapshot, String> {
    let snapshot = {
        let mut session = lock_session(&pending)?;
        session.defer()?;
        session.snapshot.clone()
    };
    emit_snapshot(&app, &snapshot);
    Ok(snapshot)
}

#[tauri::command]
pub(crate) fn set_auto_update_download(
    app: AppHandle,
    pending: State<'_, PendingUpdate>,
    enabled: bool,
) -> Result<UpdateSnapshot, String> {
    crate::persist_auto_update_download(enabled)?;
    pending
        .auto_download_enabled
        .store(enabled, Ordering::Release);
    let snapshot = {
        let mut session = lock_session(&pending)?;
        session.set_auto_download(enabled);
        session.snapshot.clone()
    };
    emit_snapshot(&app, &snapshot);
    if enabled {
        start_background_update(app);
    }
    Ok(snapshot)
}

#[tauri::command]
pub(crate) async fn install_update(
    app: AppHandle,
    pending: State<'_, PendingUpdate>,
    app_state: State<'_, crate::AppState>,
) -> Result<UpdateSnapshot, String> {
    crate::ensure_update_installation_is_idle(&app_state)?;
    let (id, update, bytes, metadata, installing) = {
        let mut session = lock_session(&pending)?;
        session
            .install_exit
            .reserve_installation()
            .map_err(String::from)?;
        let id = match session.reserve(UpdateOperation::Install, true) {
            Ok(id) => id,
            Err(error) => {
                session.install_exit.release_installation();
                return Err(error);
            }
        };
        let Some((update, bytes, metadata)) = session.take_ready(id) else {
            session.install_exit.release_installation();
            return Err(String::from("verified update resources are unavailable"));
        };
        let snapshot = session.snapshot.clone();
        (id, update, bytes, metadata, snapshot)
    };
    emit_snapshot(&app, &installing);

    let installation_guard = match crate::begin_update_installation(&app_state) {
        Ok(guard) => guard,
        Err(error) => {
            restore_install(&pending, id, update, bytes, metadata, error.clone())?;
            emit_current_snapshot(&app, &pending)?;
            return Err(error);
        }
    };
    if let Err(error) = crate::ensure_update_installation_is_idle(&app_state) {
        drop(installation_guard);
        restore_install(&pending, id, update, bytes, metadata, error.clone())?;
        emit_current_snapshot(&app, &pending)?;
        return Err(error);
    }

    #[cfg(target_os = "windows")]
    let cleanup_result = {
        let cleanup_app = app.clone();
        tauri::async_runtime::spawn(async move {
            crate::shutdown_runtime_for_update(&cleanup_app.state::<crate::AppState>()).await;
        })
        .await
        .map_err(|error| format!("update cleanup task failed: {error}"))
    };
    #[cfg(not(target_os = "windows"))]
    let cleanup_result = Ok(());

    let install_task = tauri::async_runtime::spawn_blocking(move || {
        let result = run_cleanup_install_handoff(
            || cleanup_result,
            || update.install(&bytes).map_err(updater_error),
        );
        (result, update, bytes)
    })
    .await;
    let install_result = match install_task {
        Ok(result) => result,
        Err(error) => {
            let message = format!("update installer task failed: {error}");
            let snapshot = {
                let mut session = lock_session(&pending)?;
                #[cfg(target_os = "windows")]
                {
                    session.install_exit.release_installation();
                    session.finish_restart_required(id, installation_guard, message);
                }
                #[cfg(not(target_os = "windows"))]
                {
                    drop(installation_guard);
                    session.finish_unrecoverable_install_failure(id, message);
                }
                session.snapshot.clone()
            };
            emit_snapshot(&app, &snapshot);
            return Ok(snapshot);
        }
    };

    match install_result {
        (Ok(()), _, _) => {
            drop(installation_guard);
            let _snapshot = {
                let mut session = lock_session(&pending)?;
                session.install_exit.release_installation();
                session.finish(id);
                session.snapshot.clone()
            };
            #[cfg(target_os = "linux")]
            app.restart();
            #[allow(unreachable_code)]
            Ok(_snapshot)
        }
        (Err(error), update, bytes) => {
            let message = format!("failed to install update: {error}");
            let snapshot = {
                let mut session = lock_session(&pending)?;
                session.install_exit.release_installation();
                #[cfg(target_os = "windows")]
                {
                    drop((update, bytes));
                    session.finish_restart_required(id, installation_guard, message);
                }
                #[cfg(not(target_os = "windows"))]
                {
                    drop(installation_guard);
                    session.restore_ready(id, update, bytes, metadata, message);
                }
                session.snapshot.clone()
            };
            emit_snapshot(&app, &snapshot);
            Ok(snapshot)
        }
    }
}

fn restore_install(
    pending: &PendingUpdate,
    id: u64,
    update: Update,
    bytes: Vec<u8>,
    metadata: UpdateMetadata,
    error: String,
) -> Result<(), String> {
    let mut session = lock_session(pending)?;
    session.install_exit.release_installation();
    session.restore_ready(id, update, bytes, metadata, error);
    Ok(())
}

fn run_cleanup_install_handoff<C, I>(cleanup: C, install: I) -> Result<(), String>
where
    C: FnOnce() -> Result<(), String>,
    I: FnOnce() -> Result<(), String>,
{
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        cleanup()?;
        install()
    }))
    .unwrap_or_else(|_| Err(String::from("update cleanup or installer panicked")))
}

#[tauri::command]
pub(crate) fn restart_for_update(
    app: AppHandle,
    pending: State<'_, PendingUpdate>,
) -> Result<(), String> {
    let session = lock_session(&pending)?;
    #[cfg(target_os = "windows")]
    let required = session.snapshot.phase == UpdatePhase::RestartRequired;
    #[cfg(not(target_os = "windows"))]
    let required = false;
    if !required {
        return Err(String::from("an updater recovery restart is not required"));
    }
    drop(session);
    app.request_restart();
    Ok(())
}

pub(crate) fn start_background_update(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        let pending = app.state::<PendingUpdate>();
        let work = lock_session(&pending)
            .map(|session| {
                select_background_work(
                    &session,
                    pending.auto_download_enabled.load(Ordering::Acquire),
                )
            })
            .unwrap_or(BackgroundWork::None);
        match work {
            BackgroundWork::None => {}
            BackgroundWork::Download => {
                let _ = perform_download(&app, &pending).await;
            }
            BackgroundWork::Check => {
                if perform_check(&app, &pending, false).await.is_ok()
                    && lock_session(&pending)
                        .map(|session| {
                            select_background_work(
                                &session,
                                pending.auto_download_enabled.load(Ordering::Acquire),
                            ) == BackgroundWork::Download
                        })
                        .unwrap_or(false)
                {
                    let _ = perform_download(&app, &pending).await;
                }
            }
        }
    });
}

pub(crate) fn configure_auto_download(pending: &PendingUpdate, enabled: bool) {
    pending
        .auto_download_enabled
        .store(enabled, Ordering::Release);
    if let Ok(mut session) = pending.session.lock() {
        session.snapshot.auto_download_enabled = enabled;
    }
}

async fn perform_check(
    app: &AppHandle,
    pending: &PendingUpdate,
    manual: bool,
) -> Result<UpdateSnapshot, String> {
    let id = {
        let mut session = lock_session(pending)?;
        initialize_version(&mut session, app);
        if session.snapshot.phase == UpdatePhase::RestartRequired {
            return Err(String::from(
                "restart is required before another update operation",
            ));
        }
        if let Err(reason) =
            update_installation_support(std::env::consts::OS, current_bundle_type())
        {
            session.finish_unsupported(reason);
            let snapshot = session.snapshot.clone();
            drop(session);
            emit_snapshot(app, &snapshot);
            return Ok(snapshot);
        }
        match session.reserve(UpdateOperation::Check, manual) {
            Ok(id) => {
                let snapshot = session.snapshot.clone();
                drop(session);
                emit_snapshot(app, &snapshot);
                id
            }
            Err(_error) if !manual && session.suppressed_for_session => {
                return Ok(session.snapshot.clone());
            }
            Err(error) => return Err(error),
        }
    };

    let checked = match app
        .updater_builder()
        .timeout(UPDATE_CHECK_TIMEOUT)
        .on_before_exit(|| {})
        .build()
    {
        Ok(updater) => updater.check().await,
        Err(error) => Err(error),
    };
    let snapshot = {
        let mut session = lock_session(pending)?;
        match checked {
            Ok(Some(mut update)) => {
                update.timeout = Some(UPDATE_DOWNLOAD_TIMEOUT);
                let metadata = UpdateMetadata::from_update(&update);
                session.finish_check(id, Some(update), Some(metadata));
            }
            Ok(None) => session.finish_check(id, None, None),
            Err(UpdaterError::ReleaseNotFound) => session.finish_unavailable(id),
            Err(error) => session.finish_failure(id, updater_error(error)),
        }
        session.snapshot.clone()
    };
    emit_snapshot(app, &snapshot);
    Ok(snapshot)
}

async fn perform_download(
    app: &AppHandle,
    pending: &PendingUpdate,
) -> Result<UpdateSnapshot, String> {
    let (id, update, metadata) = {
        let mut session = lock_session(pending)?;
        let id = session.reserve(UpdateOperation::Download, true)?;
        let snapshot = session.snapshot.clone();
        let Some(UpdateResource::Available { update, metadata }) = session.resource.take() else {
            return Err(String::from("update resource is unavailable"));
        };
        drop(session);
        emit_snapshot(app, &snapshot);
        (id, update, metadata)
    };

    let progress_app = app.clone();
    let finish_app = app.clone();
    let downloaded = update
        .download(
            move |chunk, total| {
                if let Ok(mut session) = progress_app.state::<PendingUpdate>().session.lock() {
                    if session.progress(id, chunk, total) {
                        emit_snapshot(&progress_app, &session.snapshot);
                    }
                }
            },
            move || {
                if let Ok(mut session) = finish_app.state::<PendingUpdate>().session.lock() {
                    if session.verifying(id) {
                        emit_snapshot(&finish_app, &session.snapshot);
                    }
                }
            },
        )
        .await;

    let snapshot = {
        let mut session = lock_session(pending)?;
        session.resource = Some(UpdateResource::Available { update, metadata });
        match downloaded {
            Ok(bytes) => session.finish_download(id, bytes),
            Err(error) => session.finish_failure(
                id,
                format!(
                    "failed to download or verify update: {}",
                    updater_error(error)
                ),
            ),
        }
        session.snapshot.clone()
    };
    emit_snapshot(app, &snapshot);
    Ok(snapshot)
}

fn current_bundle_type() -> Option<BundleType> {
    tauri::utils::platform::bundle_type()
}

fn update_installation_support(
    operating_system: &str,
    bundle_type: Option<BundleType>,
) -> Result<(), &'static str> {
    match (operating_system, bundle_type) {
        ("linux", Some(BundleType::AppImage)) | ("windows", Some(BundleType::Nsis)) => Ok(()),
        ("linux", _) => Err("Automatic updates require the Linux AppImage."),
        ("windows", _) => Err("Automatic updates require the installed Windows application."),
        _ => Err("Automatic updates are currently supported only on Linux and Windows."),
    }
}

fn initialize_version<U>(session: &mut UpdaterSession<U>, app: &AppHandle) {
    if session.snapshot.current_version.is_empty() {
        session.snapshot.current_version = app.package_info().version.to_string();
    }
}

fn emit_current_snapshot(app: &AppHandle, pending: &PendingUpdate) -> Result<(), String> {
    let snapshot = lock_session(pending)?.snapshot.clone();
    emit_snapshot(app, &snapshot);
    Ok(())
}

fn emit_snapshot(app: &AppHandle, snapshot: &UpdateSnapshot) {
    let _ = app.emit(UPDATE_SNAPSHOT_EVENT, snapshot);
}

fn lock_session(
    pending: &PendingUpdate,
) -> Result<std::sync::MutexGuard<'_, UpdaterSession<Update>>, String> {
    pending
        .session
        .lock()
        .map_err(|_| String::from("updater state lock is poisoned"))
}

fn updater_error(error: impl std::fmt::Display) -> String {
    error.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session() -> UpdaterSession<()> {
        UpdaterSession::new(String::from("1.0.0"), true)
    }

    fn metadata() -> UpdateMetadata {
        UpdateMetadata {
            current_version: String::from("1.0.0"),
            version: String::from("2.0.0"),
            notes: Some(String::new()),
        }
    }

    #[test]
    fn revisions_advance_for_operation_progress_and_completion() {
        let mut session = session();
        session.resource = Some(UpdateResource::Available {
            update: (),
            metadata: metadata(),
        });
        session.snapshot.phase = UpdatePhase::Available;
        let initial = session.snapshot.revision;
        let id = session.reserve(UpdateOperation::Download, true).unwrap();
        let reserved = session.snapshot.revision;
        assert!(session.progress(id, 4, Some(8)));
        let progressed = session.snapshot.revision;
        session.finish_download(id, vec![1, 2, 3, 4]);
        assert!(reserved > initial);
        assert!(progressed > reserved);
        assert!(session.snapshot.revision > progressed);
        assert_eq!(session.snapshot.phase, UpdatePhase::Ready);
    }

    #[test]
    fn download_failure_restores_available_resource_and_records_error() {
        let mut session = session();
        session.resource = Some(UpdateResource::Available {
            update: (),
            metadata: metadata(),
        });
        session.snapshot.phase = UpdatePhase::Available;
        let id = session.reserve(UpdateOperation::Download, true).unwrap();
        session.finish_failure(id, String::from("offline"));
        assert_eq!(session.snapshot.phase, UpdatePhase::Available);
        assert_eq!(session.snapshot.error.as_deref(), Some("offline"));
        assert!(matches!(
            session.resource,
            Some(UpdateResource::Available { .. })
        ));
    }

    #[test]
    fn later_drops_verified_resource_and_suppresses_background_checks() {
        let mut session = session();
        session.resource = Some(UpdateResource::Ready {
            update: (),
            bytes: vec![1],
            metadata: metadata(),
        });
        session.snapshot.phase = UpdatePhase::Ready;
        session.defer().unwrap();
        assert!(session.resource.is_none());
        assert!(session.suppressed_for_session);
        assert!(session.reserve(UpdateOperation::Check, false).is_err());
        assert!(session.reserve(UpdateOperation::Check, true).is_ok());
    }

    #[test]
    fn manual_check_does_not_reserve_a_download() {
        let mut session = session();
        let id = session.reserve(UpdateOperation::Check, true).unwrap();
        session.finish_check(id, Some(()), Some(metadata()));
        assert_eq!(session.snapshot.phase, UpdatePhase::Available);
        assert_eq!(session.snapshot.operation, None);
    }

    #[test]
    fn missing_release_state_is_retained_for_hydration_without_an_error() {
        let mut session = session();
        let id = session.reserve(UpdateOperation::Check, false).unwrap();
        session.finish_unavailable(id);
        assert_eq!(session.snapshot.phase, UpdatePhase::Unavailable);
        assert_eq!(session.snapshot.error, None);
        assert!(session.snapshot.reason.is_some());
    }

    #[test]
    fn recovery_state_rejects_all_update_operations_without_changing_the_snapshot() {
        let mut session = session();
        session.resource = Some(UpdateResource::Ready {
            update: (),
            bytes: vec![1, 2, 3],
            metadata: metadata(),
        });
        let mut retained_guard = None;
        retain_recovery_state(
            &mut session.snapshot,
            &mut session.resource,
            &mut retained_guard,
            "synthetic write guard",
            String::from("installer launch failed"),
        );
        let before = session.snapshot.clone();

        assert!(session.reserve(UpdateOperation::Check, true).is_err());
        assert!(session.reserve(UpdateOperation::Check, false).is_err());
        assert!(session.reserve(UpdateOperation::Download, true).is_err());
        assert!(session.reserve(UpdateOperation::Install, true).is_err());
        assert_eq!(session.snapshot, before);
        assert_eq!(retained_guard, Some("synthetic write guard"));
        assert!(session.resource.is_none());
    }

    #[test]
    fn background_work_reuses_available_resource_and_preserves_ready_or_recovery_state() {
        let mut available = session();
        available.snapshot.phase = UpdatePhase::Available;
        available.resource = Some(UpdateResource::Available {
            update: (),
            metadata: metadata(),
        });
        assert_eq!(
            select_background_work(&available, true),
            BackgroundWork::Download
        );
        let download = available.reserve(UpdateOperation::Download, true).unwrap();
        assert_eq!(
            select_background_work(&available, true),
            BackgroundWork::None
        );
        assert!(available.reserve(UpdateOperation::Download, true).is_err());
        available.finish_failure(download, String::from("offline"));

        let mut ready = session();
        ready.snapshot.phase = UpdatePhase::Ready;
        ready.resource = Some(UpdateResource::Ready {
            update: (),
            bytes: vec![1, 2, 3],
            metadata: metadata(),
        });
        assert_eq!(select_background_work(&ready, true), BackgroundWork::None);
        assert!(matches!(ready.resource, Some(UpdateResource::Ready { .. })));

        let mut retained_guard = None;
        retain_recovery_state(
            &mut ready.snapshot,
            &mut ready.resource,
            &mut retained_guard,
            "guard",
            String::from("install failed"),
        );
        assert_eq!(select_background_work(&ready, true), BackgroundWork::None);
        assert_eq!(retained_guard, Some("guard"));
    }

    #[test]
    fn authoritative_check_reservation_cannot_replace_available_or_ready_resources() {
        let mut available = session();
        available.snapshot.phase = UpdatePhase::Available;
        available.resource = Some(UpdateResource::Available {
            update: (),
            metadata: metadata(),
        });
        assert!(available.reserve(UpdateOperation::Check, false).is_err());

        let mut ready = session();
        ready.snapshot.phase = UpdatePhase::Ready;
        ready.resource = Some(UpdateResource::Ready {
            update: (),
            bytes: vec![1, 2, 3],
            metadata: metadata(),
        });
        assert!(ready.reserve(UpdateOperation::Check, true).is_err());
        assert!(matches!(ready.resource, Some(UpdateResource::Ready { .. })));
    }

    #[test]
    fn disabled_auto_download_still_checks_metadata_but_does_not_download() {
        let initial = session();
        assert_eq!(
            select_background_work(&initial, false),
            BackgroundWork::Check
        );

        let mut available = session();
        available.snapshot.phase = UpdatePhase::Available;
        available.resource = Some(UpdateResource::Available {
            update: (),
            metadata: metadata(),
        });
        assert_eq!(
            select_background_work(&available, false),
            BackgroundWork::None
        );

        let mut deferred = session();
        deferred.snapshot.phase = UpdatePhase::Ready;
        deferred.resource = Some(UpdateResource::Ready {
            update: (),
            bytes: vec![1],
            metadata: metadata(),
        });
        deferred.defer().unwrap();
        assert_eq!(
            select_background_work(&deferred, false),
            BackgroundWork::None
        );
    }

    #[test]
    fn lost_installer_task_releases_exit_reservation_and_leaves_a_retryable_snapshot() {
        let mut session = session();
        session.snapshot.phase = UpdatePhase::Ready;
        session.resource = Some(UpdateResource::Ready {
            update: (),
            bytes: vec![1, 2, 3],
            metadata: metadata(),
        });
        session.install_exit.reserve_installation().unwrap();
        let id = session.reserve(UpdateOperation::Install, true).unwrap();
        let _lost_resources = session.take_ready(id).unwrap();

        session.finish_unrecoverable_install_failure(id, String::from("installer task lost"));

        assert_eq!(session.snapshot.phase, UpdatePhase::Idle);
        assert_eq!(session.snapshot.operation, None);
        assert_eq!(
            session.snapshot.error.as_deref(),
            Some("installer task lost")
        );
        assert!(!session.install_exit.handle_exit_request());
    }

    #[test]
    fn only_appimage_and_nsis_bundle_markers_are_update_eligible() {
        assert!(update_installation_support("linux", Some(BundleType::AppImage)).is_ok());
        assert!(update_installation_support("windows", Some(BundleType::Nsis)).is_ok());
        assert!(update_installation_support("linux", Some(BundleType::Deb)).is_err());
    }

    #[test]
    fn accepted_exit_prevents_a_later_install_reservation() {
        let mut coordinator = InstallExitCoordinator::default();
        assert!(!coordinator.handle_exit_request());
        assert_eq!(
            coordinator.reserve_installation().unwrap_err(),
            "application exit is already committed"
        );
    }

    #[test]
    fn cleanup_failure_prevents_installer_launch() {
        let started = AtomicBool::new(false);
        let result = run_cleanup_install_handoff(
            || Err(String::from("cleanup failed")),
            || {
                started.store(true, Ordering::SeqCst);
                Ok(())
            },
        );
        assert_eq!(result, Err(String::from("cleanup failed")));
        assert!(!started.load(Ordering::SeqCst));
    }
}
