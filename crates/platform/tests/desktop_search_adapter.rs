#![cfg(feature = "web-search")]
// Compile the actual desktop search adapter without the unrelated Tauri/audio build.
#![allow(dead_code)]

use std::sync::atomic::AtomicBool;

#[derive(Clone, Copy)]
struct PromptCancellation<'a> {
    cancelled: &'a AtomicBool,
    signal: &'a tokio::sync::watch::Sender<bool>,
}
struct SourcePayload {
    url: String,
    title: String,
}
enum PromptExecutionEventPayload {
    Text {
        text: String,
    },
    Tool {
        tool: String,
        status: String,
        detail: String,
    },
    Status {
        message: String,
    },
    Sources {
        sources: Vec<SourcePayload>,
    },
}

#[path = "../../../apps/desktop-tauri/src-tauri/src/search_runtime.rs"]
mod search_runtime;

#[tokio::test]
async fn actual_desktop_quick_web_off_trims_escaped_history_and_streams_using_the_owned_client() {
    use std::{
        sync::atomic::{AtomicU64, Ordering},
        time::Duration,
    };
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };
    use voxgolem_platform::custom_openai::*;
    static ID: AtomicU64 = AtomicU64::new(0);
    let path = std::env::temp_dir().join(format!(
        "vox-desktop-synthetic-wire-{}-{}.json",
        std::process::id(),
        ID.fetch_add(1, Ordering::Relaxed)
    ));
    struct AuthFixture(std::path::PathBuf);
    impl Drop for AuthFixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }
    let auth = AuthFixture(path);
    std::fs::write(
        &auth.0,
        r#"{"openai":{"access":"synthetic-desktop","expires":4102444800}}"#,
    )
    .unwrap();
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let config = CustomOpenAiConfig {
        endpoint: format!("http://{}", listener.local_addr().unwrap()),
        auth_path: auth.0.clone(),
        total_timeout: Duration::from_secs(3),
        ..Default::default()
    };
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut bytes = Vec::new();
        let wire = loop {
            let mut buffer = [0_u8; 4096];
            let count = socket.read(&mut buffer).await.unwrap();
            assert_ne!(count, 0);
            bytes.extend_from_slice(&buffer[..count]);
            if let Some(end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
                let headers = String::from_utf8_lossy(&bytes[..end]).to_ascii_lowercase();
                let length: usize = headers
                    .lines()
                    .find_map(|line| line.strip_prefix("content-length:"))
                    .unwrap()
                    .trim()
                    .parse()
                    .unwrap();
                assert!(length <= MAX_REQUEST_BYTES);
                if bytes.len() >= end + 4 + length {
                    break serde_json::from_slice::<serde_json::Value>(
                        &bytes[end + 4..end + 4 + length],
                    )
                    .unwrap();
                }
            }
        };
        let response = b"data: {\"type\":\"response.output_text.delta\",\"delta\":\"Synthetic answer\"}\n\ndata: {\"type\":\"response.completed\",\"response\":{}}\n\n";
        socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", response.len()).as_bytes()).await.unwrap();
        socket.write_all(response).await.unwrap();
        wire
    });
    let client = CustomOpenAiClient::new(config.clone()).unwrap();
    let prompt = CustomOpenAiPrompt {
        session_id: "synthetic-desktop-envelope".into(),
        prompt: "Synthetic current".into(),
        history: (0..2)
            .flat_map(|index| {
                [
                    CustomOpenAiMessage {
                        role: CustomOpenAiRole::User,
                        content_type: CustomOpenAiContentType::OutputText,
                        text: format!("Synthetic question {index}"),
                    },
                    CustomOpenAiMessage {
                        role: CustomOpenAiRole::Assistant,
                        content_type: CustomOpenAiContentType::OutputText,
                        text: "\"".repeat(100 * 1024),
                    },
                ]
            })
            .collect(),
    };
    let state = search_runtime::DesktopSearchState::default();
    let cancelled = AtomicBool::new(false);
    let (signal, _) = tokio::sync::watch::channel(false);
    let mut delivered = String::new();
    let (result, revision) = search_runtime::run_custom(
        &state,
        &client,
        config,
        &prompt,
        voxgolem_core::search::Mode::Quick,
        false,
        voxgolem_core::conversation::SessionId(1),
        PromptCancellation {
            cancelled: &cancelled,
            signal: &signal,
        },
        |event| {
            if let PromptExecutionEventPayload::Text { text } = event {
                delivered.push_str(&text);
            }
            Ok(())
        },
    )
    .await
    .unwrap();
    assert_eq!(delivered, "Synthetic answer");
    assert_eq!(result.response.text, delivered);
    assert!(revision.is_some());
    assert_eq!(result.searches, 0);
    assert_eq!(result.reads, 0);
    let wire = server.await.unwrap();
    assert_eq!(wire["input"].as_array().unwrap().len(), 3);
    assert_eq!(
        wire["input"][0]["content"][0]["text"],
        "Synthetic question 1"
    );
    assert_eq!(wire["tools"], serde_json::json!([]));
    assert_eq!(prompt.history.len(), 4);
    state.shutdown().await;
}

