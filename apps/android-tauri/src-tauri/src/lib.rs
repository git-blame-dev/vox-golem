use std::{
    future::Future,
    path::{Path, PathBuf},
    pin::Pin,
    sync::{Arc, Mutex},
};

use tauri::{Manager, State};
use voxgolem_core::{
    assistant,
    conversation::{self, ConversationSession},
    ipc,
};
use voxgolem_platform::custom_openai::{
    CustomOpenAiAuthObservation, CustomOpenAiAuthRevision, CustomOpenAiClient, CustomOpenAiConfig,
    CustomOpenAiContentType, CustomOpenAiError, CustomOpenAiMessage, CustomOpenAiModel,
    CustomOpenAiPrompt, CustomOpenAiResponse,
};

const INVALID_REQUEST_ID: &str = "Request ID is empty or too long";
const INVALID_PROMPT: &str = "Prompt is empty or too long";
const BUSY: &str = "A request is already active";
const BACKGROUNDED: &str = "The app must be in the foreground to send a request";
const AUTH_UNAVAILABLE: &str = "OpenCode authentication is unavailable";
const AUTH_EXPIRED: &str = "OpenCode authentication has expired";
const PROVIDER_UNAVAILABLE: &str = "Custom response provider is unavailable";
const PROVIDER_RESPONSE_INVALID: &str = "Custom provider returned an invalid response";

type DeltaSender = tokio::sync::mpsc::UnboundedSender<String>;
struct ProviderAttempt {
    result: Result<CustomOpenAiResponse, CustomOpenAiError>,
    auth_revision: Option<CustomOpenAiAuthRevision>,
}
type ProviderFuture = Pin<Box<dyn Future<Output = ProviderAttempt> + Send>>;

trait Provider: Send + Sync {
    fn check_auth(&self) -> Result<(), CustomOpenAiError>;
    fn auth_revision(&self) -> Option<CustomOpenAiAuthRevision> {
        None
    }
    fn observe_auth(&self) -> CustomOpenAiAuthObservation {
        CustomOpenAiAuthObservation {
            revision: self.auth_revision(),
            status: self.check_auth(),
        }
    }
    fn respond(
        &self,
        model: CustomOpenAiModel,
        request: CustomOpenAiPrompt,
        deltas: DeltaSender,
    ) -> ProviderFuture;
}

struct CustomProvider {
    auth_path: PathBuf,
}
impl Provider for CustomProvider {
    fn check_auth(&self) -> Result<(), CustomOpenAiError> {
        self.client(CustomOpenAiModel::SolHigh)?.check_auth()
    }
    fn auth_revision(&self) -> Option<CustomOpenAiAuthRevision> {
        self.client(CustomOpenAiModel::SolHigh)
            .ok()?
            .auth_revision()
    }
    fn observe_auth(&self) -> CustomOpenAiAuthObservation {
        match self.client(CustomOpenAiModel::SolHigh) {
            Ok(client) => client.observe_auth(),
            Err(error) => CustomOpenAiAuthObservation {
                revision: None,
                status: Err(error),
            },
        }
    }
    fn respond(
        &self,
        model: CustomOpenAiModel,
        request: CustomOpenAiPrompt,
        deltas: DeltaSender,
    ) -> ProviderFuture {
        let client = match self.client(model) {
            Ok(client) => client,
            Err(error) => {
                return Box::pin(async move {
                    ProviderAttempt {
                        result: Err(error),
                        auth_revision: None,
                    }
                });
            }
        };
        Box::pin(async move {
            let mut auth_revision = None;
            let result = client
                .respond_with_auth_revision(
                    &request,
                    None,
                    |delta| {
                        let _ = deltas.send(delta.to_owned());
                    },
                    |revision| auth_revision = Some(revision),
                )
                .await;
            ProviderAttempt {
                result,
                auth_revision,
            }
        })
    }
}
impl CustomProvider {
    fn client(&self, model: CustomOpenAiModel) -> Result<CustomOpenAiClient, CustomOpenAiError> {
        CustomOpenAiClient::new(CustomOpenAiConfig {
            auth_path: self.auth_path.clone(),
            model,
            ..Default::default()
        })
    }
}

struct Physical {
    execution: conversation::ExecutionId,
    cancel: tokio::sync::watch::Sender<bool>,
}
#[derive(Clone)]
struct RemoteAuthRejection {
    revision: CustomOpenAiAuthRevision,
    error: CustomOpenAiError,
}
struct Inner {
    session: ConversationSession,
    preferences: ipc::ValidatedPreferences,
    physical: Option<Physical>,
    foreground: bool,
    remote_auth_rejection: Option<RemoteAuthRejection>,
    remote_auth_rejection_generation: u64,
}
#[derive(Clone)]
struct AppState {
    inner: Arc<Mutex<Inner>>,
    publication: Arc<Mutex<()>>,
    provider: Arc<dyn Provider>,
}

fn app_state(preferences: ipc::ValidatedPreferences, provider: Arc<dyn Provider>) -> AppState {
    AppState {
        inner: Arc::new(Mutex::new(Inner {
            session: ConversationSession::new(policy(&preferences)),
            preferences,
            physical: None,
            foreground: true,
            remote_auth_rejection: None,
            remote_auth_rejection_generation: 0,
        })),
        publication: Arc::new(Mutex::new(())),
        provider,
    }
}

fn auth_path(app_data_dir: impl AsRef<Path>) -> PathBuf {
    app_data_dir.as_ref().join("files").join("codex-auth.json")
}
fn prepare_auth_path(app_data_dir: impl AsRef<Path>) -> std::io::Result<PathBuf> {
    let app_data_dir = app_data_dir.as_ref();
    let files = app_data_dir.join("files");
    match std::fs::symlink_metadata(&files) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "Android app files path is not a directory",
            ));
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let mut builder = std::fs::DirBuilder::new();
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                builder.mode(0o700);
            }
            builder.create(&files)?;
        }
        Err(error) => return Err(error),
    }
    Ok(auth_path(app_data_dir))
}
fn error(code: ipc::CommandErrorCode, message: &'static str) -> ipc::CommandError {
    ipc::CommandError::new(code, message).expect("static command error")
}
fn invalid(message: &'static str) -> ipc::CommandError {
    error(ipc::CommandErrorCode::InvalidInput, message)
}
fn validate(request_id: &str, prompt: Option<&str>) -> Result<(), ipc::CommandError> {
    if request_id.is_empty() || request_id.len() > conversation::MAX_REQUEST_ID_BYTES {
        return Err(invalid(INVALID_REQUEST_ID));
    }
    if let Some(prompt) = prompt {
        if prompt.trim().is_empty() || prompt.len() > conversation::MAX_PROMPT_BYTES {
            return Err(invalid(INVALID_PROMPT));
        }
    }
    Ok(())
}
fn default_preferences() -> ipc::ValidatedPreferences {
    ipc::ValidatedPreferences {
        instant: ipc::InstantModelChoice::CustomSolHigh,
        deep: ipc::AgentModelChoice::CustomSolHigh,
        review: ipc::AgentModelChoice::CustomSolHigh,
        deep_enabled: false,
        review_enabled: false,
        prefetch: false,
        completion: false,
        response_profile: ipc::ResponseProfile::Fast,
        tts_enabled: false,
        ui_theme: ipc::UiTheme::Dark,
        ui_text_size: ipc::UiTextSize::Medium,
    }
}
fn policy(p: &ipc::ValidatedPreferences) -> assistant::AssistantPolicy {
    assistant::AssistantPolicy {
        instant_model: match p.instant {
            ipc::InstantModelChoice::CustomSolNone => assistant::InstantModel::CustomSolNone,
            ipc::InstantModelChoice::CustomLunaNone => assistant::InstantModel::CustomLunaNone,
            ipc::InstantModelChoice::CustomLunaLow => assistant::InstantModel::CustomLunaLow,
            ipc::InstantModelChoice::CustomSolHigh => assistant::InstantModel::CustomSolHigh,
            _ => assistant::InstantModel::CustomSolHigh,
        },
        deep_model: assistant::AgentModel::CustomSolHigh,
        review_model: assistant::AgentModel::CustomSolHigh,
        deep_enabled: false,
        review_enabled: false,
        prefetch_enabled: false,
        completion_enabled: false,
    }
}
fn map_error(e: CustomOpenAiError) -> ipc::CommandError {
    match e {
        CustomOpenAiError::AuthUnavailable => {
            error(ipc::CommandErrorCode::Unavailable, AUTH_UNAVAILABLE)
        }
        CustomOpenAiError::AuthExpired => error(ipc::CommandErrorCode::Unavailable, AUTH_EXPIRED),
        CustomOpenAiError::Unauthorized | CustomOpenAiError::Forbidden => {
            error(ipc::CommandErrorCode::Unavailable, PROVIDER_UNAVAILABLE)
        }
        CustomOpenAiError::RateLimited => error(
            ipc::CommandErrorCode::Unavailable,
            "Custom provider rate limit reached",
        ),
        CustomOpenAiError::Timeout => error(
            ipc::CommandErrorCode::Unavailable,
            "Custom provider timed out",
        ),
        CustomOpenAiError::TooLarge => error(
            ipc::CommandErrorCode::Unavailable,
            "Custom provider response was too large",
        ),
        _ => error(ipc::CommandErrorCode::Unavailable, PROVIDER_UNAVAILABLE),
    }
}

