use serde_json::Value;
use tauri::{plugin::Builder, Runtime};

#[derive(Debug, Clone, serde::Serialize, PartialEq, Eq)]
pub struct StructuredPluginInvokeError {
    pub code: String,
    pub message: String,
}

impl StructuredPluginInvokeError {
    fn bounded(code: Option<&str>, message: Option<&str>) -> Self {
        let code = code.filter(|code| {
            matches!(
                *code,
                "invalid_input" | "not_found" | "busy" | "unavailable" | "internal"
            )
        });
        let message = message.filter(|message| !message.is_empty() && message.len() <= 512);
        match (code, message) {
            (Some(code), Some(message)) => Self {
                code: code.into(),
                message: message.into(),
            },
            _ => Self {
                code: "internal".into(),
                message: "Native command failed".into(),
            },
        }
    }
}

#[cfg(any(target_os = "android", test))]
struct InputRoutes<T> {
    current: Option<(String, T)>,
    pending: Option<(u64, String, T)>,
    sequence: u64,
}

#[cfg(any(target_os = "android", test))]
impl<T> Default for InputRoutes<T> {
    fn default() -> Self {
        Self {
            current: None,
            pending: None,
            sequence: 0,
        }
    }
}

#[cfg(any(target_os = "android", test))]
impl<T> InputRoutes<T> {
    fn begin(&mut self, session_id: String, sink: T) -> Option<u64> {
        if self.pending.is_some()
            || self
                .current
                .as_ref()
                .is_some_and(|(id, _)| *id == session_id)
        {
            return None;
        }
        self.sequence = self.sequence.checked_add(1)?;
        self.pending = Some((self.sequence, session_id, sink));
        Some(self.sequence)
    }

    fn get(&self, session_id: &str) -> Option<&T> {
        self.pending
            .as_ref()
            .filter(|(_, id, _)| id == session_id)
            .map(|(_, _, sink)| sink)
            .or_else(|| {
                self.current
                    .as_ref()
                    .filter(|(id, _)| id == session_id)
                    .map(|(_, sink)| sink)
            })
    }

    fn complete(&mut self, ticket: u64, accepted: bool) {
        if self.pending.as_ref().is_none_or(|(id, _, _)| *id != ticket) {
            return;
        }
        let Some((_, id, sink)) = self.pending.take() else {
            return;
        };
        if accepted {
            self.current = Some((id, sink));
        }
    }

    fn clear(&mut self) {
        self.pending = None;
        self.current = None;
    }
}

#[cfg(target_os = "android")]
mod android {
    use super::*;
    use std::sync::{Arc, Mutex};
    use tauri::{
        ipc::{Channel, InvokeResponseBody},
        Manager,
    };

    struct Sink {
        request_id: String,
        updates: Channel<Value>,
        failed: bool,
    }
    pub(super) struct Bridge<R: Runtime> {
        pub handle: tauri::plugin::PluginHandle<R>,
        channel: Channel<Value>,
        sink: Arc<Mutex<Option<Sink>>>,
        input_channel: Channel<Value>,
        input_sink: Arc<Mutex<InputRoutes<Channel<Value>>>>,
    }

    fn invoke_error(
        error: tauri::plugin::mobile::PluginInvokeError,
    ) -> StructuredPluginInvokeError {
        match error {
            tauri::plugin::mobile::PluginInvokeError::InvokeRejected(error) => {
                StructuredPluginInvokeError::bounded(
                    error.code.as_deref(),
                    error.message.as_deref(),
                )
            }
            _ => StructuredPluginInvokeError::bounded(None, None),
        }
    }

