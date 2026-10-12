#![cfg(feature = "web-search")]

use serde_json::{json, Value};
use std::{
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    time::Instant,
};
use voxgolem_core::search::*;
use voxgolem_platform::custom_openai::tools::ModelEvent;
use voxgolem_platform::{custom_openai::*, search::*, search_control::Cancellation, web::*};

static FIXTURE_ID: AtomicU64 = AtomicU64::new(0);

struct AuthFixture(std::path::PathBuf);
impl Drop for AuthFixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

struct EvidenceTransport {
    calls: Vec<(Provider, bool)>,
}
impl WebTransport for EvidenceTransport {
    async fn call(
        &mut self,
        provider: Provider,
        operation: &Operation,
        _: &str,
        _: Instant,
        _: &Cancellation,
    ) -> Result<Value, ProviderFailure> {
        self.calls.push((provider, operation.fetch()));
        Ok(
            json!({"structuredContent":{"results":[{"url":"https://example.com/source", "title":"Synthetic source", "excerpts":["Synthetic fact from source"]}],"errors":[]}}),
        )
    }
}

fn call(id: &str, name: &str, args: Value) -> Value {
    json!({"type":"function_call", "id":format!("synthetic-item-{id}"),"call_id":id,"name":name,"arguments":args.to_string(),"status":"completed"})
}
fn search_call(id: &str) -> Value {
    call(
        id,
        "web_search",
        json!({"objective":"Synthetic objective","queries":["Synthetic query"],"num_results":3}),
    )
}
fn answer() -> Value {
    json!({"type":"message","id":"synthetic-answer","role":"assistant","status":"completed","content":[{"type":"output_text","text":"Synthetic final answer."}]})
}

