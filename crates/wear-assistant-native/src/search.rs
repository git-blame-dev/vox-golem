use crate::{
    auth_revision_string, deliver_timed_delta, outcome::Outcome, request, CallbackEvent, ErrorCode,
    FirstTextTiming,
};
use serde::Serialize;
use std::cell::Cell;
use std::sync::OnceLock;
#[cfg(test)]
use tokio::sync::MutexGuard;
use tokio::sync::{watch, Mutex};
use tokio::time::Instant;
use voxgolem_core::router::classify_prompt_2d;
use voxgolem_core::search::{Failure, Mode, Source};
use voxgolem_platform::custom_openai::{
    tools::ModelEvent, CustomOpenAiAuthRevision, CustomOpenAiClient, CustomOpenAiConfig,
};
use voxgolem_platform::search::{
    prepare_round_zero_search_with_events, run_with_client_events, RunContext, RunError, RunEvent,
};
use voxgolem_platform::search_control::Cancellation;
#[cfg(test)]
use voxgolem_platform::web::Operation;
use voxgolem_platform::web::{McpTransport, WebService, WebTransport};

pub fn should_run_web_search(
    web_enabled: bool,
    search_mode: &str,
    query: &str,
    prev_user_query: Option<&str>,
) -> bool {
    if !web_enabled || query.trim().is_empty() {
        return false;
    }
    let normalized_mode = search_mode.trim().to_ascii_lowercase();
    if matches!(normalized_mode.as_str(), "off" | "disabled" | "none") {
        return false;
    }
    let mode = match normalized_mode.as_str() {
        "verify" => Mode::Verify,
        "research" => Mode::Research,
        _ => Mode::Quick,
    };
    if mode != Mode::Quick {
        return true;
    }
    let prev = prev_user_query.map(str::trim).filter(|s| !s.is_empty());
    classify_prompt_2d(query, prev, None).needs_search()
}

pub fn route_code_for_query(query: &str, prev_user_query: Option<&str>) -> &'static str {
    let prev = prev_user_query.map(str::trim).filter(|s| !s.is_empty());
    classify_prompt_2d(query, prev, None).primary.code()
}

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SearchPreparation {
    pub evidence_context: Option<String>,
    pub source_footer: String,
    pub sources: Vec<Source>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub needs_escalation: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub route: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub needs_search: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub fallback_used: bool,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub status_code: String,
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
    pub prev_user_turn: Option<String>,
    pub enforce_quick_router: bool,
}

fn strip_source_footer(text: &str) -> &str {
    match text.find("\n\nSources:\n") {
        Some(idx) => text[..idx].trim_end(),
        None => text,
    }
}

fn truncate_utf8(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].trim_end()
}