fn snapshot(inner: &Inner, auth: Result<(), CustomOpenAiError>) -> ipc::AppSnapshot {
    let ready = auth.is_ok();
    let reason = match auth {
        Ok(()) => "Ready",
        Err(CustomOpenAiError::AuthExpired) => AUTH_EXPIRED,
        Err(_) => AUTH_UNAVAILABLE,
    };
    let capabilities = [
        ("custom_provider", ready, reason),
        ("opencode", false, "OpenCode is unavailable on Android"),
        (
            "local_fast",
            false,
            "Local models are unavailable on Android",
        ),
        (
            "local_quality",
            false,
            "Local models are unavailable on Android",
        ),
        (
            "qwen_prediction",
            false,
            "Prediction is unavailable on Android",
        ),
        ("wake_word", false, "Wake word is unavailable on Android"),
        (
            "vad",
            false,
            "Voice activity detection is unavailable on Android",
        ),
        ("parakeet", false, "Transcription is unavailable on Android"),
        ("tts", false, "Text to speech is unavailable on Android"),
        ("deep", false, "Deep execution is disabled on Android"),
        ("review", false, "Review execution is disabled on Android"),
    ]
    .into_iter()
    .map(|(id, available, reason)| ipc::Capability {
        id: ipc::BoundedText::new(id).expect("static capability ID fits"),
        state: if available {
            ipc::CapabilityState::Available
        } else {
            ipc::CapabilityState::Unavailable
        },
        reason: ipc::BoundedText::new(reason).expect("static capability reason fits"),
        actual_provider: available
            .then(|| ipc::BoundedText::new("custom_openai").expect("static provider name fits")),
    })
    .collect();
    let options = [
        (
            "custom-sol-high",
            "Sol High",
            ipc::InstantModelChoice::CustomSolHigh,
        ),
        (
            "custom-sol-none",
            "Sol None",
            ipc::InstantModelChoice::CustomSolNone,
        ),
        (
            "custom-luna-none",
            "Luna None",
            ipc::InstantModelChoice::CustomLunaNone,
        ),
        (
            "custom-luna-low",
            "Luna Low",
            ipc::InstantModelChoice::CustomLunaLow,
        ),
    ]
    .into_iter()
    .map(|(id, label, _)| ipc::ModelOption {
        id: ipc::BoundedText::new(id).expect("static model ID fits"),
        label: ipc::BoundedText::new(label).expect("static model label fits"),
        available: ready,
        unavailable_reason: (!ready)
            .then(|| ipc::BoundedText::new(reason).expect("static model reason fits")),
    })
    .collect();
    let session = inner.session.snapshot();
    ipc::AppSnapshot {
        session_id: session.session_id.0.into(),
        runtime_phase: if inner.session.has_active_execution() {
            ipc::RuntimePhase::Executing
        } else {
            ipc::RuntimePhase::Sleeping
        },
        transcript: session.turns.iter().map(ipc::Turn::from).collect(),
        active_request: session.active.as_ref().map(ipc::ActiveRequestSummary::from),
        capabilities,
        model_options: options,
        preferences: inner.preferences.clone(),
    }
}

fn signal(sender: Option<tokio::sync::watch::Sender<bool>>) {
    if let Some(sender) = sender {
        let _ = sender.send(true);
    }
}

struct TerminalClaim {
    result: conversation::SettlementResult,
    cancel: Option<tokio::sync::watch::Sender<bool>>,
}

fn terminal(
    state: &AppState,
    id: conversation::ExecutionId,
    intent: conversation::SettlementIntent,
) -> Result<TerminalClaim, ipc::CommandError> {
    terminal_with_rejection(state, id, intent, None)
}

fn terminal_with_rejection(
    state: &AppState,
    id: conversation::ExecutionId,
    intent: conversation::SettlementIntent,
    rejection: Option<RemoteAuthRejection>,
) -> Result<TerminalClaim, ipc::CommandError> {
    let _publication = state.publication.lock().map_err(|_| {
        error(
            ipc::CommandErrorCode::Internal,
            "Android publication state is unavailable",
        )
    })?;
    let mut inner = state.inner.lock().map_err(|_| {
        error(
            ipc::CommandErrorCode::Internal,
            "Android shell state is unavailable",
        )
    })?;
    if inner
        .physical
        .as_ref()
        .is_none_or(|physical| physical.execution != id)
    {
        return Ok(TerminalClaim {
            result: conversation::SettlementResult::IgnoredStale,
            cancel: None,
        });
    }
    let cancel = inner.physical.take().map(|physical| physical.cancel);
    let result = inner.session.settle(id, intent);
    if matches!(
        result,
        conversation::SettlementResult::Applied(conversation::AppliedOutcome::Failed)
    ) {
        if let Some(rejection) = rejection {
            inner.remote_auth_rejection = Some(rejection);
            inner.remote_auth_rejection_generation =
                inner.remote_auth_rejection_generation.wrapping_add(1);
        }
    }
    Ok(TerminalClaim { result, cancel })
}

fn cancel_execution(state: &AppState, id: conversation::ExecutionId) -> bool {
    let Ok(claim) = terminal(state, id, conversation::SettlementIntent::Cancelled) else {
        return false;
    };
    let applied = matches!(
        claim.result,
        conversation::SettlementResult::Applied(conversation::AppliedOutcome::Cancelled)
    );
    signal(claim.cancel);
    applied
}

fn fail_execution(
    state: &AppState,
    id: conversation::ExecutionId,
    provider_error: CustomOpenAiError,
    auth_revision: Option<CustomOpenAiAuthRevision>,
) -> Result<ipc::PromptOutcome, ipc::CommandError> {
    let rejection = if matches!(
        provider_error,
        CustomOpenAiError::Unauthorized | CustomOpenAiError::Forbidden
    ) {
        auth_revision.map(|revision| RemoteAuthRejection {
            revision,
            error: provider_error.clone(),
        })
    } else {
        None
    };
    let claim =
        terminal_with_rejection(state, id, conversation::SettlementIntent::Failed, rejection)?;
    signal(claim.cancel);
    match claim.result {
        conversation::SettlementResult::Applied(conversation::AppliedOutcome::Failed) => {
            Err(map_error(provider_error))
        }
        conversation::SettlementResult::IgnoredStale => Ok(ipc::PromptOutcome::Cancelled),
        _ => Err(error(
            ipc::CommandErrorCode::Internal,
            "Provider failure could not be settled",
        )),
    }
}

fn effective_auth(
    inner: &mut Inner,
    observed_rejection_generation: u64,
    observation: CustomOpenAiAuthObservation,
) -> Result<(), CustomOpenAiError> {
    if let Some(rejection) = &inner.remote_auth_rejection {
        if observed_rejection_generation != inner.remote_auth_rejection_generation
            || observation
                .revision
                .is_none_or(|revision| revision == rejection.revision)
        {
            return Err(rejection.error.clone());
        }
        inner.remote_auth_rejection = None;
    }
    observation.status
}

fn snapshot_with_current_auth(state: &AppState) -> Result<ipc::AppSnapshot, ipc::CommandError> {
    let observed_rejection_generation = state
        .inner
        .lock()
        .map_err(|_| {
            error(
                ipc::CommandErrorCode::Internal,
                "Android shell state is unavailable",
            )
        })?
        .remote_auth_rejection_generation;
    let observation = state.provider.observe_auth();
    let mut inner = state.inner.lock().map_err(|_| {
        error(
            ipc::CommandErrorCode::Internal,
            "Android shell state is unavailable",
        )
    })?;
    let auth = effective_auth(&mut inner, observed_rejection_generation, observation);
    Ok(snapshot(&inner, auth))
}

#[tauri::command]
async fn get_app_snapshot(
    state: State<'_, AppState>,
) -> Result<ipc::AppSnapshot, ipc::CommandError> {
    snapshot_with_current_auth(&state)
}

struct Lease {
    state: AppState,
    id: conversation::ExecutionId,
    armed: bool,
}
impl Lease {
    fn disarm(&mut self) {
        self.armed = false;
    }
}
impl Drop for Lease {
    fn drop(&mut self) {
        if self.armed {
            cancel_execution(&self.state, self.id);
        }
    }
}

trait UpdateSink: Send {
    fn send(&mut self, update: ipc::PromptUpdate) -> bool;
}
struct ChannelSink(tauri::ipc::Channel<ipc::PromptUpdate>);
impl UpdateSink for ChannelSink {
    fn send(&mut self, update: ipc::PromptUpdate) -> bool {
        self.0.send(update).is_ok()
    }
}

