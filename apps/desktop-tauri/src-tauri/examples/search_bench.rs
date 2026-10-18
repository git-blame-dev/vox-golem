use serde::Deserialize;
use serde_json::{json, Value};
use std::error::Error;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant as StdInstant};
use tokio::sync::Mutex;
use tokio::time::Instant;
use voxgolem_core::router::{
    check_search_snippets_sufficiency, classify_prompt_2d, fold_user_history,
    normalize_for_routing, Route2D, SnippetSufficiency, CANONICAL_SUPPLIED_TEXT_INTENT,
};
use voxgolem_core::search::{EvidenceKind, Failure, Mode, Provider};
use voxgolem_platform::custom_openai::tools::ModelEvent;
use voxgolem_platform::custom_openai::{
    CustomOpenAiClient, CustomOpenAiConfig, CustomOpenAiContentType, CustomOpenAiMessage,
    CustomOpenAiModel, CustomOpenAiPrompt, CustomOpenAiRole,
};
use voxgolem_platform::search::{
    admit_web, prepare_round_zero_search_with_events, prewarm_web, run_with_client_events,
    RunContext, RunEvent, WebPrewarmGate,
};
use voxgolem_platform::search_control::Cancellation;
use voxgolem_platform::web::{
    decode_result, McpTransport, Operation, ProviderFailure, WebService, WebTransport,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BenchMode {
    Deterministic,
    Live,
}

#[derive(Debug)]
struct BenchArgs {
    mode: BenchMode,
    with_llm: bool,
    cases_path: PathBuf,
}

#[derive(Debug, Deserialize)]
struct RouterCase {
    exp: String,
    prompt: String,
    prev: Option<String>,
    py_norm: String,
}

#[derive(Debug)]
struct ScenarioRow {
    id: &'static str,
    codepath: &'static str,
    route: String,
    elapsed_ms: f64,
    passed: bool,
    detail: String,
}

#[derive(Default)]
struct DeterministicTransport {
    mode: DeterministicScenarioMode,
    exa_calls: usize,
    parallel_calls: usize,
    fetch_calls: usize,
}

#[derive(Clone, Copy, Default)]
enum DeterministicScenarioMode {
    #[default]
    StandardExa,
    ExaUntitledWeather,
    ExaMultiRecordInternalHr,
    ExaRichSnippets,
    ExaThinSnippetsAndPublishedFetch,
    ExaSoftRateLimitToParallel,
    PrewarmDeadline,
}

impl WebTransport for DeterministicTransport {
    async fn prewarm(
        &mut self,
        _provider: Provider,
        _deadline: Instant,
        _cancel: &Cancellation,
    ) -> Result<(), ProviderFailure> {
        if matches!(self.mode, DeterministicScenarioMode::PrewarmDeadline) {
            return Err(ProviderFailure::from(Failure::Deadline));
        }
        Ok(())
    }

    async fn call(
        &mut self,
        provider: Provider,
        operation: &Operation,
        _conversation_id: &str,
        _deadline: Instant,
        _cancel: &Cancellation,
    ) -> Result<Value, ProviderFailure> {
        if operation.fetch() {
            self.fetch_calls += 1;
        } else if provider == Provider::Exa {
            self.exa_calls += 1;
        } else {
            self.parallel_calls += 1;
        }

        match (self.mode, provider, operation) {
            (
                DeterministicScenarioMode::ExaUntitledWeather,
                Provider::Exa,
                Operation::Search { .. },
            ) => Ok(json!({
                "content": [{
                    "type": "text",
                    "text": "Title: N/A\nURL: https://tgftp.nws.noaa.gov/weather/current/KJFK.html\nPublished: N/A\nAuthor: N/A\nHighlights:\nNew York John F. Kennedy International Airport current temperature is 22 C (71.6 F) with clear skies."
                }]
            })),
            (
                DeterministicScenarioMode::ExaMultiRecordInternalHr,
                Provider::Exa,
                Operation::Search { .. },
            ) => Ok(json!({
                "content": [{
                    "type": "text",
                    "text": concat!(
                        "Title: Google Pixel 8 - Full phone specifications\n",
                        "URL: https://www.gsmarena.com/google_pixel_8-12546.php\n",
                        "Published: 2023-10-04\n",
                        "Author: GSMArena\n",
                        "Highlights:\n",
                        "Google Pixel 8 Android smartphone. Announced Oct 2023. Features 6.2\" Actua OLED display, Google Tensor G3 chipset, 4575 mAh battery, 128/256 GB storage, 8 GB RAM.\n\n---\n\nDimensions: 150.5 x 70.8 x 8.9 mm, Weight: 187 g, IP68 dust/water resistant.\n\n---\n\n",
                        "Title: Pixel 8 Tech Specs - Google Store\n",
                        "URL: https://store.google.com/product/pixel_8_specs\n",
                        "Published: 2023-10-04\n",
                        "Author: Google\n",
                        "Highlights:\n",
                        "60-120 Hz Smooth Display, 50 MP Octa PD wide camera, 12 MP ultrawide camera, 7 years of OS and security updates.\n\n---\n\n",
                        "Title: Google Pixel 8 Review\n",
                        "URL: https://www.theverge.com/pixel-8-review\n",
                        "Published: 2023-10-11\n",
                        "Author: Allison Johnson\n",
                        "Highlights:\n",
                        "The Pixel 8 packs Tensor G3 and a 2,000-nit Actua screen into a more compact frame."
                    )
                }]
            })),
            (
                DeterministicScenarioMode::ExaRichSnippets,
                Provider::Exa,
                Operation::Search { .. },
            ) => {
                let body_a = "Tokio uses a work-stealing multi-threaded runtime with lifo slot optimization, cooperative task budgeting, and integrated mio reactor timers. ".repeat(7);
                let body_b = "async-std provides a std-like async API built on smol and async-executor with a global work-stealing queue and async-io reactor polling. ".repeat(7);
                Ok(json!({
                    "content": [{
                        "type": "text",
                        "text": format!(
                            "Title: Tokio Scheduler Architecture\nURL: https://tokio.rs/blog/scheduler\nPublished: 2024-01-10\nAuthor: Alice Ryhl\nHighlights:\n{body_a}\n\n---\n\nTitle: async-std Runtime Internals\nURL: https://async.rs/blog/runtime\nPublished: 2024-02-15\nAuthor: Yoshua Wuyts\nHighlights:\n{body_b}"
                        )
                    }]
                }))
            }
            (
                DeterministicScenarioMode::ExaThinSnippetsAndPublishedFetch,
                Provider::Exa,
                Operation::Search { .. },
            ) => {
                let filler = "Overview of historical dividend announcements and corporate governance filings without the current numeric payout table. ".repeat(8);
                Ok(json!({
                    "content": [{
                        "type": "text",
                        "text": format!(
                            "Title: Apple Dividend Summary Stub\nURL: https://example.com/apple-dividend\nPublished: 2026-02-01\nAuthor: IR Team\nHighlights:\nClick to view full dividend table.\n\n---\n\nTitle: Corporate Filings Archive\nURL: https://example.com/archive-1\nPublished: 2026-01-15\nAuthor: Archive\nHighlights:\n{filler}\n\n---\n\nTitle: Market Commentary\nURL: https://example.com/archive-2\nPublished: 2026-01-12\nAuthor: Archive\nHighlights:\n{filler}\n\n---\n\nTitle: Sector Notes\nURL: https://example.com/archive-3\nPublished: 2026-01-10\nAuthor: Archive\nHighlights:\n{filler}\n\n---\n\nTitle: Governance Notes\nURL: https://example.com/archive-4\nPublished: 2026-01-08\nAuthor: Archive\nHighlights:\n{filler}"
                        )
                    }]
                }))
            }
            (
                DeterministicScenarioMode::ExaThinSnippetsAndPublishedFetch,
                Provider::Exa,
                Operation::Fetch { .. },
            ) => Ok(json!({
                "content": [{
                    "type": "text",
                    "text": "# Apple 2026 Quarterly Dividend Official Release\nURL: https://example.com/apple-dividend\nPublished: 2026-02-18\nAuthor: Synthetic Investor Relations\n\nSynthetic fixture: Apple declared a quarterly cash dividend of USD 0.74 per share payable on 2026-03-27."
                }]
            })),
            (
                DeterministicScenarioMode::ExaSoftRateLimitToParallel,
                Provider::Exa,
                Operation::Search { .. },
            ) => Ok(json!({
                "content": [{
                    "type": "text",
                    "text": "You've hit Exa's free MCP rate limit. Please configure an Exa API key to continue."
                }]
            })),
            (
                DeterministicScenarioMode::ExaSoftRateLimitToParallel,
                Provider::Parallel,
                Operation::Search { .. },
            ) => Ok(json!({
                "structuredContent": {
                    "results": [{
                        "url": "https://www.federalreserve.gov/monetarypolicy/openmarket.htm",
                        "title": "Federal Funds Target Range | Federal Reserve",
                        "excerpts": ["The Federal Reserve example federal funds target range is 3.50 to 3.75 percent (synthetic fixture)."]
                    }]
                }
            })),
            (_, Provider::Exa, Operation::Search { .. }) => Ok(json!({
                "content": [{
                    "type": "text",
                    "text": "Title: Urza's Saga — Magic: The Gathering Comprehensive Rules\nURL: https://scryfall.com/card/mh2/259/urzas-saga\nPublished: 2021-06-18\nAuthor: Wizards of the Coast\nHighlights:\nUrza's Saga is an Enchantment Land — Urza's Saga. Chapter II grants the land '\"2, Tap: Create a 0/0 colorless Construct artifact creature token with This creature gets +1/+1 for each artifact you control.\"' The Construct token's +1/+1 characteristic-defining ability is printed on the token itself and remains even if Urza's Saga is later destroyed."
                }]
            })),
            _ => Ok(json!({
                "content": [{
                    "type": "text",
                    "text": "# Fetched Page\nURL: https://scryfall.com/card/mh2/259/urzas-saga\nPublished: 2021-06-18\nAuthor: Wizards of the Coast\n\nFull rulings for Urza's Saga chapter abilities and Construct tokens."
                }]
            })),
        }
    }
}

fn parse_args() -> Result<BenchArgs, Box<dyn Error>> {
    let mut mode = BenchMode::Deterministic;
    let mut with_llm = false;
    let mut cases_path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../crates/core/tests/fixtures/router_cases_132.json");

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--mode" => {
                let value = args.next().ok_or("missing value for --mode")?;
                mode = match value.as_str() {
                    "deterministic" => BenchMode::Deterministic,
                    "live" => BenchMode::Live,
                    other => {
                        return Err(format!(
                            "unsupported --mode '{other}' (expected deterministic or live)"
                        )
                        .into())
                    }
                };
            }
            "--with-llm" => {
                with_llm = true;
            }
            "--cases" => {
                let value = args.next().ok_or("missing value for --cases")?;
                cases_path = PathBuf::from(value);
            }
            "--help" | "-h" => {
                println!(
                    "Usage: cargo run --locked -p vox-golem --example search_bench -- [--mode deterministic|live] [--with-llm] [--cases PATH]"
                );
                std::process::exit(0);
            }
            other => {
                return Err(format!("unrecognized argument '{other}'").into());
            }
        }
    }

    Ok(BenchArgs {
        mode,
        with_llm,
        cases_path,
    })
}

