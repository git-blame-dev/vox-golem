#![cfg(feature = "web-search")]

use serde_json::{json, Value};
use std::{collections::VecDeque, time::Duration};
use tokio::time::Instant;
use voxgolem_core::search::*;
use voxgolem_platform::{search_control::Cancellation, web::*};

struct FixtureTransport {
    responses: VecDeque<Result<Value, ProviderFailure>>,
    calls: Vec<(Provider, Value)>,
}
impl WebTransport for FixtureTransport {
    async fn call(
        &mut self,
        provider: Provider,
        operation: &Operation,
        conversation_id: &str,
        _: Instant,
        _: &Cancellation,
    ) -> Result<Value, ProviderFailure> {
        self.calls
            .push((provider, operation.arguments(provider, conversation_id)));
        self.responses.pop_front().expect("fixture response")
    }
}
fn service(responses: Vec<Result<Value, ProviderFailure>>) -> WebService<FixtureTransport> {
    WebService::new(FixtureTransport {
        responses: responses.into(),
        calls: vec![],
    })
}
fn search() -> Operation {
    Operation::Search {
        objective: "Synthetic objective".into(),
        queries: vec!["Synthetic query one".into(), "Synthetic query two".into()],
        limit: 3,
    }
}
fn parallel() -> Value {
    json!({"structuredContent":{"results":[{"url":"https://example.com/primary", "title":"Synthetic primary source", "excerpts":["Synthetic evidence"]}], "errors":[]}})
}
fn exa() -> Value {
    json!({"content":[{"type":"text","text":"Title: Synthetic alternate source\nURL: https://example.com/alternate\nPublished: N/A\nHighlights:\nSynthetic evidence\n"}]})
}

struct DeadlineTransport {
    calls: Vec<Provider>,
    expire_on_call: usize,
    cancel_on_expiry: bool,
}
impl WebTransport for DeadlineTransport {
    async fn call(
        &mut self,
        provider: Provider,
        _: &Operation,
        _: &str,
        deadline: Instant,
        cancel: &Cancellation,
    ) -> Result<Value, ProviderFailure> {
        self.calls.push(provider);
        if self.calls.len() == self.expire_on_call {
            // Complete in this poll after the absolute deadline, without a scheduler race.
            while Instant::now() < deadline {
                std::thread::sleep(deadline.saturating_duration_since(Instant::now()));
            }
            if self.cancel_on_expiry {
                cancel.cancel();
            }
            if provider == Provider::Exa {
                return Ok(
                    json!({"content":[{"type":"text","text":"# Synthetic kept page\nURL: https://example.com/kept\n\nSynthetic retained evidence"}]}),
                );
            }
            return Ok(
                json!({"structuredContent":{"results":[{"url":"https://example.com/kept","title":"Synthetic kept page","excerpts":["Synthetic retained evidence"]}],"errors":[{"url":"https://example.com/retry","error":"Synthetic service unavailable"}]}}),
            );
        }
        Err(Failure::Service.into())
    }
}

#[tokio::test]
async fn deadline_before_partial_fallback_preserves_pages_without_claiming_fallback() {
    for cancelled in [false, true] {
        let mut service = WebService::new(DeadlineTransport {
            calls: vec![],
            expire_on_call: 1,
            cancel_on_expiry: cancelled,
        });
        let operation = Operation::Fetch {
            urls: vec![
                "https://example.com/kept".into(),
                "https://example.com/retry".into(),
            ],
            focus: String::new(),
        };
        let result = service
            .execute(
                &operation,
                "synthetic-conversation",
                "synthetic-run",
                Instant::now() + Duration::from_millis(30),
                &Cancellation::default(),
            )
            .await;
        assert_eq!(service.transport.calls, vec![Provider::Parallel]);
        if cancelled {
            assert!(matches!(result, Err(Failure::Cancelled)));
        } else {
            let result = result.expect("deadline must not discard an already decoded page");
            assert_eq!(result.sources.len(), 1);
            assert_eq!(result.sources[0].url, "https://example.com/kept");
            assert!(result.partial);
            assert!(!result.fallback_used, "alternate was never started");
            assert_eq!(result.failures.len(), 1);
            assert_eq!(result.failures[0].failure, Failure::Deadline);
        }
    }
}