async fn submit_prompt_with_sink<S: UpdateSink + 'static>(
    request_id: String,
    prompt: String,
    mut sink: S,
    state: AppState,
) -> Result<ipc::PromptOutcome, ipc::CommandError> {
    validate(&request_id, Some(&prompt))?;
    let (admission, mut cancel) = {
        let mut inner = state.inner.lock().map_err(|_| {
            error(
                ipc::CommandErrorCode::Internal,
                "Android shell state is unavailable",
            )
        })?;
        if !inner.foreground {
            return Err(error(ipc::CommandErrorCode::Unavailable, BACKGROUNDED));
        }
        let admission = inner
            .session
            .admit(&request_id, &prompt)
            .map_err(|_| error(ipc::CommandErrorCode::Busy, BUSY))?;
        let (sender, receiver) = tokio::sync::watch::channel(false);
        inner.physical = Some(Physical {
            execution: admission.execution_id,
            cancel: sender,
        });
        (admission, receiver)
    };
    let mut lease = Lease {
        state: state.clone(),
        id: admission.execution_id,
        armed: true,
    };
    let observed_rejection_generation = state
        .inner
        .lock()
        .map_err(|_| {
            error(
                ipc::CommandErrorCode::Internal,
                "Android shell state is unavailable",
            )
        })?
        .remote_auth_rejection_generation;
    let observation = state.provider.observe_auth();
    let auth = {
        let mut inner = state.inner.lock().map_err(|_| {
            error(
                ipc::CommandErrorCode::Internal,
                "Android shell state is unavailable",
            )
        })?;
        effective_auth(&mut inner, observed_rejection_generation, observation)
    };
    if let Err(provider_error) = auth {
        let result = fail_execution(&state, admission.execution_id, provider_error, None);
        lease.disarm();
        return result;
    }
    let history = admission
        .history
        .iter()
        .map(|message| CustomOpenAiMessage {
            role: match message.role {
                assistant::Role::User => voxgolem_platform::custom_openai::CustomOpenAiRole::User,
                assistant::Role::Assistant => {
                    voxgolem_platform::custom_openai::CustomOpenAiRole::Assistant
                }
            },
            content_type: match message.content {
                assistant::Content::Refusal(_) => CustomOpenAiContentType::Refusal,
                _ => CustomOpenAiContentType::OutputText,
            },
            text: match &message.content {
                assistant::Content::Text(value) | assistant::Content::Refusal(value) => {
                    value.clone()
                }
            },
        })
        .collect();
    let model = match admission.policy.instant_model {
        assistant::InstantModel::CustomSolNone => CustomOpenAiModel::SolNone,
        assistant::InstantModel::CustomLunaNone => CustomOpenAiModel::LunaNone,
        assistant::InstantModel::CustomLunaLow => CustomOpenAiModel::LunaLow,
        assistant::InstantModel::CustomSolHigh => CustomOpenAiModel::SolHigh,
        _ => CustomOpenAiModel::SolHigh,
    };
    let (delta_tx, mut delta_rx) = tokio::sync::mpsc::unbounded_channel();
    let request = CustomOpenAiPrompt {
        session_id: admission.execution_id.session_id.0.to_string(),
        prompt: admission.prompt.clone(),
        history,
    };
    let provider_future = state.provider.respond(model, request, delta_tx);
    tokio::pin!(provider_future);
    let attempt = loop {
        tokio::select! {
            biased;
            _ = cancel_changed(&mut cancel) => {
                lease.disarm();
                return Ok(ipc::PromptOutcome::Cancelled);
            }
            delta = delta_rx.recv() => {
                if let Some(delta) = delta {
                    if let Err(failure) = publish_delta(&state, admission.execution_id, &mut sink, delta) {
                        let result = handle_publish_failure(
                            &state,
                            admission.execution_id,
                            failure,
                        );
                        lease.disarm();
                        return result;
                    }
                } else {
                    let result = (&mut provider_future).await;
                    break result;
                }
            }
            result = &mut provider_future => break result,
        }
    };

    while let Ok(delta) = delta_rx.try_recv() {
        if let Err(failure) = publish_delta(&state, admission.execution_id, &mut sink, delta) {
            let result = handle_publish_failure(&state, admission.execution_id, failure);
            lease.disarm();
            return result;
        }
    }

    let response = match attempt.result {
        Ok(response) => response,
        Err(provider_error) => {
            let result = fail_execution(
                &state,
                admission.execution_id,
                provider_error,
                attempt.auth_revision,
            );
            lease.disarm();
            return result;
        }
    };
    let content = if response.content_type == CustomOpenAiContentType::Refusal {
        assistant::Content::Refusal(response.text)
    } else {
        assistant::Content::Text(response.text)
    };
    let claim = complete_execution(&state, admission.execution_id, content)?;
    lease.disarm();
    match claim {
        CompletionClaim::Outcome(outcome, sender) => {
            signal(sender);
            Ok(outcome)
        }
        CompletionClaim::Cancelled => Ok(ipc::PromptOutcome::Cancelled),
        CompletionClaim::Failed(command_error, sender) => {
            signal(sender);
            Err(command_error)
        }
    }
}

#[derive(Debug, Eq, PartialEq)]
enum PublishFailure {
    ChannelClosed,
    TooLarge,
    ExecutionEnded,
}

fn publish_delta(
    state: &AppState,
    id: conversation::ExecutionId,
    sink: &mut impl UpdateSink,
    delta: String,
) -> Result<(), PublishFailure> {
    let Ok(text) = ipc::BoundedText::new(delta) else {
        return Err(PublishFailure::TooLarge);
    };
    let _publication = state
        .publication
        .lock()
        .map_err(|_| PublishFailure::ExecutionEnded)?;
    let active = state
        .inner
        .lock()
        .map(|inner| {
            inner
                .physical
                .as_ref()
                .is_some_and(|physical| physical.execution == id)
        })
        .unwrap_or(false);
    if !active {
        return Err(PublishFailure::ExecutionEnded);
    }
    if sink.send(ipc::PromptUpdate::Delta { text }) {
        Ok(())
    } else {
        Err(PublishFailure::ChannelClosed)
    }
}

fn handle_publish_failure(
    state: &AppState,
    id: conversation::ExecutionId,
    failure: PublishFailure,
) -> Result<ipc::PromptOutcome, ipc::CommandError> {
    match failure {
        PublishFailure::ChannelClosed => {
            cancel_execution(state, id);
            Ok(ipc::PromptOutcome::Cancelled)
        }
        PublishFailure::TooLarge => fail_execution(state, id, CustomOpenAiError::TooLarge, None),
        PublishFailure::ExecutionEnded => Ok(ipc::PromptOutcome::Cancelled),
    }
}

enum CompletionClaim {
    Outcome(ipc::PromptOutcome, Option<tokio::sync::watch::Sender<bool>>),
    Cancelled,
    Failed(ipc::CommandError, Option<tokio::sync::watch::Sender<bool>>),
}

fn complete_execution(
    state: &AppState,
    id: conversation::ExecutionId,
    content: assistant::Content,
) -> Result<CompletionClaim, ipc::CommandError> {
    let _publication = state.publication.lock().map_err(|_| {
        error(
            ipc::CommandErrorCode::Internal,
            "Android publication state is unavailable",
        )
    })?;
    let mut inner = state.inner.lock().map_err(|_| {
        error(
            ipc::CommandErrorCode::Internal,
            "Android shell state is unavailable",
        )
    })?;
    if inner
        .physical
        .as_ref()
        .is_none_or(|physical| physical.execution != id)
    {
        return Ok(CompletionClaim::Cancelled);
    }
    let update = inner.session.update(
        id,
        assistant::Stage::Instant,
        assistant::StageResult::Instant(assistant::InstantOutcome::Complete(content)),
    );
    if !matches!(update, conversation::UpdateResult::Resolved { .. }) {
        let _ = inner
            .session
            .settle(id, conversation::SettlementIntent::Failed);
        let sender = inner.physical.take().map(|physical| physical.cancel);
        let message = if matches!(update, conversation::UpdateResult::RejectedOversized) {
            "Custom provider response was too large"
        } else {
            PROVIDER_RESPONSE_INVALID
        };
        return Ok(CompletionClaim::Failed(
            error(ipc::CommandErrorCode::Unavailable, message),
            sender,
        ));
    }
    let result = inner
        .session
        .settle(id, conversation::SettlementIntent::Success);
    let sender = inner.physical.take().map(|physical| physical.cancel);
    match result {
        conversation::SettlementResult::Applied(value) => {
            let outcome = ipc::PromptOutcome::try_from(value)
                .map_err(|_| error(ipc::CommandErrorCode::Internal, PROVIDER_RESPONSE_INVALID))?;
            Ok(CompletionClaim::Outcome(outcome, sender))
        }
        conversation::SettlementResult::IgnoredStale => Ok(CompletionClaim::Cancelled),
        conversation::SettlementResult::Rejected(_) => {
            let _ = inner
                .session
                .settle(id, conversation::SettlementIntent::Failed);
            Ok(CompletionClaim::Failed(
                error(
                    ipc::CommandErrorCode::Unavailable,
                    "Custom provider response was too large",
                ),
                sender,
            ))
        }
    }
}

async fn cancel_changed(receiver: &mut tokio::sync::watch::Receiver<bool>) {
    let _ = receiver.wait_for(|cancelled| *cancelled).await;
}