fn verify_router_132_cases(cases_path: &Path) -> Result<(usize, f64), Box<dyn Error>> {
    let raw = std::fs::read_to_string(cases_path)?;
    let cases: Vec<RouterCase> = serde_json::from_str(&raw)?;
    if cases.len() != 132 {
        return Err(format!("expected 132 router cases, found {}", cases.len()).into());
    }

    let started = StdInstant::now();
    for (idx, c) in cases.iter().enumerate() {
        let rust_norm = normalize_for_routing(&c.prompt, c.prev.as_deref());
        if rust_norm != c.py_norm {
            return Err(format!(
                "normalizer mismatch at case #{idx}: got {rust_norm:?}, expected {:?}",
                c.py_norm
            )
            .into());
        }
        let decision = classify_prompt_2d(&c.prompt, c.prev.as_deref(), None);
        if !Route2D::ALL.contains(&decision.primary) {
            return Err(format!("invalid primary route at case #{idx} (exp={})", c.exp).into());
        }
    }
    let avg_us = (started.elapsed().as_secs_f64() * 1_000_000.0) / (cases.len() as f64);
    Ok((cases.len(), avg_us))
}

async fn run_deterministic_suite(cases_path: &Path) -> Result<Vec<ScenarioRow>, Box<dyn Error>> {
    let (case_count, avg_us) = verify_router_132_cases(cases_path)?;
    let mut rows = Vec::with_capacity(14);
    rows.push(ScenarioRow {
        id: "00_router_132_suite",
        codepath: "core::router",
        route: "F/S/C/R".to_string(),
        elapsed_ms: (avg_us * (case_count as f64)) / 1000.0,
        passed: case_count == 132,
        detail: format!("{case_count}/132 normalizer parity verified ({avg_us:.2} us/case)"),
    });

    // 1. DirectFast skips search in Quick mode
    {
        let t0 = StdInstant::now();
        let prompt = "What is 14 * 15?";
        let decision = classify_prompt_2d(prompt, None, None);
        let mut service = WebService::new(DeterministicTransport::default());
        let cancel = Cancellation::default();
        let out = prepare_round_zero_search_with_events(
            &mut service,
            "conv-f",
            "req-f",
            prompt,
            None,
            Mode::Quick,
            true,
            true,
            Instant::now() + Duration::from_secs(5),
            &cancel,
            |_| Ok(()),
        )
        .await
        .expect("DirectFast succeeds");
        let passed = decision.primary == Route2D::DirectFast
            && !decision.needs_search()
            && out.sources.is_empty()
            && out.provider_used.is_none();
        rows.push(ScenarioRow {
            id: "01_direct_fast_skip",
            codepath: "core + round_zero",
            route: decision.primary.code().to_string(),
            elapsed_ms: t0.elapsed().as_secs_f64() * 1000.0,
            passed,
            detail: format!("needs_search={} sources=0", decision.needs_search()),
        });
    }

    // 2. DirectSmart skips Round-0 search, reports "C" (not "F"), and preserves background prewarm
    {
        let t0 = StdInstant::now();
        let prompt = "Why does a Rust Vec reallocation invalidate raw pointers?";
        let decision = classify_prompt_2d(prompt, None, None);
        let gate = WebPrewarmGate::default();
        let mut service = WebService::new(DeterministicTransport::default());
        let cancel = Cancellation::default();
        let out = prepare_round_zero_search_with_events(
            &mut service,
            "conv-c",
            "req-c",
            prompt,
            None,
            Mode::Quick,
            true,
            true,
            Instant::now() + Duration::from_secs(5),
            &cancel,
            |_| Ok(()),
        )
        .await
        .expect("DirectSmart succeeds");
        let passed = decision.primary == Route2D::DirectSmart
            && decision.primary.code() == "C"
            && out.routing.primary == Route2D::DirectSmart
            && out.sources.is_empty()
            && !gate.is_prewarming();
        rows.push(ScenarioRow {
            id: "02_direct_smart_route_c",
            codepath: "core + round_zero",
            route: out.routing.primary.code().to_string(),
            elapsed_ms: t0.elapsed().as_secs_f64() * 1000.0,
            passed,
            detail: format!(
                "route={} needs_search={}",
                out.routing.primary.code(),
                out.routing.needs_search()
            ),
        });
    }

    // 3. Supplied-text payload guard
    {
        let t0 = StdInstant::now();
        let prompt = "Fix the typos in this sentence: What is the weather in Tokyo today in 2026?";
        let decision = classify_prompt_2d(prompt, None, None);
        let passed = decision.normalized_input == CANONICAL_SUPPLIED_TEXT_INTENT
            && decision.primary == Route2D::DirectFast
            && !decision.needs_search();
        rows.push(ScenarioRow {
            id: "03_supplied_text_guard",
            codepath: "core::router",
            route: decision.primary.code().to_string(),
            elapsed_ms: t0.elapsed().as_secs_f64() * 1000.0,
            passed,
            detail: "masked temporal/weather tokens inside colon-supplied text".to_string(),
        });
    }

    // 4. SearchFast with Exa "Title: N/A" fallback to URL
    {
        let t0 = StdInstant::now();
        let mut service = WebService::new(DeterministicTransport {
            mode: DeterministicScenarioMode::ExaUntitledWeather,
            ..Default::default()
        });
        let cancel = Cancellation::default();
        let out = prepare_round_zero_search_with_events(
            &mut service,
            "conv-s",
            "req-s",
            "What is the current weather temperature in New York City today?",
            None,
            Mode::Quick,
            true,
            true,
            Instant::now() + Duration::from_secs(5),
            &cancel,
            |_| Ok(()),
        )
        .await
        .expect("SearchFast succeeds");
        let first_title = out.sources.first().map(|s| s.title.as_str()).unwrap_or("");
        let passed = out.routing.primary == Route2D::SearchFast
            && out.sources.len() == 1
            && first_title == "https://tgftp.nws.noaa.gov/weather/current/KJFK.html"
            && !out.source_footer.contains("[N/A]");
        rows.push(ScenarioRow {
            id: "04_search_fast_title_na",
            codepath: "platform::round_zero",
            route: out.routing.primary.code().to_string(),
            elapsed_ms: t0.elapsed().as_secs_f64() * 1000.0,
            passed,
            detail: format!("sources={} title={first_title}", out.sources.len()),
        });
    }

    // 5. Exa v3.5.0 multi-result block with internal Markdown `---` divider inside Highlights
    {
        let t0 = StdInstant::now();
        let mut service = WebService::new(DeterministicTransport {
            mode: DeterministicScenarioMode::ExaMultiRecordInternalHr,
            ..Default::default()
        });
        let cancel = Cancellation::default();
        let out = prepare_round_zero_search_with_events(
            &mut service,
            "conv-hr",
            "req-hr",
            "What are the specs of the Pixel 8?",
            None,
            Mode::Quick,
            true,
            true,
            Instant::now() + Duration::from_secs(5),
            &cancel,
            |_| Ok(()),
        )
        .await
        .expect("multi-record parse succeeds");
        let passed = out.sources.len() == 3 && !out.partial && out.evidence_context.is_some();
        rows.push(ScenarioRow {
            id: "05_exa_internal_hr_split",
            codepath: "web::evidence",
            route: out.routing.primary.code().to_string(),
            elapsed_ms: t0.elapsed().as_secs_f64() * 1000.0,
            passed,
            detail: format!("sources={} partial={}", out.sources.len(), out.partial),
        });
    }

    // 6. 2-turn elliptical follow-up
    {
        let t0 = StdInstant::now();
        let prev = "What are the specs of the Pixel 8?";
        let cur = "What about the battery capacity and charging speed?";
        let decision = classify_prompt_2d(cur, Some(prev), None);
        let passed = decision.needs_search() && decision.normalized_input.contains("Pixel 8");
        rows.push(ScenarioRow {
            id: "06_two_turn_followup",
            codepath: "core::router",
            route: decision.primary.code().to_string(),
            elapsed_ms: t0.elapsed().as_secs_f64() * 1000.0,
            passed,
            detail: format!("norm={:?}", decision.normalized_input),
        });
    }

    // 7. 3-turn deep follow-up preserving Turn 1 root topic via fold_user_history
    {
        let t0 = StdInstant::now();
        let t1 = "In Magic: The Gathering, how does Urza's Saga work?";
        let t2 = "What about its second ability?";
        let t3 = "Does the token keep its ability if the land is destroyed?";
        let folded = fold_user_history([t1, t2]).unwrap_or_default();
        let decision = classify_prompt_2d(t3, Some(&folded), None);
        let mut service = WebService::new(DeterministicTransport::default());
        let cancel = Cancellation::default();
        let out = prepare_round_zero_search_with_events(
            &mut service,
            "conv-3turn",
            "req-3turn",
            t3,
            Some(&folded),
            Mode::Quick,
            true,
            true,
            Instant::now() + Duration::from_secs(5),
            &cancel,
            |_| Ok(()),
        )
        .await
        .expect("3-turn search succeeds");
        let passed = folded.contains("Urza's Saga")
            && decision.needs_search()
            && decision.normalized_input.contains("Urza's Saga")
            && !out.sources.is_empty();
        rows.push(ScenarioRow {
            id: "07_three_turn_root_fold",
            codepath: "core::fold_user_history",
            route: decision.primary.code().to_string(),
            elapsed_ms: t0.elapsed().as_secs_f64() * 1000.0,
            passed,
            detail: format!("folded={folded:?}"),
        });
    }

    // 8. ResearchSmart skips redundant web_fetch in Mode::Quick when snippets >= 1,600 bytes
    {
        let t0 = StdInstant::now();
        let mut service = WebService::new(DeterministicTransport {
            mode: DeterministicScenarioMode::ExaRichSnippets,
            ..Default::default()
        });
        let cancel = Cancellation::default();
        let mut saw_fetch_event = false;
        let out = prepare_round_zero_search_with_events(
            &mut service,
            "conv-rich",
            "req-rich",
            "Compare and contrast Tokio and async-std with a deep dive into their schedulers",
            None,
            Mode::Quick,
            true,
            true,
            Instant::now() + Duration::from_secs(5),
            &cancel,
            |ev| {
                if matches!(
                    ev,
                    RunEvent::ToolStarted {
                        name: "web_fetch",
                        ..
                    }
                ) {
                    saw_fetch_event = true;
                }
                Ok(())
            },
        )
        .await
        .expect("rich ResearchSmart succeeds");
        let passed = out.routing.primary == Route2D::ResearchSmart
            && !out.needs_escalation
            && !saw_fetch_event
            && out
                .sources
                .iter()
                .all(|s| s.kind == EvidenceKind::SearchExcerpt);
        rows.push(ScenarioRow {
            id: "08_research_rich_skip_fetch",
            codepath: "platform::round_zero",
            route: out.routing.primary.code().to_string(),
            elapsed_ms: t0.elapsed().as_secs_f64() * 1000.0,
            passed,
            detail: format!("saw_fetch={saw_fetch_event} sources={}", out.sources.len()),
        });
    }

    // 9. Escalation runs web_fetch, parses Published:/Author: header, and orders PageRead first
    {
        let t0 = StdInstant::now();
        let mut service = WebService::new(DeterministicTransport {
            mode: DeterministicScenarioMode::ExaThinSnippetsAndPublishedFetch,
            ..Default::default()
        });
        let cancel = Cancellation::default();
        let out = prepare_round_zero_search_with_events(
            &mut service,
            "conv-fetch",
            "req-fetch",
            "How much is the 2026 Apple quarterly dividend per share?",
            None,
            Mode::Quick,
            true,
            true,
            Instant::now() + Duration::from_secs(5),
            &cancel,
            |_| Ok(()),
        )
        .await
        .expect("escalated fetch succeeds");
        let ctx = out.evidence_context.unwrap_or_default();
        let page_read_first = ctx.starts_with("[1] Apple 2026 Quarterly Dividend Official Release");
        let passed = !out.needs_escalation
            && out.sources.iter().any(|s| s.kind == EvidenceKind::PageRead)
            && page_read_first
            && ctx.contains("USD 0.74 per share");
        rows.push(ScenarioRow {
            id: "09_fetch_published_pageread_first",
            codepath: "platform + evidence",
            route: out.routing.primary.code().to_string(),
            elapsed_ms: t0.elapsed().as_secs_f64() * 1000.0,
            passed,
            detail: format!(
                "post_fetch_escalate={} page_read_first={page_read_first}",
                out.needs_escalation
            ),
        });
    }

    // 10. Exa HTTP-200 soft rate limit -> no raw_text pollution + automatic Parallel fallback
    {
        let t0 = StdInstant::now();
        let op = Operation::Search {
            objective: "Federal Reserve federal funds target range".to_string(),
            queries: vec!["Federal Reserve federal funds target range".to_string()],
            limit: 3,
        };
        let mut next_id = 1usize;
        let decoded = decode_result(
            Provider::Exa,
            &op,
            &json!({
                "content": [{
                    "type": "text",
                    "text": "You've hit Exa's free MCP rate limit."
                }]
            }),
            "call-rl",
            &mut next_id,
        )
        .expect("decode soft rate limit");
        let mut service = WebService::new(DeterministicTransport {
            mode: DeterministicScenarioMode::ExaSoftRateLimitToParallel,
            ..Default::default()
        });
        let cancel = Cancellation::default();
        let out = prepare_round_zero_search_with_events(
            &mut service,
            "conv-rl",
            "req-rl",
            "What is the current Federal Reserve federal funds target range in the United States today?",
            None,
            Mode::Quick,
            true,
            true,
            Instant::now() + Duration::from_secs(5),
            &cancel,
            |_| Ok(()),
        )
        .await
        .expect("fallback to Parallel succeeds");
        let passed = decoded.raw_text.is_none()
            && decoded
                .failures
                .iter()
                .any(|f| f.failure == Failure::RateLimited)
            && out.fallback_used
            && out.provider_used == Some("parallel")
            && out.sources.len() == 1;
        rows.push(ScenarioRow {
            id: "10_exa_soft_ratelimit_fallback",
            codepath: "web::execute",
            route: out.routing.primary.code().to_string(),
            elapsed_ms: t0.elapsed().as_secs_f64() * 1000.0,
            passed,
            detail: format!(
                "raw_text_none={} fallback={} provider={:?}",
                decoded.raw_text.is_none(),
                out.fallback_used,
                out.provider_used
            ),
        });
    }

    // 11. Prewarm Failure::Deadline does not place providers into cooldown
    {
        let t0 = StdInstant::now();
        let mut service = WebService::new(DeterministicTransport {
            mode: DeterministicScenarioMode::PrewarmDeadline,
            ..Default::default()
        });
        let cancel = Cancellation::default();
        service
            .prewarm_all(Instant::now() + Duration::from_secs(1), &cancel)
            .await;
        let out = prepare_round_zero_search_with_events(
            &mut service,
            "conv-pw",
            "req-pw",
            "What is the current weather temperature in New York City today?",
            None,
            Mode::Quick,
            true,
            true,
            Instant::now() + Duration::from_secs(5),
            &cancel,
            |_| Ok(()),
        )
        .await
        .expect("Exa remains primary after prewarm deadline");
        let passed = out.provider_used == Some("exa") && !out.fallback_used;
        rows.push(ScenarioRow {
            id: "11_prewarm_deadline_no_cooldown",
            codepath: "web::prewarm_all",
            route: out.routing.primary.code().to_string(),
            elapsed_ms: t0.elapsed().as_secs_f64() * 1000.0,
            passed,
            detail: format!(
                "provider={:?} fallback={}",
                out.provider_used, out.fallback_used
            ),
        });
    }

    // 12. Snippet Sufficiency Guard (<0.1ms numeric / stub escalation check)
    {
        let t0 = StdInstant::now();
        let mut service = WebService::new(DeterministicTransport {
            mode: DeterministicScenarioMode::ExaMultiRecordInternalHr,
            ..Default::default()
        });
        let cancel = Cancellation::default();
        let out = prepare_round_zero_search_with_events(
            &mut service,
            "conv-suff",
            "req-suff",
            "What are the specs of the Pixel 8?",
            None,
            Mode::Quick,
            true,
            true,
            Instant::now() + Duration::from_secs(5),
            &cancel,
            |_| Ok(()),
        )
        .await
        .expect("search succeeds");
        let suff =
            check_search_snippets_sufficiency("What are the specs of the Pixel 8?", &out.sources);
        let passed = suff == SnippetSufficiency::Sufficient;
        rows.push(ScenarioRow {
            id: "12_snippet_sufficiency_guard",
            codepath: "core::router",
            route: out.routing.primary.code().to_string(),
            elapsed_ms: t0.elapsed().as_secs_f64() * 1000.0,
            passed,
            detail: format!("sufficiency={suff:?}"),
        });
    }

    Ok(rows)
}

