#![deny(unsafe_op_in_unsafe_fn)]
#![deny(unused_must_use)]

use base64::Engine;
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tauri::ipc::Channel;
use tauri::{Emitter, Manager};

#[cfg(windows)]
use std::os::windows::ffi::OsStrExt;
#[cfg(target_os = "linux")]
use webkit2gtk::glib::prelude::Cast;
#[cfg(target_os = "linux")]
use webkit2gtk::UserMediaPermissionRequest;
#[cfg(target_os = "linux")]
use webkit2gtk::{PermissionRequestExt, UserMediaPermissionRequestExt, WebViewExt};

#[cfg(windows)]
#[link(name = "kernel32")]
unsafe extern "system" {
    fn MoveFileExW(existing: *const u16, new: *const u16, flags: u32) -> i32;
}

#[cfg(windows)]
const MOVEFILE_REPLACE_EXISTING: u32 = 0x1;
#[cfg(windows)]
const MOVEFILE_WRITE_THROUGH: u32 = 0x8;

mod app_updates;
mod livekit_wakeword;
mod partial_transcription;
mod telemetry;
mod transcription;
#[allow(dead_code)]
mod tts;
mod voice_activity;
mod wake_word;

const DEFAULT_SILENCE_TIMEOUT_MS: u64 = 1_500;
const DEFAULT_PREROLL_MAX_SAMPLES: usize = 4_000;
const DEFAULT_UTTERANCE_MAX_SAMPLES: usize = 4_800_000;
const PARTIAL_TRANSCRIPTION_MINIMUM_SAMPLES: usize = 8_000;
const PARTIAL_TRANSCRIPTION_MAXIMUM_SAMPLES: usize = 480_000;
const PARTIAL_TRANSCRIPTION_THROTTLE: Duration = Duration::from_millis(350);
const COMPLETION_PROMPT_MAX_BYTES: usize = 32 * 1024;
const DEFAULT_TELEMETRY_MAX_BYTES: usize = 10 * 1024 * 1024;
const DEFAULT_TELEMETRY_BACKUP_COUNT: u8 = 3;
const LLAMA_CPP_MODEL_ALIAS: &str = "default";
const LLAMA_CPP_MAX_TOKENS: u16 = 512;
const LLAMA_CPP_CONTEXT_WINDOW_TOKENS: usize = 8_192;
const LLAMA_CPP_CONTEXT_SAFETY_MARGIN_TOKENS: usize = 512;
const LLAMA_CPP_CHAT_WRAPPER_TOKENS: usize = 64;
const RESPONSE_PROFILE_STATE_FILE: &str = "state.toml";
const RUNTIME_LOG_DIR: &str = "logs";
const RUNTIME_LOG_FILE: &str = "runtime.log";
const RUNTIME_LOG_MESSAGE_MAX_CHARS: usize = 16_384;
const PROMPT_MAX_BYTES: usize = 64 * 1024;
const PROVIDER_HISTORY_MAX_BYTES: usize = 512 * 1024;
const WINDOWS_SOUL_FILE_NAME: &str = "SOUL.md";
const OPENCODE_PROMPT_INACTIVITY_TIMEOUT: Duration = Duration::from_secs(300);
const CUE_AUDIO_DATA_URL_PREFIX: &str = "data:audio/wav;base64,";
const START_LISTENING_CUE_WAV: &[u8] = include_bytes!("../resources/start-listening.wav");
const STOP_LISTENING_CUE_WAV: &[u8] = include_bytes!("../resources/stop-listening.wav");
const STARTUP_READY_MARKER: &str = "VOXGOLEM_STARTUP_READY";
static PERSISTED_STATE_LOCK: Mutex<()> = Mutex::new(());
static TELEMETRY_ERROR_REPORTED: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Debug, PartialEq, Eq)]
struct LlamaConversationTurn {
    user: String,
    assistant: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct LlamaPromptInput {
    user_prompt: String,
    rolled_over: bool,
}

type LlamaStartupRegistry = Arc<
    Mutex<
        Vec<(
            voxgolem_platform::llama_cpp::LlamaCppStartupCancellation,
            std::thread::JoinHandle<()>,
        )>,
    >,
>;

struct AppState {
    // Synchronous admission order: prompt_publication_gate -> lifecycle_gate -> active_prompt
    // -> conversation_session. The lifecycle gate also bounds every AppSnapshot publication.
    // Never await or perform provider/process I/O while holding these locks.
    lifecycle_gate: Arc<Mutex<()>>,
    startup_state: Arc<Mutex<StartupStatePayload>>,
    runtime_config: Option<voxgolem_core::config::RuntimeConfig>,
    selected_response_profile: Arc<Mutex<ResponseProfilePayload>>,
    ui_preferences: Mutex<UiPreferencesState>,
    settings_operation_lock: Arc<tokio::sync::Mutex<()>>,
    ui_preferences_operation_lock: Mutex<()>,
    supported_response_profiles: Vec<ResponseProfilePayload>,
    response_profile_switch_generation: Arc<AtomicU64>,
    response_backend_operation_lock: Mutex<()>,
    voice_pipeline_config: voxgolem_core::voice_pipeline::VoicePipelineConfig,
    voice_pipeline_state: Arc<Mutex<voxgolem_core::voice_pipeline::VoicePipelineState>>,
    wake_word_runtime: Option<Mutex<wake_word::WakeWordRuntime>>,
    voice_activity_runtime: Option<Mutex<voice_activity::VoiceActivityRuntime>>,
    parakeet_runtime: Option<Arc<Mutex<transcription::ParakeetRuntime>>>,
    partial_transcription: Arc<Mutex<partial_transcription::PartialTranscriptionScheduler>>,
    partial_voice_session: AtomicU64,
    completion_runtime: Mutex<Option<voxgolem_platform::completion::CompletionRuntime>>,
    completion_request: Arc<Mutex<Option<voxgolem_platform::completion::CompletionRequestHandle>>>,
    completion_context: Arc<Mutex<Option<CompletionRequestContext>>>,
    completion_generation: AtomicU64,
    completion_lifecycle_lock: Mutex<()>,
    telemetry_sink: Option<Arc<Mutex<telemetry::TelemetrySink>>>,
    conversation_session: Arc<Mutex<voxgolem_core::conversation::ConversationSession>>,
    assistant_settings_generation: Arc<AtomicU64>,
    settings_publication_in_progress: Arc<AtomicBool>,
    reset_in_progress: Arc<AtomicBool>,
    reset_operation_lock: tokio::sync::Mutex<()>,
    prompt_cleanup: Arc<PromptCleanupTracker>,
    prefetch_cleanup: Arc<PromptCleanupTracker>,
    prompt_publication_gate: Arc<Mutex<()>>,
    prompt_publication_epoch: Arc<AtomicU64>,
    tts_operation_lock: tokio::sync::Mutex<()>,
    tts_startup_generation: Arc<AtomicU64>,
    local_tts_runtime: Mutex<Option<Arc<tts::LocalTtsRuntime>>>,
    llama_cpp_runtime: Arc<Mutex<Option<voxgolem_platform::llama_cpp::LlamaCppRuntime>>>,
    llama_cpp_conversation: Mutex<Vec<LlamaConversationTurn>>,
    llama_cpp_system_prompt: Option<String>,
    opencode_server: Arc<Mutex<Option<voxgolem_platform::opencode::OpencodeServer>>>,
    active_prompt: Arc<Mutex<Option<ActivePrompt>>>,
    active_prompt_generation: AtomicU64,
    prefetch_generation: AtomicU64,
    prefetch_cache: Mutex<Option<PrefetchEntry>>,
    prefetch_task: Mutex<Option<ActivePrefetch>>,
    llama_startups: LlamaStartupRegistry,
    exit_cleanup_started: Arc<AtomicBool>,
}

struct AppStateInit {
    startup_state: Arc<Mutex<StartupStatePayload>>,
    runtime_config: Option<voxgolem_core::config::RuntimeConfig>,
    selected_response_profile: Arc<Mutex<ResponseProfilePayload>>,
    supported_response_profiles: Vec<ResponseProfilePayload>,
    response_profile_switch_generation: Arc<AtomicU64>,
    voice_pipeline_config: voxgolem_core::voice_pipeline::VoicePipelineConfig,
    voice_pipeline_state: voxgolem_core::voice_pipeline::VoicePipelineState,
    wake_word_runtime: Option<Mutex<wake_word::WakeWordRuntime>>,
    voice_activity_runtime: Option<Mutex<voice_activity::VoiceActivityRuntime>>,
    parakeet_runtime: Option<Arc<Mutex<transcription::ParakeetRuntime>>>,
    lifecycle_gate: Arc<Mutex<()>>,
    telemetry_sink: Option<Arc<Mutex<telemetry::TelemetrySink>>>,
    assistant_policy: voxgolem_core::assistant::AssistantPolicy,
    local_tts_runtime: Option<tts::LocalTtsRuntime>,
    llama_cpp_runtime: Arc<Mutex<Option<voxgolem_platform::llama_cpp::LlamaCppRuntime>>>,
    llama_cpp_system_prompt: Option<String>,
    llama_startups: LlamaStartupRegistry,
    exit_cleanup_started: Arc<AtomicBool>,
}

#[derive(Clone)]
struct ActivePrompt {
    request_id: String,
    generation: u64,
    assistant_generation: voxgolem_core::conversation::ExecutionId,
    cancelled: Arc<AtomicBool>,
    cancellation_signal: Arc<tokio::sync::watch::Sender<bool>>,
    completion_signal: Arc<tokio::sync::Notify>,
    abort_client: Arc<Mutex<Option<voxgolem_platform::opencode::OpencodeClient>>>,
    deep_handle: Arc<Mutex<Option<tauri::async_runtime::JoinHandle<DeepStageResult>>>>,
    deep_abort: Arc<Mutex<Option<tokio::task::AbortHandle>>>,
    settled: Arc<AtomicBool>,
    terminal_published: Arc<AtomicBool>,
    publication_epoch: u64,
    terminal_kind: Arc<Mutex<Option<TerminalKind>>>,
    updates: Arc<Mutex<Option<Channel<PromptExecutionEventPayload>>>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TerminalKind {
    Completed,
    Refused,
    Failed,
    Cancelled,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CommitAssistantResult {
    Committed,
    Cancelled,
}

fn record_terminal_kind(
    terminal: &Mutex<Option<TerminalKind>>,
    kind: TerminalKind,
) -> TerminalKind {
    let Ok(mut terminal) = terminal.lock() else {
        return kind;
    };
    *terminal.get_or_insert(kind)
}

fn take_once<T>(slot: &Mutex<Option<T>>) -> Option<T> {
    slot.lock().ok()?.take()
}

struct ActivePromptGuard {
    active_prompt: Arc<Mutex<Option<ActivePrompt>>>,
    conversation_session: Arc<Mutex<voxgolem_core::conversation::ConversationSession>>,
    request_id: String,
    generation: u64,
    execution_id: voxgolem_core::conversation::ExecutionId,
    cancelled: Arc<AtomicBool>,
    armed: Arc<AtomicBool>,
    abort_client: Arc<Mutex<Option<voxgolem_platform::opencode::OpencodeClient>>>,
    prompt_cleanup: Arc<PromptCleanupTracker>,
    settled: Arc<AtomicBool>,
    lifecycle_gate: Arc<Mutex<()>>,
    terminal_kind: Arc<Mutex<Option<TerminalKind>>>,
    voice_pipeline_state: Arc<Mutex<voxgolem_core::voice_pipeline::VoicePipelineState>>,
    voice_pipeline_config: voxgolem_core::voice_pipeline::VoicePipelineConfig,
}

#[derive(Debug)]
enum PromptControlError {
    Cancelled,
    Error(String),
}

impl From<String> for PromptControlError {
    fn from(error: String) -> Self {
        Self::Error(error)
    }
}

impl From<PromptControlError> for String {
    fn from(error: PromptControlError) -> Self {
        error.into_message()
    }
}

impl PromptControlError {
    fn into_message(self) -> String {
        match self {
            Self::Error(error) => error,
            Self::Cancelled => String::from("assistant request cancelled"),
        }
    }
}

type ActivePromptRegistration = (
    u64,
    Arc<AtomicBool>,
    Arc<tokio::sync::watch::Sender<bool>>,
    Arc<AtomicBool>,
);

struct ResetAdmissionGuard {
    lifecycle_gate: Arc<Mutex<()>>,
    reset_in_progress: Arc<AtomicBool>,
}

struct SettingsPublicationGuard {
    in_progress: Arc<AtomicBool>,
    armed: bool,
}

struct ProfilePersistenceRollback {
    previous_profile: ResponseProfilePayload,
    previous_settings: AssistantSettingsPayload,
    exit_cleanup_started: Arc<AtomicBool>,
    switch_generation: Arc<AtomicU64>,
    expected_generation: u64,
    armed: bool,
}

struct ResetConversationExecution {
    cleanup: Option<PromptPhysicalCleanup>,
}

struct PromptAbortClaim {
    client: Option<voxgolem_platform::opencode::OpencodeClient>,
    cleanup_permit: Option<PromptCleanupPermit>,
}

struct PromptPhysicalCleanup {
    active: ActivePrompt,
    abort_claim: Option<PromptAbortClaim>,
    deep_handle: Option<tauri::async_runtime::JoinHandle<DeepStageResult>>,
    deep_abort: Option<tokio::task::AbortHandle>,
    cancel: bool,
}

#[derive(Default)]
struct PromptCleanupTracker {
    count: Mutex<usize>,
    notify: tokio::sync::Notify,
}

struct PromptCleanupPermit {
    tracker: Arc<PromptCleanupTracker>,
}

impl PromptCleanupTracker {
    fn register(self: &Arc<Self>) -> PromptCleanupPermit {
        *self.count.lock().expect("prompt cleanup count lock") += 1;
        PromptCleanupPermit {
            tracker: Arc::clone(self),
        }
    }

    #[cfg(test)]
    fn has_pending(&self) -> bool {
        self.count.lock().map(|count| *count > 0).unwrap_or(true)
    }

    #[cfg(test)]
    async fn wait(&self) {
        loop {
            let notified = self.notify.notified();
            if !self.has_pending() {
                return;
            }
            notified.await;
        }
    }
}

impl Drop for PromptCleanupPermit {
    fn drop(&mut self) {
        if let Ok(mut count) = self.tracker.count.lock() {
            *count = count.saturating_sub(1);
            if *count == 0 {
                self.tracker.notify.notify_waiters();
            }
        }
    }
}

impl PromptAbortClaim {
    async fn abort(mut self) {
        let Some(client) = self.client.take() else {
            return;
        };
        let cleanup_permit = self
            .cleanup_permit
            .take()
            .expect("claimed abort should own cleanup permit");
        let task = tauri::async_runtime::spawn(async move {
            let _cleanup_permit = cleanup_permit;
            abort_direct_opencode_client(&client).await;
        });
        let _ = task.await;
    }
}

impl Drop for PromptAbortClaim {
    fn drop(&mut self) {
        let Some(client) = self.client.take() else {
            return;
        };
        let Some(cleanup_permit) = self.cleanup_permit.take() else {
            return;
        };
        tauri::async_runtime::spawn(async move {
            let _cleanup_permit = cleanup_permit;
            abort_direct_opencode_client(&client).await;
        });
    }
}

fn claim_prompt_abort(
    abort_client: &Mutex<Option<voxgolem_platform::opencode::OpencodeClient>>,
    prompt_cleanup: &Arc<PromptCleanupTracker>,
) -> Option<PromptAbortClaim> {
    let client = take_once(abort_client)?;
    Some(PromptAbortClaim {
        client: Some(client),
        cleanup_permit: Some(prompt_cleanup.register()),
    })
}

impl PromptPhysicalCleanup {
    fn cancel_detached(mut self) {
        if self.cancel {
            self.active.cancellation_signal.send_replace(true);
            if let Some(abort) = self.deep_abort.take() {
                abort.abort();
            }
            if let Some(handle) = self.deep_handle.take() {
                handle.abort();
            }
        }
        self.active.completion_signal.notify_one();
        drop(self.abort_claim.take());
    }

    async fn cancel_and_wait(mut self) {
        if self.cancel {
            self.active.cancellation_signal.send_replace(true);
            if let Some(abort) = self.deep_abort.take() {
                abort.abort();
            }
            if let Some(handle) = self.deep_handle.take() {
                handle.abort();
            }
        }
        self.active.completion_signal.notify_one();
        if let Some(claim) = self.abort_claim.take() {
            claim.abort().await;
        }
    }
}

fn claim_prompt_deep_abort(active: &ActivePrompt) -> Option<tokio::task::AbortHandle> {
    take_once(&active.deep_abort)
}

fn claim_prompt_deep_handle(
    active: &ActivePrompt,
) -> Option<tauri::async_runtime::JoinHandle<DeepStageResult>> {
    take_once(&active.deep_handle)
}

fn repair_cancelled_prompt_runtime(app_state: &AppState) {
    if current_runtime_phase(&app_state.voice_pipeline_state)
        .is_ok_and(|phase| phase != RuntimePhasePayload::Sleeping)
    {
        let _ = reset_voice_pipeline_to_waiting(
            &app_state.voice_pipeline_state,
            &app_state.wake_word_runtime,
            &app_state.voice_activity_runtime,
            app_state.voice_pipeline_config,
        );
    }
}

fn repair_failed_prompt_runtime(app_state: &AppState, message: &str) {
    let _ = apply_voice_pipeline_transition(
        &app_state.voice_pipeline_state,
        app_state.voice_pipeline_config,
        voxgolem_core::voice_pipeline::VoicePipelineEvent::PromptFailed {
            message: message.to_string(),
        },
    );
}

fn repair_dropped_prompt_runtime(
    voice_pipeline_state: &Mutex<voxgolem_core::voice_pipeline::VoicePipelineState>,
    voice_pipeline_config: voxgolem_core::voice_pipeline::VoicePipelineConfig,
) {
    let _ = apply_voice_pipeline_transition(
        voice_pipeline_state,
        voice_pipeline_config,
        voxgolem_core::voice_pipeline::VoicePipelineEvent::PromptCompleted,
    );
}

impl Drop for ResetAdmissionGuard {
    fn drop(&mut self) {
        if let Ok(_lifecycle) = self.lifecycle_gate.lock() {
            self.reset_in_progress.store(false, Ordering::Release);
        }
    }
}

impl SettingsPublicationGuard {
    fn finish(mut self) {
        self.in_progress.store(false, Ordering::Release);
        self.armed = false;
    }
}

impl Drop for SettingsPublicationGuard {
    fn drop(&mut self) {
        if self.armed {
            self.in_progress.store(false, Ordering::Release);
        }
    }
}

impl ProfilePersistenceRollback {
    fn arm(&mut self) {
        self.armed = true;
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for ProfilePersistenceRollback {
    fn drop(&mut self) {
        if self.armed
            && (self.exit_cleanup_started.load(Ordering::SeqCst)
                || self.switch_generation.load(Ordering::SeqCst) == self.expected_generation)
        {
            if let Err(error) = persist_profile_and_assistant_settings(
                self.previous_profile,
                self.previous_settings,
            ) {
                eprintln!("failed to roll back unpublished response profile: {error}");
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct PrefetchKey {
    prompt: String,
    history: Vec<voxgolem_core::assistant::ConversationTurn>,
    model: voxgolem_core::assistant::InstantModel,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct PrefetchEntry {
    generation: u64,
    settings_generation: u64,
    key: PrefetchKey,
    answer: voxgolem_core::assistant::Content,
}

struct ActivePrefetch {
    generation: u64,
    cancelled: Arc<AtomicBool>,
    cancellation_signal: Arc<tokio::sync::watch::Sender<bool>>,
    local_cancellation: Arc<Mutex<Option<voxgolem_platform::llama_cpp::LlamaCppChatCancellation>>>,
    task: Option<tauri::async_runtime::JoinHandle<()>>,
}

impl Drop for ActivePromptGuard {
    fn drop(&mut self) {
        if !self.armed.swap(false, Ordering::AcqRel) {
            return;
        }
        let cleanup = {
            let Ok(_lifecycle) = self.lifecycle_gate.lock() else {
                return;
            };
            let unsettled = !self.settled.load(Ordering::Acquire);
            self.active_prompt.lock().ok().and_then(|mut active| {
                let matching = active.as_ref().is_some_and(|active| {
                    active.request_id == self.request_id && active.generation == self.generation
                });
                if !matching {
                    return None;
                }
                let current = active.as_ref().expect("matching prompt should exist");
                let (abort_claim, deep_handle, deep_abort) = if unsettled {
                    self.cancelled.store(true, Ordering::Release);
                    record_terminal_kind(&self.terminal_kind, TerminalKind::Cancelled);
                    if let Ok(mut session) = self.conversation_session.lock() {
                        let _ = session.settle(
                            self.execution_id,
                            voxgolem_core::conversation::SettlementIntent::Cancelled,
                        );
                    }
                    repair_dropped_prompt_runtime(
                        &self.voice_pipeline_state,
                        self.voice_pipeline_config,
                    );
                    (
                        claim_prompt_abort(&self.abort_client, &self.prompt_cleanup),
                        claim_prompt_deep_handle(current),
                        claim_prompt_deep_abort(current),
                    )
                } else {
                    (None, None, None)
                };
                Some((
                    PromptPhysicalCleanup {
                        active: active.take().expect("matching prompt should exist"),
                        abort_claim,
                        deep_handle,
                        deep_abort,
                        cancel: unsettled,
                    },
                    unsettled,
                ))
            })
        };
        if let Some((cleanup, true)) = cleanup {
            cleanup.cancel_detached();
        } else if let Some((cleanup, false)) = cleanup {
            cleanup.active.completion_signal.notify_one();
        }
    }
}

impl ActivePromptGuard {
    fn finish(&self) {
        if self.armed.swap(false, Ordering::AcqRel) {
            let removed = if let Ok(_lifecycle) = self.lifecycle_gate.lock() {
                self.settled.store(true, Ordering::Release);
                clear_active_prompt(&self.active_prompt, &self.request_id, self.generation)
                    .ok()
                    .flatten()
            } else {
                None
            };
            if let Some(removed) = removed {
                removed.completion_signal.notify_one();
            }
        }
    }
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum RuntimePhasePayload {
    Initializing,
    Sleeping,
    Listening,
    Processing,
    Executing,
    Error,
}

impl RuntimePhasePayload {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Initializing => "initializing",
            Self::Sleeping => "sleeping",
            Self::Listening => "listening",
            Self::Processing => "processing",
            Self::Executing => "executing",
            Self::Error => "error",
        }
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum PromptExecutionEventPayload {
    Stage {
        stage: StagePayload,
        status: StageStatusPayload,
        detail: Option<String>,
    },
    Text {
        text: String,
    },
    Correction {
        stage: StagePayload,
        text: String,
        correction: String,
    },
    Reasoning {
        text: String,
    },
    Status {
        message: String,
    },
    Tool {
        tool: String,
        status: String,
        detail: String,
    },
    Sources {
        sources: Vec<SourcePayload>,
    },
    Error {
        message: String,
    },
    Completed {
        runtime_phase: RuntimePhasePayload,
    },
    Cancelled {
        runtime_phase: RuntimePhasePayload,
    },
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum StagePayload {
    Instant,
    Deep,
    Review,
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum StageStatusPayload {
    Queued,
    Running,
    Completed,
    Kept,
    Corrected,
    Failed,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
struct SourcePayload {
    url: String,
    title: String,
}

#[derive(Clone, Debug, Serialize)]
struct PartialTranscriptionEventPayload {
    session_id: partial_transcription::VoiceSessionId,
    revision: u64,
    text: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum CompletionSource {
    Typed,
    Voice,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct CompletionRequestContext {
    backend_revision: u64,
    client_revision: u64,
    source: CompletionSource,
    voice_session_id: Option<partial_transcription::VoiceSessionId>,
    prompt: String,
    started_ms: u64,
}

#[derive(Clone, Debug, Serialize)]
struct CompletionEventPayload {
    source: CompletionSource,
    revision: u64,
    voice_session_id: Option<partial_transcription::VoiceSessionId>,
    suffix: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum FrontendRuntimeDiagnosticKind {
    FrontendNotice,
    Cue,
    RuntimeControl,
    Execution,
    Tts,
    Audio,
    Profile,
}

impl FrontendRuntimeDiagnosticKind {
    fn as_log_subsystem(self) -> &'static str {
        match self {
            Self::FrontendNotice => "frontend",
            Self::Cue => "cue",
            Self::RuntimeControl => "runtime-control",
            Self::Execution => "execution",
            Self::Tts => "tts",
            Self::Audio => "audio",
            Self::Profile => "profile",
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
struct FrontendRuntimeDiagnosticPayload {
    kind: FrontendRuntimeDiagnosticKind,
    detail: String,
}

struct PromptExecutionOutcome {
    events: Vec<PromptExecutionEventPayload>,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
struct RuntimePhaseResponsePayload {
    runtime_phase: RuntimePhasePayload,
    transcription_ready_samples: Option<usize>,
    transcript_text: Option<String>,
    last_activity_ms: Option<u64>,
    capturing_utterance: bool,
    preroll_samples: usize,
    utterance_samples: usize,
    telemetry: Option<RuntimeTelemetryPayload>,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
struct RuntimeTelemetryPayload {
    frame_id: Option<String>,
    backend_ingest_started_ms: Option<u64>,
    backend_ingest_completed_ms: Option<u64>,
    wake_detected_ms: Option<u64>,
    wake_confidence: Option<f32>,
    transcription_started_ms: Option<u64>,
    transcription_completed_ms: Option<u64>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
struct CueAssetPathsPayload {
    start_listening: String,
    stop_listening: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum ResponseProfilePayload {
    Fast,
    Quality,
}

impl ResponseProfilePayload {
    fn as_str(self) -> &'static str {
        match self {
            Self::Fast => "fast",
            Self::Quality => "quality",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
enum InstantChoicePayload {
    LocalFast,
    LocalQuality,
    CustomSolHigh,
    CustomLunaLow,
    #[serde(rename = "opencode-sol-high")]
    OpenCodeSolHigh,
    #[serde(rename = "opencode-luna-low")]
    OpenCodeLunaLow,
}

impl InstantChoicePayload {
    const ALL: [Self; 6] = [
        Self::LocalFast,
        Self::LocalQuality,
        Self::CustomSolHigh,
        Self::CustomLunaLow,
        Self::OpenCodeSolHigh,
        Self::OpenCodeLunaLow,
    ];

    fn as_str(self) -> &'static str {
        match self {
            Self::LocalFast => "local-fast",
            Self::LocalQuality => "local-quality",
            Self::CustomSolHigh => "custom-sol-high",
            Self::CustomLunaLow => "custom-luna-low",
            Self::OpenCodeSolHigh => "opencode-sol-high",
            Self::OpenCodeLunaLow => "opencode-luna-low",
        }
    }

    fn capability_id(self) -> &'static str {
        match self {
            Self::LocalFast => "local_fast",
            Self::LocalQuality => "local_quality",
            Self::CustomSolHigh | Self::CustomLunaLow => "custom_provider",
            Self::OpenCodeSolHigh | Self::OpenCodeLunaLow => "opencode",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::LocalFast => "Local: Fast",
            Self::LocalQuality => "Local: Quality",
            Self::CustomSolHigh => "Custom: GPT-5.6 Sol High",
            Self::CustomLunaLow => "Custom: GPT-5.6 Luna Low",
            Self::OpenCodeSolHigh => "OpenCode: GPT-5.6 Sol High",
            Self::OpenCodeLunaLow => "OpenCode: GPT-5.6 Luna Low",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
enum AgentChoicePayload {
    CustomSolHigh,
    CustomLunaLow,
    #[serde(rename = "opencode-sol-high")]
    OpenCodeSolHigh,
    #[serde(rename = "opencode-luna-low")]
    OpenCodeLunaLow,
}

impl AgentChoicePayload {
    fn as_str(self) -> &'static str {
        match self {
            Self::CustomSolHigh => "custom-sol-high",
            Self::CustomLunaLow => "custom-luna-low",
            Self::OpenCodeSolHigh => "opencode-sol-high",
            Self::OpenCodeLunaLow => "opencode-luna-low",
        }
    }

    fn capability_id(self) -> &'static str {
        match self {
            Self::CustomSolHigh | Self::CustomLunaLow => "custom_provider",
            Self::OpenCodeSolHigh | Self::OpenCodeLunaLow => "opencode",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
struct AssistantSettingsPayload {
    instant: InstantChoicePayload,
    deep: AgentChoicePayload,
    review: AgentChoicePayload,
    deep_enabled: bool,
    review_enabled: bool,
    prefetch: bool,
    completion: bool,
}

impl Default for AssistantSettingsPayload {
    fn default() -> Self {
        Self {
            instant: InstantChoicePayload::LocalFast,
            deep: AgentChoicePayload::OpenCodeSolHigh,
            review: AgentChoicePayload::OpenCodeSolHigh,
            deep_enabled: false,
            review_enabled: false,
            prefetch: false,
            completion: true,
        }
    }
}

impl From<AssistantSettingsPayload> for voxgolem_core::assistant::AssistantPolicy {
    fn from(settings: AssistantSettingsPayload) -> Self {
        use voxgolem_core::assistant::InstantModel;
        Self {
            instant_model: match settings.instant {
                InstantChoicePayload::LocalFast => InstantModel::LocalFast,
                InstantChoicePayload::LocalQuality => InstantModel::LocalQuality,
                InstantChoicePayload::CustomSolHigh => InstantModel::CustomSolHigh,
                InstantChoicePayload::CustomLunaLow => InstantModel::CustomLunaLow,
                InstantChoicePayload::OpenCodeSolHigh => InstantModel::OpenCodeSolHigh,
                InstantChoicePayload::OpenCodeLunaLow => InstantModel::OpenCodeLunaLow,
            },
            deep_model: agent_model(settings.deep),
            review_model: agent_model(settings.review),
            deep_enabled: settings.deep_enabled,
            review_enabled: settings.review_enabled,
            prefetch_enabled: settings.prefetch,
            completion_enabled: settings.completion,
        }
    }
}

fn agent_model(choice: AgentChoicePayload) -> voxgolem_core::assistant::AgentModel {
    use voxgolem_core::assistant::AgentModel;
    match choice {
        AgentChoicePayload::CustomSolHigh => AgentModel::CustomSolHigh,
        AgentChoicePayload::CustomLunaLow => AgentModel::CustomLunaLow,
        AgentChoicePayload::OpenCodeSolHigh => AgentModel::OpenCodeSolHigh,
        AgentChoicePayload::OpenCodeLunaLow => AgentModel::OpenCodeLunaLow,
    }
}

impl From<&voxgolem_core::assistant::AssistantPolicy> for AssistantSettingsPayload {
    fn from(preferences: &voxgolem_core::assistant::AssistantPolicy) -> Self {
        use voxgolem_core::assistant::{AgentModel, InstantModel};
        Self {
            instant: match preferences.instant_model {
                InstantModel::LocalFast => InstantChoicePayload::LocalFast,
                InstantModel::LocalQuality => InstantChoicePayload::LocalQuality,
                InstantModel::CustomSolHigh => InstantChoicePayload::CustomSolHigh,
                InstantModel::CustomLunaLow => InstantChoicePayload::CustomLunaLow,
                InstantModel::OpenCodeSolHigh => InstantChoicePayload::OpenCodeSolHigh,
                InstantModel::OpenCodeLunaLow => InstantChoicePayload::OpenCodeLunaLow,
            },
            deep: match preferences.deep_model {
                AgentModel::CustomSolHigh => AgentChoicePayload::CustomSolHigh,
                AgentModel::CustomLunaLow => AgentChoicePayload::CustomLunaLow,
                AgentModel::OpenCodeSolHigh => AgentChoicePayload::OpenCodeSolHigh,
                AgentModel::OpenCodeLunaLow => AgentChoicePayload::OpenCodeLunaLow,
            },
            review: match preferences.review_model {
                AgentModel::CustomSolHigh => AgentChoicePayload::CustomSolHigh,
                AgentModel::CustomLunaLow => AgentChoicePayload::CustomLunaLow,
                AgentModel::OpenCodeSolHigh => AgentChoicePayload::OpenCodeSolHigh,
                AgentModel::OpenCodeLunaLow => AgentChoicePayload::OpenCodeLunaLow,
            },
            deep_enabled: preferences.deep_enabled,
            review_enabled: preferences.review_enabled,
            prefetch: preferences.prefetch_enabled,
            completion: preferences.completion_enabled,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum UiTextSizePayload {
    Small,
    Medium,
    Large,
    ExtraLarge,
}

impl UiTextSizePayload {
    fn as_str(self) -> &'static str {
        match self {
            Self::Small => "small",
            Self::Medium => "medium",
            Self::Large => "large",
            Self::ExtraLarge => "extra_large",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum UiThemePayload {
    Light,
    Dark,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct UiPreferencesState {
    theme: UiThemePayload,
    text_size: UiTextSizePayload,
}

impl UiThemePayload {
    fn as_str(self) -> &'static str {
        match self {
            Self::Light => "light",
            Self::Dark => "dark",
        }
    }
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
struct SwitchResponseProfilePayload {
    selected_response_profile: ResponseProfilePayload,
    supported_response_profiles: Vec<ResponseProfilePayload>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
struct SetTtsEnabledPayload {
    enabled: bool,
    sample_rate_hz: u32,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
struct SynthesizeLocalTtsPayload {
    pcm_f32: Vec<f32>,
    sample_rate_hz: u32,
    duration_ms: u64,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
struct CapabilityPayload {
    id: &'static str,
    state: CapabilityStatePayload,
    reason: String,
    actual_provider: Option<&'static str>,
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum CapabilityStatePayload {
    Available,
    Warming,
    NotConfigured,
    Unavailable,
    Failed,
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum StartupStatePayload {
    WarmingModel {
        cue_asset_paths: CueAssetPathsPayload,
        runtime_phase: RuntimePhasePayload,
        voice_input_available: bool,
        voice_input_error: Option<String>,
        silence_timeout_ms: u64,
        message: String,
        selected_response_profile: ResponseProfilePayload,
        supported_response_profiles: Vec<ResponseProfilePayload>,
        prompt_cancellation_available: bool,
        tts_enabled: bool,
        tts_output_gain_db: f32,
        capabilities: Vec<CapabilityPayload>,
    },
    Ready {
        cue_asset_paths: CueAssetPathsPayload,
        runtime_phase: RuntimePhasePayload,
        voice_input_available: bool,
        voice_input_error: Option<String>,
        silence_timeout_ms: u64,
        selected_response_profile: ResponseProfilePayload,
        supported_response_profiles: Vec<ResponseProfilePayload>,
        prompt_cancellation_available: bool,
        tts_enabled: bool,
        tts_output_gain_db: f32,
        capabilities: Vec<CapabilityPayload>,
    },
    Error {
        message: String,
    },
}

fn validated_ipc_text<const MAX_BYTES: usize>(
    value: &str,
) -> voxgolem_core::ipc::BoundedText<MAX_BYTES> {
    voxgolem_core::ipc::BoundedText::new(value)
        .expect("static desktop IPC metadata should fit its wire bound")
}

fn bounded_capability_reason(
    value: &str,
) -> voxgolem_core::ipc::BoundedText<{ voxgolem_core::ipc::DISPLAY_TEXT_MAX_BYTES }> {
    voxgolem_core::ipc::BoundedText::new(value)
        .unwrap_or_else(|_| validated_ipc_text("details omitted; see local logs"))
}

fn ipc_capabilities(startup_state: &StartupStatePayload) -> Vec<voxgolem_core::ipc::Capability> {
    let capabilities = match startup_state {
        StartupStatePayload::WarmingModel { capabilities, .. }
        | StartupStatePayload::Ready { capabilities, .. } => capabilities.clone(),
        StartupStatePayload::Error { message } => failed_capabilities(message.clone()),
    };
    capabilities
        .into_iter()
        .map(|capability| voxgolem_core::ipc::Capability {
            id: validated_ipc_text(capability.id),
            state: match capability.state {
                CapabilityStatePayload::Available => voxgolem_core::ipc::CapabilityState::Available,
                CapabilityStatePayload::Warming => voxgolem_core::ipc::CapabilityState::Warming,
                CapabilityStatePayload::NotConfigured => {
                    voxgolem_core::ipc::CapabilityState::NotConfigured
                }
                CapabilityStatePayload::Unavailable => {
                    voxgolem_core::ipc::CapabilityState::Unavailable
                }
                CapabilityStatePayload::Failed => voxgolem_core::ipc::CapabilityState::Failed,
            },
            reason: bounded_capability_reason(&capability.reason),
            actual_provider: capability.actual_provider.map(validated_ipc_text),
        })
        .collect()
}

fn ipc_model_options(
    capabilities: &[voxgolem_core::ipc::Capability],
) -> Vec<voxgolem_core::ipc::ModelOption> {
    InstantChoicePayload::ALL
        .into_iter()
        .map(|choice| {
            let capability = capabilities
                .iter()
                .find(|capability| capability.id.as_str() == choice.capability_id());
            let available = capability.is_some_and(|capability| {
                capability.state == voxgolem_core::ipc::CapabilityState::Available
            });
            voxgolem_core::ipc::ModelOption {
                id: validated_ipc_text(choice.as_str()),
                label: validated_ipc_text(choice.label()),
                available,
                unavailable_reason: (!available).then(|| {
                    capability.map_or_else(
                        || validated_ipc_text("not configured"),
                        |capability| capability.reason.clone(),
                    )
                }),
            }
        })
        .collect()
}

fn build_ipc_app_snapshot(
    conversation: voxgolem_core::conversation::ConversationSnapshot,
    policy: &voxgolem_core::assistant::AssistantPolicy,
    runtime_phase: voxgolem_core::runtime::RuntimePhase,
    startup_state: &StartupStatePayload,
    response_profile: ResponseProfilePayload,
    ui_preferences: UiPreferencesState,
) -> voxgolem_core::ipc::AppSnapshot {
    use voxgolem_core::assistant::InstantModel;
    use voxgolem_core::ipc;

    let capabilities = ipc_capabilities(startup_state);
    let tts_enabled = match startup_state {
        StartupStatePayload::WarmingModel { tts_enabled, .. }
        | StartupStatePayload::Ready { tts_enabled, .. } => *tts_enabled,
        StartupStatePayload::Error { .. } => false,
    };
    ipc::AppSnapshot {
        session_id: conversation.session_id.0.into(),
        runtime_phase: runtime_phase.into(),
        transcript: conversation.turns.iter().map(Into::into).collect(),
        active_request: conversation.active.as_ref().map(Into::into),
        model_options: ipc_model_options(&capabilities),
        capabilities,
        preferences: ipc::ValidatedPreferences {
            instant: policy.instant_model.into(),
            deep: policy.deep_model.into(),
            review: policy.review_model.into(),
            deep_enabled: policy.deep_enabled,
            review_enabled: policy.review_enabled,
            prefetch: policy.prefetch_enabled,
            completion: policy.completion_enabled,
            response_profile: match policy.instant_model {
                InstantModel::LocalFast => ipc::ResponseProfile::Fast,
                InstantModel::LocalQuality => ipc::ResponseProfile::Quality,
                _ => match response_profile {
                    ResponseProfilePayload::Fast => ipc::ResponseProfile::Fast,
                    ResponseProfilePayload::Quality => ipc::ResponseProfile::Quality,
                },
            },
            tts_enabled,
            ui_theme: match ui_preferences.theme {
                UiThemePayload::Light => ipc::UiTheme::Light,
                UiThemePayload::Dark => ipc::UiTheme::Dark,
            },
            ui_text_size: match ui_preferences.text_size {
                UiTextSizePayload::Small => ipc::UiTextSize::Small,
                UiTextSizePayload::Medium => ipc::UiTextSize::Medium,
                UiTextSizePayload::Large => ipc::UiTextSize::Large,
                UiTextSizePayload::ExtraLarge => ipc::UiTextSize::ExtraLarge,
            },
        },
    }
}

fn ipc_command_error(message: &'static str) -> voxgolem_core::ipc::CommandError {
    voxgolem_core::ipc::CommandError::new(voxgolem_core::ipc::CommandErrorCode::Internal, message)
        .expect("static IPC command errors should fit the wire bound")
}

fn ensure_app_running(exit_cleanup_started: &AtomicBool) -> Result<(), String> {
    if exit_cleanup_started.load(Ordering::SeqCst) {
        Err(String::from("application shutdown is in progress"))
    } else {
        Ok(())
    }
}

async fn lock_live_settings_operation(
    app_state: &AppState,
) -> Result<tokio::sync::OwnedMutexGuard<()>, String> {
    ensure_app_running(&app_state.exit_cleanup_started)?;
    let guard = Arc::clone(&app_state.settings_operation_lock)
        .lock_owned()
        .await;
    ensure_app_running(&app_state.exit_cleanup_started)?;
    Ok(guard)
}

#[tauri::command]
fn get_app_snapshot(
    app_state: tauri::State<'_, AppState>,
) -> Result<voxgolem_core::ipc::AppSnapshot, voxgolem_core::ipc::CommandError> {
    get_app_snapshot_from_state(&app_state)
}

fn get_app_snapshot_from_state(
    app_state: &AppState,
) -> Result<voxgolem_core::ipc::AppSnapshot, voxgolem_core::ipc::CommandError> {
    let _lifecycle = app_state
        .lifecycle_gate
        .lock()
        .map_err(|_| ipc_command_error("lifecycle state is unavailable"))?;
    let runtime_phase = app_state
        .voice_pipeline_state
        .lock()
        .map_err(|_| ipc_command_error("runtime state is unavailable"))?
        .session()
        .runtime()
        .phase();
    let startup_state = app_state
        .startup_state
        .lock()
        .map_err(|_| ipc_command_error("capability state is unavailable"))?
        .clone();
    let response_profile = *app_state
        .selected_response_profile
        .lock()
        .map_err(|_| ipc_command_error("response profile state is unavailable"))?;
    let ui_preferences = *app_state
        .ui_preferences
        .lock()
        .map_err(|_| ipc_command_error("interface preferences are unavailable"))?;
    let session = app_state
        .conversation_session
        .lock()
        .map_err(|_| ipc_command_error("conversation state is unavailable"))?;
    Ok(build_ipc_app_snapshot(
        session.snapshot(),
        session.policy(),
        runtime_phase,
        &startup_state,
        response_profile,
        ui_preferences,
    ))
}

#[tauri::command]
fn get_startup_state(app_state: tauri::State<'_, AppState>) -> StartupStatePayload {
    app_state
        .startup_state
        .lock()
        .expect("startup state lock should not be poisoned")
        .clone()
}

#[tauri::command]
async fn set_tts_enabled(
    enabled: bool,
    app_state: tauri::State<'_, AppState>,
) -> Result<SetTtsEnabledPayload, String> {
    let _settings_operation = lock_live_settings_operation(&app_state).await?;
    let config = app_state
        .runtime_config
        .as_ref()
        .ok_or_else(|| String::from("startup config is not ready"))?;

    let tts_config = config.local_tts.clone();
    let runtime_file_logging_enabled = config.logging.enabled;
    let _operation_guard = app_state.tts_operation_lock.lock().await;
    ensure_app_running(&app_state.exit_cleanup_started)?;
    let operation_generation = app_state
        .tts_startup_generation
        .fetch_add(1, Ordering::SeqCst)
        .saturating_add(1);
    let current_runtime_present = app_state
        .local_tts_runtime
        .lock()
        .map_err(|_| String::from("local tts runtime lock is poisoned"))?
        .is_some();
    let proposed_runtime = prepare_tts_runtime(
        enabled,
        current_runtime_present,
        &tts_config,
        runtime_file_logging_enabled,
        operation_generation,
        &app_state.tts_startup_generation,
    )
    .await?;
    let proposed_runtime = persist_tts_preference(enabled, proposed_runtime)?;
    let mut runtime_guard = app_state
        .local_tts_runtime
        .lock()
        .map_err(|_| String::from("local tts runtime lock is poisoned"))?;
    let mut unused_proposed_runtime = proposed_runtime;
    let mut old_runtime = None;
    if enabled {
        if runtime_guard.is_none() {
            *runtime_guard = unused_proposed_runtime.take().map(Arc::new);
            log_tts_runtime_event(config.logging.enabled, "runtime enabled");
        }
    } else {
        if let Some(runtime) = runtime_guard.as_ref() {
            runtime.cancel_generation();
        }
        old_runtime = runtime_guard.take();
        log_tts_runtime_event(config.logging.enabled, "runtime disabled and unloaded");
    }
    let sample_rate_hz = runtime_guard
        .as_ref()
        .map(|runtime| runtime.sample_rate_hz())
        .unwrap_or(config.local_tts.sample_rate_hz);
    let runtime_available = runtime_guard.is_some();
    let actual_provider = runtime_guard
        .as_deref()
        .and_then(tts::LocalTtsRuntime::actual_provider)
        .map(|provider| match provider {
            tts::TtsActualProvider::Cuda => "cuda",
            tts::TtsActualProvider::Cpu => "cpu",
        });
    drop(runtime_guard);
    drop(unused_proposed_runtime);
    drop(old_runtime);
    let _lifecycle = app_state
        .lifecycle_gate
        .lock()
        .map_err(|_| String::from("lifecycle gate is poisoned"))?;
    if let Ok(mut state) = app_state.startup_state.lock() {
        if let StartupStatePayload::WarmingModel {
            capabilities,
            tts_enabled,
            ..
        }
        | StartupStatePayload::Ready {
            capabilities,
            tts_enabled,
            ..
        } = &mut *state
        {
            *tts_enabled = enabled;
            repair_tts_capability_from_status(
                capabilities,
                enabled,
                runtime_available,
                actual_provider,
            );
        }
    }

    Ok(SetTtsEnabledPayload {
        enabled,
        sample_rate_hz,
    })
}

fn tts_runtime_needs_initialization(enabled: bool, current_runtime_present: bool) -> bool {
    enabled && !current_runtime_present
}

async fn prepare_tts_runtime(
    enabled: bool,
    current_runtime_present: bool,
    config: &voxgolem_core::config::LocalTtsConfig,
    runtime_file_logging_enabled: bool,
    operation_generation: u64,
    generation: &AtomicU64,
) -> Result<Option<tts::LocalTtsRuntime>, String> {
    let proposed_runtime = if tts_runtime_needs_initialization(enabled, current_runtime_present) {
        let config = config.clone();
        tauri::async_runtime::spawn_blocking(move || {
            initialize_local_tts_runtime(&config, true, runtime_file_logging_enabled)
        })
        .await
        .map_err(|error| format!("local tts initialization task failed: {error}"))??
    } else {
        None
    };
    if generation.load(Ordering::SeqCst) != operation_generation {
        if let Some(mut runtime) = proposed_runtime {
            runtime.shutdown_owned();
        }
        return Err(String::from("local tts operation was superseded"));
    }
    Ok(proposed_runtime)
}

fn persist_tts_preference(
    enabled: bool,
    proposed_runtime: Option<tts::LocalTtsRuntime>,
) -> Result<Option<tts::LocalTtsRuntime>, String> {
    if let Err(error) = persist_tts_enabled(enabled) {
        if let Some(mut runtime) = proposed_runtime {
            runtime.shutdown_owned();
        }
        return Err(error);
    }
    Ok(proposed_runtime)
}

#[tauri::command]
async fn synthesize_local_tts(
    text: String,
    app_state: tauri::State<'_, AppState>,
) -> Result<SynthesizeLocalTtsPayload, String> {
    ensure_app_running(&app_state.exit_cleanup_started)?;
    let operation_guard = app_state.tts_operation_lock.lock().await;
    ensure_app_running(&app_state.exit_cleanup_started)?;
    let runtime_file_logging_enabled = app_state
        .runtime_config
        .as_ref()
        .map(|config| config.logging.enabled)
        .unwrap_or(false);
    let runtime = {
        let runtime_guard = app_state
            .local_tts_runtime
            .lock()
            .map_err(|_| String::from("local tts runtime lock is poisoned"))?;
        Arc::clone(runtime_guard.as_ref().ok_or_else(|| {
            log_tts_runtime_event(
                runtime_file_logging_enabled,
                "synthesis rejected: runtime unavailable",
            );
            String::from("local tts runtime is not available")
        })?)
    };
    let generation = runtime.start_generation();
    drop(operation_guard);
    let audio = tauri::async_runtime::spawn_blocking(move || {
        runtime.synthesize_for_generation(&text, generation)
    })
    .await
    .map_err(|error| format!("local tts synthesis task failed: {error}"))?
    .map_err(|error| {
        log_tts_runtime_event(
            runtime_file_logging_enabled,
            &format!("synthesis failed: {error}"),
        );
        error
    })?;

    Ok(SynthesizeLocalTtsPayload {
        pcm_f32: audio.pcm_f32,
        sample_rate_hz: audio.sample_rate_hz,
        duration_ms: audio.duration_ms,
    })
}

#[tauri::command]
fn record_frontend_runtime_diagnostic(
    event: FrontendRuntimeDiagnosticPayload,
    app_state: tauri::State<'_, AppState>,
) -> Result<(), String> {
    let runtime_file_logging_enabled = app_state
        .runtime_config
        .as_ref()
        .map(|config| config.logging.enabled)
        .unwrap_or(false);

    append_runtime_log_line(
        runtime_file_logging_enabled,
        event.kind.as_log_subsystem(),
        &event.detail,
    )
}

#[tauri::command]
fn get_ui_text_size(app_state: tauri::State<'_, AppState>) -> Result<UiTextSizePayload, String> {
    app_state
        .ui_preferences
        .lock()
        .map(|preferences| preferences.text_size)
        .map_err(|_| String::from("interface preferences lock is poisoned"))
}

#[tauri::command]
async fn set_ui_text_size(
    text_size: UiTextSizePayload,
    app_state: tauri::State<'_, AppState>,
) -> Result<UiTextSizePayload, String> {
    let _settings_operation = lock_live_settings_operation(&app_state).await?;
    let _operation = app_state
        .ui_preferences_operation_lock
        .lock()
        .map_err(|_| String::from("interface preferences operation lock is poisoned"))?;
    persist_ui_text_size(text_size)?;
    let _lifecycle = app_state
        .lifecycle_gate
        .lock()
        .map_err(|_| String::from("lifecycle gate is poisoned"))?;
    app_state
        .ui_preferences
        .lock()
        .map_err(|_| String::from("interface preferences lock is poisoned"))?
        .text_size = text_size;
    Ok(text_size)
}

#[tauri::command]
fn get_ui_theme(app_state: tauri::State<'_, AppState>) -> Result<UiThemePayload, String> {
    app_state
        .ui_preferences
        .lock()
        .map(|preferences| preferences.theme)
        .map_err(|_| String::from("interface preferences lock is poisoned"))
}

#[tauri::command]
async fn set_ui_theme(
    theme: UiThemePayload,
    app_state: tauri::State<'_, AppState>,
) -> Result<UiThemePayload, String> {
    let _settings_operation = lock_live_settings_operation(&app_state).await?;
    let _operation = app_state
        .ui_preferences_operation_lock
        .lock()
        .map_err(|_| String::from("interface preferences operation lock is poisoned"))?;
    persist_ui_theme(theme)?;
    let _lifecycle = app_state
        .lifecycle_gate
        .lock()
        .map_err(|_| String::from("lifecycle gate is poisoned"))?;
    app_state
        .ui_preferences
        .lock()
        .map_err(|_| String::from("interface preferences lock is poisoned"))?
        .theme = theme;
    Ok(theme)
}

#[tauri::command]
fn get_assistant_settings(
    app_state: tauri::State<'_, AppState>,
) -> Result<AssistantSettingsPayload, String> {
    let coordinator = app_state
        .conversation_session
        .lock()
        .map_err(|_| String::from("assistant coordinator lock is poisoned"))?;
    Ok(AssistantSettingsPayload::from(coordinator.policy()))
}

#[tauri::command]
async fn set_assistant_settings(
    settings: AssistantSettingsPayload,
    app_state: tauri::State<'_, AppState>,
) -> Result<AssistantSettingsPayload, String> {
    let _settings_operation = lock_live_settings_operation(&app_state).await?;
    let _operation_guard = lock_ready_response_backend_operation(
        &app_state.response_backend_operation_lock,
        &app_state.startup_state,
    )?;
    let publication = {
        let _lifecycle = app_state
            .lifecycle_gate
            .lock()
            .map_err(|_| String::from("lifecycle gate is poisoned"))?;
        let coordinator = app_state
            .conversation_session
            .lock()
            .map_err(|_| String::from("assistant coordinator lock is poisoned"))?;
        let previous = AssistantSettingsPayload::from(coordinator.policy());
        ensure_assistant_settings_available(&settings, &previous, &app_state.startup_state)?;
        if coordinator.has_active_execution() {
            return Err(String::from(
                "assistant settings cannot change while a prompt is active",
            ));
        }
        app_state
            .settings_publication_in_progress
            .store(true, Ordering::Release);
        let publication = SettingsPublicationGuard {
            in_progress: Arc::clone(&app_state.settings_publication_in_progress),
            armed: true,
        };
        invalidate_prefetch(&app_state)?;
        publication
    };
    persist_assistant_settings(settings)?;
    {
        let _lifecycle = app_state
            .lifecycle_gate
            .lock()
            .map_err(|_| String::from("lifecycle gate is poisoned"))?;
        app_state
            .conversation_session
            .lock()
            .map_err(|_| String::from("assistant coordinator lock is poisoned"))?
            .replace_policy(settings.into())
            .map_err(|_| {
                String::from("assistant settings cannot change while a prompt is active")
            })?;
        app_state
            .assistant_settings_generation
            .fetch_add(1, Ordering::SeqCst);
        invalidate_prefetch(&app_state)?;
        publication.finish();
    }
    if !settings.completion {
        clear_completion_state(&app_state)?;
    }
    Ok(settings)
}

fn lock_ready_response_backend_operation<'a>(
    operation_lock: &'a Mutex<()>,
    startup_state: &Arc<Mutex<StartupStatePayload>>,
) -> Result<MutexGuard<'a, ()>, String> {
    let guard = lock_response_backend_operation(operation_lock)?;
    ensure_startup_ready_for_profile_switch(startup_state)?;
    Ok(guard)
}

fn ensure_assistant_settings_available(
    settings: &AssistantSettingsPayload,
    previous: &AssistantSettingsPayload,
    startup_state: &Arc<Mutex<StartupStatePayload>>,
) -> Result<(), String> {
    let guard = startup_state
        .lock()
        .map_err(|_| String::from("startup state lock is poisoned"))?;
    let capabilities = match &*guard {
        StartupStatePayload::WarmingModel { capabilities, .. }
        | StartupStatePayload::Ready { capabilities, .. } => capabilities,
        StartupStatePayload::Error { .. } => {
            return Err(String::from("startup is not ready"));
        }
    };
    let mut required = Vec::new();
    if settings.instant != previous.instant {
        required.push(settings.instant.capability_id());
    }
    if settings.deep_enabled && (!previous.deep_enabled || settings.deep != previous.deep) {
        required.push(settings.deep.capability_id());
        required.push("deep");
    }
    if settings.review_enabled && (!previous.review_enabled || settings.review != previous.review) {
        required.push(settings.review.capability_id());
        required.push("review");
    }
    if settings.completion && !previous.completion {
        required.push("qwen_prediction");
    }
    for capability_id in required {
        let available = capabilities.iter().any(|capability| {
            capability.id == capability_id && capability.state == CapabilityStatePayload::Available
        });
        if !available {
            return Err(format!(
                "assistant capability `{capability_id}` is unavailable"
            ));
        }
    }
    Ok(())
}

#[derive(Debug, PartialEq, Eq)]
enum RuntimeReplacement<R, V> {
    Applied {
        runtime: R,
        value: V,
    },
    Restored {
        runtime: R,
        error: String,
    },
    RestoreFailed {
        error: String,
        restore_error: String,
    },
    Superseded,
    RejectedExistingExternal {
        runtime: R,
    },
    RejectedStartedExternal,
}

fn replace_owned_runtime_transaction<R, S, V, E>(
    current_runtime: Option<R>,
    specs: (S, S),
    is_owned: impl Fn(&R) -> bool,
    is_current: impl Fn() -> bool,
    mut shutdown: impl FnMut(&mut R),
    mut start: impl FnMut(S) -> Result<R, E>,
    prepare: impl FnOnce(&R) -> Result<V, String>,
) -> RuntimeReplacement<R, V>
where
    E: ToString,
{
    let (requested_spec, previous_spec) = specs;
    if let Some(mut runtime) = current_runtime {
        if !is_owned(&runtime) {
            return RuntimeReplacement::RejectedExistingExternal { runtime };
        }
        shutdown(&mut runtime);
    }

    let mut requested_runtime = match start(requested_spec) {
        Ok(runtime) => runtime,
        Err(error) => {
            if !is_current() {
                return RuntimeReplacement::Superseded;
            }
            let error = error.to_string();
            return match start(previous_spec) {
                Ok(mut runtime) if !is_current() => {
                    shutdown(&mut runtime);
                    RuntimeReplacement::Superseded
                }
                Ok(runtime) if !is_owned(&runtime) => RuntimeReplacement::RejectedStartedExternal,
                Ok(runtime) => RuntimeReplacement::Restored { runtime, error },
                Err(_) if !is_current() => RuntimeReplacement::Superseded,
                Err(restore_error) => RuntimeReplacement::RestoreFailed {
                    error,
                    restore_error: restore_error.to_string(),
                },
            };
        }
    };
    if !is_current() {
        shutdown(&mut requested_runtime);
        return RuntimeReplacement::Superseded;
    }
    if !is_owned(&requested_runtime) {
        return RuntimeReplacement::RejectedStartedExternal;
    }

    match prepare(&requested_runtime) {
        Ok(_) if !is_current() => {
            shutdown(&mut requested_runtime);
            RuntimeReplacement::Superseded
        }
        Ok(value) => RuntimeReplacement::Applied {
            runtime: requested_runtime,
            value,
        },
        Err(error) => {
            shutdown(&mut requested_runtime);
            if !is_current() {
                return RuntimeReplacement::Superseded;
            }
            match start(previous_spec) {
                Ok(mut runtime) if !is_current() => {
                    shutdown(&mut runtime);
                    RuntimeReplacement::Superseded
                }
                Ok(runtime) if !is_owned(&runtime) => RuntimeReplacement::RejectedStartedExternal,
                Ok(runtime) => RuntimeReplacement::Restored { runtime, error },
                Err(_) if !is_current() => RuntimeReplacement::Superseded,
                Err(restore_error) => RuntimeReplacement::RestoreFailed {
                    error,
                    restore_error: restore_error.to_string(),
                },
            }
        }
    }
}

fn begin_response_profile_switch_transition(
    lifecycle_gate: &Mutex<()>,
    exit_cleanup_started: &AtomicBool,
    conversation_session: &Mutex<voxgolem_core::conversation::ConversationSession>,
    voice_pipeline_state: &Mutex<voxgolem_core::voice_pipeline::VoicePipelineState>,
    startup_state: &Mutex<StartupStatePayload>,
    warming_state: StartupStatePayload,
) -> Result<(), String> {
    let _lifecycle = lifecycle_gate
        .lock()
        .map_err(|_| String::from("lifecycle gate is poisoned"))?;
    ensure_app_running(exit_cleanup_started)?;
    ensure_response_profile_switch_runtime_is_idle(voice_pipeline_state)?;
    if conversation_session
        .lock()
        .map_err(|_| String::from("conversation session lock is poisoned"))?
        .has_active_execution()
    {
        return Err(String::from(
            "response profile cannot change while a prompt is active",
        ));
    }
    *startup_state
        .lock()
        .map_err(|_| String::from("startup state lock is poisoned"))? = warming_state;
    Ok(())
}

#[tauri::command]
async fn switch_response_profile(
    profile: ResponseProfilePayload,
    app_state: tauri::State<'_, AppState>,
) -> Result<SwitchResponseProfilePayload, String> {
    let settings_operation = lock_live_settings_operation(&app_state).await?;
    let supported_response_profiles = app_state.supported_response_profiles.clone();
    if !supported_response_profiles.contains(&profile) {
        return Err(format!(
            "response profile `{}` is not supported",
            profile.as_str()
        ));
    }

    ensure_startup_ready_for_profile_switch(&app_state.startup_state)?;

    let response = SwitchResponseProfilePayload {
        selected_response_profile: profile,
        supported_response_profiles: supported_response_profiles.clone(),
    };

    ensure_response_profile_switch_runtime_is_idle(&app_state.voice_pipeline_state)?;
    invalidate_and_wait_for_prefetch(&app_state).await?;
    let _operation_guard = lock_ready_response_backend_operation(
        &app_state.response_backend_operation_lock,
        &app_state.startup_state,
    )?;
    ensure_app_running(&app_state.exit_cleanup_started)?;

    let current_profile = *app_state
        .selected_response_profile
        .lock()
        .map_err(|_| String::from("selected response profile lock is poisoned"))?;
    let (runtime_loaded, runtime_owned) = {
        let runtime = app_state
            .llama_cpp_runtime
            .lock()
            .map_err(|_| String::from("local llama.cpp runtime lock is poisoned"))?;
        (
            runtime.is_some(),
            runtime
                .as_ref()
                .is_none_or(voxgolem_platform::llama_cpp::LlamaCppRuntime::is_owned),
        )
    };
    if current_profile == profile && runtime_loaded {
        return Ok(response);
    }
    if current_profile != profile && !runtime_owned {
        return Err(String::from(
            "response profile cannot change while using an externally managed llama.cpp runtime",
        ));
    }

    let Some(config) = app_state.runtime_config.as_ref() else {
        return Ok(response);
    };

    let voxgolem_core::config::ResponseBackendConfig::LlamaCpp {
        server_path,
        host,
        port,
        fast_model_path,
        quality_model_path,
    } = &config.response_backend
    else {
        return Ok(response);
    };

    let model_path = model_path_for_profile(profile, fast_model_path, quality_model_path.as_ref())?
        .to_path_buf();
    let previous_model_path = model_path_for_profile(
        current_profile,
        fast_model_path,
        quality_model_path.as_ref(),
    )?
    .to_path_buf();
    let startup_snapshot = startup_snapshot_for_profile_switch(
        &app_state.startup_state,
        profile,
        supported_response_profiles,
    )?;

    let startup_state = Arc::clone(&app_state.startup_state);
    let llama_cpp_runtime = Arc::clone(&app_state.llama_cpp_runtime);
    let selected_response_profile = Arc::clone(&app_state.selected_response_profile);
    let conversation_session = Arc::clone(&app_state.conversation_session);
    let lifecycle_gate = Arc::clone(&app_state.lifecycle_gate);
    let assistant_settings_generation = Arc::clone(&app_state.assistant_settings_generation);
    let response_profile_switch_generation =
        Arc::clone(&app_state.response_profile_switch_generation);
    let exit_cleanup_started = Arc::clone(&app_state.exit_cleanup_started);
    let inference_policy = config
        .llama_cpp
        .as_ref()
        .map(|llama| platform_inference_policy(llama.inference_provider))
        .unwrap_or(voxgolem_platform::inference::InferencePolicy::Auto);
    let server_spec = voxgolem_platform::llama_cpp::LlamaCppServerSpec::new(
        server_path.clone(),
        model_path,
        host.clone(),
        *port,
        LLAMA_CPP_MODEL_ALIAS,
    );
    let fallback_server_spec = voxgolem_platform::llama_cpp::LlamaCppServerSpec::new(
        server_path.clone(),
        previous_model_path,
        host.clone(),
        *port,
        LLAMA_CPP_MODEL_ALIAS,
    );

    begin_response_profile_switch_transition(
        &app_state.lifecycle_gate,
        &app_state.exit_cleanup_started,
        &conversation_session,
        &app_state.voice_pipeline_state,
        &startup_state,
        startup_warming_state_from_snapshot(&startup_snapshot, profile),
    )?;
    invalidate_prefetch(&app_state)?;

    let switch_generation = app_state
        .response_profile_switch_generation
        .fetch_add(1, Ordering::SeqCst)
        .saturating_add(1);
    app_state
        .llama_cpp_conversation
        .lock()
        .map_err(|_| String::from("local llama.cpp conversation lock is poisoned"))?
        .clear();
    let previous_runtime = app_state
        .llama_cpp_runtime
        .lock()
        .map_err(|_| String::from("local llama.cpp runtime lock is poisoned"))?
        .take();
    let previous_assistant_settings = AssistantSettingsPayload::from(
        app_state
            .conversation_session
            .lock()
            .map_err(|_| String::from("assistant coordinator lock is poisoned"))?
            .policy(),
    );
    let expected_assistant_settings_generation =
        assistant_settings_generation.load(Ordering::SeqCst);
    let llama_startups = Arc::clone(&app_state.llama_startups);
    ensure_app_running(&app_state.exit_cleanup_started)?;
    let startup_cancellation = voxgolem_platform::llama_cpp::LlamaCppStartupCancellation::default();
    let worker_cancellation = startup_cancellation.clone();
    let startup_coordinator = std::thread::spawn(move || {
        let _settings_operation = settings_operation;
        let mut persistence_rollback = ProfilePersistenceRollback {
            previous_profile: current_profile,
            previous_settings: previous_assistant_settings,
            exit_cleanup_started: Arc::clone(&exit_cleanup_started),
            switch_generation: Arc::clone(&response_profile_switch_generation),
            expected_generation: switch_generation,
            armed: false,
        };
        let mut runtime_to_publish = None;
        let mut policy_to_publish = None;
        let replacement = replace_owned_runtime_transaction(
            previous_runtime,
            (server_spec, fallback_server_spec),
            voxgolem_platform::llama_cpp::LlamaCppRuntime::is_owned,
            || {
                runtime_startup_is_current(
                    &exit_cleanup_started,
                    &response_profile_switch_generation,
                    switch_generation,
                )
            },
            voxgolem_platform::llama_cpp::LlamaCppRuntime::shutdown_owned,
            |spec| {
                voxgolem_platform::llama_cpp::LlamaCppRuntime::start_with_policy_cancellation(
                    spec,
                    inference_policy,
                    worker_cancellation.clone(),
                )
                .join()
                .unwrap_or_else(|_| {
                    Err(voxgolem_platform::llama_cpp::LlamaCppRuntimeError::StartupCancelled)
                })
            },
            |_| {
                let prepared = prepare_local_instant_model(
                    &conversation_session,
                    &lifecycle_gate,
                    &assistant_settings_generation,
                    expected_assistant_settings_generation,
                    profile,
                )
                .and_then(|prepared| {
                    persist_prepared_local_instant_model_with(
                        prepared,
                        persist_profile_and_assistant_settings,
                    )
                })?;
                persistence_rollback.arm();
                Ok(prepared)
            },
        );
        let published_state = match replacement {
            RuntimeReplacement::Applied { runtime, value } => {
                let actual_provider = actual_inference_provider_name(runtime.actual_provider());
                runtime_to_publish = Some(runtime);
                policy_to_publish = Some(value.policy);
                let mut ready_snapshot = startup_snapshot.clone();
                if let Some(capability) =
                    ready_snapshot.capabilities.iter_mut().find(|capability| {
                        capability.id
                            == if profile == ResponseProfilePayload::Quality {
                                "local_quality"
                            } else {
                                "local_fast"
                            }
                    })
                {
                    capability.state = CapabilityStatePayload::Available;
                    capability.reason = String::from("ready");
                    capability.actual_provider = Some(actual_provider);
                }
                startup_ready_state_from_snapshot(&ready_snapshot, profile)
            }
            RuntimeReplacement::Restored { runtime, error } => {
                let actual_provider = actual_inference_provider_name(runtime.actual_provider());
                runtime_to_publish = Some(runtime);
                let mut ready_snapshot = startup_snapshot.clone();
                update_restored_profile_capabilities(
                    &mut ready_snapshot.capabilities,
                    profile,
                    current_profile,
                    &error,
                    actual_provider,
                );
                startup_ready_state_from_snapshot(&ready_snapshot, current_profile)
            }
            RuntimeReplacement::RestoreFailed {
                error,
                restore_error,
            } => startup_state_after_profile_restore_failure(
                &startup_snapshot,
                profile,
                current_profile,
                &error,
                &restore_error,
            ),
            RuntimeReplacement::Superseded => return,
            RuntimeReplacement::RejectedExistingExternal { runtime } => {
                let actual_provider = actual_inference_provider_name(runtime.actual_provider());
                runtime_to_publish = Some(runtime);
                let mut ready_snapshot = startup_snapshot.clone();
                update_restored_profile_capabilities(
                    &mut ready_snapshot.capabilities,
                    profile,
                    current_profile,
                    "response profile cannot change while using an externally managed llama.cpp runtime",
                    actual_provider,
                );
                startup_ready_state_from_snapshot(&ready_snapshot, current_profile)
            }
            RuntimeReplacement::RejectedStartedExternal => {
                startup_state_after_profile_restore_failure(
                    &startup_snapshot,
                    profile,
                    current_profile,
                    "an externally managed llama.cpp runtime cannot satisfy the requested profile",
                    "an externally managed llama.cpp runtime cannot restore the previous profile",
                )
            }
        };

        if !runtime_startup_is_current(
            &exit_cleanup_started,
            &response_profile_switch_generation,
            switch_generation,
        ) {
            return;
        }

        let Ok(_lifecycle) = lifecycle_gate.lock() else {
            return;
        };
        if !runtime_startup_is_current(
            &exit_cleanup_started,
            &response_profile_switch_generation,
            switch_generation,
        ) || assistant_settings_generation.load(Ordering::SeqCst)
            != expected_assistant_settings_generation
        {
            return;
        }
        let published_profile = match &published_state {
            StartupStatePayload::WarmingModel {
                selected_response_profile,
                ..
            }
            | StartupStatePayload::Ready {
                selected_response_profile,
                ..
            } => *selected_response_profile,
            StartupStatePayload::Error { .. } => return,
        };
        let Ok(mut session) = conversation_session.lock() else {
            return;
        };
        if session.has_active_execution() {
            return;
        }
        let Ok(mut runtime_slot) = llama_cpp_runtime.lock() else {
            return;
        };
        let Ok(mut selected) = selected_response_profile.lock() else {
            return;
        };
        let Ok(mut startup) = startup_state.lock() else {
            return;
        };
        if matches!(*startup, StartupStatePayload::Error { .. }) {
            return;
        }
        if let Some(policy) = policy_to_publish {
            if session.replace_policy(policy).is_err() {
                return;
            }
            assistant_settings_generation.fetch_add(1, Ordering::SeqCst);
        }
        let previous_runtime = runtime_to_publish
            .take()
            .and_then(|runtime| runtime_slot.replace(runtime));
        *selected = published_profile;
        merge_profile_startup_publication(&mut startup, published_state);
        persistence_rollback.disarm();
        drop(startup);
        drop(selected);
        drop(runtime_slot);
        drop(session);
        drop(_lifecycle);
        if let Some(mut runtime) = previous_runtime {
            runtime.shutdown_owned();
        }
    });
    register_llama_startup(
        &llama_startups,
        &app_state.exit_cleanup_started,
        startup_cancellation,
        startup_coordinator,
    );

    Ok(response)
}

#[derive(Clone, Debug, Serialize)]
struct PromptFinalPayload {
    request_id: String,
    runtime_phase: RuntimePhasePayload,
    outcome: String,
    error_message: Option<String>,
}

enum OpencodePromptResult {
    Completed(String),
    Cancelled,
    Failed(String),
}

struct AgentTextResult {
    text: String,
    evidence: Vec<voxgolem_platform::opencode::OpencodeToolEvidence>,
    refusal: bool,
}

struct DeepTask {
    handle: Arc<Mutex<Option<tauri::async_runtime::JoinHandle<DeepStageResult>>>>,
    abort: Arc<Mutex<Option<tokio::task::AbortHandle>>>,
    cancellation: Arc<tokio::sync::watch::Sender<bool>>,
}

struct DeepTaskJoin {
    handle: Option<tauri::async_runtime::JoinHandle<DeepStageResult>>,
    cancellation: Arc<tokio::sync::watch::Sender<bool>>,
}

impl Drop for DeepTaskJoin {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            self.cancellation.send_replace(true);
            handle.abort();
        }
    }
}

impl DeepTask {
    async fn join(self) -> Result<DeepStageResult, String> {
        let handle = self
            .handle
            .lock()
            .map_err(|_| String::from("Deep task handle lock is poisoned"))?
            .take()
            .ok_or_else(|| String::from("Deep task was cancelled"))?;
        let mut pending = DeepTaskJoin {
            handle: Some(handle),
            cancellation: Arc::clone(&self.cancellation),
        };
        let result = pending
            .handle
            .as_mut()
            .expect("Deep task join should own its handle")
            .await
            .map_err(|error| format!("Deep task failed: {error}"));
        take_once(&self.abort);
        pending.handle.take();
        result
    }
}

impl Drop for DeepTask {
    fn drop(&mut self) {
        let abort = take_once(&self.abort);
        let handle = take_once(&self.handle);
        if abort.is_some() || handle.is_some() {
            self.cancellation.send_replace(true);
        }
        if let Some(abort) = abort {
            abort.abort();
        }
        if let Some(handle) = handle {
            handle.abort();
        }
    }
}

struct DeepStageResult {
    report: Option<voxgolem_core::agent_pipeline::DeepReport>,
    elapsed_ms: u64,
    model: voxgolem_core::assistant::AgentModel,
}

struct StageContext<'a> {
    app: &'a tauri::AppHandle,
    app_state: &'a AppState,
    request_id: &'a str,
    request: &'a AssistantRequestContext,
    active_generation: u64,
    deep_task: Option<DeepTask>,
    cancellation: PromptCancellation<'a>,
}

fn stage_context<'a>(
    app: &'a tauri::AppHandle,
    app_state: &'a AppState,
    request_id: &'a str,
    request: &'a AssistantRequestContext,
    active_generation: u64,
    deep_task: Option<DeepTask>,
    cancellation: PromptCancellation<'a>,
) -> StageContext<'a> {
    StageContext {
        app,
        app_state,
        request_id,
        request,
        active_generation,
        deep_task,
        cancellation,
    }
}

fn start_deep_task(
    app: &tauri::AppHandle,
    request_id: &str,
    prompt: &str,
    history: &[voxgolem_core::assistant::ConversationTurn],
    model: voxgolem_core::assistant::AgentModel,
    control: &ActivePrompt,
) -> Option<DeepTask> {
    let cancelled = Arc::clone(&control.cancelled);
    let signal = Arc::clone(&control.cancellation_signal);
    let physical_generation = control.generation;
    let handle_slot = Arc::clone(&control.deep_handle);
    let abort_slot = Arc::clone(&control.deep_abort);
    let cleanup_permit = app.state::<AppState>().prompt_cleanup.register();
    let task_app = app.clone();
    let original_request_id = request_id.to_string();
    let agent_request_id = format!("{request_id}-deep");
    let prompt = prompt.to_string();
    let history = history.to_vec();
    let task_signal = Arc::clone(&signal);
    let handle = tauri::async_runtime::spawn(async move {
        let _cleanup_permit = cleanup_permit;
        let started = Instant::now();
        let request = voxgolem_core::agent_pipeline::DeepRequest {
            original_request: prompt,
            canonical_history: agent_history(&history),
        };
        let is_opencode = matches!(
            model,
            voxgolem_core::assistant::AgentModel::OpenCodeSolHigh
                | voxgolem_core::assistant::AgentModel::OpenCodeLunaLow
        );
        let prompt = if is_opencode {
            voxgolem_core::agent_pipeline::opencode_deep_prompt(&request)
        } else {
            voxgolem_core::agent_pipeline::custom_deep_prompt(&request)
        };
        let _ = emit_stage_event_controlled(
            &task_app,
            &original_request_id,
            physical_generation,
            StagePayload::Deep,
            StageStatusPayload::Running,
            None,
        );
        let result = run_agent_text(
            &task_app.state::<AppState>(),
            model,
            if is_opencode {
                voxgolem_platform::opencode::OpencodeToolPolicy::Research
            } else {
                voxgolem_platform::opencode::OpencodeToolPolicy::AnswerOnly
            },
            &agent_request_id,
            &prompt,
            &cancelled,
            &task_signal,
        )
        .await;
        let report = result.and_then(|result| {
            if result.refusal {
                return Err(String::from("Deep provider refusal"));
            }
            parse_deep_agent_json_with_evidence(
                &result.text,
                started.elapsed().as_millis() as u64,
                is_opencode,
                &result.evidence,
            )
        });
        DeepStageResult {
            model,
            elapsed_ms: started.elapsed().as_millis() as u64,
            report: report.ok(),
        }
    });
    let state = app.state::<AppState>();
    let _lifecycle = state.lifecycle_gate.lock().ok()?;
    let active = state.active_prompt.lock().ok()?;
    let current = active.as_ref().is_some_and(|active| {
        active.request_id == request_id
            && Arc::ptr_eq(&active.deep_handle, &handle_slot)
            && !active.cancelled.load(Ordering::Acquire)
    });
    if !current {
        handle.abort();
        return None;
    }
    abort_slot
        .lock()
        .ok()?
        .replace(handle.inner().abort_handle());
    handle_slot.lock().ok()?.replace(handle);
    Some(DeepTask {
        handle: handle_slot,
        abort: abort_slot,
        cancellation: signal,
    })
}

fn initial_stage_sequence(deep_enabled: bool) -> Vec<StagePayload> {
    let mut stages = Vec::new();
    if deep_enabled {
        stages.push(StagePayload::Deep);
    }
    stages.push(StagePayload::Instant);
    stages
}

fn ensure_agent_prerequisites(
    app_state: &AppState,
    model: voxgolem_core::assistant::AgentModel,
) -> Result<(), String> {
    use voxgolem_core::assistant::AgentModel;
    match model {
        AgentModel::CustomSolHigh | AgentModel::CustomLunaLow => {
            if app_state
                .runtime_config
                .as_ref()
                .and_then(|config| config.custom_openai.as_ref())
                .is_none()
            {
                return Err(String::from("Custom provider is not configured"));
            }
        }
        AgentModel::OpenCodeSolHigh | AgentModel::OpenCodeLunaLow => {
            if app_state
                .opencode_server
                .lock()
                .map_err(|_| String::from("opencode server lock is poisoned"))?
                .is_none()
            {
                return Err(String::from("OpenCode server is not available"));
            }
        }
    }
    Ok(())
}

struct AssistantRequestContext {
    generation: voxgolem_core::conversation::ExecutionId,
    instant_model: voxgolem_core::assistant::InstantModel,
    preferences: voxgolem_core::assistant::AssistantPolicy,
    history: Vec<voxgolem_core::assistant::ConversationTurn>,
    started_ms: u64,
    source: CompletionSource,
    prompt: String,
}

struct RegisteredAssistantRequest {
    request: AssistantRequestContext,
    physical_generation: u64,
    cancelled: Arc<AtomicBool>,
    cancellation_signal: Arc<tokio::sync::watch::Sender<bool>>,
    opencode_client: Option<voxgolem_platform::opencode::OpencodeClient>,
    control: ActivePrompt,
    guard: ActivePromptGuard,
}

#[derive(Clone, Copy)]
struct PromptCancellation<'a> {
    active_generation: u64,
    cancelled: &'a AtomicBool,
    signal: &'a tokio::sync::watch::Sender<bool>,
    control: &'a ActivePrompt,
}

fn begin_registered_assistant_request(
    app_state: &AppState,
    request_id: &str,
    prompt: &str,
    source: CompletionSource,
) -> Result<RegisteredAssistantRequest, String> {
    let started_ms = current_time_ms()?;
    let _publication_admission = app_state
        .prompt_publication_gate
        .lock()
        .map_err(|_| String::from("prompt publication gate is poisoned"))?;
    let _lifecycle = app_state
        .lifecycle_gate
        .lock()
        .map_err(|_| String::from("lifecycle gate is poisoned"))?;
    if app_state.reset_in_progress.load(Ordering::Acquire) {
        return Err(String::from("session reset is in progress"));
    }
    if app_state
        .settings_publication_in_progress
        .load(Ordering::Acquire)
    {
        return Err(String::from("assistant settings update is in progress"));
    }
    ensure_startup_ready_for_prompt(&app_state.startup_state)?;
    if current_runtime_phase(&app_state.voice_pipeline_state)? == RuntimePhasePayload::Error {
        apply_voice_pipeline_transition(
            &app_state.voice_pipeline_state,
            app_state.voice_pipeline_config,
            voxgolem_core::voice_pipeline::VoicePipelineEvent::ResetToIdle,
        )?;
    }
    let opencode_client = app_state
        .opencode_server
        .lock()
        .map_err(|_| String::from("opencode server lock is poisoned"))?
        .as_ref()
        .map(voxgolem_platform::opencode::OpencodeServer::client);
    let mut coordinator = app_state
        .conversation_session
        .lock()
        .map_err(|_| String::from("assistant coordinator lock is poisoned"))?;
    let is_opencode = matches!(
        coordinator.policy().instant_model,
        voxgolem_core::assistant::InstantModel::OpenCodeSolHigh
            | voxgolem_core::assistant::InstantModel::OpenCodeLunaLow
    );
    if is_opencode && opencode_client.is_none() {
        return Err(String::from("OpenCode server is not available"));
    }
    let snapshot = coordinator
        .admit(request_id, prompt)
        .map_err(|error| format!("assistant request could not start: {error:?}"))?;
    let history = snapshot
        .history
        .iter()
        .map(|message| voxgolem_core::assistant::ConversationTurn {
            role: message.role,
            content: message.content.clone(),
        })
        .collect();
    let preferences = snapshot.policy.clone();
    let instant_model = preferences.instant_model;
    let execution_id = snapshot.execution_id;
    drop(coordinator);

    let selected_client = is_opencode.then(|| {
        opencode_client
            .clone()
            .expect("OpenCode availability was checked before admission")
    });
    let (physical_generation, cancelled, cancellation_signal, settled) =
        match register_active_prompt(
            &app_state.active_prompt,
            &app_state.active_prompt_generation,
            request_id,
            execution_id,
            None,
            app_state.prompt_publication_epoch.load(Ordering::SeqCst),
        ) {
            Ok(registration) => registration,
            Err(error) => {
                if let Ok(mut session) = app_state.conversation_session.lock() {
                    let _ = session.settle(
                        execution_id,
                        voxgolem_core::conversation::SettlementIntent::Cancelled,
                    );
                }
                return Err(error);
            }
        };
    let control = app_state
        .active_prompt
        .lock()
        .map_err(|_| String::from("active prompt lock is poisoned"))?
        .as_ref()
        .filter(|active| {
            active.request_id == request_id && active.generation == physical_generation
        })
        .cloned()
        .ok_or_else(|| String::from("active prompt registration disappeared"))?;
    if let Err(error) = apply_voice_pipeline_transition(
        &app_state.voice_pipeline_state,
        app_state.voice_pipeline_config,
        voxgolem_core::voice_pipeline::VoicePipelineEvent::SubmitPrompt,
    ) {
        if let Ok(mut session) = app_state.conversation_session.lock() {
            let _ = session.settle(
                execution_id,
                voxgolem_core::conversation::SettlementIntent::Failed,
            );
        }
        let _ = clear_active_prompt(&app_state.active_prompt, request_id, physical_generation);
        return Err(error);
    }
    let guard = ActivePromptGuard {
        active_prompt: Arc::clone(&app_state.active_prompt),
        conversation_session: Arc::clone(&app_state.conversation_session),
        request_id: request_id.to_string(),
        generation: physical_generation,
        execution_id,
        cancelled: Arc::clone(&cancelled),
        armed: Arc::new(AtomicBool::new(true)),
        abort_client: Arc::clone(&control.abort_client),
        prompt_cleanup: Arc::clone(&app_state.prompt_cleanup),
        settled,
        lifecycle_gate: Arc::clone(&app_state.lifecycle_gate),
        terminal_kind: Arc::clone(&control.terminal_kind),
        voice_pipeline_state: Arc::clone(&app_state.voice_pipeline_state),
        voice_pipeline_config: app_state.voice_pipeline_config,
    };

    Ok(RegisteredAssistantRequest {
        request: AssistantRequestContext {
            generation: execution_id,
            instant_model,
            preferences,
            history,
            started_ms,
            source,
            prompt: prompt.to_string(),
        },
        physical_generation,
        cancelled,
        cancellation_signal,
        opencode_client: selected_client,
        control,
        guard,
    })
}

fn finish_assistant_request(
    app_state: &AppState,
    request_id: &str,
    active_generation: u64,
    generation: voxgolem_core::conversation::ExecutionId,
    result: voxgolem_core::assistant::InstantOutcome,
) -> Result<voxgolem_core::conversation::UpdateResult, String> {
    accept_assistant_stage_if_active(
        app_state,
        request_id,
        active_generation,
        generation,
        voxgolem_core::assistant::Stage::Instant,
        voxgolem_core::assistant::StageResult::Instant(result),
    )
}

fn accept_assistant_stage_if_active(
    app_state: &AppState,
    request_id: &str,
    active_generation: u64,
    generation: voxgolem_core::conversation::ExecutionId,
    stage: voxgolem_core::assistant::Stage,
    result: voxgolem_core::assistant::StageResult,
) -> Result<voxgolem_core::conversation::UpdateResult, String> {
    let _lifecycle = app_state
        .lifecycle_gate
        .lock()
        .map_err(|_| String::from("lifecycle gate is poisoned"))?;
    let active = app_state
        .active_prompt
        .lock()
        .map_err(|_| String::from("active prompt lock is poisoned"))?;
    if !active.as_ref().is_some_and(|active| {
        active.request_id == request_id
            && active.generation == active_generation
            && !active.cancelled.load(Ordering::SeqCst)
    }) {
        return Ok(voxgolem_core::conversation::UpdateResult::IgnoredStale);
    }
    Ok(app_state
        .conversation_session
        .lock()
        .map_err(|_| String::from("assistant coordinator lock is poisoned"))?
        .update(generation, stage, result))
}

fn commit_assistant_request_if_active(
    app_state: &AppState,
    request_id: &str,
    active_generation: u64,
    generation: voxgolem_core::conversation::ExecutionId,
    control: &ActivePrompt,
) -> Result<CommitAssistantResult, String> {
    let _lifecycle = app_state
        .lifecycle_gate
        .lock()
        .map_err(|_| String::from("lifecycle gate is poisoned"))?;
    let removed = {
        let mut active = app_state
            .active_prompt
            .lock()
            .map_err(|_| String::from("active prompt lock is poisoned"))?;
        let Some(current) = active.as_ref().filter(|active| {
            active.request_id == request_id && active.generation == active_generation
        }) else {
            return if control
                .terminal_kind
                .lock()
                .ok()
                .and_then(|terminal| *terminal)
                == Some(TerminalKind::Cancelled)
            {
                Ok(CommitAssistantResult::Cancelled)
            } else {
                Err(String::from("assistant request is no longer active"))
            };
        };
        if current
            .terminal_kind
            .lock()
            .ok()
            .and_then(|terminal| *terminal)
            == Some(TerminalKind::Cancelled)
        {
            return Ok(CommitAssistantResult::Cancelled);
        }
        let result = app_state
            .conversation_session
            .lock()
            .map_err(|_| String::from("assistant coordinator lock is poisoned"))?
            .settle(
                generation,
                voxgolem_core::conversation::SettlementIntent::Success,
            );
        match result {
            voxgolem_core::conversation::SettlementResult::Applied(
                outcome @ (voxgolem_core::conversation::AppliedOutcome::Completed(_)
                | voxgolem_core::conversation::AppliedOutcome::Refused(_)),
            ) => {
                record_terminal_kind(
                    &current.terminal_kind,
                    if matches!(
                        outcome,
                        voxgolem_core::conversation::AppliedOutcome::Refused(_)
                    ) {
                        TerminalKind::Refused
                    } else {
                        TerminalKind::Completed
                    },
                );
                current.settled.store(true, Ordering::Release);
                let _ = apply_voice_pipeline_transition(
                    &app_state.voice_pipeline_state,
                    app_state.voice_pipeline_config,
                    voxgolem_core::voice_pipeline::VoicePipelineEvent::PromptCompleted,
                );
                active.take()
            }
            voxgolem_core::conversation::SettlementResult::Applied(
                voxgolem_core::conversation::AppliedOutcome::Cancelled,
            ) => return Ok(CommitAssistantResult::Cancelled),
            voxgolem_core::conversation::SettlementResult::Applied(
                voxgolem_core::conversation::AppliedOutcome::Failed,
            ) => return Err(String::from("assistant request failed")),
            voxgolem_core::conversation::SettlementResult::IgnoredStale
                if control
                    .terminal_kind
                    .lock()
                    .ok()
                    .and_then(|terminal| *terminal)
                    == Some(TerminalKind::Cancelled) =>
            {
                return Ok(CommitAssistantResult::Cancelled);
            }
            voxgolem_core::conversation::SettlementResult::IgnoredStale => {
                return Err(String::from("assistant request is stale"));
            }
            voxgolem_core::conversation::SettlementResult::Rejected(_) => {
                return Err(String::from("assistant request did not resolve"));
            }
        }
    };
    drop(_lifecycle);
    if let Some(removed) = removed {
        removed.completion_signal.notify_one();
    }
    Ok(CommitAssistantResult::Committed)
}

fn cancel_assistant_request(
    app_state: &AppState,
    generation: voxgolem_core::conversation::ExecutionId,
) -> voxgolem_core::conversation::SettlementResult {
    let Ok(_lifecycle) = app_state.lifecycle_gate.lock() else {
        return voxgolem_core::conversation::SettlementResult::IgnoredStale;
    };
    let Ok(mut active) = app_state.active_prompt.lock() else {
        return voxgolem_core::conversation::SettlementResult::IgnoredStale;
    };
    let Ok(mut session) = app_state.conversation_session.lock() else {
        return voxgolem_core::conversation::SettlementResult::IgnoredStale;
    };
    let matching = active
        .as_ref()
        .is_some_and(|active| active.assistant_generation == generation);
    let result = session.settle(
        generation,
        voxgolem_core::conversation::SettlementIntent::Cancelled,
    );
    let cleanup = if matches!(
        result,
        voxgolem_core::conversation::SettlementResult::Applied(
            voxgolem_core::conversation::AppliedOutcome::Cancelled
        )
    ) && matching
    {
        let current = active.as_ref().expect("matching prompt should exist");
        record_terminal_kind(&current.terminal_kind, TerminalKind::Cancelled);
        current.cancelled.store(true, Ordering::Release);
        current.settled.store(true, Ordering::Release);
        repair_cancelled_prompt_runtime(app_state);
        let abort_claim = claim_prompt_abort(&current.abort_client, &app_state.prompt_cleanup);
        let deep_handle = claim_prompt_deep_handle(current);
        let deep_abort = claim_prompt_deep_abort(current);
        Some(PromptPhysicalCleanup {
            active: active.take().expect("matching prompt should exist"),
            abort_claim,
            deep_handle,
            deep_abort,
            cancel: true,
        })
    } else {
        None
    };
    drop(session);
    drop(active);
    drop(_lifecycle);
    if let Some(cleanup) = cleanup {
        cleanup.cancel_detached();
    }
    result
}

fn fail_assistant_request(
    app_state: &AppState,
    generation: voxgolem_core::conversation::ExecutionId,
    message: &str,
) -> voxgolem_core::conversation::SettlementResult {
    let Ok(_lifecycle) = app_state.lifecycle_gate.lock() else {
        return voxgolem_core::conversation::SettlementResult::IgnoredStale;
    };
    let Ok(mut active) = app_state.active_prompt.lock() else {
        return voxgolem_core::conversation::SettlementResult::IgnoredStale;
    };
    let Ok(mut session) = app_state.conversation_session.lock() else {
        return voxgolem_core::conversation::SettlementResult::IgnoredStale;
    };
    let matching = active
        .as_ref()
        .is_some_and(|active| active.assistant_generation == generation);
    let result = session.settle(
        generation,
        voxgolem_core::conversation::SettlementIntent::Failed,
    );
    let cleanup = if matches!(
        result,
        voxgolem_core::conversation::SettlementResult::Applied(
            voxgolem_core::conversation::AppliedOutcome::Failed
        )
    ) && matching
    {
        let current = active.as_ref().expect("matching prompt should exist");
        record_terminal_kind(&current.terminal_kind, TerminalKind::Failed);
        current.cancelled.store(true, Ordering::Release);
        current.settled.store(true, Ordering::Release);
        repair_failed_prompt_runtime(app_state, message);
        let abort_claim = claim_prompt_abort(&current.abort_client, &app_state.prompt_cleanup);
        let deep_handle = claim_prompt_deep_handle(current);
        let deep_abort = claim_prompt_deep_abort(current);
        Some(PromptPhysicalCleanup {
            active: active.take().expect("matching prompt should exist"),
            abort_claim,
            deep_handle,
            deep_abort,
            cancel: true,
        })
    } else {
        None
    };
    drop(session);
    drop(active);
    drop(_lifecycle);
    if let Some(cleanup) = cleanup {
        cleanup.cancel_detached();
    }
    result
}

struct OpencodeStreamContext<'a> {
    app: &'a tauri::AppHandle,
    active_prompt: &'a Mutex<Option<ActivePrompt>>,
    request_id: &'a str,
    generation: u64,
    client: &'a voxgolem_platform::opencode::OpencodeClient,
    cancelled: &'a AtomicBool,
    cancellation_signal: &'a tokio::sync::watch::Sender<bool>,
}

struct TransientOpencodeClient {
    client: voxgolem_platform::opencode::OpencodeClient,
    armed: bool,
    cleanup_permit: Option<PromptCleanupPermit>,
}

type CleanupCallback<T> = Box<dyn FnOnce(T) -> futures_util::future::BoxFuture<'static, ()> + Send>;

struct SupervisedCreation<T: Send + 'static> {
    handle: Option<tauri::async_runtime::JoinHandle<T>>,
    cleanup: Option<CleanupCallback<T>>,
}

impl<T: Send + 'static> SupervisedCreation<T> {
    fn new(handle: tauri::async_runtime::JoinHandle<T>, cleanup: CleanupCallback<T>) -> Self {
        Self {
            handle: Some(handle),
            cleanup: Some(cleanup),
        }
    }

    async fn join(&mut self) -> Result<T, String> {
        let result = self
            .handle
            .as_mut()
            .expect("supervised creation handle must exist")
            .await
            .map_err(|error| error.to_string())?;
        self.handle.take();
        self.cleanup.take();
        Ok(result)
    }
}

impl<T: Send + 'static> Drop for SupervisedCreation<T> {
    fn drop(&mut self) {
        let (Some(handle), Some(cleanup)) = (self.handle.take(), self.cleanup.take()) else {
            return;
        };
        tauri::async_runtime::spawn(async move {
            if let Ok(value) = handle.await {
                cleanup(value).await;
            }
        });
    }
}

struct TransientOpencodeCreation {
    inner: SupervisedCreation<
        Result<
            voxgolem_platform::opencode::OpencodeClient,
            voxgolem_platform::opencode::OpencodeServerError,
        >,
    >,
    cleanup_permit: Arc<Mutex<Option<PromptCleanupPermit>>>,
}

async fn abort_direct_opencode_client(client: &voxgolem_platform::opencode::OpencodeClient) {
    let _ = tokio::time::timeout(Duration::from_secs(1), client.abort()).await;
}

impl TransientOpencodeClient {
    fn spawn_cleanup(&mut self) -> tauri::async_runtime::JoinHandle<()> {
        self.armed = false;
        let client = self.client.clone();
        let cleanup_permit = self
            .cleanup_permit
            .take()
            .expect("transient client should own cleanup permit");
        tauri::async_runtime::spawn(async move {
            let _cleanup_permit = cleanup_permit;
            cleanup_transient_client(client).await;
        })
    }

    async fn finish(mut self) {
        let task = self.spawn_cleanup();
        let _ = task.await;
    }
}

impl Drop for TransientOpencodeClient {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        drop(self.spawn_cleanup());
    }
}

impl TransientOpencodeCreation {
    fn new(
        base: voxgolem_platform::opencode::OpencodeClient,
        prompt_cleanup: &Arc<PromptCleanupTracker>,
    ) -> Self {
        let cleanup_permit = Arc::new(Mutex::new(Some(prompt_cleanup.register())));
        let permit_for_cleanup = Arc::clone(&cleanup_permit);
        let handle = tauri::async_runtime::spawn(async move { base.create_transient().await });
        Self {
            inner: SupervisedCreation::new(
                handle,
                Box::new(move |result| {
                    let cleanup_permit = take_once(&permit_for_cleanup);
                    Box::pin(async move {
                        let _cleanup_permit = cleanup_permit;
                        if let Ok(client) = result {
                            cleanup_transient_client(client).await;
                        }
                    })
                }),
            ),
            cleanup_permit,
        }
    }

    async fn join(
        &mut self,
    ) -> Result<
        (
            voxgolem_platform::opencode::OpencodeClient,
            PromptCleanupPermit,
        ),
        String,
    > {
        let client = self
            .inner
            .join()
            .await?
            .map_err(|error| error.to_string())?;
        let cleanup_permit = take_once(&self.cleanup_permit)
            .ok_or_else(|| String::from("transient cleanup ownership disappeared"))?;
        Ok((client, cleanup_permit))
    }
}

async fn create_transient_opencode_client(
    base: &voxgolem_platform::opencode::OpencodeClient,
    cancellation: &tokio::sync::watch::Sender<bool>,
    prompt_cleanup: &Arc<PromptCleanupTracker>,
) -> Result<TransientOpencodeClient, String> {
    let mut receiver = cancellation.subscribe();
    if *receiver.borrow() {
        return Err(String::from("assistant request cancelled"));
    }
    let mut creation = TransientOpencodeCreation::new(base.clone(), prompt_cleanup);
    tokio::select! {
        biased;
        _ = receiver.changed() => Err(String::from("assistant request cancelled")),
        result = creation.join() => result
            .map(|(client, cleanup_permit)| TransientOpencodeClient {
                client,
                armed: true,
                cleanup_permit: Some(cleanup_permit),
            }),
    }
}

async fn cleanup_transient_client(client: voxgolem_platform::opencode::OpencodeClient) {
    cleanup_sequential(
        Duration::from_secs(1),
        || client.abort(),
        || client.delete(),
    )
    .await;
}

async fn cleanup_sequential<A, D, AF, DF, AR, DR>(timeout: Duration, abort: A, delete: D)
where
    A: FnOnce() -> AF,
    D: FnOnce() -> DF,
    AF: std::future::Future<Output = AR>,
    DF: std::future::Future<Output = DR>,
{
    let _ = tokio::time::timeout(timeout, abort()).await;
    let _ = tokio::time::timeout(timeout, delete()).await;
}

async fn race_durable_cancellation<F, T>(
    receiver: &mut tokio::sync::watch::Receiver<bool>,
    future: F,
) -> Result<T, ()>
where
    F: std::future::Future<Output = T>,
{
    if *receiver.borrow() {
        return Err(());
    }
    tokio::select! {
        biased;
        _ = receiver.changed() => Err(()),
        result = future => Ok(result),
    }
}

#[tauri::command]
async fn submit_prompt(
    request_id: String,
    prompt: String,
    source: Option<CompletionSource>,
    updates: Channel<PromptExecutionEventPayload>,
    app: tauri::AppHandle,
    app_state: tauri::State<'_, AppState>,
) -> Result<PromptFinalPayload, String> {
    ensure_startup_ready_for_prompt(&app_state.startup_state)?;
    validate_prompt_request_id(&request_id)?;
    let prompt = validate_prompt_text(prompt)?;
    let registered = begin_registered_assistant_request(
        &app_state,
        &request_id,
        &prompt,
        source.unwrap_or(CompletionSource::Typed),
    )?;
    let RegisteredAssistantRequest {
        request: assistant_request,
        physical_generation: generation,
        cancelled,
        cancellation_signal,
        opencode_client,
        control: active_prompt_control,
        guard: active_prompt_guard,
    } = registered;
    active_prompt_control
        .updates
        .lock()
        .map_err(|_| String::from("prompt update channel lock is poisoned"))?
        .replace(updates);
    let is_opencode = opencode_client.is_some();
    cancel_active_tts_generation(&app_state);
    if assistant_request.preferences.deep_enabled {
        if let Err(error) =
            ensure_agent_prerequisites(&app_state, assistant_request.preferences.deep_model)
        {
            fail_assistant_request(&app_state, assistant_request.generation, &error);
            return Err(error);
        }
    }
    if assistant_request.preferences.review_enabled {
        if let Err(error) =
            ensure_agent_prerequisites(&app_state, assistant_request.preferences.review_model)
        {
            fail_assistant_request(&app_state, assistant_request.generation, &error);
            return Err(error);
        }
    }
    if let Some(expected_profile) = local_profile_for_model(assistant_request.instant_model) {
        let selected_profile = *app_state
            .selected_response_profile
            .lock()
            .map_err(|_| String::from("selected response profile lock is poisoned"))?;
        if selected_profile != expected_profile {
            fail_assistant_request(
                &app_state,
                assistant_request.generation,
                "selected local model is not loaded",
            );
            return Err(String::from("selected local model is not loaded"));
        }
    }
    let mut deep_task = None;
    for stage in initial_stage_sequence(assistant_request.preferences.deep_enabled) {
        if stage == StagePayload::Deep {
            let stage_result = emit_stage_event_controlled(
                &app,
                &request_id,
                generation,
                StagePayload::Deep,
                StageStatusPayload::Queued,
                None,
            );
            if let Err(error) = stage_result {
                match error {
                    PromptControlError::Cancelled => {
                        return finish_cancelled_prompt(
                            &app,
                            &app_state,
                            &request_id,
                            assistant_request.generation,
                            &active_prompt_control,
                        );
                    }
                    PromptControlError::Error(error) => return Err(error),
                }
            }
            deep_task = start_deep_task(
                &app,
                &request_id,
                &assistant_request.prompt,
                &assistant_request.history,
                assistant_request.preferences.deep_model,
                &active_prompt_control,
            );
        } else {
            let stage_result = emit_stage_event_controlled(
                &app,
                &request_id,
                generation,
                StagePayload::Instant,
                StageStatusPayload::Running,
                None,
            );
            if let Err(error) = stage_result {
                match error {
                    PromptControlError::Cancelled => {
                        return finish_cancelled_prompt(
                            &app,
                            &app_state,
                            &request_id,
                            assistant_request.generation,
                            &active_prompt_control,
                        );
                    }
                    PromptControlError::Error(error) => return Err(error),
                }
            }
        }
    }
    let prefetch_key = PrefetchKey {
        prompt: prompt.clone(),
        history: assistant_request.history.clone(),
        model: assistant_request.instant_model,
    };
    let prefetched_answer = {
        let _lifecycle = app_state
            .completion_lifecycle_lock
            .lock()
            .map_err(|_| String::from("completion lifecycle lock is poisoned"))?;
        let prefetched_answer = take_and_invalidate_prefetch(
            &app_state.prefetch_cache,
            &app_state.prefetch_generation,
            &app_state.assistant_settings_generation,
            &prefetch_key,
        )?;
        clear_completion_request_state_locked(&app_state, false)?;
        invalidate_prefetch(&app_state)?;
        prefetched_answer
    };
    if let Some(answer) = prefetched_answer {
        let answer_text = assistant_content_text(&answer).to_string();
        match emit_prompt_event_controlled(
            &app,
            &request_id,
            generation,
            PromptExecutionEventPayload::Text {
                text: answer_text.clone(),
            },
        ) {
            Ok(()) => {}
            Err(PromptControlError::Cancelled) => {
                return finish_cancelled_prompt(
                    &app,
                    &app_state,
                    &request_id,
                    assistant_request.generation,
                    &active_prompt_control,
                );
            }
            Err(PromptControlError::Error(error)) => return Err(error),
        }
        let instant_result = finish_assistant_request(
            &app_state,
            &request_id,
            generation,
            assistant_request.generation,
            voxgolem_core::assistant::InstantOutcome::Complete(answer),
        )?;
        if let Err(error) = resolve_enabled_agents(
            stage_context(
                &app,
                &app_state,
                &request_id,
                &assistant_request,
                generation,
                deep_task,
                PromptCancellation {
                    active_generation: generation,
                    cancelled: &cancelled,
                    signal: &cancellation_signal,
                    control: &active_prompt_control,
                },
            ),
            &answer_text,
            instant_result,
        )
        .await
        {
            if matches!(error, PromptControlError::Cancelled) {
                return finish_cancelled_prompt(
                    &app,
                    &app_state,
                    &request_id,
                    assistant_request.generation,
                    &active_prompt_control,
                );
            }
            return fail_started_prompt(
                &app,
                &app_state,
                &request_id,
                assistant_request.generation,
                &active_prompt_control,
                error.into_message(),
            );
        }
        if cancelled.load(Ordering::SeqCst) {
            return finish_cancelled_prompt(
                &app,
                &app_state,
                &request_id,
                assistant_request.generation,
                &active_prompt_control,
            );
        }
        if commit_assistant_request_if_active(
            &app_state,
            &request_id,
            generation,
            assistant_request.generation,
            &active_prompt_control,
        )? == CommitAssistantResult::Cancelled
        {
            return finish_cancelled_prompt(
                &app,
                &app_state,
                &request_id,
                assistant_request.generation,
                &active_prompt_control,
            );
        }
        let runtime_phase = current_runtime_phase(&app_state.voice_pipeline_state)?;
        let _ = emit_terminal_prompt_event_controlled(
            &app,
            &active_prompt_control,
            PromptExecutionEventPayload::Completed {
                runtime_phase: runtime_phase.clone(),
            },
        );
        active_prompt_guard.finish();
        return Ok(PromptFinalPayload {
            request_id,
            runtime_phase,
            outcome: String::from("completed"),
            error_message: None,
        });
    }
    if matches!(
        assistant_request.instant_model,
        voxgolem_core::assistant::InstantModel::CustomSolHigh
            | voxgolem_core::assistant::InstantModel::CustomLunaLow
    ) {
        return submit_custom_prompt(
            &request_id,
            &prompt,
            assistant_request,
            &app,
            &app_state,
            PromptCancellation {
                active_generation: generation,
                cancelled: &cancelled,
                signal: &cancellation_signal,
                control: &active_prompt_control,
            },
            deep_task,
        )
        .await;
    }
    if !is_opencode {
        let expected_profile = match assistant_request.instant_model {
            voxgolem_core::assistant::InstantModel::LocalFast => ResponseProfilePayload::Fast,
            voxgolem_core::assistant::InstantModel::LocalQuality => ResponseProfilePayload::Quality,
            _ => unreachable!("non-local providers return before local dispatch"),
        };
        if *app_state
            .selected_response_profile
            .lock()
            .map_err(|_| String::from("selected response profile lock is poisoned"))?
            != expected_profile
        {
            fail_assistant_request(
                &app_state,
                assistant_request.generation,
                "selected local model is not loaded",
            );
            return Err(String::from("selected local model is not loaded"));
        }
        sync_llama_history(&app_state, &assistant_request.history)?;
        let result = match stream_local_prompt(
            &app,
            &app_state,
            &request_id,
            generation,
            &prompt,
            &cancelled,
        )
        .await
        {
            Ok(result) => result,
            Err(PromptControlError::Cancelled) => {
                return finish_cancelled_prompt(
                    &app,
                    &app_state,
                    &request_id,
                    assistant_request.generation,
                    &active_prompt_control,
                )
            }
            Err(PromptControlError::Error(error)) => {
                if let Err(publication) = emit_prompt_event_controlled(
                    &app,
                    &request_id,
                    generation,
                    PromptExecutionEventPayload::Error {
                        message: error.clone(),
                    },
                ) {
                    match publication {
                        PromptControlError::Cancelled => {
                            return finish_cancelled_prompt(
                                &app,
                                &app_state,
                                &request_id,
                                assistant_request.generation,
                                &active_prompt_control,
                            )
                        }
                        PromptControlError::Error(error) => return Err(error),
                    }
                }
                match settle_instant_failure(
                    stage_context(
                        &app,
                        &app_state,
                        &request_id,
                        &assistant_request,
                        generation,
                        deep_task,
                        PromptCancellation {
                            active_generation: generation,
                            cancelled: &cancelled,
                            signal: &cancellation_signal,
                            control: &active_prompt_control,
                        },
                    ),
                    error,
                )
                .await
                {
                    Ok(Some(result)) => return Ok(result),
                    Ok(None) => return Err(String::from("local response failed")),
                    Err(error) => return Err(error),
                }
            }
        };
        let answer = prompt_execution_text(&result.events);
        if cancelled.load(Ordering::SeqCst) {
            return finish_cancelled_prompt(
                &app,
                &app_state,
                &request_id,
                assistant_request.generation,
                &active_prompt_control,
            );
        }
        if answer.trim().is_empty() {
            let message = String::from("response provider completed without visible text");
            match settle_instant_failure(
                stage_context(
                    &app,
                    &app_state,
                    &request_id,
                    &assistant_request,
                    generation,
                    deep_task,
                    PromptCancellation {
                        active_generation: generation,
                        cancelled: &cancelled,
                        signal: &cancellation_signal,
                        control: &active_prompt_control,
                    },
                ),
                message.clone(),
            )
            .await
            {
                Ok(Some(result)) => return Ok(result),
                Ok(None) => return Err(message),
                Err(error) => return Err(error),
            }
        }
        let instant_result = finish_assistant_request(
            &app_state,
            &request_id,
            generation,
            assistant_request.generation,
            voxgolem_core::assistant::InstantOutcome::Complete(
                voxgolem_core::assistant::Content::Text(answer.clone()),
            ),
        )?;
        if let Err(error) = resolve_enabled_agents(
            stage_context(
                &app,
                &app_state,
                &request_id,
                &assistant_request,
                generation,
                deep_task,
                PromptCancellation {
                    active_generation: generation,
                    cancelled: &cancelled,
                    signal: &cancellation_signal,
                    control: &active_prompt_control,
                },
            ),
            &answer,
            instant_result,
        )
        .await
        {
            if matches!(error, PromptControlError::Cancelled) {
                return finish_cancelled_prompt(
                    &app,
                    &app_state,
                    &request_id,
                    assistant_request.generation,
                    &active_prompt_control,
                );
            }
            return fail_started_prompt(
                &app,
                &app_state,
                &request_id,
                assistant_request.generation,
                &active_prompt_control,
                error.into_message(),
            );
        }
        if cancelled.load(Ordering::SeqCst) {
            return finish_cancelled_prompt(
                &app,
                &app_state,
                &request_id,
                assistant_request.generation,
                &active_prompt_control,
            );
        }
        if commit_assistant_request_if_active(
            &app_state,
            &request_id,
            generation,
            assistant_request.generation,
            &active_prompt_control,
        )? == CommitAssistantResult::Cancelled
        {
            return finish_cancelled_prompt(
                &app,
                &app_state,
                &request_id,
                assistant_request.generation,
                &active_prompt_control,
            );
        }
        let phase = current_runtime_phase(&app_state.voice_pipeline_state)?;
        let _ = emit_terminal_prompt_event_controlled(
            &app,
            &active_prompt_control,
            PromptExecutionEventPayload::Completed {
                runtime_phase: phase.clone(),
            },
        );
        active_prompt_guard.finish();
        return Ok(PromptFinalPayload {
            request_id,
            runtime_phase: phase,
            outcome: "completed".into(),
            error_message: None,
        });
    }
    let base_client = opencode_client.expect("OpenCode availability was checked before dispatch");
    let transient_client = match create_transient_opencode_client(
        &base_client,
        &cancellation_signal,
        &app_state.prompt_cleanup,
    )
    .await
    {
        Ok(client) => client,
        Err(_) if cancelled.load(Ordering::Acquire) => {
            return finish_cancelled_prompt(
                &app,
                &app_state,
                &request_id,
                assistant_request.generation,
                &active_prompt_control,
            );
        }
        Err(error) => {
            return fail_started_prompt(
                &app,
                &app_state,
                &request_id,
                assistant_request.generation,
                &active_prompt_control,
                error,
            );
        }
    };
    let result = stream_opencode_prompt(
        OpencodeStreamContext {
            app: &app,
            active_prompt: &app_state.active_prompt,
            request_id: &request_id,
            generation,
            client: &transient_client.client,
            cancelled: &cancelled,
            cancellation_signal: &cancellation_signal,
        },
        &render_provider_prompt(&assistant_request.history, &prompt),
        assistant_request.instant_model,
    )
    .await;
    transient_client.finish().await;
    if cancelled.load(Ordering::SeqCst) {
        return finish_cancelled_prompt(
            &app,
            &app_state,
            &request_id,
            assistant_request.generation,
            &active_prompt_control,
        );
    }
    if let OpencodePromptResult::Failed(message) = &result {
        if let Err(publication) = emit_prompt_event_controlled(
            &app,
            &request_id,
            generation,
            PromptExecutionEventPayload::Error {
                message: message.clone(),
            },
        ) {
            match publication {
                PromptControlError::Cancelled => {
                    return finish_cancelled_prompt(
                        &app,
                        &app_state,
                        &request_id,
                        assistant_request.generation,
                        &active_prompt_control,
                    )
                }
                PromptControlError::Error(error) => return Err(error),
            }
        }
        match settle_instant_failure(
            stage_context(
                &app,
                &app_state,
                &request_id,
                &assistant_request,
                generation,
                deep_task,
                PromptCancellation {
                    active_generation: generation,
                    cancelled: &cancelled,
                    signal: &cancellation_signal,
                    control: &active_prompt_control,
                },
            ),
            message.clone(),
        )
        .await
        {
            Ok(Some(result)) => return Ok(result),
            Ok(None) => return Err(message.clone()),
            Err(error) => return Err(error),
        }
    }

    let (outcome, event, error_message) = match result {
        OpencodePromptResult::Completed(answer) => {
            if answer.trim().is_empty() {
                let message = String::from("OpenCode completed without visible text");
                match settle_instant_failure(
                    stage_context(
                        &app,
                        &app_state,
                        &request_id,
                        &assistant_request,
                        generation,
                        deep_task,
                        PromptCancellation {
                            active_generation: generation,
                            cancelled: &cancelled,
                            signal: &cancellation_signal,
                            control: &active_prompt_control,
                        },
                    ),
                    message.clone(),
                )
                .await
                {
                    Ok(Some(result)) => return Ok(result),
                    Ok(None) => return Err(message),
                    Err(error) => return Err(error),
                }
            }
            let instant_result = finish_assistant_request(
                &app_state,
                &request_id,
                generation,
                assistant_request.generation,
                voxgolem_core::assistant::InstantOutcome::Complete(
                    voxgolem_core::assistant::Content::Text(answer.clone()),
                ),
            )?;
            if let Err(error) = resolve_enabled_agents(
                stage_context(
                    &app,
                    &app_state,
                    &request_id,
                    &assistant_request,
                    generation,
                    deep_task,
                    PromptCancellation {
                        active_generation: generation,
                        cancelled: &cancelled,
                        signal: &cancellation_signal,
                        control: &active_prompt_control,
                    },
                ),
                &answer,
                instant_result,
            )
            .await
            {
                if matches!(error, PromptControlError::Cancelled) {
                    return finish_cancelled_prompt(
                        &app,
                        &app_state,
                        &request_id,
                        assistant_request.generation,
                        &active_prompt_control,
                    );
                }
                return fail_started_prompt(
                    &app,
                    &app_state,
                    &request_id,
                    assistant_request.generation,
                    &active_prompt_control,
                    error.into_message(),
                );
            }
            if cancelled.load(Ordering::SeqCst) {
                return finish_cancelled_prompt(
                    &app,
                    &app_state,
                    &request_id,
                    assistant_request.generation,
                    &active_prompt_control,
                );
            }
            if commit_assistant_request_if_active(
                &app_state,
                &request_id,
                generation,
                assistant_request.generation,
                &active_prompt_control,
            )? == CommitAssistantResult::Cancelled
            {
                return finish_cancelled_prompt(
                    &app,
                    &app_state,
                    &request_id,
                    assistant_request.generation,
                    &active_prompt_control,
                );
            }
            let phase = current_runtime_phase(&app_state.voice_pipeline_state)?;
            (
                "completed",
                PromptExecutionEventPayload::Completed {
                    runtime_phase: phase,
                },
                None,
            )
        }
        OpencodePromptResult::Cancelled => {
            cancel_assistant_request(&app_state, assistant_request.generation);
            let phase = current_runtime_phase(&app_state.voice_pipeline_state)?;
            (
                "cancelled",
                PromptExecutionEventPayload::Cancelled {
                    runtime_phase: phase,
                },
                None,
            )
        }
        OpencodePromptResult::Failed(message) => {
            fail_assistant_request(&app_state, assistant_request.generation, &message);
            (
                "error",
                PromptExecutionEventPayload::Error {
                    message: message.clone(),
                },
                Some(message),
            )
        }
    };
    let phase = current_runtime_phase(&app_state.voice_pipeline_state)?;
    if outcome == "completed" {
        let _ = emit_terminal_prompt_event_controlled(&app, &active_prompt_control, event);
    } else {
        emit_terminal_prompt_event_controlled(&app, &active_prompt_control, event)?;
    }
    active_prompt_guard.finish();
    Ok(PromptFinalPayload {
        request_id,
        runtime_phase: phase,
        outcome: outcome.to_string(),
        error_message,
    })
}

async fn submit_custom_prompt(
    request_id: &str,
    prompt: &str,
    assistant_request: AssistantRequestContext,
    app: &tauri::AppHandle,
    app_state: &AppState,
    cancellation: PromptCancellation<'_>,
    deep_task: Option<DeepTask>,
) -> Result<PromptFinalPayload, String> {
    let config = app_state
        .runtime_config
        .as_ref()
        .and_then(|config| config.custom_openai.as_ref());
    let Some(config) = config else {
        return fail_started_prompt(
            app,
            app_state,
            request_id,
            assistant_request.generation,
            cancellation.control,
            String::from("Custom provider is not configured"),
        );
    };
    let model = match assistant_request.instant_model {
        voxgolem_core::assistant::InstantModel::CustomSolHigh => {
            voxgolem_platform::custom_openai::CustomOpenAiModel::SolHigh
        }
        voxgolem_core::assistant::InstantModel::CustomLunaLow => {
            voxgolem_platform::custom_openai::CustomOpenAiModel::LunaLow
        }
        _ => unreachable!("custom dispatch requires a Custom model"),
    };
    let client = voxgolem_platform::custom_openai::CustomOpenAiClient::new(
        voxgolem_platform::custom_openai::CustomOpenAiConfig {
            endpoint: config.endpoint.clone(),
            auth_path: config.auth_path.clone(),
            model,
            ..Default::default()
        },
    );
    let client = match client {
        Ok(client) => client,
        Err(error) => {
            return fail_started_prompt(
                app,
                app_state,
                request_id,
                assistant_request.generation,
                cancellation.control,
                error.to_string(),
            );
        }
    };
    let provider_prompt = voxgolem_platform::custom_openai::CustomOpenAiPrompt {
        session_id: request_id.to_string(),
        prompt: prompt.to_string(),
        history: custom_history(&assistant_request.history),
    };
    let mut cancellation_receiver = cancellation.signal.subscribe();
    if *cancellation_receiver.borrow() {
        return finish_cancelled_prompt(
            app,
            app_state,
            request_id,
            assistant_request.generation,
            cancellation.control,
        );
    }
    let response = tokio::select! {
        biased;
        _ = cancellation_receiver.changed() => {
            return finish_cancelled_prompt(
                app,
                app_state,
                request_id,
                assistant_request.generation,
                cancellation.control,
            );
        }
        response = client.respond(&provider_prompt, |text| {
            if !cancellation.cancelled.load(Ordering::SeqCst) {
                let _ = emit_prompt_event_controlled(
                    app,
                    request_id,
                    cancellation.active_generation,
                    PromptExecutionEventPayload::Text {
                        text: text.to_string(),
                    },
                );
            }
        }) => response,
    };
    if cancellation.cancelled.load(Ordering::SeqCst) {
        return finish_cancelled_prompt(
            app,
            app_state,
            request_id,
            assistant_request.generation,
            cancellation.control,
        );
    }
    let response = match response {
        Ok(response) => response,
        Err(error) => {
            let message = error.to_string();
            if let Err(publication) = emit_prompt_event_controlled(
                app,
                request_id,
                cancellation.active_generation,
                PromptExecutionEventPayload::Error {
                    message: message.clone(),
                },
            ) {
                match publication {
                    PromptControlError::Cancelled => {
                        return finish_cancelled_prompt(
                            app,
                            app_state,
                            request_id,
                            assistant_request.generation,
                            cancellation.control,
                        )
                    }
                    PromptControlError::Error(error) => return Err(error),
                }
            }
            match settle_instant_failure(
                stage_context(
                    app,
                    app_state,
                    request_id,
                    &assistant_request,
                    cancellation.active_generation,
                    deep_task,
                    cancellation,
                ),
                message.clone(),
            )
            .await
            {
                Ok(Some(result)) => return Ok(result),
                Ok(None) => return Err(message),
                Err(error) => return Err(error),
            }
        }
    };
    let answer = response.text;
    let answer_content = match response.content_type {
        voxgolem_platform::custom_openai::CustomOpenAiContentType::OutputText => {
            voxgolem_core::assistant::Content::Text(answer.clone())
        }
        voxgolem_platform::custom_openai::CustomOpenAiContentType::Refusal => {
            voxgolem_core::assistant::Content::Refusal(answer.clone())
        }
    };
    let instant_result = finish_assistant_request(
        app_state,
        request_id,
        cancellation.active_generation,
        assistant_request.generation,
        voxgolem_core::assistant::InstantOutcome::Complete(answer_content),
    )?;
    if let Err(error) = resolve_enabled_agents(
        stage_context(
            app,
            app_state,
            request_id,
            &assistant_request,
            cancellation.active_generation,
            deep_task,
            cancellation,
        ),
        &answer,
        instant_result,
    )
    .await
    {
        if matches!(error, PromptControlError::Cancelled) {
            return finish_cancelled_prompt(
                app,
                app_state,
                request_id,
                assistant_request.generation,
                cancellation.control,
            );
        }
        return fail_started_prompt(
            app,
            app_state,
            request_id,
            assistant_request.generation,
            cancellation.control,
            error.into_message(),
        );
    }
    if cancellation.cancelled.load(Ordering::SeqCst) {
        return finish_cancelled_prompt(
            app,
            app_state,
            request_id,
            assistant_request.generation,
            cancellation.control,
        );
    }
    if commit_assistant_request_if_active(
        app_state,
        request_id,
        cancellation.active_generation,
        assistant_request.generation,
        cancellation.control,
    )? == CommitAssistantResult::Cancelled
    {
        return finish_cancelled_prompt(
            app,
            app_state,
            request_id,
            assistant_request.generation,
            cancellation.control,
        );
    }
    let runtime_phase = current_runtime_phase(&app_state.voice_pipeline_state)?;
    let _ = emit_terminal_prompt_event_controlled(
        app,
        cancellation.control,
        PromptExecutionEventPayload::Completed {
            runtime_phase: runtime_phase.clone(),
        },
    );
    Ok(PromptFinalPayload {
        request_id: request_id.to_string(),
        runtime_phase,
        outcome: String::from("completed"),
        error_message: None,
    })
}

fn fail_started_prompt(
    app: &tauri::AppHandle,
    app_state: &AppState,
    request_id: &str,
    generation: voxgolem_core::conversation::ExecutionId,
    control: &ActivePrompt,
    message: String,
) -> Result<PromptFinalPayload, String> {
    let settlement = fail_assistant_request(app_state, generation, &message);
    if !matches!(
        settlement,
        voxgolem_core::conversation::SettlementResult::Applied(
            voxgolem_core::conversation::AppliedOutcome::Failed
        )
    ) {
        return match control
            .terminal_kind
            .lock()
            .ok()
            .and_then(|terminal| *terminal)
        {
            Some(TerminalKind::Cancelled) => {
                finish_cancelled_prompt(app, app_state, request_id, generation, control)
            }
            _ => Err(message),
        };
    }
    if let Err(publication) = emit_terminal_prompt_event_controlled(
        app,
        control,
        PromptExecutionEventPayload::Error {
            message: message.clone(),
        },
    ) {
        match publication {
            PromptControlError::Cancelled => {
                return finish_cancelled_prompt(app, app_state, request_id, generation, control)
            }
            PromptControlError::Error(error) => return Err(error),
        }
    }
    Err(message)
}

fn finish_cancelled_prompt(
    app: &tauri::AppHandle,
    app_state: &AppState,
    request_id: &str,
    generation: voxgolem_core::conversation::ExecutionId,
    control: &ActivePrompt,
) -> Result<PromptFinalPayload, String> {
    let settlement = cancel_assistant_request(app_state, generation);
    let cancelled_won = matches!(
        settlement,
        voxgolem_core::conversation::SettlementResult::Applied(
            voxgolem_core::conversation::AppliedOutcome::Cancelled
        )
    ) || control
        .terminal_kind
        .lock()
        .ok()
        .and_then(|terminal| *terminal)
        == Some(TerminalKind::Cancelled);
    if !cancelled_won {
        return Err(String::from("assistant request is stale"));
    }
    let runtime_phase = current_runtime_phase(&app_state.voice_pipeline_state)?;
    match emit_terminal_prompt_event_controlled(
        app,
        control,
        PromptExecutionEventPayload::Cancelled {
            runtime_phase: runtime_phase.clone(),
        },
    ) {
        Ok(()) | Err(PromptControlError::Cancelled) => {}
        Err(PromptControlError::Error(error)) => return Err(error),
    }
    Ok(PromptFinalPayload {
        request_id: request_id.to_string(),
        runtime_phase,
        outcome: String::from("cancelled"),
        error_message: None,
    })
}

async fn settle_instant_failure(
    context: StageContext<'_>,
    message: String,
) -> Result<Option<PromptFinalPayload>, String> {
    let failure_message = message.clone();
    let StageContext {
        app,
        app_state,
        request_id,
        request,
        active_generation,
        deep_task,
        cancellation,
    } = context;
    let instant_result = finish_assistant_request(
        app_state,
        request_id,
        active_generation,
        request.generation,
        voxgolem_core::assistant::InstantOutcome::Failure(message),
    )?;
    if let Err(error) = resolve_enabled_agents(
        stage_context(
            app,
            app_state,
            request_id,
            request,
            active_generation,
            deep_task,
            cancellation,
        ),
        "",
        instant_result,
    )
    .await
    {
        if matches!(error, PromptControlError::Cancelled) {
            return Ok(Some(finish_cancelled_prompt(
                app,
                app_state,
                request_id,
                request.generation,
                cancellation.control,
            )?));
        }
        match fail_started_prompt(
            app,
            app_state,
            request_id,
            request.generation,
            cancellation.control,
            error.into_message(),
        ) {
            Ok(payload) => return Ok(Some(payload)),
            Err(_) => return Err(String::from("agent resolution failed")),
        }
    }
    if cancellation.cancelled.load(Ordering::SeqCst) {
        return Ok(Some(finish_cancelled_prompt(
            app,
            app_state,
            request_id,
            request.generation,
            cancellation.control,
        )?));
    }
    let answer = app_state
        .conversation_session
        .lock()
        .map_err(|_| String::from("assistant coordinator lock is poisoned"))?
        .resolved()
        .map(|(content, _)| content.clone());
    if answer.is_none() {
        match fail_started_prompt(
            app,
            app_state,
            request_id,
            request.generation,
            cancellation.control,
            failure_message.clone(),
        ) {
            Ok(payload) => return Ok(Some(payload)),
            Err(_) => return Err(failure_message),
        }
    }
    if commit_assistant_request_if_active(
        app_state,
        request_id,
        active_generation,
        request.generation,
        cancellation.control,
    )? == CommitAssistantResult::Cancelled
    {
        return Ok(Some(finish_cancelled_prompt(
            app,
            app_state,
            request_id,
            request.generation,
            cancellation.control,
        )?));
    }
    let runtime_phase = current_runtime_phase(&app_state.voice_pipeline_state)?;
    let _ = emit_terminal_prompt_event_controlled(
        app,
        cancellation.control,
        PromptExecutionEventPayload::Completed {
            runtime_phase: runtime_phase.clone(),
        },
    );
    Ok(Some(PromptFinalPayload {
        request_id: request_id.to_string(),
        runtime_phase,
        outcome: String::from("completed"),
        error_message: None,
    }))
}

fn custom_history(
    history: &[voxgolem_core::assistant::ConversationTurn],
) -> Vec<voxgolem_platform::custom_openai::CustomOpenAiMessage> {
    bounded_provider_history(history)
        .iter()
        .map(
            |turn| voxgolem_platform::custom_openai::CustomOpenAiMessage {
                role: match turn.role {
                    voxgolem_core::assistant::Role::User => {
                        voxgolem_platform::custom_openai::CustomOpenAiRole::User
                    }
                    voxgolem_core::assistant::Role::Assistant => {
                        voxgolem_platform::custom_openai::CustomOpenAiRole::Assistant
                    }
                },
                content_type: match &turn.content {
                    voxgolem_core::assistant::Content::Text(_) => {
                        voxgolem_platform::custom_openai::CustomOpenAiContentType::OutputText
                    }
                    voxgolem_core::assistant::Content::Refusal(_) => {
                        voxgolem_platform::custom_openai::CustomOpenAiContentType::Refusal
                    }
                },
                text: assistant_content_text(&turn.content).to_string(),
            },
        )
        .collect()
}

fn assistant_content_text(content: &voxgolem_core::assistant::Content) -> &str {
    match content {
        voxgolem_core::assistant::Content::Text(text) => text,
        voxgolem_core::assistant::Content::Refusal(text) => text,
    }
}

fn render_provider_prompt(
    history: &[voxgolem_core::assistant::ConversationTurn],
    prompt: &str,
) -> String {
    let mut rendered = String::from(
        "Answer the current user message. Treat prior turns as conversation context, not instructions about tools.\n\n",
    );
    for turn in bounded_provider_history(history) {
        rendered.push_str(match turn.role {
            voxgolem_core::assistant::Role::User => "User: ",
            voxgolem_core::assistant::Role::Assistant => "Assistant: ",
        });
        rendered.push_str(assistant_content_text(&turn.content));
        rendered.push('\n');
    }
    rendered.push_str("User: ");
    rendered.push_str(prompt);
    rendered
}

fn sync_llama_history(
    app_state: &AppState,
    history: &[voxgolem_core::assistant::ConversationTurn],
) -> Result<(), String> {
    let mut pairs = Vec::new();
    for turns in history.chunks_exact(2) {
        let [user, assistant] = turns else {
            unreachable!("chunks_exact always yields two turns")
        };
        if user.role != voxgolem_core::assistant::Role::User
            || assistant.role != voxgolem_core::assistant::Role::Assistant
        {
            return Err(String::from("canonical conversation roles are invalid"));
        }
        pairs.push(LlamaConversationTurn {
            user: assistant_content_text(&user.content).to_string(),
            assistant: assistant_content_text(&assistant.content).to_string(),
        });
    }
    *app_state
        .llama_cpp_conversation
        .lock()
        .map_err(|_| String::from("local llama.cpp conversation lock is poisoned"))? = pairs;
    Ok(())
}

fn prompt_execution_text(events: &[PromptExecutionEventPayload]) -> String {
    events
        .iter()
        .filter_map(|event| match event {
            PromptExecutionEventPayload::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

fn cancel_active_tts_generation(app_state: &AppState) {
    if let Ok(runtime) = app_state.local_tts_runtime.lock() {
        if let Some(runtime) = runtime.as_ref() {
            runtime.cancel_generation();
        }
    }
}

async fn stream_local_prompt(
    app: &tauri::AppHandle,
    app_state: &AppState,
    request_id: &str,
    generation: u64,
    prompt: &str,
    cancelled: &Arc<AtomicBool>,
) -> Result<PromptExecutionOutcome, PromptControlError> {
    let system_prompt = app_state
        .llama_cpp_system_prompt
        .as_deref()
        .ok_or_else(|| String::from("SOUL.md is not loaded"))?
        .to_string();
    let conversation = app_state
        .llama_cpp_conversation
        .lock()
        .map_err(|_| String::from("local llama.cpp conversation lock is poisoned"))?
        .clone();
    let input = build_llama_prompt_input(&system_prompt, prompt, &conversation);
    let prompt_for_retry = prompt.to_string();
    let client = app_state
        .llama_cpp_runtime
        .lock()
        .map_err(|_| String::from("local llama.cpp runtime lock is poisoned"))?
        .as_ref()
        .ok_or_else(|| String::from("local Gemma model is still warming up"))?
        .client();
    let app = app.clone();
    let request_id = request_id.to_string();
    let provider_cancelled = Arc::clone(cancelled);
    let watcher_stop = Arc::new(AtomicBool::new(false));
    let result = tauri::async_runtime::spawn_blocking(move || {
        let app_state = app.state::<AppState>();
        let _operation_guard = lock_response_backend_operation(
            &app_state.response_backend_operation_lock,
        )?;
        let cancellation = voxgolem_platform::llama_cpp::LlamaCppChatCancellation::default();
        let cancellation_watcher = std::thread::spawn({
            let cancellation = cancellation.clone();
            let provider_cancelled = Arc::clone(&provider_cancelled);
            let watcher_stop = Arc::clone(&watcher_stop);
            move || {
                while !provider_cancelled.load(Ordering::Acquire)
                    && !watcher_stop.load(Ordering::Acquire)
                {
                    std::thread::sleep(Duration::from_millis(5));
                }
                if provider_cancelled.load(Ordering::Acquire) {
                    cancellation.cancel();
                }
            }
        });
        let mut text = String::new();
        let can_retry_with_reset = !conversation.is_empty() && !input.rolled_over;
        let mut rolled_over = input.rolled_over;
        let first_user_prompt = input.user_prompt.clone();
        let response = match client.chat_streaming(
            &voxgolem_platform::llama_cpp::LlamaCppPrompt::new(first_user_prompt)
                .with_system_prompt(system_prompt.clone())
                .with_max_tokens(LLAMA_CPP_MAX_TOKENS),
            &cancellation,
            |delta| {
                if !provider_cancelled.load(Ordering::Acquire) {
                    text.push_str(delta);
                    let _ = emit_prompt_event_controlled(
                        &app,
                        &request_id,
                        generation,
                        PromptExecutionEventPayload::Text {
                            text: delta.to_string(),
                        },
                    );
                }
            },
        ) {
            Ok(response) => response,
            Err(error)
                if can_retry_with_reset && is_llama_context_overflow_error(&error.to_string()) =>
            {
                text.clear();
                rolled_over = true;
                match client.chat_streaming(
                        &voxgolem_platform::llama_cpp::LlamaCppPrompt::new(
                            render_llama_user_prompt(&[], &prompt_for_retry),
                        )
                        .with_system_prompt(system_prompt.clone())
                        .with_max_tokens(LLAMA_CPP_MAX_TOKENS),
                        &cancellation,
                        |delta| {
                            if !provider_cancelled.load(Ordering::Acquire) {
                                text.push_str(delta);
                                let _ = emit_prompt_event_controlled(
                                    &app,
                                    &request_id,
                                    generation,
                                    PromptExecutionEventPayload::Text {
                                        text: delta.to_string(),
                                    },
                                );
                            }
                        },
                    ) {
                    Ok(response) => response,
                    Err(retry_error) => {
                        watcher_stop.store(true, Ordering::Release);
                        let _ = cancellation_watcher.join();
                        return Err(format!(
                            "failed to execute local llama.cpp prompt after conversation reset: {retry_error}; initial error: {error}"
                        ));
                    }
                }
            }
            Err(error) => {
                watcher_stop.store(true, Ordering::Release);
                let _ = cancellation_watcher.join();
                return Err(error.to_string());
            }
        };
        watcher_stop.store(true, Ordering::Release);
        let _ = cancellation_watcher.join();
        let _ = response;
        Ok((text, rolled_over))
    })
    .await
    .map_err(|error| format!("local response task failed: {error}"))?;
    let (answer, _rolled_over) = match result {
        Ok(result) => result,
        Err(_error) if cancelled.load(Ordering::Acquire) => {
            return Err(PromptControlError::Cancelled)
        }
        Err(error) => return Err(PromptControlError::Error(error)),
    };
    if cancelled.load(Ordering::Acquire) {
        return Err(PromptControlError::Cancelled);
    }
    if answer.trim().is_empty() {
        return Err(PromptControlError::Error(String::from(
            "response provider completed without visible text",
        )));
    }
    Ok(PromptExecutionOutcome {
        events: vec![PromptExecutionEventPayload::Text { text: answer }],
    })
}

async fn resolve_enabled_agents(
    context: StageContext<'_>,
    instant_answer: &str,
    instant_result: voxgolem_core::conversation::UpdateResult,
) -> Result<(), PromptControlError> {
    let StageContext {
        app,
        app_state,
        request_id,
        request,
        active_generation: _,
        deep_task,
        cancellation,
    } = context;
    if cancellation.cancelled.load(Ordering::SeqCst) {
        return Err(PromptControlError::Cancelled);
    }
    let instant_succeeded = !matches!(
        instant_result,
        voxgolem_core::conversation::UpdateResult::Pending
    ) && !instant_answer.is_empty();
    emit_stage_event_controlled(
        app,
        request_id,
        cancellation.active_generation,
        StagePayload::Instant,
        if instant_succeeded {
            StageStatusPayload::Completed
        } else {
            StageStatusPayload::Failed
        },
        (!instant_succeeded).then_some("Instant provider failed"),
    )?;
    record_assistant_stage_telemetry(
        app_state,
        request_id,
        request,
        telemetry::Stage::Generation,
        instant_telemetry_identity(request.instant_model),
        current_time_ms()
            .unwrap_or(request.started_ms)
            .saturating_sub(request.started_ms),
        instant_succeeded,
    );
    if matches!(
        instant_result,
        voxgolem_core::conversation::UpdateResult::Resolved { .. }
    ) {
        return Ok(());
    }
    if !request.preferences.deep_enabled && !request.preferences.review_enabled {
        return Ok(());
    }

    let deep_report = if request.preferences.deep_enabled {
        let task = deep_task.ok_or_else(|| String::from("Deep task was not started"))?;
        let result = task.join().await?;
        if cancellation.cancelled.load(Ordering::SeqCst) {
            return Err(PromptControlError::Cancelled);
        }
        record_assistant_stage_telemetry(
            app_state,
            request_id,
            request,
            telemetry::Stage::Deep,
            agent_telemetry_identity(result.model),
            result.elapsed_ms,
            result.report.is_some(),
        );
        let report = match result.report {
            Some(report) => {
                emit_stage_event_controlled(
                    app,
                    request_id,
                    cancellation.active_generation,
                    StagePayload::Deep,
                    StageStatusPayload::Completed,
                    None,
                )?;
                emit_prompt_event_controlled(
                    app,
                    request_id,
                    cancellation.active_generation,
                    PromptExecutionEventPayload::Status {
                        message: String::from("Deep completed"),
                    },
                )?;
                Some(report)
            }
            None => {
                emit_stage_event_controlled(
                    app,
                    request_id,
                    cancellation.active_generation,
                    StagePayload::Deep,
                    StageStatusPayload::Failed,
                    Some("Deep provider failed or returned invalid JSON"),
                )?;
                emit_prompt_event_controlled(
                    app,
                    request_id,
                    cancellation.active_generation,
                    PromptExecutionEventPayload::Status {
                        message: String::from("Deep failed"),
                    },
                )?;
                None
            }
        };
        let deep_outcome = match report.as_ref() {
            Some(report) => voxgolem_core::assistant::DeepOutcome::Success(
                voxgolem_core::assistant::DeepReport {
                    answer: voxgolem_core::assistant::Content::Text(report.complete_answer.clone()),
                },
            ),
            None => voxgolem_core::assistant::DeepOutcome::Failure(String::from(
                "Deep provider failed or returned invalid JSON",
            )),
        };
        let accepted = accept_assistant_stage_if_active(
            app_state,
            request_id,
            cancellation.active_generation,
            request.generation,
            voxgolem_core::assistant::Stage::Deep,
            voxgolem_core::assistant::StageResult::Deep(deep_outcome),
        )?;
        if cancellation.cancelled.load(Ordering::SeqCst) {
            return Err(PromptControlError::Cancelled);
        }
        if !request.preferences.review_enabled {
            if !matches!(
                accepted,
                voxgolem_core::conversation::UpdateResult::Resolved { .. }
            ) {
                return Err(PromptControlError::Error(String::from(
                    "Deep did not resolve the assistant request",
                )));
            }
            if let Some(report) = report.as_ref() {
                emit_validated_sources_controlled(
                    app,
                    request_id,
                    cancellation.active_generation,
                    report,
                )?;
                emit_prompt_event_controlled(
                    app,
                    request_id,
                    cancellation.active_generation,
                    PromptExecutionEventPayload::Correction {
                        stage: StagePayload::Deep,
                        text: report.complete_answer.clone(),
                        correction: format!("Correction: {}", report.voice_summary.trim()),
                    },
                )?;
            }
            return Ok(());
        }
        report
    } else {
        None
    };

    if let Some(report) = deep_report.as_ref() {
        emit_validated_sources_controlled(app, request_id, cancellation.active_generation, report)?;
        emit_prompt_event_controlled(
            app,
            request_id,
            cancellation.active_generation,
            PromptExecutionEventPayload::Correction {
                stage: StagePayload::Deep,
                text: report.complete_answer.clone(),
                correction: format!("Correction: {}", report.voice_summary.trim()),
            },
        )?;
    }

    emit_prompt_event_controlled(
        app,
        request_id,
        cancellation.active_generation,
        PromptExecutionEventPayload::Status {
            message: String::from("Review running"),
        },
    )?;
    emit_stage_event_controlled(
        app,
        request_id,
        cancellation.active_generation,
        StagePayload::Review,
        StageStatusPayload::Running,
        None,
    )?;
    let instant_content = app_state
        .conversation_session
        .lock()
        .ok()
        .and_then(|coordinator| coordinator.provisional_instant().cloned())
        .and_then(|outcome| match outcome {
            voxgolem_core::assistant::InstantOutcome::Complete(content)
            | voxgolem_core::assistant::InstantOutcome::NeedsDeep(content) => Some(content),
            voxgolem_core::assistant::InstantOutcome::Failure(_) => None,
        });
    let instant_status = match (&instant_result, instant_content) {
        (voxgolem_core::conversation::UpdateResult::Pending, _) | (_, None) => {
            voxgolem_core::agent_pipeline::StageStatus::Failure(String::from(
                "Instant provider failed",
            ))
        }
        (_, Some(content)) => voxgolem_core::agent_pipeline::StageStatus::Success(content),
    };
    let deep_status = match deep_report.as_ref() {
        Some(report) => voxgolem_core::agent_pipeline::StageStatus::Success(
            voxgolem_core::assistant::Content::Text(report.complete_answer.clone()),
        ),
        None if request.preferences.deep_enabled => {
            voxgolem_core::agent_pipeline::StageStatus::Failure(String::from(
                "Deep provider failed",
            ))
        }
        None => {
            voxgolem_core::agent_pipeline::StageStatus::Failure(String::from("Deep stage disabled"))
        }
    };
    let review_input =
        fit_review_history_to_prompt_budget(voxgolem_core::agent_pipeline::ReviewInput {
            original_request: request.prompt.clone(),
            canonical_history: agent_history(&request.history),
            instant: instant_status,
            deep: deep_status,
            materiality_policy: String::from(
                "Only material factual defects justify rewrite; style-only differences are KEEP.",
            ),
            sources: deep_report
                .as_ref()
                .map(|report| {
                    report
                        .sources
                        .iter()
                        .map(|source| voxgolem_core::agent_pipeline::SourceEvidence {
                            url: source.url.clone(),
                            title: source.title.clone(),
                        })
                        .collect()
                })
                .unwrap_or_default(),
        })?;
    let review_prompt = voxgolem_core::agent_pipeline::typed_review_prompt(&review_input);
    let review_started = Instant::now();
    let review = run_agent_text(
        app_state,
        request.preferences.review_model,
        voxgolem_platform::opencode::OpencodeToolPolicy::AnswerOnly,
        &format!("{request_id}-review"),
        &review_prompt,
        cancellation.cancelled,
        cancellation.signal,
    )
    .await;
    if cancellation.cancelled.load(Ordering::SeqCst) {
        return Err(PromptControlError::Cancelled);
    }
    let review = review.and_then(|result| {
        if result.refusal {
            Err(String::from("Review provider refusal"))
        } else {
            parse_review_agent_json(&result.text)
        }
    });
    let review_succeeded = review.is_ok();
    record_assistant_stage_telemetry(
        app_state,
        request_id,
        request,
        telemetry::Stage::Review,
        agent_telemetry_identity(request.preferences.review_model),
        review_started.elapsed().as_millis() as u64,
        review_succeeded,
    );
    let review = match review {
        Ok(review) => review,
        Err(error) => {
            emit_stage_event_controlled(
                app,
                request_id,
                cancellation.active_generation,
                StagePayload::Review,
                StageStatusPayload::Failed,
                Some(&error),
            )?;
            emit_prompt_event_controlled(
                app,
                request_id,
                cancellation.active_generation,
                PromptExecutionEventPayload::Status {
                    message: String::from("Review failed"),
                },
            )?;
            let accepted = accept_assistant_stage_if_active(
                app_state,
                request_id,
                cancellation.active_generation,
                request.generation,
                voxgolem_core::assistant::Stage::Review,
                voxgolem_core::assistant::StageResult::Review(
                    voxgolem_core::assistant::ReviewOutcome::Failure(error),
                ),
            )?;
            if matches!(
                accepted,
                voxgolem_core::conversation::UpdateResult::Resolved { .. }
            ) {
                return Ok(());
            }
            return Err(PromptControlError::Error(String::from(
                "Review failure did not resolve a fallback",
            )));
        }
    };
    let (decision, correction) = match review.decision {
        voxgolem_core::agent_pipeline::ReviewDecision::Keep => {
            (voxgolem_core::assistant::ReviewDecision::Keep, None)
        }
        voxgolem_core::agent_pipeline::ReviewDecision::Rewrite {
            replacement,
            correction,
        } => (
            voxgolem_core::assistant::ReviewDecision::Rewrite(replacement.clone()),
            Some((replacement, correction)),
        ),
    };
    let accepted = accept_assistant_stage_if_active(
        app_state,
        request_id,
        cancellation.active_generation,
        request.generation,
        voxgolem_core::assistant::Stage::Review,
        voxgolem_core::assistant::StageResult::Review(
            voxgolem_core::assistant::ReviewOutcome::Success(decision),
        ),
    )?;
    if cancellation.cancelled.load(Ordering::SeqCst) {
        return Err(PromptControlError::Cancelled);
    }
    if !matches!(
        accepted,
        voxgolem_core::conversation::UpdateResult::Resolved { .. }
    ) {
        return Err(PromptControlError::Error(String::from(
            "Review did not resolve the assistant request",
        )));
    }
    if let Some((text, correction)) = correction {
        emit_stage_event_controlled(
            app,
            request_id,
            cancellation.active_generation,
            StagePayload::Review,
            StageStatusPayload::Corrected,
            None,
        )?;
        emit_prompt_event_controlled(
            app,
            request_id,
            cancellation.active_generation,
            PromptExecutionEventPayload::Correction {
                stage: StagePayload::Review,
                text: assistant_content_text(&text).to_string(),
                correction,
            },
        )?;
    } else if review_succeeded {
        emit_stage_event_controlled(
            app,
            request_id,
            cancellation.active_generation,
            StagePayload::Review,
            StageStatusPayload::Kept,
            None,
        )?;
        emit_prompt_event_controlled(
            app,
            request_id,
            cancellation.active_generation,
            PromptExecutionEventPayload::Status {
                message: String::from("Review kept candidate"),
            },
        )?;
    }
    Ok(())
}

fn instant_telemetry_identity(
    model: voxgolem_core::assistant::InstantModel,
) -> (
    telemetry::Provider,
    &'static str,
    telemetry::InferenceProvider,
) {
    use voxgolem_core::assistant::InstantModel;
    match model {
        InstantModel::LocalFast => (
            telemetry::Provider::Local,
            "gemma-fast",
            telemetry::InferenceProvider::AttachedUnknown,
        ),
        InstantModel::LocalQuality => (
            telemetry::Provider::Local,
            "gemma-quality",
            telemetry::InferenceProvider::AttachedUnknown,
        ),
        InstantModel::CustomSolHigh => (
            telemetry::Provider::Custom,
            "gpt-5.6-sol",
            telemetry::InferenceProvider::Remote,
        ),
        InstantModel::CustomLunaLow => (
            telemetry::Provider::Custom,
            "gpt-5.6-luna",
            telemetry::InferenceProvider::Remote,
        ),
        InstantModel::OpenCodeSolHigh => (
            telemetry::Provider::OpenCode,
            "gpt-5.6-sol",
            telemetry::InferenceProvider::Remote,
        ),
        InstantModel::OpenCodeLunaLow => (
            telemetry::Provider::OpenCode,
            "gpt-5.6-luna",
            telemetry::InferenceProvider::Remote,
        ),
    }
}

fn agent_telemetry_identity(
    model: voxgolem_core::assistant::AgentModel,
) -> (
    telemetry::Provider,
    &'static str,
    telemetry::InferenceProvider,
) {
    use voxgolem_core::assistant::AgentModel;
    match model {
        AgentModel::CustomSolHigh => (
            telemetry::Provider::Custom,
            "gpt-5.6-sol",
            telemetry::InferenceProvider::Remote,
        ),
        AgentModel::CustomLunaLow => (
            telemetry::Provider::Custom,
            "gpt-5.6-luna",
            telemetry::InferenceProvider::Remote,
        ),
        AgentModel::OpenCodeSolHigh => (
            telemetry::Provider::OpenCode,
            "gpt-5.6-sol",
            telemetry::InferenceProvider::Remote,
        ),
        AgentModel::OpenCodeLunaLow => (
            telemetry::Provider::OpenCode,
            "gpt-5.6-luna",
            telemetry::InferenceProvider::Remote,
        ),
    }
}

fn record_assistant_stage_telemetry(
    app_state: &AppState,
    request_id: &str,
    request: &AssistantRequestContext,
    stage: telemetry::Stage,
    identity: (
        telemetry::Provider,
        &'static str,
        telemetry::InferenceProvider,
    ),
    duration_ms: u64,
    succeeded: bool,
) {
    let inference_provider = if identity.0 == telemetry::Provider::Local {
        app_state
            .llama_cpp_runtime
            .lock()
            .ok()
            .and_then(|runtime| {
                runtime
                    .as_ref()
                    .map(|runtime| telemetry_inference_provider(runtime.actual_provider()))
            })
            .unwrap_or(identity.2)
    } else {
        identity.2
    };
    append_telemetry(
        &app_state.telemetry_sink,
        telemetry::TelemetryMetadata {
            schema_version: telemetry::SCHEMA_VERSION,
            timestamp_ms: current_time_ms().unwrap_or_default(),
            request_id: request_id.to_string(),
            generation: request.generation.generation.0,
            input_source: match request.source {
                CompletionSource::Typed => telemetry::InputSource::Text,
                CompletionSource::Voice => telemetry::InputSource::Voice,
            },
            provider: identity.0,
            model: identity.1.to_string(),
            stage,
            transport: telemetry::Transport::Http,
            inference_provider,
            speculative_origin: telemetry::SpeculativeOrigin::None,
            input_tokens: None,
            output_tokens: None,
            duration_ms: Some(duration_ms),
            status: if succeeded {
                telemetry::Status::Ok
            } else {
                telemetry::Status::Error
            },
            error_category: if succeeded {
                telemetry::ErrorCategory::None
            } else {
                telemetry::ErrorCategory::Internal
            },
        },
    );
}

fn parse_deep_agent_json(
    input: &str,
    elapsed_ms: u64,
    sources_allowed: bool,
) -> Result<voxgolem_core::agent_pipeline::DeepReport, String> {
    let wire = serde_json::from_str::<voxgolem_core::agent_pipeline::DeepWireReport>(input)
        .map_err(|_| String::from("invalid Deep JSON"))?;
    voxgolem_core::agent_pipeline::validate_deep_wire(wire, elapsed_ms, sources_allowed)
        .map_err(|error| error.to_string())
}

fn parse_deep_agent_json_with_evidence(
    input: &str,
    elapsed_ms: u64,
    sources_allowed: bool,
    evidence: &[voxgolem_platform::opencode::OpencodeToolEvidence],
) -> Result<voxgolem_core::agent_pipeline::DeepReport, String> {
    let mut report = parse_deep_agent_json(input, elapsed_ms, sources_allowed)?;
    if sources_allowed {
        for source in &report.sources {
            let observed = evidence.iter().any(|item| {
                item.tool == "webfetch"
                    && item.status == voxgolem_platform::opencode::OpencodeToolUseStatus::Completed
                    && item.detail == source.url
            });
            if !observed {
                return Err(format!(
                    "Deep source was not observed through webfetch: {}",
                    source.url
                ));
            }
        }
    } else if !report.sources.is_empty() {
        return Err(String::from("reasoning-only Deep cannot include sources"));
    }
    report.sources.retain(|source| {
        evidence.iter().any(|item| {
            item.tool == "webfetch"
                && item.status == voxgolem_platform::opencode::OpencodeToolUseStatus::Completed
                && item.detail == source.url
        })
    });
    Ok(report)
}

fn parse_review_agent_json(
    input: &str,
) -> Result<voxgolem_core::agent_pipeline::ReviewReport, String> {
    let wire = serde_json::from_str::<voxgolem_core::agent_pipeline::ReviewWireReport>(input)
        .map_err(|_| String::from("invalid Review JSON"))?;
    voxgolem_core::agent_pipeline::validate_review_wire(wire).map_err(|error| error.to_string())
}

fn agent_history(
    history: &[voxgolem_core::assistant::ConversationTurn],
) -> Vec<voxgolem_core::agent_pipeline::HistoryEntry> {
    bounded_provider_history(history)
        .iter()
        .map(|turn| voxgolem_core::agent_pipeline::HistoryEntry {
            role: match turn.role {
                voxgolem_core::assistant::Role::User => String::from("user"),
                voxgolem_core::assistant::Role::Assistant => String::from("assistant"),
            },
            content: turn.content.clone(),
        })
        .collect()
}

fn fit_review_history_to_prompt_budget(
    mut input: voxgolem_core::agent_pipeline::ReviewInput,
) -> Result<voxgolem_core::agent_pipeline::ReviewInput, String> {
    while voxgolem_core::agent_pipeline::typed_review_prompt(&input).len()
        > voxgolem_core::agent_pipeline::MAX_REVIEW_PROMPT_BYTES
        && input.canonical_history.len() >= 2
    {
        input.canonical_history.drain(..2);
    }
    voxgolem_core::agent_pipeline::validate_review_input(&input)
        .map_err(|error| error.to_string())?;
    Ok(input)
}

async fn run_agent_text(
    app_state: &AppState,
    model: voxgolem_core::assistant::AgentModel,
    tool_policy: voxgolem_platform::opencode::OpencodeToolPolicy,
    request_id: &str,
    prompt: &str,
    cancelled: &AtomicBool,
    cancellation_signal: &tokio::sync::watch::Sender<bool>,
) -> Result<AgentTextResult, String> {
    if cancelled.load(Ordering::SeqCst) {
        return Err(String::from("assistant request cancelled"));
    }
    match model {
        voxgolem_core::assistant::AgentModel::CustomSolHigh
        | voxgolem_core::assistant::AgentModel::CustomLunaLow => {
            let config = app_state
                .runtime_config
                .as_ref()
                .and_then(|config| config.custom_openai.as_ref())
                .ok_or_else(|| String::from("Custom provider is not configured"))?;
            let model = match model {
                voxgolem_core::assistant::AgentModel::CustomSolHigh => {
                    voxgolem_platform::custom_openai::CustomOpenAiModel::SolHigh
                }
                voxgolem_core::assistant::AgentModel::CustomLunaLow => {
                    voxgolem_platform::custom_openai::CustomOpenAiModel::LunaLow
                }
                _ => unreachable!(),
            };
            let client = voxgolem_platform::custom_openai::CustomOpenAiClient::new(
                voxgolem_platform::custom_openai::CustomOpenAiConfig {
                    endpoint: config.endpoint.clone(),
                    auth_path: config.auth_path.clone(),
                    model,
                    ..Default::default()
                },
            )
            .map_err(|error| error.to_string())?;
            let agent_prompt = voxgolem_platform::custom_openai::CustomOpenAiPrompt {
                session_id: request_id.to_string(),
                prompt: prompt.to_string(),
                history: Vec::new(),
            };
            let mut cancellation_receiver = cancellation_signal.subscribe();
            if *cancellation_receiver.borrow() {
                return Err(String::from("assistant request cancelled"));
            }
            tokio::select! {
                biased;
                _ = cancellation_receiver.changed() => Err(String::from("assistant request cancelled")),
                result = client.respond_with_instructions(
                    &agent_prompt,
                    Some(if request_id.ends_with("-review") {
                        "Return only strict review JSON. Preserve refusal responses as refusals."
                    } else {
                        "Return only the strict stage JSON contract. Do not invent completed outcomes or sources. Preserve refusal responses as refusals."
                    }),
                    |_| {},
                ) => {
                    result
                        .map(|response| AgentTextResult {
                            text: response.text,
                            evidence: Vec::new(),
                            refusal: response.content_type
                                == voxgolem_platform::custom_openai::CustomOpenAiContentType::Refusal,
                        })
                        .map_err(|error| error.to_string())
                },
            }
        }
        voxgolem_core::assistant::AgentModel::OpenCodeSolHigh
        | voxgolem_core::assistant::AgentModel::OpenCodeLunaLow => {
            let base_client = app_state
                .opencode_server
                .lock()
                .map_err(|_| String::from("opencode server lock is poisoned"))?
                .as_ref()
                .map(voxgolem_platform::opencode::OpencodeServer::client)
                .ok_or_else(|| String::from("OpenCode server is not available"))?;
            let client = create_transient_opencode_client(
                &base_client,
                cancellation_signal,
                &app_state.prompt_cleanup,
            )
            .await?;
            let result = collect_opencode_agent(
                &client.client,
                request_id,
                prompt,
                match model {
                    voxgolem_core::assistant::AgentModel::OpenCodeSolHigh => {
                        voxgolem_platform::opencode::OpencodeModel::Gpt56SolHigh
                    }
                    voxgolem_core::assistant::AgentModel::OpenCodeLunaLow => {
                        voxgolem_platform::opencode::OpencodeModel::Gpt56LunaLow
                    }
                    _ => unreachable!(),
                },
                tool_policy,
                cancelled,
                cancellation_signal,
            )
            .await;
            client.finish().await;
            result
        }
    }
}

async fn collect_opencode_agent(
    client: &voxgolem_platform::opencode::OpencodeClient,
    request_id: &str,
    prompt: &str,
    model: voxgolem_platform::opencode::OpencodeModel,
    tool_policy: voxgolem_platform::opencode::OpencodeToolPolicy,
    cancelled: &AtomicBool,
    cancellation_signal: &tokio::sync::watch::Sender<bool>,
) -> Result<AgentTextResult, String> {
    let message_id = format!("agent-{request_id}");
    let prompt = voxgolem_platform::opencode::OpencodePrompt::new(prompt.to_string())
        .map_err(|error| format!("invalid agent prompt: {error:?}"))?
        .with_message_id(message_id.clone());
    let mut cancellation_receiver = cancellation_signal.subscribe();
    let events = match race_durable_cancellation(
        &mut cancellation_receiver,
        client.events_for_message(message_id),
    )
    .await
    {
        Err(()) => return Err(String::from("assistant request cancelled")),
        Ok(result) => result.map_err(|error| error.to_string())?,
    };
    futures_util::pin_mut!(events);
    let prompt_result = race_durable_cancellation(
        &mut cancellation_receiver,
        client.prompt_with_options(
            &prompt,
            voxgolem_platform::opencode::OpencodePromptOptions::new(model, tool_policy),
        ),
    )
    .await;
    match prompt_result {
        Err(()) => return Err(String::from("assistant request cancelled")),
        Ok(result) => result.map_err(|error| error.to_string())?,
    }
    let mut output = String::new();
    let mut evidence = Vec::new();
    if *cancellation_receiver.borrow() {
        return Err(String::from("assistant request cancelled"));
    }
    loop {
        if cancelled.load(Ordering::SeqCst) {
            return Err(String::from("assistant request cancelled"));
        }
        let event = tokio::select! {
            biased;
            _ = cancellation_receiver.changed() => {
                return Err(String::from("assistant request cancelled"));
            }
            event = tokio::time::timeout(OPENCODE_PROMPT_INACTIVITY_TIMEOUT, events.next()) => event,
        }
        .map_err(|_| String::from("OpenCode agent timed out"))?
        .ok_or_else(|| String::from("OpenCode agent stream closed"))?
        .map_err(|error| error.to_string())?;
        match event {
            voxgolem_platform::opencode::OpencodeEvent::Text(text) => output.push_str(&text),
            voxgolem_platform::opencode::OpencodeEvent::Error(message) => return Err(message),
            voxgolem_platform::opencode::OpencodeEvent::Completed => {
                if output.trim().is_empty() {
                    return Err(String::from(
                        "OpenCode agent completed without visible text",
                    ));
                }
                return Ok(AgentTextResult {
                    text: output,
                    evidence,
                    refusal: false,
                });
            }
            voxgolem_platform::opencode::OpencodeEvent::Reasoning(_)
            | voxgolem_platform::opencode::OpencodeEvent::Status(_)
            | voxgolem_platform::opencode::OpencodeEvent::Tool { .. } => {}
            voxgolem_platform::opencode::OpencodeEvent::ToolEvidence(evidence_item) => {
                evidence.push(evidence_item);
            }
        }
    }
}

fn emit_prompt_event_controlled(
    app: &tauri::AppHandle,
    request_id: &str,
    generation: u64,
    event: PromptExecutionEventPayload,
) -> Result<(), PromptControlError> {
    let terminal = matches!(&event, PromptExecutionEventPayload::Cancelled { .. });
    let state = app.state::<AppState>();
    let _publication_guard = state.prompt_publication_gate.lock().map_err(|_| {
        PromptControlError::Error(String::from("prompt publication gate is poisoned"))
    })?;
    let (updates, execution_id) = {
        let active = state.active_prompt.lock().map_err(|_| {
            PromptControlError::Error(String::from("active prompt lock is poisoned"))
        })?;
        let Some(active) = active
            .as_ref()
            .filter(|active| prompt_event_identity_matches(active, request_id, generation))
        else {
            return Err(PromptControlError::Cancelled);
        };
        if terminal {
            if !claim_cancelled_prompt_publication(active) {
                return Err(PromptControlError::Cancelled);
            }
        } else if active.cancelled.load(Ordering::Acquire) {
            return Err(PromptControlError::Cancelled);
        }
        let updates = active
            .updates
            .lock()
            .map_err(|_| {
                PromptControlError::Error(String::from("prompt update channel lock is poisoned"))
            })?
            .clone();
        (updates, active.assistant_generation)
    };
    if terminal {
        return Ok(());
    }
    let Some(updates) = updates else {
        return Err(PromptControlError::Error(String::from(
            "prompt update channel is unavailable",
        )));
    };
    if updates.send(event).is_err() {
        drop(_publication_guard);
        let _ = cancel_assistant_request(&state, execution_id);
        return Err(PromptControlError::Cancelled);
    }
    Ok(())
}

fn prompt_event_identity_matches(active: &ActivePrompt, request_id: &str, generation: u64) -> bool {
    active.request_id == request_id && active.generation == generation
}

fn emit_terminal_prompt_event_controlled(
    app: &tauri::AppHandle,
    active: &ActivePrompt,
    _event: PromptExecutionEventPayload,
) -> Result<(), PromptControlError> {
    let state = app.state::<AppState>();
    let _publication_guard = state.prompt_publication_gate.lock().map_err(|_| {
        PromptControlError::Error(String::from("prompt publication gate is poisoned"))
    })?;
    let superseded = state
        .active_prompt
        .lock()
        .map_err(|_| PromptControlError::Error(String::from("active prompt lock is poisoned")))?
        .as_ref()
        .is_some_and(|current| {
            current.request_id == active.request_id && current.generation != active.generation
        });
    if superseded {
        return Err(PromptControlError::Cancelled);
    }
    if !claim_terminal_prompt_publication(
        active,
        state.prompt_publication_epoch.load(Ordering::Acquire),
        state.active_prompt_generation.load(Ordering::Acquire),
    ) {
        return Err(PromptControlError::Cancelled);
    }
    Ok(())
}

fn claim_cancelled_prompt_publication(active: &ActivePrompt) -> bool {
    active.cancelled.load(Ordering::Acquire)
        && !active.terminal_published.swap(true, Ordering::AcqRel)
}

fn claim_terminal_prompt_publication(
    active: &ActivePrompt,
    publication_epoch: u64,
    latest_generation: u64,
) -> bool {
    active.generation == latest_generation
        && active.publication_epoch == publication_epoch
        && active
            .terminal_kind
            .lock()
            .ok()
            .and_then(|terminal| *terminal)
            .is_some()
        && !active.terminal_published.swap(true, Ordering::AcqRel)
}

fn emit_validated_sources_controlled(
    app: &tauri::AppHandle,
    request_id: &str,
    generation: u64,
    report: &voxgolem_core::agent_pipeline::DeepReport,
) -> Result<(), PromptControlError> {
    if report.sources.is_empty() {
        return Ok(());
    }
    emit_prompt_event_controlled(
        app,
        request_id,
        generation,
        PromptExecutionEventPayload::Sources {
            sources: report
                .sources
                .iter()
                .map(|source| SourcePayload {
                    url: source.url.clone(),
                    title: source.title.clone(),
                })
                .collect(),
        },
    )
}

fn emit_stage_event_controlled(
    app: &tauri::AppHandle,
    request_id: &str,
    generation: u64,
    stage: StagePayload,
    status: StageStatusPayload,
    detail: Option<&str>,
) -> Result<(), PromptControlError> {
    let detail = detail.map(|detail| detail.chars().take(256).collect::<String>());
    emit_prompt_event_controlled(
        app,
        request_id,
        generation,
        PromptExecutionEventPayload::Stage {
            stage,
            status,
            detail,
        },
    )
}

fn register_active_prompt(
    active_prompt: &Mutex<Option<ActivePrompt>>,
    generation_counter: &AtomicU64,
    request_id: &str,
    assistant_generation: voxgolem_core::conversation::ExecutionId,
    client: Option<voxgolem_platform::opencode::OpencodeClient>,
    publication_epoch: u64,
) -> Result<ActivePromptRegistration, String> {
    let mut active = active_prompt
        .lock()
        .map_err(|_| String::from("active prompt lock is poisoned"))?;
    if active.is_some() {
        return Err(String::from("another prompt is already active"));
    }
    let generation = generation_counter
        .fetch_add(1, Ordering::SeqCst)
        .saturating_add(1);
    let cancelled = Arc::new(AtomicBool::new(false));
    let (cancellation_sender, _) = tokio::sync::watch::channel(false);
    let cancellation_signal = Arc::new(cancellation_sender);
    let completion_signal = Arc::new(tokio::sync::Notify::new());
    let settled = Arc::new(AtomicBool::new(false));
    let terminal_published = Arc::new(AtomicBool::new(false));
    let terminal_kind = Arc::new(Mutex::new(None));
    let abort_client = Arc::new(Mutex::new(client));
    *active = Some(ActivePrompt {
        request_id: request_id.to_string(),
        generation,
        assistant_generation,
        cancelled: Arc::clone(&cancelled),
        cancellation_signal: Arc::clone(&cancellation_signal),
        completion_signal,
        abort_client,
        deep_handle: Arc::new(Mutex::new(None)),
        deep_abort: Arc::new(Mutex::new(None)),
        settled: Arc::clone(&settled),
        terminal_published,
        publication_epoch,
        terminal_kind,
        updates: Arc::new(Mutex::new(None)),
    });
    Ok((generation, cancelled, cancellation_signal, settled))
}

fn reset_conversation_execution<F>(
    prompt_publication_gate: &Mutex<()>,
    prompt_publication_epoch: &AtomicU64,
    lifecycle_gate: &Mutex<()>,
    reset_state: (&AtomicBool, &Arc<PromptCleanupTracker>),
    active_prompt: &Mutex<Option<ActivePrompt>>,
    session: &Mutex<voxgolem_core::conversation::ConversationSession>,
    reset_runtime: F,
) -> Result<ResetConversationExecution, String>
where
    F: FnOnce() -> Result<(), String>,
{
    let (reset_in_progress, prompt_cleanup) = reset_state;
    let _publication = prompt_publication_gate
        .lock()
        .map_err(|_| String::from("prompt publication gate is poisoned"))?;
    let _lifecycle = lifecycle_gate
        .lock()
        .map_err(|_| String::from("lifecycle gate is poisoned"))?;
    let mut active_guard = active_prompt
        .lock()
        .map_err(|_| String::from("active prompt lock is poisoned"))?;
    let mut session = session
        .lock()
        .map_err(|_| String::from("conversation session lock is poisoned"))?;
    reset_runtime()?;
    reset_in_progress.store(true, Ordering::Release);
    let reset = session.reset();
    drop(session);
    let active = active_guard.take();
    prompt_publication_epoch.fetch_add(1, Ordering::SeqCst);
    if let Some(active) = active.as_ref() {
        active.terminal_published.store(true, Ordering::Release);
    }
    let cleanup = active.map(|active| {
        let reset_cancelled = reset.invalidated == Some(active.assistant_generation);
        let was_cancelled = active
            .terminal_kind
            .lock()
            .ok()
            .and_then(|terminal| *terminal)
            == Some(TerminalKind::Cancelled);
        if reset_cancelled || was_cancelled {
            record_terminal_kind(&active.terminal_kind, TerminalKind::Cancelled);
            active.cancelled.store(true, Ordering::SeqCst);
            active.settled.store(true, Ordering::Release);
        }
        let abort_claim = (reset_cancelled || was_cancelled)
            .then(|| claim_prompt_abort(&active.abort_client, prompt_cleanup))
            .flatten();
        let deep_handle = (reset_cancelled || was_cancelled)
            .then(|| claim_prompt_deep_handle(&active))
            .flatten();
        let deep_abort = (reset_cancelled || was_cancelled)
            .then(|| claim_prompt_deep_abort(&active))
            .flatten();
        PromptPhysicalCleanup {
            active,
            abort_claim,
            deep_handle,
            deep_abort,
            cancel: reset_cancelled || was_cancelled,
        }
    });
    Ok(ResetConversationExecution { cleanup })
}

fn clear_active_prompt(
    active_prompt: &Mutex<Option<ActivePrompt>>,
    request_id: &str,
    generation: u64,
) -> Result<Option<ActivePrompt>, String> {
    let mut active = active_prompt
        .lock()
        .map_err(|_| String::from("active prompt lock is poisoned"))?;
    if active
        .as_ref()
        .is_some_and(|active| active.request_id == request_id && active.generation == generation)
    {
        return Ok(active.take());
    }
    Ok(None)
}

fn is_active_prompt(
    active_prompt: &Mutex<Option<ActivePrompt>>,
    request_id: &str,
    generation: u64,
) -> Result<bool, String> {
    active_prompt
        .lock()
        .map_err(|_| String::from("active prompt lock is poisoned"))
        .map(|active| {
            active.as_ref().is_some_and(|active| {
                active.request_id == request_id && active.generation == generation
            })
        })
}

async fn stream_opencode_prompt(
    context: OpencodeStreamContext<'_>,
    prompt: &str,
    model: voxgolem_core::assistant::InstantModel,
) -> OpencodePromptResult {
    let message_id = format!(
        "msg_voxgolem_{}_{}",
        context.generation,
        current_time_ms().unwrap_or(0)
    );
    let prompt = match voxgolem_platform::opencode::OpencodePrompt::new(prompt.to_string()) {
        Ok(prompt) => prompt.with_message_id(message_id.clone()),
        Err(error) => return OpencodePromptResult::Failed(format!("invalid prompt: {error:?}")),
    };
    let mut cancellation_receiver = context.cancellation_signal.subscribe();
    if *cancellation_receiver.borrow() {
        return OpencodePromptResult::Cancelled;
    }
    let events_result = race_durable_cancellation(
        &mut cancellation_receiver,
        context.client.events_for_message(message_id),
    )
    .await;
    let events = match events_result {
        Err(()) => return OpencodePromptResult::Cancelled,
        Ok(result) => match result {
            Ok(events) => events,
            Err(error) => return OpencodePromptResult::Failed(error.to_string()),
        },
    };
    futures_util::pin_mut!(events);
    let model = match model {
        voxgolem_core::assistant::InstantModel::OpenCodeSolHigh => {
            voxgolem_platform::opencode::OpencodeModel::Gpt56SolHigh
        }
        voxgolem_core::assistant::InstantModel::OpenCodeLunaLow => {
            voxgolem_platform::opencode::OpencodeModel::Gpt56LunaLow
        }
        _ => {
            return OpencodePromptResult::Failed(String::from("invalid OpenCode model selection"));
        }
    };
    if *cancellation_receiver.borrow() {
        return OpencodePromptResult::Cancelled;
    }
    let prompt_result = race_durable_cancellation(
        &mut cancellation_receiver,
        context.client.prompt_with_options(
            &prompt,
            voxgolem_platform::opencode::OpencodePromptOptions::new(
                model,
                voxgolem_platform::opencode::OpencodeToolPolicy::AnswerOnly,
            ),
        ),
    )
    .await;
    let prompt_result = match prompt_result {
        Err(()) => return OpencodePromptResult::Cancelled,
        Ok(result) => result,
    };
    if let Err(error) = prompt_result {
        return OpencodePromptResult::Failed(error.to_string());
    }

    let mut output = String::new();
    loop {
        let still_active = is_active_prompt(
            context.active_prompt,
            context.request_id,
            context.generation,
        )
        .unwrap_or(false);
        if context.cancelled.load(Ordering::SeqCst) || !still_active {
            return OpencodePromptResult::Cancelled;
        }
        let event = tokio::select! {
            _ = cancellation_receiver.changed() => {
                return OpencodePromptResult::Cancelled;
            }
            event = tokio::time::timeout(OPENCODE_PROMPT_INACTIVITY_TIMEOUT, events.next()) => {
                match event {
                    Ok(Some(event)) => event,
                    Ok(None) => {
                        return OpencodePromptResult::Failed(String::from(
                            "OpenCode event stream closed before completion",
                        ));
                    }
                    Err(_) => {
                        return OpencodePromptResult::Failed(String::from(
                            "OpenCode prompt timed out waiting for activity",
                        ));
                    }
                }
            }
        };
        let event = match event {
            Ok(event) => event,
            Err(error) => return OpencodePromptResult::Failed(error.to_string()),
        };
        let payload = match event {
            voxgolem_platform::opencode::OpencodeEvent::Text(text) => {
                output.push_str(&text);
                PromptExecutionEventPayload::Text { text }
            }
            voxgolem_platform::opencode::OpencodeEvent::Reasoning(text) => {
                PromptExecutionEventPayload::Reasoning { text }
            }
            voxgolem_platform::opencode::OpencodeEvent::Status(message) => {
                PromptExecutionEventPayload::Status { message }
            }
            voxgolem_platform::opencode::OpencodeEvent::Tool {
                name,
                status,
                detail,
            } => PromptExecutionEventPayload::Tool {
                tool: name,
                status: format!("{status:?}").to_ascii_lowercase(),
                detail,
            },
            voxgolem_platform::opencode::OpencodeEvent::ToolEvidence(evidence) => {
                PromptExecutionEventPayload::Tool {
                    tool: evidence.tool,
                    status: String::from("completed"),
                    detail: evidence.detail,
                }
            }
            voxgolem_platform::opencode::OpencodeEvent::Error(message) => {
                return OpencodePromptResult::Failed(message);
            }
            voxgolem_platform::opencode::OpencodeEvent::Completed => {
                return OpencodePromptResult::Completed(output);
            }
        };
        if let Err(error) = emit_prompt_event_controlled(
            context.app,
            context.request_id,
            context.generation,
            payload,
        ) {
            return match error {
                PromptControlError::Cancelled => OpencodePromptResult::Cancelled,
                PromptControlError::Error(error) => OpencodePromptResult::Failed(error),
            };
        }
    }
}

#[tauri::command]
async fn cancel_prompt(
    request_id: String,
    app_state: tauri::State<'_, AppState>,
) -> Result<bool, String> {
    cancel_prompt_if_active(&app_state, &request_id)
}

fn cancel_prompt_if_active(app_state: &AppState, request_id: &str) -> Result<bool, String> {
    cancel_active_tts_generation(app_state);
    let Some(active) = app_state
        .active_prompt
        .lock()
        .map_err(|_| String::from("active prompt lock is poisoned"))?
        .as_ref()
        .filter(|active| active.request_id == request_id)
        .cloned()
    else {
        return Ok(false);
    };
    if !matches!(
        cancel_assistant_request(app_state, active.assistant_generation),
        voxgolem_core::conversation::SettlementResult::Applied(
            voxgolem_core::conversation::AppliedOutcome::Cancelled
        )
    ) {
        return Ok(false);
    }
    {
        let _publication_guard = app_state
            .prompt_publication_gate
            .lock()
            .map_err(|_| String::from("prompt publication gate is poisoned"))?;
    }
    Ok(true)
}

#[tauri::command]
fn record_speech_activity(
    now_ms: u64,
    app_state: tauri::State<'_, AppState>,
) -> Result<RuntimePhaseResponsePayload, String> {
    let _operation_guard =
        lock_response_backend_operation(&app_state.response_backend_operation_lock)?;
    let _lifecycle = app_state
        .lifecycle_gate
        .lock()
        .map_err(|_| String::from("lifecycle gate is poisoned"))?;
    ensure_startup_ready_for_prompt(&app_state.startup_state)?;
    apply_voice_pipeline_transition(
        &app_state.voice_pipeline_state,
        app_state.voice_pipeline_config,
        voxgolem_core::voice_pipeline::VoicePipelineEvent::SpeechDetected { now_ms },
    )?;

    Ok(RuntimePhaseResponsePayload {
        ..current_runtime_phase_response(&app_state.voice_pipeline_state, None, None)?
    })
}

#[tauri::command]
fn mark_silence(
    telemetry_frame_id: Option<String>,
    app_state: tauri::State<'_, AppState>,
) -> Result<RuntimePhaseResponsePayload, String> {
    let _operation_guard =
        lock_response_backend_operation(&app_state.response_backend_operation_lock)?;
    ensure_startup_ready_for_prompt(&app_state.startup_state)?;
    let now_ms = current_time_ms()?;

    let action = {
        let _lifecycle = app_state
            .lifecycle_gate
            .lock()
            .map_err(|_| String::from("lifecycle gate is poisoned"))?;
        apply_voice_pipeline_transition(
            &app_state.voice_pipeline_state,
            app_state.voice_pipeline_config,
            voxgolem_core::voice_pipeline::VoicePipelineEvent::SilenceCheck { now_ms },
        )?
    };

    let should_measure_transcription = matches!(
        action,
        voxgolem_core::voice_pipeline::VoicePipelineAction::FinishedUtterance { .. }
    );
    if should_measure_transcription {
        app_state
            .partial_transcription
            .lock()
            .map_err(|_| String::from("partial transcription lock is poisoned"))?
            .finalize();
        let _lifecycle = app_state
            .completion_lifecycle_lock
            .lock()
            .map_err(|_| String::from("completion lifecycle lock is poisoned"))?;
        clear_completion_request_state_locked(&app_state, false)?;
    }
    let transcription_started_ms = if should_measure_transcription {
        Some(current_time_ms()?)
    } else {
        None
    };

    let transcript_text = match transcribe_finished_utterance(&action, &app_state.parakeet_runtime)
    {
        Ok(transcript_text) => transcript_text,
        Err(error) => {
            let _lifecycle = app_state
                .lifecycle_gate
                .lock()
                .map_err(|_| String::from("lifecycle gate is poisoned"))?;
            reset_voice_pipeline_to_waiting(
                &app_state.voice_pipeline_state,
                &app_state.wake_word_runtime,
                &app_state.voice_activity_runtime,
                app_state.voice_pipeline_config,
            )?;

            return Err(error);
        }
    };

    let transcription_completed_ms = if should_measure_transcription {
        Some(current_time_ms()?)
    } else {
        None
    };

    build_mark_silence_response(
        &app_state.voice_pipeline_state,
        &action,
        transcript_text,
        Some(RuntimeTelemetryPayload {
            frame_id: telemetry_frame_id,
            backend_ingest_started_ms: None,
            backend_ingest_completed_ms: None,
            wake_detected_ms: None,
            wake_confidence: None,
            transcription_started_ms,
            transcription_completed_ms,
        }),
    )
}

#[tauri::command]
async fn reset_session(
    app_state: tauri::State<'_, AppState>,
) -> Result<RuntimePhaseResponsePayload, String> {
    let _reset_guard = app_state.reset_operation_lock.lock().await;
    cancel_active_tts_generation(&app_state);
    ensure_startup_ready_for_prompt(&app_state.startup_state)?;
    let reset_execution = reset_conversation_execution(
        &app_state.prompt_publication_gate,
        &app_state.prompt_publication_epoch,
        &app_state.lifecycle_gate,
        (&app_state.reset_in_progress, &app_state.prompt_cleanup),
        &app_state.active_prompt,
        &app_state.conversation_session,
        || reset_voice_runtime_session(&app_state),
    )?;
    let _admission_guard = ResetAdmissionGuard {
        lifecycle_gate: Arc::clone(&app_state.lifecycle_gate),
        reset_in_progress: Arc::clone(&app_state.reset_in_progress),
    };
    let cleanup = reset_execution.cleanup;
    reset_llama_session(&app_state)?;
    reset_runtime_session(&app_state)?;
    abort_prefetch(&app_state)?;
    if let Some(cleanup) = cleanup {
        cleanup.cancel_and_wait().await;
    }
    let has_opencode = app_state
        .opencode_server
        .lock()
        .map_err(|_| String::from("opencode server lock is poisoned"))?
        .is_some();
    if has_opencode {
        reset_opencode_session(&app_state).await?;
    }

    Ok(RuntimePhaseResponsePayload {
        ..current_runtime_phase_response(&app_state.voice_pipeline_state, None, None)?
    })
}

fn reset_runtime_session(app_state: &AppState) -> Result<(), String> {
    app_state
        .partial_transcription
        .lock()
        .map_err(|_| String::from("partial transcription lock is poisoned"))?
        .reset();
    clear_completion_state(app_state)?;
    Ok(())
}

fn reset_voice_runtime_session(app_state: &AppState) -> Result<(), String> {
    if current_runtime_phase(&app_state.voice_pipeline_state)? == RuntimePhasePayload::Sleeping {
        reset_wake_word_runtime(&app_state.wake_word_runtime)?;
        reset_voice_activity_runtime(&app_state.voice_activity_runtime)?;
        return Ok(());
    }
    reset_voice_pipeline_to_waiting(
        &app_state.voice_pipeline_state,
        &app_state.wake_word_runtime,
        &app_state.voice_activity_runtime,
        app_state.voice_pipeline_config,
    )
}

fn reset_llama_session(app_state: &AppState) -> Result<(), String> {
    app_state
        .llama_cpp_conversation
        .lock()
        .map_err(|_| String::from("local llama.cpp conversation lock is poisoned"))?
        .clear();
    Ok(())
}

struct OpencodeServerRestoreGuard {
    server: Option<voxgolem_platform::opencode::OpencodeServer>,
    slot: Arc<Mutex<Option<voxgolem_platform::opencode::OpencodeServer>>>,
}

impl Drop for OpencodeServerRestoreGuard {
    fn drop(&mut self) {
        let Some(server) = self.server.take() else {
            return;
        };
        if let Ok(mut slot) = self.slot.lock() {
            slot.replace(server);
        }
    }
}

async fn reset_opencode_session(app_state: &AppState) -> Result<(), String> {
    let server = app_state
        .opencode_server
        .lock()
        .map_err(|_| String::from("opencode server lock is poisoned"))?
        .take()
        .ok_or_else(|| String::from("OpenCode server is not available"))?;
    let slot = Arc::clone(&app_state.opencode_server);
    let task = tauri::async_runtime::spawn(async move {
        let mut restore = OpencodeServerRestoreGuard {
            server: Some(server),
            slot,
        };
        restore
            .server
            .as_mut()
            .expect("OpenCode reset guard should own server")
            .reset_with_deadlines(
                tokio::time::Instant::now() + Duration::from_secs(5),
                Duration::from_secs(1),
            )
            .await
            .map_err(|error| format!("failed to reset OpenCode session: {error}"))
    });
    task.await
        .map_err(|error| format!("OpenCode reset task failed: {error}"))?
}

#[tauri::command]
fn ingest_audio_frame(
    frame: Vec<f32>,
    telemetry_frame_id: Option<String>,
    app: tauri::AppHandle,
    app_state: tauri::State<'_, AppState>,
) -> Result<RuntimePhaseResponsePayload, String> {
    let _lifecycle = app_state
        .lifecycle_gate
        .lock()
        .map_err(|_| String::from("lifecycle gate is poisoned"))?;
    ensure_startup_ready_for_prompt(&app_state.startup_state)?;
    let backend_ingest_started_ms = current_time_ms()?;
    let mut guard = app_state
        .voice_pipeline_state
        .lock()
        .map_err(|_| String::from("voice pipeline lock is poisoned"))?;
    let now_ms = current_time_ms()?;
    let started_listening = matches!(
        guard.session().runtime().phase(),
        voxgolem_core::runtime::RuntimePhase::Listening
    );

    let (wake_word_detection, wake_word_confidence) = if matches!(
        guard.session().runtime().phase(),
        voxgolem_core::runtime::RuntimePhase::Sleeping
    ) {
        process_wake_word_frame(&app_state.wake_word_runtime, &frame)?
    } else {
        (None, None)
    };
    let wake_word_now_ms = wake_word_event_timestamp(now_ms, wake_word_detection);
    let speech_detected = if started_listening {
        process_voice_activity_frame(&app_state.voice_activity_runtime, &frame)?
    } else {
        false
    };

    let mut next_state = ingest_audio_frame_with_optional_wake_word_detection(
        &guard,
        app_state.voice_pipeline_config,
        frame,
        wake_word_now_ms,
    )?;

    if wake_word_now_ms.is_some() {
        reset_voice_activity_runtime(&app_state.voice_activity_runtime)?;
    }

    next_state = apply_optional_speech_activity(
        next_state,
        app_state.voice_pipeline_config,
        speech_detected,
        now_ms,
    )?;

    *guard = next_state;

    let partial_action = if app_state.parakeet_runtime.is_none() {
        partial_transcription::PartialTranscriptionAction::Ignore
    } else if wake_word_now_ms.is_some() {
        let session_id = app_state
            .partial_voice_session
            .fetch_add(1, Ordering::SeqCst)
            + 1;
        app_state
            .partial_transcription
            .lock()
            .map_err(|_| String::from("partial transcription lock is poisoned"))?
            .start_session(session_id);
        partial_transcription::PartialTranscriptionAction::Ignore
    } else if started_listening {
        app_state
            .partial_transcription
            .lock()
            .map_err(|_| String::from("partial transcription lock is poisoned"))?
            .request_snapshot(now_ms, guard.capture().utterance_samples())
    } else {
        partial_transcription::PartialTranscriptionAction::Ignore
    };

    let backend_ingest_completed_ms = current_time_ms()?;
    let response = runtime_phase_response_from_state(
        &guard,
        None,
        None,
        Some(RuntimeTelemetryPayload {
            frame_id: telemetry_frame_id,
            backend_ingest_started_ms: Some(backend_ingest_started_ms),
            backend_ingest_completed_ms: Some(backend_ingest_completed_ms),
            wake_detected_ms: wake_word_now_ms,
            wake_confidence: wake_word_confidence,
            transcription_started_ms: None,
            transcription_completed_ms: None,
        }),
    );
    drop(guard);

    if let Some(parakeet_runtime) = app_state.parakeet_runtime.as_ref() {
        spawn_partial_transcription(
            app,
            partial_action,
            Arc::clone(parakeet_runtime),
            Arc::clone(&app_state.partial_transcription),
            app_state.voice_pipeline_config.sample_rate_hz(),
        );
    }

    Ok(response)
}

fn spawn_partial_transcription(
    app: tauri::AppHandle,
    action: partial_transcription::PartialTranscriptionAction,
    parakeet_runtime: Arc<Mutex<transcription::ParakeetRuntime>>,
    scheduler: Arc<Mutex<partial_transcription::PartialTranscriptionScheduler>>,
    sample_rate_hz: u32,
) {
    if !matches!(
        action,
        partial_transcription::PartialTranscriptionAction::StartSnapshot { .. }
    ) {
        return;
    }

    tauri::async_runtime::spawn(async move {
        let mut next_action = action;
        while let partial_transcription::PartialTranscriptionAction::StartSnapshot {
            session_id,
            revision,
            samples,
        } = next_action
        {
            let started = Instant::now();
            let runtime = Arc::clone(&parakeet_runtime);
            let result = tauri::async_runtime::spawn_blocking(move || {
                let input = voxgolem_model::parakeet::ParakeetTranscriptionInput::new(
                    sample_rate_hz,
                    samples,
                )
                .map_err(|_| String::from("invalid partial transcription input"))?;
                let transcript = runtime
                    .lock()
                    .map_err(|_| String::from("partial transcription runtime lock is poisoned"))?
                    .transcribe(&input)
                    .map_err(|_| String::from("partial transcription inference failed"))?;
                Ok::<String, String>(transcript.text().to_string())
            })
            .await
            .map_err(|_| String::from("partial transcription worker failed"))
            .and_then(|result| result);

            let succeeded = result.is_ok();
            let text = result.unwrap_or_default();
            let app_state = app.state::<AppState>();
            append_telemetry(
                &app_state.telemetry_sink,
                telemetry::TelemetryMetadata {
                    schema_version: telemetry::SCHEMA_VERSION,
                    timestamp_ms: current_time_ms().unwrap_or_default(),
                    request_id: format!("partial-{session_id}-{revision}"),
                    generation: revision,
                    input_source: telemetry::InputSource::Voice,
                    provider: telemetry::Provider::Local,
                    model: String::from("parakeet-v2-int8"),
                    stage: telemetry::Stage::PartialTranscription,
                    transport: telemetry::Transport::InProcess,
                    inference_provider: telemetry::InferenceProvider::Cpu,
                    speculative_origin: telemetry::SpeculativeOrigin::System,
                    input_tokens: None,
                    output_tokens: None,
                    duration_ms: Some(started.elapsed().as_millis() as u64),
                    status: if succeeded {
                        telemetry::Status::Ok
                    } else {
                        telemetry::Status::Error
                    },
                    error_category: if succeeded {
                        telemetry::ErrorCategory::None
                    } else {
                        telemetry::ErrorCategory::Internal
                    },
                },
            );
            next_action = match current_time_ms() {
                Ok(now_ms) => match scheduler.lock() {
                    Ok(mut scheduler) => scheduler.complete(now_ms, session_id, revision, text),
                    Err(_) => break,
                },
                Err(_) => break,
            };

            if let partial_transcription::PartialTranscriptionAction::PublishText {
                session_id,
                revision,
                text,
            } = &next_action
            {
                if !text.trim().is_empty() {
                    let _ = app.emit(
                        "partial-transcription-event",
                        PartialTranscriptionEventPayload {
                            session_id: *session_id,
                            revision: *revision,
                            text: text.clone(),
                        },
                    );
                    let _ = queue_completion_request(
                        CompletionSource::Voice,
                        *revision,
                        Some(*session_id),
                        text.clone(),
                        &app_state,
                    );
                }
                break;
            }
        }
    });
}

#[tauri::command]
fn request_completion(
    revision: u64,
    prompt: String,
    app_state: tauri::State<'_, AppState>,
) -> Result<(), String> {
    queue_completion_request(CompletionSource::Typed, revision, None, prompt, &app_state)
}

#[tauri::command]
fn clear_completion(app_state: tauri::State<'_, AppState>) -> Result<(), String> {
    let _lifecycle = app_state
        .completion_lifecycle_lock
        .lock()
        .map_err(|_| String::from("completion lifecycle lock is poisoned"))?;
    clear_completion_state_locked(&app_state, true)
}

fn clear_completion_state(app_state: &AppState) -> Result<(), String> {
    let _lifecycle = app_state
        .completion_lifecycle_lock
        .lock()
        .map_err(|_| String::from("completion lifecycle lock is poisoned"))?;
    clear_completion_state_locked(app_state, false)
}

fn clear_completion_state_locked(
    app_state: &AppState,
    require_runtime: bool,
) -> Result<(), String> {
    clear_completion_request_state_locked(app_state, require_runtime)?;
    invalidate_prefetch(app_state)?;
    Ok(())
}

fn clear_completion_request_state_locked(
    app_state: &AppState,
    require_runtime: bool,
) -> Result<(), String> {
    let request = app_state
        .completion_request
        .lock()
        .map_err(|_| String::from("completion request lock is poisoned"))?
        .clone();
    if require_runtime && request.is_none() {
        return Err(String::from("completion runtime is not available"));
    }
    if let Some(request) = request {
        request.clear();
    }
    app_state
        .completion_context
        .lock()
        .map_err(|_| String::from("completion context lock is poisoned"))?
        .take();
    Ok(())
}

fn queue_completion_request(
    source: CompletionSource,
    client_revision: u64,
    voice_session_id: Option<partial_transcription::VoiceSessionId>,
    prompt: String,
    app_state: &AppState,
) -> Result<(), String> {
    let _lifecycle = app_state
        .completion_lifecycle_lock
        .lock()
        .map_err(|_| String::from("completion lifecycle lock is poisoned"))?;
    invalidate_prefetch(app_state)?;
    if !assistant_completion_enabled(&app_state.conversation_session)? {
        clear_completion_state_locked(app_state, false)?;
        return Ok(());
    }
    let request = app_state
        .completion_request
        .lock()
        .map_err(|_| String::from("completion request lock is poisoned"))?
        .clone()
        .ok_or_else(|| String::from("completion runtime is not available"))?;
    if prompt.trim().is_empty() || prompt.len() > COMPLETION_PROMPT_MAX_BYTES {
        request.clear();
        app_state
            .completion_context
            .lock()
            .map_err(|_| String::from("completion context lock is poisoned"))?
            .take();
        return Ok(());
    }
    let backend_revision = app_state
        .completion_generation
        .fetch_add(1, Ordering::SeqCst)
        .saturating_add(1);
    let started_ms = current_time_ms()?;
    *app_state
        .completion_context
        .lock()
        .map_err(|_| String::from("completion context lock is poisoned"))? =
        Some(CompletionRequestContext {
            backend_revision,
            client_revision,
            source,
            voice_session_id,
            prompt: prompt.clone(),
            started_ms,
        });
    request.request(backend_revision, prompt);
    Ok(())
}

fn assistant_completion_enabled(
    coordinator: &Mutex<voxgolem_core::conversation::ConversationSession>,
) -> Result<bool, String> {
    coordinator
        .lock()
        .map(|coordinator| coordinator.policy().completion_enabled)
        .map_err(|_| String::from("assistant coordinator lock is poisoned"))
}

async fn emit_completion_predictions(
    app: tauri::AppHandle,
    mut predictor: voxgolem_platform::completion::CompletionPredictor,
    context: Arc<Mutex<Option<CompletionRequestContext>>>,
) {
    while let Some(prediction) = predictor.next().await {
        let app_state = app.state::<AppState>();
        let Ok(_lifecycle) = app_state.completion_lifecycle_lock.lock() else {
            break;
        };
        if !assistant_completion_enabled(&app_state.conversation_session).unwrap_or(false) {
            continue;
        }
        let matching = context.lock().ok().and_then(|mut current| {
            let matches = current.as_ref().is_some_and(|request| {
                request.backend_revision == prediction.revision
                    && request.prompt == prediction.prompt
            });
            matches.then(|| current.take()).flatten()
        });
        let Some(request) = matching else {
            continue;
        };
        append_telemetry(
            &app_state.telemetry_sink,
            telemetry::TelemetryMetadata {
                schema_version: telemetry::SCHEMA_VERSION,
                timestamp_ms: current_time_ms().unwrap_or_default(),
                request_id: format!("completion-{}", request.backend_revision),
                generation: request.backend_revision,
                input_source: match request.source {
                    CompletionSource::Typed => telemetry::InputSource::Text,
                    CompletionSource::Voice => telemetry::InputSource::Voice,
                },
                provider: telemetry::Provider::Local,
                model: String::from("qwen-completion"),
                stage: telemetry::Stage::CompletionPrediction,
                transport: telemetry::Transport::Http,
                inference_provider: app_state
                    .completion_runtime
                    .lock()
                    .ok()
                    .and_then(|runtime| {
                        runtime
                            .as_ref()
                            .map(|runtime| telemetry_inference_provider(runtime.actual_provider()))
                    })
                    .unwrap_or(telemetry::InferenceProvider::AttachedUnknown),
                speculative_origin: match request.source {
                    CompletionSource::Typed => telemetry::SpeculativeOrigin::User,
                    CompletionSource::Voice => telemetry::SpeculativeOrigin::System,
                },
                input_tokens: None,
                output_tokens: None,
                duration_ms: Some(
                    current_time_ms()
                        .unwrap_or(request.started_ms)
                        .saturating_sub(request.started_ms),
                ),
                status: telemetry::Status::Ok,
                error_category: telemetry::ErrorCategory::None,
            },
        );
        if let Some(suffix) = prediction
            .suffix
            .as_deref()
            .filter(|suffix| !suffix.is_empty())
        {
            let _ = queue_assistant_prefetch(
                &app,
                format!("{}{}", request.prompt, suffix),
                request.source,
            );
        }
        let _ = app.emit(
            "completion-event",
            CompletionEventPayload {
                source: request.source,
                revision: request.client_revision,
                voice_session_id: request.voice_session_id,
                suffix: prediction.suffix,
            },
        );
    }

    let app_state = app.state::<AppState>();
    let mut capability_failed = false;
    let runtime = {
        let Ok(_lifecycle) = app_state.completion_lifecycle_lock.lock() else {
            return;
        };
        if let Ok(mut request) = app_state.completion_request.lock() {
            if let Some(request) = request.take() {
                request.clear();
            }
        }
        if let Ok(mut context) = app_state.completion_context.lock() {
            context.take();
        }
        if !app_state.exit_cleanup_started.load(Ordering::SeqCst) {
            capability_failed = true;
        }
        app_state
            .completion_runtime
            .lock()
            .map(|mut runtime| runtime.take())
            .unwrap_or(None)
    };
    if capability_failed {
        fail_startup_capability(
            &app_state.lifecycle_gate,
            &app_state.startup_state,
            "qwen_prediction",
            String::from("completion prediction worker stopped unexpectedly"),
        );
    }
    if let Some(mut runtime) = runtime {
        let _ = runtime.shutdown().await;
    }
}

fn invalidate_prefetch(app_state: &AppState) -> Result<(), String> {
    app_state.prefetch_generation.fetch_add(1, Ordering::SeqCst);
    let active = app_state
        .prefetch_task
        .lock()
        .map_err(|_| String::from("prefetch task lock is poisoned"))?;
    if let Some(task) = active.as_ref() {
        task.cancelled.store(true, Ordering::SeqCst);
        task.cancellation_signal.send_replace(true);
        if let Some(cancellation) = task
            .local_cancellation
            .lock()
            .ok()
            .and_then(|cancellation| cancellation.clone())
        {
            cancellation.cancel();
        }
    }
    drop(active);
    app_state
        .prefetch_cache
        .lock()
        .map_err(|_| String::from("prefetch cache lock is poisoned"))?
        .take();
    Ok(())
}

fn abort_prefetch(app_state: &AppState) -> Result<(), String> {
    invalidate_prefetch(app_state)?;
    let mut active = app_state
        .prefetch_task
        .lock()
        .map_err(|_| String::from("prefetch task lock is poisoned"))?;
    if let Some(mut active) = active.take() {
        if let Some(task) = active.task.take() {
            task.abort();
        }
    }
    Ok(())
}

async fn invalidate_and_wait_for_prefetch(app_state: &AppState) -> Result<(), String> {
    invalidate_prefetch(app_state)?;
    let active = app_state
        .prefetch_task
        .lock()
        .map_err(|_| String::from("prefetch task lock is poisoned"))?
        .take();
    let Some(mut active) = active else {
        return Ok(());
    };
    let Some(mut task) = active.task.take() else {
        return Ok(());
    };
    match tokio::time::timeout(Duration::from_secs(3), &mut task).await {
        Ok(result) => result.map_err(|_| String::from("active prefetch task failed")),
        Err(_) => {
            task.abort();
            Err(String::from(
                "timed out waiting for the active prefetch to stop",
            ))
        }
    }
}

fn take_and_invalidate_prefetch(
    cache: &Mutex<Option<PrefetchEntry>>,
    generation: &AtomicU64,
    settings_generation: &AtomicU64,
    key: &PrefetchKey,
) -> Result<Option<voxgolem_core::assistant::Content>, String> {
    let mut cache = cache
        .lock()
        .map_err(|_| String::from("prefetch cache lock is poisoned"))?;
    let current_generation = generation.fetch_add(1, Ordering::SeqCst);
    Ok(cache
        .take()
        .filter(|entry| {
            entry.generation == current_generation
                && entry.settings_generation == settings_generation.load(Ordering::SeqCst)
                && entry.key == *key
        })
        .filter(|entry| !assistant_content_text(&entry.answer).trim().is_empty())
        .map(|entry| entry.answer))
}

fn prefetch_result_is_current(
    app_state: &AppState,
    key: &PrefetchKey,
    generation: u64,
    settings_generation: u64,
    cancelled: &AtomicBool,
) -> bool {
    let policy_is_current = app_state
        .conversation_session
        .lock()
        .map(|session| {
            !session.has_active_execution()
                && session.policy().instant_model == key.model
                && session.provider_history() == key.history
        })
        .unwrap_or(false);
    app_state.prefetch_generation.load(Ordering::SeqCst) == generation
        && app_state
            .assistant_settings_generation
            .load(Ordering::SeqCst)
            == settings_generation
        && !app_state.reset_in_progress.load(Ordering::Acquire)
        && !app_state
            .settings_publication_in_progress
            .load(Ordering::Acquire)
        && !cancelled.load(Ordering::SeqCst)
        && policy_is_current
}

fn queue_assistant_prefetch(
    app: &tauri::AppHandle,
    prompt: String,
    source: CompletionSource,
) -> Result<(), String> {
    let app_state = app.state::<AppState>();
    let _lifecycle = app_state
        .lifecycle_gate
        .lock()
        .map_err(|_| String::from("lifecycle gate is poisoned"))?;
    if app_state.reset_in_progress.load(Ordering::Acquire) {
        return Ok(());
    }
    if app_state
        .settings_publication_in_progress
        .load(Ordering::Acquire)
        || ensure_startup_ready_for_prompt(&app_state.startup_state).is_err()
    {
        return Ok(());
    }
    invalidate_prefetch(&app_state)?;
    let (key, settings_generation) = {
        let coordinator = app_state
            .conversation_session
            .lock()
            .map_err(|_| String::from("assistant coordinator lock is poisoned"))?;
        if coordinator.has_active_execution() || !coordinator.policy().prefetch_enabled {
            return Ok(());
        }
        (
            PrefetchKey {
                prompt,
                history: coordinator.provider_history(),
                model: coordinator.policy().instant_model,
            },
            app_state
                .assistant_settings_generation
                .load(Ordering::SeqCst),
        )
    };
    if let Some(expected_profile) = local_profile_for_model(key.model) {
        let selected_profile = *app_state
            .selected_response_profile
            .lock()
            .map_err(|_| String::from("selected response profile lock is poisoned"))?;
        if selected_profile != expected_profile {
            return Ok(());
        }
    }
    let generation = app_state
        .prefetch_generation
        .fetch_add(1, Ordering::SeqCst)
        .saturating_add(1);
    app_state
        .prefetch_cache
        .lock()
        .map_err(|_| String::from("prefetch cache lock is poisoned"))?
        .take();
    let app = app.clone();
    let cancelled = Arc::new(AtomicBool::new(false));
    let (cancellation_sender, _) = tokio::sync::watch::channel(false);
    let cancellation_signal = Arc::new(cancellation_sender);
    let local_cancellation = Arc::new(Mutex::new(None));
    app_state
        .prefetch_task
        .lock()
        .map_err(|_| String::from("prefetch task lock is poisoned"))?
        .replace(ActivePrefetch {
            generation,
            cancelled: Arc::clone(&cancelled),
            cancellation_signal: Arc::clone(&cancellation_signal),
            local_cancellation: Arc::clone(&local_cancellation),
            task: None,
        });
    let task_cancelled = Arc::clone(&cancelled);
    let task_cancellation_signal = Arc::clone(&cancellation_signal);
    let task_local_cancellation = Arc::clone(&local_cancellation);
    if app_state
        .assistant_settings_generation
        .load(Ordering::SeqCst)
        != settings_generation
    {
        if let Ok(mut active) = app_state.prefetch_task.lock() {
            if active
                .as_ref()
                .is_some_and(|active| active.generation == generation)
            {
                active.take();
            }
        }
        return Ok(());
    }
    let cleanup_permit = app_state.prefetch_cleanup.register();
    let task = tauri::async_runtime::spawn(async move {
        let _cleanup_permit = cleanup_permit;
        let started = Instant::now();
        let result = run_assistant_prefetch(
            &app,
            &key,
            generation,
            &task_cancelled,
            &task_cancellation_signal,
            &task_local_cancellation,
        )
        .await;
        let app_state = app.state::<AppState>();
        let current = if let Ok(_lifecycle) = app_state.lifecycle_gate.lock() {
            let current = prefetch_result_is_current(
                &app_state,
                &key,
                generation,
                settings_generation,
                &task_cancelled,
            );
            if current {
                if let (Ok(answer), Ok(mut cache)) = (&result, app_state.prefetch_cache.lock()) {
                    *cache = Some(PrefetchEntry {
                        generation,
                        settings_generation,
                        key: key.clone(),
                        answer: answer.clone(),
                    });
                }
            }
            current
        } else {
            false
        };
        let (provider, model, inference_provider) = instant_telemetry_identity(key.model);
        append_telemetry(
            &app_state.telemetry_sink,
            telemetry::TelemetryMetadata {
                schema_version: telemetry::SCHEMA_VERSION,
                timestamp_ms: current_time_ms().unwrap_or_default(),
                request_id: format!("prefetch-{generation}"),
                generation,
                input_source: match source {
                    CompletionSource::Typed => telemetry::InputSource::Text,
                    CompletionSource::Voice => telemetry::InputSource::Voice,
                },
                provider,
                model: model.to_string(),
                stage: telemetry::Stage::Prefetch,
                transport: telemetry::Transport::Http,
                inference_provider,
                speculative_origin: match source {
                    CompletionSource::Typed => telemetry::SpeculativeOrigin::User,
                    CompletionSource::Voice => telemetry::SpeculativeOrigin::System,
                },
                input_tokens: None,
                output_tokens: None,
                duration_ms: Some(started.elapsed().as_millis() as u64),
                status: if result.is_ok() && current {
                    telemetry::Status::Ok
                } else {
                    telemetry::Status::Error
                },
                error_category: if result.is_ok() && current {
                    telemetry::ErrorCategory::None
                } else {
                    telemetry::ErrorCategory::Internal
                },
            },
        );
        if let Ok(mut active) = app_state.prefetch_task.lock() {
            if active
                .as_ref()
                .is_some_and(|active| active.generation == generation)
            {
                active.take();
            }
        };
    });
    let mut active = app_state
        .prefetch_task
        .lock()
        .map_err(|_| String::from("prefetch task lock is poisoned"))?;
    if let Some(active) = active
        .as_mut()
        .filter(|active| active.generation == generation)
    {
        active.task = Some(task);
    }
    Ok(())
}

fn local_profile_for_model(
    model: voxgolem_core::assistant::InstantModel,
) -> Option<ResponseProfilePayload> {
    use voxgolem_core::assistant::InstantModel;
    match model {
        InstantModel::LocalFast => Some(ResponseProfilePayload::Fast),
        InstantModel::LocalQuality => Some(ResponseProfilePayload::Quality),
        InstantModel::CustomSolHigh
        | InstantModel::CustomLunaLow
        | InstantModel::OpenCodeSolHigh
        | InstantModel::OpenCodeLunaLow => None,
    }
}

#[derive(Debug)]
struct PreparedLocalInstantModel {
    profile: ResponseProfilePayload,
    policy: voxgolem_core::assistant::AssistantPolicy,
    settings: AssistantSettingsPayload,
}

fn prepare_local_instant_model(
    coordinator: &Mutex<voxgolem_core::conversation::ConversationSession>,
    lifecycle_gate: &Mutex<()>,
    settings_generation: &AtomicU64,
    expected_generation: u64,
    profile: ResponseProfilePayload,
) -> Result<PreparedLocalInstantModel, String> {
    prepare_local_instant_model_with(
        coordinator,
        lifecycle_gate,
        settings_generation,
        expected_generation,
        profile,
    )?
    .ok_or_else(|| String::from("assistant settings changed during profile switch"))
}

fn prepare_local_instant_model_with(
    coordinator: &Mutex<voxgolem_core::conversation::ConversationSession>,
    lifecycle_gate: &Mutex<()>,
    settings_generation: &AtomicU64,
    expected_generation: u64,
    profile: ResponseProfilePayload,
) -> Result<Option<PreparedLocalInstantModel>, String> {
    let mut preferences = {
        let _lifecycle = lifecycle_gate
            .lock()
            .map_err(|_| String::from("lifecycle gate is poisoned"))?;
        if settings_generation.load(Ordering::SeqCst) != expected_generation {
            return Ok(None);
        }
        let coordinator = coordinator
            .lock()
            .map_err(|_| String::from("assistant coordinator lock is poisoned"))?;
        if coordinator.has_active_execution() {
            return Err(String::from(
                "assistant settings cannot change while a prompt is active",
            ));
        }
        coordinator.policy().clone()
    };
    preferences.instant_model = local_instant_model(profile);
    let settings = AssistantSettingsPayload::from(&preferences);
    Ok(Some(PreparedLocalInstantModel {
        profile,
        policy: preferences,
        settings,
    }))
}

fn persist_prepared_local_instant_model_with<F>(
    prepared: PreparedLocalInstantModel,
    persist: F,
) -> Result<PreparedLocalInstantModel, String>
where
    F: FnOnce(ResponseProfilePayload, AssistantSettingsPayload) -> Result<(), String>,
{
    persist(prepared.profile, prepared.settings)?;
    Ok(prepared)
}

fn local_instant_model(profile: ResponseProfilePayload) -> voxgolem_core::assistant::InstantModel {
    match profile {
        ResponseProfilePayload::Fast => voxgolem_core::assistant::InstantModel::LocalFast,
        ResponseProfilePayload::Quality => voxgolem_core::assistant::InstantModel::LocalQuality,
    }
}

async fn run_assistant_prefetch(
    app: &tauri::AppHandle,
    key: &PrefetchKey,
    generation: u64,
    cancelled: &AtomicBool,
    cancellation_signal: &tokio::sync::watch::Sender<bool>,
    local_cancellation: &Mutex<Option<voxgolem_platform::llama_cpp::LlamaCppChatCancellation>>,
) -> Result<voxgolem_core::assistant::Content, String> {
    use voxgolem_core::assistant::InstantModel;
    if cancelled.load(Ordering::SeqCst) {
        return Err(String::from("prefetch cancelled"));
    }
    match key.model {
        InstantModel::LocalFast | InstantModel::LocalQuality => {
            let app = app.clone();
            let key = key.clone();
            let mut cancellation_receiver = cancellation_signal.subscribe();
            if *cancellation_receiver.borrow() {
                return Err(String::from("prefetch cancelled"));
            }
            let cancellation = voxgolem_platform::llama_cpp::LlamaCppChatCancellation::default();
            local_cancellation
                .lock()
                .map_err(|_| String::from("local prefetch cancellation lock is poisoned"))?
                .replace(cancellation.clone());
            if cancelled.load(Ordering::Acquire) {
                cancellation.cancel();
                local_cancellation
                    .lock()
                    .map_err(|_| String::from("local prefetch cancellation lock is poisoned"))?
                    .take();
                return Err(String::from("prefetch cancelled"));
            }
            let provider_cancellation = cancellation.clone();
            let provider = tauri::async_runtime::spawn_blocking(move || {
                let app_state = app.state::<AppState>();
                if app_state.prefetch_generation.load(Ordering::SeqCst) != generation {
                    return Err(String::from("prefetch cancelled"));
                }
                let _operation_guard =
                    lock_response_backend_operation(&app_state.response_backend_operation_lock)?;
                if app_state.prefetch_generation.load(Ordering::SeqCst) != generation {
                    return Err(String::from("prefetch cancelled"));
                }
                let expected_profile = local_profile_for_model(key.model)
                    .expect("local prefetch always has a local profile");
                if *app_state
                    .selected_response_profile
                    .lock()
                    .map_err(|_| String::from("selected response profile lock is poisoned"))?
                    != expected_profile
                {
                    return Err(String::from("selected local model is not loaded"));
                }
                let system_prompt = app_state
                    .llama_cpp_system_prompt
                    .as_deref()
                    .ok_or_else(|| String::from("SOUL.md is not loaded"))?;
                let conversation = assistant_history_as_llama(&key.history)?;
                let input = build_llama_prompt_input(system_prompt, &key.prompt, &conversation);
                let client = app_state
                    .llama_cpp_runtime
                    .lock()
                    .map_err(|_| String::from("local llama.cpp runtime lock is poisoned"))?
                    .as_ref()
                    .ok_or_else(|| String::from("local Gemma model is still warming up"))?
                    .client();
                let mut response_text = String::new();
                client
                    .chat_streaming(
                        &voxgolem_platform::llama_cpp::LlamaCppPrompt::new(input.user_prompt)
                            .with_system_prompt(system_prompt)
                            .with_max_tokens(LLAMA_CPP_MAX_TOKENS),
                        &provider_cancellation,
                        |delta| response_text.push_str(delta),
                    )
                    .map(|_| voxgolem_core::assistant::Content::Text(response_text))
                    .map_err(|error| format!("failed to prefetch local response: {error}"))
            });
            tokio::pin!(provider);
            let result = tokio::select! {
                biased;
                _ = cancellation_receiver.changed() => {
                    cancellation.cancel();
                    let _ = provider.await;
                    Err(String::from("prefetch cancelled"))
                }
                result = &mut provider => result
                    .map_err(|error| format!("local prefetch task failed: {error}"))?,
            };
            local_cancellation
                .lock()
                .map_err(|_| String::from("local prefetch cancellation lock is poisoned"))?
                .take();
            result
        }
        InstantModel::CustomSolHigh | InstantModel::CustomLunaLow => {
            let app_state = app.state::<AppState>();
            let config = app_state
                .runtime_config
                .as_ref()
                .and_then(|config| config.custom_openai.as_ref())
                .ok_or_else(|| String::from("Custom provider is not configured"))?;
            let model = if key.model == InstantModel::CustomSolHigh {
                voxgolem_platform::custom_openai::CustomOpenAiModel::SolHigh
            } else {
                voxgolem_platform::custom_openai::CustomOpenAiModel::LunaLow
            };
            let client = voxgolem_platform::custom_openai::CustomOpenAiClient::new(
                voxgolem_platform::custom_openai::CustomOpenAiConfig {
                    endpoint: config.endpoint.clone(),
                    auth_path: config.auth_path.clone(),
                    model,
                    ..Default::default()
                },
            )
            .map_err(|error| error.to_string())?;
            let prefetch_prompt = voxgolem_platform::custom_openai::CustomOpenAiPrompt {
                session_id: format!("prefetch-{generation}"),
                prompt: key.prompt.clone(),
                history: custom_history(&key.history),
            };
            let mut cancellation_receiver = cancellation_signal.subscribe();
            if *cancellation_receiver.borrow() {
                return Err(String::from("prefetch cancelled"));
            }
            tokio::select! {
                biased;
                _ = cancellation_receiver.changed() => Err(String::from("prefetch cancelled")),
                result = client.respond(&prefetch_prompt, |_| {}) => {
                    result
                        .map(|response| match response.content_type {
                            voxgolem_platform::custom_openai::CustomOpenAiContentType::OutputText => {
                                voxgolem_core::assistant::Content::Text(response.text)
                            }
                            voxgolem_platform::custom_openai::CustomOpenAiContentType::Refusal => {
                                voxgolem_core::assistant::Content::Refusal(response.text)
                            }
                        })
                        .map_err(|error| error.to_string())
                },
            }
        }
        InstantModel::OpenCodeSolHigh | InstantModel::OpenCodeLunaLow => {
            let app_state = app.state::<AppState>();
            let base_client = app_state
                .opencode_server
                .lock()
                .map_err(|_| String::from("opencode server lock is poisoned"))?
                .as_ref()
                .map(voxgolem_platform::opencode::OpencodeServer::client)
                .ok_or_else(|| String::from("OpenCode server is not available"))?;
            let client = create_transient_opencode_client(
                &base_client,
                cancellation_signal,
                &app_state.prefetch_cleanup,
            )
            .await?;
            let result = collect_opencode_agent(
                &client.client,
                &format!("prefetch-{generation}"),
                &render_provider_prompt(&key.history, &key.prompt),
                if key.model == InstantModel::OpenCodeSolHigh {
                    voxgolem_platform::opencode::OpencodeModel::Gpt56SolHigh
                } else {
                    voxgolem_platform::opencode::OpencodeModel::Gpt56LunaLow
                },
                voxgolem_platform::opencode::OpencodeToolPolicy::AnswerOnly,
                cancelled,
                cancellation_signal,
            )
            .await;
            client.finish().await;
            result.map(|answer| voxgolem_core::assistant::Content::Text(answer.text))
        }
    }
}

fn assistant_history_as_llama(
    history: &[voxgolem_core::assistant::ConversationTurn],
) -> Result<Vec<LlamaConversationTurn>, String> {
    let mut conversation = Vec::new();
    for turns in history.chunks_exact(2) {
        let [user, assistant] = turns else {
            unreachable!("chunks_exact always yields two turns")
        };
        if user.role != voxgolem_core::assistant::Role::User
            || assistant.role != voxgolem_core::assistant::Role::Assistant
        {
            return Err(String::from("canonical conversation roles are invalid"));
        }
        conversation.push(LlamaConversationTurn {
            user: assistant_content_text(&user.content).to_string(),
            assistant: assistant_content_text(&assistant.content).to_string(),
        });
    }
    Ok(conversation)
}

#[derive(Clone)]
struct StartupSnapshot {
    cue_asset_paths: CueAssetPathsPayload,
    voice_input_available: bool,
    voice_input_error: Option<String>,
    silence_timeout_ms: u64,
    tts_enabled: bool,
    tts_output_gain_db: f32,
    supported_response_profiles: Vec<ResponseProfilePayload>,
    capabilities: Vec<CapabilityPayload>,
}

fn startup_ready_state_from_snapshot(
    startup_snapshot: &StartupSnapshot,
    selected_response_profile: ResponseProfilePayload,
) -> StartupStatePayload {
    StartupStatePayload::Ready {
        cue_asset_paths: startup_snapshot.cue_asset_paths.clone(),
        runtime_phase: RuntimePhasePayload::Sleeping,
        voice_input_available: startup_snapshot.voice_input_available,
        voice_input_error: startup_snapshot.voice_input_error.clone(),
        silence_timeout_ms: startup_snapshot.silence_timeout_ms,
        selected_response_profile,
        supported_response_profiles: startup_snapshot.supported_response_profiles.clone(),
        prompt_cancellation_available: true,
        tts_enabled: startup_snapshot.tts_enabled,
        tts_output_gain_db: startup_snapshot.tts_output_gain_db,
        capabilities: startup_snapshot.capabilities.clone(),
    }
}

fn startup_warming_state_from_snapshot(
    startup_snapshot: &StartupSnapshot,
    profile: ResponseProfilePayload,
) -> StartupStatePayload {
    let mut capabilities = startup_snapshot.capabilities.clone();
    mark_capability(
        &mut capabilities,
        if profile == ResponseProfilePayload::Quality {
            "local_quality"
        } else {
            "local_fast"
        },
        CapabilityStatePayload::Warming,
        String::from("local model is loading"),
    );
    StartupStatePayload::WarmingModel {
        cue_asset_paths: startup_snapshot.cue_asset_paths.clone(),
        runtime_phase: RuntimePhasePayload::Initializing,
        voice_input_available: startup_snapshot.voice_input_available,
        voice_input_error: startup_snapshot.voice_input_error.clone(),
        silence_timeout_ms: startup_snapshot.silence_timeout_ms,
        message: String::from("Loading local Gemma model..."),
        selected_response_profile: profile,
        supported_response_profiles: startup_snapshot.supported_response_profiles.clone(),
        prompt_cancellation_available: false,
        tts_enabled: startup_snapshot.tts_enabled,
        tts_output_gain_db: startup_snapshot.tts_output_gain_db,
        capabilities,
    }
}

fn merge_profile_startup_publication(
    current: &mut StartupStatePayload,
    profile_state: StartupStatePayload,
) {
    let StartupStatePayload::Ready {
        selected_response_profile,
        supported_response_profiles,
        capabilities: profile_capabilities,
        ..
    } = profile_state
    else {
        return;
    };
    let (
        cue_asset_paths,
        voice_input_available,
        voice_input_error,
        silence_timeout_ms,
        tts_enabled,
        tts_output_gain_db,
        mut capabilities,
    ) = match current.clone() {
        StartupStatePayload::WarmingModel {
            cue_asset_paths,
            voice_input_available,
            voice_input_error,
            silence_timeout_ms,
            tts_enabled,
            tts_output_gain_db,
            capabilities,
            ..
        }
        | StartupStatePayload::Ready {
            cue_asset_paths,
            voice_input_available,
            voice_input_error,
            silence_timeout_ms,
            tts_enabled,
            tts_output_gain_db,
            capabilities,
            ..
        } => (
            cue_asset_paths,
            voice_input_available,
            voice_input_error,
            silence_timeout_ms,
            tts_enabled,
            tts_output_gain_db,
            capabilities,
        ),
        StartupStatePayload::Error { .. } => return,
    };
    for profile_capability in profile_capabilities
        .into_iter()
        .filter(|capability| matches!(capability.id, "local_fast" | "local_quality"))
    {
        if let Some(capability) = capabilities
            .iter_mut()
            .find(|capability| capability.id == profile_capability.id)
        {
            *capability = profile_capability;
        }
    }
    *current = StartupStatePayload::Ready {
        cue_asset_paths,
        runtime_phase: RuntimePhasePayload::Sleeping,
        voice_input_available,
        voice_input_error,
        silence_timeout_ms,
        selected_response_profile,
        supported_response_profiles,
        prompt_cancellation_available: true,
        tts_enabled,
        tts_output_gain_db,
        capabilities,
    };
}

fn startup_snapshot_for_profile_switch(
    startup_state: &Arc<Mutex<StartupStatePayload>>,
    profile: ResponseProfilePayload,
    supported_response_profiles: Vec<ResponseProfilePayload>,
) -> Result<StartupSnapshot, String> {
    let startup_state = startup_state
        .lock()
        .map_err(|_| String::from("startup state lock should not be poisoned"))?;

    match &*startup_state {
        StartupStatePayload::WarmingModel {
            cue_asset_paths,
            voice_input_available,
            voice_input_error,
            silence_timeout_ms,
            tts_enabled,
            tts_output_gain_db,
            capabilities,
            ..
        }
        | StartupStatePayload::Ready {
            cue_asset_paths,
            voice_input_available,
            voice_input_error,
            silence_timeout_ms,
            tts_enabled,
            tts_output_gain_db,
            capabilities,
            ..
        } => Ok(StartupSnapshot {
            cue_asset_paths: cue_asset_paths.clone(),
            voice_input_available: *voice_input_available,
            voice_input_error: voice_input_error.clone(),
            silence_timeout_ms: *silence_timeout_ms,
            tts_enabled: *tts_enabled,
            tts_output_gain_db: *tts_output_gain_db,
            supported_response_profiles,
            capabilities: capabilities.clone(),
        }),
        StartupStatePayload::Error { .. } => Err(format!(
            "cannot switch response profile `{}` while startup is in error",
            profile.as_str()
        )),
    }
}

fn supported_response_profiles(
    backend: &voxgolem_core::config::ResponseBackendConfig,
) -> Vec<ResponseProfilePayload> {
    let mut profiles = if matches!(
        backend,
        voxgolem_core::config::ResponseBackendConfig::Unconfigured
    ) {
        Vec::new()
    } else {
        vec![ResponseProfilePayload::Fast]
    };
    if let voxgolem_core::config::ResponseBackendConfig::LlamaCpp {
        quality_model_path: Some(_),
        ..
    } = backend
    {
        profiles.push(ResponseProfilePayload::Quality);
    }

    profiles
}

fn default_response_profile() -> ResponseProfilePayload {
    ResponseProfilePayload::Fast
}

fn default_ui_text_size() -> UiTextSizePayload {
    UiTextSizePayload::Medium
}

fn default_ui_theme() -> UiThemePayload {
    UiThemePayload::Dark
}

fn resolved_ui_preferences() -> UiPreferencesState {
    let text_size = load_persisted_ui_text_size().unwrap_or_else(|error| {
        eprintln!("failed to read UI text size: {error}");
        None
    });
    let theme = load_persisted_ui_theme().unwrap_or_else(|error| {
        eprintln!("failed to read UI theme: {error}");
        None
    });
    UiPreferencesState {
        theme: theme.unwrap_or_else(default_ui_theme),
        text_size: text_size.unwrap_or_else(default_ui_text_size),
    }
}

fn model_path_for_profile<'a>(
    profile: ResponseProfilePayload,
    fast_model_path: &'a Path,
    quality_model_path: Option<&'a PathBuf>,
) -> Result<&'a Path, String> {
    match profile {
        ResponseProfilePayload::Fast => Ok(fast_model_path),
        ResponseProfilePayload::Quality => quality_model_path
            .map(PathBuf::as_path)
            .ok_or_else(|| String::from("response profile `quality` is not supported")),
    }
}

fn resolve_selected_response_profile(
    supported_response_profiles: &[ResponseProfilePayload],
) -> ResponseProfilePayload {
    let default_profile = default_response_profile();
    let persisted_profile = load_selected_response_profile().unwrap_or_else(|error| {
        eprintln!("failed to read response profile state: {error}");
        None
    });

    let selected = persisted_profile
        .filter(|profile| supported_response_profiles.contains(profile))
        .unwrap_or(default_profile);

    if persisted_profile != Some(selected) {
        if let Err(error) = persist_selected_response_profile(selected) {
            eprintln!("failed to persist response profile state: {error}");
        }
    }

    selected
}

fn resolve_effective_tts_enabled(default_enabled: bool, tts_available: bool) -> bool {
    if !tts_available {
        return false;
    }
    let persisted_tts_enabled = load_persisted_tts_enabled().unwrap_or_else(|error| {
        eprintln!("failed to read tts state: {error}");
        None
    });

    persisted_tts_enabled.unwrap_or(default_enabled)
}

fn response_profile_state_path() -> Result<PathBuf, String> {
    let config_path = application_config_path()?;

    Ok(config_path.with_file_name(RESPONSE_PROFILE_STATE_FILE))
}

fn runtime_log_path() -> Result<PathBuf, String> {
    let config_path = application_config_path()?;

    Ok(config_path
        .with_file_name(RUNTIME_LOG_DIR)
        .join(RUNTIME_LOG_FILE))
}

fn application_config_path() -> Result<PathBuf, String> {
    #[cfg(test)]
    if let Some(appdata) = std::env::var_os("APPDATA") {
        return Ok(PathBuf::from(appdata).join("VoxGolem").join("config.toml"));
    }
    voxgolem_core::config::default_config_path()
        .map_err(|error| format!("failed to resolve application config path: {error}"))
}

fn append_runtime_log_line(enabled: bool, subsystem: &str, message: &str) -> Result<(), String> {
    if !enabled {
        return Ok(());
    }

    let log_path = runtime_log_path()?;
    if let Some(parent) = log_path.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            format!(
                "failed to create runtime log directory {}: {error}",
                parent.display()
            )
        })?;
    }

    let timestamp_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0);

    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .map_err(|error| format!("failed to open runtime log {}: {error}", log_path.display()))?;

    writeln!(
        file,
        "{timestamp_ms} [{}] {}",
        sanitize_log_subsystem(subsystem),
        sanitize_log_message(message)
    )
    .map_err(|error| {
        format!(
            "failed to append runtime log {}: {error}",
            log_path.display()
        )
    })
}

fn append_tts_runtime_log_line(enabled: bool, message: &str) -> Result<(), String> {
    append_runtime_log_line(enabled, "tts", message)
}

fn sanitize_log_subsystem(subsystem: &str) -> String {
    let sanitized: String = subsystem
        .chars()
        .filter(|character| {
            character.is_ascii_alphanumeric() || *character == '_' || *character == '-'
        })
        .take(32)
        .collect();

    if sanitized.is_empty() {
        return String::from("runtime");
    }

    sanitized
}

fn sanitize_log_message(message: &str) -> String {
    let mut characters = message.chars();
    let mut sanitized = String::new();

    for character in characters.by_ref().take(RUNTIME_LOG_MESSAGE_MAX_CHARS) {
        match character {
            '\r' => sanitized.push_str("\\r"),
            '\n' => sanitized.push_str("\\n"),
            _ => sanitized.push(character),
        }
    }

    if characters.next().is_some() {
        sanitized.push_str("…[truncated]");
    }

    sanitized
}

fn log_tts_runtime_event(enabled: bool, message: &str) {
    if let Err(error) = append_tts_runtime_log_line(enabled, message) {
        eprintln!("failed to append tts runtime log: {error}");
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct PersistedState {
    selected_response_profile: Option<ResponseProfilePayload>,
    tts_enabled: Option<bool>,
    ui_text_size: Option<UiTextSizePayload>,
    ui_theme: Option<UiThemePayload>,
    assistant_settings: Option<AssistantSettingsPayload>,
}

fn parse_persisted_state(contents: &str) -> Result<PersistedState, String> {
    let mut state = PersistedState::default();
    let mut assistant_settings = AssistantSettingsPayload::default();
    let mut assistant_settings_seen = false;

    for raw_line in contents.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with('[') {
            continue;
        }

        if let Some(value) = line.strip_prefix("selected_response_profile") {
            let Some(value) = value.trim_start().strip_prefix('=') else {
                return Err(String::from(
                    "invalid state.toml: expected `selected_response_profile = \"...\"`",
                ));
            };

            let value = value.trim().trim_matches('"').to_ascii_lowercase();
            state.selected_response_profile = match value.as_str() {
                "fast" => Some(ResponseProfilePayload::Fast),
                "quality" => Some(ResponseProfilePayload::Quality),
                _ => {
                    return Err(format!(
                        "invalid state.toml: unsupported selected_response_profile `{value}`"
                    ));
                }
            };
            continue;
        }

        if let Some(value) = line.strip_prefix("tts_enabled") {
            let Some(value) = value.trim_start().strip_prefix('=') else {
                return Err(String::from(
                    "invalid state.toml: expected `tts_enabled = true|false`",
                ));
            };

            let value = value.trim().trim_matches('"').to_ascii_lowercase();
            state.tts_enabled = match value.as_str() {
                "true" => Some(true),
                "false" => Some(false),
                _ => {
                    return Err(format!(
                        "invalid state.toml: unsupported tts_enabled `{value}`"
                    ));
                }
            };
            continue;
        }

        if let Some(value) = line.strip_prefix("ui_text_size") {
            let Some(value) = value.trim_start().strip_prefix('=') else {
                return Err(String::from(
                    "invalid state.toml: expected `ui_text_size = \"...\"`",
                ));
            };

            let value = value.trim().trim_matches('"').to_ascii_lowercase();
            state.ui_text_size = match value.as_str() {
                "small" => Some(UiTextSizePayload::Small),
                "medium" => Some(UiTextSizePayload::Medium),
                "large" => Some(UiTextSizePayload::Large),
                "extra_large" => Some(UiTextSizePayload::ExtraLarge),
                _ => {
                    return Err(format!(
                        "invalid state.toml: unsupported ui_text_size `{value}`"
                    ));
                }
            };
            continue;
        }

        if let Some(value) = line.strip_prefix("ui_theme") {
            let Some(value) = value.trim_start().strip_prefix('=') else {
                return Err(String::from(
                    "invalid state.toml: expected `ui_theme = \"...\"`",
                ));
            };

            let value = value.trim().trim_matches('"').to_ascii_lowercase();
            state.ui_theme = match value.as_str() {
                "light" => Some(UiThemePayload::Light),
                "dark" => Some(UiThemePayload::Dark),
                _ => {
                    return Err(format!(
                        "invalid state.toml: unsupported ui_theme `{value}`"
                    ));
                }
            };
            continue;
        }

        if let Some(value) = line.strip_prefix("assistant_instant") {
            assistant_settings.instant = parse_instant_choice(value)?;
            assistant_settings_seen = true;
            continue;
        }
        if let Some(value) = line.strip_prefix("assistant_deep_enabled") {
            assistant_settings.deep_enabled =
                parse_persisted_bool(value, "assistant_deep_enabled")?;
            assistant_settings_seen = true;
            continue;
        }
        if let Some(value) = line.strip_prefix("assistant_deep") {
            assistant_settings.deep = parse_agent_choice(value, "assistant_deep")?;
            assistant_settings_seen = true;
            continue;
        }
        if let Some(value) = line.strip_prefix("assistant_review_enabled") {
            assistant_settings.review_enabled =
                parse_persisted_bool(value, "assistant_review_enabled")?;
            assistant_settings_seen = true;
            continue;
        }
        if let Some(value) = line.strip_prefix("assistant_review") {
            assistant_settings.review = parse_agent_choice(value, "assistant_review")?;
            assistant_settings_seen = true;
            continue;
        }
        if let Some(value) = line.strip_prefix("assistant_prefetch") {
            assistant_settings.prefetch = parse_persisted_bool(value, "assistant_prefetch")?;
            assistant_settings_seen = true;
            continue;
        }
        if let Some(value) = line.strip_prefix("assistant_completion") {
            assistant_settings.completion = parse_persisted_bool(value, "assistant_completion")?;
            assistant_settings_seen = true;
        }
    }

    state.assistant_settings = assistant_settings_seen.then_some(assistant_settings);

    Ok(state)
}

fn persisted_value<'a>(value: &'a str, field: &str) -> Result<&'a str, String> {
    value
        .trim_start()
        .strip_prefix('=')
        .map(|value| value.trim().trim_matches('"'))
        .ok_or_else(|| format!("invalid state.toml: expected `{field} = ...`"))
}

fn parse_instant_choice(value: &str) -> Result<InstantChoicePayload, String> {
    let value = persisted_value(value, "assistant_instant")?;
    match value {
        "local-fast" => Ok(InstantChoicePayload::LocalFast),
        "local-quality" => Ok(InstantChoicePayload::LocalQuality),
        "custom-sol-high" => Ok(InstantChoicePayload::CustomSolHigh),
        "custom-luna-low" => Ok(InstantChoicePayload::CustomLunaLow),
        "opencode-sol-high" => Ok(InstantChoicePayload::OpenCodeSolHigh),
        "opencode-luna-low" => Ok(InstantChoicePayload::OpenCodeLunaLow),
        _ => Err(format!(
            "invalid state.toml: unsupported assistant_instant `{value}`"
        )),
    }
}

fn parse_agent_choice(value: &str, field: &str) -> Result<AgentChoicePayload, String> {
    let value = persisted_value(value, field)?;
    match value {
        "custom-sol-high" => Ok(AgentChoicePayload::CustomSolHigh),
        "custom-luna-low" => Ok(AgentChoicePayload::CustomLunaLow),
        "opencode-sol-high" => Ok(AgentChoicePayload::OpenCodeSolHigh),
        "opencode-luna-low" => Ok(AgentChoicePayload::OpenCodeLunaLow),
        _ => Err(format!("invalid state.toml: unsupported {field} `{value}`")),
    }
}

fn parse_persisted_bool(value: &str, field: &str) -> Result<bool, String> {
    match persisted_value(value, field)? {
        "true" => Ok(true),
        "false" => Ok(false),
        value => Err(format!("invalid state.toml: unsupported {field} `{value}`")),
    }
}

fn load_persisted_state() -> Result<PersistedState, String> {
    let state_path = response_profile_state_path()?;
    let contents = match fs::read_to_string(&state_path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(PersistedState::default()),
        Err(error) => {
            return Err(format!(
                "failed to read response profile state {}: {error}",
                state_path.display()
            ));
        }
    };

    parse_persisted_state(&contents)
}

fn lock_persisted_state_file(state_path: &Path) -> std::io::Result<File> {
    let lock_file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(state_path.with_extension("lock"))?;
    lock_file.lock()?;
    Ok(lock_file)
}

fn is_persisted_state_temp(file_name: &std::ffi::OsStr, state_path: &Path) -> bool {
    let prefix_path = state_path.with_extension("tmp-");
    let Some(prefix) = prefix_path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    let Some(file_name) = file_name.to_str() else {
        return false;
    };
    let Some(pid) = file_name.strip_prefix(prefix) else {
        return false;
    };
    !pid.is_empty() && pid.bytes().all(|byte| byte.is_ascii_digit())
}

fn cleanup_orphan_state_temps() -> Result<(), String> {
    cleanup_orphan_state_temps_with(|| {})
}

fn cleanup_orphan_state_temps_with(before_lock: impl FnOnce()) -> Result<(), String> {
    let state_path = response_profile_state_path()?;
    let Some(parent) = state_path.parent() else {
        return Ok(());
    };
    before_lock();
    let _lock = match lock_persisted_state_file(&state_path) {
        Ok(lock) => lock,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(format!(
                "failed to lock persisted state {}: {error}",
                state_path.display()
            ));
        }
    };
    let entries = match fs::read_dir(parent) {
        Ok(entries) => entries,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(format!(
                "failed to inspect persisted state directory {}: {error}",
                parent.display()
            ));
        }
    };
    for entry in entries {
        let entry = entry.map_err(|error| {
            format!(
                "failed to inspect persisted state directory {}: {error}",
                parent.display()
            )
        })?;
        if is_persisted_state_temp(&entry.file_name(), &state_path) {
            fs::remove_file(entry.path()).map_err(|error| {
                format!(
                    "failed to remove orphan persisted state temporary {}: {error}",
                    entry.path().display()
                )
            })?;
        }
    }
    Ok(())
}

fn persist_state(state: PersistedState) -> Result<(), String> {
    let state_path = response_profile_state_path()?;
    if let Some(parent) = state_path.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            format!(
                "failed to create response profile state directory {}: {error}",
                parent.display()
            )
        })?;
    }

    let mut lines = Vec::<String>::new();
    if let Some(profile) = state.selected_response_profile {
        lines.push(format!(
            "selected_response_profile = \"{}\"",
            profile.as_str()
        ));
    }
    if let Some(tts_enabled) = state.tts_enabled {
        lines.push(format!("tts_enabled = {tts_enabled}"));
    }
    if let Some(ui_text_size) = state.ui_text_size {
        lines.push(format!("ui_text_size = \"{}\"", ui_text_size.as_str()));
    }
    if let Some(ui_theme) = state.ui_theme {
        lines.push(format!("ui_theme = \"{}\"", ui_theme.as_str()));
    }
    if let Some(settings) = state.assistant_settings {
        lines.extend([
            format!("assistant_instant = \"{}\"", settings.instant.as_str()),
            format!("assistant_deep = \"{}\"", settings.deep.as_str()),
            format!("assistant_review = \"{}\"", settings.review.as_str()),
            format!("assistant_deep_enabled = {}", settings.deep_enabled),
            format!("assistant_review_enabled = {}", settings.review_enabled),
            format!("assistant_prefetch = {}", settings.prefetch),
            format!("assistant_completion = {}", settings.completion),
        ]);
    }

    let contents = if lines.is_empty() {
        String::new()
    } else {
        format!("{}\n", lines.join("\n"))
    };

    let result = atomic_replace_state_file(&state_path, contents.as_bytes());
    if let Err(error) = result {
        return Err(format!(
            "failed to write response profile state {}: {error}",
            state_path.display()
        ));
    }
    Ok(())
}

fn atomic_replace_state_file(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    let _lock = lock_persisted_state_file(path)?;
    let temporary_path = path.with_extension(format!("tmp-{}", std::process::id()));
    let result = File::create(&temporary_path)
        .and_then(|mut file| {
            file.write_all(contents)?;
            file.sync_all()
        })
        .and_then(|_| {
            #[cfg(not(windows))]
            {
                fs::rename(&temporary_path, path)
            }
            #[cfg(windows)]
            {
                let source = std::ffi::OsStr::new(&temporary_path)
                    .encode_wide()
                    .chain(std::iter::once(0))
                    .collect::<Vec<_>>();
                let destination = std::ffi::OsStr::new(path)
                    .encode_wide()
                    .chain(std::iter::once(0))
                    .collect::<Vec<_>>();
                let result = unsafe {
                    MoveFileExW(
                        source.as_ptr(),
                        destination.as_ptr(),
                        MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
                    )
                };
                if result == 0 {
                    Err(std::io::Error::last_os_error())
                } else {
                    Ok(())
                }
            }
        });
    if result.is_err() {
        let _ = fs::remove_file(&temporary_path);
    }
    result
}

fn load_selected_response_profile() -> Result<Option<ResponseProfilePayload>, String> {
    Ok(load_persisted_state()?.selected_response_profile)
}

fn persist_selected_response_profile(profile: ResponseProfilePayload) -> Result<(), String> {
    let _guard = PERSISTED_STATE_LOCK
        .lock()
        .map_err(|_| String::from("persisted state lock is poisoned"))?;
    let mut persisted = load_persisted_state()?;
    persisted.selected_response_profile = Some(profile);
    persist_state(persisted)
}

fn persist_profile_and_assistant_settings(
    profile: ResponseProfilePayload,
    settings: AssistantSettingsPayload,
) -> Result<(), String> {
    let _guard = PERSISTED_STATE_LOCK
        .lock()
        .map_err(|_| String::from("persisted state lock is poisoned"))?;
    let mut persisted = load_persisted_state()?;
    persisted.selected_response_profile = Some(profile);
    persisted.assistant_settings = Some(settings);
    persist_state(persisted)
}

fn load_persisted_tts_enabled() -> Result<Option<bool>, String> {
    Ok(load_persisted_state()?.tts_enabled)
}

fn persist_tts_enabled(enabled: bool) -> Result<(), String> {
    let _guard = PERSISTED_STATE_LOCK
        .lock()
        .map_err(|_| String::from("persisted state lock is poisoned"))?;
    let mut persisted = load_persisted_state()?;
    persisted.tts_enabled = Some(enabled);
    persist_state(persisted)
}

fn load_persisted_ui_text_size() -> Result<Option<UiTextSizePayload>, String> {
    Ok(load_persisted_state()?.ui_text_size)
}

fn persist_ui_text_size(text_size: UiTextSizePayload) -> Result<(), String> {
    let _guard = PERSISTED_STATE_LOCK
        .lock()
        .map_err(|_| String::from("persisted state lock is poisoned"))?;
    let mut persisted = load_persisted_state()?;
    persisted.ui_text_size = Some(text_size);
    persist_state(persisted)
}

fn load_persisted_ui_theme() -> Result<Option<UiThemePayload>, String> {
    Ok(load_persisted_state()?.ui_theme)
}

fn persist_ui_theme(theme: UiThemePayload) -> Result<(), String> {
    let _guard = PERSISTED_STATE_LOCK
        .lock()
        .map_err(|_| String::from("persisted state lock is poisoned"))?;
    let mut persisted = load_persisted_state()?;
    persisted.ui_theme = Some(theme);
    persist_state(persisted)
}

fn load_assistant_settings() -> Result<Option<AssistantSettingsPayload>, String> {
    Ok(load_persisted_state()?.assistant_settings)
}

fn persist_assistant_settings(settings: AssistantSettingsPayload) -> Result<(), String> {
    let _guard = PERSISTED_STATE_LOCK
        .lock()
        .map_err(|_| String::from("persisted state lock is poisoned"))?;
    let mut persisted = load_persisted_state()?;
    persisted.assistant_settings = Some(settings);
    persist_state(persisted)
}

fn resolve_assistant_settings(
    config: &voxgolem_core::config::RuntimeConfig,
    capabilities: &[CapabilityPayload],
    selected_profile: ResponseProfilePayload,
) -> AssistantSettingsPayload {
    let default_agent = if capability_available(capabilities, "opencode") {
        AgentChoicePayload::OpenCodeSolHigh
    } else {
        AgentChoicePayload::CustomSolHigh
    };
    let defaults = AssistantSettingsPayload {
        instant: match &config.response_backend {
            voxgolem_core::config::ResponseBackendConfig::LlamaCpp { .. } => match selected_profile
            {
                ResponseProfilePayload::Fast => InstantChoicePayload::LocalFast,
                ResponseProfilePayload::Quality => InstantChoicePayload::LocalQuality,
            },
            voxgolem_core::config::ResponseBackendConfig::Opencode { .. } => {
                InstantChoicePayload::OpenCodeSolHigh
            }
            voxgolem_core::config::ResponseBackendConfig::Unconfigured
                if capability_available(capabilities, "custom_provider") =>
            {
                InstantChoicePayload::CustomSolHigh
            }
            voxgolem_core::config::ResponseBackendConfig::Unconfigured => {
                InstantChoicePayload::LocalFast
            }
        },
        deep: default_agent,
        review: default_agent,
        ..AssistantSettingsPayload::default()
    };
    let persisted = load_assistant_settings().unwrap_or_else(|error| {
        eprintln!("failed to read assistant settings: {error}");
        None
    });
    let mut resolved = persisted.unwrap_or(defaults);
    if !capability_available(capabilities, resolved.instant.capability_id()) {
        resolved.instant = defaults.instant;
    }
    if !capability_available(capabilities, resolved.deep.capability_id()) {
        resolved.deep = defaults.deep;
        resolved.deep_enabled = false;
    }
    if !capability_available(capabilities, resolved.review.capability_id()) {
        resolved.review = defaults.review;
        resolved.review_enabled = false;
    }
    if matches!(
        resolved.instant,
        InstantChoicePayload::LocalFast | InstantChoicePayload::LocalQuality
    ) {
        resolved.instant = match selected_profile {
            ResponseProfilePayload::Fast => InstantChoicePayload::LocalFast,
            ResponseProfilePayload::Quality => InstantChoicePayload::LocalQuality,
        };
    }
    if !capabilities.iter().any(|capability| {
        capability.id == "qwen_prediction"
            && matches!(
                capability.state,
                CapabilityStatePayload::Available | CapabilityStatePayload::Warming
            )
    }) {
        resolved.completion = false;
    }
    if persisted != Some(resolved) {
        if let Err(error) = persist_assistant_settings(resolved) {
            eprintln!("failed to persist resolved assistant settings: {error}");
        }
    }
    resolved
}

fn capability_available(capabilities: &[CapabilityPayload], id: &str) -> bool {
    capabilities.iter().any(|capability| {
        capability.id == id && capability.state == CapabilityStatePayload::Available
    })
}

fn fail_startup_capability(
    lifecycle_gate: &Mutex<()>,
    startup_state: &Arc<Mutex<StartupStatePayload>>,
    capability_id: &str,
    reason: String,
) {
    let Ok(_lifecycle) = lifecycle_gate.lock() else {
        return;
    };
    if let Ok(mut guard) = startup_state.lock() {
        let capabilities = match &mut *guard {
            StartupStatePayload::WarmingModel { capabilities, .. }
            | StartupStatePayload::Ready { capabilities, .. } => capabilities,
            StartupStatePayload::Error { .. } => return,
        };
        if let Some(capability) = capabilities
            .iter_mut()
            .find(|item| item.id == capability_id)
        {
            capability.state = CapabilityStatePayload::Failed;
            capability.reason = reason;
            capability.actual_provider = None;
        }
    }
}

fn set_startup_capability_provider(
    lifecycle_gate: &Mutex<()>,
    startup_state: &Arc<Mutex<StartupStatePayload>>,
    capability_id: &str,
    actual_provider: &'static str,
) {
    let Ok(_lifecycle) = lifecycle_gate.lock() else {
        return;
    };
    if let Ok(mut guard) = startup_state.lock() {
        let capabilities = match &mut *guard {
            StartupStatePayload::WarmingModel { capabilities, .. }
            | StartupStatePayload::Ready { capabilities, .. } => capabilities,
            StartupStatePayload::Error { .. } => return,
        };
        if let Some(capability) = capabilities
            .iter_mut()
            .find(|capability| capability.id == capability_id)
        {
            capability.state = CapabilityStatePayload::Available;
            capability.reason = String::from("ready");
            capability.actual_provider = Some(actual_provider);
        }
    }
}

fn repair_tts_capability(
    capabilities: &mut [CapabilityPayload],
    enabled: bool,
    runtime: Option<&tts::LocalTtsRuntime>,
) {
    let actual_provider = runtime
        .and_then(tts::LocalTtsRuntime::actual_provider)
        .map(|provider| match provider {
            tts::TtsActualProvider::Cuda => "cuda",
            tts::TtsActualProvider::Cpu => "cpu",
        });
    repair_tts_capability_from_status(capabilities, enabled, runtime.is_some(), actual_provider);
}

fn repair_tts_capability_from_status(
    capabilities: &mut [CapabilityPayload],
    enabled: bool,
    runtime_available: bool,
    actual_provider: Option<&'static str>,
) {
    if let Some(capability) = capabilities.iter_mut().find(|item| item.id == "tts") {
        capability.actual_provider = enabled.then_some(actual_provider).flatten();
        if enabled && runtime_available {
            capability.state = CapabilityStatePayload::Available;
            capability.reason = String::from("ready");
        }
    }
}

const CAPABILITY_IDS: [&str; 11] = [
    "custom_provider",
    "opencode",
    "local_fast",
    "local_quality",
    "qwen_prediction",
    "wake_word",
    "vad",
    "parakeet",
    "tts",
    "deep",
    "review",
];

fn configured_capabilities(
    config: &voxgolem_core::config::RuntimeConfig,
) -> Vec<CapabilityPayload> {
    CAPABILITY_IDS
        .into_iter()
        .map(|id| {
            let issue = config.capability_issues.iter().find(|issue| {
                issue.capability == id
                    || (issue.capability == "response_provider"
                        && matches!(
                            id,
                            "custom_provider" | "opencode" | "local_fast" | "local_quality"
                        ))
            });
            let opencode_configured = config
                .opencode
                .as_ref()
                .is_some_and(|opencode| opencode.path.is_file());
            let custom_configured = config
                .custom_openai
                .as_ref()
                .is_some_and(|custom| custom.auth_path.is_file())
                && !config
                    .capability_issues
                    .iter()
                    .any(|issue| issue.capability == "custom_provider");
            let configured = match id {
                "custom_provider" => custom_configured,
                "opencode" => opencode_configured,
                "local_fast" => config.llama_cpp.as_ref().is_some_and(|llama| {
                    llama.server_path.is_file() && llama.fast_model_path.is_file()
                }),
                "local_quality" => config.llama_cpp.as_ref().is_some_and(|llama| {
                    llama.server_path.is_file()
                        && llama
                            .quality_model_path
                            .as_ref()
                            .is_some_and(|path| path.is_file())
                }),
                "qwen_prediction" => config.completion.as_ref().is_some_and(|completion| {
                    completion.server_path.is_file() && completion.model_path.is_file()
                }),
                "wake_word" => config.wake_word_model_path.is_file(),
                "vad" => config.silero_vad_model.is_file(),
                "parakeet" => config.parakeet_model_dir.is_dir(),
                "tts" => config.local_tts.model_path.is_file(),
                "deep" | "review" => custom_configured || opencode_configured,
                _ => false,
            };
            let state = if id == "qwen_prediction" && configured {
                CapabilityStatePayload::Warming
            } else if configured {
                CapabilityStatePayload::Available
            } else if issue.is_some_and(|issue| issue.reason != "not configured") {
                CapabilityStatePayload::Unavailable
            } else {
                CapabilityStatePayload::NotConfigured
            };
            CapabilityPayload {
                id,
                state,
                reason: if id == "qwen_prediction" && configured {
                    String::from("completion model is loading")
                } else if configured {
                    String::from("ready")
                } else {
                    issue
                        .map(|issue| issue.reason.clone())
                        .unwrap_or_else(|| String::from("not configured"))
                },
                actual_provider: if configured && matches!(id, "wake_word" | "vad" | "parakeet") {
                    Some("cpu")
                } else {
                    None
                },
            }
        })
        .collect()
}

fn failed_capabilities(reason: String) -> Vec<CapabilityPayload> {
    CAPABILITY_IDS
        .into_iter()
        .map(|id| CapabilityPayload {
            id,
            state: CapabilityStatePayload::Failed,
            reason: reason.clone(),
            actual_provider: None,
        })
        .collect()
}

fn mark_capability(
    capabilities: &mut [CapabilityPayload],
    id: &str,
    state: CapabilityStatePayload,
    reason: String,
) {
    if let Some(capability) = capabilities.iter_mut().find(|item| item.id == id) {
        capability.state = state;
        capability.reason = reason;
        capability.actual_provider = None;
    }
}

fn update_restored_profile_capabilities(
    capabilities: &mut [CapabilityPayload],
    requested: ResponseProfilePayload,
    restored: ResponseProfilePayload,
    startup_error: &str,
    actual_provider: &'static str,
) {
    let requested_id = if requested == ResponseProfilePayload::Quality {
        "local_quality"
    } else {
        "local_fast"
    };
    if requested == restored {
        mark_capability(
            capabilities,
            requested_id,
            CapabilityStatePayload::Available,
            String::from("ready"),
        );
    } else {
        mark_capability(
            capabilities,
            requested_id,
            CapabilityStatePayload::Failed,
            format!(
                "failed to initialize requested {} profile: {startup_error}",
                requested.as_str()
            ),
        );
    }

    let restored_id = if restored == ResponseProfilePayload::Quality {
        "local_quality"
    } else {
        "local_fast"
    };
    if let Some(capability) = capabilities.iter_mut().find(|item| item.id == restored_id) {
        capability.state = CapabilityStatePayload::Available;
        capability.reason = String::from("ready");
        capability.actual_provider = Some(actual_provider);
    }
}

fn startup_state_after_profile_restore_failure(
    startup_snapshot: &StartupSnapshot,
    requested: ResponseProfilePayload,
    restored: ResponseProfilePayload,
    startup_error: &str,
    restore_error: &str,
) -> StartupStatePayload {
    let mut snapshot = startup_snapshot.clone();
    let requested_id = if requested == ResponseProfilePayload::Quality {
        "local_quality"
    } else {
        "local_fast"
    };
    mark_capability(
        &mut snapshot.capabilities,
        requested_id,
        CapabilityStatePayload::Failed,
        if requested == restored {
            format!(
                "failed to initialize requested {} profile: {startup_error}; retry failed: {restore_error}",
                requested.as_str()
            )
        } else {
            format!(
                "failed to initialize requested {} profile: {startup_error}",
                requested.as_str()
            )
        },
    );
    if requested != restored {
        let restored_id = if restored == ResponseProfilePayload::Quality {
            "local_quality"
        } else {
            "local_fast"
        };
        mark_capability(
            &mut snapshot.capabilities,
            restored_id,
            CapabilityStatePayload::Failed,
            format!(
                "failed to restore previous {} profile: {restore_error}",
                restored.as_str()
            ),
        );
    }
    startup_ready_state_from_snapshot(&snapshot, restored)
}

fn new_partial_transcription_scheduler(
) -> Arc<Mutex<partial_transcription::PartialTranscriptionScheduler>> {
    Arc::new(Mutex::new(
        partial_transcription::PartialTranscriptionScheduler::new(
            partial_transcription::PartialTranscriptionConfig {
                minimum_samples: PARTIAL_TRANSCRIPTION_MINIMUM_SAMPLES,
                throttle: PARTIAL_TRANSCRIPTION_THROTTLE,
                maximum_copied_samples: PARTIAL_TRANSCRIPTION_MAXIMUM_SAMPLES,
            },
        ),
    ))
}

fn platform_inference_policy(
    policy: voxgolem_core::config::InferencePolicy,
) -> voxgolem_platform::inference::InferencePolicy {
    match policy {
        voxgolem_core::config::InferencePolicy::Auto => {
            voxgolem_platform::inference::InferencePolicy::Auto
        }
        voxgolem_core::config::InferencePolicy::Cuda => {
            voxgolem_platform::inference::InferencePolicy::Cuda
        }
        voxgolem_core::config::InferencePolicy::Cpu => {
            voxgolem_platform::inference::InferencePolicy::Cpu
        }
    }
}

fn actual_inference_provider_name(
    provider: voxgolem_platform::inference::ActualInferenceProvider,
) -> &'static str {
    match provider {
        voxgolem_platform::inference::ActualInferenceProvider::Cuda => "cuda",
        voxgolem_platform::inference::ActualInferenceProvider::Cpu => "cpu",
        voxgolem_platform::inference::ActualInferenceProvider::AttachedUnknown => {
            "attached_unknown"
        }
        voxgolem_platform::inference::ActualInferenceProvider::RequestedCuda => "requested_cuda",
    }
}

fn telemetry_inference_provider(
    provider: voxgolem_platform::inference::ActualInferenceProvider,
) -> telemetry::InferenceProvider {
    match provider {
        voxgolem_platform::inference::ActualInferenceProvider::Cuda => {
            telemetry::InferenceProvider::Cuda
        }
        voxgolem_platform::inference::ActualInferenceProvider::Cpu => {
            telemetry::InferenceProvider::Cpu
        }
        voxgolem_platform::inference::ActualInferenceProvider::AttachedUnknown => {
            telemetry::InferenceProvider::AttachedUnknown
        }
        voxgolem_platform::inference::ActualInferenceProvider::RequestedCuda => {
            telemetry::InferenceProvider::AttachedUnknown
        }
    }
}

fn new_telemetry_sink(
    config: voxgolem_core::config::TelemetryConfig,
) -> Option<Arc<Mutex<telemetry::TelemetrySink>>> {
    let state_dir = voxgolem_core::config::default_state_dir().ok()?;
    Some(Arc::new(Mutex::new(telemetry::TelemetrySink::new(
        state_dir,
        telemetry::TelemetryConfig {
            enabled: config.enabled,
            max_bytes: config.max_bytes as u64,
            backup_count: usize::from(config.backup_count),
        },
    ))))
}

fn default_telemetry_sink() -> Option<Arc<Mutex<telemetry::TelemetrySink>>> {
    new_telemetry_sink(voxgolem_core::config::TelemetryConfig {
        enabled: true,
        max_bytes: DEFAULT_TELEMETRY_MAX_BYTES,
        backup_count: DEFAULT_TELEMETRY_BACKUP_COUNT,
    })
}

fn append_telemetry(
    sink: &Option<Arc<Mutex<telemetry::TelemetrySink>>>,
    metadata: telemetry::TelemetryMetadata,
) {
    if let Some(sink) = sink {
        let result = sink
            .lock()
            .map_err(|_| String::from("telemetry sink lock is poisoned"))
            .and_then(|sink| sink.append(&metadata).map_err(|error| error.to_string()));
        if let Err(error) = result {
            if !TELEMETRY_ERROR_REPORTED.swap(true, Ordering::Relaxed) {
                eprintln!("telemetry persistence failed: {error}");
            }
        }
    }
}

fn initialize_app_state(init: AppStateInit) -> AppState {
    AppState {
        startup_state: init.startup_state,
        runtime_config: init.runtime_config,
        selected_response_profile: init.selected_response_profile,
        ui_preferences: Mutex::new(resolved_ui_preferences()),
        settings_operation_lock: Arc::new(tokio::sync::Mutex::new(())),
        ui_preferences_operation_lock: Mutex::new(()),
        supported_response_profiles: init.supported_response_profiles,
        response_profile_switch_generation: init.response_profile_switch_generation,
        response_backend_operation_lock: Mutex::new(()),
        voice_pipeline_config: init.voice_pipeline_config,
        voice_pipeline_state: Arc::new(Mutex::new(init.voice_pipeline_state)),
        wake_word_runtime: init.wake_word_runtime,
        voice_activity_runtime: init.voice_activity_runtime,
        parakeet_runtime: init.parakeet_runtime,
        partial_transcription: new_partial_transcription_scheduler(),
        partial_voice_session: AtomicU64::new(0),
        completion_runtime: Mutex::new(None),
        completion_request: Arc::new(Mutex::new(None)),
        completion_context: Arc::new(Mutex::new(None)),
        completion_generation: AtomicU64::new(0),
        lifecycle_gate: init.lifecycle_gate,
        completion_lifecycle_lock: Mutex::new(()),
        telemetry_sink: init.telemetry_sink,
        conversation_session: Arc::new(Mutex::new(
            voxgolem_core::conversation::ConversationSession::new(init.assistant_policy),
        )),
        assistant_settings_generation: Arc::new(AtomicU64::new(0)),
        settings_publication_in_progress: Arc::new(AtomicBool::new(false)),
        reset_in_progress: Arc::new(AtomicBool::new(false)),
        reset_operation_lock: tokio::sync::Mutex::new(()),
        prompt_cleanup: Arc::new(PromptCleanupTracker::default()),
        prefetch_cleanup: Arc::new(PromptCleanupTracker::default()),
        prompt_publication_gate: Arc::new(Mutex::new(())),
        prompt_publication_epoch: Arc::new(AtomicU64::new(0)),
        tts_operation_lock: tokio::sync::Mutex::new(()),
        tts_startup_generation: Arc::new(AtomicU64::new(0)),
        local_tts_runtime: Mutex::new(init.local_tts_runtime.map(Arc::new)),
        llama_cpp_runtime: init.llama_cpp_runtime,
        llama_cpp_conversation: Mutex::new(Vec::new()),
        llama_cpp_system_prompt: init.llama_cpp_system_prompt,
        opencode_server: Arc::new(Mutex::new(None)),
        active_prompt: Arc::new(Mutex::new(None)),
        active_prompt_generation: AtomicU64::new(0),
        prefetch_generation: AtomicU64::new(0),
        prefetch_cache: Mutex::new(None),
        prefetch_task: Mutex::new(None),
        llama_startups: init.llama_startups,
        exit_cleanup_started: init.exit_cleanup_started,
    }
}

fn build_app_state<R: tauri::Runtime>(_app: &tauri::AppHandle<R>) -> AppState {
    let fallback_voice_pipeline_config = default_voice_pipeline_config();
    let cue_asset_paths = embedded_cue_asset_paths();
    if let Err(error) = cleanup_orphan_state_temps() {
        eprintln!("failed to clean persisted state temporaries: {error}");
    }

    match voxgolem_core::config::load_runtime_config(None) {
        Ok(config) => {
            let telemetry_sink = new_telemetry_sink(config.telemetry);
            let mut capabilities = configured_capabilities(&config);
            let voice_pipeline_config =
                voice_pipeline_config_with_silence_timeout(config.silence_timeout_ms);
            let supported_response_profiles = supported_response_profiles(&config.response_backend);
            let selected_response_profile = Arc::new(Mutex::new(
                resolve_selected_response_profile(&supported_response_profiles),
            ));
            let selected_profile_at_startup = selected_response_profile
                .lock()
                .map(|guard| *guard)
                .unwrap_or_else(|_| default_response_profile());
            let assistant_settings =
                resolve_assistant_settings(&config, &capabilities, selected_profile_at_startup);
            let lifecycle_gate = Arc::new(Mutex::new(()));
            let exit_cleanup_started = Arc::new(AtomicBool::new(false));
            let response_profile_switch_generation = Arc::new(AtomicU64::new(0));
            let llama_startups = Arc::new(Mutex::new(Vec::new()));
            let wake_word_runtime = if config.wake_word_model_path.is_file() {
                wake_word::WakeWordRuntime::new(
                    &config.wake_word_model_path,
                    config.wake_word_detection_threshold,
                )
                .map(Mutex::new)
                .map_err(|error| eprintln!("failed to initialize wake word detector: {error}"))
                .ok()
            } else {
                None
            };
            if wake_word_runtime.is_none() && config.wake_word_model_path.is_file() {
                mark_capability(
                    &mut capabilities,
                    "wake_word",
                    CapabilityStatePayload::Failed,
                    String::from("wake word runtime failed to initialize"),
                );
            }
            let effective_tts_enabled = resolve_effective_tts_enabled(
                config.local_tts.enabled,
                config.local_tts.model_path.is_file(),
            );
            // TTS is optional and native provider construction can be slow or stuck. It is
            // warmed after the shell state exists so it cannot delay unrelated startup.
            let local_tts_runtime = None;
            let tts_enabled = local_tts_runtime.is_some();
            repair_tts_capability(&mut capabilities, tts_enabled, local_tts_runtime.as_ref());
            if effective_tts_enabled && local_tts_runtime.is_none() {
                mark_capability(
                    &mut capabilities,
                    "tts",
                    CapabilityStatePayload::Warming,
                    String::from("TTS runtime is warming up"),
                );
            }
            let tts_output_gain_db = config.local_tts.output_gain_db;
            let mut voice_input_errors = Vec::new();
            let parakeet_runtime = if config.parakeet_model_dir.is_dir() {
                match transcription::ParakeetRuntime::load(&config.parakeet_model_dir) {
                    Ok(runtime) => Some(Arc::new(Mutex::new(runtime))),
                    Err(error) => {
                        let error_message =
                            format!("failed to initialize parakeet transcriber: {error:?}");
                        eprintln!("{error_message}");
                        voice_input_errors.push(error_message);
                        None
                    }
                }
            } else {
                None
            };
            let voice_activity_runtime = if config.silero_vad_model.is_file() {
                match voice_activity::VoiceActivityRuntime::load(&config.silero_vad_model) {
                    Ok(runtime) => Some(Mutex::new(runtime)),
                    Err(error) => {
                        let error_message =
                            format!("failed to initialize voice activity detector: {error:?}");
                        eprintln!("{error_message}");
                        voice_input_errors.push(error_message);
                        None
                    }
                }
            } else {
                None
            };
            if parakeet_runtime.is_none() && config.parakeet_model_dir.is_dir() {
                mark_capability(
                    &mut capabilities,
                    "parakeet",
                    CapabilityStatePayload::Failed,
                    String::from("Parakeet runtime failed to initialize"),
                );
            }
            if voice_activity_runtime.is_none() && config.silero_vad_model.is_file() {
                mark_capability(
                    &mut capabilities,
                    "vad",
                    CapabilityStatePayload::Failed,
                    String::from("VAD runtime failed to initialize"),
                );
            }
            let voice_input_available = wake_word_runtime.is_some()
                && parakeet_runtime.is_some()
                && voice_activity_runtime.is_some();
            if wake_word_runtime.is_none() {
                voice_input_errors.push(String::from("wake word runtime is unavailable"));
            }
            let voice_input_error = if voice_input_errors.is_empty() {
                None
            } else {
                Some(voice_input_errors.join("\n"))
            };
            let llama_cpp_system_prompt = match &config.response_backend {
                voxgolem_core::config::ResponseBackendConfig::LlamaCpp { .. } => {
                    match load_llama_cpp_system_prompt() {
                        Ok(prompt) => Some(prompt),
                        Err(error) => {
                            return build_startup_error_app_state(
                                voice_pipeline_config,
                                format!("failed to load SOUL.md: {error}"),
                            );
                        }
                    }
                }
                voxgolem_core::config::ResponseBackendConfig::Opencode { .. } => None,
                voxgolem_core::config::ResponseBackendConfig::Unconfigured => None,
            };
            let startup_state = Arc::new(Mutex::new(match &config.response_backend {
                voxgolem_core::config::ResponseBackendConfig::LlamaCpp { .. } => {
                    let mut warming_capabilities = capabilities.clone();
                    mark_capability(
                        &mut warming_capabilities,
                        if selected_profile_at_startup == ResponseProfilePayload::Quality {
                            "local_quality"
                        } else {
                            "local_fast"
                        },
                        CapabilityStatePayload::Warming,
                        String::from("local model is loading"),
                    );
                    StartupStatePayload::WarmingModel {
                        cue_asset_paths: cue_asset_paths.clone(),
                        runtime_phase: RuntimePhasePayload::Initializing,
                        voice_input_available,
                        voice_input_error: voice_input_error.clone(),
                        silence_timeout_ms: config.silence_timeout_ms,
                        message: String::from("Loading local Gemma model..."),
                        selected_response_profile: selected_profile_at_startup,
                        supported_response_profiles: supported_response_profiles.clone(),
                        prompt_cancellation_available: false,
                        tts_enabled,
                        tts_output_gain_db,
                        capabilities: warming_capabilities,
                    }
                }
                voxgolem_core::config::ResponseBackendConfig::Opencode { .. } => {
                    StartupStatePayload::Ready {
                        cue_asset_paths: cue_asset_paths.clone(),
                        runtime_phase: RuntimePhasePayload::Sleeping,
                        voice_input_available,
                        voice_input_error: voice_input_error.clone(),
                        silence_timeout_ms: config.silence_timeout_ms,
                        selected_response_profile: selected_profile_at_startup,
                        supported_response_profiles: supported_response_profiles.clone(),
                        prompt_cancellation_available: true,
                        tts_enabled,
                        tts_output_gain_db,
                        capabilities: capabilities.clone(),
                    }
                }
                voxgolem_core::config::ResponseBackendConfig::Unconfigured => {
                    StartupStatePayload::Ready {
                        cue_asset_paths: cue_asset_paths.clone(),
                        runtime_phase: RuntimePhasePayload::Sleeping,
                        voice_input_available,
                        voice_input_error: voice_input_error.clone(),
                        silence_timeout_ms: config.silence_timeout_ms,
                        selected_response_profile: selected_profile_at_startup,
                        supported_response_profiles: Vec::new(),
                        prompt_cancellation_available: true,
                        tts_enabled,
                        tts_output_gain_db,
                        capabilities: capabilities.clone(),
                    }
                }
            }));
            let llama_cpp_runtime = Arc::new(Mutex::new(None));
            if capabilities.iter().any(|capability| {
                capability.id == "local_fast"
                    && capability.state == CapabilityStatePayload::Available
            }) {
                if let voxgolem_core::config::ResponseBackendConfig::LlamaCpp {
                    server_path,
                    host,
                    port,
                    fast_model_path,
                    quality_model_path,
                } = &config.response_backend
                {
                    let startup_state = Arc::clone(&startup_state);
                    let lifecycle_gate = Arc::clone(&lifecycle_gate);
                    let llama_cpp_runtime = Arc::clone(&llama_cpp_runtime);
                    let response_profile_switch_generation =
                        Arc::clone(&response_profile_switch_generation);
                    let startup_generation =
                        response_profile_switch_generation.load(Ordering::SeqCst);
                    let startup_exit_cleanup = Arc::clone(&exit_cleanup_started);
                    let cue_asset_paths = cue_asset_paths.clone();
                    let voice_input_error = voice_input_error.clone();
                    let silence_timeout_ms = config.silence_timeout_ms;
                    let selected_response_profile = selected_profile_at_startup;
                    let supported_response_profiles = supported_response_profiles.clone();
                    let inference_policy = config
                        .llama_cpp
                        .as_ref()
                        .map(|llama| platform_inference_policy(llama.inference_provider))
                        .unwrap_or(voxgolem_platform::inference::InferencePolicy::Auto);
                    let model_path = match model_path_for_profile(
                        selected_response_profile,
                        fast_model_path,
                        quality_model_path.as_ref(),
                    ) {
                        Ok(path) => path.to_path_buf(),
                        Err(_) => fast_model_path.clone(),
                    };
                    let server_spec = voxgolem_platform::llama_cpp::LlamaCppServerSpec::new(
                        server_path.clone(),
                        model_path,
                        host.clone(),
                        *port,
                        LLAMA_CPP_MODEL_ALIAS,
                    );

                    let (startup_cancellation, startup_worker) =
                        voxgolem_platform::llama_cpp::LlamaCppRuntime::start_with_policy_cancellable(
                            server_spec, inference_policy);
                    let startup_coordinator = std::thread::spawn(move || {
                        let start_result = startup_worker.join().unwrap_or_else(|_| Err(voxgolem_platform::llama_cpp::LlamaCppRuntimeError::StartupCancelled));
                        if !runtime_startup_is_current(
                            &startup_exit_cleanup,
                            &response_profile_switch_generation,
                            startup_generation,
                        ) {
                            shutdown_llama_start_result(start_result);
                            return;
                        }

                        let local_capability_id =
                            if selected_response_profile == ResponseProfilePayload::Quality {
                                "local_quality"
                            } else {
                                "local_fast"
                            };
                        let local_start_result = match start_result {
                            Ok(runtime) => {
                                let actual_provider =
                                    actual_inference_provider_name(runtime.actual_provider());
                                if !store_llama_runtime_if_current(
                                    runtime,
                                    &llama_cpp_runtime,
                                    &response_profile_switch_generation,
                                    startup_generation,
                                ) {
                                    return;
                                }
                                Ok(actual_provider)
                            }
                            Err(error) => Err(format!(
                                "failed to initialize local llama.cpp runtime: {error}"
                            )),
                        };

                        let Ok(_lifecycle) = lifecycle_gate.lock() else {
                            return;
                        };
                        if let Ok(mut guard) = startup_state.lock() {
                            if !runtime_startup_is_current(
                                &startup_exit_cleanup,
                                &response_profile_switch_generation,
                                startup_generation,
                            ) {
                                return;
                            }
                            let (mut latest_capabilities, current_tts_enabled) = match &*guard {
                                StartupStatePayload::WarmingModel {
                                    capabilities,
                                    tts_enabled,
                                    ..
                                }
                                | StartupStatePayload::Ready {
                                    capabilities,
                                    tts_enabled,
                                    ..
                                } => (capabilities.clone(), *tts_enabled),
                                StartupStatePayload::Error { .. } => return,
                            };
                            let local_ready = local_start_result.is_ok();
                            if let Some(capability) = latest_capabilities
                                .iter_mut()
                                .find(|item| item.id == local_capability_id)
                            {
                                match local_start_result {
                                    Ok(actual_provider) => {
                                        capability.state = CapabilityStatePayload::Available;
                                        capability.reason = String::from("ready");
                                        capability.actual_provider = Some(actual_provider);
                                    }
                                    Err(reason) => {
                                        capability.state = CapabilityStatePayload::Failed;
                                        capability.reason = reason;
                                        capability.actual_provider = None;
                                    }
                                }
                            }
                            *guard = StartupStatePayload::Ready {
                                cue_asset_paths,
                                runtime_phase: RuntimePhasePayload::Sleeping,
                                voice_input_available,
                                voice_input_error,
                                silence_timeout_ms,
                                selected_response_profile,
                                supported_response_profiles: if local_ready {
                                    supported_response_profiles
                                } else {
                                    Vec::new()
                                },
                                prompt_cancellation_available: true,
                                tts_enabled: current_tts_enabled,
                                tts_output_gain_db,
                                capabilities: latest_capabilities,
                            };
                        }
                    });
                    register_llama_startup(
                        &llama_startups,
                        &exit_cleanup_started,
                        startup_cancellation,
                        startup_coordinator,
                    );
                }
            }
            let voice_pipeline_state = apply_voice_pipeline_event_or_panic(
                voxgolem_core::voice_pipeline::VoicePipelineState::new(voice_pipeline_config)
                    .expect("voice pipeline should initialize with valid constants"),
                voice_pipeline_config,
                voxgolem_core::voice_pipeline::VoicePipelineEvent::StartupValidated,
                "startup validation should initialize the session to sleeping",
            );

            initialize_app_state(AppStateInit {
                startup_state,
                runtime_config: Some(config),
                selected_response_profile,
                supported_response_profiles,
                response_profile_switch_generation,
                voice_pipeline_config,
                voice_pipeline_state,
                wake_word_runtime,
                voice_activity_runtime,
                parakeet_runtime,
                lifecycle_gate,
                telemetry_sink,
                assistant_policy: assistant_settings.into(),
                local_tts_runtime,
                llama_cpp_runtime,
                llama_cpp_system_prompt,
                llama_startups,
                exit_cleanup_started,
            })
        }
        Err(error) => build_nonfatal_config_error_app_state(
            fallback_voice_pipeline_config,
            cue_asset_paths,
            error.to_string(),
        ),
    }
}

fn initialize_local_tts_runtime(
    config: &voxgolem_core::config::LocalTtsConfig,
    enabled: bool,
    runtime_file_logging_enabled: bool,
) -> Result<Option<tts::LocalTtsRuntime>, String> {
    if !enabled {
        log_tts_runtime_event(
            runtime_file_logging_enabled,
            "runtime initialization skipped: disabled",
        );
        return Ok(None);
    }

    if let Some(strict_espeak_root) = tts::resolve_strict_windows_espeak_data_directory()
        .map_err(|error| format!("failed to resolve strict eSpeak data root: {error}"))?
    {
        log_tts_runtime_event(
            runtime_file_logging_enabled,
            &format!(
                "resolved strict eSpeak data root: {}",
                strict_espeak_root.display()
            ),
        );
    }

    let spec = tts::LocalTtsRuntimeSpec {
        model_path: Some(config.model_path.clone()),
        worker_count: config.worker_count,
        max_queue: config.max_queue,
        sample_rate_hz: config.sample_rate_hz,
        max_duration_s: config.max_duration_s,
        provider_policy: tts::TtsProviderPolicy::Auto,
    };

    match tts::LocalTtsRuntime::new(spec) {
        Ok(runtime) => {
            log_tts_runtime_event(
                runtime_file_logging_enabled,
                "runtime initialized successfully",
            );
            Ok(Some(runtime))
        }
        Err(error) => {
            let message = format!("failed to initialize local tts runtime: {error}");
            log_tts_runtime_event(runtime_file_logging_enabled, &message);
            Err(message)
        }
    }
}

fn embedded_cue_asset_paths() -> CueAssetPathsPayload {
    CueAssetPathsPayload {
        start_listening: cue_audio_data_url(START_LISTENING_CUE_WAV),
        stop_listening: cue_audio_data_url(STOP_LISTENING_CUE_WAV),
    }
}

fn cue_audio_data_url(bytes: &[u8]) -> String {
    format!(
        "{CUE_AUDIO_DATA_URL_PREFIX}{}",
        base64::engine::general_purpose::STANDARD.encode(bytes)
    )
}

fn build_startup_error_app_state(
    voice_pipeline_config: voxgolem_core::voice_pipeline::VoicePipelineConfig,
    message: String,
) -> AppState {
    let voice_pipeline_state = apply_voice_pipeline_event_or_panic(
        voxgolem_core::voice_pipeline::VoicePipelineState::new(voice_pipeline_config)
            .expect("voice pipeline should initialize with valid constants"),
        voice_pipeline_config,
        voxgolem_core::voice_pipeline::VoicePipelineEvent::StartupFailed {
            message: message.clone(),
        },
        "startup failure should initialize the session to error",
    );

    initialize_app_state(AppStateInit {
        startup_state: Arc::new(Mutex::new(StartupStatePayload::Error { message })),
        runtime_config: None,
        selected_response_profile: Arc::new(Mutex::new(default_response_profile())),
        supported_response_profiles: vec![default_response_profile()],
        response_profile_switch_generation: Arc::new(AtomicU64::new(0)),
        voice_pipeline_config,
        voice_pipeline_state,
        wake_word_runtime: None,
        voice_activity_runtime: None,
        parakeet_runtime: None,
        lifecycle_gate: Arc::new(Mutex::new(())),
        telemetry_sink: default_telemetry_sink(),
        assistant_policy: AssistantSettingsPayload::default().into(),
        local_tts_runtime: None,
        llama_cpp_runtime: Arc::new(Mutex::new(None)),
        llama_cpp_system_prompt: None,
        llama_startups: Arc::new(Mutex::new(Vec::new())),
        exit_cleanup_started: Arc::new(AtomicBool::new(false)),
    })
}

fn build_nonfatal_config_error_app_state(
    voice_pipeline_config: voxgolem_core::voice_pipeline::VoicePipelineConfig,
    cue_asset_paths: CueAssetPathsPayload,
    message: String,
) -> AppState {
    let voice_pipeline_state = apply_voice_pipeline_event_or_panic(
        voxgolem_core::voice_pipeline::VoicePipelineState::new(voice_pipeline_config)
            .expect("voice pipeline should initialize with valid constants"),
        voice_pipeline_config,
        voxgolem_core::voice_pipeline::VoicePipelineEvent::StartupValidated,
        "nonfatal config failure should leave the shell usable",
    );
    initialize_app_state(AppStateInit {
        startup_state: Arc::new(Mutex::new(StartupStatePayload::Ready {
            cue_asset_paths,
            runtime_phase: RuntimePhasePayload::Sleeping,
            voice_input_available: false,
            voice_input_error: Some(message.clone()),
            silence_timeout_ms: DEFAULT_SILENCE_TIMEOUT_MS,
            selected_response_profile: default_response_profile(),
            supported_response_profiles: Vec::new(),
            prompt_cancellation_available: false,
            tts_enabled: false,
            tts_output_gain_db: 3.0,
            capabilities: failed_capabilities(message),
        })),
        runtime_config: None,
        selected_response_profile: Arc::new(Mutex::new(default_response_profile())),
        supported_response_profiles: Vec::new(),
        response_profile_switch_generation: Arc::new(AtomicU64::new(0)),
        voice_pipeline_config,
        voice_pipeline_state,
        wake_word_runtime: None,
        voice_activity_runtime: None,
        parakeet_runtime: None,
        lifecycle_gate: Arc::new(Mutex::new(())),
        telemetry_sink: default_telemetry_sink(),
        assistant_policy: AssistantSettingsPayload::default().into(),
        local_tts_runtime: None,
        llama_cpp_runtime: Arc::new(Mutex::new(None)),
        llama_cpp_system_prompt: None,
        llama_startups: Arc::new(Mutex::new(Vec::new())),
        exit_cleanup_started: Arc::new(AtomicBool::new(false)),
    })
}

fn load_llama_cpp_system_prompt() -> Result<String, String> {
    let soul_path = application_config_path()?.with_file_name(WINDOWS_SOUL_FILE_NAME);
    let contents = fs::read_to_string(&soul_path)
        .map_err(|error| format!("{}: {error}", soul_path.display()))?;
    let trimmed = contents.trim();

    if trimmed.is_empty() {
        return Err(format!("{} is empty", soul_path.display()));
    }

    Ok(trimmed.to_string())
}

fn current_runtime_phase(
    voice_pipeline_state: &Mutex<voxgolem_core::voice_pipeline::VoicePipelineState>,
) -> Result<RuntimePhasePayload, String> {
    let guard = voice_pipeline_state
        .lock()
        .map_err(|_| String::from("voice pipeline lock is poisoned"))?;

    Ok(to_runtime_phase_payload(guard.session().runtime().phase()))
}

fn ensure_response_profile_switch_runtime_is_idle(
    voice_pipeline_state: &Mutex<voxgolem_core::voice_pipeline::VoicePipelineState>,
) -> Result<(), String> {
    let runtime_phase = current_runtime_phase(voice_pipeline_state)?;

    if runtime_phase != RuntimePhasePayload::Sleeping {
        return Err(format!(
            "response profile switch is only allowed while runtime is sleeping; current phase is {}",
            runtime_phase.as_str()
        ));
    }

    Ok(())
}

fn ensure_startup_ready_for_prompt(
    startup_state: &Arc<Mutex<StartupStatePayload>>,
) -> Result<(), String> {
    let startup_state = startup_state
        .lock()
        .map_err(|_| String::from("startup state lock should not be poisoned"))?;

    match &*startup_state {
        StartupStatePayload::Ready { .. } => Ok(()),
        StartupStatePayload::WarmingModel { .. } => {
            Err(String::from("local Gemma model is still warming up"))
        }
        StartupStatePayload::Error { message } => Err(format!("startup error: {message}")),
    }
}

fn ensure_startup_ready_for_profile_switch(
    startup_state: &Arc<Mutex<StartupStatePayload>>,
) -> Result<(), String> {
    let startup_state = startup_state
        .lock()
        .map_err(|_| String::from("startup state lock should not be poisoned"))?;

    match &*startup_state {
        StartupStatePayload::Ready { .. } => Ok(()),
        StartupStatePayload::WarmingModel { .. } => Err(String::from(
            "response backend is busy; wait for the active operation to finish",
        )),
        StartupStatePayload::Error { message } => Err(format!("startup error: {message}")),
    }
}

fn lock_response_backend_operation<'a>(
    operation_lock: &'a Mutex<()>,
) -> Result<MutexGuard<'a, ()>, String> {
    operation_lock
        .lock()
        .map_err(|_| String::from("response backend operation lock is poisoned"))
}

#[allow(dead_code)]
fn current_silence_deadline(
    voice_pipeline_state: &Mutex<voxgolem_core::voice_pipeline::VoicePipelineState>,
    voice_pipeline_config: voxgolem_core::voice_pipeline::VoicePipelineConfig,
) -> Result<u64, String> {
    let guard = voice_pipeline_state
        .lock()
        .map_err(|_| String::from("voice pipeline lock is poisoned"))?;

    let last_activity_ms = guard.session().voice_turn().last_activity_ms().unwrap_or(0);
    Ok(last_activity_ms.saturating_add(
        voice_pipeline_config
            .session()
            .voice_turn()
            .silence_timeout_ms(),
    ))
}

fn current_runtime_phase_response(
    voice_pipeline_state: &Mutex<voxgolem_core::voice_pipeline::VoicePipelineState>,
    transcription_ready_samples: Option<usize>,
    transcript_text: Option<String>,
) -> Result<RuntimePhaseResponsePayload, String> {
    let guard = voice_pipeline_state
        .lock()
        .map_err(|_| String::from("voice pipeline lock is poisoned"))?;

    Ok(runtime_phase_response_from_state(
        &guard,
        transcription_ready_samples,
        transcript_text,
        None,
    ))
}

fn runtime_phase_response_from_state(
    voice_pipeline_state: &voxgolem_core::voice_pipeline::VoicePipelineState,
    transcription_ready_samples: Option<usize>,
    transcript_text: Option<String>,
    telemetry: Option<RuntimeTelemetryPayload>,
) -> RuntimePhaseResponsePayload {
    RuntimePhaseResponsePayload {
        runtime_phase: to_runtime_phase_payload(voice_pipeline_state.session().runtime().phase()),
        transcription_ready_samples,
        transcript_text,
        last_activity_ms: voice_pipeline_state
            .session()
            .voice_turn()
            .last_activity_ms(),
        capturing_utterance: voice_pipeline_state.capture().capturing_utterance(),
        preroll_samples: voice_pipeline_state.capture().preroll_len(),
        utterance_samples: voice_pipeline_state.capture().utterance_len(),
        telemetry,
    }
}

fn process_wake_word_frame(
    wake_word_runtime: &Option<Mutex<wake_word::WakeWordRuntime>>,
    frame: &[f32],
) -> Result<(Option<wake_word::WakeWordDetection>, Option<f32>), String> {
    let Some(wake_word_runtime) = wake_word_runtime else {
        return Ok((None, None));
    };

    let mut guard = wake_word_runtime
        .lock()
        .map_err(|_| String::from("wake word runtime lock is poisoned"))?;

    let detection = guard.process_sleeping_frame(frame)?;
    let confidence = detection
        .map(|result| result.confidence)
        .or_else(|| guard.latest_confidence());
    Ok((detection, confidence))
}

fn reset_wake_word_runtime(
    wake_word_runtime: &Option<Mutex<wake_word::WakeWordRuntime>>,
) -> Result<(), String> {
    let Some(wake_word_runtime) = wake_word_runtime else {
        return Ok(());
    };

    let mut guard = wake_word_runtime
        .lock()
        .map_err(|_| String::from("wake word runtime lock is poisoned"))?;
    guard.reset();
    Ok(())
}

fn process_voice_activity_frame(
    voice_activity_runtime: &Option<Mutex<voice_activity::VoiceActivityRuntime>>,
    frame: &[f32],
) -> Result<bool, String> {
    let Some(voice_activity_runtime) = voice_activity_runtime else {
        return Ok(false);
    };

    let mut guard = voice_activity_runtime
        .lock()
        .map_err(|_| String::from("voice activity runtime lock is poisoned"))?;

    guard
        .process_frame(frame)
        .map_err(|error| format!("voice activity detection failed: {error:?}"))
}

fn reset_voice_activity_runtime(
    voice_activity_runtime: &Option<Mutex<voice_activity::VoiceActivityRuntime>>,
) -> Result<(), String> {
    let Some(voice_activity_runtime) = voice_activity_runtime else {
        return Ok(());
    };

    let mut guard = voice_activity_runtime
        .lock()
        .map_err(|_| String::from("voice activity runtime lock is poisoned"))?;
    guard.reset();
    Ok(())
}

fn current_time_ms() -> Result<u64, String> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .map_err(|error| format!("system clock is before unix epoch: {error}"))
}

fn ingest_audio_frame_with_optional_wake_word_detection(
    voice_pipeline_state: &voxgolem_core::voice_pipeline::VoicePipelineState,
    voice_pipeline_config: voxgolem_core::voice_pipeline::VoicePipelineConfig,
    frame: Vec<f32>,
    wake_word_now_ms: Option<u64>,
) -> Result<voxgolem_core::voice_pipeline::VoicePipelineState, String> {
    if matches!(
        voice_pipeline_state.session().runtime().phase(),
        voxgolem_core::runtime::RuntimePhase::Sleeping
    ) {
        if let Some(now_ms) = wake_word_now_ms {
            let listening_state = voxgolem_core::voice_pipeline::apply_voice_pipeline_event(
                voice_pipeline_state,
                voice_pipeline_config,
                voxgolem_core::voice_pipeline::VoicePipelineEvent::WakeWordDetected { now_ms },
            )
            .map_err(|error| format!("wake word transition failed: {error:?}"))?
            .0;

            return voxgolem_core::voice_pipeline::ingest_audio_frame(
                &listening_state,
                voice_pipeline_config,
                frame,
            )
            .map_err(|error| format!("voice pipeline frame ingestion failed: {error:?}"));
        }
    }

    voxgolem_core::voice_pipeline::ingest_audio_frame(
        voice_pipeline_state,
        voice_pipeline_config,
        frame,
    )
    .map_err(|error| format!("voice pipeline frame ingestion failed: {error:?}"))
}

fn apply_optional_speech_activity(
    voice_pipeline_state: voxgolem_core::voice_pipeline::VoicePipelineState,
    voice_pipeline_config: voxgolem_core::voice_pipeline::VoicePipelineConfig,
    speech_detected: bool,
    now_ms: u64,
) -> Result<voxgolem_core::voice_pipeline::VoicePipelineState, String> {
    if !speech_detected
        || !matches!(
            voice_pipeline_state.session().runtime().phase(),
            voxgolem_core::runtime::RuntimePhase::Listening
        )
    {
        return Ok(voice_pipeline_state);
    }

    voxgolem_core::voice_pipeline::apply_voice_pipeline_event(
        &voice_pipeline_state,
        voice_pipeline_config,
        voxgolem_core::voice_pipeline::VoicePipelineEvent::SpeechDetected { now_ms },
    )
    .map(|(next_state, _)| next_state)
    .map_err(|error| format!("speech activity transition failed: {error:?}"))
}

fn wake_word_event_timestamp(
    now_ms: u64,
    wake_word_detection: Option<wake_word::WakeWordDetection>,
) -> Option<u64> {
    wake_word_detection.map(|_| now_ms)
}

fn to_runtime_phase_payload(
    runtime_phase: voxgolem_core::runtime::RuntimePhase,
) -> RuntimePhasePayload {
    match runtime_phase {
        voxgolem_core::runtime::RuntimePhase::Initializing => RuntimePhasePayload::Initializing,
        voxgolem_core::runtime::RuntimePhase::Sleeping => RuntimePhasePayload::Sleeping,
        voxgolem_core::runtime::RuntimePhase::Listening => RuntimePhasePayload::Listening,
        voxgolem_core::runtime::RuntimePhase::Processing => RuntimePhasePayload::Processing,
        voxgolem_core::runtime::RuntimePhase::Executing => RuntimePhasePayload::Executing,
        voxgolem_core::runtime::RuntimePhase::Error => RuntimePhasePayload::Error,
    }
}

fn default_voice_pipeline_config() -> voxgolem_core::voice_pipeline::VoicePipelineConfig {
    voice_pipeline_config_with_silence_timeout(DEFAULT_SILENCE_TIMEOUT_MS)
}

fn voice_pipeline_config_with_silence_timeout(
    silence_timeout_ms: u64,
) -> voxgolem_core::voice_pipeline::VoicePipelineConfig {
    let voice_turn = voxgolem_core::voice_turn::VoiceTurnConfig::new(silence_timeout_ms)
        .expect("silence timeout constant should be valid");
    let capture = voxgolem_core::turn_capture::TurnCaptureConfig::new(
        DEFAULT_PREROLL_MAX_SAMPLES,
        DEFAULT_UTTERANCE_MAX_SAMPLES,
    )
    .expect("turn capture constants should be valid");

    voxgolem_core::voice_pipeline::VoicePipelineConfig::new(
        voxgolem_core::session::SessionConfig::new(voice_turn),
        capture,
        voxgolem_model::parakeet::PARAKEET_SAMPLE_RATE_HZ,
    )
}

fn apply_voice_pipeline_transition(
    voice_pipeline_state: &Mutex<voxgolem_core::voice_pipeline::VoicePipelineState>,
    voice_pipeline_config: voxgolem_core::voice_pipeline::VoicePipelineConfig,
    event: voxgolem_core::voice_pipeline::VoicePipelineEvent,
) -> Result<voxgolem_core::voice_pipeline::VoicePipelineAction, String> {
    let mut guard = voice_pipeline_state
        .lock()
        .map_err(|_| String::from("voice pipeline lock is poisoned"))?;

    let (next_state, action) = voxgolem_core::voice_pipeline::apply_voice_pipeline_event(
        &guard,
        voice_pipeline_config,
        event,
    )
    .map_err(|error| format!("voice pipeline transition failed: {error:?}"))?;

    *guard = next_state;
    Ok(action)
}

fn apply_voice_pipeline_transition_with_input_runtime_reset(
    voice_pipeline_state: &Mutex<voxgolem_core::voice_pipeline::VoicePipelineState>,
    wake_word_runtime: &Option<Mutex<wake_word::WakeWordRuntime>>,
    voice_activity_runtime: &Option<Mutex<voice_activity::VoiceActivityRuntime>>,
    voice_pipeline_config: voxgolem_core::voice_pipeline::VoicePipelineConfig,
    event: voxgolem_core::voice_pipeline::VoicePipelineEvent,
) -> Result<voxgolem_core::voice_pipeline::VoicePipelineAction, String> {
    let mut voice_pipeline_guard = voice_pipeline_state
        .lock()
        .map_err(|_| String::from("voice pipeline lock is poisoned"))?;

    reset_wake_word_runtime(wake_word_runtime)?;
    reset_voice_activity_runtime(voice_activity_runtime)?;

    let (next_state, action) = voxgolem_core::voice_pipeline::apply_voice_pipeline_event(
        &voice_pipeline_guard,
        voice_pipeline_config,
        event,
    )
    .map_err(|error| format!("voice pipeline transition failed: {error:?}"))?;

    *voice_pipeline_guard = next_state;
    Ok(action)
}

fn apply_voice_pipeline_event_or_panic(
    state: voxgolem_core::voice_pipeline::VoicePipelineState,
    config: voxgolem_core::voice_pipeline::VoicePipelineConfig,
    event: voxgolem_core::voice_pipeline::VoicePipelineEvent,
    message: &str,
) -> voxgolem_core::voice_pipeline::VoicePipelineState {
    voxgolem_core::voice_pipeline::apply_voice_pipeline_event(&state, config, event)
        .expect(message)
        .0
}

fn validate_prompt_text(prompt: String) -> Result<String, String> {
    if prompt.trim().is_empty() {
        return Err(String::from("invalid prompt: prompt must not be empty"));
    }
    if prompt.len() > PROMPT_MAX_BYTES {
        return Err(format!(
            "invalid prompt: prompt exceeds {PROMPT_MAX_BYTES} bytes"
        ));
    }

    Ok(prompt)
}

fn bounded_provider_history(
    history: &[voxgolem_core::assistant::ConversationTurn],
) -> &[voxgolem_core::assistant::ConversationTurn] {
    let mut start = history.len();
    let mut bytes = 0_usize;

    while start >= 2 {
        let pair_start = start - 2;
        if history.len() - pair_start > voxgolem_core::agent_pipeline::MAX_HISTORY_ENTRIES {
            break;
        }
        let pair_bytes = history[pair_start..start]
            .iter()
            .map(|turn| assistant_content_text(&turn.content).len())
            .sum::<usize>();
        let Some(next_bytes) = bytes.checked_add(pair_bytes) else {
            break;
        };
        if next_bytes > PROVIDER_HISTORY_MAX_BYTES {
            break;
        }
        bytes = next_bytes;
        start = pair_start;
    }

    &history[start..]
}

fn validate_prompt_request_id(request_id: &str) -> Result<(), String> {
    let Some((timestamp, nonce)) = request_id
        .strip_prefix("request-")
        .and_then(|value| value.split_once('-'))
    else {
        return Err(String::from(
            "request ID must be an opaque generated identifier",
        ));
    };
    if timestamp.is_empty()
        || timestamp.len() > 20
        || !timestamp.bytes().all(|byte| byte.is_ascii_digit())
        || nonce.is_empty()
        || nonce.len() > 32
        || !nonce.bytes().all(|byte| byte.is_ascii_alphanumeric())
    {
        return Err(String::from(
            "request ID must be an opaque generated identifier",
        ));
    }
    Ok(())
}

fn build_llama_prompt_input(
    system_prompt: &str,
    prompt: &str,
    conversation: &[LlamaConversationTurn],
) -> LlamaPromptInput {
    let user_prompt = render_llama_user_prompt(conversation, prompt);
    if estimate_llama_input_tokens(system_prompt, &user_prompt) <= llama_cpp_input_token_limit() {
        return LlamaPromptInput {
            user_prompt,
            rolled_over: false,
        };
    }

    if conversation.is_empty() {
        return LlamaPromptInput {
            user_prompt,
            rolled_over: false,
        };
    }

    LlamaPromptInput {
        user_prompt: render_llama_user_prompt(&[], prompt),
        rolled_over: true,
    }
}

fn render_llama_user_prompt(conversation: &[LlamaConversationTurn], prompt: &str) -> String {
    if conversation.is_empty() {
        return prompt.to_string();
    }

    let mut rendered = String::from("Conversation so far:\n");
    for turn in conversation {
        rendered.push_str("User: ");
        rendered.push_str(&turn.user);
        rendered.push('\n');
        rendered.push_str("Assistant: ");
        rendered.push_str(&turn.assistant);
        rendered.push_str("\n\n");
    }
    rendered.push_str("Current user message:\n");
    rendered.push_str(prompt);
    rendered
}

fn llama_cpp_input_token_limit() -> usize {
    LLAMA_CPP_CONTEXT_WINDOW_TOKENS
        .saturating_sub(usize::from(LLAMA_CPP_MAX_TOKENS))
        .saturating_sub(LLAMA_CPP_CONTEXT_SAFETY_MARGIN_TOKENS)
}

fn estimate_llama_input_tokens(system_prompt: &str, user_prompt: &str) -> usize {
    estimate_text_tokens(system_prompt)
        .saturating_add(estimate_text_tokens(user_prompt))
        .saturating_add(LLAMA_CPP_CHAT_WRAPPER_TOKENS)
}

fn estimate_text_tokens(text: &str) -> usize {
    let char_count = text.chars().count();
    if char_count == 0 {
        0
    } else {
        char_count.div_ceil(4)
    }
}

fn is_llama_context_overflow_error(error_message: &str) -> bool {
    let normalized = error_message.to_ascii_lowercase();
    let has_status = normalized.contains("status 400") || normalized.contains("status 413");
    let mentions_context = normalized.contains("context");
    let mentions_overflow = normalized.contains("exceed")
        || normalized.contains("too long")
        || normalized.contains("limit")
        || normalized.contains("maximum")
        || normalized.contains("window");

    has_status && mentions_context && mentions_overflow
}

fn transcription_ready_samples(
    action: &voxgolem_core::voice_pipeline::VoicePipelineAction,
) -> Option<usize> {
    match action {
        voxgolem_core::voice_pipeline::VoicePipelineAction::FinishedUtterance {
            transcription_input,
        } => Some(transcription_input.samples().len()),
        _ => None,
    }
}

fn transcribe_finished_utterance(
    action: &voxgolem_core::voice_pipeline::VoicePipelineAction,
    parakeet_runtime: &Option<Arc<Mutex<transcription::ParakeetRuntime>>>,
) -> Result<Option<String>, String> {
    let voxgolem_core::voice_pipeline::VoicePipelineAction::FinishedUtterance {
        transcription_input,
    } = action
    else {
        return Ok(None);
    };

    let parakeet_runtime = parakeet_runtime
        .as_ref()
        .ok_or_else(|| String::from("parakeet runtime is not ready"))?;
    let mut guard = parakeet_runtime
        .lock()
        .map_err(|_| String::from("parakeet runtime lock is poisoned"))?;
    let transcript = guard
        .transcribe(transcription_input)
        .map_err(|error| format!("utterance transcription failed: {error:?}"))?;

    Ok(Some(transcript.text().to_string()))
}

fn build_mark_silence_response(
    voice_pipeline_state: &Mutex<voxgolem_core::voice_pipeline::VoicePipelineState>,
    action: &voxgolem_core::voice_pipeline::VoicePipelineAction,
    transcript_text: Option<String>,
    telemetry: Option<RuntimeTelemetryPayload>,
) -> Result<RuntimePhaseResponsePayload, String> {
    let guard = voice_pipeline_state
        .lock()
        .map_err(|_| String::from("voice pipeline lock is poisoned"))?;

    Ok(runtime_phase_response_from_state(
        &guard,
        transcription_ready_samples(action),
        transcript_text,
        telemetry,
    ))
}

fn reset_voice_pipeline_to_waiting(
    voice_pipeline_state: &Mutex<voxgolem_core::voice_pipeline::VoicePipelineState>,
    wake_word_runtime: &Option<Mutex<wake_word::WakeWordRuntime>>,
    voice_activity_runtime: &Option<Mutex<voice_activity::VoiceActivityRuntime>>,
    voice_pipeline_config: voxgolem_core::voice_pipeline::VoicePipelineConfig,
) -> Result<(), String> {
    apply_voice_pipeline_transition_with_input_runtime_reset(
        voice_pipeline_state,
        wake_word_runtime,
        voice_activity_runtime,
        voice_pipeline_config,
        voxgolem_core::voice_pipeline::VoicePipelineEvent::ResetToIdle,
    )?;

    Ok(())
}

fn shutdown_llama_cpp_runtime_for_exit(app_state: &AppState) {
    let runtime = app_state
        .llama_cpp_runtime
        .lock()
        .map(|mut guard| guard.take())
        .unwrap_or(None);

    if let Some(mut runtime) = runtime {
        runtime.shutdown_owned();
    }
}

fn shutdown_completion_runtime_for_exit(app_state: &AppState) {
    let runtime = {
        let Ok(_lifecycle) = app_state.completion_lifecycle_lock.lock() else {
            return;
        };
        if let Ok(mut request) = app_state.completion_request.lock() {
            if let Some(request) = request.take() {
                request.clear();
            }
        }
        if let Ok(mut context) = app_state.completion_context.lock() {
            context.take();
        }
        app_state
            .completion_runtime
            .lock()
            .map(|mut runtime| runtime.take())
            .unwrap_or(None)
    };
    if let Some(mut runtime) = runtime {
        let _ = tauri::async_runtime::block_on(runtime.shutdown());
    }
}

fn shutdown_prefetch_for_exit(app_state: &AppState) {
    app_state.prefetch_generation.fetch_add(1, Ordering::SeqCst);
    let active = app_state
        .prefetch_task
        .lock()
        .map(|mut task| task.take())
        .unwrap_or(None);
    if let Some(mut active) = active {
        active.cancelled.store(true, Ordering::SeqCst);
        active.cancellation_signal.send_replace(true);
        if let Some(mut task) = active.task.take() {
            if tauri::async_runtime::block_on(tokio::time::timeout(
                Duration::from_secs(3),
                &mut task,
            ))
            .is_err()
            {
                task.abort();
            }
        }
    }
}

fn invalidate_runtime_startups_for_exit(app_state: &AppState) {
    app_state
        .response_profile_switch_generation
        .fetch_add(1, Ordering::SeqCst);
    app_state
        .tts_startup_generation
        .fetch_add(1, Ordering::SeqCst);
}

fn shutdown_tts_runtime_for_exit(app_state: &AppState) {
    let _operation = tauri::async_runtime::block_on(app_state.tts_operation_lock.lock());
    if let Ok(mut runtime) = app_state.local_tts_runtime.lock() {
        if let Some(runtime) = runtime.as_ref() {
            runtime.cancel_generation();
        }
        if let Some(runtime) = runtime.take() {
            if let Ok(mut runtime) = Arc::try_unwrap(runtime) {
                runtime.shutdown_bounded();
            }
        }
    }
}

fn runtime_startup_is_current(
    exit_cleanup_started: &AtomicBool,
    generation: &AtomicU64,
    expected_generation: u64,
) -> bool {
    !exit_cleanup_started.load(Ordering::SeqCst)
        && generation.load(Ordering::SeqCst) == expected_generation
}

fn store_completion_runtime(
    lifecycle_lock: &Mutex<()>,
    runtime_slot: &Mutex<Option<voxgolem_platform::completion::CompletionRuntime>>,
    request_slot: &Mutex<Option<voxgolem_platform::completion::CompletionRequestHandle>>,
    exit_cleanup_started: &AtomicBool,
    runtime: voxgolem_platform::completion::CompletionRuntime,
    request: voxgolem_platform::completion::CompletionRequestHandle,
) -> Option<voxgolem_platform::completion::CompletionRuntime> {
    let Ok(_lifecycle) = lifecycle_lock.lock() else {
        return Some(runtime);
    };
    let Ok(mut request_slot) = request_slot.lock() else {
        return Some(runtime);
    };
    let Ok(mut runtime_slot) = runtime_slot.lock() else {
        return Some(runtime);
    };
    if exit_cleanup_started.load(Ordering::SeqCst) {
        return Some(runtime);
    }
    *request_slot = Some(request);
    *runtime_slot = Some(runtime);
    None
}

fn shutdown_llama_start_result(
    start_result: Result<
        voxgolem_platform::llama_cpp::LlamaCppRuntime,
        voxgolem_platform::llama_cpp::LlamaCppRuntimeError,
    >,
) {
    if let Ok(mut runtime) = start_result {
        runtime.shutdown_owned();
    }
}

fn store_llama_runtime_if_current(
    mut runtime: voxgolem_platform::llama_cpp::LlamaCppRuntime,
    llama_cpp_runtime: &Arc<Mutex<Option<voxgolem_platform::llama_cpp::LlamaCppRuntime>>>,
    response_profile_switch_generation: &Arc<AtomicU64>,
    expected_generation: u64,
) -> bool {
    if response_profile_switch_generation.load(Ordering::SeqCst) != expected_generation {
        runtime.shutdown_owned();
        return false;
    }

    let Ok(mut guard) = llama_cpp_runtime.lock() else {
        runtime.shutdown_owned();
        return false;
    };

    if response_profile_switch_generation.load(Ordering::SeqCst) != expected_generation {
        runtime.shutdown_owned();
        return false;
    }

    *guard = Some(runtime);

    if response_profile_switch_generation.load(Ordering::SeqCst) != expected_generation {
        if let Some(mut stale_runtime) = guard.take() {
            stale_runtime.shutdown_owned();
        }
        return false;
    }

    true
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let builder = tauri::Builder::default()
        .manage(app_updates::PendingUpdate::default())
        .setup(|app| {
            let app_state = build_app_state(app.handle());
            let completion_config = app_state
                .runtime_config
                .as_ref()
                .and_then(|config| config.completion.clone());
            if let Some(config) = app_state.runtime_config.as_ref() {
                if let Some(opencode) = config.opencode.as_ref() {
                    match tauri::async_runtime::block_on(
                        voxgolem_platform::opencode::OpencodeServer::start(
                            voxgolem_platform::opencode::OpencodeServerConfig::new(&opencode.path),
                        ),
                    ) {
                        Ok(server) => {
                            *app_state
                                .opencode_server
                                .lock()
                                .expect("opencode server lock") = Some(server)
                        }
                        Err(error) => {
                            fail_startup_capability(
                                &app_state.lifecycle_gate,
                                &app_state.startup_state,
                                "opencode",
                                format!("failed to start OpenCode server: {error}"),
                            );
                        }
                    }
                }
            }
            app.manage(app_state);
            if let Some(config) = app.state::<AppState>().runtime_config.as_ref() {
                if resolve_effective_tts_enabled(
                    config.local_tts.enabled,
                    config.local_tts.model_path.is_file(),
                ) {
                    let tts_config = config.local_tts.clone();
                    let logging_enabled = config.logging.enabled;
                    let startup_generation = app
                        .state::<AppState>()
                        .tts_startup_generation
                        .fetch_add(1, Ordering::SeqCst)
                        .saturating_add(1);
                    let startup_generation_state =
                        Arc::clone(&app.state::<AppState>().tts_startup_generation);
                    let app_handle = app.handle().clone();
                    tauri::async_runtime::spawn_blocking(move || {
                        let state = app_handle.state::<AppState>();
                        let _operation_guard =
                            tauri::async_runtime::block_on(state.tts_operation_lock.lock());
                        if !runtime_startup_is_current(
                            &state.exit_cleanup_started,
                            &startup_generation_state,
                            startup_generation,
                        ) {
                            return;
                        }
                        let result =
                            initialize_local_tts_runtime(&tts_config, true, logging_enabled);
                        if !runtime_startup_is_current(
                            &state.exit_cleanup_started,
                            &startup_generation_state,
                            startup_generation,
                        ) {
                            if let Ok(mut runtime) = result {
                                if let Some(mut runtime) = runtime.take() {
                                    runtime.shutdown_owned();
                                }
                            }
                            return;
                        }
                        let Ok(_lifecycle) = state.lifecycle_gate.lock() else {
                            if let Ok(Some(mut runtime)) = result {
                                runtime.shutdown_owned();
                            }
                            return;
                        };
                        if !runtime_startup_is_current(
                            &state.exit_cleanup_started,
                            &startup_generation_state,
                            startup_generation,
                        ) {
                            if let Ok(Some(mut runtime)) = result {
                                runtime.shutdown_owned();
                            }
                            return;
                        }
                        match result {
                            Ok(Some(runtime)) => {
                                let runtime = Arc::new(runtime);
                                if let Ok(mut slot) = state.local_tts_runtime.lock() {
                                    *slot = Some(Arc::clone(&runtime));
                                }
                                if let Ok(mut startup) = state.startup_state.lock() {
                                    if let StartupStatePayload::Ready {
                                        capabilities,
                                        tts_enabled,
                                        ..
                                    }
                                    | StartupStatePayload::WarmingModel {
                                        capabilities,
                                        tts_enabled,
                                        ..
                                    } = &mut *startup
                                    {
                                        *tts_enabled = true;
                                        repair_tts_capability(
                                            capabilities,
                                            true,
                                            Some(runtime.as_ref()),
                                        );
                                    }
                                }
                            }
                            Ok(None) | Err(_) => {
                                if let Ok(mut startup) = state.startup_state.lock() {
                                    if let StartupStatePayload::Ready { capabilities, .. }
                                    | StartupStatePayload::WarmingModel {
                                        capabilities, ..
                                    } = &mut *startup
                                    {
                                        mark_capability(
                                            capabilities,
                                            "tts",
                                            CapabilityStatePayload::Failed,
                                            String::from("TTS runtime failed to initialize"),
                                        );
                                    }
                                }
                            }
                        }
                    });
                }
            }
            #[cfg(target_os = "linux")]
            configure_linux_microphone_permission(app)?;
            if let Some(config) = completion_config {
                let app_handle = app.handle().clone();
                tauri::async_runtime::spawn(async move {
                    match voxgolem_platform::completion::CompletionRuntime::start_with_policy(
                        &config.server_path,
                        &config.model_path,
                        platform_inference_policy(config.inference_provider),
                    )
                    .await
                    {
                        Ok(runtime) => {
                            let actual_provider =
                                actual_inference_provider_name(runtime.actual_provider());
                            let predictor = runtime.client().predictor();
                            let request = predictor.request_handle();
                            let context = {
                                let state = app_handle.state::<AppState>();
                                if let Some(mut runtime) = store_completion_runtime(
                                    &state.completion_lifecycle_lock,
                                    &state.completion_runtime,
                                    &state.completion_request,
                                    &state.exit_cleanup_started,
                                    runtime,
                                    request,
                                ) {
                                    let _ = runtime.shutdown().await;
                                    return;
                                }
                                Arc::clone(&state.completion_context)
                            };
                            set_startup_capability_provider(
                                &app_handle.state::<AppState>().lifecycle_gate,
                                &app_handle.state::<AppState>().startup_state,
                                "qwen_prediction",
                                actual_provider,
                            );
                            emit_completion_predictions(app_handle, predictor, context).await;
                        }
                        Err(error) => fail_startup_capability(
                            &app_handle.state::<AppState>().lifecycle_gate,
                            &app_handle.state::<AppState>().startup_state,
                            "qwen_prediction",
                            format!("failed to start completion runtime: {error}"),
                        ),
                    }
                });
            }
            eprintln!("{STARTUP_READY_MARKER}");
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_app_snapshot,
            app_updates::check_for_update,
            app_updates::install_update,
            app_updates::restart_for_update,
            get_startup_state,
            set_tts_enabled,
            synthesize_local_tts,
            get_ui_text_size,
            set_ui_text_size,
            get_ui_theme,
            set_ui_theme,
            get_assistant_settings,
            set_assistant_settings,
            switch_response_profile,
            record_frontend_runtime_diagnostic,
            submit_prompt,
            cancel_prompt,
            record_speech_activity,
            ingest_audio_frame,
            mark_silence,
            reset_session,
            request_completion,
            clear_completion
        ]);

    let app = match builder.build(tauri::generate_context!()) {
        Ok(app) => app,
        Err(error) => {
            eprintln!("failed to build vox-golem tauri shell: {error}");
            std::process::exit(1);
        }
    };

    #[cfg(unix)]
    install_unix_signal_exit_handlers(app.handle());

    app.run(|app_handle, event| {
        if let tauri::RunEvent::ExitRequested { ref api, .. } = event {
            if app_handle
                .state::<app_updates::PendingUpdate>()
                .handle_exit_request()
            {
                api.prevent_exit();
                return;
            }
        }
        if matches!(event, tauri::RunEvent::Exit) {
            let app_state = app_handle.state::<AppState>();
            if !app_state.exit_cleanup_started.swap(true, Ordering::SeqCst) {
                invalidate_runtime_startups_for_exit(&app_state);
                shutdown_prefetch_for_exit(&app_state);
                shutdown_llama_startups_for_exit(&app_state);
                shutdown_llama_cpp_runtime_for_exit(&app_state);
                shutdown_completion_runtime_for_exit(&app_state);
                shutdown_tts_runtime_for_exit(&app_state);
            }
            let opencode_server = app_state
                .opencode_server
                .lock()
                .expect("opencode server lock")
                .take();
            if let Some(server) = opencode_server {
                let _ = tauri::async_runtime::block_on(server.shutdown());
            }
        }
    });
}

fn allows_user_media(audio: bool, video: bool) -> bool {
    audio && !video
}

fn allows_user_media_from_uri(uri: Option<&str>, audio: bool, video: bool) -> bool {
    let trusted = uri.is_some_and(is_trusted_user_media_origin);
    trusted && allows_user_media(audio, video)
}

fn is_trusted_user_media_origin(uri: &str) -> bool {
    let (scheme, rest) = uri.split_once("://").unwrap_or(("", ""));
    let (authority, _) = rest.split_once(['/', '?', '#']).unwrap_or((rest, ""));
    if authority.contains('@') {
        return false;
    }
    match scheme {
        "tauri" => authority == "localhost",
        "http" => cfg!(debug_assertions) && authority == "localhost:5173",
        _ => false,
    }
}

fn shutdown_llama_startups_for_exit(app_state: &AppState) {
    let startups = app_state
        .llama_startups
        .lock()
        .expect("llama startup lock")
        .drain(..)
        .collect::<Vec<_>>();
    for (cancellation, worker) in startups {
        cancellation.cancel();
        let _ = worker.join();
    }
}

#[cfg(unix)]
fn install_unix_signal_exit_handlers(app_handle: &tauri::AppHandle) {
    for kind in [
        tokio::signal::unix::SignalKind::terminate(),
        tokio::signal::unix::SignalKind::interrupt(),
        tokio::signal::unix::SignalKind::hangup(),
        tokio::signal::unix::SignalKind::quit(),
    ] {
        let app_handle = app_handle.clone();
        tauri::async_runtime::spawn(async move {
            let Ok(mut signals) = tokio::signal::unix::signal(kind) else {
                return;
            };
            if signals.recv().await.is_some() {
                app_handle.exit(0);
            }
        });
    }
}

fn register_llama_startup(
    registry: &LlamaStartupRegistry,
    exit_cleanup_started: &AtomicBool,
    cancellation: voxgolem_platform::llama_cpp::LlamaCppStartupCancellation,
    worker: std::thread::JoinHandle<()>,
) {
    let mut guard = registry.lock().expect("llama startup lock");
    let mut finished = Vec::new();
    let mut pending = Vec::new();
    std::mem::swap(&mut *guard, &mut pending);
    for entry in pending {
        if entry.1.is_finished() {
            finished.push(entry);
        } else {
            guard.push(entry);
        }
    }
    drop(guard);
    for (_, handle) in finished {
        let _ = handle.join();
    }
    match register_owned_work(registry, exit_cleanup_started, (cancellation, worker)) {
        Ok(()) => {}
        Err((cancellation, worker)) => {
            cancellation.cancel();
            let _ = worker.join();
        }
    }
}

fn register_owned_work<T>(
    registry: &Mutex<Vec<T>>,
    exit_cleanup_started: &AtomicBool,
    entry: T,
) -> Result<(), T> {
    let Ok(mut registry) = registry.lock() else {
        return Err(entry);
    };
    if exit_cleanup_started.load(Ordering::SeqCst) {
        return Err(entry);
    }
    registry.push(entry);
    Ok(())
}

#[cfg(target_os = "linux")]
fn configure_linux_microphone_permission(app: &mut tauri::App) -> tauri::Result<()> {
    if let Some(webview_window) = app.get_webview_window("main") {
        webview_window.with_webview(|webview| {
            webview
                .inner()
                .connect_permission_request(|webview, request| {
                    let Some(user_media_request) =
                        request.downcast_ref::<UserMediaPermissionRequest>()
                    else {
                        return false;
                    };
                    let audio = user_media_request.is_for_audio_device();
                    let video = user_media_request.is_for_video_device();
                    if allows_user_media_from_uri(webview.uri().as_deref(), audio, video) {
                        request.allow();
                    } else {
                        request.deny();
                    }
                    true
                });
        })?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};
    use std::fs;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::time::Duration;

    use super::{
        agent_history, allows_user_media_from_uri, apply_optional_speech_activity,
        assistant_completion_enabled, atomic_replace_state_file, bounded_capability_reason,
        bounded_provider_history, build_ipc_app_snapshot, build_mark_silence_response,
        build_nonfatal_config_error_app_state, build_startup_error_app_state,
        claim_cancelled_prompt_publication, cleanup_orphan_state_temps_with, cleanup_sequential,
        clear_active_prompt, clear_completion_request_state_locked, current_runtime_phase_response,
        current_silence_deadline, default_response_profile, default_voice_pipeline_config,
        ensure_app_running, ensure_assistant_settings_available,
        fit_review_history_to_prompt_budget, get_app_snapshot_from_state,
        ingest_audio_frame_with_optional_wake_word_detection, initial_stage_sequence,
        invalidate_runtime_startups_for_exit, load_llama_cpp_system_prompt, load_persisted_state,
        load_persisted_tts_enabled, load_persisted_ui_text_size, load_persisted_ui_theme,
        lock_live_settings_operation, lock_persisted_state_file,
        lock_ready_response_backend_operation, merge_profile_startup_publication,
        model_path_for_profile, parse_deep_agent_json, parse_persisted_state,
        parse_review_agent_json, persist_assistant_settings,
        persist_prepared_local_instant_model_with, persist_selected_response_profile,
        persist_tts_enabled, persist_ui_text_size, persist_ui_theme,
        prepare_local_instant_model_with, process_wake_word_frame, race_durable_cancellation,
        record_terminal_kind, register_active_prompt, register_owned_work,
        replace_owned_runtime_transaction, reset_conversation_execution,
        reset_voice_pipeline_to_waiting, reset_wake_word_runtime, resolve_effective_tts_enabled,
        response_profile_state_path, runtime_log_path, runtime_phase_response_from_state,
        runtime_startup_is_current, shutdown_llama_cpp_runtime_for_exit,
        shutdown_tts_runtime_for_exit, startup_warming_state_from_snapshot,
        supported_response_profiles, take_and_invalidate_prefetch, to_runtime_phase_payload,
        transcribe_finished_utterance, transcription_ready_samples,
        tts_runtime_needs_initialization, update_restored_profile_capabilities,
        validate_prompt_request_id, validate_prompt_text, wake_word_event_timestamp,
        ActivePromptGuard, AgentChoicePayload, AssistantSettingsPayload, CapabilityPayload,
        CapabilityStatePayload, CommitAssistantResult, CueAssetPathsPayload, DeepStageResult,
        DeepTask, InstantChoicePayload, PrefetchEntry, PrefetchKey, ProfilePersistenceRollback,
        PromptExecutionEventPayload, ResponseProfilePayload, RuntimePhasePayload,
        RuntimePhaseResponsePayload, RuntimeReplacement, RuntimeTelemetryPayload, StagePayload,
        StageStatusPayload, StartupSnapshot, StartupStatePayload, SupervisedCreation, TerminalKind,
        UiPreferencesState, UiTextSizePayload, UiThemePayload, DEFAULT_SILENCE_TIMEOUT_MS,
        PROMPT_MAX_BYTES, PROVIDER_HISTORY_MAX_BYTES,
    };

    #[derive(Debug, PartialEq, Eq)]
    struct FakeRuntime {
        name: &'static str,
        owned: bool,
    }

    fn fake_runtime(name: &'static str, owned: bool) -> FakeRuntime {
        FakeRuntime { name, owned }
    }

    fn run_fake_runtime_transaction<V>(
        current: Option<FakeRuntime>,
        starts: Vec<Result<FakeRuntime, &'static str>>,
        is_current: impl Fn() -> bool,
        prepare: impl FnOnce(&FakeRuntime) -> Result<V, String>,
    ) -> (RuntimeReplacement<FakeRuntime, V>, Vec<String>) {
        let events = RefCell::new(Vec::new());
        let mut starts = starts.into_iter();
        let result = replace_owned_runtime_transaction(
            current,
            ("requested", "old"),
            |runtime| runtime.owned,
            is_current,
            |runtime| events.borrow_mut().push(format!("stop:{}", runtime.name)),
            |spec| {
                events.borrow_mut().push(format!("start:{spec}"));
                starts.next().expect("fake startup result")
            },
            prepare,
        );
        assert!(
            starts.next().is_none(),
            "all fake startup results should be used"
        );
        (result, events.into_inner())
    }

    #[test]
    fn owned_runtime_transaction_stops_old_runtime_and_commits_requested_runtime() {
        let (result, events) = run_fake_runtime_transaction(
            Some(fake_runtime("old", true)),
            vec![Ok(fake_runtime("requested", true))],
            || true,
            |_| Ok("prepared"),
        );

        assert_eq!(
            result,
            RuntimeReplacement::Applied {
                runtime: fake_runtime("requested", true),
                value: "prepared",
            }
        );
        assert_eq!(events, ["stop:old", "start:requested"]);
    }

    #[test]
    fn owned_runtime_transaction_restores_old_runtime_after_requested_start_failure() {
        let (result, events) = run_fake_runtime_transaction(
            Some(fake_runtime("old", true)),
            vec![Err("requested failed"), Ok(fake_runtime("old", true))],
            || true,
            |_| Ok(()),
        );

        assert_eq!(
            result,
            RuntimeReplacement::Restored {
                runtime: fake_runtime("old", true),
                error: String::from("requested failed"),
            }
        );
        assert_eq!(events, ["stop:old", "start:requested", "start:old"]);
    }

    #[test]
    fn owned_runtime_transaction_restores_old_runtime_after_preparation_failure() {
        let (result, events) = run_fake_runtime_transaction(
            Some(fake_runtime("old", true)),
            vec![
                Ok(fake_runtime("requested", true)),
                Ok(fake_runtime("old", true)),
            ],
            || true,
            |_| Err::<(), _>(String::from("preparation failed")),
        );

        assert_eq!(
            result,
            RuntimeReplacement::Restored {
                runtime: fake_runtime("old", true),
                error: String::from("preparation failed"),
            }
        );
        assert_eq!(
            events,
            ["stop:old", "start:requested", "stop:requested", "start:old"]
        );
    }

    #[test]
    fn owned_runtime_transaction_reports_restore_failure() {
        let (result, events) = run_fake_runtime_transaction(
            Some(fake_runtime("old", true)),
            vec![Err("requested failed"), Err("old failed")],
            || true,
            |_| Ok(()),
        );

        assert_eq!(
            result,
            RuntimeReplacement::RestoreFailed {
                error: String::from("requested failed"),
                restore_error: String::from("old failed"),
            }
        );
        assert_eq!(events, ["stop:old", "start:requested", "start:old"]);
    }

    #[test]
    fn owned_runtime_transaction_rejects_external_runtime_without_mutation() {
        let (result, events) = run_fake_runtime_transaction(
            Some(fake_runtime("external", false)),
            vec![],
            || true,
            |_| Ok(()),
        );

        assert_eq!(
            result,
            RuntimeReplacement::RejectedExistingExternal {
                runtime: fake_runtime("external", false),
            }
        );
        assert!(events.is_empty());
    }

    #[test]
    fn owned_runtime_transaction_rejects_external_requested_runtime_from_empty_slot() {
        let prepared = Cell::new(false);

        let (result, events) = run_fake_runtime_transaction(
            None,
            vec![Ok(fake_runtime("requested", false))],
            || true,
            |_| {
                prepared.set(true);
                Ok(())
            },
        );

        assert_eq!(result, RuntimeReplacement::RejectedStartedExternal);
        assert_eq!(events, ["start:requested"]);
        assert!(!prepared.get());
    }

    #[test]
    fn owned_runtime_transaction_rejects_external_requested_runtime_after_stopping_owned_runtime() {
        let prepared = Cell::new(false);

        let (result, events) = run_fake_runtime_transaction(
            Some(fake_runtime("old", true)),
            vec![Ok(fake_runtime("requested", false))],
            || true,
            |_| {
                prepared.set(true);
                Ok(())
            },
        );

        assert_eq!(result, RuntimeReplacement::RejectedStartedExternal);
        assert_eq!(events, ["stop:old", "start:requested"]);
        assert!(!prepared.get());
    }

    #[test]
    fn owned_runtime_transaction_rejects_external_fallback_runtime() {
        let prepared = Cell::new(false);

        let (result, events) = run_fake_runtime_transaction(
            Some(fake_runtime("old", true)),
            vec![Err("requested failed"), Ok(fake_runtime("old", false))],
            || true,
            |_| {
                prepared.set(true);
                Ok(())
            },
        );

        assert_eq!(result, RuntimeReplacement::RejectedStartedExternal);
        assert_eq!(events, ["stop:old", "start:requested", "start:old"]);
        assert!(!prepared.get());
    }

    #[test]
    fn owned_runtime_transaction_stops_requested_runtime_when_superseded_after_preparation() {
        let current_checks = Cell::new(0);

        let (result, events) = run_fake_runtime_transaction(
            Some(fake_runtime("old", true)),
            vec![Ok(fake_runtime("requested", true))],
            || {
                let check = current_checks.get();
                current_checks.set(check + 1);
                check == 0
            },
            |_| Ok(()),
        );

        assert_eq!(result, RuntimeReplacement::Superseded);
        assert_eq!(events, ["stop:old", "start:requested", "stop:requested"]);
    }

    #[test]
    fn tts_preparation_only_initializes_when_enabling_without_runtime() {
        assert!(tts_runtime_needs_initialization(true, false));
        assert!(!tts_runtime_needs_initialization(true, true));
        assert!(!tts_runtime_needs_initialization(false, false));
        assert!(!tts_runtime_needs_initialization(false, true));
    }

    #[test]
    fn ipc_app_snapshot_maps_one_reachable_desktop_state() {
        let mut session = voxgolem_core::conversation::ConversationSession::new(
            AssistantSettingsPayload {
                instant: InstantChoicePayload::CustomSolHigh,
                deep: AgentChoicePayload::OpenCodeSolHigh,
                review: AgentChoicePayload::CustomLunaLow,
                deep_enabled: true,
                review_enabled: false,
                prefetch: true,
                completion: false,
            }
            .into(),
        );
        let execution = session
            .admit("request-1", "hello")
            .expect("prompt should be admitted");
        let startup = StartupStatePayload::Ready {
            cue_asset_paths: super::embedded_cue_asset_paths(),
            runtime_phase: RuntimePhasePayload::Sleeping,
            voice_input_available: false,
            voice_input_error: None,
            silence_timeout_ms: DEFAULT_SILENCE_TIMEOUT_MS,
            selected_response_profile: ResponseProfilePayload::Quality,
            supported_response_profiles: vec![
                ResponseProfilePayload::Fast,
                ResponseProfilePayload::Quality,
            ],
            prompt_cancellation_available: true,
            tts_enabled: true,
            tts_output_gain_db: 3.0,
            capabilities: vec![
                CapabilityPayload {
                    id: "custom_provider",
                    state: CapabilityStatePayload::Available,
                    reason: String::from("ready"),
                    actual_provider: Some("cpu"),
                },
                CapabilityPayload {
                    id: "opencode",
                    state: CapabilityStatePayload::Failed,
                    reason: String::from("not running"),
                    actual_provider: None,
                },
            ],
        };

        let snapshot = build_ipc_app_snapshot(
            session.snapshot(),
            session.policy(),
            voxgolem_core::runtime::RuntimePhase::Executing,
            &startup,
            ResponseProfilePayload::Quality,
            UiPreferencesState {
                theme: UiThemePayload::Light,
                text_size: UiTextSizePayload::Large,
            },
        );

        assert_eq!(snapshot.session_id.as_str(), "1");
        assert_eq!(
            snapshot
                .active_request
                .as_ref()
                .expect("active request")
                .execution_id
                .generation
                .as_str(),
            execution.execution_id.generation.0.to_string()
        );
        assert_eq!(
            snapshot.runtime_phase,
            voxgolem_core::ipc::RuntimePhase::Executing
        );
        assert_eq!(
            snapshot.preferences.response_profile,
            voxgolem_core::ipc::ResponseProfile::Quality
        );
        assert!(snapshot.preferences.tts_enabled);
        assert_eq!(
            snapshot.preferences.ui_theme,
            voxgolem_core::ipc::UiTheme::Light
        );
        assert_eq!(
            snapshot.preferences.ui_text_size,
            voxgolem_core::ipc::UiTextSize::Large
        );
        assert_eq!(snapshot.capabilities.len(), 2);
        assert_eq!(snapshot.model_options.len(), 6);
        assert!(snapshot.model_options[2].available);
        assert!(!snapshot.model_options[4].available);
        assert_eq!(
            snapshot.model_options[4]
                .unavailable_reason
                .as_ref()
                .expect("unavailable reason")
                .as_str(),
            "not running"
        );
    }

    #[test]
    fn app_snapshot_waits_for_an_atomic_lifecycle_publication() {
        let state = std::sync::Arc::new(build_nonfatal_config_error_app_state(
            default_voice_pipeline_config(),
            super::embedded_cue_asset_paths(),
            String::from("not configured"),
        ));
        let lifecycle = state.lifecycle_gate.lock().expect("lifecycle gate");
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (snapshot_tx, snapshot_rx) = std::sync::mpsc::channel();
        let snapshot_state = std::sync::Arc::clone(&state);
        let snapshot_thread = std::thread::spawn(move || {
            started_tx.send(()).expect("started signal");
            snapshot_tx
                .send(get_app_snapshot_from_state(&snapshot_state))
                .expect("snapshot result");
        });
        started_rx.recv().expect("snapshot attempt should start");
        assert!(snapshot_rx.recv_timeout(Duration::from_millis(20)).is_err());

        let mut session = state
            .conversation_session
            .lock()
            .expect("conversation session");
        session.reset();
        let mut policy = session.policy().clone();
        policy.instant_model = voxgolem_core::assistant::InstantModel::LocalQuality;
        session
            .replace_policy(policy)
            .expect("idle session should accept policy");
        drop(session);
        state.ui_preferences.lock().expect("UI preferences").theme = UiThemePayload::Light;
        *state
            .selected_response_profile
            .lock()
            .expect("response profile") = ResponseProfilePayload::Quality;
        if let StartupStatePayload::Ready { tts_enabled, .. } =
            &mut *state.startup_state.lock().expect("startup state")
        {
            *tts_enabled = true;
        }
        drop(lifecycle);

        let snapshot = snapshot_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("snapshot should finish")
            .expect("snapshot should be valid");
        snapshot_thread.join().expect("snapshot thread");
        assert_eq!(snapshot.session_id.as_str(), "2");
        assert_eq!(
            snapshot.preferences.response_profile,
            voxgolem_core::ipc::ResponseProfile::Quality
        );
        assert_eq!(
            snapshot.preferences.ui_theme,
            voxgolem_core::ipc::UiTheme::Light
        );
        assert!(snapshot.preferences.tts_enabled);
    }

    #[test]
    fn exit_flag_rejects_work_waiting_to_register() {
        let registry = Arc::new(Mutex::new(Vec::new()));
        let exit = Arc::new(AtomicBool::new(false));
        let guard = registry.lock().expect("registry lock");
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let registry_for_thread = Arc::clone(&registry);
        let exit_for_thread = Arc::clone(&exit);
        let worker = std::thread::spawn(move || {
            started_tx.send(()).expect("started signal");
            register_owned_work(&registry_for_thread, &exit_for_thread, 7)
        });
        started_rx.recv().expect("registration should start");
        exit.store(true, Ordering::SeqCst);
        drop(guard);

        assert_eq!(worker.join().expect("registration thread"), Err(7));
        assert!(registry.lock().expect("registry lock").is_empty());
    }

    #[test]
    fn runtime_startup_publication_requires_current_generation_and_live_app() {
        let exit = AtomicBool::new(false);
        let generation = AtomicU64::new(4);
        assert!(runtime_startup_is_current(&exit, &generation, 4));
        assert!(!runtime_startup_is_current(&exit, &generation, 3));
        exit.store(true, Ordering::SeqCst);
        assert!(!runtime_startup_is_current(&exit, &generation, 4));
    }

    #[test]
    fn tts_exit_cleanup_waits_for_the_startup_operation() {
        let state = Arc::new(build_startup_error_app_state(
            default_voice_pipeline_config(),
            String::from("test"),
        ));
        let operation = tauri::async_runtime::block_on(state.tts_operation_lock.lock());
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let cleanup_state = Arc::clone(&state);
        let cleanup = std::thread::spawn(move || {
            shutdown_tts_runtime_for_exit(&cleanup_state);
            done_tx.send(()).expect("cleanup signal");
        });
        assert!(done_rx.recv_timeout(Duration::from_millis(20)).is_err());
        drop(operation);
        done_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("cleanup should finish");
        cleanup.join().expect("cleanup thread");
    }

    #[test]
    fn queued_runtime_operations_recheck_exit_after_locking() {
        let settings_state = Arc::new(build_startup_error_app_state(
            default_voice_pipeline_config(),
            String::from("test"),
        ));
        let settings_operation = tauri::async_runtime::block_on(
            Arc::clone(&settings_state.settings_operation_lock).lock_owned(),
        );
        let (settings_started_tx, settings_started_rx) = std::sync::mpsc::channel();
        let (settings_tx, settings_rx) = std::sync::mpsc::channel();
        let queued_settings_state = Arc::clone(&settings_state);
        let settings_worker = std::thread::spawn(move || {
            settings_started_tx.send(()).expect("settings started");
            let result = tauri::async_runtime::block_on(lock_live_settings_operation(
                &queued_settings_state,
            ));
            settings_tx.send(result.map(drop)).expect("settings result");
        });
        settings_started_rx.recv().expect("settings should start");
        assert!(settings_rx.recv_timeout(Duration::from_millis(20)).is_err());
        settings_state
            .exit_cleanup_started
            .store(true, Ordering::SeqCst);
        drop(settings_operation);
        assert_eq!(
            settings_rx
                .recv_timeout(Duration::from_secs(1))
                .expect("settings result"),
            Err(String::from("application shutdown is in progress"))
        );
        settings_worker.join().expect("settings worker");

        let profile_state = Arc::new(build_nonfatal_config_error_app_state(
            default_voice_pipeline_config(),
            super::embedded_cue_asset_paths(),
            String::from("test"),
        ));
        let profile_operation = profile_state
            .response_backend_operation_lock
            .lock()
            .expect("profile operation lock");
        let (profile_tx, profile_rx) = std::sync::mpsc::channel();
        let queued_profile_state = Arc::clone(&profile_state);
        let profile_worker = std::thread::spawn(move || {
            let result = lock_ready_response_backend_operation(
                &queued_profile_state.response_backend_operation_lock,
                &queued_profile_state.startup_state,
            )
            .and_then(|_guard| ensure_app_running(&queued_profile_state.exit_cleanup_started));
            profile_tx.send(result).expect("profile result");
        });
        profile_state
            .exit_cleanup_started
            .store(true, Ordering::SeqCst);
        drop(profile_operation);
        assert_eq!(
            profile_rx
                .recv_timeout(Duration::from_secs(1))
                .expect("profile result"),
            Err(String::from("application shutdown is in progress"))
        );
        profile_worker.join().expect("profile worker");

        let tts_state = Arc::new(build_startup_error_app_state(
            default_voice_pipeline_config(),
            String::from("test"),
        ));
        let tts_operation = tauri::async_runtime::block_on(tts_state.tts_operation_lock.lock());
        let (tts_tx, tts_rx) = std::sync::mpsc::channel();
        let queued_tts_state = Arc::clone(&tts_state);
        let tts_worker = std::thread::spawn(move || {
            let result = tauri::async_runtime::block_on(async {
                let _guard = queued_tts_state.tts_operation_lock.lock().await;
                ensure_app_running(&queued_tts_state.exit_cleanup_started)
            });
            tts_tx.send(result).expect("TTS result");
        });
        tts_state.exit_cleanup_started.store(true, Ordering::SeqCst);
        drop(tts_operation);
        assert_eq!(
            tts_rx
                .recv_timeout(Duration::from_secs(1))
                .expect("TTS result"),
            Err(String::from("application shutdown is in progress"))
        );
        tts_worker.join().expect("TTS worker");
    }

    #[test]
    fn oversized_capability_reasons_use_an_explicit_diagnostic() {
        for reason in [
            "x".repeat(voxgolem_core::ipc::DISPLAY_TEXT_MAX_BYTES + 1),
            "界".repeat(voxgolem_core::ipc::DISPLAY_TEXT_MAX_BYTES),
        ] {
            assert_eq!(
                bounded_capability_reason(&reason).as_str(),
                "details omitted; see local logs"
            );
        }
    }

    #[test]
    fn warming_profile_snapshot_disables_the_requested_model() {
        let startup_snapshot = StartupSnapshot {
            cue_asset_paths: super::embedded_cue_asset_paths(),
            voice_input_available: false,
            voice_input_error: None,
            silence_timeout_ms: DEFAULT_SILENCE_TIMEOUT_MS,
            tts_enabled: false,
            tts_output_gain_db: 3.0,
            supported_response_profiles: vec![
                ResponseProfilePayload::Fast,
                ResponseProfilePayload::Quality,
            ],
            capabilities: vec![CapabilityPayload {
                id: "local_quality",
                state: CapabilityStatePayload::Available,
                reason: String::from("ready"),
                actual_provider: Some("cpu"),
            }],
        };
        let warming =
            startup_warming_state_from_snapshot(&startup_snapshot, ResponseProfilePayload::Quality);
        let session = voxgolem_core::conversation::ConversationSession::new(
            AssistantSettingsPayload {
                instant: InstantChoicePayload::LocalQuality,
                ..AssistantSettingsPayload::default()
            }
            .into(),
        );
        let snapshot = build_ipc_app_snapshot(
            session.snapshot(),
            session.policy(),
            voxgolem_core::runtime::RuntimePhase::Sleeping,
            &warming,
            ResponseProfilePayload::Fast,
            UiPreferencesState {
                theme: UiThemePayload::Dark,
                text_size: UiTextSizePayload::Medium,
            },
        );
        let quality = snapshot
            .model_options
            .iter()
            .find(|option| option.id.as_str() == "local-quality")
            .expect("quality model option");
        assert!(!quality.available);
        assert_eq!(
            quality
                .unavailable_reason
                .as_ref()
                .expect("warming reason")
                .as_str(),
            "local model is loading"
        );
    }

    #[test]
    fn profile_publication_preserves_newer_unrelated_capabilities() {
        let capabilities = vec![
            CapabilityPayload {
                id: "local_fast",
                state: CapabilityStatePayload::Available,
                reason: String::from("ready"),
                actual_provider: Some("cpu"),
            },
            CapabilityPayload {
                id: "local_quality",
                state: CapabilityStatePayload::Warming,
                reason: String::from("local model is loading"),
                actual_provider: None,
            },
            CapabilityPayload {
                id: "qwen_prediction",
                state: CapabilityStatePayload::Available,
                reason: String::from("ready"),
                actual_provider: Some("cuda"),
            },
            CapabilityPayload {
                id: "tts",
                state: CapabilityStatePayload::Unavailable,
                reason: String::from("disabled"),
                actual_provider: None,
            },
        ];
        let mut current = StartupStatePayload::WarmingModel {
            cue_asset_paths: super::embedded_cue_asset_paths(),
            runtime_phase: RuntimePhasePayload::Initializing,
            voice_input_available: false,
            voice_input_error: None,
            silence_timeout_ms: DEFAULT_SILENCE_TIMEOUT_MS,
            message: String::from("Loading local Gemma model..."),
            selected_response_profile: ResponseProfilePayload::Quality,
            supported_response_profiles: vec![
                ResponseProfilePayload::Fast,
                ResponseProfilePayload::Quality,
            ],
            prompt_cancellation_available: false,
            tts_enabled: false,
            tts_output_gain_db: 3.0,
            capabilities,
        };
        let stale_profile_snapshot = StartupSnapshot {
            cue_asset_paths: super::embedded_cue_asset_paths(),
            voice_input_available: false,
            voice_input_error: None,
            silence_timeout_ms: DEFAULT_SILENCE_TIMEOUT_MS,
            tts_enabled: true,
            tts_output_gain_db: 3.0,
            supported_response_profiles: vec![
                ResponseProfilePayload::Fast,
                ResponseProfilePayload::Quality,
            ],
            capabilities: vec![
                CapabilityPayload {
                    id: "local_fast",
                    state: CapabilityStatePayload::Available,
                    reason: String::from("ready"),
                    actual_provider: Some("cpu"),
                },
                CapabilityPayload {
                    id: "local_quality",
                    state: CapabilityStatePayload::Available,
                    reason: String::from("ready"),
                    actual_provider: Some("cuda"),
                },
                CapabilityPayload {
                    id: "qwen_prediction",
                    state: CapabilityStatePayload::Warming,
                    reason: String::from("stale"),
                    actual_provider: None,
                },
                CapabilityPayload {
                    id: "tts",
                    state: CapabilityStatePayload::Available,
                    reason: String::from("stale"),
                    actual_provider: Some("cuda"),
                },
            ],
        };
        merge_profile_startup_publication(
            &mut current,
            super::startup_ready_state_from_snapshot(
                &stale_profile_snapshot,
                ResponseProfilePayload::Quality,
            ),
        );

        let StartupStatePayload::Ready {
            tts_enabled,
            capabilities,
            ..
        } = current
        else {
            panic!("profile publication should finish warming")
        };
        assert!(!tts_enabled);
        let capability = |id| {
            capabilities
                .iter()
                .find(|capability| capability.id == id)
                .expect("capability")
        };
        assert_eq!(
            capability("qwen_prediction").state,
            CapabilityStatePayload::Available
        );
        assert_eq!(capability("tts").state, CapabilityStatePayload::Unavailable);
        assert_eq!(
            capability("local_quality").state,
            CapabilityStatePayload::Available
        );
        assert_eq!(capability("local_quality").actual_provider, Some("cuda"));
    }

    #[test]
    fn user_media_policy_allows_audio_only() {
        assert!(super::allows_user_media(true, false));
        assert!(!super::allows_user_media(true, true));
        assert!(!super::allows_user_media(false, true));
        assert!(!super::allows_user_media(false, false));
        assert!(super::allows_user_media_from_uri(
            Some("tauri://localhost"),
            true,
            false
        ));
        assert!(!super::allows_user_media_from_uri(
            Some("https://evil.example"),
            true,
            false
        ));
        assert!(!super::allows_user_media_from_uri(None, true, false));
        assert!(!super::allows_user_media_from_uri(
            Some("tauri://localhost"),
            true,
            true
        ));
    }

    #[test]
    fn assistant_runtime_controls_follow_persisted_preferences() {
        let disabled = AssistantSettingsPayload {
            completion: false,
            ..AssistantSettingsPayload::default()
        };
        let coordinator = std::sync::Mutex::new(
            voxgolem_core::conversation::ConversationSession::new(disabled.into()),
        );
        let settings_generation = std::sync::atomic::AtomicU64::new(0);
        let lifecycle = std::sync::Mutex::new(());
        assert!(!assistant_completion_enabled(&coordinator).unwrap());

        let policy = prepare_local_instant_model_with(
            &coordinator,
            &lifecycle,
            &settings_generation,
            0,
            ResponseProfilePayload::Quality,
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            coordinator.lock().unwrap().policy().instant_model,
            voxgolem_core::assistant::InstantModel::LocalFast
        );
        assert_eq!(settings_generation.load(Ordering::SeqCst), 0);
        assert_eq!(policy.profile, ResponseProfilePayload::Quality);
        assert_eq!(policy.settings.instant, InstantChoicePayload::LocalQuality);
        coordinator
            .lock()
            .unwrap()
            .replace_policy(policy.policy)
            .unwrap();
        settings_generation.fetch_add(1, Ordering::SeqCst);
        assert!(!policy.settings.completion);
        assert!(prepare_local_instant_model_with(
            &coordinator,
            &lifecycle,
            &settings_generation,
            0,
            ResponseProfilePayload::Fast,
        )
        .unwrap()
        .is_none());
    }

    #[test]
    fn profile_persistence_failure_does_not_commit_preferences_or_generation() {
        let coordinator =
            std::sync::Mutex::new(voxgolem_core::conversation::ConversationSession::new(
                AssistantSettingsPayload::default().into(),
            ));
        let generation = std::sync::atomic::AtomicU64::new(4);
        let lifecycle = std::sync::Mutex::new(());
        let prepared = prepare_local_instant_model_with(
            &coordinator,
            &lifecycle,
            &generation,
            4,
            ResponseProfilePayload::Quality,
        )
        .expect("candidate derivation must succeed")
        .expect("candidate should remain current");

        let error = persist_prepared_local_instant_model_with(prepared, |_, _| {
            Err(String::from("injected persistence failure"))
        })
        .expect_err("persistence failure must be returned");

        assert_eq!(error, "injected persistence failure");
        assert_eq!(generation.load(Ordering::SeqCst), 4);
        assert_eq!(
            coordinator.lock().unwrap().policy().instant_model,
            voxgolem_core::assistant::InstantModel::LocalFast
        );
    }

    #[test]
    fn profile_persistence_does_not_hold_the_conversation_session_lock() {
        let coordinator =
            std::sync::Mutex::new(voxgolem_core::conversation::ConversationSession::new(
                AssistantSettingsPayload::default().into(),
            ));
        let generation = std::sync::atomic::AtomicU64::new(0);
        let lifecycle = std::sync::Mutex::new(());

        let prepared = prepare_local_instant_model_with(
            &coordinator,
            &lifecycle,
            &generation,
            0,
            ResponseProfilePayload::Quality,
        )
        .expect("profile synchronization should succeed")
        .expect("profile synchronization should remain current");
        persist_prepared_local_instant_model_with(prepared, |_, _| {
            assert!(coordinator.try_lock().is_ok());
            Ok(())
        })
        .expect("profile persistence should succeed");
    }

    #[test]
    fn profile_candidate_derivation_rejects_an_active_prompt() {
        let mut session = voxgolem_core::conversation::ConversationSession::new(
            AssistantSettingsPayload::default().into(),
        );
        session
            .admit("request", "prompt")
            .expect("prompt should start");
        let coordinator = std::sync::Mutex::new(session);
        let generation = std::sync::atomic::AtomicU64::new(0);
        let lifecycle = std::sync::Mutex::new(());

        let error = prepare_local_instant_model_with(
            &coordinator,
            &lifecycle,
            &generation,
            0,
            ResponseProfilePayload::Quality,
        )
        .expect_err("active prompt must reject profile preparation");
        assert_eq!(
            error,
            "assistant settings cannot change while a prompt is active"
        );
    }

    #[test]
    fn settings_publication_reservation_excludes_prompt_admission() {
        let app_state = super::build_nonfatal_config_error_app_state(
            default_voice_pipeline_config(),
            CueAssetPathsPayload {
                start_listening: String::from("start"),
                stop_listening: String::from("stop"),
            },
            String::from("test"),
        );
        app_state
            .settings_publication_in_progress
            .store(true, Ordering::Release);

        assert_eq!(
            super::begin_registered_assistant_request(
                &app_state,
                "request",
                "prompt",
                super::CompletionSource::Typed,
            )
            .err()
            .as_deref(),
            Some("assistant settings update is in progress")
        );
        app_state
            .settings_publication_in_progress
            .store(false, Ordering::Release);
        let registered = super::begin_registered_assistant_request(
            &app_state,
            "request",
            "prompt",
            super::CompletionSource::Typed,
        )
        .expect("admission should resume after settings publication");
        drop(registered);
    }

    #[test]
    fn user_media_policy_matches_only_trusted_origins() {
        assert!(allows_user_media_from_uri(
            Some("tauri://localhost/route?q=1#f"),
            true,
            false
        ));
        assert!(
            cfg!(debug_assertions)
                == allows_user_media_from_uri(Some("http://localhost:5173/nested"), true, false)
        );
        for uri in [
            "tauri://localhost.evil/",
            "tauri://user@localhost/",
            "tauri://localhost:443/",
            "https://localhost/",
            "http://localhost:5174/",
        ] {
            assert!(!allows_user_media_from_uri(Some(uri), true, false), "{uri}");
        }
    }
    use crate::wake_word::{WakeWordDetection, WakeWordRuntime};
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Mutex};

    static APPDATA_ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn prompt_request_ids_must_be_opaque_generated_identifiers() {
        assert!(validate_prompt_request_id("request-1721419200000-k3jf91").is_ok());
        assert!(validate_prompt_request_id("request-user-prompt-text").is_err());
        assert!(validate_prompt_request_id("release-notes").is_err());
    }

    #[test]
    fn assistant_settings_use_pinned_opencode_wire_names() {
        let settings = AssistantSettingsPayload {
            instant: InstantChoicePayload::OpenCodeSolHigh,
            deep: AgentChoicePayload::OpenCodeLunaLow,
            review: AgentChoicePayload::OpenCodeSolHigh,
            ..AssistantSettingsPayload::default()
        };

        let wire = serde_json::to_value(settings).expect("assistant settings should serialize");

        assert_eq!(wire["instant"], "opencode-sol-high");
        assert_eq!(wire["deep"], "opencode-luna-low");
        assert_eq!(wire["review"], "opencode-sol-high");
    }

    #[test]
    fn prompt_text_is_bounded_before_provider_dispatch() {
        assert!(validate_prompt_text("x".repeat(PROMPT_MAX_BYTES)).is_ok());
        assert!(validate_prompt_text("x".repeat(PROMPT_MAX_BYTES + 1)).is_err());
    }

    #[test]
    fn provider_history_keeps_only_complete_recent_pairs_within_budget() {
        use voxgolem_core::assistant::{Content, ConversationTurn, Role};

        let text = "x".repeat(PROVIDER_HISTORY_MAX_BYTES / 3);
        let history = vec![
            ConversationTurn {
                role: Role::User,
                content: Content::Text(format!("old-user-{text}")),
            },
            ConversationTurn {
                role: Role::Assistant,
                content: Content::Text(format!("old-assistant-{text}")),
            },
            ConversationTurn {
                role: Role::User,
                content: Content::Text(format!("new-user-{text}")),
            },
            ConversationTurn {
                role: Role::Assistant,
                content: Content::Text(format!("new-assistant-{text}")),
            },
        ];

        let bounded = bounded_provider_history(&history);

        assert_eq!(bounded, &history[2..]);
    }

    #[test]
    fn provider_history_caps_short_sessions_at_complete_pair_boundary() {
        use voxgolem_core::assistant::{Content, ConversationTurn, Role};

        let history = (0..65)
            .flat_map(|index| {
                [
                    ConversationTurn {
                        role: Role::User,
                        content: Content::Text(format!("user-{index}")),
                    },
                    ConversationTurn {
                        role: Role::Assistant,
                        content: Content::Text(format!("assistant-{index}")),
                    },
                ]
            })
            .collect::<Vec<_>>();

        let bounded = bounded_provider_history(&history);
        let review_history = agent_history(&history);

        assert_eq!(bounded.len(), 128);
        assert_eq!(bounded, &history[2..]);
        assert_eq!(review_history.len(), 128);
        voxgolem_core::agent_pipeline::validate_review_input(
            &voxgolem_core::agent_pipeline::ReviewInput {
                original_request: String::from("next question"),
                canonical_history: review_history,
                instant: voxgolem_core::agent_pipeline::StageStatus::Success(Content::Text(
                    String::from("answer"),
                )),
                deep: voxgolem_core::agent_pipeline::StageStatus::Failure(String::from("disabled")),
                materiality_policy: String::from("material factual defects only"),
                sources: Vec::new(),
            },
        )
        .unwrap();
    }

    #[test]
    fn review_history_drops_oldest_complete_pairs_to_fit_aggregate_prompt() {
        use voxgolem_core::agent_pipeline::{ReviewInput, StageStatus};
        use voxgolem_core::assistant::{Content, ConversationTurn, Role};

        let text = "x".repeat(65_519);
        let history = (0..4)
            .flat_map(|index| {
                [
                    ConversationTurn {
                        role: Role::User,
                        content: Content::Text(format!("{index}{text}")),
                    },
                    ConversationTurn {
                        role: Role::Assistant,
                        content: Content::Text(format!("{index}{text}")),
                    },
                ]
            })
            .collect::<Vec<_>>();
        let review_history = agent_history(&history);
        assert_eq!(review_history.len(), 8);
        let input = ReviewInput {
            original_request: String::from("next question"),
            canonical_history: review_history,
            instant: StageStatus::Success(Content::Text(String::from("instant answer"))),
            deep: StageStatus::Failure(String::from("disabled")),
            materiality_policy: String::from("material factual defects only"),
            sources: Vec::new(),
        };

        let fitted = fit_review_history_to_prompt_budget(input).unwrap();

        assert_eq!(fitted.canonical_history.len(), 6);
        assert_eq!(
            fitted.canonical_history[0].content,
            Content::Text(format!("1{text}"))
        );
        voxgolem_core::agent_pipeline::validate_review_input(&fitted).unwrap();
    }

    #[test]
    fn review_history_fitting_surfaces_non_history_bounds_with_empty_history() {
        let input = voxgolem_core::agent_pipeline::ReviewInput {
            original_request: "x".repeat(voxgolem_core::agent_pipeline::MAX_TEXT_BYTES + 1),
            canonical_history: Vec::new(),
            instant: voxgolem_core::agent_pipeline::StageStatus::Success(
                voxgolem_core::assistant::Content::Text(String::from("answer")),
            ),
            deep: voxgolem_core::agent_pipeline::StageStatus::Failure(String::from("disabled")),
            materiality_policy: String::from("material factual defects only"),
            sources: Vec::new(),
        };

        let error = fit_review_history_to_prompt_budget(input).unwrap_err();

        assert_eq!(error, "review input exceeds bounds");
    }

    #[test]
    fn prefetch_promotes_only_an_exact_prompt_history_and_model_match() {
        let key = PrefetchKey {
            prompt: String::from("explain ownership"),
            history: Vec::new(),
            model: voxgolem_core::assistant::InstantModel::LocalFast,
        };
        let cache = Mutex::new(Some(PrefetchEntry {
            generation: 1,
            settings_generation: 0,
            key: key.clone(),
            answer: voxgolem_core::assistant::Content::Text(String::from(
                "Ownership controls resource lifetime.",
            )),
        }));
        let generation = std::sync::atomic::AtomicU64::new(1);
        let settings_generation = std::sync::atomic::AtomicU64::new(0);
        let mut mismatch = key.clone();
        mismatch.prompt.push('?');
        assert_eq!(
            take_and_invalidate_prefetch(&cache, &generation, &settings_generation, &mismatch,)
                .unwrap(),
            None
        );
        assert!(cache.lock().unwrap().is_none());

        *cache.lock().unwrap() = Some(PrefetchEntry {
            generation: 2,
            settings_generation: 0,
            key: key.clone(),
            answer: voxgolem_core::assistant::Content::Text(String::from(
                "Ownership controls resource lifetime.",
            )),
        });
        assert_eq!(
            take_and_invalidate_prefetch(&cache, &generation, &settings_generation, &key).unwrap(),
            Some(voxgolem_core::assistant::Content::Text(String::from(
                "Ownership controls resource lifetime.",
            )))
        );
        assert!(cache.lock().unwrap().is_none());
    }

    #[test]
    fn final_voice_completion_clear_preserves_prefetch_for_submission() {
        let app_state = build_startup_error_app_state(
            default_voice_pipeline_config(),
            String::from("startup failed"),
        );
        let key = PrefetchKey {
            prompt: String::from("explain ownership"),
            history: Vec::new(),
            model: voxgolem_core::assistant::InstantModel::LocalFast,
        };
        *app_state.prefetch_cache.lock().unwrap() = Some(PrefetchEntry {
            generation: 1,
            settings_generation: 0,
            key: key.clone(),
            answer: voxgolem_core::assistant::Content::Text(String::from(
                "Ownership controls resource lifetime.",
            )),
        });

        clear_completion_request_state_locked(&app_state, false).unwrap();

        assert_eq!(
            app_state
                .prefetch_cache
                .lock()
                .unwrap()
                .as_ref()
                .map(|entry| &entry.key),
            Some(&key)
        );
    }

    #[test]
    fn blank_prefetch_entry_is_rejected_as_a_cache_miss() {
        let key = PrefetchKey {
            prompt: String::from("question"),
            history: Vec::new(),
            model: voxgolem_core::assistant::InstantModel::LocalFast,
        };
        let cache = Mutex::new(Some(PrefetchEntry {
            generation: 1,
            settings_generation: 0,
            key: key.clone(),
            answer: voxgolem_core::assistant::Content::Text(String::from(" \n\t ")),
        }));
        let generation = AtomicU64::new(1);
        let settings_generation = AtomicU64::new(0);
        assert_eq!(
            take_and_invalidate_prefetch(&cache, &generation, &settings_generation, &key).unwrap(),
            None
        );
        assert!(cache.lock().unwrap().is_none());
    }

    #[test]
    fn prefetch_completion_rejects_retired_settings_generation() {
        let app_state = build_startup_error_app_state(
            default_voice_pipeline_config(),
            String::from("startup failed"),
        );
        let key = PrefetchKey {
            prompt: String::from("question"),
            history: Vec::new(),
            model: voxgolem_core::assistant::InstantModel::LocalFast,
        };
        let cancelled = AtomicBool::new(false);
        app_state.prefetch_generation.store(7, Ordering::SeqCst);

        assert!(super::prefetch_result_is_current(
            &app_state, &key, 7, 0, &cancelled,
        ));
        app_state
            .assistant_settings_generation
            .fetch_add(1, Ordering::SeqCst);
        assert!(!super::prefetch_result_is_current(
            &app_state, &key, 7, 0, &cancelled,
        ));
    }

    #[test]
    fn exit_invalidates_pending_runtime_startups_before_shutdown() {
        let app_state = build_startup_error_app_state(
            default_voice_pipeline_config(),
            String::from("startup failed"),
        );

        invalidate_runtime_startups_for_exit(&app_state);
        shutdown_llama_cpp_runtime_for_exit(&app_state);

        assert_eq!(
            app_state
                .response_profile_switch_generation
                .load(std::sync::atomic::Ordering::SeqCst),
            1
        );
        assert_eq!(
            app_state
                .tts_startup_generation
                .load(std::sync::atomic::Ordering::SeqCst),
            1
        );
        assert!(app_state
            .llama_cpp_runtime
            .lock()
            .expect("runtime lock should not be poisoned")
            .is_none());
    }

    #[test]
    fn prompt_update_serializes_without_request_correlation() {
        let payload = serde_json::to_value(PromptExecutionEventPayload::Tool {
            tool: String::from("bash"),
            status: String::from("running"),
            detail: String::from("Checking status"),
        })
        .expect("prompt update should serialize");

        assert!(payload.get("request_id").is_none());
        assert_eq!(payload["kind"], "tool");
        assert_eq!(payload["tool"], "bash");
        assert_eq!(payload["status"], "running");
        assert_eq!(payload["detail"], "Checking status");
    }

    #[test]
    fn prompt_update_channel_delivers_the_typed_payload() {
        let delivered = Arc::new(Mutex::new(None));
        let delivered_for_channel = Arc::clone(&delivered);
        let channel = tauri::ipc::Channel::new(move |body| {
            let tauri::ipc::InvokeResponseBody::Json(json) = body else {
                panic!("prompt update should use JSON IPC");
            };
            delivered_for_channel.lock().unwrap().replace(json);
            Ok(())
        });

        channel
            .send(PromptExecutionEventPayload::Text {
                text: String::from("hello"),
            })
            .unwrap();

        let payload: serde_json::Value = serde_json::from_str(
            delivered
                .lock()
                .unwrap()
                .as_deref()
                .expect("channel should receive an update"),
        )
        .unwrap();
        assert_eq!(
            payload,
            serde_json::json!({ "kind": "text", "text": "hello" })
        );
    }

    #[test]
    fn stage_event_serialization_uses_typed_lifecycle_values() {
        let payload = serde_json::to_value(PromptExecutionEventPayload::Stage {
            stage: StagePayload::Review,
            status: StageStatusPayload::Corrected,
            detail: Some(String::from("material defect corrected")),
        })
        .expect("stage event should serialize");
        assert_eq!(payload["kind"], "stage");
        assert_eq!(payload["stage"], "review");
        assert_eq!(payload["status"], "corrected");
        assert_eq!(payload["detail"], "material defect corrected");
    }

    #[test]
    fn stage_lifecycle_does_not_promote_failed_deep_to_completed() {
        let failed = PromptExecutionEventPayload::Stage {
            stage: StagePayload::Deep,
            status: StageStatusPayload::Failed,
            detail: Some(String::from("provider failed")),
        };
        let encoded = serde_json::to_value(failed).unwrap();
        assert_eq!(encoded["status"], "failed");
        assert_ne!(encoded["status"], "completed");
    }

    #[test]
    fn cancellation_signal_wakes_two_registered_waiters() {
        use std::time::Duration;
        let (signal, _) = tokio::sync::watch::channel(false);
        tauri::async_runtime::block_on(async {
            let mut first = signal.subscribe();
            let mut second = signal.subscribe();
            signal.send_replace(true);
            tokio::time::timeout(Duration::from_secs(1), first.changed())
                .await
                .expect("first waiter should wake")
                .expect("cancellation sender should remain alive");
            tokio::time::timeout(Duration::from_secs(1), second.changed())
                .await
                .expect("second waiter should wake")
                .expect("cancellation sender should remain alive");
        });
    }

    #[test]
    fn deep_task_join_disarms_cancellation_after_completion() {
        let (signal, _) = tokio::sync::watch::channel(false);
        let cancellation = Arc::new(signal);
        let handle = tauri::async_runtime::spawn(async {
            DeepStageResult {
                report: None,
                elapsed_ms: 0,
                model: voxgolem_core::assistant::AgentModel::CustomSolHigh,
            }
        });
        let abort = handle.inner().abort_handle();
        let task = DeepTask {
            handle: Arc::new(Mutex::new(Some(handle))),
            abort: Arc::new(Mutex::new(Some(abort))),
            cancellation: Arc::clone(&cancellation),
        };
        tauri::async_runtime::block_on(async {
            task.join().await.expect("deep task should complete");
        });
        assert!(!*cancellation.subscribe().borrow());
    }

    #[test]
    fn dropping_pending_deep_task_cancels_and_aborts_it() {
        let (signal, _) = tokio::sync::watch::channel(false);
        let cancellation = Arc::new(signal);
        let handle =
            tauri::async_runtime::spawn(async { std::future::pending::<DeepStageResult>().await });
        let abort = handle.inner().abort_handle();
        let task = DeepTask {
            handle: Arc::new(Mutex::new(Some(handle))),
            abort: Arc::new(Mutex::new(Some(abort))),
            cancellation: Arc::clone(&cancellation),
        };
        drop(task);
        assert!(*cancellation.subscribe().borrow());
    }

    #[test]
    fn abandoning_pending_deep_join_keeps_cancellation_set() {
        let (signal, _) = tokio::sync::watch::channel(false);
        let cancellation = Arc::new(signal);
        let handle =
            tauri::async_runtime::spawn(async { std::future::pending::<DeepStageResult>().await });
        let abort = handle.inner().abort_handle();
        let task = DeepTask {
            handle: Arc::new(Mutex::new(Some(handle))),
            abort: Arc::new(Mutex::new(Some(abort))),
            cancellation: Arc::clone(&cancellation),
        };
        tauri::async_runtime::block_on(async {
            let mut join = Box::pin(task.join());
            let waker = futures_util::task::noop_waker();
            let mut context = std::task::Context::from_waker(&waker);
            assert!(matches!(
                std::future::Future::poll(join.as_mut(), &mut context),
                std::task::Poll::Pending
            ));
            drop(join);
        });
        assert!(*cancellation.subscribe().borrow());
    }

    #[test]
    fn active_prompt_guard_abandonment_publishes_durable_cancellation() {
        let active = Arc::new(Mutex::new(None));
        let lifecycle = Arc::new(Mutex::new(()));
        let session = Arc::new(Mutex::new(
            voxgolem_core::conversation::ConversationSession::new(
                voxgolem_core::assistant::AssistantPolicy::default(),
            ),
        ));
        let execution_id = session
            .lock()
            .unwrap()
            .admit("request", "prompt")
            .unwrap()
            .execution_id;
        let generations = AtomicU64::new(0);
        let (generation, cancelled, cancellation_signal, settled) =
            register_active_prompt(&active, &generations, "request", execution_id, None, 0)
                .expect("registration should succeed");
        let (terminal_kind, abort_client) = active
            .lock()
            .unwrap()
            .as_ref()
            .map(|active| {
                (
                    Arc::clone(&active.terminal_kind),
                    Arc::clone(&active.abort_client),
                )
            })
            .unwrap();
        let guard = ActivePromptGuard {
            active_prompt: Arc::clone(&active),
            conversation_session: Arc::clone(&session),
            request_id: String::from("request"),
            generation,
            execution_id,
            cancelled: Arc::clone(&cancelled),
            armed: Arc::new(AtomicBool::new(true)),
            abort_client,
            prompt_cleanup: Arc::new(super::PromptCleanupTracker::default()),
            settled,
            lifecycle_gate: lifecycle,
            terminal_kind,
            voice_pipeline_state: Arc::new(Mutex::new(
                voxgolem_core::voice_pipeline::VoicePipelineState::new(
                    default_voice_pipeline_config(),
                )
                .unwrap(),
            )),
            voice_pipeline_config: default_voice_pipeline_config(),
        };
        let _receiver = cancellation_signal.subscribe();
        drop(guard);
        assert!(cancelled.load(Ordering::Acquire));
        assert!(*cancellation_signal.subscribe().borrow());
        assert!(active.lock().unwrap().is_none());
        assert!(!session.lock().unwrap().has_active_execution());
    }

    #[test]
    fn active_prompt_guard_finish_disarms_terminal_cleanup() {
        let active = Arc::new(Mutex::new(None));
        let session = Arc::new(Mutex::new(
            voxgolem_core::conversation::ConversationSession::new(
                voxgolem_core::assistant::AssistantPolicy::default(),
            ),
        ));
        let execution_id = voxgolem_core::conversation::ExecutionId {
            session_id: voxgolem_core::conversation::SessionId(1),
            generation: voxgolem_core::conversation::GenerationId(1),
        };
        let generations = AtomicU64::new(0);
        let (generation, cancelled, cancellation_signal, settled) = register_active_prompt(
            active.as_ref(),
            &generations,
            "request",
            execution_id,
            None,
            0,
        )
        .expect("registration should succeed");
        let guard = ActivePromptGuard {
            active_prompt: Arc::clone(&active),
            conversation_session: session,
            request_id: String::from("request"),
            generation,
            execution_id,
            cancelled: Arc::clone(&cancelled),
            armed: Arc::new(AtomicBool::new(true)),
            abort_client: active
                .lock()
                .unwrap()
                .as_ref()
                .map(|active| Arc::clone(&active.abort_client))
                .unwrap(),
            prompt_cleanup: Arc::new(super::PromptCleanupTracker::default()),
            settled,
            lifecycle_gate: Arc::new(Mutex::new(())),
            terminal_kind: Arc::new(Mutex::new(None)),
            voice_pipeline_state: Arc::new(Mutex::new(
                voxgolem_core::voice_pipeline::VoicePipelineState::new(
                    default_voice_pipeline_config(),
                )
                .unwrap(),
            )),
            voice_pipeline_config: default_voice_pipeline_config(),
        };
        guard.finish();
        drop(guard);
        assert!(!cancelled.load(Ordering::Acquire));
        assert!(!*cancellation_signal.subscribe().borrow());
        assert!(active.lock().unwrap().is_none());
    }

    #[test]
    fn terminal_kind_is_first_writer_wins() {
        let terminal = Mutex::new(None);
        assert_eq!(
            record_terminal_kind(&terminal, TerminalKind::Cancelled),
            TerminalKind::Cancelled
        );
        assert_eq!(
            record_terminal_kind(&terminal, TerminalKind::Failed),
            TerminalKind::Cancelled
        );
        assert_eq!(*terminal.lock().unwrap(), Some(TerminalKind::Cancelled));
    }

    #[test]
    fn abort_ownership_can_be_claimed_only_once() {
        let slot = Mutex::new(Some(7_u8));
        assert_eq!(super::take_once(&slot), Some(7));
        assert_eq!(super::take_once(&slot), None);
    }

    #[test]
    fn reset_cancels_only_the_execution_it_invalidates() {
        use voxgolem_core::assistant::{
            AssistantPolicy, Content, InstantOutcome, Stage, StageResult,
        };
        use voxgolem_core::conversation::{
            ConversationSession, SettlementIntent, SettlementResult, UpdateResult,
        };

        let lifecycle = Mutex::new(());
        let reset_in_progress = AtomicBool::new(false);
        let prompt_cleanup = Arc::new(super::PromptCleanupTracker::default());
        let active = Mutex::new(None);
        let generations = AtomicU64::new(0);
        let session = Mutex::new(ConversationSession::new(AssistantPolicy::default()));
        let execution = session
            .lock()
            .unwrap()
            .admit("request", "prompt")
            .unwrap()
            .execution_id;
        let registration =
            register_active_prompt(&active, &generations, "request", execution, None, 0).unwrap();

        let invalidated = reset_conversation_execution(
            &Mutex::new(()),
            &AtomicU64::new(0),
            &lifecycle,
            (&reset_in_progress, &prompt_cleanup),
            &active,
            &session,
            || Ok(()),
        )
        .unwrap()
        .cleanup
        .expect("active execution should be invalidated")
        .active;
        assert!(invalidated.cancelled.load(Ordering::Acquire));
        assert_eq!(
            *invalidated.terminal_kind.lock().unwrap(),
            Some(TerminalKind::Cancelled)
        );
        assert_eq!(
            session
                .lock()
                .unwrap()
                .settle(execution, SettlementIntent::Failed),
            SettlementResult::IgnoredStale
        );
        clear_active_prompt(&active, "request", registration.0).unwrap();

        let next = session
            .lock()
            .unwrap()
            .admit("next", "prompt")
            .unwrap()
            .execution_id;
        assert!(register_active_prompt(&active, &generations, "next", next, None, 0).is_ok());

        let mut session = session.lock().unwrap();
        assert!(matches!(
            session.update(
                next,
                Stage::Instant,
                StageResult::Instant(InstantOutcome::Complete(Content::Text("answer".into())))
            ),
            UpdateResult::Resolved { .. }
        ));
    }

    #[test]
    fn reset_does_not_overwrite_an_already_committed_terminal() {
        use voxgolem_core::assistant::{
            AssistantPolicy, Content, InstantOutcome, Stage, StageResult,
        };
        use voxgolem_core::conversation::{
            AppliedOutcome, ConversationSession, SettlementIntent, SettlementResult,
        };

        let lifecycle = Mutex::new(());
        let reset_in_progress = AtomicBool::new(false);
        let prompt_cleanup = Arc::new(super::PromptCleanupTracker::default());
        let active = Mutex::new(None);
        let generations = AtomicU64::new(0);
        let session = Mutex::new(ConversationSession::new(AssistantPolicy::default()));
        let execution = session
            .lock()
            .unwrap()
            .admit("request", "prompt")
            .unwrap()
            .execution_id;
        register_active_prompt(&active, &generations, "request", execution, None, 0).unwrap();
        {
            let mut session = session.lock().unwrap();
            session.update(
                execution,
                Stage::Instant,
                StageResult::Instant(InstantOutcome::Complete(Content::Text("answer".into()))),
            );
            assert!(matches!(
                session.settle(execution, SettlementIntent::Success),
                SettlementResult::Applied(AppliedOutcome::Completed(_))
            ));
        }
        let terminal = active
            .lock()
            .unwrap()
            .as_ref()
            .map(|active| Arc::clone(&active.terminal_kind))
            .unwrap();
        record_terminal_kind(&terminal, TerminalKind::Completed);

        let physical = reset_conversation_execution(
            &Mutex::new(()),
            &AtomicU64::new(0),
            &lifecycle,
            (&reset_in_progress, &prompt_cleanup),
            &active,
            &session,
            || Ok(()),
        )
        .unwrap()
        .cleanup
        .expect("reset should claim the pre-reset physical prompt")
        .active;
        assert_eq!(*terminal.lock().unwrap(), Some(TerminalKind::Completed));
        assert!(!physical.cancelled.load(Ordering::Acquire));
    }

    #[test]
    fn reset_claims_an_already_cancelled_physical_prompt() {
        use voxgolem_core::assistant::AssistantPolicy;
        use voxgolem_core::conversation::{
            AppliedOutcome, ConversationSession, SettlementIntent, SettlementResult,
        };

        let lifecycle = Mutex::new(());
        let reset_in_progress = AtomicBool::new(false);
        let prompt_cleanup = Arc::new(super::PromptCleanupTracker::default());
        let active = Mutex::new(None);
        let generations = AtomicU64::new(0);
        let session = Mutex::new(ConversationSession::new(AssistantPolicy::default()));
        let execution = session
            .lock()
            .unwrap()
            .admit("request", "prompt")
            .unwrap()
            .execution_id;
        register_active_prompt(&active, &generations, "request", execution, None, 0).unwrap();
        let terminal = active
            .lock()
            .unwrap()
            .as_ref()
            .map(|active| Arc::clone(&active.terminal_kind))
            .unwrap();
        assert!(matches!(
            session
                .lock()
                .unwrap()
                .settle(execution, SettlementIntent::Cancelled),
            SettlementResult::Applied(AppliedOutcome::Cancelled)
        ));
        record_terminal_kind(&terminal, TerminalKind::Cancelled);

        let physical = reset_conversation_execution(
            &Mutex::new(()),
            &AtomicU64::new(0),
            &lifecycle,
            (&reset_in_progress, &prompt_cleanup),
            &active,
            &session,
            || Ok(()),
        )
        .unwrap()
        .cleanup
        .expect("reset should claim the pre-reset physical prompt")
        .active;
        assert_eq!(
            *physical.terminal_kind.lock().unwrap(),
            Some(TerminalKind::Cancelled)
        );
    }

    #[test]
    fn active_execution_rejects_profile_transition_without_changing_startup_state() {
        let lifecycle = Mutex::new(());
        let session = Mutex::new(voxgolem_core::conversation::ConversationSession::new(
            voxgolem_core::assistant::AssistantPolicy::default(),
        ));
        let voice_pipeline_config = default_voice_pipeline_config();
        let voice = Mutex::new(
            voxgolem_core::voice_pipeline::apply_voice_pipeline_event(
                &voxgolem_core::voice_pipeline::VoicePipelineState::new(voice_pipeline_config)
                    .unwrap(),
                voice_pipeline_config,
                voxgolem_core::voice_pipeline::VoicePipelineEvent::StartupValidated,
            )
            .unwrap()
            .0,
        );
        session.lock().unwrap().admit("request", "prompt").unwrap();
        let startup = Mutex::new(StartupStatePayload::Ready {
            cue_asset_paths: CueAssetPathsPayload {
                start_listening: String::from("start"),
                stop_listening: String::from("stop"),
            },
            runtime_phase: RuntimePhasePayload::Sleeping,
            voice_input_available: false,
            voice_input_error: None,
            silence_timeout_ms: DEFAULT_SILENCE_TIMEOUT_MS,
            selected_response_profile: ResponseProfilePayload::Fast,
            supported_response_profiles: vec![ResponseProfilePayload::Fast],
            prompt_cancellation_available: false,
            tts_enabled: false,
            tts_output_gain_db: 0.0,
            capabilities: Vec::new(),
        });
        let warming = StartupStatePayload::WarmingModel {
            cue_asset_paths: CueAssetPathsPayload {
                start_listening: String::from("start"),
                stop_listening: String::from("stop"),
            },
            runtime_phase: RuntimePhasePayload::Initializing,
            voice_input_available: false,
            voice_input_error: None,
            silence_timeout_ms: DEFAULT_SILENCE_TIMEOUT_MS,
            message: String::from("warming"),
            selected_response_profile: ResponseProfilePayload::Quality,
            supported_response_profiles: vec![ResponseProfilePayload::Fast],
            prompt_cancellation_available: false,
            tts_enabled: false,
            tts_output_gain_db: 0.0,
            capabilities: Vec::new(),
        };

        assert_eq!(
            super::begin_response_profile_switch_transition(
                &lifecycle,
                &AtomicBool::new(false),
                &session,
                &voice,
                &startup,
                warming,
            ),
            Err(String::from(
                "response profile cannot change while a prompt is active"
            ))
        );
        assert!(matches!(
            *startup.lock().unwrap(),
            StartupStatePayload::Ready {
                selected_response_profile: ResponseProfilePayload::Fast,
                ..
            }
        ));
    }

    #[test]
    fn profile_transition_rechecks_voice_runtime_after_waiting_for_lifecycle() {
        let lifecycle = Mutex::new(());
        let session = Mutex::new(voxgolem_core::conversation::ConversationSession::new(
            voxgolem_core::assistant::AssistantPolicy::default(),
        ));
        let voice_pipeline_config = default_voice_pipeline_config();
        let voice = Mutex::new(
            voxgolem_core::voice_pipeline::apply_voice_pipeline_event(
                &voxgolem_core::voice_pipeline::VoicePipelineState::new(voice_pipeline_config)
                    .unwrap(),
                voice_pipeline_config,
                voxgolem_core::voice_pipeline::VoicePipelineEvent::StartupValidated,
            )
            .unwrap()
            .0,
        );
        let startup = Mutex::new(StartupStatePayload::Ready {
            cue_asset_paths: CueAssetPathsPayload {
                start_listening: String::from("start"),
                stop_listening: String::from("stop"),
            },
            runtime_phase: RuntimePhasePayload::Sleeping,
            voice_input_available: true,
            voice_input_error: None,
            silence_timeout_ms: DEFAULT_SILENCE_TIMEOUT_MS,
            selected_response_profile: ResponseProfilePayload::Fast,
            supported_response_profiles: vec![ResponseProfilePayload::Fast],
            prompt_cancellation_available: false,
            tts_enabled: false,
            tts_output_gain_db: 0.0,
            capabilities: Vec::new(),
        });
        let warming = StartupStatePayload::WarmingModel {
            cue_asset_paths: CueAssetPathsPayload {
                start_listening: String::from("start"),
                stop_listening: String::from("stop"),
            },
            runtime_phase: RuntimePhasePayload::Initializing,
            voice_input_available: true,
            voice_input_error: None,
            silence_timeout_ms: DEFAULT_SILENCE_TIMEOUT_MS,
            message: String::from("warming"),
            selected_response_profile: ResponseProfilePayload::Quality,
            supported_response_profiles: vec![ResponseProfilePayload::Fast],
            prompt_cancellation_available: false,
            tts_enabled: false,
            tts_output_gain_db: 0.0,
            capabilities: Vec::new(),
        };
        let held_lifecycle = lifecycle.lock().unwrap();
        let (started_sender, started_receiver) = std::sync::mpsc::channel();

        std::thread::scope(|scope| {
            let waiter = scope.spawn(|| {
                started_sender.send(()).unwrap();
                super::begin_response_profile_switch_transition(
                    &lifecycle,
                    &AtomicBool::new(false),
                    &session,
                    &voice,
                    &startup,
                    warming,
                )
            });
            started_receiver.recv().unwrap();
            super::apply_voice_pipeline_transition(
                &voice,
                voice_pipeline_config,
                voxgolem_core::voice_pipeline::VoicePipelineEvent::WakeWordDetected { now_ms: 1 },
            )
            .unwrap();
            drop(held_lifecycle);

            assert_eq!(
                waiter.join().unwrap(),
                Err(String::from(
                    "response profile switch is only allowed while runtime is sleeping; current phase is listening",
                ))
            );
        });
        assert!(matches!(
            *startup.lock().unwrap(),
            StartupStatePayload::Ready { .. }
        ));
    }

    #[test]
    fn assistant_settings_update_rejects_warming_profile_transition() {
        let operation_lock = Mutex::new(());
        let startup = Arc::new(Mutex::new(StartupStatePayload::WarmingModel {
            cue_asset_paths: CueAssetPathsPayload {
                start_listening: String::from("start"),
                stop_listening: String::from("stop"),
            },
            runtime_phase: RuntimePhasePayload::Initializing,
            voice_input_available: false,
            voice_input_error: None,
            silence_timeout_ms: DEFAULT_SILENCE_TIMEOUT_MS,
            message: String::from("warming"),
            selected_response_profile: ResponseProfilePayload::Quality,
            supported_response_profiles: vec![ResponseProfilePayload::Fast],
            prompt_cancellation_available: false,
            tts_enabled: false,
            tts_output_gain_db: 0.0,
            capabilities: Vec::new(),
        }));

        assert!(super::lock_ready_response_backend_operation(&operation_lock, &startup).is_err());
    }

    #[test]
    fn serialized_response_operation_rechecks_startup_after_waiting() {
        let operation_lock = Arc::new(Mutex::new(()));
        let startup = Arc::new(Mutex::new(StartupStatePayload::Ready {
            cue_asset_paths: CueAssetPathsPayload {
                start_listening: String::from("start"),
                stop_listening: String::from("stop"),
            },
            runtime_phase: RuntimePhasePayload::Sleeping,
            voice_input_available: false,
            voice_input_error: None,
            silence_timeout_ms: DEFAULT_SILENCE_TIMEOUT_MS,
            selected_response_profile: ResponseProfilePayload::Fast,
            supported_response_profiles: vec![ResponseProfilePayload::Fast],
            prompt_cancellation_available: false,
            tts_enabled: false,
            tts_output_gain_db: 0.0,
            capabilities: Vec::new(),
        }));
        let first = operation_lock.lock().unwrap();
        let operation_for_waiter = Arc::clone(&operation_lock);
        let startup_for_waiter = Arc::clone(&startup);
        let waiter = std::thread::spawn(move || {
            super::lock_ready_response_backend_operation(&operation_for_waiter, &startup_for_waiter)
                .is_err()
        });
        *startup.lock().unwrap() = StartupStatePayload::WarmingModel {
            cue_asset_paths: CueAssetPathsPayload {
                start_listening: String::from("start"),
                stop_listening: String::from("stop"),
            },
            runtime_phase: RuntimePhasePayload::Initializing,
            voice_input_available: false,
            voice_input_error: None,
            silence_timeout_ms: DEFAULT_SILENCE_TIMEOUT_MS,
            message: String::from("warming"),
            selected_response_profile: ResponseProfilePayload::Quality,
            supported_response_profiles: vec![ResponseProfilePayload::Fast],
            prompt_cancellation_available: false,
            tts_enabled: false,
            tts_output_gain_db: 0.0,
            capabilities: Vec::new(),
        };
        drop(first);

        assert!(waiter.join().unwrap());
    }

    #[test]
    fn cancellation_winning_before_commit_returns_cancelled() {
        let app_state = super::build_nonfatal_config_error_app_state(
            default_voice_pipeline_config(),
            CueAssetPathsPayload {
                start_listening: String::from("start"),
                stop_listening: String::from("stop"),
            },
            String::from("test"),
        );
        let execution = app_state
            .conversation_session
            .lock()
            .unwrap()
            .admit("request", "prompt")
            .unwrap()
            .execution_id;
        let (physical_generation, ..) = register_active_prompt(
            &app_state.active_prompt,
            &app_state.active_prompt_generation,
            "request",
            execution,
            None,
            0,
        )
        .unwrap();
        let control = app_state
            .active_prompt
            .lock()
            .unwrap()
            .as_ref()
            .cloned()
            .unwrap();
        let deep_handle =
            tauri::async_runtime::spawn(async { std::future::pending::<DeepStageResult>().await });
        control
            .deep_abort
            .lock()
            .unwrap()
            .replace(deep_handle.inner().abort_handle());
        control.deep_handle.lock().unwrap().replace(deep_handle);
        super::apply_voice_pipeline_transition(
            &app_state.voice_pipeline_state,
            app_state.voice_pipeline_config,
            voxgolem_core::voice_pipeline::VoicePipelineEvent::SubmitPrompt,
        )
        .unwrap();
        assert!(matches!(
            super::cancel_assistant_request(&app_state, execution),
            voxgolem_core::conversation::SettlementResult::Applied(
                voxgolem_core::conversation::AppliedOutcome::Cancelled
            )
        ));
        assert!(app_state.active_prompt.lock().unwrap().is_none());
        assert!(control.deep_abort.lock().unwrap().is_none());
        assert!(control.deep_handle.lock().unwrap().is_none());
        assert_eq!(
            super::current_runtime_phase(&app_state.voice_pipeline_state).unwrap(),
            RuntimePhasePayload::Sleeping
        );
        assert_eq!(
            super::cancel_assistant_request(&app_state, execution),
            voxgolem_core::conversation::SettlementResult::IgnoredStale
        );

        assert_eq!(
            super::commit_assistant_request_if_active(
                &app_state,
                "request",
                physical_generation,
                execution,
                &control,
            ),
            Ok(CommitAssistantResult::Cancelled)
        );
        let next = super::begin_registered_assistant_request(
            &app_state,
            "next-request",
            "next prompt",
            super::CompletionSource::Typed,
        )
        .expect("winning cancellation should immediately release logical admission");
        assert_eq!(
            super::current_runtime_phase(&app_state.voice_pipeline_state).unwrap(),
            RuntimePhasePayload::Executing
        );
        assert_eq!(
            super::commit_assistant_request_if_active(
                &app_state,
                "request",
                physical_generation,
                execution,
                &control,
            ),
            Ok(CommitAssistantResult::Cancelled)
        );
        assert_eq!(
            super::current_runtime_phase(&app_state.voice_pipeline_state).unwrap(),
            RuntimePhasePayload::Executing
        );
        drop(next);
    }

    #[test]
    fn cancel_prompt_reports_only_a_matching_active_execution() {
        let app_state = super::build_nonfatal_config_error_app_state(
            default_voice_pipeline_config(),
            CueAssetPathsPayload {
                start_listening: String::from("start"),
                stop_listening: String::from("stop"),
            },
            String::from("test"),
        );
        assert_eq!(
            super::cancel_prompt_if_active(&app_state, "missing"),
            Ok(false)
        );

        let request = super::begin_registered_assistant_request(
            &app_state,
            "request",
            "prompt",
            super::CompletionSource::Typed,
        )
        .unwrap();
        assert_eq!(
            super::cancel_prompt_if_active(&app_state, "other"),
            Ok(false)
        );
        assert_eq!(
            super::cancel_prompt_if_active(&app_state, "request"),
            Ok(true)
        );
        assert_eq!(
            super::cancel_prompt_if_active(&app_state, "request"),
            Ok(false)
        );
        drop(request);
    }

    #[test]
    fn successful_settlement_repairs_runtime_and_releases_admission_together() {
        let app_state = super::build_nonfatal_config_error_app_state(
            default_voice_pipeline_config(),
            CueAssetPathsPayload {
                start_listening: String::from("start"),
                stop_listening: String::from("stop"),
            },
            String::from("test"),
        );
        let request = super::begin_registered_assistant_request(
            &app_state,
            "request",
            "prompt",
            super::CompletionSource::Typed,
        )
        .unwrap();
        super::finish_assistant_request(
            &app_state,
            "request",
            request.physical_generation,
            request.request.generation,
            voxgolem_core::assistant::InstantOutcome::Complete(
                voxgolem_core::assistant::Content::Text(String::from("answer")),
            ),
        )
        .unwrap();

        assert_eq!(
            super::commit_assistant_request_if_active(
                &app_state,
                "request",
                request.physical_generation,
                request.request.generation,
                &request.control,
            ),
            Ok(CommitAssistantResult::Committed)
        );
        assert_eq!(
            super::current_runtime_phase(&app_state.voice_pipeline_state).unwrap(),
            RuntimePhasePayload::Sleeping
        );
        assert!(app_state.active_prompt.lock().unwrap().is_none());
        drop(request);
    }

    #[test]
    fn failure_repairs_runtime_before_the_next_request_executes() {
        let app_state = super::build_nonfatal_config_error_app_state(
            default_voice_pipeline_config(),
            CueAssetPathsPayload {
                start_listening: String::from("start"),
                stop_listening: String::from("stop"),
            },
            String::from("test"),
        );
        let request = super::begin_registered_assistant_request(
            &app_state,
            "request",
            "prompt",
            super::CompletionSource::Typed,
        )
        .unwrap();

        assert!(matches!(
            super::fail_assistant_request(
                &app_state,
                request.request.generation,
                "provider failed",
            ),
            voxgolem_core::conversation::SettlementResult::Applied(
                voxgolem_core::conversation::AppliedOutcome::Failed
            )
        ));
        assert_eq!(
            super::current_runtime_phase(&app_state.voice_pipeline_state).unwrap(),
            RuntimePhasePayload::Error
        );

        let next = super::begin_registered_assistant_request(
            &app_state,
            "next-request",
            "next prompt",
            super::CompletionSource::Typed,
        )
        .expect("the next request should recover the terminal runtime state");
        assert_eq!(
            super::current_runtime_phase(&app_state.voice_pipeline_state).unwrap(),
            RuntimePhasePayload::Executing
        );
        drop(next);
        drop(request);
    }

    #[test]
    fn blocked_event_publication_does_not_block_logical_cancellation() {
        let app_state = super::build_nonfatal_config_error_app_state(
            default_voice_pipeline_config(),
            CueAssetPathsPayload {
                start_listening: String::from("start"),
                stop_listening: String::from("stop"),
            },
            String::from("test"),
        );
        let request = super::begin_registered_assistant_request(
            &app_state,
            "request",
            "prompt",
            super::CompletionSource::Typed,
        )
        .unwrap();
        let publication = app_state.prompt_publication_gate.lock().unwrap();
        let (sender, receiver) = std::sync::mpsc::channel();

        std::thread::scope(|scope| {
            scope.spawn(|| {
                let result =
                    super::cancel_assistant_request(&app_state, request.request.generation);
                sender.send(result).unwrap();
            });
            assert!(matches!(
                receiver
                    .recv_timeout(Duration::from_secs(1))
                    .expect("logical cancellation must not wait for event publication"),
                voxgolem_core::conversation::SettlementResult::Applied(
                    voxgolem_core::conversation::AppliedOutcome::Cancelled
                )
            ));
        });
        drop(publication);
        drop(request);
    }

    #[test]
    fn reset_operations_are_serialized() {
        let app_state =
            build_startup_error_app_state(default_voice_pipeline_config(), String::from("test"));

        tauri::async_runtime::block_on(async {
            let first = app_state.reset_operation_lock.lock().await;
            assert!(tokio::time::timeout(
                Duration::from_millis(10),
                app_state.reset_operation_lock.lock(),
            )
            .await
            .is_err());
            drop(first);
            let _second = tokio::time::timeout(
                Duration::from_secs(1),
                app_state.reset_operation_lock.lock(),
            )
            .await
            .expect("next reset should acquire the operation lock after release");
        });
    }

    #[test]
    fn reset_barrier_rejects_admission_until_cleanup_finishes() {
        let app_state = super::build_nonfatal_config_error_app_state(
            default_voice_pipeline_config(),
            CueAssetPathsPayload {
                start_listening: String::from("start"),
                stop_listening: String::from("stop"),
            },
            String::from("test"),
        );
        reset_conversation_execution(
            &app_state.prompt_publication_gate,
            &app_state.prompt_publication_epoch,
            &app_state.lifecycle_gate,
            (&app_state.reset_in_progress, &app_state.prompt_cleanup),
            &app_state.active_prompt,
            &app_state.conversation_session,
            || Ok(()),
        )
        .unwrap();
        let reset_guard = super::ResetAdmissionGuard {
            lifecycle_gate: Arc::clone(&app_state.lifecycle_gate),
            reset_in_progress: Arc::clone(&app_state.reset_in_progress),
        };

        assert_eq!(
            super::begin_registered_assistant_request(
                &app_state,
                "request",
                "prompt",
                super::CompletionSource::Typed,
            )
            .err()
            .as_deref(),
            Some("session reset is in progress")
        );
        drop(reset_guard);
        assert!(super::begin_registered_assistant_request(
            &app_state,
            "request",
            "prompt",
            super::CompletionSource::Typed,
        )
        .is_ok());
    }

    #[test]
    fn registered_admission_publishes_and_drops_core_and_shell_together() {
        let app_state = super::build_nonfatal_config_error_app_state(
            default_voice_pipeline_config(),
            CueAssetPathsPayload {
                start_listening: String::from("start"),
                stop_listening: String::from("stop"),
            },
            String::from("test"),
        );
        let registered = super::begin_registered_assistant_request(
            &app_state,
            "request",
            "prompt",
            super::CompletionSource::Typed,
        )
        .expect("admission should register its physical lease");

        assert_eq!(
            app_state
                .conversation_session
                .lock()
                .unwrap()
                .active_execution(),
            Some(registered.request.generation)
        );
        assert_eq!(
            app_state
                .active_prompt
                .lock()
                .unwrap()
                .as_ref()
                .map(|active| active.assistant_generation),
            Some(registered.request.generation)
        );

        drop(registered);
        assert!(!app_state
            .conversation_session
            .lock()
            .unwrap()
            .has_active_execution());
        assert!(app_state.active_prompt.lock().unwrap().is_none());
    }

    #[test]
    fn pending_physical_cleanup_does_not_retain_logical_admission() {
        let app_state = super::build_nonfatal_config_error_app_state(
            default_voice_pipeline_config(),
            CueAssetPathsPayload {
                start_listening: String::from("start"),
                stop_listening: String::from("stop"),
            },
            String::from("test"),
        );
        let _cleanup_permit = app_state.prompt_cleanup.register();

        let registered = super::begin_registered_assistant_request(
            &app_state,
            "request",
            "prompt",
            super::CompletionSource::Typed,
        )
        .expect("detached physical cleanup must not block a new logical request");
        assert!(app_state
            .conversation_session
            .lock()
            .unwrap()
            .has_active_execution());
        assert!(app_state.active_prompt.lock().unwrap().is_some());
        drop(registered);
    }

    #[test]
    fn detached_cleanup_keeps_permit_until_inner_work_finishes() {
        let tracker = Arc::new(super::PromptCleanupTracker::default());
        let cleanup_permit = tracker.register();

        tauri::async_runtime::block_on(async {
            let (started, started_receiver) = tokio::sync::oneshot::channel();
            let (release, release_receiver) = tokio::sync::oneshot::channel();
            let inner = tauri::async_runtime::spawn(async move {
                let _cleanup_permit = cleanup_permit;
                let _ = started.send(());
                let _ = release_receiver.await;
            });
            let outer = tauri::async_runtime::spawn(async move {
                let _ = inner.await;
            });
            started_receiver.await.expect("inner cleanup should start");
            outer.abort();
            tokio::task::yield_now().await;
            assert!(tracker.has_pending());
            assert!(
                tokio::time::timeout(Duration::from_millis(10), tracker.wait())
                    .await
                    .is_err()
            );

            let _ = release.send(());
            tokio::time::timeout(Duration::from_secs(1), tracker.wait())
                .await
                .expect("detached cleanup should release its permit");
            assert!(!tracker.has_pending());
        });
    }

    #[test]
    fn cancelled_prompt_terminal_publication_is_claimed_once() {
        let active = Mutex::new(None);
        let generations = AtomicU64::new(0);
        let (_, cancelled, _, _) = register_active_prompt(
            &active,
            &generations,
            "request",
            voxgolem_core::conversation::ExecutionId {
                session_id: voxgolem_core::conversation::SessionId(1),
                generation: voxgolem_core::conversation::GenerationId(1),
            },
            None,
            0,
        )
        .expect("registration should succeed");
        let prompt = active.lock().unwrap();
        assert!(!claim_cancelled_prompt_publication(
            prompt.as_ref().unwrap()
        ));
        cancelled.store(true, Ordering::Release);
        assert!(claim_cancelled_prompt_publication(prompt.as_ref().unwrap()));
        assert!(!claim_cancelled_prompt_publication(
            prompt.as_ref().unwrap()
        ));
    }

    #[test]
    fn reset_epoch_suppresses_a_detached_terminal_publication() {
        let active = Mutex::new(None);
        let generations = AtomicU64::new(0);
        register_active_prompt(
            &active,
            &generations,
            "request",
            voxgolem_core::conversation::ExecutionId {
                session_id: voxgolem_core::conversation::SessionId(1),
                generation: voxgolem_core::conversation::GenerationId(1),
            },
            None,
            4,
        )
        .unwrap();
        let prompt = active.lock().unwrap();
        let prompt = prompt.as_ref().unwrap();
        record_terminal_kind(&prompt.terminal_kind, TerminalKind::Completed);

        assert!(!super::claim_terminal_prompt_publication(prompt, 5, 1));
        assert!(!prompt.terminal_published.load(Ordering::Acquire));
    }

    #[test]
    fn reset_waits_for_terminal_publication_before_rotating_the_session() {
        let app_state = super::build_nonfatal_config_error_app_state(
            default_voice_pipeline_config(),
            CueAssetPathsPayload {
                start_listening: String::from("start"),
                stop_listening: String::from("stop"),
            },
            String::from("test"),
        );
        let request = super::begin_registered_assistant_request(
            &app_state,
            "request",
            "prompt",
            super::CompletionSource::Typed,
        )
        .unwrap();
        let publication = app_state.prompt_publication_gate.lock().unwrap();
        let (started_sender, started_receiver) = std::sync::mpsc::channel();
        let (done_sender, done_receiver) = std::sync::mpsc::channel();

        std::thread::scope(|scope| {
            let app_state = &app_state;
            scope.spawn(move || {
                started_sender.send(()).unwrap();
                done_sender
                    .send(reset_conversation_execution(
                        &app_state.prompt_publication_gate,
                        &app_state.prompt_publication_epoch,
                        &app_state.lifecycle_gate,
                        (&app_state.reset_in_progress, &app_state.prompt_cleanup),
                        &app_state.active_prompt,
                        &app_state.conversation_session,
                        || Ok(()),
                    ))
                    .unwrap();
            });
            started_receiver.recv().unwrap();
            assert!(done_receiver
                .recv_timeout(Duration::from_millis(20))
                .is_err());
            assert!(app_state
                .conversation_session
                .lock()
                .unwrap()
                .has_active_execution());
            drop(publication);
            done_receiver
                .recv_timeout(Duration::from_secs(1))
                .expect("reset should continue after terminal publication")
                .expect("reset should succeed");
        });

        assert_eq!(
            app_state.prompt_publication_epoch.load(Ordering::Acquire),
            1
        );
        assert!(!app_state
            .conversation_session
            .lock()
            .unwrap()
            .has_active_execution());
        app_state.reset_in_progress.store(false, Ordering::Release);
        drop(request);
    }

    #[test]
    fn reused_request_id_rejects_the_retired_physical_generation() {
        let active = Mutex::new(None);
        let generations = AtomicU64::new(0);
        let execution = voxgolem_core::conversation::ExecutionId {
            session_id: voxgolem_core::conversation::SessionId(1),
            generation: voxgolem_core::conversation::GenerationId(1),
        };
        let (retired_generation, ..) =
            register_active_prompt(&active, &generations, "request", execution, None, 0).unwrap();
        let retired = clear_active_prompt(&active, "request", retired_generation)
            .unwrap()
            .expect("first registration should be removed");
        let (current_generation, ..) =
            register_active_prompt(&active, &generations, "request", execution, None, 0).unwrap();
        {
            let active = active.lock().unwrap();
            let active = active.as_ref().unwrap();
            assert!(!super::prompt_event_identity_matches(
                active,
                "request",
                retired_generation,
            ));
            assert!(super::prompt_event_identity_matches(
                active,
                "request",
                current_generation,
            ));
        }
        clear_active_prompt(&active, "request", current_generation)
            .unwrap()
            .expect("second registration should be removed");
        record_terminal_kind(&retired.terminal_kind, TerminalKind::Cancelled);

        assert!(!super::claim_terminal_prompt_publication(
            &retired,
            0,
            generations.load(Ordering::Acquire),
        ));
        assert!(!retired.terminal_published.load(Ordering::Acquire));
    }

    #[test]
    fn transient_cleanup_runs_delete_after_abort_timeout() {
        let deleted = Arc::new(AtomicBool::new(false));
        let deleted_for_test = Arc::clone(&deleted);
        tauri::async_runtime::block_on(async move {
            cleanup_sequential(
                Duration::from_millis(10),
                || async { std::future::pending::<()>().await },
                || async move {
                    deleted_for_test.store(true, Ordering::Release);
                },
            )
            .await;
        });
        assert!(deleted.load(Ordering::Acquire));
    }

    #[test]
    fn dropping_transient_creation_supervises_pending_task() {
        let (release, receiver) = tokio::sync::oneshot::channel();
        let cleanup_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let cleaned_token = Arc::new(AtomicU64::new(0));
        let cleanup_count_for_callback = Arc::clone(&cleanup_count);
        let cleaned_token_for_callback = Arc::clone(&cleaned_token);
        let handle: tauri::async_runtime::JoinHandle<u64> =
            tauri::async_runtime::spawn(async move {
                let _ = receiver.await;
                42
            });
        let owner = SupervisedCreation::new(
            handle,
            Box::new(move |token| {
                Box::pin(async move {
                    cleaned_token_for_callback.store(token, Ordering::Release);
                    cleanup_count_for_callback.fetch_add(1, Ordering::AcqRel);
                })
            }),
        );
        drop(owner);
        let _ = release.send(());
        tauri::async_runtime::block_on(async {
            tokio::time::timeout(Duration::from_secs(1), async {
                while cleanup_count.load(Ordering::Acquire) == 0 {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("supervised creation should clean the returned token");
        });
        assert_eq!(cleanup_count.load(Ordering::Acquire), 1);
        assert_eq!(cleaned_token.load(Ordering::Acquire), 42);
    }

    #[test]
    fn durable_cancellation_race_cancels_event_setup_promptly() {
        let (signal, _) = tokio::sync::watch::channel(false);
        tauri::async_runtime::block_on(async {
            let (started, started_receiver) = tokio::sync::oneshot::channel();
            let dropped = Arc::new(AtomicBool::new(false));
            let late_callback = Arc::new(AtomicBool::new(false));
            let dropped_for_task = Arc::clone(&dropped);
            let late_callback_for_task = Arc::clone(&late_callback);
            let mut receiver = signal.subscribe();
            let task = tauri::async_runtime::spawn(async move {
                struct DropMarker(Arc<AtomicBool>);
                impl Drop for DropMarker {
                    fn drop(&mut self) {
                        self.0.store(true, Ordering::Release);
                    }
                }
                race_durable_cancellation(&mut receiver, async move {
                    let _marker = DropMarker(dropped_for_task);
                    let _ = started.send(());
                    std::future::pending::<()>().await;
                    late_callback_for_task.store(true, Ordering::Release);
                })
                .await
            });
            started_receiver
                .await
                .expect("setup future should be polled");
            signal.send_replace(true);
            assert!(tokio::time::timeout(Duration::from_secs(1), task)
                .await
                .expect("event setup cancellation should settle promptly")
                .expect("event setup task should join")
                .is_err());
            assert!(dropped.load(Ordering::Acquire));
            assert!(!late_callback.load(Ordering::Acquire));
        });
    }

    #[test]
    fn durable_cancellation_race_cancels_prompt_setup_without_late_callback() {
        let (signal, _) = tokio::sync::watch::channel(false);
        tauri::async_runtime::block_on(async {
            let (started, started_receiver) = tokio::sync::oneshot::channel();
            let dropped = Arc::new(AtomicBool::new(false));
            let late_callback = Arc::new(AtomicBool::new(false));
            let dropped_for_task = Arc::clone(&dropped);
            let late_callback_for_task = Arc::clone(&late_callback);
            let mut receiver = signal.subscribe();
            let task = tauri::async_runtime::spawn(async move {
                struct DropMarker(Arc<AtomicBool>);
                impl Drop for DropMarker {
                    fn drop(&mut self) {
                        self.0.store(true, Ordering::Release);
                    }
                }
                race_durable_cancellation(&mut receiver, async move {
                    let _marker = DropMarker(dropped_for_task);
                    let _ = started.send(());
                    std::future::pending::<()>().await;
                    late_callback_for_task.store(true, Ordering::Release);
                })
                .await
            });
            started_receiver
                .await
                .expect("setup future should be polled");
            signal.send_replace(true);
            assert!(tokio::time::timeout(Duration::from_secs(1), task)
                .await
                .expect("prompt setup cancellation should settle promptly")
                .expect("prompt setup task should join")
                .is_err());
            assert!(dropped.load(Ordering::Acquire));
            assert!(!late_callback.load(Ordering::Acquire));
        });
    }

    #[test]
    fn cancellation_before_waiter_registration_is_observed_by_watch() {
        let (signal, receiver) = tokio::sync::watch::channel(false);
        drop(receiver);
        signal.send_replace(true);
        assert!(*signal.subscribe().borrow());
    }

    #[test]
    fn adopted_refusal_prefetch_preserves_typed_content() {
        let key = PrefetchKey {
            prompt: String::from("refuse"),
            history: Vec::new(),
            model: voxgolem_core::assistant::InstantModel::CustomLunaLow,
        };
        let cache = Mutex::new(Some(PrefetchEntry {
            generation: 1,
            settings_generation: 0,
            key: key.clone(),
            answer: voxgolem_core::assistant::Content::Refusal(String::from("cannot comply")),
        }));
        let generation = std::sync::atomic::AtomicU64::new(1);
        let settings_generation = std::sync::atomic::AtomicU64::new(0);
        assert_eq!(
            take_and_invalidate_prefetch(&cache, &generation, &settings_generation, &key).unwrap(),
            Some(voxgolem_core::assistant::Content::Refusal(String::from(
                "cannot comply"
            )))
        );
    }

    #[test]
    fn failed_instant_stage_is_not_reported_as_successful_telemetry() {
        let event = PromptExecutionEventPayload::Stage {
            stage: StagePayload::Instant,
            status: StageStatusPayload::Failed,
            detail: Some(String::from("transport failed")),
        };
        let value = serde_json::to_value(event).unwrap();
        assert_eq!(value["stage"], "instant");
        assert_eq!(value["status"], "failed");
    }

    #[test]
    fn deep_running_event_is_distinct_from_queued_event() {
        let queued = serde_json::to_value(PromptExecutionEventPayload::Stage {
            stage: StagePayload::Deep,
            status: StageStatusPayload::Queued,
            detail: None,
        })
        .unwrap();
        let running = serde_json::to_value(PromptExecutionEventPayload::Stage {
            stage: StagePayload::Deep,
            status: StageStatusPayload::Running,
            detail: None,
        })
        .unwrap();
        assert_eq!(queued["status"], "queued");
        assert_eq!(running["status"], "running");
    }

    #[test]
    fn main_capability_grants_only_event_listener_permissions() {
        let capability: serde_json::Value =
            serde_json::from_str(include_str!("../capabilities/main.json"))
                .expect("main capability should contain valid JSON");

        assert_eq!(capability["identifier"], "main");
        assert_eq!(capability["windows"], serde_json::json!(["main"]));
        assert_eq!(
            capability["permissions"],
            serde_json::json!(["core:event:allow-listen", "core:event:allow-unlisten"])
        );
    }

    #[test]
    fn load_llama_cpp_system_prompt_reads_soul_file_from_appdata() {
        let _appdata_lock = APPDATA_ENV_LOCK
            .lock()
            .expect("APPDATA test lock should not be poisoned");
        let temp_dir = tempfile::tempdir().expect("temp dir should be created");
        let appdata_path = temp_dir.path().join("VoxGolem");
        std::fs::create_dir_all(&appdata_path).expect("appdata path should be creatable");
        std::fs::write(
            appdata_path.join("SOUL.md"),
            "  You are JARVIS, concise and precise.  ",
        )
        .expect("SOUL.md should be writable");

        let previous_appdata = std::env::var_os("APPDATA");
        std::env::set_var("APPDATA", temp_dir.path());

        let result = load_llama_cpp_system_prompt();

        match previous_appdata {
            Some(value) => std::env::set_var("APPDATA", value),
            None => std::env::remove_var("APPDATA"),
        }

        assert_eq!(
            result.expect("SOUL.md should load"),
            "You are JARVIS, concise and precise."
        );
    }

    #[test]
    fn supported_response_profiles_includes_quality_when_configured() {
        let profiles =
            supported_response_profiles(&voxgolem_core::config::ResponseBackendConfig::LlamaCpp {
                server_path: PathBuf::from("llama-server.exe"),
                host: String::from("127.0.0.1"),
                port: 11_435,
                fast_model_path: PathBuf::from("fast.gguf"),
                quality_model_path: Some(PathBuf::from("quality.gguf")),
            });

        assert_eq!(
            profiles,
            vec![
                ResponseProfilePayload::Fast,
                ResponseProfilePayload::Quality
            ]
        );
    }

    #[test]
    fn model_path_for_profile_rejects_quality_when_missing() {
        let result = model_path_for_profile(
            ResponseProfilePayload::Quality,
            Path::new("fast.gguf"),
            None,
        );

        assert_eq!(
            result,
            Err(String::from("response profile `quality` is not supported"))
        );
    }

    #[test]
    fn restored_runtime_marks_requested_profile_failed() {
        let mut capabilities = vec![
            CapabilityPayload {
                id: "local_fast",
                state: CapabilityStatePayload::Available,
                reason: String::from("ready"),
                actual_provider: Some("cpu"),
            },
            CapabilityPayload {
                id: "local_quality",
                state: CapabilityStatePayload::Available,
                reason: String::from("ready"),
                actual_provider: Some("cpu"),
            },
        ];

        update_restored_profile_capabilities(
            &mut capabilities,
            ResponseProfilePayload::Quality,
            ResponseProfilePayload::Fast,
            "boom",
            "cpu",
        );

        assert_eq!(capabilities[0].state, CapabilityStatePayload::Available);
        assert_eq!(capabilities[0].actual_provider, Some("cpu"));
        assert_eq!(capabilities[1].state, CapabilityStatePayload::Failed);
        assert_eq!(
            capabilities[1].reason,
            "failed to initialize requested quality profile: boom"
        );
        assert_eq!(capabilities[1].actual_provider, None);
    }

    #[test]
    fn same_profile_restore_marks_recovered_runtime_available() {
        let mut capabilities = vec![CapabilityPayload {
            id: "local_fast",
            state: CapabilityStatePayload::Failed,
            reason: String::from("old failure"),
            actual_provider: None,
        }];

        update_restored_profile_capabilities(
            &mut capabilities,
            ResponseProfilePayload::Fast,
            ResponseProfilePayload::Fast,
            "transient failure",
            "cuda",
        );

        assert_eq!(capabilities[0].state, CapabilityStatePayload::Available);
        assert_eq!(capabilities[0].reason, "ready");
        assert_eq!(capabilities[0].actual_provider, Some("cuda"));
    }

    #[test]
    fn failed_same_profile_retry_keeps_shell_ready_with_failed_local_capability() {
        let snapshot = super::StartupSnapshot {
            cue_asset_paths: super::CueAssetPathsPayload {
                start_listening: String::from("start"),
                stop_listening: String::from("stop"),
            },
            voice_input_available: true,
            voice_input_error: None,
            silence_timeout_ms: 1_500,
            tts_enabled: true,
            tts_output_gain_db: 0.0,
            supported_response_profiles: vec![ResponseProfilePayload::Fast],
            capabilities: vec![CapabilityPayload {
                id: "local_fast",
                state: CapabilityStatePayload::Available,
                reason: String::from("ready"),
                actual_provider: Some("cuda"),
            }],
        };

        let state = super::startup_state_after_profile_restore_failure(
            &snapshot,
            ResponseProfilePayload::Fast,
            ResponseProfilePayload::Fast,
            "first failure",
            "second failure",
        );

        let super::StartupStatePayload::Ready {
            selected_response_profile,
            capabilities,
            ..
        } = state
        else {
            panic!("local retry failure must not disable the shell");
        };
        assert_eq!(selected_response_profile, ResponseProfilePayload::Fast);
        assert_eq!(capabilities[0].state, CapabilityStatePayload::Failed);
        assert!(capabilities[0].reason.contains("second failure"));
        assert_eq!(capabilities[0].actual_provider, None);
    }

    #[test]
    fn parse_persisted_state_supports_fast_and_quality() {
        assert_eq!(
            parse_persisted_state("selected_response_profile = \"fast\"\n")
                .expect("fast profile should parse")
                .selected_response_profile,
            Some(ResponseProfilePayload::Fast)
        );
        assert_eq!(
            parse_persisted_state("selected_response_profile = \"quality\"\n")
                .expect("quality profile should parse")
                .selected_response_profile,
            Some(ResponseProfilePayload::Quality)
        );
    }

    #[test]
    fn parse_persisted_state_reads_profile_and_tts_flag() {
        let persisted =
            parse_persisted_state("selected_response_profile = \"fast\"\ntts_enabled = true\n")
                .expect("state should parse");

        assert_eq!(
            persisted.selected_response_profile,
            Some(ResponseProfilePayload::Fast)
        );
        assert_eq!(persisted.tts_enabled, Some(true));
    }

    #[test]
    fn parse_persisted_state_reads_ui_text_size() {
        let persisted = parse_persisted_state(
            "selected_response_profile = \"fast\"\ntts_enabled = true\nui_text_size = \"extra_large\"\n",
        )
        .expect("state should parse");

        assert_eq!(persisted.ui_text_size, Some(UiTextSizePayload::ExtraLarge));
    }

    #[test]
    fn parse_persisted_state_reads_ui_theme() {
        let persisted = parse_persisted_state(
            "selected_response_profile = \"fast\"\ntts_enabled = true\nui_text_size = \"large\"\nui_theme = \"dark\"\n",
        )
        .expect("state should parse");

        assert_eq!(persisted.ui_theme, Some(UiThemePayload::Dark));
    }

    #[test]
    fn parse_persisted_state_reads_assistant_settings() {
        let state = parse_persisted_state(
            "assistant_instant = \"custom-luna-low\"\nassistant_deep = \"opencode-sol-high\"\nassistant_review = \"custom-sol-high\"\nassistant_deep_enabled = true\nassistant_review_enabled = false\nassistant_prefetch = true\nassistant_completion = false\n",
        )
        .expect("assistant settings should parse");

        assert_eq!(
            state.assistant_settings,
            Some(AssistantSettingsPayload {
                instant: InstantChoicePayload::CustomLunaLow,
                deep: AgentChoicePayload::OpenCodeSolHigh,
                review: AgentChoicePayload::CustomSolHigh,
                deep_enabled: true,
                review_enabled: false,
                prefetch: true,
                completion: false,
            })
        );
    }

    #[test]
    fn agent_json_parsers_reject_unknown_fields_and_accept_escaped_text() {
        assert!(parse_deep_agent_json(
            r#"{"complete_answer":"answer","voice_summary":"Done.","sources":[],"extra":true}"#,
            1,
            true,
        )
        .is_err());
        let review = parse_review_agent_json(
            r#"{"decision":"rewrite","replacement":{"type":"text","content":"Use \"quoted\" text, then continue."},"correction":"Correction: Use the verified value."}"#,
        )
        .expect("escaped Review JSON should parse");
        assert!(matches!(
            review.decision,
            voxgolem_core::agent_pipeline::ReviewDecision::Rewrite { replacement, .. }
                if matches!(&replacement, voxgolem_core::assistant::Content::Text(text) if text.contains("quoted"))
        ));
        assert!(parse_review_agent_json(r#"{"decision":"keep","extra":true}"#).is_err());
    }

    #[test]
    fn persist_assistant_settings_preserves_legacy_profile() {
        let _appdata_lock = APPDATA_ENV_LOCK
            .lock()
            .expect("APPDATA test lock should not be poisoned");
        let temp_dir = tempfile::tempdir().expect("temp dir should be created");
        let previous_appdata = std::env::var_os("APPDATA");
        std::env::set_var("APPDATA", temp_dir.path());

        persist_selected_response_profile(ResponseProfilePayload::Quality)
            .expect("profile should persist");
        let settings = AssistantSettingsPayload {
            instant: InstantChoicePayload::OpenCodeLunaLow,
            completion: false,
            ..AssistantSettingsPayload::default()
        };
        persist_assistant_settings(settings).expect("assistant settings should persist");
        let persisted = load_persisted_state().expect("state should reload");

        match previous_appdata {
            Some(value) => std::env::set_var("APPDATA", value),
            None => std::env::remove_var("APPDATA"),
        }

        assert_eq!(
            persisted.selected_response_profile,
            Some(ResponseProfilePayload::Quality)
        );
        assert_eq!(persisted.assistant_settings, Some(settings));
    }

    #[test]
    fn startup_cleanup_removes_only_recognized_orphan_state_temps() {
        let _appdata_lock = APPDATA_ENV_LOCK
            .lock()
            .expect("APPDATA test lock should not be poisoned");
        let temp_dir = tempfile::tempdir().expect("temp dir should be created");
        let previous_appdata = std::env::var_os("APPDATA");
        std::env::set_var("APPDATA", temp_dir.path());
        let state_dir = temp_dir.path().join("VoxGolem");
        fs::create_dir_all(&state_dir).expect("state directory");
        let canonical = state_dir.join("state.toml");
        let orphan = state_dir.join("state.tmp-1234");
        let unrelated = state_dir.join("other.tmp-1234");
        let near_misses = [
            state_dir.join("state.tmp-"),
            state_dir.join("state.tmp-backup"),
            state_dir.join("state.tmp-1234.bak"),
        ];
        fs::write(&canonical, b"ui_theme = \"dark\"\n").expect("canonical fixture");
        fs::write(&orphan, b"partial").expect("orphan fixture");
        fs::write(&unrelated, b"keep").expect("unrelated fixture");
        for path in &near_misses {
            fs::write(path, b"keep").expect("near-miss fixture");
        }

        let state_lock = lock_persisted_state_file(&canonical).expect("state lock");
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (cleanup_tx, cleanup_rx) = std::sync::mpsc::channel();
        let cleanup = std::thread::spawn(move || {
            cleanup_tx
                .send(cleanup_orphan_state_temps_with(|| {
                    started_tx.send(()).expect("cleanup started");
                }))
                .expect("cleanup result");
        });
        started_rx
            .recv()
            .expect("cleanup should reach lock boundary");
        assert!(cleanup_rx.recv_timeout(Duration::from_millis(20)).is_err());
        drop(state_lock);
        cleanup_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("cleanup result")
            .expect("orphan cleanup should succeed");
        cleanup.join().expect("cleanup worker");

        match previous_appdata {
            Some(value) => std::env::set_var("APPDATA", value),
            None => std::env::remove_var("APPDATA"),
        }
        assert!(canonical.is_file());
        assert!(!orphan.exists());
        assert!(unrelated.is_file());
        assert!(near_misses.iter().all(|path| path.is_file()));
    }

    #[test]
    fn exit_rolls_back_a_profile_persisted_before_publication() {
        let _appdata_lock = APPDATA_ENV_LOCK
            .lock()
            .expect("APPDATA test lock should not be poisoned");
        let temp_dir = tempfile::tempdir().expect("temp dir should be created");
        let previous_appdata = std::env::var_os("APPDATA");
        std::env::set_var("APPDATA", temp_dir.path());
        let previous_settings = AssistantSettingsPayload::default();
        super::persist_profile_and_assistant_settings(
            ResponseProfilePayload::Fast,
            previous_settings,
        )
        .expect("previous profile should persist");
        super::persist_profile_and_assistant_settings(
            ResponseProfilePayload::Quality,
            AssistantSettingsPayload {
                instant: InstantChoicePayload::LocalQuality,
                ..previous_settings
            },
        )
        .expect("candidate profile should persist");

        let exit = Arc::new(AtomicBool::new(true));
        let generation = Arc::new(AtomicU64::new(8));
        drop(ProfilePersistenceRollback {
            previous_profile: ResponseProfilePayload::Fast,
            previous_settings,
            exit_cleanup_started: exit,
            switch_generation: generation,
            expected_generation: 7,
            armed: true,
        });
        let persisted = load_persisted_state().expect("state should reload");

        match previous_appdata {
            Some(value) => std::env::set_var("APPDATA", value),
            None => std::env::remove_var("APPDATA"),
        }
        assert_eq!(
            persisted.selected_response_profile,
            Some(ResponseProfilePayload::Fast)
        );
        assert_eq!(persisted.assistant_settings, Some(previous_settings));
    }

    #[test]
    fn parse_persisted_state_rejects_invalid_ui_text_size() {
        let result = parse_persisted_state("ui_text_size = \"giant\"\n");

        assert_eq!(
            result,
            Err(String::from(
                "invalid state.toml: unsupported ui_text_size `giant`"
            ))
        );
    }

    #[test]
    fn parse_persisted_state_rejects_invalid_ui_theme() {
        let result = parse_persisted_state("ui_theme = \"sepia\"\n");

        assert_eq!(
            result,
            Err(String::from(
                "invalid state.toml: unsupported ui_theme `sepia`"
            ))
        );
    }

    #[test]
    fn load_persisted_tts_enabled_reads_boolean_flag_from_state_file() {
        let _appdata_lock = APPDATA_ENV_LOCK
            .lock()
            .expect("APPDATA test lock should not be poisoned");
        let temp_dir = tempfile::tempdir().expect("temp dir should be created");
        let previous_appdata = std::env::var_os("APPDATA");
        std::env::set_var("APPDATA", temp_dir.path());

        persist_tts_enabled(true).expect("tts flag should be written");
        let persisted = load_persisted_tts_enabled().expect("tts flag should load");

        match previous_appdata {
            Some(value) => std::env::set_var("APPDATA", value),
            None => std::env::remove_var("APPDATA"),
        }

        assert_eq!(persisted, Some(true));
    }

    #[test]
    fn load_persisted_ui_text_size_reads_text_size_flag_from_state_file() {
        let _appdata_lock = APPDATA_ENV_LOCK
            .lock()
            .expect("APPDATA test lock should not be poisoned");
        let temp_dir = tempfile::tempdir().expect("temp dir should be created");
        let previous_appdata = std::env::var_os("APPDATA");
        std::env::set_var("APPDATA", temp_dir.path());

        persist_ui_text_size(UiTextSizePayload::Large).expect("text size should be written");
        let persisted = load_persisted_ui_text_size().expect("text size should load");

        match previous_appdata {
            Some(value) => std::env::set_var("APPDATA", value),
            None => std::env::remove_var("APPDATA"),
        }

        assert_eq!(persisted, Some(UiTextSizePayload::Large));
    }

    #[test]
    fn load_persisted_ui_theme_reads_theme_flag_from_state_file() {
        let _appdata_lock = APPDATA_ENV_LOCK
            .lock()
            .expect("APPDATA test lock should not be poisoned");
        let temp_dir = tempfile::tempdir().expect("temp dir should be created");
        let previous_appdata = std::env::var_os("APPDATA");
        std::env::set_var("APPDATA", temp_dir.path());

        persist_ui_theme(UiThemePayload::Light).expect("theme should be written");
        let persisted = load_persisted_ui_theme().expect("theme should load");

        match previous_appdata {
            Some(value) => std::env::set_var("APPDATA", value),
            None => std::env::remove_var("APPDATA"),
        }

        assert_eq!(persisted, Some(UiThemePayload::Light));
    }

    #[test]
    fn persist_selected_response_profile_preserves_tts_enabled_flag() {
        let _appdata_lock = APPDATA_ENV_LOCK
            .lock()
            .expect("APPDATA test lock should not be poisoned");
        let temp_dir = tempfile::tempdir().expect("temp dir should be created");
        let previous_appdata = std::env::var_os("APPDATA");
        std::env::set_var("APPDATA", temp_dir.path());

        persist_tts_enabled(true).expect("tts flag should be written");
        persist_selected_response_profile(ResponseProfilePayload::Quality)
            .expect("profile state should be written");

        let persisted = load_persisted_tts_enabled().expect("tts flag should load");

        match previous_appdata {
            Some(value) => std::env::set_var("APPDATA", value),
            None => std::env::remove_var("APPDATA"),
        }

        assert_eq!(persisted, Some(true));
    }

    #[test]
    fn persist_ui_text_size_preserves_profile_and_tts_enabled_flags() {
        let _appdata_lock = APPDATA_ENV_LOCK
            .lock()
            .expect("APPDATA test lock should not be poisoned");
        let temp_dir = tempfile::tempdir().expect("temp dir should be created");
        let previous_appdata = std::env::var_os("APPDATA");
        std::env::set_var("APPDATA", temp_dir.path());

        persist_selected_response_profile(ResponseProfilePayload::Quality)
            .expect("profile state should be written");
        persist_tts_enabled(true).expect("tts flag should be written");
        persist_ui_text_size(UiTextSizePayload::Small).expect("text size should be written");
        let persisted = load_persisted_state().expect("persisted state should load");

        match previous_appdata {
            Some(value) => std::env::set_var("APPDATA", value),
            None => std::env::remove_var("APPDATA"),
        }

        assert_eq!(
            persisted.selected_response_profile,
            Some(ResponseProfilePayload::Quality)
        );
        assert_eq!(persisted.tts_enabled, Some(true));
        assert_eq!(persisted.ui_text_size, Some(UiTextSizePayload::Small));
    }

    #[test]
    fn persist_ui_theme_preserves_existing_state_flags() {
        let _appdata_lock = APPDATA_ENV_LOCK
            .lock()
            .expect("APPDATA test lock should not be poisoned");
        let temp_dir = tempfile::tempdir().expect("temp dir should be created");
        let previous_appdata = std::env::var_os("APPDATA");
        std::env::set_var("APPDATA", temp_dir.path());

        persist_selected_response_profile(ResponseProfilePayload::Quality)
            .expect("profile state should be written");
        persist_tts_enabled(true).expect("tts flag should be written");
        persist_ui_text_size(UiTextSizePayload::ExtraLarge).expect("text size should be written");
        persist_ui_theme(UiThemePayload::Dark).expect("theme should be written");
        let persisted = load_persisted_state().expect("persisted state should load");

        match previous_appdata {
            Some(value) => std::env::set_var("APPDATA", value),
            None => std::env::remove_var("APPDATA"),
        }

        assert_eq!(
            persisted.selected_response_profile,
            Some(ResponseProfilePayload::Quality)
        );
        assert_eq!(persisted.tts_enabled, Some(true));
        assert_eq!(persisted.ui_text_size, Some(UiTextSizePayload::ExtraLarge));
        assert_eq!(persisted.ui_theme, Some(UiThemePayload::Dark));
    }

    #[test]
    fn resolve_effective_tts_enabled_prefers_persisted_state() {
        let _appdata_lock = APPDATA_ENV_LOCK
            .lock()
            .expect("APPDATA test lock should not be poisoned");
        let temp_dir = tempfile::tempdir().expect("temp dir should be created");
        let previous_appdata = std::env::var_os("APPDATA");
        std::env::set_var("APPDATA", temp_dir.path());

        persist_tts_enabled(true).expect("tts flag should be written");
        let effective = resolve_effective_tts_enabled(false, true);

        match previous_appdata {
            Some(value) => std::env::set_var("APPDATA", value),
            None => std::env::remove_var("APPDATA"),
        }

        assert!(effective);
    }

    #[test]
    fn resolve_effective_tts_enabled_ignores_persisted_true_without_model() {
        let _appdata_lock = APPDATA_ENV_LOCK
            .lock()
            .expect("APPDATA test lock should not be poisoned");
        let temp_dir = tempfile::tempdir().expect("temp dir should be created");
        let previous_appdata = std::env::var_os("APPDATA");
        std::env::set_var("APPDATA", temp_dir.path());

        persist_tts_enabled(true).expect("tts flag should be written");
        let effective = resolve_effective_tts_enabled(false, false);

        match previous_appdata {
            Some(value) => std::env::set_var("APPDATA", value),
            None => std::env::remove_var("APPDATA"),
        }

        assert!(!effective);
    }

    #[test]
    fn persist_selected_response_profile_writes_state_file_in_appdata() {
        let _appdata_lock = APPDATA_ENV_LOCK
            .lock()
            .expect("APPDATA test lock should not be poisoned");
        let temp_dir = tempfile::tempdir().expect("temp dir should be created");
        let previous_appdata = std::env::var_os("APPDATA");
        std::env::set_var("APPDATA", temp_dir.path());

        persist_selected_response_profile(ResponseProfilePayload::Quality)
            .expect("profile state should be written");
        let state_path = response_profile_state_path().expect("state path should resolve");
        let state_contents =
            std::fs::read_to_string(&state_path).expect("state file should be readable");

        match previous_appdata {
            Some(value) => std::env::set_var("APPDATA", value),
            None => std::env::remove_var("APPDATA"),
        }

        assert!(state_path
            .to_string_lossy()
            .replace('\\', "/")
            .ends_with("VoxGolem/state.toml"));
        assert_eq!(state_contents, "selected_response_profile = \"quality\"\n");
    }

    #[test]
    fn atomic_state_replacement_preserves_previous_file_when_write_fails() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("state.toml");
        std::fs::write(&path, b"previous").expect("previous state");
        let result = atomic_replace_state_file(&path.join("missing").join("state.toml"), b"next");
        assert!(result.is_err());
        assert_eq!(std::fs::read_to_string(path).unwrap(), "previous");
    }

    #[cfg(windows)]
    #[test]
    fn windows_atomic_state_replacement_replaces_existing_file() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("state.toml");
        std::fs::write(&path, b"previous").expect("previous state");
        atomic_replace_state_file(&path, b"next").expect("replacement should succeed");
        assert_eq!(std::fs::read_to_string(path).unwrap(), "next");
    }

    #[test]
    fn runtime_log_path_resolves_in_appdata_logs_directory() {
        let _appdata_lock = APPDATA_ENV_LOCK
            .lock()
            .expect("APPDATA test lock should not be poisoned");
        let temp_dir = tempfile::tempdir().expect("temp dir should be created");
        let previous_appdata = std::env::var_os("APPDATA");
        std::env::set_var("APPDATA", temp_dir.path());

        let log_path = runtime_log_path().expect("runtime log path should resolve");

        match previous_appdata {
            Some(value) => std::env::set_var("APPDATA", value),
            None => std::env::remove_var("APPDATA"),
        }

        assert!(log_path
            .to_string_lossy()
            .replace('\\', "/")
            .ends_with("VoxGolem/logs/runtime.log"));
    }

    #[test]
    fn append_tts_runtime_log_line_writes_tts_tagged_entry() {
        let _appdata_lock = APPDATA_ENV_LOCK
            .lock()
            .expect("APPDATA test lock should not be poisoned");
        let temp_dir = tempfile::tempdir().expect("temp dir should be created");
        let previous_appdata = std::env::var_os("APPDATA");
        std::env::set_var("APPDATA", temp_dir.path());

        super::append_tts_runtime_log_line(true, "runtime initialized successfully")
            .expect("runtime log write should succeed");
        let log_path = runtime_log_path().expect("runtime log path should resolve");
        let contents = std::fs::read_to_string(&log_path).expect("runtime log should be readable");

        match previous_appdata {
            Some(value) => std::env::set_var("APPDATA", value),
            None => std::env::remove_var("APPDATA"),
        }

        assert!(contents.contains("[tts] runtime initialized successfully"));
    }

    #[test]
    fn append_tts_runtime_log_line_skips_file_when_disabled() {
        let _appdata_lock = APPDATA_ENV_LOCK
            .lock()
            .expect("APPDATA test lock should not be poisoned");
        let temp_dir = tempfile::tempdir().expect("temp dir should be created");
        let previous_appdata = std::env::var_os("APPDATA");
        std::env::set_var("APPDATA", temp_dir.path());

        super::append_tts_runtime_log_line(false, "runtime initialized successfully")
            .expect("disabled runtime log write should be a no-op");
        let log_path = runtime_log_path().expect("runtime log path should resolve");

        match previous_appdata {
            Some(value) => std::env::set_var("APPDATA", value),
            None => std::env::remove_var("APPDATA"),
        }

        assert!(!log_path.exists());
    }

    #[test]
    fn runtime_log_message_escapes_line_breaks_and_caps_length() {
        let message = format!(
            "first\r\n{}",
            "x".repeat(super::RUNTIME_LOG_MESSAGE_MAX_CHARS)
        );

        let sanitized = super::sanitize_log_message(&message);

        assert!(sanitized.starts_with("first\\r\\n"));
        assert!(sanitized.ends_with("…[truncated]"));
        assert!(!sanitized.contains('\n'));
        assert!(!sanitized.contains('\r'));
    }

    #[test]
    fn default_response_profile_stays_fast() {
        assert_eq!(default_response_profile(), ResponseProfilePayload::Fast);
    }

    #[test]
    fn maps_core_runtime_phase_to_payload() {
        assert!(matches!(
            to_runtime_phase_payload(voxgolem_core::runtime::RuntimePhase::Processing),
            RuntimePhasePayload::Processing
        ));
    }

    #[test]
    fn contract_response_profile_switch_requires_sleeping_runtime_phase() {
        let voice_pipeline_config = default_voice_pipeline_config();
        let voice_pipeline_state = Mutex::new(
            voxgolem_core::voice_pipeline::VoicePipelineState::new(voice_pipeline_config)
                .expect("voice pipeline should initialize"),
        );

        super::apply_voice_pipeline_transition(
            &voice_pipeline_state,
            voice_pipeline_config,
            voxgolem_core::voice_pipeline::VoicePipelineEvent::StartupValidated,
        )
        .expect("startup validation should set runtime to sleeping");
        assert_eq!(
            super::ensure_response_profile_switch_runtime_is_idle(&voice_pipeline_state),
            Ok(())
        );

        super::apply_voice_pipeline_transition(
            &voice_pipeline_state,
            voice_pipeline_config,
            voxgolem_core::voice_pipeline::VoicePipelineEvent::SubmitPrompt,
        )
        .expect("submit prompt should move runtime to executing");
        assert_eq!(
            super::ensure_response_profile_switch_runtime_is_idle(&voice_pipeline_state),
            Err(String::from(
                "response profile switch is only allowed while runtime is sleeping; current phase is executing"
            ))
        );
    }

    #[test]
    fn ingest_audio_processing_is_independent_of_response_backend_lock() {
        let operation_lock = Mutex::new(());
        let _submit_guard = super::lock_response_backend_operation(&operation_lock)
            .expect("lock should be acquired");

        let config = super::default_voice_pipeline_config();
        let ready_state = voxgolem_core::voice_pipeline::apply_voice_pipeline_event(
            &voxgolem_core::voice_pipeline::VoicePipelineState::new(config)
                .expect("voice pipeline should initialize"),
            config,
            voxgolem_core::voice_pipeline::VoicePipelineEvent::StartupValidated,
        )
        .expect("startup validation should succeed")
        .0;
        let listening_state = super::ingest_audio_frame_with_optional_wake_word_detection(
            &ready_state,
            config,
            vec![0.1, 0.2, 0.3],
            Some(100),
        )
        .expect("wake frame should still be processed while response generation is locked");
        assert_eq!(
            listening_state.session().runtime().phase(),
            voxgolem_core::runtime::RuntimePhase::Listening
        );
    }

    #[test]
    fn invalidating_prefetch_waits_for_cancelled_task_to_settle() {
        let app_state = super::build_startup_error_app_state(
            super::default_voice_pipeline_config(),
            String::from("test"),
        );
        let cancelled = Arc::new(AtomicBool::new(false));
        let (sender, mut receiver) = tokio::sync::watch::channel(false);
        let cancellation_signal = Arc::new(sender);
        let task = tauri::async_runtime::spawn(async move {
            let _ = receiver.changed().await;
        });
        *app_state.prefetch_task.lock().unwrap() = Some(super::ActivePrefetch {
            generation: 1,
            cancelled: Arc::clone(&cancelled),
            cancellation_signal: Arc::clone(&cancellation_signal),
            local_cancellation: Arc::new(Mutex::new(None)),
            task: Some(task),
        });

        tauri::async_runtime::block_on(super::invalidate_and_wait_for_prefetch(&app_state))
            .expect("prefetch cancellation should settle");
        assert!(cancelled.load(Ordering::SeqCst));
        assert!(app_state.prefetch_task.lock().unwrap().is_none());
    }

    #[test]
    fn reset_prefetch_abort_does_not_wait_for_nonterminating_work() {
        let app_state = super::build_startup_error_app_state(
            super::default_voice_pipeline_config(),
            String::from("test"),
        );
        let cancelled = Arc::new(AtomicBool::new(false));
        let (sender, _) = tokio::sync::watch::channel(false);
        let cancellation_signal = Arc::new(sender);
        let task = tauri::async_runtime::spawn(async { std::future::pending::<()>().await });
        *app_state.prefetch_task.lock().unwrap() = Some(super::ActivePrefetch {
            generation: 1,
            cancelled: Arc::clone(&cancelled),
            cancellation_signal,
            local_cancellation: Arc::new(Mutex::new(None)),
            task: Some(task),
        });

        super::abort_prefetch(&app_state).expect("prefetch abort should be immediate");

        assert!(cancelled.load(Ordering::Acquire));
        assert!(app_state.prefetch_task.lock().unwrap().is_none());
    }

    #[test]
    fn contract_cue_assets_are_embedded_data_urls() {
        let cue_asset_paths = super::embedded_cue_asset_paths();

        assert!(cue_asset_paths
            .start_listening
            .starts_with(super::CUE_AUDIO_DATA_URL_PREFIX));
        assert!(cue_asset_paths
            .stop_listening
            .starts_with(super::CUE_AUDIO_DATA_URL_PREFIX));
        assert!(!cue_asset_paths.start_listening.contains("resources/"));
        assert!(!cue_asset_paths.stop_listening.contains("resources/"));
    }

    #[test]
    fn contract_response_profile_switch_requires_ready_startup_state() {
        let warming_state = Arc::new(Mutex::new(super::StartupStatePayload::WarmingModel {
            cue_asset_paths: super::CueAssetPathsPayload {
                start_listening: String::from("resources/start-listening.wav"),
                stop_listening: String::from("resources/stop-listening.wav"),
            },
            runtime_phase: RuntimePhasePayload::Initializing,
            voice_input_available: true,
            voice_input_error: None,
            silence_timeout_ms: DEFAULT_SILENCE_TIMEOUT_MS,
            message: String::from("Loading local Gemma model..."),
            selected_response_profile: ResponseProfilePayload::Quality,
            supported_response_profiles: vec![
                ResponseProfilePayload::Fast,
                ResponseProfilePayload::Quality,
            ],
            prompt_cancellation_available: false,
            tts_enabled: false,
            tts_output_gain_db: 3.0,
            capabilities: Vec::new(),
        }));

        assert_eq!(
            super::ensure_startup_ready_for_profile_switch(&warming_state),
            Err(String::from(
                "response backend is busy; wait for the active operation to finish"
            ))
        );

        let ready_state = Arc::new(Mutex::new(super::StartupStatePayload::Ready {
            cue_asset_paths: super::CueAssetPathsPayload {
                start_listening: String::from("resources/start-listening.wav"),
                stop_listening: String::from("resources/stop-listening.wav"),
            },
            runtime_phase: RuntimePhasePayload::Sleeping,
            voice_input_available: true,
            voice_input_error: None,
            silence_timeout_ms: DEFAULT_SILENCE_TIMEOUT_MS,
            selected_response_profile: ResponseProfilePayload::Fast,
            supported_response_profiles: vec![
                ResponseProfilePayload::Fast,
                ResponseProfilePayload::Quality,
            ],
            prompt_cancellation_available: false,
            tts_enabled: false,
            tts_output_gain_db: 3.0,
            capabilities: Vec::new(),
        }));

        assert_eq!(
            super::ensure_startup_ready_for_profile_switch(&ready_state),
            Ok(())
        );
    }

    #[test]
    fn current_silence_deadline_uses_refreshed_speech_activity_plus_timeout() {
        let voice_pipeline_config = default_voice_pipeline_config();
        let voice_pipeline_state = voxgolem_core::voice_pipeline::apply_voice_pipeline_event(
            &voxgolem_core::voice_pipeline::VoicePipelineState::new(voice_pipeline_config)
                .expect("voice pipeline should initialize"),
            voice_pipeline_config,
            voxgolem_core::voice_pipeline::VoicePipelineEvent::StartupValidated,
        )
        .expect("startup validation should succeed");
        let listening_state = voxgolem_core::voice_pipeline::apply_voice_pipeline_event(
            &voice_pipeline_state.0,
            voice_pipeline_config,
            voxgolem_core::voice_pipeline::VoicePipelineEvent::WakeWordDetected { now_ms: 100 },
        )
        .expect("wake word should start listening");
        let refreshed_state = voxgolem_core::voice_pipeline::apply_voice_pipeline_event(
            &listening_state.0,
            voice_pipeline_config,
            voxgolem_core::voice_pipeline::VoicePipelineEvent::SpeechDetected { now_ms: 450 },
        )
        .expect("speech activity should refresh listening deadline");
        let locked_state = Mutex::new(refreshed_state.0);

        assert_eq!(
            current_silence_deadline(&locked_state, voice_pipeline_config),
            Ok(DEFAULT_SILENCE_TIMEOUT_MS + 450)
        );
    }

    #[test]
    fn invariant_transcription_ready_samples_matches_finished_utterance_length() {
        let action = voxgolem_core::voice_pipeline::VoicePipelineAction::FinishedUtterance {
            transcription_input: voxgolem_model::parakeet::ParakeetTranscriptionInput::new(
                voxgolem_model::parakeet::PARAKEET_SAMPLE_RATE_HZ,
                vec![0.1, 0.2, 0.3],
            )
            .expect("valid transcription input"),
        };

        assert_eq!(transcription_ready_samples(&action), Some(3));
    }

    #[test]
    fn runtime_phase_response_from_state_reflects_capture_lengths() {
        let voice_pipeline_config = default_voice_pipeline_config();
        let ready_state = voxgolem_core::voice_pipeline::apply_voice_pipeline_event(
            &voxgolem_core::voice_pipeline::VoicePipelineState::new(voice_pipeline_config)
                .expect("voice pipeline should initialize"),
            voice_pipeline_config,
            voxgolem_core::voice_pipeline::VoicePipelineEvent::StartupValidated,
        )
        .expect("startup validation should succeed")
        .0;
        let preroll_state = voxgolem_core::voice_pipeline::ingest_audio_frame(
            &ready_state,
            voice_pipeline_config,
            vec![0.1, 0.2, 0.3],
        )
        .expect("sleeping frame should be recorded");
        let listening_state = voxgolem_core::voice_pipeline::apply_voice_pipeline_event(
            &preroll_state,
            voice_pipeline_config,
            voxgolem_core::voice_pipeline::VoicePipelineEvent::WakeWordDetected { now_ms: 100 },
        )
        .expect("wake word should start listening")
        .0;
        let utterance_state = voxgolem_core::voice_pipeline::ingest_audio_frame(
            &listening_state,
            voice_pipeline_config,
            vec![0.4, 0.5],
        )
        .expect("listening frame should be recorded");

        assert_eq!(
            runtime_phase_response_from_state(&utterance_state, None, None, None),
            RuntimePhaseResponsePayload {
                runtime_phase: RuntimePhasePayload::Listening,
                transcription_ready_samples: None,
                transcript_text: None,
                last_activity_ms: Some(100),
                capturing_utterance: true,
                preroll_samples: 3,
                utterance_samples: 2,
                telemetry: None,
            }
        );
    }

    #[test]
    fn contract_runtime_phase_response_from_state_surfaces_telemetry() {
        let voice_pipeline_config = default_voice_pipeline_config();
        let ready_state = voxgolem_core::voice_pipeline::apply_voice_pipeline_event(
            &voxgolem_core::voice_pipeline::VoicePipelineState::new(voice_pipeline_config)
                .expect("voice pipeline should initialize"),
            voice_pipeline_config,
            voxgolem_core::voice_pipeline::VoicePipelineEvent::StartupValidated,
        )
        .expect("startup validation should succeed")
        .0;
        let preroll_state = voxgolem_core::voice_pipeline::ingest_audio_frame(
            &ready_state,
            voice_pipeline_config,
            vec![0.1, 0.2, 0.3],
        )
        .expect("sleeping frame should be recorded");
        let listening_state = voxgolem_core::voice_pipeline::apply_voice_pipeline_event(
            &preroll_state,
            voice_pipeline_config,
            voxgolem_core::voice_pipeline::VoicePipelineEvent::WakeWordDetected { now_ms: 100 },
        )
        .expect("wake word should start listening")
        .0;
        let utterance_state = voxgolem_core::voice_pipeline::ingest_audio_frame(
            &listening_state,
            voice_pipeline_config,
            vec![0.4, 0.5],
        )
        .expect("listening frame should be recorded");
        let telemetry = RuntimeTelemetryPayload {
            frame_id: Some("frame-1".to_string()),
            backend_ingest_started_ms: Some(110),
            backend_ingest_completed_ms: Some(120),
            wake_detected_ms: Some(118),
            wake_confidence: Some(0.72),
            transcription_started_ms: None,
            transcription_completed_ms: None,
        };

        let response = runtime_phase_response_from_state(
            &utterance_state,
            None,
            None,
            Some(telemetry.clone()),
        );

        assert_eq!(response.runtime_phase, RuntimePhasePayload::Listening);
        assert_eq!(response.telemetry, Some(telemetry));
    }

    #[test]
    fn current_runtime_phase_response_reads_single_snapshot() {
        let voice_pipeline_config = default_voice_pipeline_config();
        let ready_state = voxgolem_core::voice_pipeline::apply_voice_pipeline_event(
            &voxgolem_core::voice_pipeline::VoicePipelineState::new(voice_pipeline_config)
                .expect("voice pipeline should initialize"),
            voice_pipeline_config,
            voxgolem_core::voice_pipeline::VoicePipelineEvent::StartupValidated,
        )
        .expect("startup validation should succeed")
        .0;
        let preroll_state = voxgolem_core::voice_pipeline::ingest_audio_frame(
            &ready_state,
            voice_pipeline_config,
            vec![0.1, 0.2],
        )
        .expect("sleeping frame should be recorded");
        let locked_state = Mutex::new(preroll_state);

        assert_eq!(
            current_runtime_phase_response(
                &locked_state,
                Some(2),
                Some("draft release notes".to_string()),
            ),
            Ok(RuntimePhaseResponsePayload {
                runtime_phase: RuntimePhasePayload::Sleeping,
                transcription_ready_samples: Some(2),
                transcript_text: Some("draft release notes".to_string()),
                last_activity_ms: None,
                capturing_utterance: false,
                preroll_samples: 2,
                utterance_samples: 0,
                telemetry: None,
            })
        );
    }

    #[test]
    fn transcribe_finished_utterance_returns_none_for_non_transcription_actions() {
        assert_eq!(
            transcribe_finished_utterance(
                &voxgolem_core::voice_pipeline::VoicePipelineAction::None,
                &None,
            ),
            Ok(None)
        );
    }

    #[test]
    fn apply_optional_speech_activity_refreshes_last_activity_while_listening() {
        let voice_pipeline_config = default_voice_pipeline_config();
        let listening_state = voxgolem_core::voice_pipeline::apply_voice_pipeline_event(
            &voxgolem_core::voice_pipeline::VoicePipelineState::new(voice_pipeline_config)
                .expect("voice pipeline should initialize"),
            voice_pipeline_config,
            voxgolem_core::voice_pipeline::VoicePipelineEvent::StartupValidated,
        )
        .expect("startup validation should succeed")
        .0;
        let listening_state = voxgolem_core::voice_pipeline::apply_voice_pipeline_event(
            &listening_state,
            voice_pipeline_config,
            voxgolem_core::voice_pipeline::VoicePipelineEvent::WakeWordDetected { now_ms: 100 },
        )
        .expect("wake word should start listening")
        .0;

        let refreshed_state =
            apply_optional_speech_activity(listening_state, voice_pipeline_config, true, 450)
                .expect("speech activity should refresh listening state");

        assert_eq!(
            refreshed_state.session().voice_turn().last_activity_ms(),
            Some(450)
        );
    }

    #[test]
    fn build_mark_silence_response_surfaces_transcript_text() {
        let voice_pipeline_config = default_voice_pipeline_config();
        let processing_state = voxgolem_core::voice_pipeline::apply_voice_pipeline_event(
            &voxgolem_core::voice_pipeline::VoicePipelineState::new(voice_pipeline_config)
                .expect("voice pipeline should initialize"),
            voice_pipeline_config,
            voxgolem_core::voice_pipeline::VoicePipelineEvent::StartupValidated,
        )
        .expect("startup validation should succeed")
        .0;
        let processing_state = voxgolem_core::voice_pipeline::apply_voice_pipeline_event(
            &processing_state,
            voice_pipeline_config,
            voxgolem_core::voice_pipeline::VoicePipelineEvent::WakeWordDetected { now_ms: 100 },
        )
        .expect("wake word should start listening")
        .0;
        let processing_state = voxgolem_core::voice_pipeline::ingest_audio_frame(
            &processing_state,
            voice_pipeline_config,
            vec![0.1, 0.2, 0.3],
        )
        .expect("listening frame should be recorded before silence");
        let processing_state = voxgolem_core::voice_pipeline::apply_voice_pipeline_event(
            &processing_state,
            voice_pipeline_config,
            voxgolem_core::voice_pipeline::VoicePipelineEvent::SilenceCheck {
                now_ms: DEFAULT_SILENCE_TIMEOUT_MS + 101,
            },
        )
        .expect("silence should move runtime to processing")
        .0;
        let locked_state = Mutex::new(processing_state);
        let action = voxgolem_core::voice_pipeline::VoicePipelineAction::FinishedUtterance {
            transcription_input: voxgolem_model::parakeet::ParakeetTranscriptionInput::new(
                voxgolem_model::parakeet::PARAKEET_SAMPLE_RATE_HZ,
                vec![0.1, 0.2, 0.3],
            )
            .expect("valid transcription input"),
        };

        assert_eq!(
            build_mark_silence_response(
                &locked_state,
                &action,
                Some("draft release notes".to_string()),
                Some(RuntimeTelemetryPayload {
                    frame_id: Some("frame-2".to_string()),
                    backend_ingest_started_ms: None,
                    backend_ingest_completed_ms: None,
                    wake_detected_ms: None,
                    wake_confidence: None,
                    transcription_started_ms: Some(2000),
                    transcription_completed_ms: Some(2100),
                }),
            ),
            Ok(RuntimePhaseResponsePayload {
                runtime_phase: RuntimePhasePayload::Processing,
                transcription_ready_samples: Some(3),
                transcript_text: Some("draft release notes".to_string()),
                last_activity_ms: None,
                capturing_utterance: false,
                preroll_samples: 0,
                utterance_samples: 0,
                telemetry: Some(RuntimeTelemetryPayload {
                    frame_id: Some("frame-2".to_string()),
                    backend_ingest_started_ms: None,
                    backend_ingest_completed_ms: None,
                    wake_detected_ms: None,
                    wake_confidence: None,
                    transcription_started_ms: Some(2000),
                    transcription_completed_ms: Some(2100),
                }),
            })
        );
    }

    #[test]
    fn reset_voice_pipeline_to_waiting_returns_runtime_to_sleeping() {
        let voice_pipeline_config = default_voice_pipeline_config();
        let processing_state = voxgolem_core::voice_pipeline::apply_voice_pipeline_event(
            &voxgolem_core::voice_pipeline::VoicePipelineState::new(voice_pipeline_config)
                .expect("voice pipeline should initialize"),
            voice_pipeline_config,
            voxgolem_core::voice_pipeline::VoicePipelineEvent::StartupValidated,
        )
        .expect("startup validation should succeed")
        .0;
        let processing_state = voxgolem_core::voice_pipeline::apply_voice_pipeline_event(
            &processing_state,
            voice_pipeline_config,
            voxgolem_core::voice_pipeline::VoicePipelineEvent::WakeWordDetected { now_ms: 100 },
        )
        .expect("wake word should start listening")
        .0;
        let processing_state = voxgolem_core::voice_pipeline::ingest_audio_frame(
            &processing_state,
            voice_pipeline_config,
            vec![0.1, 0.2, 0.3],
        )
        .expect("listening frame should be recorded before silence");
        let processing_state = voxgolem_core::voice_pipeline::apply_voice_pipeline_event(
            &processing_state,
            voice_pipeline_config,
            voxgolem_core::voice_pipeline::VoicePipelineEvent::SilenceCheck {
                now_ms: DEFAULT_SILENCE_TIMEOUT_MS + 101,
            },
        )
        .expect("silence should move runtime to processing")
        .0;
        let locked_state = Mutex::new(processing_state);

        reset_voice_pipeline_to_waiting(&locked_state, &None, &None, voice_pipeline_config)
            .expect("reset to waiting should succeed");
        assert_eq!(
            current_runtime_phase_response(&locked_state, None, None)
                .expect("runtime snapshot should succeed")
                .runtime_phase,
            RuntimePhasePayload::Sleeping
        );
    }

    #[test]
    fn ingest_audio_frame_wake_detection_starts_listening_without_seeding_preroll() {
        let voice_pipeline_config = default_voice_pipeline_config();
        let ready_state = voxgolem_core::voice_pipeline::apply_voice_pipeline_event(
            &voxgolem_core::voice_pipeline::VoicePipelineState::new(voice_pipeline_config)
                .expect("voice pipeline should initialize"),
            voice_pipeline_config,
            voxgolem_core::voice_pipeline::VoicePipelineEvent::StartupValidated,
        )
        .expect("startup validation should succeed")
        .0;
        let listening_state = ingest_audio_frame_with_optional_wake_word_detection(
            &ready_state,
            voice_pipeline_config,
            vec![0.1, 0.2, 0.3],
            Some(100),
        )
        .expect("wake word detection should promote sleeping to listening");

        assert_eq!(
            listening_state.session().runtime().phase(),
            voxgolem_core::runtime::RuntimePhase::Listening
        );
        assert!(listening_state.capture().capturing_utterance());
        assert_eq!(listening_state.capture().preroll_len(), 0);
        assert_eq!(listening_state.capture().utterance_len(), 3);
    }

    #[test]
    fn process_wake_word_frame_is_a_no_op_without_runtime() {
        assert_eq!(
            process_wake_word_frame(&None, &[0.1, 0.2]),
            Ok((None, None))
        );
    }

    #[test]
    fn process_wake_word_frame_propagates_detector_errors() {
        let runtime = Some(Mutex::new(WakeWordRuntime::new_failing_for_test()));

        assert_eq!(
            process_wake_word_frame(&runtime, &[0.0; 8]),
            Err(String::from("synthetic wake word scorer failure"))
        );
    }

    #[test]
    fn reset_wake_word_runtime_is_a_no_op_without_runtime() {
        assert_eq!(reset_wake_word_runtime(&None), Ok(()));
    }

    #[test]
    fn build_startup_error_app_state_tracks_error_runtime_and_no_wake_word_runtime() {
        let app_state = build_startup_error_app_state(
            default_voice_pipeline_config(),
            "wake word init failed".to_string(),
        );

        assert!(matches!(
            *app_state
                .startup_state
                .lock()
                .expect("startup state lock should not be poisoned"),
            super::StartupStatePayload::Error { .. }
        ));
        assert!(app_state.runtime_config.is_none());
        assert!(app_state.wake_word_runtime.is_none());
        let snapshot = get_app_snapshot_from_state(&app_state).expect("snapshot should serialize");
        assert_eq!(snapshot.capabilities.len(), super::CAPABILITY_IDS.len());
        assert!(snapshot.capabilities.iter().all(|capability| {
            capability.state == voxgolem_core::ipc::CapabilityState::Failed
                && capability.reason.as_str() == "wake word init failed"
        }));
        assert!(snapshot.model_options.iter().all(|option| {
            !option.available
                && option
                    .unavailable_reason
                    .as_ref()
                    .is_some_and(|reason| reason.as_str() == "wake word init failed")
        }));
    }

    #[test]
    fn wake_word_runtime_reports_missing_model_file() {
        let temp_dir = tempfile::tempdir().expect("temp dir should be created");
        let wake_word_model_path = temp_dir.path().join("missing-hey-livekit.onnx");

        let error = WakeWordRuntime::new(&wake_word_model_path, 0.68)
            .err()
            .expect("missing model file should fail");

        assert!(error.contains("failed to load wake word model"));
    }

    #[test]
    fn wake_word_event_timestamp_uses_backend_now_ms() {
        assert_eq!(
            wake_word_event_timestamp(
                42_000,
                Some(WakeWordDetection {
                    detected_at_ms: 60,
                    confidence: 0.73,
                }),
            ),
            Some(42_000)
        );
        assert_eq!(wake_word_event_timestamp(42_000, None), None);
    }

    #[test]
    fn deep_launch_order_is_before_instant_stage() {
        assert_eq!(
            initial_stage_sequence(true),
            vec![StagePayload::Deep, StagePayload::Instant]
        );
        assert_eq!(initial_stage_sequence(false), vec![StagePayload::Instant]);
    }

    #[test]
    fn assistant_settings_ignore_failure_of_existing_optional_capability() {
        let state = Arc::new(Mutex::new(StartupStatePayload::Ready {
            cue_asset_paths: CueAssetPathsPayload {
                start_listening: String::new(),
                stop_listening: String::new(),
            },
            runtime_phase: RuntimePhasePayload::Sleeping,
            voice_input_available: true,
            voice_input_error: None,
            silence_timeout_ms: 1_500,
            selected_response_profile: ResponseProfilePayload::Fast,
            supported_response_profiles: vec![ResponseProfilePayload::Fast],
            prompt_cancellation_available: true,
            tts_enabled: false,
            tts_output_gain_db: 0.0,
            capabilities: vec![CapabilityPayload {
                id: "qwen_prediction",
                state: CapabilityStatePayload::Failed,
                reason: String::from("failed later"),
                actual_provider: None,
            }],
        }));
        let previous = AssistantSettingsPayload {
            completion: true,
            ..Default::default()
        };
        let next = AssistantSettingsPayload {
            completion: true,
            prefetch: !previous.prefetch,
            ..previous
        };
        assert!(ensure_assistant_settings_available(&next, &previous, &state).is_ok());
    }

    #[test]
    fn instant_failure_deep_success_resolves_deep() {
        use voxgolem_core::assistant::*;
        use voxgolem_core::conversation::*;
        let preferences = AssistantPolicy {
            deep_enabled: true,
            ..Default::default()
        };
        let mut session = ConversationSession::new(preferences);
        let generation = session.admit("request", "prompt").unwrap().execution_id;
        assert_eq!(
            session.update(
                generation,
                Stage::Instant,
                StageResult::Instant(InstantOutcome::Failure("instant failed".into()))
            ),
            UpdateResult::Pending
        );
        assert!(matches!(
            session.update(
                generation,
                Stage::Deep,
                StageResult::Deep(DeepOutcome::Success(DeepReport {
                    answer: Content::Text("deep".into())
                }))
            ),
            UpdateResult::Resolved { content: Content::Text(answer), .. } if answer == "deep"
        ));
    }

    #[test]
    fn deep_failure_keeps_successful_instant_and_both_fail() {
        use voxgolem_core::assistant::*;
        use voxgolem_core::conversation::*;
        let preferences = AssistantPolicy {
            deep_enabled: true,
            ..Default::default()
        };
        let mut session = ConversationSession::new(preferences.clone());
        let generation = session.admit("request", "prompt").unwrap().execution_id;
        assert!(matches!(
            session.update(
                generation,
                Stage::Instant,
                StageResult::Instant(InstantOutcome::Complete(Content::Text("instant".into())))
            ),
            UpdateResult::Provisional(Content::Text(answer)) if answer == "instant"
        ));
        assert!(matches!(
            session.update(
                generation,
                Stage::Deep,
                StageResult::Deep(DeepOutcome::Failure("deep failed".into()))
            ),
            UpdateResult::Resolved { content: Content::Text(answer), .. } if answer == "instant"
        ));

        let mut session = ConversationSession::new(preferences);
        let generation = session.admit("request", "prompt").unwrap().execution_id;
        session.update(
            generation,
            Stage::Instant,
            StageResult::Instant(InstantOutcome::Failure("instant failed".into())),
        );
        assert_eq!(
            session.update(
                generation,
                Stage::Deep,
                StageResult::Deep(DeepOutcome::Failure("deep failed".into()))
            ),
            UpdateResult::Pending
        );
    }
}
