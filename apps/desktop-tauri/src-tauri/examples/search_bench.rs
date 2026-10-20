use serde::Deserialize;
#[path = "search_bench/encoder.rs"]
mod encoder;
#[path = "search_bench/legacy_router.rs"]
mod legacy_router;
#[path = "search_bench/legacy_router_weights.rs"]
mod legacy_router_weights;
use legacy_router::{classify_prompt_2d, normalize_for_routing, Route2D};
use serde_json::{json, Value};
use std::error::Error;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant as StdInstant};
use tokio::sync::Mutex;
use tokio::time::Instant;
use voxgolem_core::search::{EvidenceKind, Failure, Provider};
use voxgolem_platform::custom_openai::tools::ModelEvent;
use voxgolem_platform::custom_openai::{
    CustomOpenAiClient, CustomOpenAiConfig, CustomOpenAiContentType, CustomOpenAiError,
    CustomOpenAiMessage, CustomOpenAiModel, CustomOpenAiPrompt, CustomOpenAiRole,
};
use voxgolem_platform::search::{
    admit_web, prewarm_web, run_with_client_events, RunContext, RunError, RunEvent, WebPrewarmGate,
};
use voxgolem_platform::search_control::Cancellation;
use voxgolem_platform::web::{
    decode_result, McpTransport, Operation, ProviderFailure, WebService, WebTransport,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BenchMode {
    Encoder,
    Deterministic,
    Live,
}

#[derive(Debug)]
struct BenchArgs {
    mode: BenchMode,
    with_llm: bool,
    cases_path: PathBuf,
    model: CustomOpenAiModel,
    repetitions: usize,
    encoder_server: Option<PathBuf>,
    encoder_model: Option<PathBuf>,
    encoder_input: Option<encoder::Input>,
    encoder_task: Option<encoder::Task>,
    encoder_features: Option<PathBuf>,
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
    status: RowStatus,
    detail: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RowStatus {
    Passed,
    Failed,
    Skipped,
}

impl From<bool> for RowStatus {
    fn from(passed: bool) -> Self {
        if passed {
            Self::Passed
        } else {
            Self::Failed
        }
    }
}

impl ScenarioRow {
    fn passed(&self) -> bool {
        self.status == RowStatus::Passed
    }
    fn latency(&self) -> String {
        if self.status == RowStatus::Skipped || !self.elapsed_ms.is_finite() {
            "N/A".into()
        } else {
            format!("{:.2}", self.elapsed_ms)
        }
    }
}

#[derive(Default)]
struct DeterministicTransport {
    mode: DeterministicScenarioMode,
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
        if let Operation::Search {
            objective, queries, ..
        } = operation
        {
            assert_eq!(queries, std::slice::from_ref(objective));
        }

        match (self.mode, provider, operation) {
            (DeterministicScenarioMode::ExaRichSnippets, _, Operation::Fetch { .. }) => {
                Ok(json!({"content": []}))
            }
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
                    "text": "Title: Apple 2026 Quarterly Dividend Official Release\nURL: https://example.com/apple-dividend\nPublished: 2026-02-18\nAuthor: Synthetic IR Team\nText:\nSynthetic fixture: Apple declared a quarterly dividend of USD 0.74 per share."
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
    parse_args_from(std::env::args().skip(1))
}

fn parse_args_from(args: impl IntoIterator<Item = String>) -> Result<BenchArgs, Box<dyn Error>> {
    let mut mode = BenchMode::Deterministic;
    let mut with_llm = false;
    let mut model = CustomOpenAiModel::LunaNone;
    let mut repetitions = 1;
    let mut encoder_server = None;
    let mut encoder_model = None;
    let mut encoder_input = None;
    let mut encoder_task = None;
    let mut encoder_features = None;
    let mut cases_path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../crates/core/tests/fixtures/router_cases_132.json");

    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--mode" => {
                let value = args.next().ok_or("missing value for --mode")?;
                mode = match value.as_str() {
                    "deterministic" => BenchMode::Deterministic,
                    "live" => BenchMode::Live,
                    "encoder" => BenchMode::Encoder,
                    other => {
                        return Err(format!(
                            "unsupported --mode '{other}' (expected deterministic, live or encoder)"
                        )
                        .into())
                    }
                };
            }
            "--with-llm" => {
                with_llm = true;
            }
            "--encoder-server" => {
                encoder_server = Some(PathBuf::from(args.next().ok_or("missing encoder server")?))
            }
            "--encoder-model" => {
                encoder_model = Some(PathBuf::from(args.next().ok_or("missing encoder model")?))
            }
            "--encoder-input" => {
                encoder_input = Some(match args.next().as_deref() {
                    Some("legacy") => encoder::Input::Legacy,
                    Some("literal") => encoder::Input::Literal,
                    _ => return Err("expected encoder input legacy|literal".into()),
                })
            }
            "--encoder-task" => {
                encoder_task = Some(match args.next().as_deref() {
                    Some("routes") => encoder::Task::Routes,
                    Some("early-lookup") => encoder::Task::EarlyLookup,
                    _ => return Err("expected encoder task routes|early-lookup".into()),
                });
            }
            "--encoder-features" => {
                encoder_features = Some(PathBuf::from(
                    args.next().ok_or("missing feature output path")?,
                ));
            }
            "--model" => {
                model = match args.next().as_deref() {
                    Some("luna-none") => CustomOpenAiModel::LunaNone,
                    Some("sol-high") => CustomOpenAiModel::SolHigh,
                    _ => return Err("expected --model luna-none|sol-high".into()),
                };
            }
            "--repetitions" => {
                repetitions = args.next().ok_or("missing repetitions")?.parse()?;
                if !(1..=3).contains(&repetitions) {
                    return Err("repetitions must be 1..=3".into());
                }
            }
            "--cases" => {
                let value = args.next().ok_or("missing value for --cases")?;
                cases_path = PathBuf::from(value);
            }
            "--help" | "-h" => {
                println!(
                    "Usage: cargo run --locked -p vox-golem --example search_bench -- [--mode deterministic|live|encoder] [--with-llm] [--model luna-none|sol-high] [--repetitions 1..=3] [--cases PATH]\nEncoder requires --encoder-server PATH --encoder-model PATH; optional --encoder-input legacy|literal (default legacy), --encoder-task routes|early-lookup (default routes). Early-lookup requires its distinct expected-decision fixture and reports both fixed S-only and S-or-R mappings, without training. Explicit --encoder-features ABSOLUTE_PATH exports synthetic-fixture vectors outside the repository without reporting predictions; requires early-lookup, literal input, one repetition and new output. Encoder is isolated, CPU-only, owned local server; rejects --with-llm. Coverage completion is not semantic quality acceptance.\nDefaults: deterministic, luna-none, one repetition. Live is anonymous-only unless --with-llm. Model matrix: six requests per repetition, at most 18; 600 second whole-matrix budget. Measures local callback text, not pixels/audio; semantic/citation accuracy unassessed."
                );
                std::process::exit(0);
            }
            other => {
                return Err(format!("unrecognized argument '{other}'").into());
            }
        }
    }

    if mode == BenchMode::Encoder {
        if with_llm {
            return Err("encoder mode rejects --with-llm".into());
        }
        if encoder_server.is_none() || encoder_model.is_none() {
            return Err("encoder mode requires explicit server and model paths".into());
        }
        if encoder_features.is_some()
            && (encoder_task != Some(encoder::Task::EarlyLookup)
                || !matches!(encoder_input, Some(encoder::Input::Literal))
                || repetitions != 1)
        {
            return Err(
                "feature export requires early-lookup task, literal input and one repetition"
                    .into(),
            );
        }
    } else if encoder_server.is_some()
        || encoder_model.is_some()
        || encoder_input.is_some()
        || encoder_task.is_some()
        || encoder_features.is_some()
    {
        return Err("encoder flags require encoder mode".into());
    }
    Ok(BenchArgs {
        mode,
        with_llm,
        cases_path,
        model,
        repetitions,
        encoder_server,
        encoder_model,
        encoder_input,
        encoder_task,
        encoder_features,
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
    let mut rows = Vec::new();
    rows.push(ScenarioRow {
        id: "00_router_132_suite",
        codepath: "search_bench::legacy_router",
        route: "F/S/C/R".to_string(),
        elapsed_ms: (avg_us * (case_count as f64)) / 1000.0,
        status: (case_count == 132).into(),
        detail: format!("parked legacy research: {case_count}/132 normalizer parity ({avg_us:.2} us/case); not production route or quality acceptance"),
    });

    // Component timings use synthetic transport responses, not model routing or live latency.
    let search = Operation::Search {
        objective: "Synthetic component query".into(),
        queries: vec!["Synthetic component query".into()],
        limit: 3,
    };
    for (id, mode) in [
        (
            "title_url_fallback",
            DeterministicScenarioMode::ExaUntitledWeather,
        ),
        (
            "internal_hr_records",
            DeterministicScenarioMode::ExaMultiRecordInternalHr,
        ),
        (
            "empty_fetch_gap",
            DeterministicScenarioMode::ExaRichSnippets,
        ),
        (
            "published_page_read",
            DeterministicScenarioMode::ExaThinSnippetsAndPublishedFetch,
        ),
        (
            "soft_ratelimit_fallback",
            DeterministicScenarioMode::ExaSoftRateLimitToParallel,
        ),
        (
            "prewarm_deadline_no_cooldown",
            DeterministicScenarioMode::PrewarmDeadline,
        ),
    ] {
        let started = StdInstant::now();
        let mut service = WebService::new(DeterministicTransport { mode });
        let cancel = Cancellation::default();
        if matches!(mode, DeterministicScenarioMode::PrewarmDeadline) {
            service
                .prewarm_all(Instant::now() + Duration::from_secs(1), &cancel)
                .await;
        }
        let out = service
            .execute(
                &search,
                "synthetic-component",
                id,
                Instant::now() + Duration::from_secs(5),
                &cancel,
            )
            .await
            .map_err(|_| "synthetic Search failed")?;
        let passed = match mode {
            DeterministicScenarioMode::ExaUntitledWeather => {
                out.sources.len() == 1 && out.sources[0].title == out.sources[0].url
            }
            DeterministicScenarioMode::ExaMultiRecordInternalHr => {
                out.sources.len() == 3
                    && out
                        .sources
                        .iter()
                        .all(|s| s.kind == EvidenceKind::SearchExcerpt)
            }
            DeterministicScenarioMode::ExaRichSnippets
            | DeterministicScenarioMode::ExaThinSnippetsAndPublishedFetch => {
                let fetch = Operation::Fetch {
                    urls: vec![out.sources[0].url.clone()],
                    focus: "Synthetic payout details".into(),
                };
                let fetched = service
                    .execute(
                        &fetch,
                        "synthetic-component",
                        id,
                        Instant::now() + Duration::from_secs(5),
                        &cancel,
                    )
                    .await;
                if matches!(mode, DeterministicScenarioMode::ExaRichSnippets) {
                    // The empty SDK payload is invalid evidence, including after
                    // fallback; do not disguise a typed failure as a readable page.
                    matches!(fetched, Err(Failure::InvalidUpstream))
                } else {
                    fetched
                        .map_err(|_| "synthetic Fetch failed")?
                        .sources
                        .iter()
                        .any(|s| {
                            s.kind == EvidenceKind::PageRead
                                && s.title == "Apple 2026 Quarterly Dividend Official Release"
                                && s.url == "https://example.com/apple-dividend"
                                && s.text.contains("USD 0.74")
                        })
                }
            }
            DeterministicScenarioMode::ExaSoftRateLimitToParallel => {
                out.fallback_used
                    && out.sources.iter().any(|s| s.provider == Provider::Parallel)
                    && out.raw_text.is_none()
            }
            DeterministicScenarioMode::PrewarmDeadline => {
                !out.fallback_used && out.sources.iter().all(|s| s.provider == Provider::Exa)
            }
            _ => false,
        };
        rows.push(ScenarioRow {
            id,
            codepath: "web::execute",
            route: "component/mock".into(),
            elapsed_ms: started.elapsed().as_secs_f64() * 1000.0,
            status: passed.into(),
            detail:
                "Synthetic Search/Fetch operations; no model/tool-selection or live-latency claim"
                    .into(),
        });
    }
    // Decode soft failures without turning provider error text into evidence.
    let mut next_id = 1;
    let decoded = decode_result(
        Provider::Exa,
        &search,
        &json!({"content":[{"type":"text","text":"You've hit Exa's free MCP rate limit."}]}),
        "synthetic-rate-limit",
        &mut next_id,
    )
    .map_err(|_| "synthetic soft-failure decode failed")?;
    rows.push(ScenarioRow {
        id: "soft_failure_decode",
        codepath: "web::decode_result",
        route: "component/mock".into(),
        elapsed_ms: f64::NAN,
        status: (decoded.raw_text.is_none()
            && decoded
                .failures
                .iter()
                .any(|f| f.failure == Failure::RateLimited))
        .into(),
        detail: "Provider soft failure is not evidence; latency not measured".into(),
    });

    for (id, cancelled) in [
        ("expired_operation_deadline", false),
        ("cancelled_operation", true),
    ] {
        let mut service = WebService::new(DeterministicTransport::default());
        let cancel = Cancellation::default();
        if cancelled {
            cancel.cancel();
        }
        let result = service
            .execute(&search, "synthetic-component", id, Instant::now(), &cancel)
            .await;
        rows.push(ScenarioRow { id, codepath: "web::execute", route: "component/mock".into(),
            elapsed_ms: f64::NAN,
            status: matches!(result, Err(failure) if failure == if cancelled { Failure::Cancelled } else { Failure::Deadline }).into(),
            detail: "Expired/cancelled operation rejected before provider call; latency not measured".into() });
    }
    Ok(rows)
}

struct TextObserver {
    started: StdInstant,
    first_ms: Option<f64>,
}

impl TextObserver {
    fn new() -> Self {
        Self {
            started: StdInstant::now(),
            first_ms: None,
        }
    }
    fn observe(&mut self, text: &str) {
        if self.first_ms.is_none() && !text.trim().is_empty() {
            self.first_ms = Some(self.started.elapsed().as_secs_f64() * 1000.0);
        }
    }
}

struct ObservedTransport<T> {
    inner: T,
    ready: std::sync::Arc<std::sync::Mutex<[Option<bool>; 2]>>,
}

impl<T> ObservedTransport<T> {
    fn new(inner: T) -> Self {
        Self {
            inner,
            ready: Default::default(),
        }
    }
}

impl<T: WebTransport> WebTransport for ObservedTransport<T> {
    async fn prewarm(
        &mut self,
        provider: Provider,
        deadline: Instant,
        cancel: &Cancellation,
    ) -> Result<(), ProviderFailure> {
        let result = self.inner.prewarm(provider, deadline, cancel).await;
        self.ready.lock().unwrap()[if provider == Provider::Exa { 0 } else { 1 }] =
            Some(result.is_ok());
        result
    }
    async fn prewarm_both(
        &mut self,
        exa: bool,
        parallel: bool,
        deadline: Instant,
        cancel: &Cancellation,
    ) -> (Result<(), ProviderFailure>, Result<(), ProviderFailure>) {
        // Forward the override: McpTransport warms both providers concurrently.
        let result = self
            .inner
            .prewarm_both(exa, parallel, deadline, cancel)
            .await;
        let mut ready = self.ready.lock().unwrap();
        if exa {
            ready[0] = Some(result.0.is_ok());
        }
        if parallel {
            ready[1] = Some(result.1.is_ok());
        }
        result
    }
    async fn call(
        &mut self,
        provider: Provider,
        operation: &Operation,
        conversation_id: &str,
        deadline: Instant,
        cancel: &Cancellation,
    ) -> Result<Value, ProviderFailure> {
        self.inner
            .call(provider, operation, conversation_id, deadline, cancel)
            .await
    }
}

fn skipped_model_row(id: &'static str, reason: &'static str, repetition: usize) -> ScenarioRow {
    ScenarioRow {
        id,
        codepath: "model_matrix",
        route: "-".into(),
        elapsed_ms: 0.0,
        status: RowStatus::Skipped,
        detail: format!("skipped={reason} repetition={repetition}"),
    }
}

fn benchmark_client(model: CustomOpenAiModel) -> Result<CustomOpenAiClient, &'static str> {
    use voxgolem_core::config::{
        default_config_path, load_runtime_config, managed_chatgpt_auth_path,
    };
    let runtime = load_runtime_config(None).map_err(|_| "config_unavailable")?;
    let managed =
        managed_chatgpt_auth_path(&default_config_path().map_err(|_| "config_unavailable")?);
    client_from_selection(Some(&managed), runtime.custom_openai.as_ref(), model)
}

fn client_from_selection(
    managed: Option<&Path>,
    fallback: Option<&voxgolem_core::config::CustomOpenAiConfig>,
    model: CustomOpenAiModel,
) -> Result<CustomOpenAiClient, &'static str> {
    use voxgolem_core::config::{effective_custom_openai_config, CustomOpenAiAuthSource};
    let config = effective_custom_openai_config(managed, fallback).ok_or("auth_unavailable")?;
    // WSL paths cannot be opened as native files. Never guess an alternate account.
    if config.auth_source != CustomOpenAiAuthSource::Native {
        return Err("auth_source_unsupported");
    }
    let client = CustomOpenAiClient::new(CustomOpenAiConfig {
        endpoint: config.endpoint,
        auth_path: config.auth_path,
        model,
        ..Default::default()
    })
    .map_err(|error| BenchFailure::from(error).reason)?;
    client
        .check_auth()
        .map_err(|error| BenchFailure::from(error).reason)?;
    Ok(client)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct BenchFailure {
    reason: &'static str,
    stop: bool,
}

impl From<CustomOpenAiError> for BenchFailure {
    fn from(error: CustomOpenAiError) -> Self {
        use CustomOpenAiError::*;
        let (reason, stop) = match error {
            AuthUnavailable => ("auth_unavailable", true),
            AuthExpired => ("auth_expired", true),
            Unauthorized => ("auth_rejected", true),
            Forbidden => ("forbidden", true),
            RateLimited => ("rate_limited", true),
            Http(400) => ("unsupported_request", true),
            Http(401) => ("auth_rejected", true),
            Http(403) => ("forbidden", true),
            Http(429) => ("rate_limited", true),
            Http(_) => ("http_failed", false),
            Timeout => ("request_timeout", false),
            Transport => ("transport_failed", false),
            InvalidConfig => ("invalid_config", true),
            InvalidPrompt => ("invalid_prompt", false),
            UnexpectedContentType => ("unexpected_content_type", false),
            MalformedStream => ("malformed_stream", false),
            MixedContent => ("mixed_content", false),
            EmptyResponse => ("empty_response", false),
            Incomplete => ("incomplete_response", false),
            Failed => ("provider_failed", false),
            TooLarge => ("size_limit", false),
        };
        Self { reason, stop }
    }
}

impl From<RunError> for BenchFailure {
    fn from(error: RunError) -> Self {
        match error {
            RunError::Provider(error) => error.into(),
            RunError::Tool(_) => Self {
                reason: "tool_failed",
                stop: false,
            },
            RunError::Protocol => Self {
                reason: "protocol_failed",
                stop: false,
            },
            RunError::CallbackFailed => Self {
                reason: "callback_failed",
                stop: false,
            },
        }
    }
}

struct ModelOutcome {
    text: String,
    kind: CustomOpenAiContentType,
    sources: usize,
    searches: usize,
    reads: usize,
    page_reads: usize,
    source_read_observed: bool,
    partial: bool,
    fallbacks: usize,
}

impl ModelOutcome {
    fn passes(&self, id: &str, allows_no_sources: bool) -> bool {
        self.kind == CustomOpenAiContentType::OutputText
            && !self.text.trim().is_empty()
            && (allows_no_sources || self.sources > 0)
            && (!matches!(id, "page_read" | "mixed")
                || (self.source_read_observed && self.page_reads > 0))
            && (id != "arithmetic" || self.text.trim() == "210")
            && (id != "history" || self.text.trim() == "SYNTHETIC-ORBIT-42")
    }
}

fn model_matrix_cases() -> Vec<(&'static str, &'static str, bool, bool)> {
    static SCENARIOS: std::sync::LazyLock<serde_json::Value> = std::sync::LazyLock::new(|| {
        serde_json::from_str(include_str!("../../../../tools/benchmark/scenarios.json"))
            .expect("checked-in synthetic model matrix must be valid JSON")
    });
    let scenarios: &'static serde_json::Value = &SCENARIOS;
    scenarios
        .as_array()
        .expect("model matrix must be an array")
        .iter()
        .map(|case| {
            (
                case["id"].as_str().expect("scenario id"),
                case["prompt"].as_str().expect("scenario prompt"),
                case["web_enabled"].as_bool().expect("scenario Web flag"),
                case["allows_no_sources"]
                    .as_bool()
                    .expect("scenario source policy"),
            )
        })
        .collect()
}

async fn run_model_matrix(args: &BenchArgs) -> Vec<ScenarioRow> {
    let cases = model_matrix_cases();
    let client = benchmark_client(args.model);
    let deadline = Instant::now() + Duration::from_secs(600);
    let mut rows = Vec::new();
    let mut service = WebService::new(McpTransport::default());
    let mut stopped = None;
    for repetition in 1..=args.repetitions {
        for &(id, text, web_enabled, allows_no_sources) in &cases {
            if let Some(reason) = stopped {
                rows.push(skipped_model_row(id, reason, repetition));
                continue;
            }
            let client = match &client {
                Ok(client) => client,
                Err(reason) => {
                    rows.push(skipped_model_row(id, reason, repetition));
                    continue;
                }
            };
            if Instant::now() >= deadline {
                rows.push(skipped_model_row(id, "matrix_deadline", repetition));
                continue;
            }
            let history = if id == "history" {
                // Host component condition: 24 injected messages, not the full-app UI seed/followup.
                (0..24).map(|i| CustomOpenAiMessage { role: CustomOpenAiRole::User, content_type: CustomOpenAiContentType::OutputText, text: if i == 0 { "The synthetic marker is SYNTHETIC-ORBIT-42.".into() } else { format!("Synthetic history turn {i}: retain the marker while discussing integer arithmetic.") } }).collect()
            } else {
                Vec::new()
            };
            let prompt = CustomOpenAiPrompt {
                session_id: "synthetic-benchmark".into(),
                prompt: text.into(),
                history,
            };
            let cancel = Cancellation::default();
            let mut observer = TextObserver::new();
            let request = async {
                run_with_client_events(
                    client,
                    &prompt,
                    RunContext {
                        conversation_id: "synthetic-benchmark",
                        instructions: voxgolem_core::response_style::PLAIN_TEXT_POLICY,
                        web_enabled,
                    },
                    &mut service,
                    &cancel,
                    |event| {
                        if let RunEvent::Model(ModelEvent::TextDelta { text, .. }) = event {
                            observer.observe(text);
                        }
                        Ok(())
                    },
                )
                .await
                .map(|done| ModelOutcome {
                    text: done.response.text,
                    kind: done.response.content_type,
                    sources: done.sources.len(),
                    searches: done.searches,
                    reads: done.reads,
                    page_reads: done
                        .sources
                        .iter()
                        .filter(|source| {
                            source.kind == EvidenceKind::PageRead && !source.text.trim().is_empty()
                        })
                        .count(),
                    source_read_observed: done.source_read_observed,
                    partial: done.partial,
                    fallbacks: done.fallbacks,
                })
                .map_err(BenchFailure::from)
            };
            let result = tokio::time::timeout_at(deadline, request).await;
            let mut row = ScenarioRow { id, codepath: "search::run_with_client_events", route: "-".into(), elapsed_ms: observer.started.elapsed().as_secs_f64() * 1000.0, status: RowStatus::Failed, detail: format!("model={} repetition={repetition} scope=local_request_to_visible_text_callback first_visible_text_ms={:?}; semantic/citation_accuracy=unassessed", args.model.model_id(), observer.first_ms) };
            row.detail.push_str(&format!(" web_enabled={web_enabled}"));
            if id == "history" {
                row.detail
                    .push_str(" history_condition=24-injected-user-messages-not-ui-seeded");
            }
            match result {
                Ok(Ok(outcome)) => {
                    row.status = outcome.passes(id, allows_no_sources).into();
                    row.detail.push_str(&format!(" output_kind={} sources={} searches={} reads={} readable_page_reads={} source_read_observed={} partial={} fallbacks={}", outcome.kind.as_str(), outcome.sources, outcome.searches, outcome.reads, outcome.page_reads, outcome.source_read_observed, outcome.partial, outcome.fallbacks));
                }
                Ok(Err(error)) => {
                    row.detail.push_str(&format!(" outcome={}", error.reason));
                    if error.stop {
                        stopped = Some(error.reason);
                    }
                }
                Err(_) => {
                    cancel.cancel();
                    stopped = Some("matrix_deadline");
                    row.detail.push_str(" outcome=matrix_deadline");
                }
            }
            rows.push(row);
        }
    }
    rows
}

async fn run_live_suite(args: &BenchArgs) -> Result<Vec<ScenarioRow>, Box<dyn Error>> {
    let mut rows = match run_anonymous_suite().await {
        Ok(rows) => rows,
        Err(_) => vec![ScenarioRow {
            id: "anonymous_suite",
            codepath: "MCP",
            route: "-".into(),
            elapsed_ms: f64::NAN,
            status: RowStatus::Failed,
            detail: "outcome=anonymous_suite_failed; subsequent anonymous coverage incomplete"
                .into(),
        }],
    };
    if args.with_llm {
        rows.extend(run_model_matrix(args).await);
    }
    Ok(rows)
}

async fn run_anonymous_suite() -> Result<Vec<ScenarioRow>, Box<dyn Error>> {
    let mut rows = Vec::new();
    let gate = WebPrewarmGate::default();
    let observed = ObservedTransport::new(McpTransport::default());
    let ready = observed.ready.clone();
    let web_mutex = Mutex::new(WebService::new(observed));

    // 1. Live concurrent MCP prewarm (Exa + Parallel)
    {
        let t0 = StdInstant::now();
        prewarm_web(&web_mutex, &gate).await;
        let ms = t0.elapsed().as_secs_f64() * 1000.0;
        let snapshot = *ready.lock().unwrap();
        rows.push(prewarm_row(snapshot, ms));
    }

    // Explicit component operations; this does not benchmark automatic model selection.
    for (id, objective) in [
        ("live_02_warm_search", "Current New York City weather temperature"),
        (
            "live_03_rules_search",
            "Urza's Saga Construct token ability after land destruction",
        ),
    ] {
        let started = StdInstant::now();
        let cancel = Cancellation::default();
        let mut service = admit_web(
            &web_mutex,
            &gate,
            &cancel,
            Instant::now() + Duration::from_secs(5),
        )
        .await
        .map_err(|_| "web admission failed")?;
        let operation = Operation::Search {
            objective: objective.into(),
            queries: vec![objective.into()],
            limit: 3,
        };
        let out = service
            .execute(
                &operation,
                "synthetic-live-component",
                id,
                Instant::now() + Duration::from_secs(12),
                &cancel,
            )
            .await
            .map_err(|_| "live search failed")?;
        rows.push(ScenarioRow {
            id,
            codepath: "web::execute",
            route: "explicit Search component".into(),
            elapsed_ms: started.elapsed().as_secs_f64() * 1000.0,
            status: (!out.sources.is_empty()).into(),
            detail: format!(
                "sources={} fallback={}; no model selection exercised",
                out.sources.len(),
                out.fallback_used
            ),
        });
    }

    Ok(rows)
}

fn prewarm_row(snapshot: [Option<bool>; 2], elapsed_ms: f64) -> ScenarioRow {
    ScenarioRow {
        id: "live_01_concurrent_mcp_prewarm",
        codepath: "search::prewarm_web",
        route: "-".into(),
        elapsed_ms,
        status: (snapshot == [Some(true), Some(true)]).into(),
        detail: format!("provider_readiness={snapshot:?}; concurrent MCP handshake"),
    }
}

fn print_table(title: &str, rows: &[ScenarioRow]) {
    println!("### {title}");
    println!("| Scenario | Codepath | Model / component / parked diagnostic | Latency (ms) | Status | Detail |");
    println!("| :--- | :--- | :---: | ---: | :---: | :--- |");
    for row in rows {
        let status = match row.status {
            RowStatus::Skipped => "SKIP",
            RowStatus::Passed => "PASS",
            RowStatus::Failed => "FAIL",
        };
        println!(
            "| `{}` | `{}` | `{}` | {} | **{}** | {} |",
            row.id,
            row.codepath,
            row.route,
            row.latency(),
            status,
            row.detail
        );
    }
}

fn deterministic_model_coverage(args: &BenchArgs) -> Vec<ScenarioRow> {
    if args.mode == BenchMode::Deterministic && args.with_llm {
        vec![skipped_model_row(
            "model_matrix",
            "deterministic_network_disabled",
            1,
        )]
    } else {
        Vec::new()
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

    if args.mode == BenchMode::Encoder {
        // The owner is retired inside run before a nonzero exit, including failure paths.
        if !encoder::run(&args) {
            std::process::exit(1);
        }
        return;
    }
    let det_rows = match run_deterministic_suite(&args.cases_path).await {
        Ok(rows) => rows,
        Err(err) => {
            eprintln!("search_bench deterministic suite error: {err}");
            std::process::exit(1);
        }
    };
    print_table(
        "Deterministic Web Components & Parked Router Diagnostics",
        &det_rows,
    );

    let mut all_passed = det_rows.iter().all(ScenarioRow::passed);

    let rows = deterministic_model_coverage(&args);
    if !rows.is_empty() {
        print_table("Requested Model Coverage", &rows);
        all_passed &= rows.iter().all(ScenarioRow::passed);
    }

    if args.mode == BenchMode::Live {
        println!();
        let live_rows = match run_live_suite(&args).await {
            Ok(rows) => rows,
            Err(err) => {
                eprintln!("search_bench live suite error: {err}");
                std::process::exit(1);
            }
        };
        print_table("Live MCP & Provider Search Suite", &live_rows);
        all_passed &= live_rows.iter().all(ScenarioRow::passed);
    }

    if !all_passed {
        eprintln!("search_bench: requested coverage failed or was skipped/incomplete");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn shared_model_matrix_loads_through_production_parser() {
        let cases = super::model_matrix_cases();
        assert_eq!(cases.len(), 6);
        assert!(cases.iter().all(|(_, prompt, _, _)| !prompt.is_empty()));
        assert_eq!(cases[0].0, "arithmetic");
        assert!(!cases[0].2);
        assert!(cases[1].2); // Preserve the historical coding Web flag.
        assert_eq!(cases[5].0, "history");
        assert!(!cases[5].2);
    }

    use super::*;

    #[test]
    fn encoder_accepts_explicit_literal_feature_export() {
        assert!(args(&[
            "--mode",
            "encoder",
            "--encoder-server",
            "synthetic-server",
            "--encoder-model",
            "synthetic-model",
            "--encoder-task",
            "early-lookup",
            "--encoder-input",
            "literal",
            "--encoder-features",
            "/tmp/opencode/synthetic-features.json",
        ])
        .is_ok());
    }

    #[test]
    fn encoder_accepts_retrieval_opportunity_task() {
        assert!(args(&[
            "--mode",
            "encoder",
            "--encoder-server",
            "synthetic-server",
            "--encoder-model",
            "synthetic-model",
            "--encoder-task",
            "early-lookup",
        ])
        .is_ok());
        assert!(args(&[
            "--encoder-features",
            "/tmp/opencode/synthetic-features.json"
        ])
        .is_err());
        assert!(args(&[
            "--mode",
            "encoder",
            "--encoder-server",
            "synthetic-server",
            "--encoder-model",
            "synthetic-model",
            "--encoder-features",
            "/tmp/opencode/synthetic-features.json"
        ])
        .is_err());
        assert!(args(&[
            "--mode",
            "encoder",
            "--encoder-server",
            "synthetic-server",
            "--encoder-model",
            "synthetic-model",
            "--encoder-task",
            "early-lookup",
            "--encoder-input",
            "literal",
            "--repetitions",
            "2",
            "--encoder-features",
            "/tmp/opencode/synthetic-features.json"
        ])
        .is_err());
    }

    #[test]
    fn encoder_cli_is_isolated() {
        assert!(args(&[
            "--mode",
            "encoder",
            "--encoder-server",
            "synthetic-server",
            "--encoder-model",
            "synthetic-model"
        ])
        .is_ok());
        assert!(args(&["--mode", "encoder", "--with-llm"]).is_err());
        assert!(args(&["--encoder-input", "literal"]).is_err());
    }

    fn args(values: &[&str]) -> Result<BenchArgs, Box<dyn Error>> {
        parse_args_from(values.iter().map(|value| (*value).to_string()))
    }

    #[test]
    fn cli_bounds_and_network_free_requested_coverage() {
        for value in ["0", "4"] {
            assert!(args(&["--repetitions", value]).is_err());
        }
        for value in ["1", "3"] {
            assert_eq!(
                args(&["--repetitions", value]).unwrap().repetitions,
                value.parse::<usize>().unwrap()
            );
        }
        assert!(args(&["--model", "synthetic-invalid"]).is_err());
        assert_eq!(
            args(&["--model", "sol-high"]).unwrap().model,
            CustomOpenAiModel::SolHigh
        );
        let requested = args(&["--mode", "deterministic", "--with-llm"]).unwrap();
        // This path creates coverage only: no resolver/client/transport is needed.
        let rows = deterministic_model_coverage(&requested);
        assert_eq!(rows.len(), 1);
        assert!(!rows.iter().all(ScenarioRow::passed));
        assert_eq!(rows[0].status, RowStatus::Skipped);
        assert!(deterministic_model_coverage(&args(&[]).unwrap()).is_empty());
        assert!(!args(&["--mode", "live"]).unwrap().with_llm);
    }

    #[test]
    fn synthetic_missing_auth_produces_incomplete_requested_coverage() {
        let temp = tempfile::tempdir().unwrap();
        let missing = temp.path().join("synthetic-missing-auth.json");
        let fallback = voxgolem_core::config::CustomOpenAiConfig {
            endpoint: CustomOpenAiConfig::default().endpoint,
            auth_path: missing,
            auth_source: voxgolem_core::config::CustomOpenAiAuthSource::Native,
        };
        let result = client_from_selection(None, Some(&fallback), CustomOpenAiModel::LunaNone);
        let reason = result
            .err()
            .expect("missing synthetic auth must fail before requests");
        assert_eq!(reason, "auth_unavailable");
        let rows = [skipped_model_row("coding", reason, 1)];
        assert!(!rows.iter().all(ScenarioRow::passed));
        assert_eq!(rows[0].status, RowStatus::Skipped);
        assert!(client_from_selection(None, None, CustomOpenAiModel::LunaNone).is_err());
        // App-owned selection is derived even when no raw custom entry exists.
        let managed =
            voxgolem_core::config::managed_chatgpt_auth_path(&temp.path().join("config.toml"));
        assert_eq!(
            client_from_selection(Some(&managed), None, CustomOpenAiModel::LunaNone).err(),
            Some("auth_unavailable")
        );
    }

    fn outcome() -> ModelOutcome {
        ModelOutcome {
            text: "Synthetic answer".into(),
            kind: CustomOpenAiContentType::OutputText,
            sources: 1,
            searches: 1,
            reads: 1,
            page_reads: 0,
            source_read_observed: false,
            partial: true,
            fallbacks: 1,
        }
    }

    #[test]
    fn read_attempts_without_readable_page_evidence_do_not_pass() {
        let mut done = outcome();
        assert!(done.passes("lookup", false));
        for id in ["page_read", "mixed"] {
            assert!(!done.passes(id, false));
        }
        done.source_read_observed = true;
        assert!(!done.passes("page_read", false));
        done.page_reads = 1;
        assert!(done.passes("page_read", false));
        assert!(done.passes("mixed", false));
        // Partial evidence/fallback are disclosed, not blanket failures.
        assert!(done.partial);
    }

    #[test]
    fn refusals_are_not_normal_answers() {
        let mut done = outcome();
        assert!(done.passes("coding", true));
        done.kind = CustomOpenAiContentType::Refusal;
        assert!(!done.passes("coding", true));
        assert!(!done.passes("mixed", false));
    }

    #[test]
    fn terminal_failures_stop_remaining_requests() {
        for error in [
            CustomOpenAiError::AuthUnavailable,
            CustomOpenAiError::AuthExpired,
            CustomOpenAiError::Unauthorized,
            CustomOpenAiError::Forbidden,
            CustomOpenAiError::RateLimited,
            CustomOpenAiError::Http(400),
            CustomOpenAiError::Http(401),
            CustomOpenAiError::Http(403),
            CustomOpenAiError::Http(429),
        ] {
            let failure = BenchFailure::from(RunError::Provider(error));
            assert!(failure.stop);
            let row = skipped_model_row("history", failure.reason, 1);
            assert_eq!(row.status, RowStatus::Skipped);
            assert!(!row.passed());
        }
        assert!(!BenchFailure::from(CustomOpenAiError::Transport).stop);
        assert!(!BenchFailure::from(CustomOpenAiError::Timeout).stop);
    }

    #[tokio::test]
    async fn single_provider_prewarm_forwards_failure() {
        let mut transport = ObservedTransport::new(DeterministicTransport {
            mode: DeterministicScenarioMode::PrewarmDeadline,
        });
        assert!(transport
            .prewarm(
                Provider::Exa,
                Instant::now() + Duration::from_secs(1),
                &Cancellation::default()
            )
            .await
            .is_err());
        assert_eq!(*transport.ready.lock().unwrap(), [Some(false), None]);
    }

    #[tokio::test]
    async fn both_provider_prewarm_preserves_inner_override() {
        struct BothOnly;
        impl WebTransport for BothOnly {
            async fn prewarm(
                &mut self,
                _: Provider,
                _: Instant,
                _: &Cancellation,
            ) -> Result<(), ProviderFailure> {
                panic!("sequential trait default must not replace the inner override");
            }
            async fn prewarm_both(
                &mut self,
                _: bool,
                _: bool,
                _: Instant,
                _: &Cancellation,
            ) -> (Result<(), ProviderFailure>, Result<(), ProviderFailure>) {
                (Err(ProviderFailure::from(Failure::Deadline)), Ok(()))
            }
            async fn call(
                &mut self,
                _: Provider,
                _: &Operation,
                _: &str,
                _: Instant,
                _: &Cancellation,
            ) -> Result<Value, ProviderFailure> {
                panic!("prewarm regression must not call providers");
            }
        }
        let mut transport = ObservedTransport::new(BothOnly);
        let result = transport
            .prewarm_both(true, true, Instant::now(), &Cancellation::default())
            .await;
        assert!(result.0.is_err());
        assert!(result.1.is_ok());
        let snapshot = *transport.ready.lock().unwrap();
        assert_eq!(snapshot, [Some(false), Some(true)]);
        assert!(!prewarm_row(snapshot, 0.1).passed());
    }

    #[test]
    fn whitespace_is_not_visible_and_completion_is_not_first_text() {
        let mut observer = TextObserver::new();
        observer.observe(" \n\t");
        assert_eq!(observer.first_ms, None);
        observer.observe("answer");
        let first = observer.first_ms;
        assert!(first.is_some());
        observer.observe("later");
        assert_eq!(observer.first_ms, first);
    }

    #[test]
    fn skipped_requested_coverage_is_incomplete() {
        let row = skipped_model_row("arithmetic", "auth_unavailable", 1);
        assert_eq!(row.status, RowStatus::Skipped);
        assert!(!row.passed());
        assert_eq!(row.latency(), "N/A");
    }

    #[tokio::test]
    async fn fast_prewarm_failure_is_not_readiness() {
        let mut transport = ObservedTransport::new(DeterministicTransport {
            mode: DeterministicScenarioMode::PrewarmDeadline,
        });
        let _ = transport
            .prewarm_both(
                true,
                true,
                Instant::now() + Duration::from_secs(1),
                &Cancellation::default(),
            )
            .await;
        assert_eq!(*transport.ready.lock().unwrap(), [Some(false), Some(false)]);
        let snapshot = *transport.ready.lock().unwrap();
        let row = prewarm_row(snapshot, 0.1);
        assert_eq!(row.status, RowStatus::Failed);
        assert_eq!(row.latency(), "0.10");
    }

    #[tokio::test]
    async fn deterministic_cross_codepath_suite_passes() {
        let cases_path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../crates/core/tests/fixtures/router_cases_132.json");
        let rows = run_deterministic_suite(&cases_path)
            .await
            .expect("deterministic suite runs");
        for id in [
            "title_url_fallback",
            "internal_hr_records",
            "empty_fetch_gap",
            "published_page_read",
            "soft_ratelimit_fallback",
            "prewarm_deadline_no_cooldown",
            "soft_failure_decode",
            "expired_operation_deadline",
            "cancelled_operation",
        ] {
            assert!(
                rows.iter().any(|row| row.id == id),
                "missing component: {id}"
            );
        }
        for row in &rows {
            assert!(
                row.passed(),
                "scenario {} ({}) failed: {}",
                row.id,
                row.codepath,
                row.detail
            );
        }
    }
}
