use crate::{
    auth_revision_string, deliver_timed_delta,
    outcome::{Outcome, SearchTelemetry},
    request, CallbackEvent, ErrorCode, FirstTextTiming,
};
use std::cell::Cell;
use std::sync::OnceLock;
#[cfg(test)]
use tokio::sync::MutexGuard;
use tokio::sync::{watch, Mutex};
use tokio::time::Instant;
use voxgolem_core::search::Failure;
use voxgolem_platform::custom_openai::{
    tools::ModelEvent, CustomOpenAiAuthRevision, CustomOpenAiClient, CustomOpenAiClientPool,
    CustomOpenAiConfig,
};
use voxgolem_platform::search::{
    run_with_client_events, RunContext, RunError, RunEvent, SharedWebService,
};
use voxgolem_platform::search_control::Cancellation;
use voxgolem_platform::web::{McpTransport, WebService};

pub(crate) struct ProcessProvider {
    pub runtime: tokio::runtime::Runtime,
    transport: CustomOpenAiClientPool,
    web: Mutex<WebService<McpTransport>>,
    prewarm_gate: voxgolem_platform::search::WebPrewarmGate,
}

impl ProcessProvider {
    pub(crate) fn new(config: CustomOpenAiConfig) -> Result<Self, ErrorCode> {
        let transport = CustomOpenAiClientPool::default();
        transport.client_for(config).map_err(ErrorCode::from)?;
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .map_err(|_| ErrorCode::InternalError)?;
        Ok(Self {
            runtime,
            transport,
            web: Mutex::new(WebService::new(McpTransport::default())),
            prewarm_gate: voxgolem_platform::search::WebPrewarmGate::default(),
        })
    }

    pub(crate) fn client(
        &self,
        config: CustomOpenAiConfig,
    ) -> Result<CustomOpenAiClient, ErrorCode> {
        self.transport.client_for(config).map_err(ErrorCode::from)
    }

    #[cfg(not(test))]
    pub(crate) fn prewarm_web(&'static self) {
        self.runtime.spawn(async move {
            voxgolem_platform::search::prewarm_web(&self.web, &self.prewarm_gate).await;
        });
    }
}

#[cfg(test)]
impl ProcessProvider {
    pub(super) async fn hold_web_for_test(&self) -> MutexGuard<'_, WebService<McpTransport>> {
        self.web.lock().await
    }
}

pub(crate) fn process() -> Result<&'static ProcessProvider, ErrorCode> {
    static PROCESS: OnceLock<ProcessProvider> = OnceLock::new();
    static INITIALIZE: std::sync::Mutex<()> = std::sync::Mutex::new(());
    if let Some(process) = PROCESS.get() {
        return Ok(process);
    }
    let _initializing = INITIALIZE.lock().map_err(|_| ErrorCode::InternalError)?;
    if let Some(process) = PROCESS.get() {
        return Ok(process);
    }
    // Publish only success so a transient construction failure can be retried.
    let process = ProcessProvider::new(CustomOpenAiConfig::default())?;
    Ok(PROCESS.get_or_init(|| process))
}

pub(crate) fn prewarm(config: CustomOpenAiConfig, web_enabled: bool) {
    let Ok(process) = process() else {
        return;
    };
    let client = process.client(config).ok();
    if client.is_none() && !web_enabled {
        return;
    }
    process.runtime.spawn(async move {
        voxgolem_platform::search::prewarm_client_and_web(
            client.as_ref(),
            &process.web,
            &process.prewarm_gate,
            web_enabled,
        )
        .await;
    });
}

fn count_status(sources: &[voxgolem_core::search::Source]) -> Option<String> {
    use voxgolem_core::search::EvidenceKind;
    let pages = sources
        .iter()
        .filter(|s| s.kind == EvidenceKind::PageRead)
        .count()
        .min(99);
    let results = sources
        .iter()
        .filter(|s| s.kind == EvidenceKind::SearchExcerpt)
        .count()
        .min(99);
    if pages > 0 {
        Some(format!(
            "Reading · {pages} {}",
            if pages == 1 { "page" } else { "pages" }
        ))
    } else if results > 0 {
        Some(format!(
            "Searching · {results} {}",
            if results == 1 { "result" } else { "results" }
        ))
    } else {
        None
    }
}