async fn run_live_suite(with_llm: bool) -> Result<Vec<ScenarioRow>, Box<dyn Error>> {
    let mut rows = Vec::new();
    let gate = WebPrewarmGate::default();
    let web_mutex = Mutex::new(WebService::new(McpTransport::default()));

    // 1. Live concurrent MCP prewarm (Exa + Parallel)
    {
        let t0 = StdInstant::now();
        prewarm_web(&web_mutex, &gate).await;
        let ms = t0.elapsed().as_secs_f64() * 1000.0;
        rows.push(ScenarioRow {
            id: "live_01_concurrent_mcp_prewarm",
            codepath: "search::prewarm_web",
            route: "-".to_string(),
            elapsed_ms: ms,
            passed: ms < 8000.0,
            detail: "concurrent tokio::join!(Exa, Parallel) MCP handshake".to_string(),
        });
    }

    // 2. Live SearchFast via warm MCP session
    {
        let t0 = StdInstant::now();
        let cancel = Cancellation::default();
        let mut service = admit_web(
            &web_mutex,
            &gate,
            &cancel,
            Instant::now() + Duration::from_secs(5),
        )
        .await
        .map_err(|_| "admit_web timed out")?;
        let out = prepare_round_zero_search_with_events(
            &mut service,
            "live-s",
            "req-live-s",
            "What is the current weather temperature in New York City today?",
            None,
            Mode::Quick,
            true,
            true,
            Instant::now() + Duration::from_secs(12),
            &cancel,
            |_| Ok(()),
        )
        .await
        .map_err(|_| "live SearchFast failed")?;
        let ms = t0.elapsed().as_secs_f64() * 1000.0;
        rows.push(ScenarioRow {
            id: "live_02_warm_search_fast",
            codepath: "platform::round_zero",
            route: out.routing.primary.code().to_string(),
            elapsed_ms: ms,
            passed: !out.sources.is_empty(),
            detail: format!(
                "provider={:?} fallback={} sources={}",
                out.provider_used,
                out.fallback_used,
                out.sources.len()
            ),
        });
    }

    // 3. Live 3-turn MTG follow-up via Round-0 search
    {
        let t0 = StdInstant::now();
        let t1 = "In Magic: The Gathering, how does Urza's Saga work?";
        let t2 = "What about its second ability?";
        let t3 = "Does the token keep its ability if the land is destroyed?";
        let folded = fold_user_history([t1, t2]);
        let cancel = Cancellation::default();
        let mut service = admit_web(
            &web_mutex,
            &gate,
            &cancel,
            Instant::now() + Duration::from_secs(5),
        )
        .await
        .map_err(|_| "admit_web timed out")?;
        let out = prepare_round_zero_search_with_events(
            &mut service,
            "live-mtg",
            "req-live-mtg",
            t3,
            folded.as_deref(),
            Mode::Quick,
            true,
            true,
            Instant::now() + Duration::from_secs(12),
            &cancel,
            |_| Ok(()),
        )
        .await
        .map_err(|_| "live 3-turn MTG search failed")?;
        let ms = t0.elapsed().as_secs_f64() * 1000.0;
        rows.push(ScenarioRow {
            id: "live_03_three_turn_mtg",
            codepath: "platform::round_zero",
            route: out.routing.primary.code().to_string(),
            elapsed_ms: ms,
            passed: !out.sources.is_empty() && out.evidence_context.is_some(),
            detail: format!(
                "sources={} footer_len={}",
                out.sources.len(),
                out.source_footer.len()
            ),
        });
    }

    // 4. Optional live CustomOpenAi LunaNone (only when --with-llm is explicitly passed)
    if with_llm {
        let auth_path = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/synthetic/example-home"))
            .join(".local/share/opencode/auth.json");
        if auth_path.exists() {
            let t0 = StdInstant::now();
            let config = CustomOpenAiConfig {
                auth_path,
                model: CustomOpenAiModel::LunaNone,
                ..CustomOpenAiConfig::default()
            };
            let client = CustomOpenAiClient::new(config)?;
            let cancel = Cancellation::default();
            let mut service = admit_web(
                &web_mutex,
                &gate,
                &cancel,
                Instant::now() + Duration::from_secs(5),
            )
            .await
            .map_err(|_| "admit_web timed out")?;
            let prompt = CustomOpenAiPrompt {
                session_id: "bench-luna-none".to_string(),
                prompt: "What is the current weather temperature in New York City today? Reply in one short sentence.".to_string(),
                history: vec![CustomOpenAiMessage {
                    role: CustomOpenAiRole::User,
                    content_type: CustomOpenAiContentType::OutputText,
                    text: "Hello!".to_string(),
                }],
            };
            let mut first_token_ms = None;
            let res = run_with_client_events(
                &client,
                &prompt,
                Mode::Quick,
                RunContext {
                    conversation_id: "bench-luna-none",
                    instructions: voxgolem_core::response_style::PLAIN_TEXT_POLICY,
                    explicit_search: false,
                    web_enabled: true,
                },
                &mut service,
                &cancel,
                |ev| {
                    if matches!(ev, RunEvent::Model(ModelEvent::TextDelta { .. }))
                        && first_token_ms.is_none()
                    {
                        first_token_ms = Some(t0.elapsed().as_secs_f64() * 1000.0);
                    }
                    Ok(())
                },
            )
            .await;
            let ms = t0.elapsed().as_secs_f64() * 1000.0;
            match res {
                Ok(done) => rows.push(ScenarioRow {
                    id: "live_04_luna_none_e2e",
                    codepath: "CustomOpenAi::LunaNone",
                    route: "S".to_string(),
                    elapsed_ms: ms,
                    passed: !done.response.text.is_empty() && !done.sources.is_empty(),
                    detail: format!(
                        "ttft_ms={:.0} sources={} searches={}",
                        first_token_ms.unwrap_or(ms),
                        done.sources.len(),
                        done.searches
                    ),
                }),
                Err(err) => rows.push(ScenarioRow {
                    id: "live_04_luna_none_e2e",
                    codepath: "CustomOpenAi::LunaNone",
                    route: "S".to_string(),
                    elapsed_ms: ms,
                    passed: false,
                    detail: format!("error={err:?}"),
                }),
            }
        }
    }

    Ok(rows)
}