fn cancel_request(state: &AppState, request_id: &str) -> Result<bool, ipc::CommandError> {
    let id = {
        let inner = state.inner.lock().map_err(|_| {
            error(
                ipc::CommandErrorCode::Internal,
                "Android shell state is unavailable",
            )
        })?;
        inner
            .session
            .snapshot()
            .active
            .filter(|active| active.request_id.0 == request_id)
            .map(|active| active.execution_id)
    };
    Ok(id.is_some_and(|id| cancel_execution(state, id)))
}

fn reset(state: &AppState) -> Result<(), ipc::CommandError> {
    let _publication = state.publication.lock().map_err(|_| {
        error(
            ipc::CommandErrorCode::Internal,
            "Android publication state is unavailable",
        )
    })?;
    let sender = {
        let mut inner = state.inner.lock().map_err(|_| {
            error(
                ipc::CommandErrorCode::Internal,
                "Android shell state is unavailable",
            )
        })?;
        let invalidated = inner.session.reset().invalidated;
        invalidated.and_then(|id| {
            if inner
                .physical
                .as_ref()
                .is_some_and(|physical| physical.execution == id)
            {
                inner.physical.take().map(|physical| physical.cancel)
            } else {
                None
            }
        })
    };
    drop(_publication);
    signal(sender);
    Ok(())
}

#[tauri::command]
async fn submit_prompt(
    request_id: String,
    prompt: String,
    updates: tauri::ipc::Channel<ipc::PromptUpdate>,
    state: State<'_, AppState>,
) -> Result<ipc::PromptOutcome, ipc::CommandError> {
    submit_prompt_with_sink(request_id, prompt, ChannelSink(updates), (*state).clone()).await
}

#[tauri::command]
async fn cancel_prompt(
    request_id: String,
    state: State<'_, AppState>,
) -> Result<bool, ipc::CommandError> {
    validate(&request_id, None)?;
    cancel_request(&state, &request_id)
}
#[tauri::command]
async fn reset_session(state: State<'_, AppState>) -> Result<ipc::AppSnapshot, ipc::CommandError> {
    reset(&state)?;
    snapshot_with_current_auth(&state)
}
#[tauri::command]
async fn set_preferences(
    preferences: ipc::ValidatedPreferences,
    state: State<'_, AppState>,
) -> Result<ipc::ValidatedPreferences, ipc::CommandError> {
    set_preferences_inner(preferences, &state)
}

fn set_preferences_inner(
    preferences: ipc::ValidatedPreferences,
    state: &AppState,
) -> Result<ipc::ValidatedPreferences, ipc::CommandError> {
    if !matches!(
        preferences.instant,
        ipc::InstantModelChoice::CustomSolHigh
            | ipc::InstantModelChoice::CustomSolNone
            | ipc::InstantModelChoice::CustomLunaNone
            | ipc::InstantModelChoice::CustomLunaLow
    ) || !matches!(
        preferences.deep,
        ipc::AgentModelChoice::CustomSolHigh | ipc::AgentModelChoice::CustomLunaLow
    ) || !matches!(
        preferences.review,
        ipc::AgentModelChoice::CustomSolHigh | ipc::AgentModelChoice::CustomLunaLow
    ) || preferences.deep_enabled
        || preferences.review_enabled
        || preferences.prefetch
        || preferences.completion
        || preferences.tts_enabled
        || preferences.response_profile != ipc::ResponseProfile::Fast
    {
        return Err(error(
            ipc::CommandErrorCode::Unavailable,
            "This Android capability is unavailable",
        ));
    }
    let mut inner = state.inner.lock().map_err(|_| {
        error(
            ipc::CommandErrorCode::Internal,
            "Android shell state is unavailable",
        )
    })?;
    inner
        .session
        .replace_policy(policy(&preferences))
        .map_err(|_| error(ipc::CommandErrorCode::Busy, BUSY))?;
    inner.preferences = preferences.clone();
    Ok(preferences)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let builder = tauri::Builder::default()
        .plugin(tauri_plugin_capture::init())
        .setup(|app| {
            let path = prepare_auth_path(app.path().app_data_dir()?)?;
            let provider = Arc::new(CustomProvider { auth_path: path });
            let preferences = default_preferences();
            app.manage(app_state(preferences, provider));
            eprintln!("VOXGOLEM_ANDROID_SHELL_STARTED");
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_app_snapshot,
            submit_prompt,
            cancel_prompt,
            reset_session,
            set_preferences
        ])
        .build(tauri::generate_context!())
        .expect("error while building Android shell");
    builder.run(move |app, event| match event {
        tauri::RunEvent::Exit => cancel_active(&app.state::<AppState>()),
        #[cfg(mobile)]
        tauri::RunEvent::WindowEvent {
            event: tauri::WindowEvent::Suspended,
            ..
        } => set_foreground(&app.state::<AppState>(), false),
        #[cfg(mobile)]
        tauri::RunEvent::WindowEvent {
            event: tauri::WindowEvent::Resumed,
            ..
        } => set_foreground(&app.state::<AppState>(), true),
        _ => {}
    });
}

fn cancel_active(state: &AppState) {
    let id = state
        .inner
        .lock()
        .ok()
        .and_then(|inner| inner.session.active_execution());
    if let Some(id) = id {
        cancel_execution(state, id);
    }
}