#[tokio::test]
async fn alternate_deadline_replaces_failed_page_status_but_keeps_primary_evidence() {
    let mut service = service(vec![
        Ok(
            json!({"structuredContent":{"results":[{"url":"https://example.com/kept","excerpts":["Synthetic retained evidence"]}],"errors":[{"url":"https://example.com/retry","error":"Synthetic service unavailable"}]}}),
        ),
        Err(Failure::Deadline.into()),
    ]);
    let result = service
        .execute(
            &Operation::Fetch {
                urls: vec![
                    "https://example.com/kept".into(),
                    "https://example.com/retry".into(),
                ],
                focus: String::new(),
            },
            "synthetic-conversation",
            "synthetic-run",
            Instant::now() + Duration::from_secs(5),
            &Cancellation::default(),
        )
        .await
        .unwrap();
    assert_eq!(service.transport.calls.len(), 2);
    assert_eq!(result.sources.len(), 1);
    assert!(result.fallback_used);
    assert!(result.partial);
    assert_eq!(result.failures[0].failure, Failure::Deadline);
}

#[tokio::test]
async fn exa_batch_deadline_retains_success_and_stops_remaining_calls() {
    let mut service = service(vec![
        Err(Failure::Service.into()),
        Ok(
            json!({"content":[{"type":"text","text":"# Synthetic kept page\nURL: https://example.com/kept\n\nSynthetic evidence"}]}),
        ),
        Err(Failure::Deadline.into()),
        Ok(
            json!({"content":[{"type":"text","text":"# Synthetic forbidden late page\nURL: https://example.com/not-started\n\nSynthetic evidence"}]}),
        ),
    ]);
    let result = service
        .execute(
            &Operation::Fetch {
                urls: vec![
                    "https://example.com/kept".into(),
                    "https://example.com/expired".into(),
                    "https://example.com/not-started".into(),
                ],
                focus: String::new(),
            },
            "synthetic-conversation",
            "synthetic-run",
            Instant::now() + Duration::from_secs(5),
            &Cancellation::default(),
        )
        .await
        .unwrap();
    assert_eq!(
        service.transport.calls.len(),
        3,
        "no subsequent page after transport deadline"
    );
    assert_eq!(result.sources.len(), 1);
    assert!(result.partial);
    assert!(result.fallback_used);
    assert_eq!(result.failures.len(), 2);
    assert!(result
        .failures
        .iter()
        .all(|failure| failure.failure == Failure::Deadline));
}

#[tokio::test]
async fn parent_deadline_between_exa_pages_preserves_completed_page_without_late_requests() {
    let mut service = WebService::new(DeadlineTransport {
        calls: vec![],
        expire_on_call: 2,
        cancel_on_expiry: false,
    });
    let result = service
        .execute(
            &Operation::Fetch {
                urls: vec![
                    "https://example.com/kept".into(),
                    "https://example.com/not-started".into(),
                    "https://example.com/also-not-started".into(),
                ],
                focus: String::new(),
            },
            "synthetic-conversation",
            "synthetic-run",
            Instant::now() + Duration::from_millis(30),
            &Cancellation::default(),
        )
        .await
        .unwrap();
    assert_eq!(
        service.transport.calls,
        vec![Provider::Parallel, Provider::Exa]
    );
    assert_eq!(result.sources.len(), 1);
    assert_eq!(result.sources[0].url, "https://example.com/kept");
    assert!(result.partial);
    assert!(result.fallback_used);
    assert_eq!(result.failures.len(), 2);
    assert!(result
        .failures
        .iter()
        .all(|failure| failure.failure == Failure::Deadline));
}