    pub(super) fn setup<R: Runtime>(
        app: &tauri::AppHandle<R>,
        handle: tauri::plugin::PluginHandle<R>,
    ) {
        let sink: Arc<Mutex<Option<Sink>>> = Arc::new(Mutex::new(None));
        let forwarding = sink.clone();
        let app_handle = app.clone();
        // One registered native Channel per process: Tauri retains registered mobile channels.
        let channel = Channel::new(move |body| {
            let payload: Value = match body {
                InvokeResponseBody::Json(raw) => serde_json::from_str(&raw).unwrap_or(Value::Null),
                InvokeResponseBody::Raw(_) => Value::Null,
            };
            let request_id = payload
                .get("requestId")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned();
            let revision = payload
                .get("revision")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned();
            let provider_request_id = payload
                .get("providerRequestId")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned();
            let accepted = forwarding
                .lock()
                .ok()
                .and_then(|mut current| {
                    let sink = current.as_mut()?;
                    let updates = sink.updates.clone();
                    Some(forward_update(
                        &payload,
                        &sink.request_id,
                        &mut sink.failed,
                        |update| updates.send(update).is_ok(),
                    ))
                })
                .unwrap_or(false);
            let app = app_handle.clone();
            let _ = tauri::async_runtime::spawn(async move {
                let result: Result<Value, _> = app.state::<Bridge<R>>().handle.run_mobile_plugin_async(
                    "ack_delivery", serde_json::json!({ "requestId": request_id, "providerRequestId": provider_request_id, "revision": revision, "accepted": accepted }),
                ).await;
                // A lost ACK is handled by the native bounded delivery gate; never assume delivery.
                let _ = result;
            });
            Ok(())
        });
        let input_sink = Arc::new(Mutex::new(InputRoutes::<Channel<Value>>::default()));
        let input_forwarding = input_sink.clone();
        let input_app = app.clone();
        let input_channel = Channel::new(move |body| {
            let payload: Value = match body {
                InvokeResponseBody::Json(raw) => serde_json::from_str(&raw).unwrap_or(Value::Null),
                InvokeResponseBody::Raw(_) => Value::Null,
            };
            let failed = input_forwarding.lock().ok().is_some_and(|mut sink| {
                let Some(session_id) = payload.get("sessionId").and_then(Value::as_str) else {
                    return false;
                };
                let Some(updates) = sink.get(session_id) else {
                    return false;
                };
                if updates.send(payload).is_ok() {
                    false
                } else {
                    sink.clear();
                    true
                }
            });
            if failed {
                let app = input_app.clone();
                let _ = tauri::async_runtime::spawn(async move {
                    let _ = forward(app, "stop_assistant_voice", serde_json::json!({})).await;
                });
            }
            Ok(())
        });
        app.manage(Bridge {
            handle,
            channel,
            sink,
            input_channel,
            input_sink,
        });
    }

    pub(super) async fn forward<R: Runtime>(
        app: tauri::AppHandle<R>,
        command: &str,
        arguments: Value,
    ) -> Result<Value, StructuredPluginInvokeError> {
        if command == "stop_assistant_voice" {
            if let Ok(mut sink) = app.state::<Bridge<R>>().input_sink.lock() {
                sink.clear();
            }
        }
        app.state::<Bridge<R>>()
            .handle
            .run_mobile_plugin_async(command, arguments)
            .await
            .map_err(invoke_error)
    }

    pub(super) async fn start_input<R: Runtime>(
        app: tauri::AppHandle<R>,
        session_id: String,
        loop_mode: bool,
        draft: String,
        updates: Channel<Value>,
        completion_policy: Option<String>,
        launch_token: Option<String>,
    ) -> Result<Value, StructuredPluginInvokeError> {
        let bridge = app.state::<Bridge<R>>();
        let ticket = bridge
            .input_sink
            .lock()
            .map_err(|_| StructuredPluginInvokeError::bounded(None, None))?
            .begin(session_id.clone(), updates)
            .ok_or_else(|| {
                StructuredPluginInvokeError::bounded(
                    Some("busy"),
                    Some("Voice admission is already pending"),
                )
            })?;
        let result = bridge.handle.run_mobile_plugin_async("start_assistant_voice", serde_json::json!({ "sessionId": session_id, "loopMode": loop_mode, "draft": draft, "updates": bridge.input_channel, "completionPolicy": completion_policy.unwrap_or_else(|| "manual".into()), "launchToken": launch_token })).await.map_err(invoke_error);
        if let Ok(mut sink) = bridge.input_sink.lock() {
            sink.complete(ticket, result.is_ok());
        }
        result
    }

    pub(super) async fn submit<R: Runtime>(
        app: tauri::AppHandle<R>,
        request_id: String,
        prompt: String,
        updates: Channel<Value>,
        search_mode: Option<String>,
    ) -> Result<Value, StructuredPluginInvokeError> {
        let bridge = app.state::<Bridge<R>>();
        {
            let mut sink = bridge
                .sink
                .lock()
                .map_err(|_| StructuredPluginInvokeError::bounded(None, None))?;
            if sink.is_some() {
                return Err(StructuredPluginInvokeError::bounded(
                    Some("busy"),
                    Some("A request is already active"),
                ));
            }
            *sink = Some(Sink {
                request_id: request_id.clone(),
                updates,
                failed: false,
            });
        }
        struct Lease<R: Runtime> {
            sink: Arc<Mutex<Option<Sink>>>,
            app: tauri::AppHandle<R>,
            request_id: String,
            armed: bool,
        }
        impl<R: Runtime> Drop for Lease<R> {
            fn drop(&mut self) {
                if let Ok(mut sink) = self.sink.lock() {
                    let _ = sink.take();
                }
                if self.armed {
                    let app = self.app.clone();
                    let request_id = self.request_id.clone();
                    let _ = tauri::async_runtime::spawn(async move {
                        let _ = forward(
                            app,
                            "cancel_prompt",
                            serde_json::json!({ "requestId": request_id }),
                        )
                        .await;
                    });
                }
            }
        }
        let mut lease = Lease {
            sink: bridge.sink.clone(),
            app: app.clone(),
            request_id: request_id.clone(),
            armed: true,
        };
        let result = bridge
            .handle
            .run_mobile_plugin_async(
                "submit_prompt",
                serde_json::json!({
                    "requestId": request_id, "prompt": prompt, "updates": bridge.channel,
                    "searchMode": search_mode,
                }),
            )
            .await
            .map_err(invoke_error);
        lease.armed = false;
        result
    }
}

