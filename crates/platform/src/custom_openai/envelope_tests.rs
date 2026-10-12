use super::*;
#[cfg(feature = "web-search")]
use serde_json::json;

fn message(role: CustomOpenAiRole, text: &str) -> CustomOpenAiMessage {
    CustomOpenAiMessage {
        role,
        content_type: CustomOpenAiContentType::OutputText,
        text: text.into(),
    }
}

fn prompt(history: Vec<CustomOpenAiMessage>) -> CustomOpenAiPrompt {
    CustomOpenAiPrompt {
        session_id: "synthetic-envelope".into(),
        prompt: "Synthetic current".into(),
        history,
    }
}

#[test]
fn escaping_evicts_only_oldest_complete_pair_without_mutating_history() {
    let history = vec![
        message(CustomOpenAiRole::User, "Synthetic first"),
        message(CustomOpenAiRole::Assistant, &"\"".repeat(100 * 1024)),
        message(CustomOpenAiRole::User, "Synthetic second"),
        message(CustomOpenAiRole::Assistant, &"\"".repeat(100 * 1024)),
    ];
    let prompt = prompt(history.clone());
    let bytes = checked_request(&CustomOpenAiConfig::default(), &prompt, None).unwrap();
    assert!(bytes.len() <= MAX_REQUEST_BYTES);
    let request: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(request["input"].as_array().unwrap().len(), 3);
    assert_eq!(
        request["input"][0]["content"][0]["text"],
        "Synthetic second"
    );
    assert_eq!(prompt.history, history);
}

#[test]
fn unpaired_refusal_and_current_are_mandatory_and_preserve_utf8_controls() {
    let mut orphan = message(
        CustomOpenAiRole::Assistant,
        "Synthetic refusal\n\u{0001}界\\\"",
    );
    orphan.content_type = CustomOpenAiContentType::Refusal;
    let mut prompt = prompt(vec![orphan.clone()]);
    prompt.prompt = "Synthetic current\n界".into();
    let bytes = checked_request(&CustomOpenAiConfig::default(), &prompt, None).unwrap();
    let request: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(request["input"][0]["content"][0]["refusal"], orphan.text);
    assert_eq!(request["input"][1]["content"][0]["text"], prompt.prompt);
    prompt.history[0].text = "\"".repeat(MAX_REQUEST_BYTES);
    assert_eq!(
        checked_request(&CustomOpenAiConfig::default(), &prompt, None),
        Err(CustomOpenAiError::TooLarge)
    );
}

#[test]
fn final_wire_buffer_has_exact_limit_and_one_more_byte_rejects() {
    let config = CustomOpenAiConfig::default();
    let mut prompt = prompt(vec![]);
    let overhead = checked_request(&config, &prompt, None).unwrap().len() - prompt.prompt.len();
    prompt.prompt = "x".repeat(MAX_REQUEST_BYTES - overhead);
    assert_eq!(
        checked_request(&config, &prompt, None).unwrap().len(),
        MAX_REQUEST_BYTES
    );
    prompt.prompt.push('x');
    assert_eq!(
        checked_request(&config, &prompt, None),
        Err(CustomOpenAiError::TooLarge)
    );
}

#[cfg(feature = "web-search")]
#[test]
fn opaque_continuation_and_linked_results_are_never_evicted() {
    use super::envelope::{checked_envelope, InputProjection};
    let config = CustomOpenAiConfig::default();
    let prompt = prompt(vec![
        message(CustomOpenAiRole::User, "Synthetic old"),
        message(CustomOpenAiRole::Assistant, &"\"".repeat(100 * 1024)),
    ]);
    let continuation = vec![
        json!({"type":"reasoning","encrypted_content":"synthetic-opaque"}),
        json!({"type":"function_call","call_id":"synthetic-call","name":"web_search","arguments":"{}"}),
        json!({"type":"function_call_output","call_id":"synthetic-call","output":"x".repeat(220 * 1024)}),
    ];
    let bytes = checked_envelope(
        &config,
        &prompt.session_id,
        InputProjection::initial(&prompt, &continuation),
        "Synthetic policy",
        Some(&[]),
        false,
    )
    .unwrap();
    let request: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(request["input"].as_array().unwrap().len(), 4);
    assert_eq!(&request["input"].as_array().unwrap()[1..], &continuation);
    let mut impossible = continuation;
    impossible[2]["output"] = json!("\"".repeat(MAX_REQUEST_BYTES));
    assert_eq!(
        checked_envelope(
            &config,
            &prompt.session_id,
            InputProjection::initial(&prompt, &impossible),
            "Synthetic policy",
            Some(&[]),
            false
        ),
        Err(CustomOpenAiError::TooLarge)
    );
}