#[tokio::test]
async fn cancellation_on_alternate_invalidates_retained_primary_page() {
    struct CancelAlternate {
        calls: usize,
    }
    impl WebTransport for CancelAlternate {
        async fn call(
            &mut self,
            _: Provider,
            _: &Operation,
            _: &str,
            _: Instant,
            cancel: &Cancellation,
        ) -> Result<Value, ProviderFailure> {
            self.calls += 1;
            if self.calls == 1 {
                Ok(
                    json!({"structuredContent":{"results":[{"url":"https://example.com/kept","excerpts":["Synthetic retained evidence"]}],"errors":[{"url":"https://example.com/retry","error":"Synthetic service unavailable"}]}}),
                )
            } else {
                cancel.cancel();
                std::future::pending().await
            }
        }
    }
    let mut service = WebService::new(CancelAlternate { calls: 0 });
    let result = service
        .execute(
            &Operation::Fetch {
                urls: vec![
                    "https://example.com/kept".into(),
                    "https://example.com/retry".into(),
                ],
                focus: String::new(),
            },
            "synthetic-conversation",
            "synthetic-run",
            Instant::now() + Duration::from_secs(5),
            &Cancellation::default(),
        )
        .await;
    assert_eq!(service.transport.calls, 2);
    assert!(
        matches!(result, Err(Failure::Cancelled)),
        "cancelled execution cannot return retained evidence"
    );
}

#[test]
fn source_url_digits_do_not_turn_page_service_errors_into_rate_limits() {
    let result = decode_result(Provider::Parallel, &Operation::Fetch { urls: vec!["https://example.com/429".into()], focus: String::new() }, &json!({"structuredContent":{"results":[],"errors":[{"url":"https://example.com/429","error":"Synthetic service unavailable"}]}}), "synthetic-run", &mut 0).unwrap();
    assert_eq!(result.failures[0].failure, Failure::Service);
}

#[tokio::test]
async fn primary_success_uses_one_provider_and_empty_result_does_not_fallback() {
    for response in [parallel(), json!({"structuredContent":{"results":[]}})] {
        let mut service = service(vec![Ok(response)]);
        let result = service
            .execute(
                &search(),
                "synthetic-conversation",
                "synthetic-run",
                Instant::now() + Duration::from_secs(20),
                &Cancellation::default(),
            )
            .await
            .unwrap();
        assert_eq!(service.transport.calls.len(), 1);
        assert!(!result.fallback_used);
        assert_eq!(service.transport.calls[0].0, Provider::Parallel);
    }
}

#[tokio::test]
async fn rate_limit_falls_back_once_and_next_call_skips_cooling_primary() {
    let mut service = service(vec![
        Err(ProviderFailure {
            kind: Failure::RateLimited,
            retry_after: Some(Duration::from_secs(90)),
        }),
        Ok(exa()),
        Ok(exa()),
    ]);
    let cancel = Cancellation::default();
    let result = service
        .execute(
            &search(),
            "synthetic-conversation",
            "synthetic-run",
            Instant::now() + Duration::from_secs(20),
            &cancel,
        )
        .await
        .unwrap();
    assert!(result.fallback_used);
    assert_eq!(result.sources[0].provider, Provider::Exa);
    service
        .execute(
            &search(),
            "synthetic-conversation",
            "synthetic-run",
            Instant::now() + Duration::from_secs(20),
            &cancel,
        )
        .await
        .unwrap();
    assert_eq!(
        service
            .transport
            .calls
            .iter()
            .map(|call| call.0)
            .collect::<Vec<_>>(),
        vec![Provider::Parallel, Provider::Exa, Provider::Exa]
    );
    assert_eq!(
        service.transport.calls[0].1["session_id"],
        "synthetic-conversation"
    );
    assert_eq!(
        service.transport.calls[1].1["query"],
        "Synthetic query one\nSynthetic query two"
    );
}

#[tokio::test]
async fn two_failures_never_retry_more_than_one_alternate() {
    let mut service = service(vec![
        Err(Failure::Service.into()),
        Err(Failure::Transport.into()),
    ]);
    assert!(matches!(
        service
            .execute(
                &search(),
                "synthetic-conversation",
                "synthetic-run",
                Instant::now() + Duration::from_secs(20),
                &Cancellation::default()
            )
            .await,
        Err(Failure::Transport)
    ));
    assert_eq!(service.transport.calls.len(), 2);
}

