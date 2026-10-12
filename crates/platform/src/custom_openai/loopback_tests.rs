use super::*;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const CHILD_TEST: &str = "custom_openai::loopback_tests::constructor_dispatch_child";
const CHILD_ENDPOINT: &str = "VOX_SYNTHETIC_LOOPBACK_ENDPOINT";
const CHILD_AUTH: &str = "VOX_SYNTHETIC_LOOPBACK_AUTH";
static FIXTURE_ID: AtomicU64 = AtomicU64::new(0);

struct AuthFixture(PathBuf);
impl AuthFixture {
    fn new() -> Self {
        use std::io::Write;
        let path = std::env::temp_dir().join(format!(
            "vox-synthetic-loopback-auth-{}-{}.json",
            std::process::id(),
            FIXTURE_ID.fetch_add(1, Ordering::Relaxed)
        ));
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&path).expect("synthetic auth fixture");
        let fixture = Self(path);
        file.write_all(br#"{"openai":{"access":"synthetic-loopback-token","accountId":"synthetic-loopback-account","expires":4102444800}}"#).unwrap();
        fixture
    }
}
impl Drop for AuthFixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

struct HarnessChild(Child);
impl HarnessChild {
    async fn wait(&mut self) -> std::process::ExitStatus {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(status) = self.0.try_wait().expect("synthetic child status") {
                return status;
            }
            assert!(
                Instant::now() < deadline,
                "synthetic constructor child exceeded its deadline"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
}
impl Drop for HarnessChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn constructor_dispatch_child() {
    // The parent regression invokes this exact helper in a fresh harness process
    // with synthetic endpoints/auth; ordinary suite discovery does not dispatch.
    let Ok(endpoint) = std::env::var(CHILD_ENDPOINT) else {
        return;
    };
    let auth_path = PathBuf::from(std::env::var_os(CHILD_AUTH).expect("synthetic child auth path"));
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let config = CustomOpenAiConfig {
            endpoint: endpoint.clone(),
            auth_path,
            connect_timeout: Duration::from_secs(2),
            idle_timeout: Duration::from_secs(2),
            total_timeout: Duration::from_secs(3),
            ..Default::default()
        };
        let owner = CustomOpenAiClient::new(config.clone()).expect("accepted loopback spelling");
        // Trimming remains compatible; this fix must not broaden cross-origin pool reuse.
        let mut request_config = config.clone();
        request_config.endpoint = format!(" {endpoint} ");
        let client = owner.with_reused_transport(request_config).unwrap();
        let changed_origin = CustomOpenAiConfig {
            endpoint: DEFAULT_ENDPOINT.into(),
            ..config
        };
        assert!(matches!(
            owner.with_reused_transport(changed_origin),
            Err(CustomOpenAiError::InvalidConfig)
        ));
        let response = client
            .respond(
                &CustomOpenAiPrompt {
                    session_id: "synthetic-loopback-session".into(),
                    prompt: "Synthetic loopback question".into(),
                    history: vec![],
                },
                |_| {},
            )
            .await
            .expect("direct synthetic loopback response");
        assert_eq!(response.text, "Synthetic loopback answer");
    });
}

#[tokio::test]
async fn public_constructor_bypasses_ambient_proxy_for_accepted_loopback_spellings() {
    for (host, scheme) in [
        ("127.0.0.1", "http://"),
        ("127.0.0.1", "http:/"),
        ("::1", "http:/"),
        ("127.0.0.1", "HTTP:/"),
    ] {
        let origin = TcpListener::bind((host, 0))
            .await
            .expect("synthetic loopback listener");
        let endpoint = format!("{scheme}{}/responses", origin.local_addr().unwrap());
        assert!(
            valid_endpoint(&endpoint),
            "fixture spelling must be accepted by production validation"
        );
        assert_eq!(Url::parse(&endpoint).unwrap().scheme(), "http");
        let proxy = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let proxy_url = format!("http://{}", proxy.local_addr().unwrap());
        let proxy_hit = Arc::new(AtomicBool::new(false));
        let observed_proxy = proxy_hit.clone();
        let (stop_proxy, stopped_proxy) = tokio::sync::oneshot::channel::<()>();
        let proxy_task = tokio::spawn(async move {
            let accepted = tokio::select! {
                biased;
                result = proxy.accept() => Some(result.unwrap()),
                _ = stopped_proxy => None,
            };
            if let Some((mut socket, _)) = accepted {
                observed_proxy.store(true, Ordering::Release);
                let _ = socket.write_all(b"HTTP/1.1 502 Synthetic Proxy\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await;
            }
        });
        let origin_task = tokio::spawn(async move {
            tokio::time::timeout(Duration::from_secs(8), async move {
                let (mut socket, _) = origin.accept().await.unwrap();
                let mut request = Vec::new();
                loop {
                    let mut buffer = [0_u8; 4096];
                    let count = socket.read(&mut buffer).await.unwrap();
                    assert_ne!(count, 0);
                    request.extend_from_slice(&buffer[..count]);
                    assert!(request.len() <= MAX_REQUEST_BYTES + 16 * 1024);
                    if let Some(end) = request.windows(4).position(|window| window == b"\r\n\r\n") {
                        let headers = String::from_utf8_lossy(&request[..end]).to_ascii_lowercase();
                        let length: usize = headers.lines().find_map(|line| line.strip_prefix("content-length:")).unwrap().trim().parse().unwrap();
                        if request.len() < end + 4 + length { continue; }
                        assert!(headers.starts_with("post /responses "));
                        assert!(headers.contains("authorization: bearer synthetic-loopback-token"), "synthetic authorization header missing");
                        assert!(headers.contains("chatgpt-account-id: synthetic-loopback-account"), "synthetic account header missing");
                        let body: Value = serde_json::from_slice(&request[end + 4..end + 4 + length]).unwrap();
                        assert_eq!(body["input"][0]["content"][0]["text"], "Synthetic loopback question");
                        break;
                    }
                }
                let body = b"data: {\"type\":\"response.output_text.delta\",\"delta\":\"Synthetic loopback answer\"}\n\ndata: {\"type\":\"response.completed\",\"response\":{}}\n\n";
                socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).as_bytes()).await.unwrap();
                socket.write_all(body).await.unwrap();
            }).await.expect("synthetic origin request deadline");
        });
        let auth = AuthFixture::new();
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args(["--exact", CHILD_TEST, "--nocapture"])
            .env(CHILD_ENDPOINT, &endpoint)
            .env(CHILD_AUTH, &auth.0)
            .stdout(Stdio::null())
            .stderr(Stdio::inherit());
        // All proxy changes are confined to a fresh test-harness process. Clearing
        // bypass/CGI settings ensures the injected synthetic proxy is observable.
        for key in [
            "HTTP_PROXY",
            "http_proxy",
            "HTTPS_PROXY",
            "https_proxy",
            "ALL_PROXY",
            "all_proxy",
            "NO_PROXY",
            "no_proxy",
            "REQUEST_METHOD",
        ] {
            command.env_remove(key);
        }
        for key in ["HTTP_PROXY", "http_proxy", "ALL_PROXY", "all_proxy"] {
            command.env(key, &proxy_url);
        }
        let mut child = HarnessChild(command.spawn().expect("synthetic constructor child"));
        let status = child.wait().await;
        let _ = stop_proxy.send(());
        proxy_task.await.unwrap();
        assert!(
            !proxy_hit.load(Ordering::Acquire),
            "accepted loopback request reached the synthetic ambient proxy"
        );
        assert!(status.success(), "actual constructor/dispatch child failed");
        origin_task.await.unwrap();
    }
}