#[tokio::test]
async fn impossible_request_rejects_before_credentials_or_dispatch() {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let client = CustomOpenAiClient::new(CustomOpenAiConfig {
        endpoint: format!("http://{}", listener.local_addr().unwrap()),
        auth_path: PathBuf::from("/synthetic-unavailable/envelope-auth.json"),
        ..Default::default()
    })
    .unwrap();
    let mut prompt = prompt(vec![]);
    prompt.prompt = "\"".repeat(MAX_REQUEST_BYTES);
    let result = client
        .respond_with_auth_revision_and_dispatch(
            &prompt,
            None,
            |_| panic!("no text"),
            |_| panic!("no auth"),
            |_| panic!("no dispatch"),
        )
        .await;
    assert_eq!(result, Err(CustomOpenAiError::TooLarge));
    assert!(
        tokio::time::timeout(Duration::from_millis(20), listener.accept())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn transport_sends_the_exact_checked_json_buffer_for_direct_and_tool_requests() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    static ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let path = std::env::temp_dir().join(format!(
        "vox-synthetic-wire-{}-{}.json",
        std::process::id(),
        ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    std::fs::write(
        &path,
        r#"{"openai":{"access":"synthetic-wire","expires":4102444800}}"#,
    )
    .unwrap();
    for tool_route in [false, true] {
        if tool_route && !cfg!(feature = "web-search") {
            continue;
        }
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let config = CustomOpenAiConfig {
            endpoint: format!("http://{}", listener.local_addr().unwrap()),
            auth_path: path.clone(),
            ..Default::default()
        };
        let prompt = prompt(vec![message(
            CustomOpenAiRole::Assistant,
            "Synthetic orphan 界\n\\\"",
        )]);
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let (end, length) = loop {
                let mut buffer = [0_u8; 4096];
                let count = socket.read(&mut buffer).await.unwrap();
                assert_ne!(count, 0);
                request.extend_from_slice(&buffer[..count]);
                if let Some(end) = request.windows(4).position(|window| window == b"\r\n\r\n") {
                    let headers = String::from_utf8_lossy(&request[..end]).to_ascii_lowercase();
                    assert!(headers.contains("content-type: application/json"));
                    let length: usize = headers
                        .lines()
                        .find_map(|line| line.strip_prefix("content-length:"))
                        .unwrap()
                        .trim()
                        .parse()
                        .unwrap();
                    if request.len() >= end + 4 + length {
                        break (end + 4, length);
                    }
                }
            };
            let body = b"data: {\"type\":\"response.output_text.delta\",\"delta\":\"Synthetic answer\"}\n\ndata: {\"type\":\"response.completed\",\"response\":{}}\n\n";
            socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).as_bytes()).await.unwrap();
            socket.write_all(body).await.unwrap();
            request[end..end + length].to_vec()
        });
        let client = CustomOpenAiClient::new(config.clone()).unwrap();
        let expected;
        if tool_route {
            #[cfg(feature = "web-search")]
            {
                use super::envelope::{checked_envelope, InputProjection};
                expected = checked_envelope(
                    &config,
                    &prompt.session_id,
                    InputProjection::initial(&prompt, &[]),
                    "Synthetic policy",
                    Some(&[]),
                    false,
                )
                .unwrap();
                client
                    .projected_round_with_events(
                        &prompt.session_id,
                        InputProjection::initial(&prompt, &[]),
                        "Synthetic policy",
                        &[],
                        tools::RoundOptions {
                            deadline: tokio::time::Instant::now() + Duration::from_secs(3),
                            expected_auth_revision: None,
                            require_tool: false,
                        },
                        || Ok(()),
                        |_| Ok(()),
                    )
                    .await
                    .unwrap();
            }
            #[cfg(not(feature = "web-search"))]
            unreachable!();
        } else {
            expected = checked_request(&config, &prompt, None).unwrap();
            client.respond(&prompt, |_| {}).await.unwrap();
        }
        assert_eq!(server.await.unwrap(), expected);
    }
    std::fs::remove_file(path).unwrap();
}

#[cfg(feature = "web-search")]
#[tokio::test]
async fn tool_mandatory_overflow_and_expired_deadline_do_not_read_auth_or_dispatch() {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let client = CustomOpenAiClient::new(CustomOpenAiConfig {
        endpoint: format!("http://{}", listener.local_addr().unwrap()),
        auth_path: PathBuf::from("/synthetic-unavailable/tool-auth.json"),
        ..Default::default()
    })
    .unwrap();
    let input = [json!({"type":"reasoning","encrypted_content":"\"".repeat(MAX_REQUEST_BYTES)})];
    for expired in [false, true] {
        let result = client
            .tool_round_with_events(
                tools::RoundRequest {
                    session_id: "synthetic-mandatory",
                    input: &input,
                    instructions: "Synthetic policy",
                    tools: &[],
                },
                tools::RoundOptions {
                    deadline: tokio::time::Instant::now()
                        + if expired {
                            Duration::ZERO
                        } else {
                            Duration::from_secs(1)
                        },
                    expected_auth_revision: None,
                    require_tool: false,
                },
                |_| panic!("no auth or dispatch event"),
            )
            .await;
        let expected = if expired {
            CustomOpenAiError::Timeout
        } else {
            CustomOpenAiError::TooLarge
        };
        assert!(matches!(result, Err(tools::RoundError::Provider(error)) if error == expected));
    }
    assert!(
        tokio::time::timeout(Duration::from_millis(20), listener.accept())
            .await
            .is_err()
    );
}