fn print_table(title: &str, rows: &[ScenarioRow]) {
    println!("### {title}");
    println!("| Scenario | Codepath | Route | Latency (ms) | Status | Detail |");
    println!("| :--- | :--- | :---: | ---: | :---: | :--- |");
    for row in rows {
        let status = if row.passed { "PASS" } else { "FAIL" };
        println!(
            "| `{}` | `{}` | `{}` | {:.2} | **{}** | {} |",
            row.id, row.codepath, row.route, row.elapsed_ms, status, row.detail
        );
    }
}

#[tokio::main]
async fn main() {
    let args = match parse_args() {
        Ok(args) => args,
        Err(err) => {
            eprintln!("search_bench: {err}");
            std::process::exit(1);
        }
    };

    let det_rows = match run_deterministic_suite(&args.cases_path).await {
        Ok(rows) => rows,
        Err(err) => {
            eprintln!("search_bench deterministic suite error: {err}");
            std::process::exit(1);
        }
    };
    print_table(
        "Deterministic Cross-Codepath Search & Router Suite",
        &det_rows,
    );

    let mut all_passed = det_rows.iter().all(|r| r.passed);

    if args.mode == BenchMode::Live {
        println!();
        let live_rows = match run_live_suite(args.with_llm).await {
            Ok(rows) => rows,
            Err(err) => {
                eprintln!("search_bench live suite error: {err}");
                std::process::exit(1);
            }
        };
        print_table("Live MCP & Provider Search Suite", &live_rows);
        all_passed &= live_rows.iter().all(|r| r.passed);
    }

    if !all_passed {
        eprintln!("search_bench: one or more scenarios failed");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn deterministic_cross_codepath_suite_passes() {
        let cases_path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../crates/core/tests/fixtures/router_cases_132.json");
        let rows = run_deterministic_suite(&cases_path)
            .await
            .expect("deterministic suite runs");
        for row in &rows {
            assert!(
                row.passed,
                "scenario {} ({}) failed: {}",
                row.id, row.codepath, row.detail
            );
        }
    }
}