fn fold_prior_user_turns<'a>(
    user_msgs: impl IntoIterator<Item = &'a str>,
    current_query: &str,
) -> Option<String> {
    let trimmed_current = strip_source_footer(current_query.trim()).trim();
    let mut prior: Vec<&'a str> = user_msgs
        .into_iter()
        .map(|msg| strip_source_footer(msg.trim()).trim())
        .filter(|msg| !msg.is_empty())
        .collect();
    if !trimmed_current.is_empty() && prior.last().copied() == Some(trimmed_current) {
        prior.pop();
    }
    match prior.len() {
        0 => None,
        1 => Some(prior[0].to_string()),
        len => {
            let window = &prior[len.saturating_sub(3)..];
            let first = window[0];
            let last = window[window.len() - 1];
            let combined = format!("{first} {last}");
            Some(truncate_utf8(&combined, 200).to_string())
        }
    }
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
            Some("quick" | "auto" | "") | None => Mode::Quick,
            Some(_) => return Err(ErrorCode::InvalidRequest),
        };

        let prompt_field = obj
            .get("prompt")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);

        let query = if let Some(prompt) = prompt_field.as_ref() {
            prompt.clone()
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

        let prev_user_turn = obj
            .get("messages")
            .and_then(|v| v.as_array())
            .and_then(|messages| {
                let user_msgs = messages.iter().filter_map(|msg| {
                    let role = msg.get("role").and_then(|r| r.as_str()).unwrap_or("user");
                    if role.eq_ignore_ascii_case("user") {
                        msg.get("content").and_then(|c| c.as_str())
                    } else {
                        None
                    }
                });
                fold_prior_user_turns(user_msgs, &query)
            });

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

        let enforce_quick_router = obj
            .get("enforceQuickRouter")
            .or_else(|| obj.get("enforce_quick_router"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        Ok(Self {
            request_id,
            conversation_id,
            execution_id,
            query,
            web_enabled,
            mode,
            prev_user_turn,
            enforce_quick_router,
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
            SearchPreparation {
                status_code: "skipped".to_string(),
                ..SearchPreparation::default()
            },
            SearchPreparationStatus::Skipped,
        );
    }
    let deadline = Instant::now() + request.mode.policy().tool_time;
    let mut callback_error = None;
    let prepared = match prepare_round_zero_search_with_events(
        web,
        &request.conversation_id,
        &request.execution_id,
        &request.query,
        request.prev_user_turn.as_deref(),
        request.mode,
        request.web_enabled,
        request.enforce_quick_router,
        deadline,
        cancellation,
        |event| {
            let res = match event {
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
                RunEvent::Model(_) => Ok(()),
            };
            if let Err(err) = res {
                callback_error = Some(err);
                Err(())
            } else {
                Ok(())
            }
        },
    )
    .await
    {
        Ok(prepared) => prepared,
        Err(RunError::CallbackFailed) => {
            return (
                SearchPreparation {
                    status_code: "unavailable".to_string(),
                    ..SearchPreparation::default()
                },
                SearchPreparationStatus::Failed(
                    callback_error.unwrap_or(ErrorCode::CallbackFailed),
                ),
            );
        }
        Err(RunError::Tool(Failure::Cancelled)) => {
            return (
                SearchPreparation {
                    status_code: "cancelled".to_string(),
                    ..SearchPreparation::default()
                },
                SearchPreparationStatus::Cancelled,
            );
        }
        Err(RunError::Tool(Failure::InvalidArguments)) => {
            if cancellation.is_cancelled() {
                return (
                    SearchPreparation {
                        status_code: "cancelled".to_string(),
                        ..SearchPreparation::default()
                    },
                    SearchPreparationStatus::Cancelled,
                );
            }
            return (
                SearchPreparation {
                    status_code: "invalid".to_string(),
                    ..SearchPreparation::default()
                },
                SearchPreparationStatus::Ready,
            );
        }
        Err(_) => {
            if cancellation.is_cancelled() {
                return (
                    SearchPreparation {
                        status_code: "cancelled".to_string(),
                        ..SearchPreparation::default()
                    },
                    SearchPreparationStatus::Cancelled,
                );
            }
            return (
                SearchPreparation {
                    status_code: "unavailable".to_string(),
                    ..SearchPreparation::default()
                },
                SearchPreparationStatus::Ready,
            );
        }
    };

    if prepared.searches == 0 && !prepared.needs_search {
        return (
            SearchPreparation {
                route: Some(prepared.route.code().to_string()),
                needs_search: false,
                fallback_used: false,
                status_code: "skipped".to_string(),
                ..SearchPreparation::default()
            },
            SearchPreparationStatus::Skipped,
        );
    }

    let status_code = if prepared.evidence_context.is_none() && prepared.sources.is_empty() {
        "unavailable"
    } else if prepared.partial {
        "partial"
    } else {
        "ready"
    };

    (
        SearchPreparation {
            evidence_context: prepared.evidence_context,
            source_footer: prepared.source_footer,
            sources: prepared.sources,
            needs_escalation: prepared.needs_escalation,
            route: Some(prepared.route.code().to_string()),
            needs_search: prepared.needs_search,
            fallback_used: prepared.fallbacks > 0,
            status_code: status_code.to_string(),
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
    let mut web = match voxgolem_platform::search::admit_web(
        &process.web,
        &process.prewarm_gate,
        &cancellation,
        deadline,
    )
    .await
    {
        Ok(service) => service,
        Err(Failure::Cancelled) => {
            return (
                SearchPreparation {
                    status_code: "cancelled".to_string(),
                    ..SearchPreparation::default()
                },
                SearchPreparationStatus::Cancelled,
            )
        }
        Err(_) => {
            if let Err(error) = callback(CallbackEvent::Status("Partial")) {
                return (
                    SearchPreparation {
                        status_code: "unavailable".to_string(),
                        ..SearchPreparation::default()
                    },
                    SearchPreparationStatus::Failed(error),
                );
            }
            return (
                SearchPreparation {
                    status_code: "unavailable".to_string(),
                    ..SearchPreparation::default()
                },
                SearchPreparationStatus::Ready,
            );
        }
    };
    prepare_search_context_with_service(&mut web, request, &cancellation, callback).await
}

pub(crate) struct ProcessProvider {
    pub runtime: tokio::runtime::Runtime,
    transport: CustomOpenAiClient,
    web: Mutex<WebService<McpTransport>>,
    prewarm_gate: voxgolem_platform::search::WebPrewarmGate,
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
            prewarm_gate: voxgolem_platform::search::WebPrewarmGate::default(),
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

pub(crate) fn prewarm(config: Option<CustomOpenAiConfig>, web_enabled: bool) {
    let Ok(process) = process() else {
        return;
    };
    if let Some(config) = config {
        if let Ok(client) = process.client(config) {
            process.runtime.spawn(async move {
                let _ = client.prewarm().await;
            });
        }
    }
    if web_enabled {
        process.runtime.spawn(async move {
            voxgolem_platform::search::prewarm_web(&process.web, &process.prewarm_gate).await;
        });
    }
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

pub(crate) fn route_prompt_2d(
    prompt: &voxgolem_platform::custom_openai::CustomOpenAiPrompt,
) -> voxgolem_core::router::RoutingDecision {
    let prev_user = fold_prior_user_turns(
        prompt.history.iter().filter_map(|msg| {
            matches!(
                msg.role,
                voxgolem_platform::custom_openai::CustomOpenAiRole::User
            )
            .then_some(msg.text.as_str())
        }),
        &prompt.prompt,
    );
    classify_prompt_2d(&prompt.prompt, prev_user.as_deref(), None)
}

pub(crate) async fn respond(
    process: &ProcessProvider,
    mut config: CustomOpenAiConfig,
    request: &request::Request,
    cancelled: watch::Receiver<bool>,
    callback: &mut impl FnMut(CallbackEvent<'_>) -> Result<(), ErrorCode>,
) -> Outcome {
    let turn_started = Instant::now();
    let routing = route_prompt_2d(&request.prompt);
    let route_code = routing.primary.code().to_string();
    let mode = request.search_mode.expect("search admission requires mode");
    let effective_mode = if request.web_enabled {
        routing.primary.effective_search_mode(mode).unwrap_or(mode)
    } else {
        mode
    };
    let use_shared_web = request.web_enabled
        && (mode != voxgolem_core::search::Mode::Quick
            || routing.needs_search()
            || !process.prewarm_gate.is_prewarming());
    let search_started = Cell::<Option<Instant>>::new(None);
    let search_elapsed_ms = Cell::<u64>::new(0);
    let cancellation = Cancellation::from_receiver(cancelled);
    let deadline = Instant::now() + config.total_timeout;
    let mut isolated_web = (!use_shared_web).then(|| WebService::new(McpTransport::default()));
    let mut shared_web = if use_shared_web {
        match voxgolem_platform::search::admit_web(
            &process.web,
            &process.prewarm_gate,
            &cancellation,
            deadline,
        )
        .await
        {
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
        effective_mode,
        RunContext {
            conversation_id: &request.prompt.session_id,
            instructions: &request.instructions,
            explicit_search: mode != voxgolem_core::search::Mode::Quick,
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
                    if search_started.get().is_none() {
                        search_started.set(Some(Instant::now()));
                    }
                    callback(CallbackEvent::Status(if name == "web_fetch" {
                        "Reading"
                    } else {
                        "Searching"
                    }))
                }
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
                RunEvent::ToolFailed { .. } => {
                    if let Some(started) = search_started.take() {
                        let elapsed = started.elapsed().as_millis().min(u64::MAX as u128) as u64;
                        search_elapsed_ms.set(search_elapsed_ms.get().saturating_add(elapsed));
                    }
                    callback(CallbackEvent::Status("Partial"))
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
                let search_status = if result.searches == 0 {
                    "skipped"
                } else if result.sources.is_empty() {
                    "unavailable"
                } else if result.partial {
                    "partial"
                } else {
                    "ready"
                };
                let fallback_used = result.fallbacks > 0;
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
                let completed_ms = turn_started.elapsed().as_millis().min(u64::MAX as u128) as u64;
                outcome.with_search_telemetry(
                    route_code,
                    search_elapsed_ms.get(),
                    search_status,
                    fallback_used,
                    completed_ms,
                )
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
    use voxgolem_core::router::Route2D;
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
            let gate = voxgolem_platform::search::WebPrewarmGate::default();
            let _owner = service.lock().await;
            let (tx, rx) = watch::channel(false);
            let cancellation = Cancellation::from_receiver(rx);
            let signal = async {
                tokio::task::yield_now().await;
                tx.send(true).unwrap();
            };
            let (result, ()) = tokio::join!(
                voxgolem_platform::search::admit_web(
                    &service,
                    &gate,
                    &cancellation,
                    Instant::now() + std::time::Duration::from_secs(1)
                ),
                signal
            );
            assert!(matches!(result, Err(Failure::Cancelled)));
        });
    }

    #[test]
    fn in_flight_web_prewarm_is_immediately_preempted_by_admit_web() {
        struct SlowPrewarmTransport;
        impl WebTransport for SlowPrewarmTransport {
            async fn prewarm(
                &mut self,
                _provider: Provider,
                deadline: Instant,
                cancel: &Cancellation,
            ) -> Result<(), ProviderFailure> {
                cancel
                    .guard(
                        deadline,
                        tokio::time::sleep(std::time::Duration::from_secs(10)),
                    )
                    .await
                    .map_err(ProviderFailure::from)
            }
            async fn call(
                &mut self,
                _provider: Provider,
                _operation: &Operation,
                _conversation_id: &str,
                _deadline: Instant,
                _cancel: &Cancellation,
            ) -> Result<Value, ProviderFailure> {
                Ok(json!({}))
            }
        }

        process().unwrap().runtime.block_on(async {
            let service = Mutex::new(WebService::new(SlowPrewarmTransport));
            let gate = voxgolem_platform::search::WebPrewarmGate::default();
            let started = Instant::now();
            let prewarm_task = async {
                voxgolem_platform::search::prewarm_web(&service, &gate).await;
            };
            let admit_task = async {
                tokio::task::yield_now().await;
                let cancel = Cancellation::default();
                let guard = voxgolem_platform::search::admit_web(
                    &service,
                    &gate,
                    &cancel,
                    Instant::now() + std::time::Duration::from_secs(1),
                )
                .await;
                assert!(guard.is_ok());
            };
            tokio::join!(prewarm_task, admit_task);
            assert!(
                started.elapsed() < std::time::Duration::from_millis(200),
                "admit_web must immediately preempt in-flight prewarm without waiting for timeout"
            );
        });
    }

    #[test]
    fn runtime_and_service_share_one_process_lifetime_and_admission_deadline() {
        assert!(std::ptr::eq(process().unwrap(), process().unwrap()));
        process().unwrap().runtime.block_on(async {
            let service = Mutex::new(());
            let gate = voxgolem_platform::search::WebPrewarmGate::default();
            let _owner = service.lock().await;
            let result = voxgolem_platform::search::admit_web(
                &service,
                &gate,
                &Cancellation::default(),
                Instant::now(),
            )
            .await;
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
                    },
                    "content": [{
                        "type": "text",
                        "text": "Title: Synthetic Article One\nURL: https://example.com/article-1\nHighlights:\nKey synthetic search excerpt one."
                    }]
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
                    },
                    "content": [{
                        "type": "text",
                        "text": "# Synthetic Article One\nURL: https://example.com/article-1\n\nFull page body text from synthetic article one."
                    }]
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
            struct FallbackTransport {
                search_attempts: usize,
            }
            impl WebTransport for FallbackTransport {
                async fn call(
                    &mut self,
                    _provider: Provider,
                    operation: &Operation,
                    _conversation_id: &str,
                    _deadline: Instant,
                    _cancel: &Cancellation,
                ) -> Result<Value, ProviderFailure> {
                    if operation.fetch() {
                        return Err(Failure::Service.into());
                    }
                    self.search_attempts += 1;
                    if self.search_attempts == 1 {
                        Err(Failure::Service.into())
                    } else {
                        Ok(json!({
                            "structuredContent": {
                                "results": [{
                                    "url": "https://example.org/fallback",
                                    "title": "Fallback Result",
                                    "excerpts": ["Fallback evidence snippet"]
                                }]
                            },
                            "content": [{
                                "type": "text",
                                "text": "Title: Fallback Result\nURL: https://example.org/fallback\nHighlights:\nFallback evidence snippet"
                            }]
                        }))
                    }
                }
            }

            let mut web = WebService::new(FallbackTransport { search_attempts: 0 });
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

    #[test]
    fn prepare_search_skips_in_quick_mode_for_supplied_text_intent() {
        process().unwrap().runtime.block_on(async {
            let mut web = WebService::new(SyntheticWebTransport {
                search_response: Err(Failure::Service.into()),
                fetch_response: Err(Failure::Service.into()),
                calls: vec![],
            });
            let req = SearchPrepareRequest::parse(
                r#"{"requestId":"req-skip-supplied","prompt":"Fix the typos in this sentence: Who won the game today?","webEnabled":true,"searchMode":"quick"}"#,
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
            assert!(!prep.needs_escalation);
            assert_eq!(prep.route, Some("F".to_string()));
            assert!(!prep.needs_search);
            assert!(statuses.is_empty());
            assert!(web.transport.calls.is_empty());
        });
    }

    #[test]
    fn prepare_search_escalates_and_upgrades_max_reads_on_stub_snippets_without_digits() {
        process().unwrap().runtime.block_on(async {
            struct StubTransport {
                fetched_urls: Vec<String>,
            }
            impl WebTransport for StubTransport {
                async fn call(
                    &mut self,
                    _provider: Provider,
                    operation: &Operation,
                    _conversation_id: &str,
                    _deadline: Instant,
                    _cancel: &Cancellation,
                ) -> Result<Value, ProviderFailure> {
                    match operation {
                        Operation::Search { .. } => Ok(json!({
                            "structuredContent": {
                                "results": [
                                    {
                                        "url": "https://example.com/amd-1",
                                        "title": "AMD Chart One",
                                        "excerpts": ["Click to view full chart"]
                                    },
                                    {
                                        "url": "https://example.com/amd-2",
                                        "title": "AMD Chart Two",
                                        "excerpts": ["See full chart on page"]
                                    }
                                ]
                            },
                            "content": [
                                {
                                    "type": "text",
                                    "text": "Title: AMD Chart One\nURL: https://example.com/amd-1\nHighlights:\nClick to view full chart"
                                },
                                {
                                    "type": "text",
                                    "text": "Title: AMD Chart Two\nURL: https://example.com/amd-2\nHighlights:\nSee full chart on page"
                                }
                            ]
                        })),
                        Operation::Fetch { urls, .. } => {
                            self.fetched_urls.extend(urls.iter().cloned());
                            let results: Vec<Value> = urls
                                .iter()
                                .map(|url| {
                                    json!({
                                        "url": url,
                                        "title": "AMD Chart",
                                        "full_content": "Full page without numeric price details."
                                    })
                                })
                                .collect();
                            let content: Vec<Value> = urls
                                .iter()
                                .map(|url| {
                                    json!({
                                        "type": "text",
                                        "text": format!("# AMD Chart\nURL: {url}\n\nFull page without numeric price details.")
                                    })
                                })
                                .collect();
                            Ok(json!({
                                "structuredContent": { "results": results },
                                "content": content
                            }))
                        }
                    }
                }
            }

            let mut web = WebService::new(StubTransport {
                fetched_urls: Vec::new(),
            });
            let req = SearchPrepareRequest::parse(
                r#"{"requestId":"req-escalate-numeric","prompt":"What is the stock price of AMD today?","webEnabled":true,"searchMode":"quick"}"#,
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
            // In Quick mode, max_reads normally defaults to 1.
            // Because snippet sufficiency was NeedsEscalation, max_reads was upgraded to 2!
            assert_eq!(
                web.transport.fetched_urls,
                vec![
                    "https://example.com/amd-1".to_string(),
                    "https://example.com/amd-2".to_string(),
                ]
            );
            assert!(prep.needs_escalation);

            let serialized: Value = serde_json::from_str(&prep.json()).unwrap();
            assert_eq!(serialized["needsEscalation"], true);
        });
    }

    #[test]
    fn route_prompt_2d_skips_quick_search_for_direct_fast_and_preserves_multiturn_followup() {
        use voxgolem_platform::custom_openai::{
            CustomOpenAiContentType, CustomOpenAiMessage, CustomOpenAiPrompt, CustomOpenAiRole,
        };

        let supplied_prompt = CustomOpenAiPrompt {
            session_id: "test-session".to_string(),
            prompt: "Fix the typos in this sentence: What is the weather in Tokyo today?"
                .to_string(),
            history: vec![],
        };
        let supplied_route = route_prompt_2d(&supplied_prompt);
        assert_eq!(supplied_route.primary, Route2D::DirectFast);
        assert!(!supplied_route.needs_search());

        let followup_prompt = CustomOpenAiPrompt {
            session_id: "test-session".to_string(),
            prompt: "What about Nvidia?".to_string(),
            history: vec![CustomOpenAiMessage {
                role: CustomOpenAiRole::User,
                content_type: CustomOpenAiContentType::OutputText,
                text: "What is the current stock price of AMD right now?".to_string(),
            }],
        };
        let followup_route = route_prompt_2d(&followup_prompt);
        assert_eq!(followup_route.primary, Route2D::SearchFast);
        assert!(followup_route.needs_search());
    }

    #[test]
    fn prepare_search_enforce_quick_router_skips_direct_and_runs_search_fast() {
        process().unwrap().runtime.block_on(async {
            // 1. DirectFast with enforceQuickRouter: true skips search
            let mut web = WebService::new(SyntheticWebTransport {
                search_response: Err(Failure::Service.into()),
                fetch_response: Err(Failure::Service.into()),
                calls: vec![],
            });
            let req_fast = SearchPrepareRequest::parse(
                r#"{"requestId":"req-fast","prompt":"Hello, what is 2+2?","webEnabled":true,"searchMode":"quick","enforceQuickRouter":true}"#,
            )
            .unwrap();
            let mut statuses = Vec::new();
            let (prep_fast, status_fast) = prepare_search_context_with_service(
                &mut web,
                &req_fast,
                &Cancellation::default(),
                &mut |event| {
                    if let CallbackEvent::Status(s) = event {
                        statuses.push(s.to_string());
                    }
                    Ok(())
                },
            )
            .await;
            assert_eq!(status_fast, SearchPreparationStatus::Skipped);
            assert_eq!(prep_fast.route, Some("F".to_string()));
            assert!(!prep_fast.needs_search);
            assert!(web.transport.calls.is_empty());
            assert!(statuses.is_empty());

            // 2. DirectSmart with enforceQuickRouter: true skips search
            let req_smart = SearchPrepareRequest::parse(
                r#"{"requestId":"req-smart","prompt":"How do I write a rust macro with pattern matching?","webEnabled":true,"searchMode":"quick","enforceQuickRouter":true}"#,
            )
            .unwrap();
            let (prep_smart, status_smart) = prepare_search_context_with_service(
                &mut web,
                &req_smart,
                &Cancellation::default(),
                &mut |_| Ok(()),
            )
            .await;
            assert_eq!(status_smart, SearchPreparationStatus::Skipped);
            assert_eq!(prep_smart.route, Some("C".to_string()));
            assert!(!prep_smart.needs_search);
            assert!(web.transport.calls.is_empty());

            // 3. SearchFast with enforceQuickRouter: true proceeds to search
            let mut web_search = WebService::new(SyntheticWebTransport {
                search_response: Ok(json!({
                    "structuredContent": {
                        "results": [
                            {
                                "url": "https://example.com/weather-tokyo",
                                "title": "Tokyo Weather",
                                "excerpts": ["Current Tokyo temperature is 18C and clear."]
                            }
                        ]
                    },
                    "content": [{
                        "type": "text",
                        "text": "Title: Tokyo Weather\nURL: https://example.com/weather-tokyo\nHighlights:\nCurrent Tokyo temperature is 18C and clear."
                    }]
                })),
                fetch_response: Err(Failure::Service.into()),
                calls: vec![],
            });
            let req_search = SearchPrepareRequest::parse(
                r#"{"requestId":"req-search","prompt":"What is the current weather in Tokyo?","webEnabled":true,"searchMode":"quick","enforceQuickRouter":true}"#,
            )
            .unwrap();
            let (prep_search, status_search) = prepare_search_context_with_service(
                &mut web_search,
                &req_search,
                &Cancellation::default(),
                &mut |_| Ok(()),
            )
            .await;
            assert_eq!(status_search, SearchPreparationStatus::Ready);
            assert_eq!(prep_search.route, Some("S".to_string()));
            assert!(prep_search.needs_search);
            assert!(!prep_search.fallback_used);
            assert_eq!(prep_search.status_code, "ready");
            assert_eq!(web_search.transport.calls.len(), 1);
            assert_eq!(prep_search.sources.len(), 1);
        });
    }

    #[test]
    fn should_run_web_search_delegates_to_2d_router_and_respects_modes() {
        assert!(!should_run_web_search(
            false,
            "verify",
            "What is the weather in Tokyo?",
            None
        ));
        assert!(!should_run_web_search(true, "verify", "   ", None));
        assert!(!should_run_web_search(
            true,
            "off",
            "What is the weather in Tokyo?",
            None
        ));
        assert!(should_run_web_search(true, "verify", "Hello", None));
        assert!(should_run_web_search(true, "research", "Hello", None));

        // DirectFast -> false
        assert!(!should_run_web_search(
            true,
            "quick",
            "What is 14 * 15?",
            None
        ));
        assert!(!should_run_web_search(
            true,
            "quick",
            "Fix typos: Who won the game today?",
            None
        ));

        // DirectSmart -> false
        assert!(!should_run_web_search(
            true,
            "quick",
            "Why does this Rust ring buffer panic?",
            None
        ));

        // SearchFast -> true
        assert!(should_run_web_search(
            true,
            "quick",
            "What is the current weather in Tokyo?",
            None
        ));

        // ResearchSmart -> true
        assert!(should_run_web_search(
            true,
            "quick",
            "Compare and contrast Tokio and async-std with a deep dive into their schedulers",
            None
        ));

        // Multi-turn elliptical follow-up -> true when previous turn was search
        assert!(should_run_web_search(
            true,
            "quick",
            "And what about the other one?",
            Some("What is the current stock price of AMD right now?")
        ));
        assert!(!should_run_web_search(
            true,
            "quick",
            "And what about the other one?",
            None
        ));
    }

    #[test]
    fn three_turn_conversation_folds_root_topic_into_parse_and_route_prompt_2d() {
        use voxgolem_platform::custom_openai::{
            CustomOpenAiContentType, CustomOpenAiMessage, CustomOpenAiPrompt, CustomOpenAiRole,
        };

        let turn1 = "In Magic: The Gathering, how does Urza's Saga work?";
        let turn2 = "What about its second ability?";
        let turn3 = "And what if it gets destroyed?";

        let req_json = json!({
            "requestId": "req-three-turn",
            "webEnabled": true,
            "searchMode": "quick",
            "messages": [
                { "role": "user", "content": turn1 },
                { "role": "assistant", "content": "It is an enchantment land with chapter abilities.\n\nSources:\nhttps://example.com/mtg\n" },
                { "role": "user", "content": format!("{turn2}\n\nSources:\nhttps://example.com/mtg\n") },
                { "role": "assistant", "content": "Chapter II creates Construct tokens." },
                { "role": "user", "content": turn3 }
            ]
        })
        .to_string();

        let parsed = SearchPrepareRequest::parse(&req_json).unwrap();
        assert_eq!(parsed.query, turn3);
        assert_eq!(
            parsed.prev_user_turn.as_deref(),
            Some("In Magic: The Gathering, how does Urza's Saga work? What about its second ability?")
        );
        assert!(should_run_web_search(
            true,
            "quick",
            &parsed.query,
            parsed.prev_user_turn.as_deref()
        ));

        let prompt = CustomOpenAiPrompt {
            session_id: "test-session".to_string(),
            prompt: turn3.to_string(),
            history: vec![
                CustomOpenAiMessage {
                    role: CustomOpenAiRole::User,
                    content_type: CustomOpenAiContentType::OutputText,
                    text: turn1.to_string(),
                },
                CustomOpenAiMessage {
                    role: CustomOpenAiRole::Assistant,
                    content_type: CustomOpenAiContentType::OutputText,
                    text: "It is an enchantment land.".to_string(),
                },
                CustomOpenAiMessage {
                    role: CustomOpenAiRole::User,
                    content_type: CustomOpenAiContentType::OutputText,
                    text: turn2.to_string(),
                },
                CustomOpenAiMessage {
                    role: CustomOpenAiRole::Assistant,
                    content_type: CustomOpenAiContentType::OutputText,
                    text: "Chapter II creates tokens.".to_string(),
                },
            ],
        };
        let decision = route_prompt_2d(&prompt);
        assert_eq!(decision.primary, Route2D::SearchFast);
        assert!(decision.needs_search());
    }
}