#[tokio::test]
async fn invalid_inputs_cancel_and_denial_do_not_fallback() {
    for failure in [
        Failure::InvalidArguments,
        Failure::DeniedUrl,
        Failure::Cancelled,
        Failure::Unsupported,
        Failure::IncompatibleSchema,
    ] {
        let mut service = service(vec![Err(failure.into())]);
        assert!(
            matches!(service.execute(&search(),"synthetic-conversation","synthetic-run",Instant::now()+Duration::from_secs(20),&Cancellation::default()).await,Err(error) if error == failure)
        );
        assert_eq!(service.transport.calls.len(), 1);
    }
    let mut service = service(vec![]);
    let cancel = Cancellation::default();
    cancel.cancel();
    assert!(matches!(
        service
            .execute(
                &search(),
                "synthetic-conversation",
                "synthetic-run",
                Instant::now() + Duration::from_secs(20),
                &cancel
            )
            .await,
        Err(Failure::Cancelled)
    ));
    assert!(service.transport.calls.is_empty());
}

#[tokio::test]
async fn malformed_primary_response_uses_the_alternate() {
    let mut service = service(vec![
        Ok(json!({"structuredContent":{"unexpected":"synthetic"}})),
        Ok(exa()),
    ]);
    let result = service
        .execute(
            &search(),
            "synthetic-conversation",
            "synthetic-run",
            Instant::now() + Duration::from_secs(20),
            &Cancellation::default(),
        )
        .await
        .unwrap();
    assert!(result.fallback_used);
}

#[test]
fn rejects_private_destinations_and_unknown_tool_arguments_before_transport() {
    for url in [
        "file:///tmp/synthetic",
        "https://synthetic:secret@example.com/",
        "http://127.0.0.1/",
        "http://10.0.0.1/",
        "http://169.254.169.254/",
        "http://[::1]/",
        "http://localhost/",
        "http://synthetic.internal/",
        "http://100.64.0.1/",
        "https://example.com:1234/",
    ] {
        assert!(
            Operation::parse("web_fetch", &json!({"urls":[url],"focus":""})).is_err(),
            "{url}"
        );
    }
    assert!(Operation::parse(
        "web_fetch",
        &json!({"urls":["https://example.com/"],"focus":""})
    )
    .is_ok());
    assert!(Operation::parse("web_search",&json!({"objective":"Synthetic","queries":["Synthetic"],"num_results":3,"endpoint":"https://synthetic.invalid"})).is_err());
    assert!(Operation::parse(
        "web_fetch",
        &json!({"urls":["https://example.com/"],"focus":"🧪".repeat(201)})
    )
    .is_err());
    assert!(Operation::parse("shell", &json!({})).is_err());
}

#[tokio::test]
async fn partial_fetch_retries_only_failed_pages_and_keeps_primary_successes() {
    for all_failed in [false, true] {
        let primary = json!({"structuredContent":{"results":if all_failed { vec![] } else { vec![json!({"url":"https://example.com/kept","title":"Synthetic kept source","excerpts":["Synthetic evidence"]})] },"errors":[{"url":"https://example.com/retry","error":"Synthetic service unavailable"}]}});
        let alternate = json!({"content":[{"type":"text","text":"# Synthetic recovered source\nURL: https://example.com/retry\n\nSynthetic recovered evidence"}]});
        let mut service = service(vec![Ok(primary), Ok(alternate)]);
        let operation = Operation::Fetch {
            urls: if all_failed {
                vec!["https://example.com/retry".into()]
            } else {
                vec![
                    "https://example.com/kept".into(),
                    "https://example.com/retry".into(),
                ]
            },
            focus: "Synthetic focus".into(),
        };
        let result = service
            .execute(
                &operation,
                "synthetic-conversation",
                "synthetic-run",
                Instant::now() + Duration::from_secs(20),
                &Cancellation::default(),
            )
            .await
            .unwrap();
        assert_eq!(service.transport.calls.len(), 2);
        assert_eq!(
            service.transport.calls[1].1["urls"],
            json!(["https://example.com/retry"])
        );
        assert_eq!(result.sources.len(), if all_failed { 1 } else { 2 });
        assert!(result.failures.is_empty());
        assert!(!result.partial);
        assert!(result.fallback_used);
    }
}