#[cfg(any(target_os = "android", test))]
fn valid_revision(value: &str) -> bool {
    value
        .parse::<u64>()
        .is_ok_and(|revision| revision > 0 && revision.to_string() == value)
}

#[cfg(any(target_os = "android", test))]
fn forward_update(
    payload: &Value,
    expected_request: &str,
    failed: &mut bool,
    send: impl FnOnce(Value) -> bool,
) -> bool {
    if payload.get("requestId").and_then(Value::as_str) != Some(expected_request) {
        return false;
    }
    let revision = payload
        .get("revision")
        .and_then(Value::as_str)
        .unwrap_or("");
    let provider = payload
        .get("providerRequestId")
        .and_then(Value::as_str)
        .unwrap_or("");
    let update = payload.get("update");
    let valid_update =
        update.is_some_and(|update| match update.get("kind").and_then(Value::as_str) {
            Some("delta") => update
                .get("text")
                .and_then(Value::as_str)
                .is_some_and(|text| text.len() <= 256 * 1024),
            Some("status") => update
                .get("message")
                .and_then(Value::as_str)
                .is_some_and(|text| text.len() <= 512),
            Some("custom_direct_timings") => {
                update
                    .get("first_text_ms")
                    .and_then(Value::as_u64)
                    .is_some()
                    && update.get("completed_ms").and_then(Value::as_u64).is_some()
            }
            _ => false,
        });
    let accepted = !*failed
        && valid_revision(revision)
        && !provider.is_empty()
        && provider.len() <= 128
        && valid_update
        && update.is_some_and(|update| send(update.clone()));
    *failed |= !accepted;
    accepted
}

pub async fn forward<R: Runtime>(
    app: tauri::AppHandle<R>,
    command: &str,
    arguments: Value,
) -> Result<Value, StructuredPluginInvokeError> {
    #[cfg(target_os = "android")]
    {
        android::forward(app, command, arguments).await
    }
    #[cfg(not(target_os = "android"))]
    {
        let _ = (app, command, arguments);
        Err(StructuredPluginInvokeError::bounded(
            Some("unavailable"),
            Some("Assistant mobile bridge is available only on Android"),
        ))
    }
}

pub async fn submit<R: Runtime>(
    app: tauri::AppHandle<R>,
    request_id: String,
    prompt: String,
    updates: tauri::ipc::Channel<Value>,
) -> Result<Value, StructuredPluginInvokeError> {
    submit_with_search_mode(app, request_id, prompt, updates, None).await
}

pub async fn submit_with_search_mode<R: Runtime>(
    app: tauri::AppHandle<R>,
    request_id: String,
    prompt: String,
    updates: tauri::ipc::Channel<Value>,
    search_mode: Option<String>,
) -> Result<Value, StructuredPluginInvokeError> {
    #[cfg(target_os = "android")]
    {
        android::submit(app, request_id, prompt, updates, search_mode).await
    }
    #[cfg(not(target_os = "android"))]
    {
        let _ = (app, request_id, prompt, updates, search_mode);
        Err(StructuredPluginInvokeError::bounded(
            Some("unavailable"),
            Some("Assistant mobile bridge is available only on Android"),
        ))
    }
}

pub async fn start_input<R: Runtime>(
    app: tauri::AppHandle<R>,
    session_id: String,
    loop_mode: bool,
    draft: String,
    updates: tauri::ipc::Channel<Value>,
    completion_policy: Option<String>,
    launch_token: Option<String>,
) -> Result<Value, StructuredPluginInvokeError> {
    #[cfg(target_os = "android")]
    {
        android::start_input(
            app,
            session_id,
            loop_mode,
            draft,
            updates,
            completion_policy,
            launch_token,
        )
        .await
    }
    #[cfg(not(target_os = "android"))]
    {
        let _ = (
            app,
            session_id,
            loop_mode,
            draft,
            updates,
            completion_policy,
            launch_token,
        );
        Err(StructuredPluginInvokeError::bounded(
            Some("unavailable"),
            Some("Native Assistant input is available only on Android"),
        ))
    }
}

