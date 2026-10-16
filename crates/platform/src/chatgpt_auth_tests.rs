use super::*;
use std::collections::VecDeque;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::{mpsc, oneshot};

struct Reply {
    status: u16,
    body: String,
    gate: Option<oneshot::Receiver<()>>,
    headers: String,
    content_type: &'static str,
}

impl Reply {
    fn json(status: u16, body: Value) -> Self {
        Self {
            status,
            body: body.to_string(),
            gate: None,
            headers: String::new(),
            content_type: "application/json",
        }
    }
}

struct Mock {
    engine: Arc<Engine>,
    requests: mpsc::Receiver<String>,
    server: tokio::task::JoinHandle<()>,
}

impl Drop for Mock {
    fn drop(&mut self) {
        self.server.abort();
    }
}

async fn mock(replies: Vec<Reply>) -> Mock {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let issuer = format!("http://{}", listener.local_addr().unwrap());
    let (tx, requests) = mpsc::channel(32);
    let server = tokio::spawn(async move {
        let mut replies = VecDeque::from(replies);
        let mut workers = tokio::task::JoinSet::new();
        while let Some(reply) = replies.pop_front() {
            let (mut socket, _) = listener.accept().await.unwrap();
            let tx = tx.clone();
            workers.spawn(async move {
                let mut request = Vec::new();
                loop {
                    let mut buffer = [0; 4096];
                    let n = socket.read(&mut buffer).await.unwrap();
                    assert!(n > 0);
                    request.extend_from_slice(&buffer[..n]);
                    assert!(request.len() < MAX_BODY + 8192);
                    if let Some(end) = request.windows(4).position(|part| part == b"\r\n\r\n") {
                        let headers = String::from_utf8_lossy(&request[..end]);
                        let length = headers.lines().find_map(|line| {
                            let (name, value) = line.split_once(':')?;
                            name.eq_ignore_ascii_case("content-length").then(|| value.trim().parse::<usize>().unwrap())
                        }).unwrap_or(0);
                        if request.len() >= end + 4 + length { break }
                    }
                }
                tx.send(String::from_utf8(request).unwrap()).await.unwrap();
                if let Some(gate) = reply.gate { let _ = gate.await; }
                let response = format!("HTTP/1.1 {} Test\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n{}\r\n{}",
                    reply.status, reply.content_type, reply.body.len(), reply.headers, reply.body);
                let _ = socket.write_all(response.as_bytes()).await;
            });
        }
        while let Some(result) = workers.join_next().await {
            result.unwrap();
        }
    });
    let client = CustomOpenAiClient::transport(&CustomOpenAiConfig {
        endpoint: issuer.clone(),
        connect_timeout: Duration::from_secs(1),
        ..Default::default()
    })
    .unwrap();
    Mock {
        engine: Arc::new(Engine {
            entries: Mutex::new(HashMap::new()),
            client,
            revoke: format!("{issuer}/api/accounts/oauth/revoke"),
            issuer,
            poll_safety: Duration::ZERO,
            flow_timeout: Duration::from_secs(8),
        }),
        requests,
        server,
    }
}