#[test]
fn actual_desktop_source_links_survive_review_validation_for_both_providers() {
    use voxgolem_core::{
        agent_pipeline::{ReviewInput, StageStatus},
        assistant::Content,
        search::Provider,
    };
    use voxgolem_platform::web::{decode_result, Operation};
    for provider in [Provider::Parallel, Provider::Exa] {
        for title in [None, Some(""), Some("  ")] {
            let value = match provider {
                Provider::Parallel => {
                    let mut item = serde_json::json!({"url":"https://example.com","excerpts":["Synthetic source evidence"]});
                    if let Some(title) = title {
                        item["title"] = title.into();
                    }
                    serde_json::json!({"structuredContent":{"results":[item]}})
                }
                Provider::Exa => {
                    let heading = title
                        .map(|title| format!("Title: {title}\n"))
                        .unwrap_or_default();
                    serde_json::json!({"content":[{"type":"text","text":format!("{heading}URL: https://example.com\nHighlights:\nSynthetic evidence")}]})
                }
            };
            let outcome = decode_result(
                provider,
                &Operation::Search {
                    objective: "Synthetic".into(),
                    queries: vec!["Synthetic".into()],
                    limit: 3,
                },
                &value,
                "synthetic-run",
                &mut 0,
            )
            .unwrap();
            let links = search_runtime::source_links(&outcome.sources);
            assert_eq!(links[0].title, "https://example.com/");
            voxgolem_core::agent_pipeline::validate_review_input(&ReviewInput {
                original_request: "Synthetic question".into(),
                canonical_history: vec![],
                instant: StageStatus::Success(Content::Text("Synthetic answer".into())),
                deep: StageStatus::Failure("Disabled".into()),
                materiality_policy: "Material facts only".into(),
                sources: links,
            })
            .unwrap();
        }
    }
}

#[test]
fn actual_desktop_persistence_uses_plain_deduplicated_urls_and_preserves_refusal() {
    use voxgolem_core::{
        assistant::Content,
        search::{EvidenceKind, Provider, Source},
    };
    let source = Source {
        id: "synthetic-source".into(),
        url: "https://example.invalid/synthetic".into(),
        title: "Synthetic source".into(),
        provider: Provider::Parallel,
        kind: EvidenceKind::PageRead,
        text: "Synthetic evidence".into(),
        retrieved_unix_ms: 1,
        publisher_date: None,
        truncated: false,
    };
    let sources = vec![source.clone(), source];
    match search_runtime::persisted_content(
        Content::Text("Synthetic answer with literal -2".into()),
        &sources,
    ) {
        Content::Text(text) => assert_eq!(
            text,
            "Synthetic answer with literal -2\n\nSources:\nhttps://example.invalid/synthetic\n"
        ),
        _ => panic!("text must retain its content kind"),
    }
    match search_runtime::persisted_content(Content::Refusal("Synthetic refusal".into()), &sources)
    {
        Content::Refusal(text) => assert_eq!(text, "Synthetic refusal"),
        _ => panic!("refusal must retain its content kind"),
    }
}