#[cfg(any(mobile, test))]
fn set_foreground(state: &AppState, foreground: bool) {
    let Ok(_publication) = state.publication.lock() else {
        return;
    };
    let sender = {
        let Ok(mut inner) = state.inner.lock() else {
            return;
        };
        inner.foreground = foreground;
        if foreground {
            None
        } else {
            let id = inner.session.active_execution();
            id.and_then(|id| {
                let matches = inner
                    .physical
                    .as_ref()
                    .is_some_and(|physical| physical.execution == id);
                if !matches {
                    return None;
                }
                let _ = inner
                    .session
                    .settle(id, conversation::SettlementIntent::Cancelled);
                inner.physical.take().map(|physical| physical.cancel)
            })
        }
    };
    drop(_publication);
    signal(sender);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::sync::oneshot;

    static AUTH_DIR_ID: AtomicUsize = AtomicUsize::new(0);

    type PendingState = (
        AppState,
        oneshot::Receiver<()>,
        oneshot::Sender<Result<CustomOpenAiResponse, CustomOpenAiError>>,
        Arc<Mutex<Vec<CustomOpenAiModel>>>,
    );

    struct FakeProvider {
        result: Mutex<Result<CustomOpenAiResponse, CustomOpenAiError>>,
        models: Arc<Mutex<Vec<CustomOpenAiModel>>>,
        requests: Mutex<Vec<CustomOpenAiPrompt>>,
    }
    impl Provider for FakeProvider {
        fn check_auth(&self) -> Result<(), CustomOpenAiError> {
            Ok(())
        }
        fn respond(
            &self,
            model: CustomOpenAiModel,
            request: CustomOpenAiPrompt,
            deltas: DeltaSender,
        ) -> ProviderFuture {
            self.models.lock().unwrap().push(model);
            self.requests.lock().unwrap().push(request);
            let result = self.result.lock().unwrap().clone();
            let _ = deltas.send("delta".into());
            Box::pin(async move {
                ProviderAttempt {
                    result,
                    auth_revision: None,
                }
            })
        }
    }

    struct PendingProvider {
        started: Mutex<Option<oneshot::Sender<()>>>,
        finish: Mutex<Option<oneshot::Receiver<Result<CustomOpenAiResponse, CustomOpenAiError>>>>,
        models: Arc<Mutex<Vec<CustomOpenAiModel>>>,
    }
    impl Provider for PendingProvider {
        fn check_auth(&self) -> Result<(), CustomOpenAiError> {
            Ok(())
        }
        fn respond(
            &self,
            model: CustomOpenAiModel,
            _request: CustomOpenAiPrompt,
            deltas: DeltaSender,
        ) -> ProviderFuture {
            self.models.lock().unwrap().push(model);
            self.started.lock().unwrap().take().unwrap().send(()).ok();
            let finish = self.finish.lock().unwrap().take().unwrap();
            Box::pin(async move {
                let result = finish.await.expect("test controls provider completion");
                let _ = deltas.send("delayed delta".into());
                ProviderAttempt {
                    result,
                    auth_revision: None,
                }
            })
        }
    }

    struct PanicProvider;
    impl Provider for PanicProvider {
        fn check_auth(&self) -> Result<(), CustomOpenAiError> {
            Ok(())
        }
        fn respond(
            &self,
            _model: CustomOpenAiModel,
            _request: CustomOpenAiPrompt,
            _deltas: DeltaSender,
        ) -> ProviderFuture {
            Box::pin(async move { panic!("provider panic") })
        }
    }

    struct AuthErrorProvider(CustomOpenAiError);
    impl Provider for AuthErrorProvider {
        fn check_auth(&self) -> Result<(), CustomOpenAiError> {
            Err(self.0.clone())
        }
        fn respond(
            &self,
            _model: CustomOpenAiModel,
            _request: CustomOpenAiPrompt,
            _deltas: DeltaSender,
        ) -> ProviderFuture {
            panic!("auth failure must prevent provider dispatch")
        }
    }

    struct OversizedDeltaProvider;
    impl Provider for OversizedDeltaProvider {
        fn check_auth(&self) -> Result<(), CustomOpenAiError> {
            Ok(())
        }
        fn respond(
            &self,
            _model: CustomOpenAiModel,
            _request: CustomOpenAiPrompt,
            deltas: DeltaSender,
        ) -> ProviderFuture {
            let _ = deltas.send("x".repeat(conversation::MAX_UPDATE_BYTES + 1));
            Box::pin(async move {
                ProviderAttempt {
                    result: Ok(response(CustomOpenAiContentType::OutputText)),
                    auth_revision: None,
                }
            })
        }
    }
    struct RevisionRaceProvider {
        sampled: CustomOpenAiAuthRevision,
        used: CustomOpenAiAuthRevision,
        revision_reads: AtomicUsize,
    }
    impl Provider for RevisionRaceProvider {
        fn check_auth(&self) -> Result<(), CustomOpenAiError> {
            Ok(())
        }
        fn auth_revision(&self) -> Option<CustomOpenAiAuthRevision> {
            if self.revision_reads.fetch_add(1, Ordering::Relaxed) == 0 {
                Some(self.sampled)
            } else {
                Some(self.used)
            }
        }
        fn respond(
            &self,
            _model: CustomOpenAiModel,
            _request: CustomOpenAiPrompt,
            _deltas: DeltaSender,
        ) -> ProviderFuture {
            let used = self.used;
            Box::pin(async move {
                ProviderAttempt {
                    result: Err(CustomOpenAiError::Unauthorized),
                    auth_revision: Some(used),
                }
            })
        }
    }
    struct Sink {
        count: Arc<AtomicUsize>,
        closed: bool,
    }
    impl UpdateSink for Sink {
        fn send(&mut self, _update: ipc::PromptUpdate) -> bool {
            self.count.fetch_add(1, Ordering::Relaxed);
            !self.closed
        }
    }
    fn response(content_type: CustomOpenAiContentType) -> CustomOpenAiResponse {
        CustomOpenAiResponse {
            text: "answer".into(),
            content_type,
            model: None,
            service_tier: None,
            usage: None,
            timings: Default::default(),
        }
    }
    fn state(
        result: Result<CustomOpenAiResponse, CustomOpenAiError>,
    ) -> (AppState, Arc<FakeProvider>) {
        let models = Arc::new(Mutex::new(Vec::new()));
        let preferences = default_preferences();
        let provider = Arc::new(FakeProvider {
            result: Mutex::new(result),
            models,
            requests: Mutex::new(Vec::new()),
        });
        (app_state(preferences, provider.clone()), provider)
    }

    fn pending_state() -> PendingState {
        pending_state_with_preferences(default_preferences())
    }

    fn pending_state_with_preferences(preferences: ipc::ValidatedPreferences) -> PendingState {
        let (started_tx, started_rx) = oneshot::channel();
        let (finish_tx, finish_rx) = oneshot::channel();
        let models = Arc::new(Mutex::new(Vec::new()));
        let state = app_state(
            preferences,
            Arc::new(PendingProvider {
                started: Mutex::new(Some(started_tx)),
                finish: Mutex::new(Some(finish_rx)),
                models: models.clone(),
            }),
        );
        (state, started_rx, finish_tx, models)
    }

    fn sink(closed: bool) -> (Sink, Arc<AtomicUsize>) {
        let count = Arc::new(AtomicUsize::new(0));
        (
            Sink {
                count: count.clone(),
                closed,
            },
            count,
        )
    }
    fn test_auth_revision(value: u64) -> CustomOpenAiAuthRevision {
        let id = AUTH_DIR_ID.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "vox-golem-android-auth-revision-{}-{id}",
            std::process::id()
        ));
        std::fs::write(&path, vec![b'x'; value as usize]).unwrap();
        let client = CustomOpenAiClient::new(CustomOpenAiConfig {
            auth_path: path.clone(),
            ..Default::default()
        })
        .unwrap();
        let revision = client.auth_revision().unwrap();
        std::fs::remove_file(path).unwrap();
        revision
    }

    fn admit_physical(state: &AppState, request_id: &str) -> conversation::ExecutionId {
        let mut inner = state.inner.lock().unwrap();
        let admission = inner.session.admit(request_id, "prompt").unwrap();
        let (cancel, _) = tokio::sync::watch::channel(false);
        inner.physical = Some(Physical {
            execution: admission.execution_id,
            cancel,
        });
        admission.execution_id
    }

    #[test]
    fn auth_path_is_fixed() {
        assert_eq!(
            auth_path("/data/app"),
            PathBuf::from("/data/app/files/codex-auth.json")
        );
    }
    #[test]
    fn prepare_auth_path_creates_fresh_private_files_directory() {
        let id = AUTH_DIR_ID.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "vox-golem-android-auth-dir-{}-{id}",
            std::process::id()
        ));
        std::fs::create_dir(&root).unwrap();
        let path = prepare_auth_path(&root).unwrap();
        assert_eq!(path, root.join("files/codex-auth.json"));
        let metadata = std::fs::symlink_metadata(root.join("files")).unwrap();
        assert!(metadata.is_dir() && !metadata.file_type().is_symlink());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(metadata.permissions().mode() & 0o777, 0o700);
        }
        std::fs::remove_dir_all(root).unwrap();
    }
    #[cfg(unix)]
    #[test]
    fn prepare_auth_path_rejects_symlinked_files_directory() {
        let id = AUTH_DIR_ID.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "vox-golem-android-auth-symlink-{}-{id}",
            std::process::id(),
        ));
        let target = root.with_extension("target");
        std::fs::create_dir(&root).unwrap();
        std::fs::create_dir(&target).unwrap();
        std::os::unix::fs::symlink(&target, root.join("files")).unwrap();
        assert_eq!(
            prepare_auth_path(&root).unwrap_err().kind(),
            std::io::ErrorKind::InvalidData
        );
        std::fs::remove_dir_all(root).unwrap();
        std::fs::remove_dir_all(target).unwrap();
    }
    #[test]
    fn defaults_are_custom_and_disabled() {
        let p = default_preferences();
        assert_eq!(p.instant, ipc::InstantModelChoice::CustomSolHigh);
        assert!(!p.deep_enabled && !p.review_enabled && !p.tts_enabled);
    }
    #[test]
    fn canonical_permissions_are_exact() {
        let value: serde_json::Value =
            serde_json::from_str(include_str!("../capabilities/main.json")).unwrap();
        assert_eq!(
            value["permissions"],
            serde_json::json!([
                "allow-get-app-snapshot",
                "allow-submit-prompt",
                "allow-cancel-prompt",
                "allow-reset-session",
                "allow-set-preferences",
                "capture:default"
            ])
        );
        assert!(!value.to_string().contains("event"));
        assert!(!value.to_string().contains("legacy"));
    }
    #[test]
    fn snapshot_reports_valid_missing_and_expired_auth() {
        let (state, _) = state(Ok(response(CustomOpenAiContentType::OutputText)));
        for (auth, expected, reason) in [
            (Ok(()), ipc::CapabilityState::Available, "Ready"),
            (
                Err(CustomOpenAiError::AuthUnavailable),
                ipc::CapabilityState::Unavailable,
                AUTH_UNAVAILABLE,
            ),
            (
                Err(CustomOpenAiError::AuthExpired),
                ipc::CapabilityState::Unavailable,
                AUTH_EXPIRED,
            ),
        ] {
            let inner = state.inner.lock().unwrap();
            let value = snapshot(&inner, auth);
            let custom = value
                .capabilities
                .iter()
                .find(|capability| capability.id.as_str() == "custom_provider")
                .unwrap();
            assert_eq!(custom.state, expected);
            assert_eq!(custom.reason.as_str(), reason);
            assert_eq!(value.model_options.len(), 4);
            assert!(value
                .model_options
                .iter()
                .all(|option| option.available == (expected == ipc::CapabilityState::Available)));
        }
    }
    #[tokio::test]
    async fn header_invalid_auth_is_unavailable_before_dispatch() {
        let id = AUTH_DIR_ID.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "vox-golem-android-invalid-auth-{}-{id}",
            std::process::id()
        ));
        std::fs::create_dir(&root).unwrap();
        let path = prepare_auth_path(&root).unwrap();
        let expiry = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
            + 3600;
        std::fs::write(
            &path,
            format!(
                r#"{{"openai":{{"access":"bad\nvalue","accountId":"account","expires":{expiry}}}}}"#
            ),
        )
        .unwrap();
        let provider = Arc::new(CustomProvider { auth_path: path });
        assert_eq!(
            provider.check_auth(),
            Err(CustomOpenAiError::AuthUnavailable)
        );
        let preferences = default_preferences();
        let state = app_state(preferences, provider);
        let unavailable = snapshot(
            &state.inner.lock().unwrap(),
            Err(CustomOpenAiError::AuthUnavailable),
        );
        assert_eq!(
            unavailable.capabilities[0].state,
            ipc::CapabilityState::Unavailable
        );
        let (updates, _) = sink(false);
        assert_eq!(
            submit_prompt_with_sink("id".into(), "prompt".into(), updates, state.clone())
                .await
                .unwrap_err()
                .message(),
            AUTH_UNAVAILABLE
        );
        assert!(!state.inner.lock().unwrap().session.has_active_execution());
        std::fs::remove_dir_all(root).unwrap();
    }
    #[tokio::test]
    async fn oversized_auth_disables_only_provider_admission() {
        let id = AUTH_DIR_ID.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "vox-golem-android-oversized-auth-{}-{id}",
            std::process::id()
        ));
        std::fs::create_dir(&root).unwrap();
        let path = prepare_auth_path(&root).unwrap();
        std::fs::write(
            &path,
            vec![
                b'x';
                (voxgolem_platform::custom_openai::MAX_CUSTOM_OPENAI_AUTH_BYTES + 1) as usize
            ],
        )
        .unwrap();
        let preferences = default_preferences();
        let state = app_state(preferences, Arc::new(CustomProvider { auth_path: path }));
        let snapshot = snapshot_with_current_auth(&state).unwrap();
        assert_eq!(
            snapshot.capabilities[0].state,
            ipc::CapabilityState::Unavailable
        );
        let (updates, _) = sink(false);
        assert_eq!(
            submit_prompt_with_sink("id".into(), "prompt".into(), updates, state.clone())
                .await
                .unwrap_err()
                .message(),
            AUTH_UNAVAILABLE
        );
        assert!(!state.inner.lock().unwrap().session.has_active_execution());
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn validation_rejects_without_state_change() {
        assert!(validate("", Some("x")).is_err());
        assert!(validate("id", Some(" ")).is_err());
    }
    #[test]
    fn policy_keeps_deep_review_disabled() {
        let mut p = default_preferences();
        p.instant = ipc::InstantModelChoice::CustomLunaLow;
        let value = policy(&p);
        assert_eq!(value.instant_model, assistant::InstantModel::CustomLunaLow);
        assert!(!value.deep_enabled && !value.review_enabled);
    }
    #[test]
    fn process_local_preferences_validate_then_publish_atomically() {
        let (state, _) = state(Ok(response(CustomOpenAiContentType::OutputText)));
        let mut next = default_preferences();
        next.instant = ipc::InstantModelChoice::CustomLunaLow;
        next.ui_theme = ipc::UiTheme::Light;
        assert_eq!(set_preferences_inner(next.clone(), &state).unwrap(), next);
        assert_eq!(state.inner.lock().unwrap().preferences, next);

        let before = snapshot(&state.inner.lock().unwrap(), Ok(()));
        let mut unavailable = before.preferences.clone();
        unavailable.instant = ipc::InstantModelChoice::LocalFast;
        assert_eq!(
            set_preferences_inner(unavailable, &state)
                .unwrap_err()
                .code(),
            ipc::CommandErrorCode::Unavailable
        );
        assert_eq!(snapshot(&state.inner.lock().unwrap(), Ok(())), before);

        for unsupported_agent in [
            ipc::AgentModelChoice::OpenCodeSolHigh,
            ipc::AgentModelChoice::OpenCodeLunaLow,
        ] {
            let mut unsupported = before.preferences.clone();
            unsupported.deep = unsupported_agent;
            assert_eq!(
                set_preferences_inner(unsupported, &state)
                    .unwrap_err()
                    .code(),
                ipc::CommandErrorCode::Unavailable
            );
            let mut unsupported = before.preferences.clone();
            unsupported.review = unsupported_agent;
            assert_eq!(
                set_preferences_inner(unsupported, &state)
                    .unwrap_err()
                    .code(),
                ipc::CommandErrorCode::Unavailable
            );
            assert_eq!(snapshot(&state.inner.lock().unwrap(), Ok(())), before);
        }
    }
    #[test]
    fn fixed_error_mapping_is_display_safe() {
        for error_value in [
            CustomOpenAiError::Unauthorized,
            CustomOpenAiError::RateLimited,
            CustomOpenAiError::Timeout,
            CustomOpenAiError::TooLarge,
        ] {
            assert!(map_error(error_value).message().len() <= ipc::CommandError::MAX_MESSAGE_BYTES);
        }
    }
    #[tokio::test]
    async fn completion_streams_and_commits_canonical_turn() {
        let (state, _) = state(Ok(response(CustomOpenAiContentType::OutputText)));
        let count = Arc::new(AtomicUsize::new(0));
        let outcome = submit_prompt_with_sink(
            "id".into(),
            "prompt".into(),
            Sink {
                count: count.clone(),
                closed: false,
            },
            state.clone(),
        )
        .await
        .unwrap();
        assert!(matches!(outcome, ipc::PromptOutcome::Completed { .. }));
        assert_eq!(count.load(Ordering::Relaxed), 1);
        assert_eq!(state.inner.lock().unwrap().session.turns().len(), 1);
    }
    #[tokio::test]
    async fn later_prompts_receive_canonical_committed_history() {
        let (state, provider) = state(Ok(response(CustomOpenAiContentType::OutputText)));
        for request_id in ["first", "second"] {
            let (updates, _) = sink(false);
            assert!(matches!(
                submit_prompt_with_sink(
                    request_id.into(),
                    "prompt".into(),
                    updates,
                    state.clone(),
                )
                .await
                .unwrap(),
                ipc::PromptOutcome::Completed { .. }
            ));
        }
        let requests = provider.requests.lock().unwrap();
        assert!(requests[0].history.is_empty());
        assert_eq!(requests[1].history.len(), 2);
        assert_eq!(requests[1].history[0].text, "prompt");
        assert_eq!(requests[1].history[1].text, "answer");
    }
    #[tokio::test]
    async fn refusal_commits_refusal_outcome() {
        let (state, _) = state(Ok(response(CustomOpenAiContentType::Refusal)));
        let outcome = submit_prompt_with_sink(
            "id".into(),
            "prompt".into(),
            Sink {
                count: Arc::new(AtomicUsize::new(0)),
                closed: false,
            },
            state,
        )
        .await
        .unwrap();
        assert!(matches!(outcome, ipc::PromptOutcome::Refused { .. }));
    }
    #[tokio::test]
    async fn provider_error_releases_admission() {
        let (state, provider) = state(Err(CustomOpenAiError::Timeout));
        assert!(submit_prompt_with_sink(
            "id".into(),
            "prompt".into(),
            Sink {
                count: Arc::new(AtomicUsize::new(0)),
                closed: false
            },
            state.clone()
        )
        .await
        .is_err());
        assert!(!state.inner.lock().unwrap().session.has_active_execution());
        *provider.result.lock().unwrap() = Ok(response(CustomOpenAiContentType::OutputText));
        let (updates, _) = sink(false);
        assert!(matches!(
            submit_prompt_with_sink("next".into(), "prompt".into(), updates, state)
                .await
                .unwrap(),
            ipc::PromptOutcome::Completed { .. }
        ));
    }
    #[test]
    fn remote_auth_rejection_remains_until_credentials_change() {
        let (state, _) = state(Ok(response(CustomOpenAiContentType::OutputText)));
        let revision = test_auth_revision(1);
        let id = admit_physical(&state, "rejected");
        let error = fail_execution(&state, id, CustomOpenAiError::Unauthorized, Some(revision))
            .unwrap_err();
        assert_eq!(error.message(), PROVIDER_UNAVAILABLE);

        let mut inner = state.inner.lock().unwrap();
        let generation = inner.remote_auth_rejection_generation;
        assert_eq!(
            effective_auth(
                &mut inner,
                generation,
                CustomOpenAiAuthObservation {
                    revision: None,
                    status: Err(CustomOpenAiError::AuthUnavailable),
                },
            ),
            Err(CustomOpenAiError::Unauthorized)
        );
        assert_eq!(
            effective_auth(
                &mut inner,
                generation,
                CustomOpenAiAuthObservation {
                    revision: Some(revision),
                    status: Ok(()),
                },
            ),
            Err(CustomOpenAiError::Unauthorized)
        );
        assert_eq!(
            effective_auth(
                &mut inner,
                generation,
                CustomOpenAiAuthObservation {
                    revision: Some(test_auth_revision(2)),
                    status: Ok(()),
                },
            ),
            Ok(())
        );
        assert!(inner.remote_auth_rejection.is_none());
    }
    #[test]
    fn stale_remote_auth_failure_does_not_disable_replacement_request() {
        let (state, _) = state(Ok(response(CustomOpenAiContentType::OutputText)));
        let revision = test_auth_revision(1);
        let id = admit_physical(&state, "cancelled");
        assert!(cancel_execution(&state, id));
        assert!(matches!(
            fail_execution(&state, id, CustomOpenAiError::Forbidden, Some(revision),).unwrap(),
            ipc::PromptOutcome::Cancelled
        ));
        let mut inner = state.inner.lock().unwrap();
        let generation = inner.remote_auth_rejection_generation;
        assert_eq!(
            effective_auth(
                &mut inner,
                generation,
                CustomOpenAiAuthObservation {
                    revision: Some(revision),
                    status: Ok(()),
                },
            ),
            Ok(())
        );
    }
    #[test]
    fn older_auth_observation_cannot_clear_a_newer_rejection() {
        let (state, _) = state(Ok(response(CustomOpenAiContentType::OutputText)));
        let old_revision = test_auth_revision(1);
        let rejected_revision = test_auth_revision(2);
        let observed_generation = state.inner.lock().unwrap().remote_auth_rejection_generation;
        let id = admit_physical(&state, "rejected");
        assert!(fail_execution(
            &state,
            id,
            CustomOpenAiError::Unauthorized,
            Some(rejected_revision),
        )
        .is_err());

        let mut inner = state.inner.lock().unwrap();
        assert_eq!(
            effective_auth(
                &mut inner,
                observed_generation,
                CustomOpenAiAuthObservation {
                    revision: Some(old_revision),
                    status: Ok(()),
                },
            ),
            Err(CustomOpenAiError::Unauthorized)
        );
        assert_eq!(
            inner
                .remote_auth_rejection
                .as_ref()
                .map(|rejection| rejection.revision),
            Some(rejected_revision)
        );
    }
    #[tokio::test]
    async fn rejection_tracks_the_dispatched_credential_and_survives_reset() {
        let sampled = test_auth_revision(1);
        let used = test_auth_revision(2);
        let preferences = default_preferences();
        let state = app_state(
            preferences,
            Arc::new(RevisionRaceProvider {
                sampled,
                used,
                revision_reads: AtomicUsize::new(0),
            }),
        );
        let (updates, _) = sink(false);
        assert_eq!(
            submit_prompt_with_sink("id".into(), "prompt".into(), updates, state.clone())
                .await
                .unwrap_err()
                .message(),
            PROVIDER_UNAVAILABLE
        );
        reset(&state).unwrap();
        let snapshot = snapshot_with_current_auth(&state).unwrap();
        let provider = snapshot
            .capabilities
            .iter()
            .find(|capability| capability.id.as_str() == "custom_provider")
            .unwrap();
        assert_eq!(provider.state, ipc::CapabilityState::Unavailable);
        assert_eq!(provider.reason.as_str(), AUTH_UNAVAILABLE);
        assert_eq!(
            state
                .inner
                .lock()
                .unwrap()
                .remote_auth_rejection
                .as_ref()
                .map(|rejection| rejection.revision),
            Some(used)
        );
    }
    #[tokio::test]
    async fn invalid_submit_does_not_admit_or_call_provider() {
        let (state, provider) = state(Ok(response(CustomOpenAiContentType::OutputText)));
        let before = snapshot(&state.inner.lock().unwrap(), Ok(()));
        let (updates, _) = sink(false);
        let error = submit_prompt_with_sink("".into(), "prompt".into(), updates, state.clone())
            .await
            .unwrap_err();
        assert_eq!(error.code(), ipc::CommandErrorCode::InvalidInput);
        assert_eq!(snapshot(&state.inner.lock().unwrap(), Ok(())), before);
        assert!(provider.models.lock().unwrap().is_empty());
    }
    #[tokio::test]
    async fn auth_failure_releases_admission_without_dispatch() {
        let preferences = default_preferences();
        let state = app_state(
            preferences,
            Arc::new(AuthErrorProvider(CustomOpenAiError::AuthExpired)),
        );
        let (updates, _) = sink(false);
        let error = submit_prompt_with_sink("id".into(), "prompt".into(), updates, state.clone())
            .await
            .unwrap_err();
        assert_eq!(error.message(), AUTH_EXPIRED);
        assert!(!state.inner.lock().unwrap().session.has_active_execution());
    }
    #[tokio::test]
    async fn matching_cancel_wins_and_stale_cancel_does_nothing() {
        let (state, started, finish, _) = pending_state();
        let (updates, _) = sink(false);
        let submit_state = state.clone();
        let submit = tokio::spawn(submit_prompt_with_sink(
            "active".into(),
            "prompt".into(),
            updates,
            submit_state,
        ));
        started.await.unwrap();
        assert!(!cancel_request(&state, "stale").unwrap());
        assert!(state.inner.lock().unwrap().session.has_active_execution());
        assert!(cancel_request(&state, "active").unwrap());
        assert!(matches!(
            submit.await.unwrap().unwrap(),
            ipc::PromptOutcome::Cancelled
        ));
        assert!(finish
            .send(Ok(response(CustomOpenAiContentType::OutputText)))
            .is_err());
        let inner = state.inner.lock().unwrap();
        assert!(!inner.session.has_active_execution());
        assert!(inner.session.turns().is_empty());
    }
    #[tokio::test]
    async fn reset_cancels_pending_and_suppresses_late_completion() {
        let (state, started, finish, _) = pending_state();
        let (updates, _) = sink(false);
        let submit_state = state.clone();
        let submit = tokio::spawn(submit_prompt_with_sink(
            "active".into(),
            "prompt".into(),
            updates,
            submit_state,
        ));
        started.await.unwrap();
        reset(&state).unwrap();
        assert!(matches!(
            submit.await.unwrap().unwrap(),
            ipc::PromptOutcome::Cancelled
        ));
        assert!(finish
            .send(Ok(response(CustomOpenAiContentType::OutputText)))
            .is_err());
        let inner = state.inner.lock().unwrap();
        assert_eq!(inner.session.session_id(), conversation::SessionId(2));
        assert!(inner.session.turns().is_empty());
        assert!(!inner.session.has_active_execution());
    }
    #[tokio::test]
    async fn background_lifecycle_cancels_pending_provider() {
        let (state, started, finish, _) = pending_state();
        let (updates, _) = sink(false);
        let submit_state = state.clone();
        let submit = tokio::spawn(submit_prompt_with_sink(
            "active".into(),
            "prompt".into(),
            updates,
            submit_state,
        ));
        started.await.unwrap();
        cancel_active(&state);
        assert!(matches!(
            submit.await.unwrap().unwrap(),
            ipc::PromptOutcome::Cancelled
        ));
        assert!(finish
            .send(Ok(response(CustomOpenAiContentType::OutputText)))
            .is_err());
        assert!(!state.inner.lock().unwrap().session.has_active_execution());
    }
    #[tokio::test]
    async fn suspension_rejects_admission_until_resume() {
        let (state, provider) = state(Ok(response(CustomOpenAiContentType::OutputText)));
        set_foreground(&state, false);
        let (updates, _) = sink(false);
        let error = submit_prompt_with_sink(
            "backgrounded".into(),
            "prompt".into(),
            updates,
            state.clone(),
        )
        .await
        .unwrap_err();
        assert_eq!(error.code(), ipc::CommandErrorCode::Unavailable);
        assert_eq!(error.message(), BACKGROUNDED);
        assert!(provider.models.lock().unwrap().is_empty());

        set_foreground(&state, true);
        let (updates, _) = sink(false);
        assert!(matches!(
            submit_prompt_with_sink("resumed".into(), "prompt".into(), updates, state).await,
            Ok(ipc::PromptOutcome::Completed { .. })
        ));
    }
    #[tokio::test]
    async fn suspension_cancels_active_and_rejects_a_successor() {
        let (state, started, finish, _) = pending_state();
        let (updates, _) = sink(false);
        let submit_state = state.clone();
        let active = tokio::spawn(submit_prompt_with_sink(
            "active".into(),
            "prompt".into(),
            updates,
            submit_state,
        ));
        started.await.unwrap();
        set_foreground(&state, false);

        let (updates, _) = sink(false);
        let successor =
            submit_prompt_with_sink("successor".into(), "prompt".into(), updates, state.clone())
                .await
                .unwrap_err();
        assert_eq!(successor.message(), BACKGROUNDED);
        assert!(matches!(
            active.await.unwrap().unwrap(),
            ipc::PromptOutcome::Cancelled
        ));
        assert!(finish
            .send(Ok(response(CustomOpenAiContentType::OutputText)))
            .is_err());
        let inner = state.inner.lock().unwrap();
        assert!(inner.session.turns().is_empty());
        assert!(!inner.session.has_active_execution());
    }
    #[test]
    fn terminal_transitions_suppress_late_deltas() {
        let (cancelled, _) = state(Ok(response(CustomOpenAiContentType::OutputText)));
        let cancelled_id = admit_physical(&cancelled, "cancelled");
        assert!(cancel_execution(&cancelled, cancelled_id));
        let (mut updates, count) = sink(false);
        assert_eq!(
            publish_delta(&cancelled, cancelled_id, &mut updates, "late".into()),
            Err(PublishFailure::ExecutionEnded)
        );
        assert_eq!(count.load(Ordering::Relaxed), 0);
        assert!(cancelled.inner.lock().unwrap().session.turns().is_empty());

        let (reset_state, _) = state(Ok(response(CustomOpenAiContentType::OutputText)));
        let reset_id = admit_physical(&reset_state, "reset");
        reset(&reset_state).unwrap();
        let (mut updates, count) = sink(false);
        assert_eq!(
            publish_delta(&reset_state, reset_id, &mut updates, "late".into()),
            Err(PublishFailure::ExecutionEnded)
        );
        assert_eq!(count.load(Ordering::Relaxed), 0);
        assert!(reset_state.inner.lock().unwrap().session.turns().is_empty());

        let (suspended, _) = state(Ok(response(CustomOpenAiContentType::OutputText)));
        let suspended_id = admit_physical(&suspended, "suspended");
        set_foreground(&suspended, false);
        let (mut updates, count) = sink(false);
        assert_eq!(
            publish_delta(&suspended, suspended_id, &mut updates, "late".into()),
            Err(PublishFailure::ExecutionEnded)
        );
        assert_eq!(count.load(Ordering::Relaxed), 0);
        assert!(suspended.inner.lock().unwrap().session.turns().is_empty());
    }
    #[test]
    fn completed_execution_suppresses_a_late_drain_delta() {
        let (state, _) = state(Ok(response(CustomOpenAiContentType::OutputText)));
        let id = admit_physical(&state, "completed");
        assert!(matches!(
            complete_execution(&state, id, assistant::Content::Text("answer".into())).unwrap(),
            CompletionClaim::Outcome(ipc::PromptOutcome::Completed { .. }, _)
        ));
        let (mut updates, count) = sink(false);
        assert_eq!(
            publish_delta(&state, id, &mut updates, "late".into()),
            Err(PublishFailure::ExecutionEnded)
        );
        assert_eq!(count.load(Ordering::Relaxed), 0);
    }
    #[tokio::test]
    async fn channel_failure_cancels_without_commit() {
        let (state, _) = state(Ok(response(CustomOpenAiContentType::OutputText)));
        let (updates, count) = sink(true);
        let outcome = submit_prompt_with_sink("id".into(), "prompt".into(), updates, state.clone())
            .await
            .unwrap();
        assert!(matches!(outcome, ipc::PromptOutcome::Cancelled));
        assert_eq!(count.load(Ordering::Relaxed), 1);
        let inner = state.inner.lock().unwrap();
        assert!(inner.session.turns().is_empty());
        assert!(!inner.session.has_active_execution());
    }
    #[tokio::test]
    async fn oversized_stream_delta_fails_without_commit() {
        let preferences = default_preferences();
        let state = app_state(preferences, Arc::new(OversizedDeltaProvider));
        let (updates, _) = sink(false);
        let error = submit_prompt_with_sink("id".into(), "prompt".into(), updates, state.clone())
            .await
            .unwrap_err();
        assert_eq!(error.message(), "Custom provider response was too large");
        let inner = state.inner.lock().unwrap();
        assert!(inner.session.turns().is_empty());
        assert!(!inner.session.has_active_execution());
    }
    #[tokio::test]
    async fn dropped_command_lease_releases_admission() {
        let (state, started, finish, _) = pending_state();
        let (updates, _) = sink(false);
        let submit_state = state.clone();
        let submit = tokio::spawn(submit_prompt_with_sink(
            "id".into(),
            "prompt".into(),
            updates,
            submit_state,
        ));
        started.await.unwrap();
        submit.abort();
        assert!(submit.await.unwrap_err().is_cancelled());
        assert!(finish
            .send(Ok(response(CustomOpenAiContentType::OutputText)))
            .is_err());
        assert!(!state.inner.lock().unwrap().session.has_active_execution());
    }
    #[tokio::test]
    async fn provider_panic_drops_lease_and_releases_admission() {
        let preferences = default_preferences();
        let state = app_state(preferences, Arc::new(PanicProvider));
        let (updates, _) = sink(false);
        let submit_state = state.clone();
        let submit = tokio::spawn(submit_prompt_with_sink(
            "id".into(),
            "prompt".into(),
            updates,
            submit_state,
        ));
        assert!(submit.await.unwrap_err().is_panic());
        assert!(!state.inner.lock().unwrap().session.has_active_execution());
    }
    #[tokio::test]
    async fn oversized_provider_response_fails_without_wedging_admission() {
        let mut oversized = response(CustomOpenAiContentType::OutputText);
        oversized.text = "x".repeat(conversation::MAX_UPDATE_BYTES + 1);
        let (state, _) = state(Ok(oversized));
        let (updates, _) = sink(false);
        let error = submit_prompt_with_sink("id".into(), "prompt".into(), updates, state.clone())
            .await
            .unwrap_err();
        assert_eq!(error.code(), ipc::CommandErrorCode::Unavailable);
        assert!(!state.inner.lock().unwrap().session.has_active_execution());
    }
    #[tokio::test]
    async fn busy_rejects_second_admission() {
        let (state, started, finish, _) = pending_state();
        let (first_sink, _) = sink(false);
        let first_state = state.clone();
        let first = tokio::spawn(submit_prompt_with_sink(
            "a".into(),
            "p".into(),
            first_sink,
            first_state,
        ));
        started.await.unwrap();
        let (second_sink, _) = sink(false);
        let error = submit_prompt_with_sink("b".into(), "p".into(), second_sink, state.clone())
            .await
            .unwrap_err();
        assert_eq!(error.code(), ipc::CommandErrorCode::Busy);
        finish
            .send(Ok(response(CustomOpenAiContentType::OutputText)))
            .unwrap();
        assert!(matches!(
            first.await.unwrap().unwrap(),
            ipc::PromptOutcome::Completed { .. }
        ));
    }
    #[tokio::test]
    async fn every_current_custom_model_choice_is_passed_to_provider() {
        for (choice, expected) in [
            (
                ipc::InstantModelChoice::CustomSolHigh,
                CustomOpenAiModel::SolHigh,
            ),
            (
                ipc::InstantModelChoice::CustomSolNone,
                CustomOpenAiModel::SolNone,
            ),
            (
                ipc::InstantModelChoice::CustomLunaNone,
                CustomOpenAiModel::LunaNone,
            ),
            (
                ipc::InstantModelChoice::CustomLunaLow,
                CustomOpenAiModel::LunaLow,
            ),
        ] {
            let mut preferences = default_preferences();
            preferences.instant = choice;
            let models = Arc::new(Mutex::new(Vec::new()));
            let state = app_state(
                preferences,
                Arc::new(FakeProvider {
                    result: Mutex::new(Ok(response(CustomOpenAiContentType::OutputText))),
                    models: models.clone(),
                    requests: Mutex::new(Vec::new()),
                }),
            );
            let _ = submit_prompt_with_sink(
                "id".into(),
                "p".into(),
                Sink {
                    count: Arc::new(AtomicUsize::new(0)),
                    closed: false,
                },
                state,
            )
            .await;
            assert_eq!(models.lock().unwrap().as_slice(), &[expected]);
        }
    }
    #[tokio::test]
    async fn admitted_preferences_are_immutable_until_completion() {
        let mut luna = default_preferences();
        luna.instant = ipc::InstantModelChoice::CustomLunaLow;
        let (state, started, finish, models) = pending_state_with_preferences(luna);
        let (updates, _) = sink(false);
        let submit_state = state.clone();
        let submit = tokio::spawn(submit_prompt_with_sink(
            "id".into(),
            "prompt".into(),
            updates,
            submit_state,
        ));
        started.await.unwrap();
        let error = set_preferences_inner(default_preferences(), &state).unwrap_err();
        assert_eq!(error.code(), ipc::CommandErrorCode::Busy);
        assert_eq!(
            models.lock().unwrap().as_slice(),
            &[CustomOpenAiModel::LunaLow]
        );
        finish
            .send(Ok(response(CustomOpenAiContentType::OutputText)))
            .unwrap();
        assert!(matches!(
            submit.await.unwrap().unwrap(),
            ipc::PromptOutcome::Completed { .. }
        ));
    }
    #[test]
    fn terminal_is_identity_checked() {
        let (state, _) = state(Ok(response(CustomOpenAiContentType::OutputText)));
        assert!(matches!(
            terminal(
                &state,
                conversation::ExecutionId {
                    session_id: conversation::SessionId(99),
                    generation: conversation::GenerationId(1)
                },
                conversation::SettlementIntent::Cancelled
            )
            .unwrap()
            .result,
            conversation::SettlementResult::IgnoredStale
        ));
    }
    #[test]
    fn cancellation_winner_suppresses_late_provider_error() {
        let (state, _) = state(Ok(response(CustomOpenAiContentType::OutputText)));
        let id = admit_physical(&state, "cancelled");
        assert!(cancel_execution(&state, id));
        assert!(matches!(
            fail_execution(&state, id, CustomOpenAiError::Timeout, None).unwrap(),
            ipc::PromptOutcome::Cancelled
        ));
    }
    #[test]
    fn reset_winner_suppresses_late_auth_error() {
        let (state, _) = state(Ok(response(CustomOpenAiContentType::OutputText)));
        let id = admit_physical(&state, "reset");
        reset(&state).unwrap();
        assert!(matches!(
            fail_execution(&state, id, CustomOpenAiError::AuthExpired, None).unwrap(),
            ipc::PromptOutcome::Cancelled
        ));
    }
    #[test]
    fn background_winner_suppresses_late_provider_error() {
        let (state, _) = state(Ok(response(CustomOpenAiContentType::OutputText)));
        let id = admit_physical(&state, "backgrounded");
        cancel_active(&state);
        assert!(matches!(
            fail_execution(&state, id, CustomOpenAiError::Transport, None).unwrap(),
            ipc::PromptOutcome::Cancelled
        ));
    }
}