async fn request(mock: &mut Mock, route: &str) -> String {
    let request = tokio::time::timeout(Duration::from_secs(5), mock.requests.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(
        request.starts_with(&format!("POST {route} HTTP/1.1")),
        "{request}"
    );
    request
}

async fn phase(engine: &Arc<Engine>, path: &Path, expected: Phase) -> AuthStatus {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let status = engine.command(path, "status").await.unwrap();
            if status.phase == expected {
                return status;
            }
            assert_ne!(status.phase, Phase::Error, "{status:?}");
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap()
}

fn device() -> Reply {
    Reply::json(
        200,
        json!({"device_auth_id":"synthetic-device-auth","user_code":"ABCD-EFGH","interval":"1"}),
    )
}

fn code() -> Reply {
    Reply::json(
        200,
        json!({"authorization_code":"synthetic-auth-code","code_verifier":"synthetic-verifier"}),
    )
}

fn tokens() -> Reply {
    Reply::json(
        200,
        json!({"access_token":"synthetic-new-access","refresh_token":"synthetic-rotated-refresh","expires_in":3600}),
    )
}

fn path() -> (crate::test_support::TestDir, PathBuf) {
    let dir = crate::test_support::TestDir::new("device-auth");
    let path = dir.path().join("private/auth.json");
    (dir, path)
}

#[tokio::test]
async fn imported_credentials_are_never_mutated_or_exported() {
    let (_dir, path) = path();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let imported =
        br#"{"openai":{"access":"synthetic-import","refresh":"synthetic-refresh","expires":1}}"#;
    std::fs::write(&path, imported).unwrap();
    assert!(!is_managed(&path));
    assert!(!can_refresh(&path));
    assert!(export_access(&path).await.unwrap().is_none());
    assert_eq!(command(&path, "sign_out").await, Err(AuthError::Conflict));
    assert_eq!(command(&path, "start").await, Err(AuthError::Conflict));
    assert_eq!(std::fs::read(&path).unwrap(), imported);
}

#[tokio::test]
async fn expired_managed_credentials_are_observed_as_renewable() {
    let (_dir, path) = path();
    let credential = managed("synthetic-old-access", "synthetic-refresh", 1);
    write_file(&path, &credential).unwrap();
    assert!(is_managed(&path));
    assert!(can_refresh(&path));
    assert!(crate::custom_openai::observe_custom_openai_auth(&path)
        .status
        .is_ok());
    let status = command(&path, "status").await.unwrap();
    assert_eq!(status.phase, Phase::Connected);
    let public = serde_json::to_string(&status).unwrap();
    assert!(!public.contains("synthetic-"));
}

#[test]
fn status_serialization_has_only_public_display_fields() {
    let status = AuthStatus::pending("ABCD-EFGH".into());
    assert_eq!(
        serde_json::to_value(status).unwrap(),
        serde_json::json!({
            "phase":"pending", "userCode":"ABCD-EFGH",
            "verificationUrl":"https://auth.openai.com/codex/device"
        })
    );
}

fn managed(access: &str, refresh: &str, expires: u64) -> ManagedFile {
    ManagedFile {
        voxgolem_chatgpt: MANAGED_VERSION,
        openai: Credential {
            access: access.into(),
            refresh: Some(refresh.into()),
            expires,
            account_id: None,
        },
    }
}

#[tokio::test]
async fn device_flow_pending_exchange_private_persistence_and_access_only_export() {
    let (_dir, path) = path();
    let mut mock = mock(vec![
        device(),
        Reply::json(403, json!({"pending":"synthetic"})),
        Reply::json(404, json!({})),
        code(),
        tokens(),
    ])
    .await;
    assert_eq!(
        mock.engine.command(&path, "start").await.unwrap().phase,
        Phase::Starting
    );
    let start = request(&mut mock, "/api/accounts/deviceauth/usercode").await;
    assert!(start.contains(CLIENT_ID));
    let pending = phase(&mock.engine, &path, Phase::Pending).await;
    assert_eq!(pending.user_code.as_deref(), Some("ABCD-EFGH"));
    assert_eq!(
        mock.engine.command(&path, "start").await.unwrap().phase,
        Phase::Pending
    );
    for _ in 0..3 {
        let poll = request(&mut mock, "/api/accounts/deviceauth/token").await;
        assert!(poll.contains("synthetic-device-auth"));
        assert!(poll.contains("ABCD-EFGH"));
    }
    let exchange = request(&mut mock, "/oauth/token").await;
    assert!(exchange.contains("application/x-www-form-urlencoded"));
    let body = exchange.split("\r\n\r\n").nth(1).unwrap();
    let parsed = reqwest::Url::parse(&format!("https://synthetic.invalid/?{body}")).unwrap();
    let params: HashMap<_, _> = parsed.query_pairs().collect();
    assert_eq!(params["grant_type"], "authorization_code");
    assert_eq!(params["redirect_uri"], CALLBACK_URL);
    assert_eq!(params["code_verifier"], "synthetic-verifier");
    phase(&mock.engine, &path, Phase::Connected).await;
    assert!(is_managed(&path));
    assert!(can_refresh(&path));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            std::fs::metadata(path.parent().unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
    }
    // This public native export exercises real shared refresh selection without
    // network: the just-issued credential is already fresh.
    let exported = export_access(&path).await.unwrap().unwrap();
    assert!(!exported.contains("refresh"));
    assert!(!exported.contains("id_token"));
    let value: Value = serde_json::from_str(&exported).unwrap();
    assert_eq!(value["openai"]["type"], "oauth");
    assert_eq!(value["openai"]["access"], "synthetic-new-access");
    let watch_path = path.parent().unwrap().join("watch-auth.json");
    std::fs::write(&watch_path, exported).unwrap();
    assert!(is_managed(&watch_path));
    assert!(!can_refresh(&watch_path));
    ensure_fresh(&watch_path).await.unwrap();
    // A brand-new native owner recovers connected from the managed file.
    let restarted = super::tests::mock(vec![]).await;
    assert_eq!(
        restarted
            .engine
            .command(&path, "status")
            .await
            .unwrap()
            .phase,
        Phase::Connected
    );
}

#[tokio::test]
async fn signout_retains_source_tombstone_and_attempts_correct_revocation() {
    let (_dir, path) = path();
    write_file(
        &path,
        &managed(
            "synthetic-access",
            "synthetic-refresh",
            now_ms().unwrap() + 3_600_000,
        ),
    )
    .unwrap();
    let mut mock = mock(vec![Reply::json(
        503,
        json!({"error":"synthetic-private-server-body"}),
    )])
    .await;
    assert_eq!(
        mock.engine.command(&path, "sign_out").await.unwrap().phase,
        Phase::SignedOut
    );
    assert!(path.exists());
    assert!(is_managed(&path));
    assert!(!can_refresh(&path));
    assert!(export_access(&path).await.unwrap().is_none());
    let revoked = request(&mut mock, "/api/accounts/oauth/revoke").await;
    assert!(revoked.contains("token=synthetic-refresh"));
    assert!(revoked.contains("token_type_hint=refresh_token"));
    assert!(!String::from_utf8(std::fs::read(&path).unwrap())
        .unwrap()
        .contains("synthetic-"));
    assert_eq!(
        mock.engine.command(&path, "status").await.unwrap().phase,
        Phase::SignedOut
    );
}

#[tokio::test]
async fn refresh_is_single_flight_rotates_and_preserves_imports() {
    let (_dir, path) = path();
    write_file(
        &path,
        &managed("synthetic-old-access", "synthetic-refresh", 1),
    )
    .unwrap();
    let mut mock = mock(vec![tokens()]).await;
    let (first, second) = tokio::join!(
        mock.engine.ensure_fresh(&path),
        mock.engine.ensure_fresh(&path)
    );
    first.unwrap();
    second.unwrap();
    let refresh = request(&mut mock, "/oauth/token").await;
    assert!(refresh.contains("grant_type=refresh_token"));
    assert!(refresh.contains("refresh_token=synthetic-refresh"));
    let bytes = std::fs::read(&path).unwrap();
    let file = storage::parse(&bytes).unwrap();
    assert_eq!(
        file.openai.refresh.as_deref(),
        Some("synthetic-rotated-refresh")
    );
    assert_eq!(file.openai.access, "synthetic-new-access");
    assert!(file.openai.expires > now_ms().unwrap() + 60_000);
    assert!(
        mock.requests.try_recv().is_err(),
        "only one refresh may be dispatched"
    );
}

#[tokio::test]
async fn late_refresh_cannot_resurrect_signout_or_overwrite_external_replacement() {
    for signout in [true, false] {
        let (_dir, path) = path();
        write_file(
            &path,
            &managed("synthetic-old-access", "synthetic-refresh", 1),
        )
        .unwrap();
        let (release, gate) = oneshot::channel();
        let mut response = tokens();
        response.gate = Some(gate);
        let mut mock = mock(vec![response, Reply::json(200, json!({}))]).await;
        let engine = mock.engine.clone();
        let refreshing = path.clone();
        let task = tokio::spawn(async move { engine.ensure_fresh(&refreshing).await });
        request(&mut mock, "/oauth/token").await;
        if signout {
            let engine = mock.engine.clone();
            let signing_out = path.clone();
            let signing_out =
                tokio::spawn(async move { engine.command(&signing_out, "sign_out").await });
            let entry = mock.engine.entry(&path).await.unwrap();
            tokio::time::timeout(Duration::from_secs(2), async {
                while entry.state.lock().unwrap().generation == 0 {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            release.send(()).unwrap();
            assert_eq!(task.await.unwrap(), Err(AuthError::Cancelled));
            signing_out.await.unwrap().unwrap();
            request(&mut mock, "/api/accounts/oauth/revoke").await;
            assert!(storage::tombstone(&std::fs::read(&path).unwrap()));
        } else {
            std::fs::write(
                &path,
                br#"{"openai":{"access":"synthetic-import-replacement","expires":1}}"#,
            )
            .unwrap();
            let before = std::fs::read(&path).unwrap();
            release.send(()).unwrap();
            assert_eq!(task.await.unwrap(), Err(AuthError::Conflict));
            assert_eq!(std::fs::read(&path).unwrap(), before);
        }
    }
}

#[tokio::test]
async fn cancel_and_signout_fence_late_start_response() {
    for action in ["cancel", "sign_out"] {
        let (_dir, path) = path();
        let (release, gate) = oneshot::channel();
        let mut response = device();
        response.gate = Some(gate);
        let mut mock = mock(vec![response]).await;
        mock.engine.command(&path, "start").await.unwrap();
        request(&mut mock, "/api/accounts/deviceauth/usercode").await;
        assert_eq!(
            mock.engine.command(&path, action).await.unwrap().phase,
            Phase::SignedOut
        );
        release.send(()).unwrap();
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert_eq!(
            mock.engine.command(&path, "status").await.unwrap().phase,
            Phase::SignedOut
        );
        assert!(!can_refresh(&path));
        assert!(mock.requests.try_recv().is_err());
    }
}

#[tokio::test]
async fn cancel_does_not_abort_shared_refresh() {
    let (_dir, path) = path();
    write_file(&path, &managed("synthetic-old", "synthetic-refresh", 1)).unwrap();
    let (release, gate) = oneshot::channel();
    let mut response = tokens();
    response.gate = Some(gate);
    let mut mock = mock(vec![response]).await;
    let engine = mock.engine.clone();
    let refreshing = path.clone();
    let task = tokio::spawn(async move { engine.ensure_fresh(&refreshing).await });
    request(&mut mock, "/oauth/token").await;
    mock.engine.command(&path, "cancel").await.unwrap();
    release.send(()).unwrap();
    task.await.unwrap().unwrap();
    assert_eq!(
        storage::parse(&std::fs::read(path).unwrap())
            .unwrap()
            .openai
            .access,
        "synthetic-new-access"
    );
}

#[tokio::test]
async fn terminal_start_rejection_is_safe_and_poll_pending_is_route_specific() {
    let (_dir, path) = path();
    let mut mock = mock(vec![Reply::json(
        403,
        json!({"error":"synthetic-secret-server-text"}),
    )])
    .await;
    mock.engine.command(&path, "start").await.unwrap();
    request(&mut mock, "/api/accounts/deviceauth/usercode").await;
    let status = phase(&mock.engine, &path, Phase::Error).await;
    assert_eq!(
        status.error.as_deref(),
        Some(AuthError::Rejected.to_string().as_str())
    );
    assert!(!serde_json::to_string(&status)
        .unwrap()
        .contains("synthetic-secret"));
    assert!(!path.exists());
}

#[tokio::test]
async fn flow_has_a_bounded_deadline() {
    let (_dir, path) = path();
    let mut mock = mock(vec![device()]).await;
    Arc::get_mut(&mut mock.engine).unwrap().flow_timeout = Duration::from_millis(50);
    mock.engine.command(&path, "start").await.unwrap();
    request(&mut mock, "/api/accounts/deviceauth/usercode").await;
    let status = phase(&mock.engine, &path, Phase::Error).await;
    assert_eq!(status.error, Some(AuthError::Timeout.to_string()));
    assert!(!path.exists());
}

#[test]
fn metadata_uses_only_documented_claim_and_never_retains_id_token() {
    // Synthetic JWT payload {"https://api.openai.com/auth":{"chatgpt_account_id":"synthetic-account"}}
    let token = "synthetic.eyJodHRwczovL2FwaS5vcGVuYWkuY29tL2F1dGgiOnsiY2hhdGdwdF9hY2NvdW50X2lkIjoic3ludGhldGljLWFjY291bnQifX0.synthetic";
    let file = Tokens {
        access_token: "synthetic-access".into(),
        refresh_token: Some("synthetic-refresh".into()),
        id_token: Some(token.into()),
        expires_in: Some(3600),
    }
    .credential(None)
    .unwrap();
    assert_eq!(file.openai.account_id.as_deref(), Some("synthetic-account"));
    let encoded = serde_json::to_string(&file).unwrap();
    assert!(!encoded.contains("id_token"));
    assert!(!encoded.contains("eyJ"));
}

#[tokio::test]
async fn late_code_exchange_cannot_publish_after_signout() {
    let (_dir, path) = path();
    let (release, gate) = oneshot::channel();
    let mut response = tokens();
    response.gate = Some(gate);
    let mut mock = mock(vec![device(), code(), response]).await;
    mock.engine.command(&path, "start").await.unwrap();
    request(&mut mock, "/api/accounts/deviceauth/usercode").await;
    request(&mut mock, "/api/accounts/deviceauth/token").await;
    request(&mut mock, "/oauth/token").await;
    mock.engine.command(&path, "sign_out").await.unwrap();
    release.send(()).unwrap();
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert!(storage::tombstone(&std::fs::read(&path).unwrap()));
    assert_eq!(
        mock.engine.command(&path, "status").await.unwrap().phase,
        Phase::SignedOut
    );
}

#[tokio::test]
async fn refresh_failure_is_safe_and_does_not_mutate_or_disable_retry() {
    let (_dir, path) = path();
    write_file(&path, &managed("synthetic-old", "synthetic-refresh", 1)).unwrap();
    let original = std::fs::read(&path).unwrap();
    let mock = mock(vec![
        Reply::json(400, json!({"error":"synthetic-provider-private-failure"})),
        tokens(),
    ])
    .await;
    let error = mock.engine.ensure_fresh(&path).await.unwrap_err();
    assert_eq!(error, AuthError::Rejected);
    assert!(!error.to_string().contains("synthetic-"));
    assert_eq!(std::fs::read(&path).unwrap(), original);
    mock.engine.ensure_fresh(&path).await.unwrap();
    assert_eq!(
        storage::parse(&std::fs::read(path).unwrap())
            .unwrap()
            .openai
            .access,
        "synthetic-new-access"
    );
}

#[tokio::test]
async fn optional_refresh_rotation_and_account_metadata_are_preserved() {
    let (_dir, path) = path();
    let mut file = managed("synthetic-old", "synthetic-refresh", 1);
    file.openai.account_id = Some("synthetic-account".into());
    write_file(&path, &file).unwrap();
    let mock = mock(vec![Reply::json(
        200,
        json!({"access_token":"synthetic-new", "expires_in":3600}),
    )])
    .await;
    mock.engine.ensure_fresh(&path).await.unwrap();
    let new = storage::parse(&std::fs::read(path).unwrap()).unwrap();
    assert_eq!(new.openai.refresh.as_deref(), Some("synthetic-refresh"));
    assert_eq!(new.openai.account_id.as_deref(), Some("synthetic-account"));
}

#[tokio::test]
async fn response_length_and_redirects_are_rejected_without_following() {
    let target = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let mut redirect = Reply::json(302, json!({}));
    redirect.headers = format!(
        "Location: http://{}/stolen\r\n",
        target.local_addr().unwrap()
    );
    let mock = mock(vec![
        redirect,
        Reply::json(200, json!({"oversized":"x".repeat(MAX_BODY)})),
    ])
    .await;
    let response: Result<Value, _> = mock
        .engine
        .json(
            mock.engine
                .client
                .post(format!("{}/redirect", mock.engine.issuer)),
        )
        .await;
    assert_eq!(response, Err(AuthError::Rejected));
    assert!(
        tokio::time::timeout(Duration::from_millis(50), target.accept())
            .await
            .is_err()
    );
    let response: Result<Value, _> = mock
        .engine
        .json(
            mock.engine
                .client
                .post(format!("{}/oversized", mock.engine.issuer)),
        )
        .await;
    assert_eq!(response, Err(AuthError::InvalidResponse));
}

#[tokio::test]
async fn registry_is_bounded_and_path_aliases_share_owner() {
    let (dir, path) = path();
    let mock = mock(vec![]).await;
    let first = mock.engine.entry(&path).await.unwrap();
    let alias = mock
        .engine
        .entry(&path.parent().unwrap().join("../private/auth.json"))
        .await
        .unwrap();
    assert!(Arc::ptr_eq(&first, &alias));
    drop(alias);
    let mut held = vec![first];
    for n in 1..MAX_PATHS {
        held.push(
            mock.engine
                .entry(&dir.path().join(format!("private/auth-{n}.json")))
                .await
                .unwrap(),
        );
    }
    assert!(matches!(
        mock.engine
            .entry(&dir.path().join("private/overflow.json"))
            .await,
        Err(AuthError::Busy)
    ));
    assert_eq!(mock.engine.entries.lock().unwrap().len(), MAX_PATHS);
    drop(held);
    mock.engine
        .entry(&dir.path().join("private/after-eviction.json"))
        .await
        .unwrap();
    assert!(mock.engine.entries.lock().unwrap().len() <= MAX_PATHS);
}

#[tokio::test]
async fn request_cancellation_does_not_cancel_native_refresh_owner() {
    let (_dir, path) = path();
    write_file(&path, &managed("synthetic-old", "synthetic-refresh", 1)).unwrap();
    let (release, gate) = oneshot::channel();
    let mut response = tokens();
    response.gate = Some(gate);
    let mut mock = mock(vec![response]).await;
    let engine = mock.engine.clone();
    let refreshing = path.clone();
    // Same runtime boundary used by public fresh(): caller owns only a JoinHandle,
    // never the native refresh task itself.
    let caller = tokio::spawn(async move {
        owner()
            .unwrap()
            .runtime
            .spawn(async move { engine.ensure_fresh(&refreshing).await })
            .await
            .unwrap()
    });
    request(&mut mock, "/oauth/token").await;
    caller.abort();
    let _ = caller.await;
    release.send(()).unwrap();
    mock.engine.ensure_fresh(&path).await.unwrap();
    assert_eq!(
        storage::parse(&std::fs::read(&path).unwrap())
            .unwrap()
            .openai
            .access,
        "synthetic-new-access"
    );
    assert!(mock.requests.try_recv().is_err());
}

#[tokio::test]
async fn provider_guard_never_sends_managed_access_to_a_custom_loopback_endpoint() {
    use crate::custom_openai::CustomOpenAiPrompt;
    let (_dir, path) = path();
    write_file(
        &path,
        &managed(
            "synthetic-access",
            "synthetic-refresh",
            now_ms().unwrap() + 3_600_000,
        ),
    )
    .unwrap();
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let client = CustomOpenAiClient::new(CustomOpenAiConfig {
        endpoint: format!("http://{}", listener.local_addr().unwrap()),
        auth_path: path,
        ..Default::default()
    })
    .unwrap();
    let result = client
        .respond(
            &CustomOpenAiPrompt {
                session_id: "synthetic-session".into(),
                prompt: "Synthetic prompt".into(),
                history: vec![],
            },
            |_| {},
        )
        .await;
    assert_eq!(result, Err(CustomOpenAiError::InvalidConfig));
    assert!(
        tokio::time::timeout(Duration::from_millis(50), listener.accept())
            .await
            .is_err()
    );
}

#[test]
fn unsafe_storage_and_oversized_files_fail_closed() {
    let (dir, path) = path();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, vec![b' '; MAX_BODY + 1]).unwrap();
    assert_eq!(read_file(&path), Err(AuthError::Storage));
    #[cfg(unix)]
    {
        use std::os::unix::fs::{symlink, PermissionsExt};
        std::fs::remove_file(&path).unwrap();
        let imported = dir.path().join("imported.json");
        std::fs::write(&imported, "synthetic-import").unwrap();
        symlink(&imported, &path).unwrap();
        assert_eq!(read_file(&path), Err(AuthError::Storage));
        std::fs::remove_file(&path).unwrap();
        std::fs::set_permissions(
            path.parent().unwrap(),
            std::fs::Permissions::from_mode(0o777),
        )
        .unwrap();
        assert_eq!(
            write_file(&path, &managed("synthetic-access", "synthetic-refresh", 1)),
            Err(AuthError::Storage)
        );
        assert_eq!(
            std::fs::read_to_string(imported).unwrap(),
            "synthetic-import"
        );
    }
}

#[tokio::test]
async fn independent_native_owners_serialize_refresh_through_file_lock() {
    let (_dir, path) = path();
    write_file(&path, &managed("synthetic-old", "synthetic-refresh", 1)).unwrap();
    let (release, gate) = oneshot::channel();
    let mut response = tokens();
    response.gate = Some(gate);
    let mut first = mock(vec![response]).await;
    let second = mock(vec![]).await;
    let engine = first.engine.clone();
    let refreshing = path.clone();
    let first_task = tokio::spawn(async move { engine.ensure_fresh(&refreshing).await });
    request(&mut first, "/oauth/token").await;
    let engine = second.engine.clone();
    let refreshing = path.clone();
    let second_task = tokio::spawn(async move { engine.ensure_fresh(&refreshing).await });
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert!(!second_task.is_finished());
    release.send(()).unwrap();
    first_task.await.unwrap().unwrap();
    second_task.await.unwrap().unwrap();
    assert_eq!(
        storage::parse(&std::fs::read(path).unwrap())
            .unwrap()
            .openai
            .access,
        "synthetic-new-access"
    );
}

#[tokio::test]
async fn independent_owner_signout_durably_fences_an_older_pending_login() {
    let (_dir, path) = path();
    storage::write_tombstone(&path).unwrap();
    let original = std::fs::read(&path).unwrap();
    let (release, gate) = oneshot::channel();
    let mut response = tokens();
    response.gate = Some(gate);
    let mut first = mock(vec![device(), code(), response]).await;
    let second = mock(vec![]).await;
    first.engine.command(&path, "start").await.unwrap();
    request(&mut first, "/api/accounts/deviceauth/usercode").await;
    request(&mut first, "/api/accounts/deviceauth/token").await;
    request(&mut first, "/oauth/token").await;
    second.engine.command(&path, "sign_out").await.unwrap();
    let fence = std::fs::read(&path).unwrap();
    assert_ne!(
        fence, original,
        "every sign-out must change the durable fence"
    );
    release.send(()).unwrap();
    let status = phase(&first.engine, &path, Phase::Error).await;
    assert_eq!(status.error, Some(AuthError::Conflict.to_string()));
    assert_eq!(std::fs::read(&path).unwrap(), fence);
}

#[test]
fn subprocess_lock_probe() {
    let Some(path) = std::env::var_os("VOX_SYNTHETIC_AUTH_LOCK_PATH") else {
        return;
    };
    let lock = storage::try_lock(Path::new(&path)).unwrap();
    assert_eq!(
        lock.is_some(),
        std::env::var_os("VOX_SYNTHETIC_AUTH_LOCK_AVAILABLE").is_some()
    );
}

#[test]
fn credential_lock_excludes_an_actual_second_process() {
    let (_dir, path) = path();
    let lock = storage::try_lock(&path).unwrap().unwrap();
    let probe = |available: bool| {
        let mut child = std::process::Command::new(std::env::current_exe().unwrap());
        child
            .args(["--exact", "chatgpt_auth::tests::subprocess_lock_probe"])
            .env("VOX_SYNTHETIC_AUTH_LOCK_PATH", &path)
            .env_remove("VOX_SYNTHETIC_AUTH_LOCK_AVAILABLE");
        if available {
            child.env("VOX_SYNTHETIC_AUTH_LOCK_AVAILABLE", "1");
        }
        let output = child.output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stdout)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
    };
    probe(false);
    drop(lock);
    probe(true);
}

#[cfg(unix)]
#[tokio::test]
async fn imported_symlink_dispatches_unchanged_but_managed_symlink_is_rejected() {
    use crate::custom_openai::CustomOpenAiPrompt;
    use std::os::unix::fs::symlink;
    let (dir, path) = path();
    let imported = dir.path().join("synthetic-import.json");
    let contents = json!({"openai":{"access":"synthetic-imported-access", "expires":now_ms().unwrap() + 3_600_000}}).to_string();
    std::fs::write(&imported, &contents).unwrap();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    symlink(&imported, &path).unwrap();
    let mut reply = Reply::json(200, json!({}));
    reply.content_type = "text/event-stream";
    reply.body = concat!(
        "data: {\"type\":\"response.output_text.delta\",\"delta\":\"Synthetic reply\"}\n\n",
        "data: {\"type\":\"response.completed\",\"response\":{}}\n\n"
    )
    .into();
    let mut mock = mock(vec![reply]).await;
    let client = CustomOpenAiClient::new(CustomOpenAiConfig {
        endpoint: mock.engine.issuer.clone(),
        auth_path: path.clone(),
        ..Default::default()
    })
    .unwrap();
    let prompt = CustomOpenAiPrompt {
        session_id: "synthetic-session".into(),
        prompt: "Synthetic prompt".into(),
        history: vec![],
    };
    assert_eq!(
        client.respond(&prompt, |_| {}).await.unwrap().text,
        "Synthetic reply"
    );
    let sent = request(&mut mock, "/").await;
    assert!(sent.contains("Bearer synthetic-imported-access"));
    assert_eq!(std::fs::read_to_string(&imported).unwrap(), contents);
    assert!(std::fs::symlink_metadata(&path).unwrap().is_symlink());
    // A managed marker behind the same symlink is not a refresh/publication path.
    write_file(
        &imported,
        &managed("synthetic-managed", "synthetic-refresh", 1),
    )
    .unwrap();
    let before = std::fs::read(&imported).unwrap();
    assert_eq!(
        client.respond(&prompt, |_| {}).await,
        Err(CustomOpenAiError::AuthUnavailable)
    );
    assert!(!can_refresh(&path));
    assert_eq!(std::fs::read(imported).unwrap(), before);
}

#[tokio::test]
async fn terminal_refresh_codes_require_reconnect_without_import_fallback() {
    for code in [
        "invalid_grant",
        "invalid_refresh_token",
        "token_expired",
        "refresh_token_expired",
        "refresh_token_invalidated",
        "refresh_token_reused",
    ] {
        let (_dir, path) = path();
        write_file(&path, &managed("synthetic-old", "synthetic-refresh", 1)).unwrap();
        let body = if code == "invalid_grant" {
            json!({"error":code,"error_description":"synthetic-private-server-description"})
        } else {
            json!({"error":{"code":code,"message":"synthetic-private-server-description"}})
        };
        let mock = mock(vec![Reply::json(400, body)]).await;
        assert_eq!(
            mock.engine.ensure_fresh(&path).await,
            Err(AuthError::ReconnectRequired)
        );
        assert!(is_managed(&path));
        assert!(!can_refresh(&path));
        assert!(storage::tombstone(&std::fs::read(&path).unwrap()));
        let status = mock.engine.command(&path, "status").await.unwrap();
        assert_eq!(status.phase, Phase::Error);
        assert_eq!(status.error, Some(AuthError::ReconnectRequired.to_string()));
        assert!(!serde_json::to_string(&status)
            .unwrap()
            .contains("synthetic-private"));
        assert!(export_access(&path).await.unwrap().is_none());
        // A later prompt will not keep consuming a known terminal refresh token.
        mock.engine.ensure_fresh(&path).await.unwrap();
    }
}

#[tokio::test]
async fn rate_limits_infrastructure_and_unknown_errors_preserve_credentials() {
    for status in [429, 500, 503, 400] {
        let (_dir, path) = path();
        write_file(&path, &managed("synthetic-old", "synthetic-refresh", 1)).unwrap();
        let original = std::fs::read(&path).unwrap();
        let code = if status == 400 {
            "synthetic_unrecognized_infrastructure_error"
        } else {
            "invalid_grant"
        };
        let mock = mock(vec![Reply::json(status, json!({"error":code})), tokens()]).await;
        let error = mock.engine.ensure_fresh(&path).await.unwrap_err();
        assert_eq!(
            error,
            if status == 400 {
                AuthError::Rejected
            } else {
                AuthError::Unavailable
            }
        );
        assert_eq!(std::fs::read(&path).unwrap(), original);
        assert!(can_refresh(&path));
        assert_eq!(
            mock.engine.command(&path, "status").await.unwrap().phase,
            Phase::Connected
        );
        mock.engine.ensure_fresh(&path).await.unwrap();
    }
}

#[tokio::test]
async fn retry_start_is_available_after_terminal_refresh() {
    let (_dir, path) = path();
    write_file(&path, &managed("synthetic-old", "synthetic-refresh", 1)).unwrap();
    let mut mock = mock(vec![
        Reply::json(401, json!({"error":"invalid_grant"})),
        device(),
    ])
    .await;
    assert_eq!(
        mock.engine.ensure_fresh(&path).await,
        Err(AuthError::ReconnectRequired)
    );
    request(&mut mock, "/oauth/token").await;
    assert_eq!(
        mock.engine.command(&path, "start").await.unwrap().phase,
        Phase::Starting
    );
    request(&mut mock, "/api/accounts/deviceauth/usercode").await;
    phase(&mock.engine, &path, Phase::Pending).await;
    mock.engine.command(&path, "cancel").await.unwrap();
}

#[tokio::test]
async fn refresh_transport_failure_preserves_renewable_credentials() {
    let (_dir, path) = path();
    write_file(&path, &managed("synthetic-old", "synthetic-refresh", 1)).unwrap();
    let original = std::fs::read(&path).unwrap();
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);
    let mut mock = mock(vec![]).await;
    Arc::get_mut(&mut mock.engine).unwrap().issuer = format!("http://{address}");
    assert_eq!(
        mock.engine.ensure_fresh(&path).await,
        Err(AuthError::Transport)
    );
    assert_eq!(std::fs::read(&path).unwrap(), original);
    assert!(can_refresh(&path));
}

#[cfg(feature = "web-search")]
#[tokio::test]
async fn tool_round_never_sends_managed_access_to_custom_endpoint() {
    use crate::custom_openai::tools::{RoundError, RoundOptions, RoundRequest};
    let (_dir, path) = path();
    write_file(
        &path,
        &managed(
            "synthetic-managed-access",
            "synthetic-refresh",
            now_ms().unwrap() + 3_600_000,
        ),
    )
    .unwrap();
    let mut response = Reply::json(200, json!({}));
    response.content_type = "text/event-stream";
    response.body = "data: {\"type\":\"response.completed\",\"response\":{\"output\":[{\"type\":\"message\",\"content\":[{\"type\":\"output_text\",\"text\":\"Synthetic reply\"}]}]}}\n\n".into();
    let mut mock = mock(vec![response]).await;
    let client = CustomOpenAiClient::new(CustomOpenAiConfig {
        endpoint: mock.engine.issuer.clone(),
        auth_path: path,
        ..Default::default()
    })
    .unwrap();
    let result = client
        .tool_round_with_events(
            RoundRequest {
                session_id: "synthetic-session",
                input: &[],
                instructions: "Synthetic instruction",
                tools: &[],
            },
            RoundOptions {
                deadline: tokio::time::Instant::now() + Duration::from_secs(2),
                expected_auth_revision: None,
                require_tool: false,
            },
            |_| Ok(()),
        )
        .await;
    assert!(matches!(
        result,
        Err(RoundError::Provider(CustomOpenAiError::InvalidConfig))
    ));
    assert!(mock.requests.try_recv().is_err());
}

#[cfg(feature = "web-search")]
#[tokio::test]
async fn first_tool_round_refreshes_managed_access_before_loading_or_publishing_revision() {
    use crate::custom_openai::envelope::InputProjection;
    use crate::custom_openai::tools::{ModelEvent, RoundError, RoundOptions};
    let (_dir, path) = path();
    write_file(&path, &managed("synthetic-expired", "synthetic-refresh", 1)).unwrap();
    let old_revision = crate::custom_openai::custom_openai_auth_revision(&path).unwrap();
    let mut mock = mock(vec![tokens()]).await;
    let client = CustomOpenAiClient::new(CustomOpenAiConfig {
        auth_path: path.clone(),
        ..Default::default()
    })
    .unwrap();
    let ordering = Mutex::new(Vec::new());
    let result = client
        .projected_round_with_refresh(
            "synthetic-session",
            InputProjection::mandatory(&[]),
            "Synthetic policy",
            &[],
            RoundOptions {
                deadline: tokio::time::Instant::now() + Duration::from_secs(3),
                expected_auth_revision: None,
                require_tool: false,
            },
            || {
                ordering.lock().unwrap().push("check");
                Ok(())
            },
            |event| {
                if let ModelEvent::AuthRevision(revision) = event {
                    ordering.lock().unwrap().push("auth");
                    assert_ne!(revision, old_revision);
                    let file = storage::parse(&std::fs::read(&path).unwrap()).unwrap();
                    assert_eq!(file.openai.access, "synthetic-new-access");
                    // Stop at the native admission callback: the production token
                    // endpoint is mocked; no real Codex request is authorized here.
                    return Err(());
                }
                panic!("no provider dispatch or text event is authorized in this fixture");
            },
            async {
                ordering.lock().unwrap().push("refresh");
                mock.engine
                    .ensure_fresh(&path)
                    .await
                    .map_err(|_| CustomOpenAiError::AuthUnavailable)?;
                ordering.lock().unwrap().push("refreshed");
                Ok(())
            },
        )
        .await;
    assert!(matches!(result, Err(RoundError::CallbackFailed)));
    let refresh_request = request(&mut mock, "/oauth/token").await;
    assert!(refresh_request.contains("grant_type=refresh_token"));
    let ordering = ordering.into_inner().unwrap();
    let refresh = ordering
        .iter()
        .position(|stage| *stage == "refresh")
        .unwrap();
    let refreshed = ordering
        .iter()
        .position(|stage| *stage == "refreshed")
        .unwrap();
    assert_eq!(ordering[refresh - 1], "check");
    assert_eq!(ordering[refreshed + 1], "check");
    assert_eq!(ordering.last(), Some(&"auth"));
}

#[cfg(feature = "web-search")]
#[tokio::test]
async fn continuation_never_refreshes_away_its_established_auth_revision() {
    use crate::custom_openai::envelope::InputProjection;
    use crate::custom_openai::tools::{RoundError, RoundOptions};
    let (_dir, path) = path();
    write_file(&path, &managed("synthetic-expired", "synthetic-refresh", 1)).unwrap();
    let revision = crate::custom_openai::custom_openai_auth_revision(&path);
    let client = CustomOpenAiClient::new(CustomOpenAiConfig {
        auth_path: path,
        ..Default::default()
    })
    .unwrap();
    let result = client
        .projected_round_with_refresh(
            "synthetic-session",
            InputProjection::mandatory(&[]),
            "Synthetic policy",
            &[],
            RoundOptions {
                deadline: tokio::time::Instant::now() + Duration::from_secs(1),
                expected_auth_revision: revision,
                require_tool: false,
            },
            || Ok(()),
            |_| panic!("expired continuation cannot dispatch or publish"),
            async { panic!("continuation must never initiate credential rotation") },
        )
        .await;
    assert!(matches!(
        result,
        Err(RoundError::Provider(CustomOpenAiError::AuthExpired))
    ));
}

#[tokio::test]
async fn concurrent_refresh_waiters_share_failure_instead_of_retrying_consumed_work() {
    let (_dir, path) = path();
    write_file(&path, &managed("synthetic-old", "synthetic-refresh", 1)).unwrap();
    let (release, gate) = oneshot::channel();
    let mut response = Reply::json(503, json!({"error":"synthetic-infrastructure-failure"}));
    response.gate = Some(gate);
    let mut mock = mock(vec![response]).await;
    let engine = mock.engine.clone();
    let refreshing = path.clone();
    let first = tokio::spawn(async move { engine.ensure_fresh(&refreshing).await });
    request(&mut mock, "/oauth/token").await;
    let engine = mock.engine.clone();
    let refreshing = path.clone();
    let second = tokio::spawn(async move { engine.ensure_fresh(&refreshing).await });
    tokio::time::sleep(Duration::from_millis(30)).await;
    release.send(()).unwrap();
    assert_eq!(first.await.unwrap(), Err(AuthError::Unavailable));
    assert_eq!(second.await.unwrap(), Err(AuthError::Unavailable));
    assert!(can_refresh(&path));
    assert!(mock.requests.try_recv().is_err());
}