pub fn init<R: Runtime>() -> tauri::plugin::TauriPlugin<R> {
    Builder::new("assistant")
        .setup(|app, api| {
            #[cfg(target_os = "android")]
            android::setup(
                app,
                api.register_android_plugin(
                    "dev.gitblame.voxgolem.assistant.phone",
                    "AssistantPlugin",
                )?,
            );
            #[cfg(not(target_os = "android"))]
            let _ = (app, api);
            Ok(())
        })
        .build()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_codes_survive_without_display_formatting() {
        let value =
            StructuredPluginInvokeError::bounded(Some("busy"), Some("Synthetic busy response"));
        assert_eq!(
            serde_json::to_value(value).unwrap(),
            serde_json::json!({"code":"busy", "message":"Synthetic busy response"})
        );
    }

    #[test]
    fn malformed_rejections_are_bounded_and_redacted() {
        for (code, message) in [
            (Some("secret_code"), Some("unexpected")),
            (Some("internal"), Some("")),
            (None, Some("unexpected")),
        ] {
            assert_eq!(
                StructuredPluginInvokeError::bounded(code, message).message,
                "Native command failed"
            );
        }
        assert_eq!(
            StructuredPluginInvokeError::bounded(Some("internal"), Some(&"é".repeat(257))).message,
            "Native command failed"
        );
    }

    #[test]
    fn delivery_revision_is_positive_canonical_u64() {
        for invalid in ["", "0", "01", "-1", "18446744073709551616"] {
            assert!(!valid_revision(invalid));
        }
        assert!(valid_revision("18446744073709551615"));
    }

    #[test]
    fn failed_javascript_sink_negatively_acknowledges_and_stays_failed() {
        let payload = serde_json::json!({"requestId":"synthetic-request", "providerRequestId":"synthetic-provider", "revision":"1", "update":{"kind":"delta", "text":"Synthetic delta"}});
        let mut failed = false;
        assert!(!forward_update(
            &payload,
            "synthetic-request",
            &mut failed,
            |_| false
        ));
        assert!(failed);
        assert!(!forward_update(
            &payload,
            "synthetic-request",
            &mut failed,
            |_| panic!("failed sink must not publish again")
        ));
    }

    #[test]
    fn stale_packets_cannot_fail_a_new_request_or_reach_its_sink() {
        let payload = serde_json::json!({"requestId":"old-synthetic-request"});
        let mut failed = false;
        assert!(!forward_update(
            &payload,
            "new-synthetic-request",
            &mut failed,
            |_| panic!("stale packet must not reach sink")
        ));
        assert!(!failed);
    }

    #[test]
    fn rejected_voice_admission_preserves_the_existing_event_route() {
        let mut routes = InputRoutes::<String>::default();
        let first = routes
            .begin("synthetic-active".into(), "old-sink".into())
            .unwrap();
        routes.complete(first, true);
        let attempted = routes
            .begin("synthetic-busy".into(), "candidate-sink".into())
            .unwrap();
        assert_eq!(
            routes.get("synthetic-active").map(String::as_str),
            Some("old-sink")
        );
        assert_eq!(
            routes.get("synthetic-busy").map(String::as_str),
            Some("candidate-sink")
        );
        assert!(routes
            .begin("synthetic-third".into(), "ignored-sink".into())
            .is_none());
        routes.complete(attempted, false);
        assert_eq!(
            routes.get("synthetic-active").map(String::as_str),
            Some("old-sink")
        );
        assert!(routes.get("synthetic-busy").is_none());
    }

    #[test]
    fn stopped_or_superseded_voice_admission_cannot_restore_a_stale_route() {
        let mut routes = InputRoutes::<String>::default();
        let old = routes
            .begin("synthetic-session".into(), "old-sink".into())
            .unwrap();
        routes.clear();
        let new = routes
            .begin("synthetic-session".into(), "new-sink".into())
            .unwrap();
        routes.complete(old, true);
        assert_eq!(
            routes.get("synthetic-session").map(String::as_str),
            Some("new-sink")
        );
        routes.complete(new, true);
        assert!(routes
            .begin("synthetic-session".into(), "replacement-sink".into())
            .is_none());
        let next = routes
            .begin("synthetic-next".into(), "next-sink".into())
            .unwrap();
        routes.complete(next, true);
        assert!(routes.get("synthetic-session").is_none());
        assert_eq!(
            routes.get("synthetic-next").map(String::as_str),
            Some("next-sink")
        );
    }
}