#[tokio::test]
async fn fallback_recovery_preserves_partial_for_retained_or_recovered_truncated_pages() {
    let long_body = "Synthetic bounded evidence. ".repeat(500);
    assert!(long_body.len() > 12000);
    for truncate_primary in [false, true] {
        let primary_body = if truncate_primary {
            long_body.as_str()
        } else {
            "Synthetic retained evidence"
        };
        let recovered_body = if truncate_primary {
            "Synthetic recovered evidence"
        } else {
            long_body.as_str()
        };
        let primary = json!({"structuredContent":{"results":[{"url":"https://example.com/kept","title":"Synthetic retained page","excerpts":[primary_body]}],"errors":[{"url":"https://example.com/retry","error":"Synthetic service unavailable"}]}});
        let alternate = json!({"content":[{"type":"text","text":format!("# Synthetic recovered page\nURL: https://example.com/retry\n\n{recovered_body}")} ]});
        let mut service = service(vec![Ok(primary), Ok(alternate)]);
        let result = service
            .execute(
                &Operation::Fetch {
                    urls: vec![
                        "https://example.com/kept".into(),
                        "https://example.com/retry".into(),
                    ],
                    focus: String::new(),
                },
                "synthetic-conversation",
                "synthetic-run",
                Instant::now() + Duration::from_secs(5),
                &Cancellation::default(),
            )
            .await
            .unwrap();
        assert_eq!(service.transport.calls.len(), 2);
        assert_eq!(
            service.transport.calls[1].1["urls"],
            json!(["https://example.com/retry"])
        );
        assert_eq!(result.sources.len(), 2);
        assert_eq!(result.sources[0].provider, Provider::Parallel);
        assert_eq!(result.sources[0].url, "https://example.com/kept");
        assert_eq!(result.sources[1].provider, Provider::Exa);
        assert_eq!(result.sources[1].url, "https://example.com/retry");
        let truncated = &result.sources[usize::from(!truncate_primary)];
        assert!(truncated.truncated);
        assert_eq!(truncated.text.len(), 12000);
        assert!(result.failures.is_empty());
        assert!(result.fallback_used);
        assert!(!result.evidence_unavailable);
        assert!(result.partial);
    }
}

#[tokio::test]
async fn fallback_recovery_preserves_partial_for_extra_unparsed_raw_text() {
    let raw = "Synthetic extra provider text without attributable page metadata.";
    let primary = json!({"structuredContent":{"results":[{"url":"https://example.com/kept","title":"Synthetic retained page","excerpts":["Synthetic retained evidence"]}],"errors":[{"url":"https://example.com/retry","error":"Synthetic service unavailable"}]}});
    let alternate = json!({"content":[{"type":"text","text":"# Synthetic recovered page\nURL: https://example.com/retry\n\nSynthetic recovered evidence"}, {"type":"text","text":raw}]});
    let mut service = service(vec![Ok(primary), Ok(alternate)]);
    let result = service
        .execute(
            &Operation::Fetch {
                urls: vec![
                    "https://example.com/kept".into(),
                    "https://example.com/retry".into(),
                ],
                focus: String::new(),
            },
            "synthetic-conversation",
            "synthetic-run",
            Instant::now() + Duration::from_secs(5),
            &Cancellation::default(),
        )
        .await
        .unwrap();
    assert_eq!(service.transport.calls.len(), 2);
    assert_eq!(result.sources.len(), 2);
    assert_eq!(result.sources[0].provider, Provider::Parallel);
    assert_eq!(result.sources[1].provider, Provider::Exa);
    assert!(result.failures.is_empty());
    assert!(result.fallback_used);
    assert!(!result.evidence_unavailable);
    assert_eq!(result.raw_text.as_deref(), Some(raw));
    assert!(result.partial);
}

