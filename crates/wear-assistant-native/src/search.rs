use crate::{
    auth_revision_string, deliver_timed_delta, outcome::Outcome, request, CallbackEvent, ErrorCode,
    FirstTextTiming,
};
use std::cell::Cell;
use std::sync::OnceLock;
use tokio::sync::{watch, Mutex, MutexGuard};
use tokio::time::Instant;
use voxgolem_core::search::Failure;
use voxgolem_platform::custom_openai::{
    tools::ModelEvent, CustomOpenAiAuthRevision, CustomOpenAiClient, CustomOpenAiConfig,
};
use voxgolem_platform::search::{run_with_client_events, RunContext, RunError, RunEvent};
use voxgolem_platform::search_control::Cancellation;
use voxgolem_platform::web::{McpTransport, WebService};

pub(crate) struct ProcessProvider {
    pub runtime: tokio::runtime::Runtime,
    transport: CustomOpenAiClient,
    web: Mutex<WebService<McpTransport>>,
}

impl ProcessProvider {
    pub(crate) fn new(config: CustomOpenAiConfig) -> Result<Self, ErrorCode> {
        let transport = CustomOpenAiClient::new(config).map_err(ErrorCode::from)?;
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .map_err(|_| ErrorCode::InternalError)?;
        Ok(Self {
            runtime,
            transport,
            web: Mutex::new(WebService::new(McpTransport::default())),
        })
    }

    pub(crate) fn client(
        &self,
        config: CustomOpenAiConfig,
    ) -> Result<CustomOpenAiClient, ErrorCode> {
        self.transport
            .with_reused_transport(config)
            .map_err(ErrorCode::from)
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

async fn admit<'a, T>(
    service: &'a Mutex<T>,
    cancellation: &Cancellation,
    deadline: Instant,
) -> Result<MutexGuard<'a, T>, Failure> {
    cancellation.guard(deadline, service.lock()).await
}

pub(crate) async fn respond(
    process: &ProcessProvider,
    mut config: CustomOpenAiConfig,
    request: &request::Request,
    cancelled: watch::Receiver<bool>,
    callback: &mut impl FnMut(CallbackEvent<'_>) -> Result<(), ErrorCode>,
) -> Outcome {
    let cancellation = Cancellation::from_receiver(cancelled);
    let deadline = Instant::now() + config.total_timeout;
    let mut isolated_web = (!request.web_enabled).then(|| WebService::new(McpTransport::default()));
    let mut shared_web = if request.web_enabled {
        match admit(&process.web, &cancellation, deadline).await {
            Ok(service) => Some(service),
            Err(Failure::Cancelled) => return Outcome::Cancelled,
            Err(_) => return Outcome::error(ErrorCode::Timeout),
        }
    } else {
        None
    };
    let web = match (shared_web.as_deref_mut(), isolated_web.as_mut()) {
        (Some(service), _) | (_, Some(service)) => service,
        _ => return Outcome::error(ErrorCode::InternalError),
    };
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
        request.search_mode.expect("search admission requires mode"),
        RunContext {
            conversation_id: &request.prompt.session_id,
            instructions: &request.instructions,
            explicit_search: request.search_mode != Some(voxgolem_core::search::Mode::Quick),
            web_enabled: request.web_enabled,
        },
        &mut *web,
        &cancellation,
        |event| {
            let delivered = match event {
                RunEvent::Model(ModelEvent::AuthRevision(value)) => {
                    revision.set(Some(value));
                    Ok(())
                }
                RunEvent::Model(ModelEvent::RequestDispatched(instant)) => {
                    first_text.dispatched(instant);
                    Ok(())
                }
                RunEvent::Model(ModelEvent::TextDelta { text, .. }) => deliver_timed_delta(
                    &mut bytes,
                    &first_text,
                    text,
                    || cancellation.is_cancelled(),
                    callback,
                ),
                RunEvent::ToolStarted { name, .. } => {
                    callback(CallbackEvent::Status(if name == "web_fetch" {
                        "Reading"
                    } else {
                        "Searching"
                    }))
                }
                RunEvent::ToolCompleted { outcome, .. } => {
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
                        } else {
                            Ok(())
                        }
                    })
                }
                RunEvent::ToolFailed { .. } => callback(CallbackEvent::Status("Partial")),
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
                let persisted = result.persisted_text();
                let outcome = Outcome::from_search_response(result.response, persisted);
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
                outcome
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn waiting_for_shared_service_is_cancelled_without_admission() {
        process().unwrap().runtime.block_on(async {
            let service = Mutex::new(());
            let _owner = service.lock().await;
            let (tx, rx) = watch::channel(false);
            let cancellation = Cancellation::from_receiver(rx);
            let signal = async {
                tokio::task::yield_now().await;
                tx.send(true).unwrap();
            };
            let (result, ()) = tokio::join!(
                admit(
                    &service,
                    &cancellation,
                    Instant::now() + std::time::Duration::from_secs(1)
                ),
                signal
            );
            assert!(matches!(result, Err(Failure::Cancelled)));
        });
    }

    #[test]
    fn runtime_and_service_share_one_process_lifetime_and_admission_deadline() {
        assert!(std::ptr::eq(process().unwrap(), process().unwrap()));
        process().unwrap().runtime.block_on(async {
            let service = Mutex::new(());
            let _owner = service.lock().await;
            let result = admit(&service, &Cancellation::default(), Instant::now()).await;
            assert!(matches!(result, Err(Failure::Deadline)));
        });
    }
}
