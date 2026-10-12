mod cancellation;
mod domain;
mod outcome;
mod request;
mod search;

#[cfg(test)]
mod domain_tests;

pub use domain::{apply_json, MAX_GATEWAY_BYTES, MAX_NATIVE_SNAPSHOT_BYTES};

use jni::objects::{JObject, JString, JValue};
use jni::sys::jstring;
use jni::JNIEnv;
use outcome::Outcome;
use std::borrow::Cow;
use std::cell::Cell;
use std::future::Future;
use std::io::Read;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Component, Path};
use tokio::sync::watch;
use voxgolem_platform::custom_openai::{
    custom_openai_auth_revision, observe_custom_openai_auth, CustomOpenAiAuthObservation,
    CustomOpenAiAuthRevision, CustomOpenAiClient, CustomOpenAiConfig, CustomOpenAiError,
    CustomOpenAiResponse, MAX_CUSTOM_OPENAI_AUTH_BYTES,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
enum ErrorCode {
    InvalidRequest,
    DuplicateRequest,
    Busy,
    AuthUnavailable,
    AuthExpired,
    Unauthorized,
    Forbidden,
    RateLimited,
    Timeout,
    Transport,
    ProviderHttpError,
    InvalidResponse,
    TooLarge,
    CallbackFailed,
    InternalError,
}

impl From<CustomOpenAiError> for ErrorCode {
    fn from(error: CustomOpenAiError) -> Self {
        match error {
            CustomOpenAiError::AuthUnavailable => Self::AuthUnavailable,
            CustomOpenAiError::AuthExpired => Self::AuthExpired,
            CustomOpenAiError::Unauthorized => Self::Unauthorized,
            CustomOpenAiError::Forbidden => Self::Forbidden,
            CustomOpenAiError::RateLimited => Self::RateLimited,
            CustomOpenAiError::Timeout => Self::Timeout,
            CustomOpenAiError::Transport => Self::Transport,
            CustomOpenAiError::Http(_) => Self::ProviderHttpError,
            CustomOpenAiError::TooLarge => Self::TooLarge,
            CustomOpenAiError::InvalidConfig | CustomOpenAiError::InvalidPrompt => {
                Self::InvalidRequest
            }
            CustomOpenAiError::UnexpectedContentType
            | CustomOpenAiError::MalformedStream
            | CustomOpenAiError::MixedContent
            | CustomOpenAiError::EmptyResponse
            | CustomOpenAiError::Incomplete
            | CustomOpenAiError::Failed => Self::InvalidResponse,
        }
    }
}

fn provider_config(auth_path: &str, request: &request::Request) -> CustomOpenAiConfig {
    CustomOpenAiConfig {
        auth_path: auth_path.into(),
        model: request.model,
        ..Default::default()
    }
}

fn valid_auth_path(auth_path: &str) -> bool {
    let path = Path::new(auth_path);
    auth_path.len() <= 4096
        && !auth_path.contains('\0')
        && path.is_absolute()
        && path.file_name().is_some()
        && !path.components().any(|part| part == Component::ParentDir)
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyEmptyAuth {
    #[serde(rename = "_assistant")]
    metadata: LegacyEmptyMetadata,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyEmptyMetadata {
    version: u32,
    #[serde(rename = "installationId")]
    installation_id: String,
    generation: String,
    #[serde(default, rename = "association")]
    _association: Option<serde_json::Value>,
    #[serde(default, rename = "pending")]
    _pending: Option<serde_json::Value>,
    #[serde(default, rename = "incoming")]
    _incoming: Option<serde_json::Value>,
}

fn auth_source_kind(path: &Path, observation: &CustomOpenAiAuthObservation) -> &'static str {
    if observation.status != Err(CustomOpenAiError::AuthUnavailable) {
        return "provider";
    }
    let metadata = match std::fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return "absent",
        Ok(metadata) if metadata.is_file() => metadata,
        _ => return "provider",
    };
    if metadata.len() > MAX_CUSTOM_OPENAI_AUTH_BYTES
        || observation.revision.is_none()
        || custom_openai_auth_revision(path) != observation.revision
    {
        return "provider";
    }
    let empty = (|| {
        let file = std::fs::File::open(path).ok()?;
        let mut bytes = Vec::new();
        file.take(MAX_CUSTOM_OPENAI_AUTH_BYTES + 1)
            .read_to_end(&mut bytes)
            .ok()?;
        if bytes.len() as u64 > MAX_CUSTOM_OPENAI_AUTH_BYTES {
            return None;
        }
        let legacy: LegacyEmptyAuth = serde_json::from_slice(&bytes).ok()?;
        (legacy.metadata.version == 1
            && legacy.metadata.generation == "0"
            && voxgolem_core::durable_conversation::validate_replica(
                &legacy.metadata.installation_id,
            )
            .is_ok()
            && custom_openai_auth_revision(path) == observation.revision)
            .then_some(())
    })();
    if empty.is_some() {
        "legacy_empty"
    } else {
        "provider"
    }
}

fn auth_revision_string(revision: CustomOpenAiAuthRevision) -> String {
    format!("{revision:?}")
}

fn observe_auth_json(auth_path: &str) -> String {
    let path = Path::new(auth_path);
    let observation = valid_auth_path(auth_path).then(|| observe_custom_openai_auth(path));
    let source_kind = observation
        .as_ref()
        .map(|observation| auth_source_kind(path, observation))
        .unwrap_or("provider");
    let revision = observation
        .as_ref()
        .and_then(|value| value.revision)
        .map(auth_revision_string);
    let code = observation
        .map(|value| value.status)
        .unwrap_or(Err(CustomOpenAiError::AuthUnavailable))
        .err()
        .map(ErrorCode::from);
    serde_json::json!({ "revision": revision, "code": code, "sourceKind": source_kind }).to_string()
}

#[allow(unsafe_code)]
#[no_mangle]
pub extern "system" fn Java_dev_gitblame_voxgolem_assistant_runtime_AssistantProviderNative_observeAuth(
    mut env: JNIEnv<'_>,
    _receiver: JObject<'_>,
    auth_path: JString<'_>,
) -> jstring {
    let output = catch_unwind(AssertUnwindSafe(|| {
        read_string(&mut env, &auth_path, 4096)
            .map(|path| observe_auth_json(&path))
            .unwrap_or_else(|_| observe_auth_json(""))
    }))
    .unwrap_or_else(|_| observe_auth_json(""));
    clear_exception(&env);
    env.new_string(output)
        .map(JString::into_raw)
        .unwrap_or(std::ptr::null_mut())
}

async fn await_response(
    response: impl Future<Output = Result<CustomOpenAiResponse, CustomOpenAiError>>,
    mut cancelled: watch::Receiver<bool>,
    mut callback_failure: watch::Receiver<Option<ErrorCode>>,
) -> Outcome {
    tokio::select! {
        biased;
        failure = async { callback_failure.wait_for(Option::is_some).await.map(|value| *value) } => {
            Outcome::error(failure.ok().flatten().unwrap_or(ErrorCode::CallbackFailed))
        }
        _ = async { cancelled.wait_for(|value| *value).await.map(|_| ()) } => Outcome::Cancelled,
        result = response => {
            if let Some(error) = *callback_failure.borrow() {
                Outcome::error(error)
            } else if *cancelled.borrow() {
                Outcome::Cancelled
            } else {
                Outcome::from_response(result)
            }
        }
    }
}

async fn await_authenticated_response(
    response: impl Future<Output = Result<CustomOpenAiResponse, CustomOpenAiError>>,
    revision: &Cell<Option<CustomOpenAiAuthRevision>>,
    cancelled: watch::Receiver<bool>,
    callback_failure: watch::Receiver<Option<ErrorCode>>,
) -> Outcome {
    await_response(response, cancelled, callback_failure)
        .await
        .with_auth_revision(revision.get().map(auth_revision_string))
}

fn respond(
    request_id: &str,
    auth_path: &str,
    input: &str,
    mut callback: impl FnMut(CallbackEvent<'_>) -> Result<(), ErrorCode>,
) -> Outcome {
    let request = match request::parse(input) {
        Ok(request) => request,
        Err(error) => return Outcome::error(error),
    };
    if request_id != request.request_id || !valid_auth_path(auth_path) {
        return Outcome::error(ErrorCode::InvalidRequest);
    }
    let registration = match cancellation::registry().register(&request.request_id) {
        Ok(registration) => registration,
        Err(error) => return Outcome::error(error),
    };
    if let Err(error) = callback(CallbackEvent::Ready) {
        return Outcome::error(error);
    }
    let process = match search::process() {
        Ok(process) => process,
        Err(error) => return Outcome::error(error),
    };
    process.runtime.block_on(async {
        if *registration.cancelled.borrow() {
            return Outcome::Cancelled;
        }
        if request.search_mode.is_some() {
            return search::respond(
                process,
                provider_config(auth_path, &request),
                &request,
                registration.cancelled.clone(),
                &mut callback,
            )
            .await;
        }
        let client = match process.client(provider_config(auth_path, &request)) {
            Ok(client) => client,
            Err(error) => return Outcome::error(error),
        };
        direct_response(
            &client,
            &request,
            registration.cancelled.clone(),
            &mut callback,
        )
        .await
    })
}

async fn direct_response(
    client: &CustomOpenAiClient,
    request: &request::Request,
    cancelled: watch::Receiver<bool>,
    callback: &mut impl FnMut(CallbackEvent<'_>) -> Result<(), ErrorCode>,
) -> Outcome {
    let (failure_tx, failure_rx) = watch::channel(None);
    let delta_cancelled = cancelled.clone();
    let mut delivered_bytes = 0_usize;
    let auth_revision = Cell::new(None);
    let first_text = FirstTextTiming::default();
    let direct_instructions = format!(
            "{}\n\nNo tools are available for this response. Web access is {}. Answer ordinary chat directly from existing knowledge. If current lookup or verification is requested while web access is disabled, clearly explain that it cannot be verified; do not claim a lookup or invent retrieved evidence.",
            request.instructions,
            if request.web_enabled { "not used on this direct route" } else { "disabled" }
        );
    let response = client.respond_with_auth_revision_and_dispatch(
        &request.prompt,
        Some(&direct_instructions),
        |delta| {
            if *delta_cancelled.borrow() || failure_tx.borrow().is_some() {
                return;
            }
            let result = deliver_timed_delta(
                &mut delivered_bytes,
                &first_text,
                delta,
                || *delta_cancelled.borrow(),
                callback,
            );
            if let Err(error) = result {
                let _ = failure_tx.send(Some(error));
            }
        },
        |revision| auth_revision.set(Some(revision)),
        |dispatched| first_text.dispatched(dispatched),
    );
    await_authenticated_response(response, &auth_revision, cancelled, failure_rx).await
}

fn deliver_bounded_delta(
    delivered_bytes: &mut usize,
    delta: &str,
    deliver: impl FnOnce() -> Result<(), ErrorCode>,
) -> Result<(), ErrorCode> {
    *delivered_bytes = delivered_bytes.saturating_add(delta.len());
    if *delivered_bytes > request::MAX_OUTPUT_BYTES {
        Err(ErrorCode::TooLarge)
    } else {
        deliver()
    }
}

enum CallbackEvent<'a> {
    Ready,
    FirstText(u64),
    Delta(&'a str),
    Status(&'static str),
}

#[derive(Default)]
struct FirstTextTiming {
    dispatched: Cell<Option<std::time::Instant>>,
    delivered: Cell<bool>,
}

impl FirstTextTiming {
    fn dispatched(&self, instant: std::time::Instant) {
        if self.dispatched.get().is_none() {
            self.dispatched.set(Some(instant));
        }
    }

    fn deliver(
        &self,
        text: &str,
        callback: &mut impl FnMut(CallbackEvent<'_>) -> Result<(), ErrorCode>,
    ) -> Result<(), ErrorCode> {
        if !self.delivered.get() && text.chars().any(|character| !character.is_whitespace()) {
            if let Some(dispatched) = self.dispatched.get() {
                let milliseconds = dispatched.elapsed().as_millis().min(i64::MAX as u128) as u64;
                callback(CallbackEvent::FirstText(milliseconds))?;
                self.delivered.set(true);
            }
        }
        Ok(())
    }
}

fn deliver_timed_delta(
    bytes: &mut usize,
    timing: &FirstTextTiming,
    text: &str,
    cancelled: impl Fn() -> bool,
    callback: &mut impl FnMut(CallbackEvent<'_>) -> Result<(), ErrorCode>,
) -> Result<(), ErrorCode> {
    if cancelled() {
        return Ok(());
    }
    deliver_bounded_delta(bytes, text, || {
        timing.deliver(text, callback)?;
        if cancelled() {
            return Ok(());
        }
        callback(CallbackEvent::Delta(text))
    })
}

fn read_string(
    env: &mut JNIEnv<'_>,
    value: &JString<'_>,
    max_bytes: usize,
) -> Result<String, ErrorCode> {
    read_bounded_string(env, value, max_bytes).map_err(|_| ErrorCode::InvalidRequest)
}

enum StringReadError {
    Invalid,
    TooLarge,
}

fn read_bounded_string(
    env: &mut JNIEnv<'_>,
    value: &JString<'_>,
    max_bytes: usize,
) -> Result<String, StringReadError> {
    if value.is_null() {
        return Err(StringReadError::Invalid);
    }
    let length = env
        .call_method(value, "length", "()I", &[])
        .and_then(|value| value.i())
        .map_err(|_| StringReadError::Invalid)?;
    if length < 0 {
        return Err(StringReadError::Invalid);
    }
    if length as usize > max_bytes {
        return Err(StringReadError::TooLarge);
    }
    let java_string = env
        .get_string(value)
        .map_err(|_| StringReadError::Invalid)?;
    decode_jni_string(&java_string, max_bytes).map_err(|_| StringReadError::TooLarge)
}

fn decode_jni_string(value: &jni::strings::JNIStr, max_bytes: usize) -> Result<String, ErrorCode> {
    let decoded: Cow<'_, str> = value.into();
    if decoded.len() > max_bytes {
        return Err(ErrorCode::InvalidRequest);
    }
    Ok(decoded.into_owned())
}

fn clear_exception(env: &JNIEnv<'_>) {
    if env.exception_check().unwrap_or(false) {
        let _ = env.exception_clear();
    }
}

fn apply_jni(mut env: JNIEnv<'_>, request: JString<'_>) -> jstring {
    let output = catch_unwind(AssertUnwindSafe(|| {
        match read_bounded_string(&mut env, &request, MAX_GATEWAY_BYTES) {
            Ok(input) => apply_json(&input),
            Err(StringReadError::Invalid) => {
                domain::failure(domain::invalid("native_request_string"))
            }
            Err(StringReadError::TooLarge) => {
                domain::failure(domain::limit("native_request_bytes"))
            }
        }
    }))
    .unwrap_or_else(|_| domain::failure(domain::invalid("native_internal")));
    clear_exception(&env);
    env.new_string(output)
        .map(JString::into_raw)
        .unwrap_or(std::ptr::null_mut())
}

#[allow(unsafe_code)]
#[no_mangle]
pub extern "system" fn Java_dev_gitblame_voxgolem_assistant_AssistantDomainNative_apply(
    env: JNIEnv<'_>,
    _receiver: JObject<'_>,
    request: JString<'_>,
) -> jstring {
    apply_jni(env, request)
}

#[allow(unsafe_code)]
#[no_mangle]
pub extern "system" fn Java_dev_gitblame_voxgolem_android_wear_assistant_WearAssistantNative_apply(
    env: JNIEnv<'_>,
    _receiver: JObject<'_>,
    request: JString<'_>,
) -> jstring {
    apply_jni(env, request)
}

fn respond_jni(
    mut env: JNIEnv<'_>,
    _receiver: JObject<'_>,
    request_id: JString<'_>,
    auth_path: JString<'_>,
    request_json: JString<'_>,
    callback: JObject<'_>,
) -> jstring {
    let outcome = catch_unwind(AssertUnwindSafe(|| {
        if callback.is_null() {
            return Outcome::error(ErrorCode::InvalidRequest);
        }
        let request_id = match read_string(&mut env, &request_id, request::MAX_REQUEST_ID_BYTES) {
            Ok(id) => id,
            Err(error) => return Outcome::error(error),
        };
        let auth_path = match read_string(&mut env, &auth_path, 4096) {
            Ok(path) => path,
            Err(error) => return Outcome::error(error),
        };
        let input = match read_string(&mut env, &request_json, request::MAX_REQUEST_BYTES) {
            Ok(input) => input,
            Err(error) => return Outcome::error(error),
        };
        respond(&request_id, &auth_path, &input, |event| {
            let delivered: jni::errors::Result<()> = env.with_local_frame(2, |env| {
                match event {
                    CallbackEvent::Ready => {
                        env.call_method(&callback, "onReady", "()V", &[])?;
                    }
                    CallbackEvent::FirstText(milliseconds) => {
                        env.call_method(
                            &callback,
                            "onFirstText",
                            "(J)V",
                            &[JValue::Long(milliseconds as i64)],
                        )?;
                    }
                    CallbackEvent::Delta(delta) | CallbackEvent::Status(delta) => {
                        let text = env.new_string(delta)?;
                        env.call_method(
                            &callback,
                            if matches!(event, CallbackEvent::Status(_)) {
                                "onStatus"
                            } else {
                                "onDelta"
                            },
                            "(Ljava/lang/String;)V",
                            &[JValue::Object(text.as_ref())],
                        )?;
                    }
                }
                Ok(())
            });
            if delivered.is_err() {
                clear_exception(&env);
                Err(ErrorCode::CallbackFailed)
            } else {
                Ok(())
            }
        })
    }))
    .unwrap_or_else(|_| Outcome::error(ErrorCode::InternalError));
    clear_exception(&env);
    env.new_string(outcome.json())
        .map(JString::into_raw)
        .unwrap_or(std::ptr::null_mut())
}

#[allow(unsafe_code)]
#[no_mangle]
pub extern "system" fn Java_dev_gitblame_voxgolem_android_wear_assistant_WearAssistantNative_cancel(
    env: JNIEnv<'_>,
    receiver: JObject<'_>,
    request_id: JString<'_>,
) {
    cancel_jni(env, receiver, request_id);
}

fn cancel_jni(mut env: JNIEnv<'_>, _receiver: JObject<'_>, request_id: JString<'_>) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        if let Ok(request_id) = read_string(&mut env, &request_id, request::MAX_REQUEST_ID_BYTES) {
            if request::valid_request_id(&request_id) {
                cancellation::registry().cancel(&request_id);
            }
        }
    }));
    clear_exception(&env);
}

#[allow(unsafe_code)]
#[no_mangle]
pub extern "system" fn Java_dev_gitblame_voxgolem_android_wear_assistant_WearAssistantNative_respond(
    env: JNIEnv<'_>,
    receiver: JObject<'_>,
    request_id: JString<'_>,
    auth_path: JString<'_>,
    request_json: JString<'_>,
    callback: JObject<'_>,
) -> jstring {
    respond_jni(env, receiver, request_id, auth_path, request_json, callback)
}

#[allow(unsafe_code)]
#[no_mangle]
pub extern "system" fn Java_dev_gitblame_voxgolem_assistant_runtime_AssistantProviderNative_respond(
    env: JNIEnv<'_>,
    receiver: JObject<'_>,
    request_id: JString<'_>,
    auth_path: JString<'_>,
    request_json: JString<'_>,
    callback: JObject<'_>,
) -> jstring {
    respond_jni(env, receiver, request_id, auth_path, request_json, callback)
}

#[allow(unsafe_code)]
#[no_mangle]
pub extern "system" fn Java_dev_gitblame_voxgolem_assistant_runtime_AssistantProviderNative_cancel(
    env: JNIEnv<'_>,
    receiver: JObject<'_>,
    request_id: JString<'_>,
) {
    cancel_jni(env, receiver, request_id);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    #[test]
    fn auth_observation_is_bounded_and_uses_portable_provider_errors() {
        for path in [
            "relative/auth.json",
            "/synthetic/../auth.json",
            "/synthetic/missing/auth.json",
        ] {
            let observation: serde_json::Value =
                serde_json::from_str(&observe_auth_json(path)).unwrap();
            assert_eq!(observation["code"], "auth_unavailable");
            assert!(observation["revision"].is_null());
            assert_eq!(observation.as_object().unwrap().len(), 3);
        }
    }

    struct AuthFixture(std::path::PathBuf);

    impl AuthFixture {
        fn new() -> Self {
            let nonce = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let root = std::env::temp_dir().join(format!(
                "vox-native-synthetic-auth-{}-{nonce}",
                std::process::id()
            ));
            std::fs::create_dir(&root).unwrap();
            Self(root)
        }

        fn path(&self) -> std::path::PathBuf {
            self.0.join("assistant-auth.json")
        }

        fn replace(&self, access: &str) {
            let staged = self.0.join("synthetic-external-stage");
            std::fs::write(
                &staged,
                serde_json::json!({"openai":{"access":access,"expires":4102444800000_u64}})
                    .to_string(),
            )
            .unwrap();
            std::fs::rename(staged, self.path()).unwrap();
        }
    }

    impl Drop for AuthFixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn source_kind_only_allows_absent_or_recognized_generation_zero_metadata() {
        let fixture = AuthFixture::new();
        let path = fixture.path();
        let observe = || {
            serde_json::from_str::<serde_json::Value>(&observe_auth_json(path.to_str().unwrap()))
                .unwrap()
        };
        assert_eq!(observe()["sourceKind"], "absent");
        std::fs::write(&path, r#"{"_assistant":{"version":1,"installationId":"00000000-0000-4000-8000-000000000001","generation":"0"}}"#).unwrap();
        assert_eq!(observe()["sourceKind"], "legacy_empty");
        for bytes in [
            br#"{"_assistant":{"version":1,"installationId":"00000000-0000-4000-8000-000000000001","generation":"1"}}"#.to_vec(),
            br#"{"_assistant":{"version":1,"generation":"0"}}"#.to_vec(),
            br#"{"_assistant":{"version":1,"installationId":"00000000-0000-4000-8000-000000000001","generation":"1","generation":"0"}}"#.to_vec(),
            br#"{"_assistant":{"version":1,"installationId":"00000000-0000-4000-8000-000000000001","generation":"0"},"openai":null}"#.to_vec(),
            br#"{"openai":{"access":"bad\nheader","expires":4102444800000}}"#.to_vec(),
            vec![b'x'; MAX_CUSTOM_OPENAI_AUTH_BYTES as usize + 1],
        ] {
            std::fs::write(&path, &bytes).unwrap();
            let observation = observe();
            assert_eq!(observation["code"], "auth_unavailable");
            assert_eq!(observation["sourceKind"], "provider");
            assert_eq!(std::fs::read(&path).unwrap(), bytes);
        }
    }

    #[test]
    fn provider_error_envelope_carries_revision_actually_loaded_after_atomic_replacement() {
        use std::io::Write;
        use std::net::TcpListener;
        use std::time::Duration;

        let fixture = AuthFixture::new();
        fixture.replace("synthetic-preflight-A");
        let path = fixture.path();
        let admitted: serde_json::Value =
            serde_json::from_str(&observe_auth_json(path.to_str().unwrap())).unwrap();
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let endpoint = format!(
            "http://{}/synthetic-response",
            listener.local_addr().unwrap()
        );
        let server = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut request = Vec::new();
            let mut chunk = [0; 1024];
            while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                let count = socket.read(&mut chunk).unwrap();
                assert!(count > 0 && request.len() < 4096);
                request.extend_from_slice(&chunk[..count]);
            }
            let header_end = request
                .windows(4)
                .position(|window| window == b"\r\n\r\n")
                .unwrap()
                + 4;
            let headers = std::str::from_utf8(&request[..header_end]).unwrap();
            let length = headers
                .lines()
                .filter_map(|line| line.split_once(':'))
                .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                .map(|(_, value)| value.trim().parse::<usize>().unwrap())
                .unwrap();
            assert!(header_end + length <= 4096);
            while request.len() < header_end + length {
                let count = socket.read(&mut chunk).unwrap();
                assert!(count > 0);
                request.extend_from_slice(&chunk[..count]);
            }
            socket
                .write_all(
                    b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .unwrap();
        });
        let request = request::parse(r#"{"requestId":"synthetic-revision-request","instructions":"Synthetic policy","messages":[{"role":"user","content":"Synthetic question"}]}"#).unwrap();
        fixture.replace("synthetic-provider-B");
        let used: serde_json::Value =
            serde_json::from_str(&observe_auth_json(path.to_str().unwrap())).unwrap();
        assert_ne!(admitted["revision"], used["revision"]);
        let outcome = runtime().block_on(async {
            let client = CustomOpenAiClient::new(CustomOpenAiConfig {
                auth_path: path,
                endpoint,
                total_timeout: Duration::from_secs(3),
                ..Default::default()
            })
            .unwrap();
            let revision = Cell::new(None);
            let response = client.respond_with_auth_revision(
                &request.prompt,
                Some(&request.instructions),
                |_| {},
                |value| revision.set(Some(value)),
            );
            let (_cancel_tx, cancel_rx) = watch::channel(false);
            let (_failure_tx, failure_rx) = watch::channel(None);
            await_authenticated_response(response, &revision, cancel_rx, failure_rx).await
        });
        server.join().unwrap();
        let value: serde_json::Value = serde_json::from_str(&outcome.json()).unwrap();
        assert_eq!(value["code"], "unauthorized");
        assert_eq!(value["authRevision"], used["revision"]);
        assert_eq!(value.as_object().unwrap().len(), 3);
    }

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
    }

    struct PendingResponse(Arc<AtomicBool>);

    impl Future for PendingResponse {
        type Output = Result<CustomOpenAiResponse, CustomOpenAiError>;
        fn poll(
            self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Self::Output> {
            std::task::Poll::Pending
        }
    }

    impl Drop for PendingResponse {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    #[test]
    fn cancellation_drops_active_provider_future() {
        runtime().block_on(async {
            let dropped = Arc::new(AtomicBool::new(false));
            let (cancel_tx, cancel_rx) = watch::channel(false);
            let (_failure_tx, failure_rx) = watch::channel(None);
            let future = PendingResponse(dropped.clone());
            let signal = async {
                tokio::task::yield_now().await;
                cancel_tx.send(true).unwrap();
            };
            let (outcome, ()) = tokio::join!(await_response(future, cancel_rx, failure_rx), signal);
            assert_eq!(outcome.json(), r#"{"status":"cancelled"}"#);
            assert!(dropped.load(Ordering::SeqCst));
        });
    }

    #[test]
    fn callback_failure_beats_completion() {
        runtime().block_on(async {
            let (_cancel_tx, cancel_rx) = watch::channel(false);
            let (failure_tx, failure_rx) = watch::channel(None);
            let response = async {
                failure_tx.send(Some(ErrorCode::CallbackFailed)).unwrap();
                Err(CustomOpenAiError::Transport)
            };
            let result = await_response(response, cancel_rx, failure_rx).await;
            assert_eq!(
                result.json(),
                r#"{"status":"error","code":"callback_failed"}"#
            );
        });
    }

    #[test]
    fn callback_failure_drops_pending_provider_future() {
        runtime().block_on(async {
            let dropped = Arc::new(AtomicBool::new(false));
            let (_cancel_tx, cancel_rx) = watch::channel(false);
            let (failure_tx, failure_rx) = watch::channel(None);
            let signal = async {
                tokio::task::yield_now().await;
                failure_tx.send(Some(ErrorCode::CallbackFailed)).unwrap();
            };
            let (outcome, ()) = tokio::join!(
                await_response(PendingResponse(dropped.clone()), cancel_rx, failure_rx),
                signal
            );
            assert_eq!(
                outcome.json(),
                r#"{"status":"error","code":"callback_failed"}"#
            );
            assert!(dropped.load(Ordering::SeqCst));
        });
    }

    #[test]
    fn java_modified_utf8_preserves_supplementary_characters_and_counts_utf8_bytes() {
        let encoded = jni::strings::JNIString::from("Synthetic 🧪\0");
        assert_eq!(decode_jni_string(&encoded, 128).unwrap(), "Synthetic 🧪\0");
        assert_eq!(
            decode_jni_string(&encoded, 4),
            Err(ErrorCode::InvalidRequest)
        );
    }

    #[test]
    fn invalid_request_or_relative_auth_path_never_reaches_callback_or_provider() {
        let result = respond(
            "synthetic-1",
            "/synthetic/private/auth.json",
            "invalid JSON",
            |_| panic!("invalid input must not reach callback"),
        );
        assert_eq!(
            result.json(),
            r#"{"status":"error","code":"invalid_request"}"#
        );
        let result = respond(
            "synthetic-1",
            "relative/auth.json",
            r#"{"requestId":"synthetic-1","instructions":"Be brief.","messages":[{"role":"user","content":"Synthetic question"}]}"#,
            |_| panic!("invalid path must not reach callback"),
        );
        assert_eq!(
            result.json(),
            r#"{"status":"error","code":"invalid_request"}"#
        );
    }

    #[test]
    fn ready_handshake_closes_cancel_before_native_registration_race() {
        let request_id = "synthetic-ready-cancel";
        let input = format!(
            r#"{{"requestId":"{request_id}","instructions":"Synthetic policy","messages":[{{"role":"user","content":"Synthetic question"}}]}}"#
        );
        let mut ready_count = 0;
        let outcome = respond(
            request_id,
            "/synthetic/missing/auth.json",
            &input,
            |event| {
                match event {
                    CallbackEvent::Ready => {
                        ready_count += 1;
                        cancellation::registry().cancel(request_id);
                    }
                    CallbackEvent::FirstText(_)
                    | CallbackEvent::Delta(_)
                    | CallbackEvent::Status(_) => {
                        panic!("cancelled request must not deliver timing, text or status")
                    }
                }
                Ok(())
            },
        );
        assert_eq!(ready_count, 1);
        assert_eq!(outcome.json(), r#"{"status":"cancelled"}"#);
        assert!(cancellation::registry().register(request_id).is_ok());
    }

    #[test]
    fn ready_callback_failure_retires_registration_without_provider_execution() {
        let input = r#"{"requestId":"synthetic-ready-failure","instructions":"Synthetic policy","messages":[{"role":"user","content":"Synthetic question"}]}"#;
        let outcome = respond(
            "synthetic-ready-failure",
            "/synthetic/missing/auth.json",
            input,
            |_| Err(ErrorCode::CallbackFailed),
        );
        assert_eq!(
            outcome.json(),
            r#"{"status":"error","code":"callback_failed"}"#
        );
        assert!(cancellation::registry()
            .register("synthetic-ready-failure")
            .is_ok());
    }

    #[test]
    fn search_ready_cancel_and_callback_failure_preserve_registry_contract() {
        for fail_callback in [false, true] {
            let id = if fail_callback {
                "synthetic-search-ready-failure"
            } else {
                "synthetic-search-ready-cancel"
            };
            let input = serde_json::json!({"requestId": id, "conversationId":"synthetic-conversation:1", "searchMode":"verify", "instructions":"Synthetic policy", "messages":[{"role":"user","content":"Synthetic question"}]}).to_string();
            let outcome = respond(id, "/synthetic/missing/auth.json", &input, |event| {
                assert!(matches!(event, CallbackEvent::Ready));
                if fail_callback {
                    Err(ErrorCode::CallbackFailed)
                } else {
                    cancellation::registry().cancel(id);
                    Ok(())
                }
            });
            let value: serde_json::Value = serde_json::from_str(&outcome.json()).unwrap();
            assert_eq!(
                value["status"],
                if fail_callback { "error" } else { "cancelled" }
            );
            if fail_callback {
                assert_eq!(value["code"], "callback_failed");
            }
            assert!(cancellation::registry().register(id).is_ok());
        }
    }

    fn search_server(
        auth: &AuthFixture,
        output: serde_json::Value,
    ) -> (
        CustomOpenAiConfig,
        std::thread::JoinHandle<serde_json::Value>,
    ) {
        search_server_timed(auth, output, std::time::Duration::ZERO, false)
    }

    fn search_server_timed(
        auth: &AuthFixture,
        output: serde_json::Value,
        delay: std::time::Duration,
        terminal_only: bool,
    ) -> (
        CustomOpenAiConfig,
        std::thread::JoinHandle<serde_json::Value>,
    ) {
        use std::io::Write;
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let endpoint = format!("http://{}/synthetic", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(std::time::Duration::from_secs(3)))
                .unwrap();
            let mut bytes = Vec::new();
            let mut buffer = [0; 4096];
            let request = loop {
                let count = socket.read(&mut buffer).unwrap();
                assert!(count > 0 && bytes.len() < request::MAX_REQUEST_BYTES);
                bytes.extend_from_slice(&buffer[..count]);
                if let Some(end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
                    let headers = std::str::from_utf8(&bytes[..end]).unwrap();
                    let length = headers
                        .lines()
                        .filter_map(|line| line.split_once(':'))
                        .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                        .unwrap()
                        .1
                        .trim()
                        .parse::<usize>()
                        .unwrap();
                    if bytes.len() >= end + 4 + length {
                        break serde_json::from_slice(&bytes[end + 4..end + 4 + length]).unwrap();
                    }
                }
            };
            let mut body = String::new();
            if let Some(content) = output
                .get("content")
                .and_then(serde_json::Value::as_array)
                .and_then(|items| items.first())
                .filter(|_| !terminal_only)
            {
                let refusal = content["type"] == "refusal";
                let event = serde_json::json!({"type": if refusal { "response.refusal.delta" } else { "response.output_text.delta" }, "delta": content[if refusal { "refusal" } else { "text" }]});
                body.push_str(&format!("data: {event}\r\n\r\n"));
            }
            let completed = serde_json::json!({"type":"response.completed","response":{"status":"completed","model":"synthetic-returned-model","output":[output]}});
            body.push_str(&format!("data: {completed}\r\n\r\n"));
            std::thread::sleep(delay);
            if socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n").is_ok() {
                std::thread::sleep(delay);
                if !terminal_only && !delay.is_zero() {
                    let kind = if output["content"][0]["type"] == "refusal" { "response.refusal.delta" } else { "response.output_text.delta" };
                    let whitespace = serde_json::json!({"type":kind,"delta":" \n"});
                    let _ = socket.write_all(format!("data: {whitespace}\n\n").as_bytes());
                    std::thread::sleep(delay);
                }
                let _ = socket.write_all(body.as_bytes());
            }
            request
        });
        (
            CustomOpenAiConfig {
                auth_path: auth.path(),
                endpoint,
                total_timeout: std::time::Duration::from_secs(3),
                ..Default::default()
            },
            server,
        )
    }

    #[test]
    fn search_native_refusal_preserves_auth_revision_identity_and_final_kind() {
        let fixture = AuthFixture::new();
        fixture.replace("synthetic-search-auth");
        let request = request::parse(r#"{"requestId":"synthetic-search-refusal","conversationId":"synthetic-conversation:1","searchMode":"verify","instructions":"Synthetic policy","messages":[{"role":"user","content":"Synthetic question"}]}"#).unwrap();
        let (config, server) = search_server(
            &fixture,
            serde_json::json!({"type":"message","role":"assistant","status":"completed","content":[{"type":"refusal","refusal":"Synthetic refusal"}]}),
        );
        let (_cancel, rx) = watch::channel(false);
        let process = search::ProcessProvider::new(config.clone()).unwrap();
        let mut events = Vec::new();
        let outcome = process.runtime.block_on(search::respond(
            &process,
            config,
            &request,
            rx,
            &mut |event| {
                events.push(match event {
                    CallbackEvent::FirstText(_) => "first",
                    CallbackEvent::Delta(_) => "delta",
                    _ => "other",
                });
                Ok(())
            },
        ));
        assert_eq!(
            events,
            ["first", "delta"],
            "timing must precede the visible refusal delta"
        );
        let value: serde_json::Value = serde_json::from_str(&outcome.json()).unwrap();
        assert_eq!(value["status"], "complete");
        assert_eq!(value["contentType"], "refusal");
        assert_eq!(value["text"], "Synthetic refusal");
        assert_eq!(value["speechText"], "Synthetic refusal");
        assert!(value["authRevision"].is_string());
        let sent = server.join().unwrap();
        assert_eq!(sent["prompt_cache_key"], request.prompt.session_id);
        assert!(!sent.to_string().contains("synthetic-conversation:1"));
        assert_eq!(sent["tools"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn web_off_native_route_keeps_chat_without_tools_or_shared_service_wait() {
        for mode in ["quick", "verify", "research"] {
            let fixture = AuthFixture::new();
            fixture.replace("synthetic-web-off-auth");
            let request = request::parse(
                &serde_json::json!({
                    "requestId": format!("synthetic-web-off-{mode}"),
                    "conversationId": "synthetic-private-conversation:1",
                    "searchMode": mode,
                    "webEnabled": false,
                    "instructions": "Synthetic policy",
                    "messages": [{"role":"user", "content":"Synthetic ordinary question"}]
                })
                .to_string(),
            )
            .unwrap();
            assert!(!request.web_enabled);
            let (config, server) = search_server(
                &fixture,
                serde_json::json!({
                    "type":"message", "role":"assistant", "status":"completed",
                    "content":[{"type":"output_text", "text":"Synthetic ordinary answer"}]
                }),
            );
            let (_cancel, rx) = watch::channel(false);
            let process = search::ProcessProvider::new(config.clone()).unwrap();
            let outcome = process.runtime.block_on(async {
                let _other_web_request = process.hold_web_for_test().await;
                search::respond(&process, config, &request, rx, &mut |_| Ok(())).await
            });
            let value: serde_json::Value = serde_json::from_str(&outcome.json()).unwrap();
            assert_eq!(value["status"], "complete");
            assert!(value["text"]
                .as_str()
                .unwrap()
                .ends_with("Synthetic ordinary answer"));
            if mode != "quick" {
                assert!(value["text"]
                    .as_str()
                    .unwrap()
                    .starts_with("Web lookup disabled:"));
            }
            let sent = server.join().unwrap();
            assert!(sent["tools"].as_array().unwrap().is_empty());
            assert!(sent["instructions"]
                .as_str()
                .unwrap()
                .contains("Web lookup is disabled by native permission"));
            assert!(sent["instructions"]
                .as_str()
                .unwrap()
                .contains(voxgolem_core::response_style::PLAIN_TEXT_POLICY));
            assert!(!sent.to_string().contains("synthetic-private-conversation"));
        }
    }

    #[test]
    fn native_dispatch_timing_includes_headers_body_and_whitespace_for_both_routes() {
        use std::time::Duration;
        for search_route in [false, true] {
            let fixture = AuthFixture::new();
            fixture.replace("synthetic-timing-auth");
            let request = request::parse(r#"{"requestId":"synthetic-timing","conversationId":"synthetic-conversation:1","searchMode":"quick","instructions":"Synthetic policy","messages":[{"role":"user","content":"Synthetic question"}]}"#).unwrap();
            let output = serde_json::json!({"type":"message","role":"assistant","status":"completed","content":[{"type":"output_text","text":"Synthetic answer"}]});
            let (config, server) =
                search_server_timed(&fixture, output, Duration::from_millis(25), false);
            let process = search::ProcessProvider::new(config.clone()).unwrap();
            let (_cancel, rx) = watch::channel(false);
            let mut events = Vec::new();
            let mut first = None;
            let mut callback = |event: CallbackEvent<'_>| {
                match event {
                    CallbackEvent::FirstText(ms) => {
                        assert!(first.replace(ms).is_none());
                        events.push("first");
                    }
                    CallbackEvent::Delta(text) => events.push(if text.trim().is_empty() {
                        "space"
                    } else {
                        "text"
                    }),
                    _ => panic!("no tool or ready callback expected"),
                }
                Ok(())
            };
            let outcome = if search_route {
                process.runtime.block_on(search::respond(
                    &process,
                    config,
                    &request,
                    rx,
                    &mut callback,
                ))
            } else {
                let client = process.client(config).unwrap();
                process
                    .runtime
                    .block_on(direct_response(&client, &request, rx, &mut callback))
            };
            server.join().unwrap();
            assert!(matches!(outcome, Outcome::Complete { .. }));
            assert_eq!(events, ["space", "first", "text"]);
            assert!(
                first.unwrap() >= 75,
                "all three server delays belong to dispatch latency"
            );
        }
    }

    #[test]
    fn first_text_callback_failure_fails_both_routes_without_visible_delta() {
        for search_route in [false, true] {
            let fixture = AuthFixture::new();
            fixture.replace("synthetic-timing-failure");
            let request = request::parse(r#"{"requestId":"synthetic-timing-failure","conversationId":"synthetic-conversation:1","searchMode":"quick","instructions":"Synthetic policy","messages":[{"role":"user","content":"Synthetic question"}]}"#).unwrap();
            let (config, server) = search_server(
                &fixture,
                serde_json::json!({"type":"message","content":[{"type":"output_text","text":"Synthetic answer"}]}),
            );
            let process = search::ProcessProvider::new(config.clone()).unwrap();
            let (_cancel, rx) = watch::channel(false);
            let mut calls = 0;
            let mut callback = |event: CallbackEvent<'_>| {
                assert!(matches!(event, CallbackEvent::FirstText(_)));
                calls += 1;
                Err(ErrorCode::CallbackFailed)
            };
            let outcome = if search_route {
                process.runtime.block_on(search::respond(
                    &process,
                    config,
                    &request,
                    rx,
                    &mut callback,
                ))
            } else {
                let client = process.client(config).unwrap();
                process
                    .runtime
                    .block_on(direct_response(&client, &request, rx, &mut callback))
            };
            server.join().unwrap();
            assert_eq!(calls, 1);
            let value: serde_json::Value = serde_json::from_str(&outcome.json()).unwrap();
            assert_eq!(value["code"], "callback_failed");
            assert!(value["authRevision"].is_string());
        }
    }

    #[test]
    fn cancelled_after_whitespace_never_emits_first_text_or_visible_delta() {
        for search_route in [false, true] {
            let fixture = AuthFixture::new();
            fixture.replace("synthetic-timing-cancel");
            let request = request::parse(r#"{"requestId":"synthetic-timing-cancel","conversationId":"synthetic-conversation:1","searchMode":"quick","instructions":"Synthetic policy","messages":[{"role":"user","content":"Synthetic question"}]}"#).unwrap();
            let (config, server) = search_server_timed(
                &fixture,
                serde_json::json!({"type":"message","content":[{"type":"output_text","text":"Synthetic answer"}]}),
                std::time::Duration::from_millis(25),
                false,
            );
            let process = search::ProcessProvider::new(config.clone()).unwrap();
            let (cancel, rx) = watch::channel(false);
            let mut calls = 0;
            let mut callback = |event: CallbackEvent<'_>| {
                let CallbackEvent::Delta(text) = event else {
                    panic!("cancelled request cannot emit timing")
                };
                assert!(text.trim().is_empty());
                calls += 1;
                cancel.send(true).unwrap();
                Ok(())
            };
            let outcome = if search_route {
                process.runtime.block_on(search::respond(
                    &process,
                    config,
                    &request,
                    rx,
                    &mut callback,
                ))
            } else {
                let client = process.client(config).unwrap();
                process
                    .runtime
                    .block_on(direct_response(&client, &request, rx, &mut callback))
            };
            server.join().unwrap();
            assert_eq!(calls, 1);
            assert!(matches!(outcome, Outcome::Cancelled));
        }
    }

    #[test]
    fn terminal_only_text_reports_first_delivery_at_completion() {
        let fixture = AuthFixture::new();
        fixture.replace("synthetic-terminal-auth");
        let request = request::parse(r#"{"requestId":"synthetic-terminal","conversationId":"synthetic-conversation:1","searchMode":"quick","instructions":"Synthetic policy","messages":[{"role":"user","content":"Synthetic question"}]}"#).unwrap();
        let (config, server) = search_server_timed(
            &fixture,
            serde_json::json!({"type":"message","content":[{"type":"output_text","text":"Synthetic terminal answer"}]}),
            std::time::Duration::from_millis(25),
            true,
        );
        let process = search::ProcessProvider::new(config.clone()).unwrap();
        let (_cancel, rx) = watch::channel(false);
        let mut events = Vec::new();
        let outcome = process.runtime.block_on(search::respond(
            &process,
            config,
            &request,
            rx,
            &mut |event| {
                events.push(match event {
                    CallbackEvent::FirstText(ms) => {
                        assert!(ms >= 50);
                        "first"
                    }
                    CallbackEvent::Delta(text) => {
                        assert_eq!(text, "Synthetic terminal answer");
                        "terminal-delivery"
                    }
                    _ => panic!("unexpected callback"),
                });
                Ok(())
            },
        ));
        server.join().unwrap();
        assert!(matches!(outcome, Outcome::Complete { .. }));
        assert_eq!(events, ["first", "terminal-delivery"]);
    }

    #[test]
    fn timing_keeps_first_dispatch_and_obeys_cancellation_and_byte_budget() {
        let timing = FirstTextTiming::default();
        timing.dispatched(std::time::Instant::now() - std::time::Duration::from_millis(80));
        timing.dispatched(std::time::Instant::now());
        let mut bytes = 0;
        deliver_timed_delta(&mut bytes, &timing, "Synthetic", || true, &mut |_| {
            panic!("stale cancelled text")
        })
        .unwrap();
        assert_eq!(bytes, 0);
        bytes = request::MAX_OUTPUT_BYTES;
        assert_eq!(
            deliver_timed_delta(&mut bytes, &timing, "x", || false, &mut |_| panic!(
                "over budget"
            )),
            Err(ErrorCode::TooLarge)
        );
        bytes = 0;
        let cancelled = Cell::new(false);
        deliver_timed_delta(
            &mut bytes,
            &timing,
            "Synthetic",
            || cancelled.get(),
            &mut |event| {
                let CallbackEvent::FirstText(ms) = event else {
                    panic!("delta after callback cancellation")
                };
                assert!(ms >= 80);
                cancelled.set(true);
                Ok(())
            },
        )
        .unwrap();
        assert!(timing.delivered.get());
    }

    #[test]
    fn mobile_owner_reuses_one_connection_for_direct_and_multi_round_search_with_new_auth_config() {
        use std::io::Write;
        use std::time::Duration;

        let first_auth = AuthFixture::new();
        first_auth.replace("synthetic-direct-access");
        let search_auth = AuthFixture::new();
        search_auth.replace("synthetic-search-access");
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let endpoint = format!("http://{}/synthetic", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut requests = Vec::<(String, serde_json::Value)>::new();
            // All three requests must arrive on this single accepted socket.
            for round in 0..3 {
                let mut bytes = Vec::new();
                let mut buffer = [0; 4096];
                loop {
                    let count = socket.read(&mut buffer).unwrap();
                    assert!(count > 0 && bytes.len() < request::MAX_REQUEST_BYTES);
                    bytes.extend_from_slice(&buffer[..count]);
                    if let Some(end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
                        let headers = std::str::from_utf8(&bytes[..end])
                            .unwrap()
                            .to_ascii_lowercase();
                        let length = headers
                            .lines()
                            .filter_map(|line| line.split_once(':'))
                            .find(|(name, _)| *name == "content-length")
                            .unwrap()
                            .1
                            .trim()
                            .parse::<usize>()
                            .unwrap();
                        if bytes.len() >= end + 4 + length {
                            requests.push((
                                headers,
                                serde_json::from_slice(&bytes[end + 4..end + 4 + length]).unwrap(),
                            ));
                            break;
                        }
                    }
                }
                let body = if round == 1 {
                    // Invalid arguments exercise continuation without an external web request.
                    let call = serde_json::json!({"type":"function_call","id":"synthetic-item","call_id":"synthetic-call","name":"web_search","arguments":"not-json","status":"completed"});
                    format!(
                        "data: {}\n\n",
                        serde_json::json!({"type":"response.completed","response":{"status":"completed","output":[call]}})
                    )
                } else {
                    concat!(
                        "data: {\"type\":\"response.output_text.delta\",\"delta\":\"Synthetic pooled answer\"}\n\n",
                        "data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\"}}\n\n"
                    ).to_string()
                };
                if round > 0 {
                    std::thread::sleep(Duration::from_millis(40));
                }
                socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\n\r\n{body}", body.len()).as_bytes()).unwrap();
            }
            requests
        });
        let config = CustomOpenAiConfig {
            endpoint,
            auth_path: first_auth.path(),
            total_timeout: Duration::from_secs(3),
            ..Default::default()
        };
        let process = search::ProcessProvider::new(config.clone()).unwrap();
        let direct = request::parse(r#"{"requestId":"synthetic-pooled-direct","instructions":"Synthetic policy","messages":[{"role":"user","content":"Synthetic question"}]}"#).unwrap();
        let client = process.client(config.clone()).unwrap();
        let direct_revision = Cell::new(None);
        let direct_response = process
            .runtime
            .block_on(client.respond_with_auth_revision(
                &direct.prompt,
                Some(&direct.instructions),
                |_| {},
                |revision| direct_revision.set(Some(revision)),
            ))
            .unwrap();
        assert_eq!(direct_response.text, "Synthetic pooled answer");
        let search_request = request::parse(r#"{"requestId":"synthetic-pooled-search","conversationId":"synthetic-conversation:1","searchMode":"quick","model":"custom-luna-none","instructions":"Synthetic policy","messages":[{"role":"user","content":"Synthetic question"}]}"#).unwrap();
        let search_config = CustomOpenAiConfig {
            auth_path: search_auth.path(),
            model: search_request.model,
            ..config
        };
        let (_cancel, rx) = watch::channel(false);
        let mut events = Vec::new();
        let outcome = process.runtime.block_on(search::respond(
            &process,
            search_config,
            &search_request,
            rx,
            &mut |event| {
                events.push(match event {
                    CallbackEvent::Status(_) => "status",
                    CallbackEvent::FirstText(ms) => {
                        assert!(ms >= 80, "clock must retain the first round's dispatch");
                        "first"
                    }
                    CallbackEvent::Delta(_) => "text",
                    _ => panic!("unexpected ready callback"),
                });
                Ok(())
            },
        ));
        assert_eq!(events, ["status", "first", "text"]);
        let result: serde_json::Value = serde_json::from_str(&outcome.json()).unwrap();
        assert_eq!(result["status"], "complete");
        assert_eq!(result["text"], "Synthetic pooled answer");
        assert_ne!(
            result["authRevision"],
            auth_revision_string(direct_revision.get().unwrap())
        );
        let requests = server.join().unwrap();
        assert_eq!(requests.len(), 3);
        assert!(requests[0]
            .0
            .contains("authorization: bearer synthetic-direct-access"));
        for (headers, body) in &requests[1..] {
            assert!(headers.contains("authorization: bearer synthetic-search-access"));
            assert_eq!(body["model"], "gpt-6-luna");
        }
        assert_eq!(
            requests[2].1["input"].as_array().unwrap().last().unwrap()["type"],
            "function_call_output"
        );
    }

    #[test]
    fn search_status_callback_failure_cancels_before_external_tool_execution() {
        let fixture = AuthFixture::new();
        fixture.replace("synthetic-search-auth");
        let request = request::parse(r#"{"requestId":"synthetic-search-status-failure","conversationId":"synthetic-conversation:1","searchMode":"verify","instructions":"Synthetic policy","messages":[{"role":"user","content":"Synthetic question"}]}"#).unwrap();
        let (config, server) = search_server(
            &fixture,
            serde_json::json!({"type":"function_call","id":"synthetic-item","call_id":"synthetic-call","name":"web_search","status":"completed","arguments":serde_json::json!({"objective":"Synthetic objective","queries":["Synthetic query"],"num_results":1}).to_string()}),
        );
        let (_cancel, rx) = watch::channel(false);
        let process = search::ProcessProvider::new(config.clone()).unwrap();
        let mut statuses = Vec::new();
        let outcome = process.runtime.block_on(search::respond(
            &process,
            config,
            &request,
            rx,
            &mut |event| match event {
                CallbackEvent::Status(status) => {
                    statuses.push(status);
                    Err(ErrorCode::CallbackFailed)
                }
                CallbackEvent::FirstText(_) | CallbackEvent::Delta(_) => {
                    panic!("Tool JSON must never reach timing or text callback")
                }
                CallbackEvent::Ready => panic!("Ready is owned by native registration"),
            },
        ));
        server.join().unwrap();
        let value: serde_json::Value = serde_json::from_str(&outcome.json()).unwrap();
        assert_eq!(statuses, ["Searching"]);
        assert_eq!(value["code"], "callback_failed");
        assert!(value["authRevision"].is_string());
    }

    #[test]
    fn search_text_callback_failure_is_not_a_successful_completion() {
        let fixture = AuthFixture::new();
        fixture.replace("synthetic-search-auth");
        let request = request::parse(r#"{"requestId":"synthetic-search-text-failure","conversationId":"synthetic-conversation:1","searchMode":"quick","instructions":"Synthetic policy","messages":[{"role":"user","content":"Synthetic question"}]}"#).unwrap();
        let (config, server) = search_server(
            &fixture,
            serde_json::json!({"type":"message","role":"assistant","status":"completed","content":[{"type":"output_text","text":"Synthetic answer"}]}),
        );
        let (_cancel, rx) = watch::channel(false);
        let process = search::ProcessProvider::new(config.clone()).unwrap();
        let mut calls = 0;
        let outcome = process.runtime.block_on(search::respond(
            &process,
            config,
            &request,
            rx,
            &mut |event| {
                if let CallbackEvent::Delta(text) = event {
                    assert_eq!(text, "Synthetic answer");
                    calls += 1;
                    Err(ErrorCode::CallbackFailed)
                } else {
                    Ok(())
                }
            },
        ));
        server.join().unwrap();
        let value: serde_json::Value = serde_json::from_str(&outcome.json()).unwrap();
        assert_eq!(calls, 1);
        assert_eq!(value["code"], "callback_failed");
    }

    #[test]
    fn provider_configuration_reuses_portable_defaults_and_explicit_auth_path() {
        let request = request::parse(r#"{"requestId":"synthetic-1","model":"custom-sol-none","instructions":"Be brief.","messages":[{"role":"user","content":"Synthetic question"}]}"#).unwrap();
        let config = provider_config("/synthetic/private/codex-auth.json", &request);
        assert_eq!(config.endpoint, CustomOpenAiConfig::default().endpoint);
        assert_eq!(
            config.auth_path,
            Path::new("/synthetic/private/codex-auth.json")
        );
        assert_eq!(config.model, request.model);
        assert!(!valid_auth_path("relative/auth.json"));
        assert!(!valid_auth_path("/synthetic/../auth.json"));
        assert!(!valid_auth_path("/synthetic/\0auth.json"));
    }

    #[test]
    fn streamed_callback_budget_accepts_exact_utf8_boundary_and_rejects_overflow() {
        let mut bytes = 0;
        let mut calls = 0;
        let boundary = "🧪".repeat(request::MAX_OUTPUT_BYTES / 4);
        assert!(deliver_bounded_delta(&mut bytes, &boundary, || {
            calls += 1;
            Ok(())
        })
        .is_ok());
        assert_eq!(bytes, request::MAX_OUTPUT_BYTES);
        assert_eq!(
            deliver_bounded_delta(&mut bytes, "x", || {
                calls += 1;
                Ok(())
            }),
            Err(ErrorCode::TooLarge)
        );
        assert_eq!(calls, 1);
        bytes = usize::MAX;
        assert_eq!(
            deliver_bounded_delta(&mut bytes, "x", || panic!(
                "overflow must not reach callback"
            )),
            Err(ErrorCode::TooLarge)
        );
    }

    #[test]
    fn explicit_worker_identity_must_match_projection_before_registration() {
        let input = r#"{"requestId":"synthetic-json-id","instructions":"Synthetic policy","messages":[{"role":"user","content":"Synthetic question"}]}"#;
        let outcome = respond(
            "synthetic-worker-id",
            "/synthetic/missing/auth.json",
            input,
            |_| panic!("mismatched identity must not register"),
        );
        assert_eq!(
            outcome.json(),
            r#"{"status":"error","code":"invalid_request"}"#
        );
    }
}