#[tokio::test]
async fn split_exa_batch_reports_availability_from_all_retained_pages() {
    for second_readable in [false, true] {
        let page = |url: &str, body: &str| json!({"content":[{"type":"text","text":format!("# Synthetic page\nURL: {url}\n\n{body}")} ]});
        let mut service = service(vec![
            Err(Failure::Service.into()),
            Ok(page("https://example.com/one", "\0\x07")),
            Ok(page(
                "https://example.com/two",
                if second_readable {
                    "Synthetic readable evidence"
                } else {
                    "\0\x07"
                },
            )),
        ]);
        let result = service
            .execute(
                &Operation::Fetch {
                    urls: vec![
                        "https://example.com/one".into(),
                        "https://example.com/two".into(),
                    ],
                    focus: String::new(),
                },
                "synthetic-conversation",
                "synthetic-run",
                Instant::now() + Duration::from_secs(20),
                &Cancellation::default(),
            )
            .await
            .unwrap();
        assert_eq!(result.evidence_unavailable, !second_readable);
        assert_eq!(result.sources.len(), usize::from(second_readable));
        assert_eq!(result.failures.len(), if second_readable { 1 } else { 2 });
        assert!(result.partial);
        assert_eq!(service.transport.calls.len(), 3);
    }
}

#[tokio::test]
async fn exa_batch_reads_preserve_provider_content_boundaries_with_one_sdk_call_per_page() {
    let content = |url: &str| json!({"content":[{"type":"text","text":format!("# Synthetic page\nURL: {url}\n\nSynthetic evidence")} ]});
    let mut service = service(vec![
        Err(Failure::Service.into()),
        Ok(content("https://example.com/one")),
        Ok(content("https://example.com/two")),
    ]);
    let result = service
        .execute(
            &Operation::Fetch {
                urls: vec![
                    "https://example.com/one".into(),
                    "https://example.com/two".into(),
                ],
                focus: String::new(),
            },
            "synthetic-conversation",
            "synthetic-run",
            Instant::now() + Duration::from_secs(20),
            &Cancellation::default(),
        )
        .await
        .unwrap();
    assert_eq!(service.transport.calls.len(), 3);
    assert_eq!(
        service.transport.calls[1].1["urls"],
        json!(["https://example.com/one"])
    );
    assert_eq!(
        service.transport.calls[2].1["urls"],
        json!(["https://example.com/two"])
    );
    assert_eq!(result.sources.len(), 2);
    assert!(result.failures.is_empty());
    assert!(!result.partial);
}

#[tokio::test]
async fn partial_recovery_reconciles_canonical_root_urls() {
    let primary = json!({"structuredContent":{"results":[],"errors":[{"url":"https://example.com","error":"Synthetic service unavailable"}]}});
    let alternate = json!({"content":[{"type":"text","text":"# Synthetic page\nURL: https://example.com/\n\nSynthetic evidence"}]});
    let mut service = service(vec![Ok(primary), Ok(alternate)]);
    let result = service
        .execute(
            &Operation::Fetch {
                urls: vec!["https://example.com".into()],
                focus: String::new(),
            },
            "synthetic-conversation",
            "synthetic-run",
            Instant::now() + Duration::from_secs(20),
            &Cancellation::default(),
        )
        .await
        .unwrap();
    assert_eq!(result.sources.len(), 1);
    assert!(result.failures.is_empty());
    assert!(!result.partial);
}

#[tokio::test]
async fn page_result_rate_limits_apply_cooldown_before_the_next_operation() {
    let primary = json!({"structuredContent":{"results":[],"errors":[{"url":"https://example.com/retry","error":"Synthetic rate limit"}]}});
    let alternate = json!({"content":[{"type":"text","text":"# Synthetic recovered source\nURL: https://example.com/retry\n\nSynthetic evidence"}]});
    let mut either_provider = parallel();
    either_provider["content"] = exa()["content"].clone();
    let mut service = service(vec![Ok(primary), Ok(alternate), Ok(either_provider)]);
    let cancel = Cancellation::default();
    service
        .execute(
            &Operation::Fetch {
                urls: vec!["https://example.com/retry".into()],
                focus: String::new(),
            },
            "synthetic-conversation",
            "synthetic-run",
            Instant::now() + Duration::from_secs(20),
            &cancel,
        )
        .await
        .unwrap();
    service
        .execute(
            &search(),
            "synthetic-conversation",
            "synthetic-run",
            Instant::now() + Duration::from_secs(20),
            &cancel,
        )
        .await
        .unwrap();
    assert_eq!(
        service
            .transport
            .calls
            .iter()
            .map(|call| call.0)
            .collect::<Vec<_>>(),
        vec![Provider::Parallel, Provider::Exa, Provider::Exa]
    );
}