async fn fixture(
    rounds: Vec<Vec<Value>>,
) -> (
    CustomOpenAiConfig,
    AuthFixture,
    tokio::task::JoinHandle<Vec<Value>>,
) {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let path = std::env::temp_dir().join(format!(
        "vox-search-synthetic-auth-{}-{}.json",
        std::process::id(),
        FIXTURE_ID.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::write(&path,r#"{"openai":{"access":"synthetic-access","accountId":"synthetic-account","expires":4102444800}}"#).unwrap();
    let task = tokio::spawn(async move {
        let mut requests = vec![];
        for output in rounds {
            let (mut socket, _) = tokio::time::timeout(Duration::from_secs(5), listener.accept())
                .await
                .unwrap()
                .unwrap();
            let mut bytes = vec![];
            let mut buffer = [0; 4096];
            loop {
                let count = socket.read(&mut buffer).await.unwrap();
                assert_ne!(count, 0, "fixture request unexpectedly closed");
                bytes.extend_from_slice(&buffer[..count]);
                if let Some(end) = bytes.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
                    let headers = String::from_utf8_lossy(&bytes[..end]).to_ascii_lowercase();
                    let length = headers
                        .lines()
                        .find_map(|line| {
                            line.strip_prefix("content-length:")?
                                .trim()
                                .parse::<usize>()
                                .ok()
                        })
                        .unwrap();
                    if bytes.len() >= end + 4 + length {
                        assert!(headers.contains("authorization: bearer synthetic-access"));
                        assert!(headers.contains("originator: opencode"));
                        requests.push(
                            serde_json::from_slice(&bytes[end + 4..end + 4 + length]).unwrap(),
                        );
                        break;
                    }
                }
            }
            socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n").await.unwrap();
            for item in &output {
                let event = if item["type"]
                    .as_str()
                    .is_some_and(|kind| kind.starts_with("response."))
                {
                    item.clone()
                } else {
                    json!({"type":"response.output_item.done","item":item})
                };
                let data = format!("data: {event}\r\n\r\n");
                for chunk in data.as_bytes().chunks(3) {
                    if socket.write_all(chunk).await.is_err() {
                        return requests;
                    }
                }
            }
            let items = output
                .into_iter()
                .filter(|item| {
                    !item["type"]
                        .as_str()
                        .is_some_and(|kind| kind.starts_with("response."))
                })
                .collect::<Vec<_>>();
            let terminal = json!({"type":"response.completed","response":{"status":"completed","output":items}});
            let _ = socket
                .write_all(format!("data: {terminal}\r\n\r\n").as_bytes())
                .await;
        }
        requests
    });
    (
        CustomOpenAiConfig {
            endpoint,
            auth_path: path.clone(),
            total_timeout: Duration::from_secs(5),
            ..Default::default()
        },
        AuthFixture(path),
        task,
    )
}

fn prompt() -> CustomOpenAiPrompt {
    CustomOpenAiPrompt {
        session_id: "synthetic-conversation".into(),
        prompt: "Synthetic current question".into(),
        history: vec![
            CustomOpenAiMessage {
                role: CustomOpenAiRole::User,
                content_type: CustomOpenAiContentType::OutputText,
                text: "Synthetic earlier question".into(),
            },
            CustomOpenAiMessage {
                role: CustomOpenAiRole::Assistant,
                content_type: CustomOpenAiContentType::Refusal,
                text: "Synthetic earlier refusal".into(),
            },
        ],
    }
}

#[tokio::test]
async fn exact_current_request_history_auth_and_opaque_tool_continuation_reach_final_answer() {
    let reasoning = json!({"type":"reasoning","id":"synthetic-reasoning","summary":[],"encrypted_content":"synthetic-opaque"});
    let (config, _auth, server) = fixture(vec![
        vec![reasoning.clone(), search_call("synthetic-search")],
        vec![call(
            "synthetic-fetch",
            "web_fetch",
            json!({"urls":["https://example.com/source"],"focus":"Synthetic focus"}),
        )],
        vec![answer()],
    ])
    .await;
    let mut web = WebService::new(EvidenceTransport { calls: vec![] });
    let result = run(
        config,
        &prompt(),
        Mode::Verify,
        &mut web,
        &Cancellation::default(),
    )
    .await
    .unwrap();
    assert_eq!((result.rounds, result.searches, result.reads), (3, 1, 1));
    assert!(result.source_read_observed);
    assert!(!result.partial);
    assert!(result.response.text.contains("Synthetic final answer."));
    assert!(result
        .persisted_text()
        .contains("Sources:\nhttps://example.com/source"));
    let requests = server.await.unwrap();
    assert_eq!(requests[0]["model"], "gpt-6.1-sol");
    assert_eq!(requests[0]["reasoning"]["effort"], "high");
    assert_eq!(requests[0]["stream"], true);
    assert_eq!(requests[0]["store"], false);
    assert_eq!(requests[0]["parallel_tool_calls"], false);
    assert_eq!(requests[0]["input"][1]["content"][0]["type"], "refusal");
    assert_eq!(requests[1]["input"][3], reasoning);
    assert_eq!(requests[1]["input"][4]["call_id"], "synthetic-search");
    assert_eq!(requests[1]["input"][5]["type"], "function_call_output");
    assert_eq!(requests[1]["input"][5]["call_id"], "synthetic-search");
    assert!(requests[1]["input"][5].get("id").is_none());
    assert_eq!(requests[2]["input"][7]["call_id"], "synthetic-fetch");
}

#[tokio::test]
async fn dispatch_is_forwarded_for_each_round_while_text_waits_for_readable_evidence() {
    let filtered = json!({"type":"response.output_text.delta","delta":"Synthetic withheld text"});
    let (config, _auth, server) = fixture(vec![
        vec![filtered.clone(), search_call("synthetic-search")],
        vec![
            filtered,
            call(
                "synthetic-read",
                "web_fetch",
                json!({"urls":["https://example.com/source"],"focus":"Synthetic focus"}),
            ),
        ],
        vec![answer()],
    ])
    .await;
    let mut web = WebService::new(EvidenceTransport { calls: vec![] });
    let mut dispatches = Vec::new();
    let mut texts = Vec::new();
    let result = run_with_events(
        config,
        &prompt(),
        Mode::Verify,
        RunContext {
            conversation_id: "synthetic-conversation",
            instructions: "Synthetic policy",
            explicit_search: false,
            web_enabled: true,
        },
        &mut web,
        &Cancellation::default(),
        |event| {
            match event {
                RunEvent::Model(ModelEvent::RequestDispatched(instant)) => {
                    dispatches.push(instant);
                    assert!(texts.is_empty());
                }
                RunEvent::Model(ModelEvent::TextDelta { text, .. }) => {
                    assert_eq!(dispatches.len(), 3);
                    texts.push(text.to_string());
                }
                _ => {}
            }
            Ok(())
        },
    )
    .await
    .unwrap();
    server.await.unwrap();
    assert_eq!(dispatches.len(), result.rounds);
    assert!(dispatches.windows(2).all(|pair| pair[0] <= pair[1]));
    assert_eq!(texts, ["Synthetic final answer."]);
    assert!(result.source_read_observed);
}

#[tokio::test]
async fn quick_budget_prevents_third_search_and_reserves_tools_disabled_final_round() {
    let (config, _auth, server) = fixture(vec![
        vec![search_call("synthetic-one")],
        vec![search_call("synthetic-two")],
        vec![search_call("synthetic-three")],
        vec![answer()],
    ])
    .await;
    let mut web = WebService::new(EvidenceTransport { calls: vec![] });
    let result = run(
        config,
        &prompt(),
        Mode::Quick,
        &mut web,
        &Cancellation::default(),
    )
    .await
    .unwrap();
    assert_eq!(result.searches, 2);
    assert_eq!(web.transport.calls.len(), 2);
    assert!(result.partial);
    let requests = server.await.unwrap();
    assert_eq!(requests[3]["tool_choice"], "none");
    assert_eq!(requests[3]["tools"], json!([]));
    assert!(
        requests[3]["input"].as_array().unwrap().last().unwrap()["output"]
            .as_str()
            .unwrap()
            .contains("budget")
    );
}

#[tokio::test]
async fn replayed_call_uses_cached_result_and_conflicting_reuse_never_reexecutes() {
    for conflicting in [false, true] {
        let repeated = if conflicting {
            call(
                "synthetic-repeat",
                "web_fetch",
                json!({"urls":["https://example.com/source"],"focus":""}),
            )
        } else {
            search_call("synthetic-repeat")
        };
        let rounds = if conflicting {
            vec![vec![search_call("synthetic-repeat")], vec![repeated]]
        } else {
            vec![
                vec![search_call("synthetic-repeat")],
                vec![repeated],
                vec![answer()],
            ]
        };
        let (config, _auth, server) = fixture(rounds).await;
        let mut web = WebService::new(EvidenceTransport { calls: vec![] });
        let result = run(
            config,
            &prompt(),
            Mode::Quick,
            &mut web,
            &Cancellation::default(),
        )
        .await;
        if conflicting {
            assert!(matches!(result, Err(RunError::Protocol)));
        } else {
            assert!(result.is_ok());
        }
        assert_eq!(web.transport.calls.len(), 1);
        server.await.unwrap();
    }
}

#[tokio::test]
async fn malformed_arguments_are_returned_to_model_without_network_execution() {
    let mut invalid = search_call("synthetic-invalid");
    invalid["arguments"] = json!("{unfinished");
    let (config, _auth, server) = fixture(vec![vec![invalid], vec![answer()]]).await;
    let mut web = WebService::new(EvidenceTransport { calls: vec![] });
    let result = run(
        config,
        &prompt(),
        Mode::Quick,
        &mut web,
        &Cancellation::default(),
    )
    .await
    .unwrap();
    assert!(web.transport.calls.is_empty());
    assert!(result.partial);
    assert!(!result.source_read_observed);
    let requests = server.await.unwrap();
    assert!(
        requests[1]["input"].as_array().unwrap().last().unwrap()["output"]
            .as_str()
            .unwrap()
            .contains("invalid_arguments")
    );
}

#[tokio::test]
async fn verify_and_research_cannot_settle_without_retrieval_or_explicit_failure() {
    for mode in [Mode::Verify, Mode::Research] {
        let (config, _auth, server) = fixture(vec![vec![answer()]]).await;
        let mut web = WebService::new(EvidenceTransport { calls: vec![] });
        let outcome = run(config, &prompt(), mode, &mut web, &Cancellation::default()).await;
        assert!(matches!(outcome, Err(RunError::Tool(Failure::Unsupported))));
        let requests = server.await.unwrap();
        assert_eq!(requests[0]["tool_choice"], "required");
        assert!(web.transport.calls.is_empty());
    }
}

#[tokio::test]
async fn changed_credentials_are_rejected_before_sending_continuation() {
    struct ReplacingTransport(std::path::PathBuf);
    impl WebTransport for ReplacingTransport {
        async fn call(
            &mut self,
            _: Provider,
            _: &Operation,
            _: &str,
            _: Instant,
            _: &Cancellation,
        ) -> Result<Value, ProviderFailure> {
            std::fs::write(&self.0,r#"{"openai":{"access":"synthetic-replacement-access","accountId":"synthetic-replacement-account","expires":4102444800}}"#).unwrap();
            Ok(json!({"structuredContent":{"results":[]}}))
        }
    }
    let (config, auth, server) = fixture(vec![vec![search_call("synthetic-search")]]).await;
    let mut web = WebService::new(ReplacingTransport(auth.0.clone()));
    let result = run(
        config,
        &prompt(),
        Mode::Quick,
        &mut web,
        &Cancellation::default(),
    )
    .await;
    assert!(matches!(
        result,
        Err(RunError::Provider(CustomOpenAiError::AuthUnavailable))
    ));
    assert_eq!(server.await.unwrap().len(), 1);
}

#[tokio::test]
async fn pre_cancelled_run_never_calls_model_or_tools() {
    let cancel = Cancellation::default();
    cancel.cancel();
    let mut web = WebService::new(EvidenceTransport { calls: vec![] });
    assert!(matches!(
        run(
            CustomOpenAiConfig::default(),
            &prompt(),
            Mode::Quick,
            &mut web,
            &cancel
        )
        .await,
        Err(RunError::Tool(Failure::Cancelled))
    ));
    assert!(web.transport.calls.is_empty());
}

#[tokio::test]
async fn deadline_or_cancel_drops_provider_future_and_cannot_return_a_late_answer() {
    struct DropProof(Arc<std::sync::atomic::AtomicBool>);
    impl Drop for DropProof {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }
    for cancel_it in [false, true] {
        let cancel = Cancellation::default();
        let dropped = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let future = async {
            let _proof = DropProof(dropped.clone());
            std::future::pending::<()>().await;
            "Synthetic late answer"
        };
        let signal = async {
            if cancel_it {
                tokio::task::yield_now().await;
                cancel.cancel();
            }
        };
        let (outcome, ()) = tokio::join!(
            cancel.guard(Instant::now() + Duration::from_millis(5), future),
            signal
        );
        assert_eq!(
            outcome,
            Err(if cancel_it {
                Failure::Cancelled
            } else {
                Failure::Deadline
            })
        );
        assert!(dropped.load(Ordering::SeqCst));
    }
}

#[tokio::test]
async fn native_style_and_terminal_stream_callback_reach_the_current_provider_contract() {
    let (config, _auth, server) = fixture(vec![vec![answer()]]).await;
    let mut web = WebService::new(EvidenceTransport { calls: vec![] });
    let mut deltas = vec![];
    let mut revisions = 0;
    let result = run_with_events(
        config,
        &prompt(),
        Mode::Quick,
        RunContext {
            conversation_id: "synthetic-conversation-key",
            instructions: "Synthetic compact watch style.",
            explicit_search: false,
            web_enabled: true,
        },
        &mut web,
        &Cancellation::default(),
        |event| {
            match event {
                RunEvent::Model(ModelEvent::TextDelta { text, .. }) => {
                    deltas.push(text.to_string())
                }
                RunEvent::Model(ModelEvent::AuthRevision(_)) => revisions += 1,
                _ => {}
            }
            Ok(())
        },
    )
    .await
    .unwrap();
    assert_eq!(deltas, vec!["Synthetic final answer."]);
    assert_eq!(revisions, 1);
    assert!(result.response.timings.first_text_at.is_some());
    let requests = server.await.unwrap();
    assert!(requests[0]["instructions"]
        .as_str()
        .unwrap()
        .contains("Synthetic compact watch style."));
}

#[tokio::test]
async fn per_turn_model_ids_do_not_rotate_the_stable_parallel_conversation_identity() {
    struct IdentifiedTransport(Vec<String>);
    impl WebTransport for IdentifiedTransport {
        async fn call(
            &mut self,
            _: Provider,
            _: &Operation,
            conversation: &str,
            _: Instant,
            _: &Cancellation,
        ) -> Result<Value, ProviderFailure> {
            self.0.push(conversation.into());
            Ok(
                json!({"structuredContent":{"results":[{"url":"https://example.com/source","title":"Synthetic source","excerpts":["Synthetic evidence"]}]}}),
            )
        }
    }
    let mut web = WebService::new(IdentifiedTransport(vec![]));
    for turn in ["synthetic-turn-one", "synthetic-turn-two"] {
        let (config, _auth, server) =
            fixture(vec![vec![search_call("synthetic-call")], vec![answer()]]).await;
        let mut prompt = prompt();
        prompt.session_id = turn.into();
        let result = run_with_events(
            config,
            &prompt,
            Mode::Quick,
            RunContext {
                conversation_id: "synthetic-conversation-key",
                instructions: "Synthetic style.",
                explicit_search: false,
                web_enabled: true,
            },
            &mut web,
            &Cancellation::default(),
            |_| Ok(()),
        )
        .await
        .unwrap();
        assert!(result.sources[0].id.starts_with(turn));
        let requests = server.await.unwrap();
        assert_eq!(requests[0]["prompt_cache_key"], turn);
    }
    assert_eq!(
        web.transport.0,
        vec!["synthetic-conversation-key", "synthetic-conversation-key"]
    );
}

#[tokio::test]
async fn failed_native_text_consumer_prevents_a_completed_result() {
    let (config, _auth, server) = fixture(vec![vec![answer()]]).await;
    let mut web = WebService::new(EvidenceTransport { calls: vec![] });
    let result = run_with_events(
        config,
        &prompt(),
        Mode::Quick,
        RunContext {
            conversation_id: "synthetic-conversation-key",
            instructions: "Synthetic style.",
            explicit_search: false,
            web_enabled: true,
        },
        &mut web,
        &Cancellation::default(),
        |event| {
            if matches!(event, RunEvent::Model(ModelEvent::TextDelta { .. })) {
                Err(())
            } else {
                Ok(())
            }
        },
    )
    .await;
    assert!(matches!(result, Err(RunError::CallbackFailed)));
    server.await.unwrap();
}

#[tokio::test]
async fn verification_deltas_wait_for_page_evidence_and_tools_never_enter_the_text_stream() {
    let mut preliminary = answer();
    preliminary["content"][0]["text"] = json!("Synthetic unverified claim.");
    let (config, _auth, server) = fixture(vec![
        vec![preliminary, search_call("synthetic-search")],
        vec![call(
            "synthetic-fetch",
            "web_fetch",
            json!({"urls":["https://example.com/source"],"focus":""}),
        )],
        vec![answer()],
    ])
    .await;
    let mut web = WebService::new(EvidenceTransport { calls: vec![] });
    let mut deltas = vec![];
    let result = run_with_events(
        config,
        &prompt(),
        Mode::Verify,
        RunContext {
            conversation_id: "synthetic-conversation-key",
            instructions: "Synthetic style.",
            explicit_search: true,
            web_enabled: true,
        },
        &mut web,
        &Cancellation::default(),
        |event| {
            if let RunEvent::Model(ModelEvent::TextDelta { text, .. }) = event {
                deltas.push(text.to_string());
            }
            Ok(())
        },
    )
    .await
    .unwrap();
    assert_eq!(deltas, vec!["Synthetic final answer."]);
    assert!(result.source_read_observed);
    assert_eq!(result.response.text, "Synthetic final answer.");
    assert!(result
        .persisted_text()
        .contains("Sources:\nhttps://example.com/source"));
    server.await.unwrap();
}

#[tokio::test]
async fn native_parent_cancellation_and_closed_registration_prevent_network_admission() {
    for closed in [false, true] {
        let (sender, receiver) = tokio::sync::watch::channel(false);
        if closed {
            drop(sender);
        } else {
            sender.send(true).unwrap();
        }
        let cancel = Cancellation::from_receiver(receiver);
        let mut web = WebService::new(EvidenceTransport { calls: vec![] });
        assert!(matches!(
            run(
                CustomOpenAiConfig::default(),
                &prompt(),
                Mode::Quick,
                &mut web,
                &cancel
            )
            .await,
            Err(RunError::Tool(Failure::Cancelled))
        ));
        assert!(web.transport.calls.is_empty());
    }
}

#[tokio::test]
async fn actual_utf8_text_deltas_stream_without_replaying_the_terminal_message() {
    let mut final_message = answer();
    final_message["content"][0]["text"] = json!("Synthetic 🧪 answer.");
    let (config, _auth, server) = fixture(vec![vec![
        json!({"type":"response.output_text.delta","delta":"Synthetic 🧪 "}),
        json!({"type":"response.output_text.delta","delta":"answer."}),
        final_message,
    ]])
    .await;
    let mut web = WebService::new(EvidenceTransport { calls: vec![] });
    let mut deltas = vec![];
    let result = run_with_events(
        config,
        &prompt(),
        Mode::Quick,
        RunContext {
            conversation_id: "synthetic-conversation-key",
            instructions: "Synthetic style.",
            explicit_search: false,
            web_enabled: true,
        },
        &mut web,
        &Cancellation::default(),
        |event| {
            if let RunEvent::Model(ModelEvent::TextDelta { text, content_type }) = event {
                assert_eq!(content_type, CustomOpenAiContentType::OutputText);
                deltas.push(text.to_string());
            }
            Ok(())
        },
    )
    .await
    .unwrap();
    assert_eq!(deltas, vec!["Synthetic 🧪 ", "answer."]);
    assert_eq!(result.response.text, "Synthetic 🧪 answer.");
    assert!(result.response.timings.headers_at <= result.response.timings.first_text_at.unwrap());
    assert!(result.response.timings.first_text_at.unwrap() <= result.response.timings.completed_at);
    server.await.unwrap();
}

#[tokio::test]
async fn cancellation_during_a_delta_stops_later_callbacks_and_result_settlement() {
    let (config, _auth, server) = fixture(vec![vec![
        json!({"type":"response.output_text.delta","delta":"Synthetic first"}),
        json!({"type":"response.output_text.delta","delta":"Synthetic late"}),
        answer(),
    ]])
    .await;
    let (sender, receiver) = tokio::sync::watch::channel(false);
    let cancel = Cancellation::from_receiver(receiver);
    let mut web = WebService::new(EvidenceTransport { calls: vec![] });
    let mut deltas = vec![];
    let result = run_with_events(
        config,
        &prompt(),
        Mode::Quick,
        RunContext {
            conversation_id: "synthetic-conversation-key",
            instructions: "Synthetic style.",
            explicit_search: false,
            web_enabled: true,
        },
        &mut web,
        &cancel,
        |event| {
            if let RunEvent::Model(ModelEvent::TextDelta { text, .. }) = event {
                deltas.push(text.to_string());
                sender.send(true).unwrap();
            }
            Ok(())
        },
    )
    .await;
    assert!(matches!(result, Err(RunError::Tool(Failure::Cancelled))));
    assert_eq!(deltas, vec!["Synthetic first"]);
    server.await.unwrap();
}

#[tokio::test]
async fn refusal_keeps_native_kind_and_is_not_relabelled_as_unsupported_search() {
    let refusal = json!({"type":"message","id":"synthetic-refusal","content":[{"type":"refusal","refusal":"Synthetic refusal"}]});
    let (config, _auth, server) = fixture(vec![vec![refusal]]).await;
    let mut web = WebService::new(EvidenceTransport { calls: vec![] });
    let result = run_with_events(
        config,
        &prompt(),
        Mode::Verify,
        RunContext {
            conversation_id: "synthetic-conversation-key",
            instructions: "Synthetic style.",
            explicit_search: true,
            web_enabled: true,
        },
        &mut web,
        &Cancellation::default(),
        |_| Ok(()),
    )
    .await
    .unwrap();
    assert_eq!(
        result.response.content_type,
        CustomOpenAiContentType::Refusal
    );
    assert_eq!(result.persisted_text(), "Synthetic refusal");
    assert!(web.transport.calls.is_empty());
    server.await.unwrap();
}

#[tokio::test]
async fn trusted_library_explicit_quick_search_cannot_complete_without_a_retrieval_attempt() {
    let (config, _auth, server) = fixture(vec![vec![answer()]]).await;
    let mut web = WebService::new(EvidenceTransport { calls: vec![] });
    let result = run_with_events(
        config,
        &prompt(),
        Mode::Quick,
        RunContext {
            conversation_id: "synthetic-conversation-key",
            instructions: "Synthetic style.",
            explicit_search: true,
            web_enabled: true,
        },
        &mut web,
        &Cancellation::default(),
        |_| Ok(()),
    )
    .await;
    assert!(matches!(result, Err(RunError::Tool(Failure::Unsupported))));
    assert_eq!(server.await.unwrap()[0]["tool_choice"], "required");
}

#[tokio::test]
async fn disabled_web_keeps_chat_and_verify_research_but_advertises_no_tools_or_fallback() {
    for mode in [Mode::Quick, Mode::Verify, Mode::Research] {
        let (config, _auth, server) = fixture(vec![vec![answer()]]).await;
        let mut web = WebService::new(EvidenceTransport { calls: vec![] });
        let mut deltas = String::new();
        let composed =
            voxgolem_core::response_style::compose_instructions("Synthetic custom instructions.");
        let result = run_with_events(
            config,
            &prompt(),
            mode,
            RunContext {
                conversation_id: "synthetic-conversation",
                instructions: &composed,
                explicit_search: mode != Mode::Quick,
                web_enabled: false,
            },
            &mut web,
            &Cancellation::default(),
            |event| {
                if let RunEvent::Model(ModelEvent::TextDelta { text, .. }) = event {
                    deltas.push_str(text);
                }
                Ok(())
            },
        )
        .await
        .unwrap();
        assert_eq!(result.response.text, deltas);
        assert!(result.response.text.ends_with("Synthetic final answer."));
        if mode != Mode::Quick {
            assert!(result.response.text.starts_with("Web lookup disabled:"));
        }
        assert!(web.transport.calls.is_empty());
        assert_eq!((result.searches, result.reads, result.fallbacks), (0, 0, 0));
        assert!(!result.source_read_observed);
        let requests = server.await.unwrap();
        assert_eq!(requests[0]["tools"], json!([]));
        assert_eq!(requests[0]["tool_choice"], "none");
        let instructions = requests[0]["instructions"].as_str().unwrap();
        assert!(instructions.contains("Web lookup is disabled by native permission"));
        assert!(instructions.contains(voxgolem_core::response_style::PLAIN_TEXT_POLICY));
        assert_eq!(
            instructions
                .matches(voxgolem_core::response_style::PLAIN_TEXT_POLICY)
                .count(),
            1
        );
    }
}

#[tokio::test]
async fn disabled_lookup_disclosure_preserves_refusal_kind_in_stream_and_terminal() {
    let refusal = json!({"type":"message","id":"synthetic-refusal","content":[{"type":"refusal","refusal":"Synthetic refusal"}]});
    let (config, _auth, server) = fixture(vec![vec![refusal]]).await;
    let mut web = WebService::new(EvidenceTransport { calls: vec![] });
    let mut text = String::new();
    let result = run_with_events(
        config,
        &prompt(),
        Mode::Verify,
        RunContext {
            conversation_id: "synthetic-conversation",
            instructions: "Synthetic instructions.",
            explicit_search: true,
            web_enabled: false,
        },
        &mut web,
        &Cancellation::default(),
        |event| {
            if let RunEvent::Model(ModelEvent::TextDelta {
                text: delta,
                content_type,
            }) = event
            {
                assert_eq!(content_type, CustomOpenAiContentType::Refusal);
                text.push_str(delta);
            }
            Ok(())
        },
    )
    .await
    .unwrap();
    assert_eq!(
        result.response.content_type,
        CustomOpenAiContentType::Refusal
    );
    assert_eq!(result.response.text, text);
    assert!(text.ends_with("Synthetic refusal"));
    assert!(web.transport.calls.is_empty());
    server.await.unwrap();
}

#[tokio::test]
async fn disabled_permission_rejects_provider_tool_calls_before_any_transport() {
    let (config, _auth, server) = fixture(vec![vec![search_call("synthetic-denied")]]).await;
    let mut web = WebService::new(EvidenceTransport { calls: vec![] });
    let result = run_with_events(
        config,
        &prompt(),
        Mode::Research,
        RunContext {
            conversation_id: "synthetic-conversation",
            instructions: "Synthetic instructions.",
            explicit_search: true,
            web_enabled: false,
        },
        &mut web,
        &Cancellation::default(),
        |_| Ok(()),
    )
    .await;
    assert!(matches!(result, Err(RunError::Protocol)));
    assert!(web.transport.calls.is_empty());
    assert_eq!(server.await.unwrap().len(), 1);
}

#[tokio::test]
async fn operation_timing_includes_tools_and_rounds_but_not_suppressed_text() {
    struct TimedTransport {
        origin: Instant,
        after_tools: Duration,
    }
    impl WebTransport for TimedTransport {
        async fn call(
            &mut self,
            _: Provider,
            _: &Operation,
            _: &str,
            _: Instant,
            _: &Cancellation,
        ) -> Result<Value, ProviderFailure> {
            tokio::time::sleep(Duration::from_millis(20)).await;
            self.after_tools = self.origin.elapsed();
            Ok(
                json!({"structuredContent":{"results":[{"url":"https://example.com/source","excerpts":["Synthetic source-page content"]}]}}),
            )
        }
    }
    let (config, _auth, server) = fixture(vec![
        vec![
            json!({"type":"response.output_text.delta","delta":"Synthetic suppressed claim"}),
            search_call("synthetic-search"),
        ],
        vec![call(
            "synthetic-read",
            "web_fetch",
            json!({"urls":["https://example.com/source"],"focus":""}),
        )],
        vec![answer()],
    ])
    .await;
    let mut web = WebService::new(TimedTransport {
        origin: Instant::now(),
        after_tools: Duration::ZERO,
    });
    let mut deltas = vec![];
    let result = run_with_events(
        config,
        &prompt(),
        Mode::Verify,
        RunContext {
            conversation_id: "synthetic-conversation",
            instructions: "Synthetic instructions.",
            explicit_search: true,
            web_enabled: true,
        },
        &mut web,
        &Cancellation::default(),
        |event| {
            if let RunEvent::Model(ModelEvent::TextDelta { text, .. }) = event {
                deltas.push(text.to_owned());
            }
            Ok(())
        },
    )
    .await
    .unwrap();
    assert_eq!(deltas, vec!["Synthetic final answer."]);
    // Common observer; a small origin offset is measured rather than assumed.
    assert!(result.response.timings.first_text_at.unwrap() >= Duration::from_millis(40));
    assert!(result.response.timings.completed_at >= Duration::from_millis(40));
    assert!(result.response.timings.headers_at < result.response.timings.first_text_at.unwrap());
    assert!(result.response.timings.first_text_at.unwrap() <= result.response.timings.completed_at);
    assert!(web.transport.after_tools >= Duration::from_millis(40));
    server.await.unwrap();
}

#[test]
fn shared_runner_future_can_use_the_desktop_send_executor() {
    fn assert_send(_: impl Send) {}
    let mut web = WebService::new(EvidenceTransport { calls: vec![] });
    let prompt = prompt();
    let cancel = Cancellation::default();
    assert_send(run_with_events(
        CustomOpenAiConfig::default(),
        &prompt,
        Mode::Quick,
        RunContext {
            conversation_id: "synthetic-conversation-key",
            instructions: "Synthetic style.",
            explicit_search: false,
            web_enabled: true,
        },
        &mut web,
        &cancel,
        |_| Ok(()),
    ));
}

#[tokio::test]
async fn terminal_omitted_or_conflicting_calls_never_reach_web_transport() {
    for output in [
        json!([]),
        json!([answer()]),
        json!([call("synthetic-call", "web_fetch", json!({}))]),
    ] {
        let terminal =
            json!({"type":"response.completed","response":{"status":"completed","output":output}});
        let (config, _auth, server) =
            fixture(vec![vec![search_call("synthetic-call"), terminal]]).await;
        let mut web = WebService::new(EvidenceTransport { calls: vec![] });
        let result = run(
            config,
            &prompt(),
            Mode::Quick,
            &mut web,
            &Cancellation::default(),
        )
        .await;
        assert!(matches!(
            result,
            Err(RunError::Provider(CustomOpenAiError::MalformedStream))
        ));
        assert!(web.transport.calls.is_empty());
        server.await.unwrap();
    }
}

#[tokio::test]
async fn cancelling_first_tool_failure_prevents_later_callbacks_in_the_same_round() {
    let invalid = |id| {
        call(
            id,
            "web_search",
            json!({"objective":"Synthetic missing fields"}),
        )
    };
    let (config, _auth, server) = fixture(vec![vec![
        invalid("synthetic-one"),
        invalid("synthetic-two"),
    ]])
    .await;
    let cancel = Cancellation::default();
    let mut web = WebService::new(EvidenceTransport { calls: vec![] });
    let mut failures = vec![];
    let result = run_with_events(
        config,
        &prompt(),
        Mode::Quick,
        RunContext {
            conversation_id: "synthetic-conversation-key",
            instructions: "Synthetic style.",
            explicit_search: false,
            web_enabled: true,
        },
        &mut web,
        &cancel,
        |event| {
            if let RunEvent::ToolFailed { call_id, .. } = event {
                failures.push(call_id.to_string());
                cancel.cancel();
            }
            Ok(())
        },
    )
    .await;
    assert!(matches!(result, Err(RunError::Tool(Failure::Cancelled))));
    assert_eq!(failures, vec!["synthetic-one"]);
    assert!(web.transport.calls.is_empty());
    assert_eq!(server.await.unwrap().len(), 1);
}

#[tokio::test]
async fn cancelling_completed_tool_prevents_a_following_invalid_call_callback() {
    let (config, _auth, server) = fixture(vec![vec![
        search_call("synthetic-one"),
        call("synthetic-two", "web_fetch", json!({})),
    ]])
    .await;
    let cancel = Cancellation::default();
    let mut web = WebService::new(EvidenceTransport { calls: vec![] });
    let mut events = vec![];
    let result = run_with_events(
        config,
        &prompt(),
        Mode::Quick,
        RunContext {
            conversation_id: "synthetic-conversation-key",
            instructions: "Synthetic style.",
            explicit_search: false,
            web_enabled: true,
        },
        &mut web,
        &cancel,
        |event| {
            match event {
                RunEvent::ToolCompleted { call_id, .. } => {
                    events.push(call_id.to_string());
                    cancel.cancel();
                }
                RunEvent::ToolFailed { call_id, .. } => events.push(call_id.to_string()),
                _ => {}
            }
            Ok(())
        },
    )
    .await;
    assert!(matches!(result, Err(RunError::Tool(Failure::Cancelled))));
    assert_eq!(events, vec!["synthetic-one"]);
    assert_eq!(web.transport.calls.len(), 1);
    assert_eq!(server.await.unwrap().len(), 1);
}

#[tokio::test]
async fn verification_gate_rechecks_readability_after_the_run_evidence_budget() {
    struct BudgetedTransport;
    impl WebTransport for BudgetedTransport {
        async fn call(
            &mut self,
            _: Provider,
            operation: &Operation,
            _: &str,
            _: Instant,
            _: &Cancellation,
        ) -> Result<Value, ProviderFailure> {
            let sources = if operation.fetch() {
                vec![
                    json!({"url":"https://example.com/source","title":"Synthetic page","excerpts":[format!("{}Readable synthetic content","\0".repeat(3000))]}),
                ]
            } else {
                vec![
                    json!({"url":"https://example.com/one","title":"Synthetic one","excerpts":["a".repeat(11000)]}),
                    json!({"url":"https://example.com/two","title":"Synthetic two","excerpts":["b".repeat(11000)]}),
                ]
            };
            Ok(json!({"structuredContent":{"results":sources}}))
        }
    }
    let (config, _auth, server) = fixture(vec![
        vec![search_call("synthetic-search")],
        vec![call(
            "synthetic-fetch",
            "web_fetch",
            json!({"urls":["https://example.com/source"],"focus":""}),
        )],
        vec![answer()],
    ])
    .await;
    let mut web = WebService::new(BudgetedTransport);
    let mut deltas = vec![];
    let result = run_with_events(
        config,
        &prompt(),
        Mode::Quick,
        RunContext {
            conversation_id: "synthetic-conversation-key",
            instructions: "Synthetic style.",
            explicit_search: true,
            web_enabled: true,
        },
        &mut web,
        &Cancellation::default(),
        |event| {
            if let RunEvent::Model(ModelEvent::TextDelta { text, .. }) = event {
                deltas.push(text.to_string());
            }
            Ok(())
        },
    )
    .await
    .unwrap();
    assert!(!result.source_read_observed);
    assert!(deltas.is_empty());
    assert!(result
        .response
        .text
        .starts_with("Source verification incomplete:"));
    server.await.unwrap();
}