pub(crate) async fn respond(
    process: &ProcessProvider,
    config: CustomOpenAiConfig,
    request: &request::Request,
    cancelled: watch::Receiver<bool>,
    callback: &mut impl FnMut(CallbackEvent<'_>) -> Result<(), ErrorCode>,
) -> Outcome {
    let mut web = SharedWebService::new(&process.web, &process.prewarm_gate);
    respond_with_web(process, config, request, &mut web, cancelled, callback).await
}

pub(crate) async fn respond_with_web<W: voxgolem_platform::search::WebExecutor>(
    process: &ProcessProvider,
    mut config: CustomOpenAiConfig,
    request: &request::Request,
    web: &mut W,
    cancelled: watch::Receiver<bool>,
    callback: &mut impl FnMut(CallbackEvent<'_>) -> Result<(), ErrorCode>,
) -> Outcome {
    let turn_started = Instant::now();
    let search_started = Cell::<Option<Instant>>::new(None);
    let search_elapsed_ms = Cell::<u64>::new(0);
    let cancellation = Cancellation::from_receiver(cancelled);
    let deadline = turn_started + config.total_timeout;
    config.total_timeout = deadline.saturating_duration_since(Instant::now());
    if config.total_timeout.is_zero() {
        return Outcome::error(ErrorCode::Timeout);
    }
    let client = match process.client(config) {
        Ok(client) => client,
        Err(error) => return Outcome::error(error),
    };
    let revision = Cell::<Option<CustomOpenAiAuthRevision>>::new(None);
    let mut bytes = 0;
    let mut callback_failure = None;
    let first_text = FirstTextTiming::default();
    let result = run_with_client_events(
        &client,
        &request.prompt,
        RunContext {
            conversation_id: &request.prompt.session_id,
            instructions: &request.instructions,
            web_enabled: request.web_enabled,
        },
        web,
        &cancellation,
        |event| {
            let delivered = match event {
                RunEvent::Model(ModelEvent::AuthRevision(value)) => {
                    revision.set(Some(value));
                    Ok(())
                }
                RunEvent::Model(ModelEvent::RequestDispatched(instant)) => {
                    first_text.dispatched(instant, || cancellation.is_cancelled(), callback)
                }
                RunEvent::Model(ModelEvent::TextDelta { text, .. }) => deliver_timed_delta(
                    &mut bytes,
                    &first_text,
                    text,
                    || cancellation.is_cancelled(),
                    callback,
                ),
                RunEvent::ToolStarted { name, .. } => {
                    if search_started.get().is_none() {
                        search_started.set(Some(Instant::now()));
                    }
                    callback(CallbackEvent::Status(if name == "web_fetch" {
                        "Reading"
                    } else {
                        "Searching"
                    }))
                }
                RunEvent::RateLimit { info } => callback(CallbackEvent::Status(&info.status())),
                RunEvent::ToolCompleted { outcome, .. } => {
                    if let Some(started) = search_started.take() {
                        let elapsed = started.elapsed().as_millis().min(u64::MAX as u128) as u64;
                        search_elapsed_ms.set(search_elapsed_ms.get().saturating_add(elapsed));
                    }
                    let fallback = if outcome.fallback_used {
                        callback(CallbackEvent::Status("Fallback"))
                    } else {
                        Ok(())
                    };
                    fallback.and_then(|()| {
                        if !cancellation.is_cancelled()
                            && (outcome.partial || outcome.evidence_unavailable)
                        {
                            callback(CallbackEvent::Status("Partial"))
                        } else if let Some(status) = count_status(&outcome.sources) {
                            callback(CallbackEvent::Status(&status))
                        } else {
                            Ok(())
                        }
                    })
                }
                RunEvent::ToolFailed { fallback_used, .. } => {
                    if let Some(started) = search_started.take() {
                        let elapsed = started.elapsed().as_millis().min(u64::MAX as u128) as u64;
                        search_elapsed_ms.set(search_elapsed_ms.get().saturating_add(elapsed));
                    }
                    let fallback = if fallback_used {
                        callback(CallbackEvent::Status("Fallback"))
                    } else {
                        Ok(())
                    };
                    fallback.and_then(|()| callback(CallbackEvent::Status("Partial")))
                }
            };
            if let Err(error) = delivered {
                callback_failure = Some(error);
                cancellation.cancel();
                Err(())
            } else {
                Ok(())
            }
        },
    )
    .await;
    let outcome = if let Some(error) = callback_failure {
        Outcome::error(error)
    } else if cancellation.is_cancelled() {
        Outcome::Cancelled
    } else {
        match result {
            Ok(result) => {
                let search_status = if result.searches == 0 && result.reads == 0 {
                    "skipped"
                } else if result.sources.is_empty() {
                    "unavailable"
                } else if result.partial {
                    "partial"
                } else {
                    "ready"
                };
                let fallback_used = result.fallbacks > 0;
                let sources = result
                    .sources
                    .into_iter()
                    .map(|source| voxgolem_core::sources::SourceLink {
                        url: source.url,
                        title: source.title,
                    })
                    .collect();
                let outcome = Outcome::from_search_response(result.response, sources);
                // Some search output is withheld until the final outcome, not streamed.
                if let Outcome::Complete { text, .. } = &outcome {
                    if cancellation.is_cancelled() {
                        return Outcome::Cancelled;
                    }
                    if let Err(error) = first_text.deliver(text, callback) {
                        return Outcome::error(error)
                            .with_auth_revision(revision.get().map(auth_revision_string));
                    }
                    if cancellation.is_cancelled() {
                        return Outcome::Cancelled;
                    }
                }
                let completed_ms = turn_started.elapsed().as_millis().min(u64::MAX as u128) as u64;
                outcome.with_search_telemetry(SearchTelemetry {
                    search_ms: search_elapsed_ms.get(),
                    search_status,
                    fallback_used,
                    completed_ms,
                    rounds: result.rounds as u64,
                    searches: result.searches as u64,
                    reads: result.reads as u64,
                })
            }
            Err(RunError::Provider(error)) => Outcome::error(error.into()),
            Err(RunError::Tool(Failure::Cancelled)) => Outcome::Cancelled,
            Err(RunError::Tool(Failure::Deadline)) => Outcome::error(ErrorCode::Timeout),
            Err(RunError::CallbackFailed) => Outcome::error(ErrorCode::CallbackFailed),
            Err(_) => Outcome::error(ErrorCode::InvalidResponse),
        }
    };
    outcome.with_auth_revision(revision.get().map(auth_revision_string))
}