#[tokio::test]
async fn rate_limited_exa_batch_stops_remaining_sdk_calls() {
    let mut service = service(vec![
        Err(Failure::Service.into()),
        Err(Failure::RateLimited.into()),
    ]);
    let result = service
        .execute(
            &Operation::Fetch {
                urls: vec![
                    "https://example.com/one".into(),
                    "https://example.com/two".into(),
                ],
                focus: String::new(),
            },
            "synthetic-conversation",
            "synthetic-run",
            Instant::now() + Duration::from_secs(20),
            &Cancellation::default(),
        )
        .await
        .unwrap();
    assert_eq!(service.transport.calls.len(), 2);
    assert!(result.partial);
    assert_eq!(result.failures.len(), 2);
    assert!(result
        .failures
        .iter()
        .all(|failure| failure.failure == Failure::RateLimited));
}

#[tokio::test]
async fn canonical_page_error_does_not_become_a_retryable_missing_result() {
    let primary = json!({"structuredContent":{"results":[],"errors":[{"url":"https://example.com/","error":"Synthetic invalid arguments"}]}});
    let mut service = service(vec![Ok(primary)]);
    let result = service
        .execute(
            &Operation::Fetch {
                urls: vec!["https://example.com".into()],
                focus: String::new(),
            },
            "synthetic-conversation",
            "synthetic-run",
            Instant::now() + Duration::from_secs(20),
            &Cancellation::default(),
        )
        .await
        .unwrap();
    assert_eq!(service.transport.calls.len(), 1);
    assert_eq!(result.failures.len(), 1);
    assert_eq!(result.failures[0].failure, Failure::InvalidArguments);
    assert!(!result.fallback_used);
}

#[test]
fn schema_drift_rejects_new_required_fields_instead_of_guessing() {
    let schema = json!({"properties":{"query":{"type":"string"},"objective":{"type":"string"},"numResults":{"type":"integer"}},"required":["query","objective"],"additionalProperties":false});
    assert!(validate_discovery(
        &schema,
        &search().arguments(Provider::Exa, "synthetic-conversation")
    )
    .is_ok());
    let mut drifted = schema;
    drifted["required"] = json!(["query", "objective", "account_required"]);
    assert_eq!(
        validate_discovery(
            &drifted,
            &search().arguments(Provider::Exa, "synthetic-conversation")
        ),
        Err(Failure::IncompatibleSchema)
    );
}

#[tokio::test]
async fn cancellation_drops_pending_network_work_without_starting_an_alternate() {
    struct PendingTransport {
        calls: usize,
        dropped: std::sync::Arc<std::sync::atomic::AtomicBool>,
    }
    struct DropProof(std::sync::Arc<std::sync::atomic::AtomicBool>);
    impl Drop for DropProof {
        fn drop(&mut self) {
            self.0.store(true, std::sync::atomic::Ordering::SeqCst);
        }
    }
    impl WebTransport for PendingTransport {
        async fn call(
            &mut self,
            _: Provider,
            _: &Operation,
            _: &str,
            _: Instant,
            _: &Cancellation,
        ) -> Result<Value, ProviderFailure> {
            self.calls += 1;
            let _proof = DropProof(self.dropped.clone());
            std::future::pending().await
        }
    }
    let dropped = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let mut service = WebService::new(PendingTransport {
        calls: 0,
        dropped: dropped.clone(),
    });
    let cancel = Cancellation::default();
    let signal = async {
        tokio::task::yield_now().await;
        cancel.cancel();
    };
    let operation = search();
    let (result, ()) = tokio::join!(
        service.execute(
            &operation,
            "synthetic-conversation",
            "synthetic-run",
            Instant::now() + Duration::from_secs(20),
            &cancel
        ),
        signal
    );
    assert!(matches!(result, Err(Failure::Cancelled)));
    assert_eq!(service.transport.calls, 1);
    assert!(dropped.load(std::sync::atomic::Ordering::SeqCst));
}
