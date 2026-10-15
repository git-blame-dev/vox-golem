use crate::{
    auth_revision_string, deliver_timed_delta, outcome::Outcome, request, CallbackEvent, ErrorCode,
    FirstTextTiming,
};
use serde::Serialize;
use std::cell::Cell;
use std::sync::OnceLock;
use tokio::sync::{watch, Mutex, MutexGuard};
use tokio::time::Instant;
use voxgolem_core::search::{has_readable_text, truncate_utf8, Failure, Mode, Source};
use voxgolem_platform::custom_openai::{
    tools::ModelEvent, CustomOpenAiAuthRevision, CustomOpenAiClient, CustomOpenAiConfig,
};
use voxgolem_platform::search::{
    append_source_footer, run_with_client_events, RunContext, RunError, RunEvent,
};
use voxgolem_platform::search_control::Cancellation;
use voxgolem_platform::web::{McpTransport, Operation, WebService, WebTransport};

pub(crate) const MAX_PREPARED_EVIDENCE_BYTES: usize = 4096;
const MAX_SOURCE_EXCERPT_BYTES: usize = 1200;

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SearchPreparation {
    pub evidence_context: Option<String>,
    pub source_footer: String,
    pub sources: Vec<Source>,
}

impl SearchPreparation {
    pub fn json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| {
            r#"{"evidenceContext":null,"sourceFooter":"","sources":[]}"#.to_string()
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SearchPreparationStatus {
    Ready,
    Skipped,
    Cancelled,
    Failed(ErrorCode),
}

#[derive(Clone, Debug)]
pub(crate) struct SearchPrepareRequest {
    pub request_id: Option<String>,
    pub conversation_id: String,
    pub execution_id: String,
    pub query: String,
    pub web_enabled: bool,
    pub mode: Mode,
}

impl SearchPrepareRequest {
    pub fn parse(input: &str) -> Result<Self, ErrorCode> {
        if input.len() > request::MAX_REQUEST_BYTES {
            return Err(ErrorCode::InvalidRequest);
        }
        let value: serde_json::Value =
            serde_json::from_str(input).map_err(|_| ErrorCode::InvalidRequest)?;
        let obj = value.as_object().ok_or(ErrorCode::InvalidRequest)?;

        let request_id = obj
            .get("requestId")
            .or_else(|| obj.get("request_id"))
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        if let Some(id) = &request_id {
            if !request::valid_request_id(id) {
                return Err(ErrorCode::InvalidRequest);
            }
        }

        let mut web_enabled = obj
            .get("webEnabled")
            .or_else(|| obj.get("web_enabled"))
            .and_then(|v| v.as_bool())
            .unwrap_or(true);

        let mode_raw = obj
            .get("searchMode")
            .or_else(|| obj.get("web_search_mode"))
            .or_else(|| obj.get("search_mode"))
            .or_else(|| obj.get("webSearchMode"))
            .and_then(|v| v.as_str())
            .map(|s| s.trim().to_ascii_lowercase());

        let mode = match mode_raw.as_deref() {
            Some("off" | "disabled" | "none") => {
                web_enabled = false;
                Mode::Quick
            }
            Some("verify") => Mode::Verify,
            Some("research") => Mode::Research,
            Some("quick") | None => Mode::Quick,
            Some(_) => return Err(ErrorCode::InvalidRequest),
        };

        let prompt_field = obj
            .get("prompt")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);

        let query = if let Some(prompt) = prompt_field {
            prompt
        } else if let Some(messages) = obj.get("messages").and_then(|v| v.as_array()) {
            messages
                .iter()
                .rev()
                .find_map(|msg| {
                    let role = msg.get("role").and_then(|r| r.as_str()).unwrap_or("user");
                    if role.eq_ignore_ascii_case("user") {
                        msg.get("content")
                            .and_then(|c| c.as_str())
                            .map(str::trim)
                            .filter(|s| !s.is_empty())
                            .map(str::to_string)
                    } else {
                        None
                    }
                })
                .unwrap_or_default()
        } else {
            String::new()
        };

        let raw_session = obj
            .get("externalSessionId")
            .or_else(|| obj.get("external_session_id"))
            .or_else(|| obj.get("conversationId"))
            .or_else(|| obj.get("conversation_id"))
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .or(request_id.as_deref())
            .unwrap_or("gemini-nano-search");
        let conversation_id = voxgolem_core::external_correlation::external_id(raw_session);
        let execution_id = request_id
            .as_deref()
            .map(voxgolem_core::external_correlation::external_id)
            .unwrap_or_else(|| conversation_id.clone());

        Ok(Self {
            request_id,
            conversation_id,
            execution_id,
            query,
            web_enabled,
            mode,
        })
    }
}

pub(crate) async fn prepare_search_context_with_service<T: WebTransport>(
    web: &mut WebService<T>,
    request: &SearchPrepareRequest,
    cancellation: &Cancellation,
    callback: &mut impl FnMut(CallbackEvent<'_>) -> Result<(), ErrorCode>,
) -> (SearchPreparation, SearchPreparationStatus) {
    if !request.web_enabled || request.query.trim().is_empty() {
        return (
            SearchPreparation::default(),
            SearchPreparationStatus::Skipped,
        );
    }
    if cancellation.is_cancelled() {
        return (
            SearchPreparation::default(),
            SearchPreparationStatus::Cancelled,
        );
    }

    let policy = request.mode.policy();
    let deadline = Instant::now() + policy.tool_time;
    let query = truncate_utf8(request.query.trim(), 2048);
    let limit = match request.mode {
        Mode::Quick => 3,
        Mode::Verify => 5,
        Mode::Research => 8,
    };
    let search_op = Operation::Search {
        objective: query.clone(),
        queries: vec![query.clone()],
        limit,
    };

    if let Err(error) = callback(CallbackEvent::Status("Searching")) {
        return (
            SearchPreparation::default(),
            SearchPreparationStatus::Failed(error),
        );
    }
    if cancellation.is_cancelled() {
        return (
            SearchPreparation::default(),
            SearchPreparationStatus::Cancelled,
        );
    }

    let search_outcome = match web
        .execute(
            &search_op,
            &request.conversation_id,
            &request.execution_id,
            deadline,
            cancellation,
        )
        .await
    {
        Err(Failure::Cancelled) => {
            return (
                SearchPreparation::default(),
                SearchPreparationStatus::Cancelled,
            );
        }
        Err(_) if cancellation.is_cancelled() => {
            return (
                SearchPreparation::default(),
                SearchPreparationStatus::Cancelled,
            );
        }
        Err(_) => {
            if let Err(error) = callback(CallbackEvent::Status("Partial")) {
                return (
                    SearchPreparation::default(),
                    SearchPreparationStatus::Failed(error),
                );
            }
            return (SearchPreparation::default(), SearchPreparationStatus::Ready);
        }
        Ok(outcome) => outcome,
    };

    if cancellation.is_cancelled() {
        return (
            SearchPreparation::default(),
            SearchPreparationStatus::Cancelled,
        );
    }
    if search_outcome.fallback_used {
        if let Err(error) = callback(CallbackEvent::Status("Fallback")) {
            return (
                SearchPreparation::default(),
                SearchPreparationStatus::Failed(error),
            );
        }
    }
    if !cancellation.is_cancelled()
        && (search_outcome.partial || search_outcome.evidence_unavailable)
    {
        if let Err(error) = callback(CallbackEvent::Status("Partial")) {
            return (
                SearchPreparation::default(),
                SearchPreparationStatus::Failed(error),
            );
        }
    }
    if cancellation.is_cancelled() {
        return (
            SearchPreparation::default(),
            SearchPreparationStatus::Cancelled,
        );
    }

    let mut collected_sources = search_outcome.sources;

    let max_reads = match request.mode {
        Mode::Quick => 1,
        Mode::Verify => 2,
        Mode::Research => 3,
    };
    let mut fetch_urls = Vec::new();
    for source in &collected_sources {
        if fetch_urls.len() >= max_reads {
            break;
        }
        if let Ok(normalized) = voxgolem_platform::web::public_url(&source.url) {
            if !fetch_urls.contains(&normalized) {
                fetch_urls.push(normalized);
            }
        }
    }

    if !fetch_urls.is_empty() && Instant::now() < deadline {
        if let Err(error) = callback(CallbackEvent::Status("Reading")) {
            return (
                SearchPreparation::default(),
                SearchPreparationStatus::Failed(error),
            );
        }
        if cancellation.is_cancelled() {
            return (
                SearchPreparation::default(),
                SearchPreparationStatus::Cancelled,
            );
        }
        let fetch_op = Operation::Fetch {
            urls: fetch_urls,
            focus: truncate_utf8(&query, 200),
        };
        match web
            .execute(
                &fetch_op,
                &request.conversation_id,
                &request.execution_id,
                deadline,
                cancellation,
            )
            .await
        {
            Err(Failure::Cancelled) => {
                return (
                    SearchPreparation::default(),
                    SearchPreparationStatus::Cancelled,
                );
            }
            Err(_) if cancellation.is_cancelled() => {
                return (
                    SearchPreparation::default(),
                    SearchPreparationStatus::Cancelled,
                );
            }
            Err(_) => {
                if let Err(error) = callback(CallbackEvent::Status("Partial")) {
                    return (
                        SearchPreparation::default(),
                        SearchPreparationStatus::Failed(error),
                    );
                }
            }
            Ok(fetch_outcome) => {
                if cancellation.is_cancelled() {
                    return (
                        SearchPreparation::default(),
                        SearchPreparationStatus::Cancelled,
                    );
                }
                if fetch_outcome.fallback_used {
                    if let Err(error) = callback(CallbackEvent::Status("Fallback")) {
                        return (
                            SearchPreparation::default(),
                            SearchPreparationStatus::Failed(error),
                        );
                    }
                }
                if !cancellation.is_cancelled()
                    && (fetch_outcome.partial || fetch_outcome.evidence_unavailable)
                {
                    if let Err(error) = callback(CallbackEvent::Status("Partial")) {
                        return (
                            SearchPreparation::default(),
                            SearchPreparationStatus::Failed(error),
                        );
                    }
                }
                collected_sources.extend(fetch_outcome.sources);
            }
        }
    }

    if cancellation.is_cancelled() {
        return (
            SearchPreparation::default(),
            SearchPreparationStatus::Cancelled,
        );
    }

    let mut evidence_bytes = 0usize;
    for source in &mut collected_sources {
        let remaining = policy.evidence_bytes.saturating_sub(evidence_bytes);
        if source.text.len() > remaining {
            source.text = truncate_utf8(&source.text, remaining);
            source.truncated = true;
        }
        evidence_bytes += source.text.len();
    }

    let mut formatted_context = String::new();
    let mut item_index = 0usize;
    for source in &collected_sources {
        if !has_readable_text(&source.text) && source.title.trim().is_empty() {
            continue;
        }
        let snippet = truncate_utf8(source.text.trim(), MAX_SOURCE_EXCERPT_BYTES);
        let title = if source.title.trim().is_empty() {
            source.url.trim()
        } else {
            source.title.trim()
        };
        item_index += 1;
        let entry = format!(
            "[{}] {} ({})\n{}\n\n",
            item_index,
            title,
            source.url.trim(),
            snippet
        );
        if formatted_context.len() + entry.len() > MAX_PREPARED_EVIDENCE_BYTES {
            let remaining = MAX_PREPARED_EVIDENCE_BYTES.saturating_sub(formatted_context.len());
            if remaining > 64 {
                formatted_context.push_str(&truncate_utf8(&entry, remaining));
            }
            break;
        }
        formatted_context.push_str(&entry);
    }

    let evidence_context =
        (!formatted_context.trim().is_empty()).then(|| formatted_context.trim_end().to_string());
    let mut source_footer = String::new();
    if evidence_context.is_some() {
        append_source_footer(
            &mut source_footer,
            collected_sources.iter().map(|source| source.url.as_str()),
        );
    }

    (
        SearchPreparation {
            evidence_context,
            source_footer,
            sources: collected_sources,
        },
        SearchPreparationStatus::Ready,
    )
}

pub(crate) async fn prepare(
    process: &ProcessProvider,
    request: &SearchPrepareRequest,
    cancelled: watch::Receiver<bool>,
    callback: &mut impl FnMut(CallbackEvent<'_>) -> Result<(), ErrorCode>,
) -> (SearchPreparation, SearchPreparationStatus) {
    let cancellation = Cancellation::from_receiver(cancelled);
    let deadline = Instant::now() + request.mode.policy().tool_time;
    let mut web = match admit(&process.web, &cancellation, deadline).await {
        Ok(service) => service,
        Err(Failure::Cancelled) => {
            return (
                SearchPreparation::default(),
                SearchPreparationStatus::Cancelled,
            )
        }
        Err(_) => {
            if let Err(error) = callback(CallbackEvent::Status("Partial")) {
                return (
                    SearchPreparation::default(),
                    SearchPreparationStatus::Failed(error),
                );
            }
            return (SearchPreparation::default(), SearchPreparationStatus::Ready);
        }
    };
    prepare_search_context_with_service(&mut web, request, &cancellation, callback).await
}

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
        if config.endpoint != self.transport.config().endpoint {
            CustomOpenAiClient::new(config).map_err(ErrorCode::from)
        } else {
            self.transport
                .with_reused_transport(config)
                .map_err(ErrorCode::from)
        }
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
                        } else if let Some(status) = count_status(&outcome.sources) {
                            callback(CallbackEvent::Status(&status))
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
    use serde_json::{json, Value};
    use voxgolem_core::search::Provider;
    use voxgolem_platform::web::ProviderFailure;

    struct SyntheticWebTransport {
        search_response: Result<Value, ProviderFailure>,
        fetch_response: Result<Value, ProviderFailure>,
        calls: Vec<(Provider, bool)>,
    }

    impl WebTransport for SyntheticWebTransport {
        async fn call(
            &mut self,
            provider: Provider,
            operation: &Operation,
            _conversation_id: &str,
            _deadline: Instant,
            _cancel: &Cancellation,
        ) -> Result<Value, ProviderFailure> {
            self.calls.push((provider, operation.fetch()));
            if operation.fetch() {
                self.fetch_response.clone()
            } else {
                self.search_response.clone()
            }
        }
    }

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

    #[test]
    fn prepare_search_skips_when_web_disabled_or_prompt_empty() {
        process().unwrap().runtime.block_on(async {
            let mut web = WebService::new(SyntheticWebTransport {
                search_response: Err(Failure::Service.into()),
                fetch_response: Err(Failure::Service.into()),
                calls: vec![],
            });
            let req = SearchPrepareRequest::parse(
                r#"{"requestId":"synthetic-req-1","prompt":"Synthetic query","webEnabled":false,"searchMode":"verify"}"#,
            )
            .unwrap();
            let mut statuses: Vec<String> = Vec::new();
            let (prep, status) = prepare_search_context_with_service(
                &mut web,
                &req,
                &Cancellation::default(),
                &mut |event| {
                    if let CallbackEvent::Status(s) = event {
                        statuses.push(s.to_string());
                    }
                    Ok(())
                },
            )
            .await;
            assert_eq!(status, SearchPreparationStatus::Skipped);
            assert!(prep.evidence_context.is_none());
            assert_eq!(prep.source_footer, "");
            assert!(prep.sources.is_empty());
            assert!(statuses.is_empty());
            assert!(web.transport.calls.is_empty());
        });
    }

    #[test]
    fn prepare_search_emits_searching_and_reading_and_builds_source_footer() {
        process().unwrap().runtime.block_on(async {
            let mut web = WebService::new(SyntheticWebTransport {
                search_response: Ok(json!({
                    "structuredContent": {
                        "results": [
                            {
                                "url": "https://example.com/article-1",
                                "title": "Synthetic Article One",
                                "excerpts": ["Key synthetic search excerpt one."]
                            }
                        ]
                    }
                })),
                fetch_response: Ok(json!({
                    "structuredContent": {
                        "results": [
                            {
                                "url": "https://example.com/article-1",
                                "title": "Synthetic Article One",
                                "full_content": "Full page body text from synthetic article one."
                            }
                        ]
                    }
                })),
                calls: vec![],
            });
            let req = SearchPrepareRequest::parse(
                r#"{"requestId":"synthetic-req-2","messages":[{"role":"user","content":"What happened in synthetic event?"}],"web_enabled":true,"web_search_mode":"verify"}"#,
            )
            .unwrap();
            let mut statuses: Vec<String> = Vec::new();
            let (prep, status) = prepare_search_context_with_service(
                &mut web,
                &req,
                &Cancellation::default(),
                &mut |event| {
                    if let CallbackEvent::Status(s) = event {
                        statuses.push(s.to_string());
                    }
                    Ok(())
                },
            )
            .await;
            assert_eq!(status, SearchPreparationStatus::Ready);
            assert_eq!(statuses, vec!["Searching", "Reading"]);
            let evidence = prep.evidence_context.as_deref().unwrap();
            assert!(evidence.contains("Synthetic Article One"));
            assert!(evidence.contains("Key synthetic search excerpt one."));
            assert!(evidence.contains("Full page body text from synthetic article one."));
            assert_eq!(
                prep.source_footer,
                "\n\nSources:\nhttps://example.com/article-1\n"
            );
            assert_eq!(prep.sources.len(), 2);

            let serialized: Value = serde_json::from_str(&prep.json()).unwrap();
            assert_eq!(
                serialized["sourceFooter"],
                "\n\nSources:\nhttps://example.com/article-1\n"
            );
            assert!(serialized["evidenceContext"].is_string());
            assert_eq!(serialized["sources"].as_array().unwrap().len(), 2);
        });
    }

    #[test]
    fn prepare_search_emits_fallback_and_partial_when_primary_fails_or_empty() {
        process().unwrap().runtime.block_on(async {
            struct FallbackTransport;
            impl WebTransport for FallbackTransport {
                async fn call(
                    &mut self,
                    provider: Provider,
                    operation: &Operation,
                    _conversation_id: &str,
                    _deadline: Instant,
                    _cancel: &Cancellation,
                ) -> Result<Value, ProviderFailure> {
                    if provider == Provider::Parallel || operation.fetch() {
                        Err(Failure::Service.into())
                    } else {
                        Ok(json!({
                            "content": [{
                                "type": "text",
                                "text": "Title: Exa Fallback Result\nURL: https://example.org/fallback\nHighlights:\nFallback evidence snippet"
                            }]
                        }))
                    }
                }
            }

            let mut web = WebService::new(FallbackTransport);
            let req = SearchPrepareRequest::parse(
                r#"{"requestId":"synthetic-req-3","prompt":"Fallback query","webEnabled":true,"searchMode":"verify"}"#,
            )
            .unwrap();
            let mut statuses: Vec<String> = Vec::new();
            let (prep, status) = prepare_search_context_with_service(
                &mut web,
                &req,
                &Cancellation::default(),
                &mut |event| {
                    if let CallbackEvent::Status(s) = event {
                        statuses.push(s.to_string());
                    }
                    Ok(())
                },
            )
            .await;
            assert_eq!(status, SearchPreparationStatus::Ready);
            assert_eq!(statuses, vec!["Searching", "Fallback", "Reading", "Partial"]);
            assert!(prep
                .evidence_context
                .as_deref()
                .unwrap()
                .contains("Fallback evidence snippet"));
            assert_eq!(
                prep.source_footer,
                "\n\nSources:\nhttps://example.org/fallback\n"
            );
        });
    }
}
