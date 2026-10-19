//! PC-only benchmark adapter; neither input variant changes production routing.
use super::legacy_router::{classify_bge_small_embedding, normalize_for_routing, Route2D};
use super::BenchArgs;
use serde::{Deserialize, Serialize};
use std::{
    io::{Read, Write},
    net::TcpListener,
    path::{Path, PathBuf},
    time::Instant,
};
use voxgolem_platform::{
    inference::InferencePolicy,
    llama_cpp::{
        LlamaCppRuntime, LlamaCppRuntimeError, LlamaCppServerSpec, LlamaCppStartupCancellation,
    },
};

const MAX_INPUT: usize = 32 * 1024;
const MAX_FIXTURE: u64 = 40 * 1024 * 1024;
const ROUTING_BUDGET_MS: f64 = 250.0;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum Task {
    #[default]
    Routes,
    EarlyLookup,
}

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Opportunity {
    ModelLed,
    EarlyLookup,
}

#[derive(Clone, Copy, Debug, Default)]
pub(super) enum Input {
    #[default]
    Legacy,
    Literal,
}

impl Input {
    fn version(self) -> &'static str {
        match self {
            Self::Legacy => "legacy-production-normalizer-v1",
            Self::Literal => "literal-labelled-context-v1",
        }
    }
    fn text(self, case: &Case) -> String {
        match self {
            Self::Legacy => normalize_for_routing(&case.prompt, case.prev.as_deref()),
            Self::Literal => match &case.prev {
                None => case.prompt.clone(),
                Some(prev) => format!(
                    "Prior user context:\n{prev}\n\nCurrent user request:\n{}",
                    case.prompt
                ),
            },
        }
    }
}

#[derive(Deserialize)]
struct Case {
    exp: Option<String>,
    expected: Option<Opportunity>,
    prompt: String,
    prev: Option<String>,
    // Original py_norm and optional id/family are accepted but not quality oracles.
    #[serde(rename = "py_norm")]
    _py_norm: Option<String>,
    #[serde(rename = "id")]
    _id: Option<String>,
    #[serde(rename = "family")]
    _family: Option<String>,
}

fn cases(raw: &str, input: Input, task: Task) -> Result<Vec<(usize, Case)>, &'static str> {
    let cases: Vec<Case> = serde_json::from_str(raw).map_err(|_| "fixture_malformed")?;
    if cases.is_empty() || cases.len() > 512 {
        return Err("fixture_case_limit");
    }
    cases
        .into_iter()
        .map(|case| {
            let label = match (task, &case.exp, case.expected) {
                (Task::Routes, Some(exp), None) => ["F", "S", "C", "R"]
                    .iter()
                    .position(|label| *label == exp.as_str())
                    .ok_or("fixture_label_invalid")?,
                (Task::EarlyLookup, None, Some(expected)) => {
                    usize::from(matches!(expected, Opportunity::EarlyLookup))
                }
                _ => return Err("fixture_task_labels_invalid"),
            };
            if case.prompt.trim().is_empty()
                || case.prompt.len() > MAX_INPUT
                || case
                    .prev
                    .as_ref()
                    .is_some_and(|p| p.trim().is_empty() || p.len() > MAX_INPUT)
            {
                return Err("fixture_input_invalid");
            }
            let text = input.text(&case);
            if text.trim().is_empty() || text.len() > MAX_INPUT {
                return Err("encoder_input_limit");
            }
            Ok((label, case))
        })
        .collect()
}

#[derive(Default)]
struct Report {
    task: Task,
    requested: usize,
    attempted: usize,
    completed: usize,
    errors: usize,
    exact: usize,
    top2: usize,
    evidence: usize,
    confusion: [[usize; 4]; 4],
    times: Vec<f64>,
    failure: Option<(&'static str, usize)>,
    mismatches: Vec<(usize, usize, usize)>,
    opportunities: [OpportunityScore; 2],
}

#[derive(Default)]
struct OpportunityScore {
    tp: usize,
    tn: usize,
    fp: usize,
    fn_: usize,
    correct_within_budget: usize,
    misses: Vec<usize>,
}

impl OpportunityScore {
    fn observe(&mut self, expected: bool, predicted: bool, elapsed_ms: f64, index: usize) {
        match (expected, predicted) {
            (true, true) => self.tp += 1,
            (false, false) => self.tn += 1,
            (false, true) => self.fp += 1,
            (true, false) => self.fn_ += 1,
        }
        if expected == predicted && elapsed_ms <= ROUTING_BUDGET_MS {
            self.correct_within_budget += 1;
        }
        if expected != predicted && self.misses.len() < 16 && !self.misses.contains(&index) {
            self.misses.push(index);
        }
    }

    fn print(&self, policy: &str, completed: usize, requested: usize) {
        println!("opportunity policy={policy} positive=early_lookup tp={} tn={} unnecessary_early_lookup_fp={} missed_early_lookup_fn={}", self.tp, self.tn, self.fp, self.fn_);
        println!("opportunity policy={policy} accuracy_completed={} precision_early={} recall_early={} correct_within_250ms_requested={} correct_within_250ms_count={}/{} mismatch_case_indices_0based_max16={:?}",
            percent(self.tp + self.tn, completed), percent(self.tp, self.tp + self.fp),
            percent(self.tp, self.tp + self.fn_), percent(self.correct_within_budget, requested),
            self.correct_within_budget, requested, self.misses);
    }
}

fn percent(numerator: usize, denominator: usize) -> String {
    if denominator == 0 {
        "N/A".into()
    } else {
        format!("{:.2}%", 100.0 * numerator as f64 / denominator as f64)
    }
}

fn opportunity_predictions(route: Route2D) -> [bool; 2] {
    [
        route == Route2D::SearchFast,
        matches!(route, Route2D::SearchFast | Route2D::ResearchSmart),
    ]
}

impl Report {
    fn complete(&self) -> bool {
        self.requested > 0 && self.completed == self.requested && self.errors == 0
    }
    fn print(&self, reason: &str) {
        println!(
            "coverage requested={} attempted={} completed={} skipped={} errors={} outcome={reason}",
            self.requested,
            self.attempted,
            self.completed,
            self.requested - self.attempted,
            self.errors
        );
        if self.completed > 0 && self.task == Task::Routes {
            let pct = |n| 100.0 * n as f64 / self.completed as f64;
            println!("quality denominator=completed exact_labels={:.2}% top2_coverage={:.2}% evidence_axis_accuracy={:.2}%", pct(self.exact), pct(self.top2), pct(self.evidence));
            println!(
                "confusion rows=expected columns=predicted order=F,S,C,R {:?}",
                self.confusion
            );
            println!("mismatch_sample case_index_0based,expected_index,predicted_index order=F,S,C,R max=16 {:?}", self.mismatches);
        } else if self.completed == 0 {
            println!("quality=unavailable latency=unavailable");
        }
        if self.task == Task::EarlyLookup {
            for (score, policy) in self.opportunities.iter().zip(["S-only-v1", "S-or-R-v1"]) {
                score.print(policy, self.completed, self.requested);
            }
            println!("opportunity_scope=annotation_agreement_not_measured_final_answer_benefit; model_led_does_not_deny_web; policies_are_diagnostic_not_production");
        }
        if let Some((reason, index)) = self.failure {
            println!("failure={reason} case_index_0based={index}");
        }
        if let Some(first) = self.times.first() {
            println!("first_inference_ms={first:.3}");
            let mut sorted = self.times.clone();
            sorted.sort_by(f64::total_cmp);
            let percentile = |p: usize| sorted[(sorted.len() * p).div_ceil(100).saturating_sub(1)];
            println!(
                "observed_completed count={} p50_ms={:.3} p95_ms={:.3} max_ms={:.3} over_250ms={}",
                sorted.len(),
                percentile(50),
                percentile(95),
                sorted[sorted.len() - 1],
                sorted.iter().filter(|ms| **ms > ROUTING_BUDGET_MS).count()
            );
        }
        if self.times.len() > 1 {
            let warm = &self.times[1..];
            println!(
                "warm count={} mean_ms={:.3} min_ms={:.3} max_ms={:.3}",
                warm.len(),
                warm.iter().sum::<f64>() / warm.len() as f64,
                warm.iter().copied().fold(f64::INFINITY, f64::min),
                warm.iter().copied().fold(0.0, f64::max)
            );
        }
        println!("coverage_completed={} semantic_quality_passed=unassessed; top2 coverage is not a safety guarantee", self.complete());
    }
}

fn suite(
    cases: &[(usize, Case)],
    repetitions: usize,
    input: Input,
    task: Task,
    mut embed: impl FnMut(&str) -> Result<Vec<f32>, &'static str>,
) -> Report {
    let mut report = Report {
        task,
        requested: cases.len() * repetitions,
        ..Default::default()
    };
    for _ in 0..repetitions {
        for (index, (expected, case)) in cases.iter().enumerate() {
            report.attempted += 1;
            let start = Instant::now();
            let text = input.text(case);
            let vector = match embed(&text) {
                Ok(vector) => vector,
                Err(reason) => {
                    report.errors += 1;
                    report.failure = Some((reason, index));
                    return report;
                }
            };
            let decision = match classify_bge_small_embedding(&vector, String::new()) {
                Some(decision) => decision,
                None => {
                    report.errors += 1;
                    report.failure = Some(("invalid_embedding", index));
                    return report;
                }
            };
            let elapsed_ms = start.elapsed().as_secs_f64() * 1000.0;
            report.times.push(elapsed_ms);
            let predicted = Route2D::ALL
                .iter()
                .position(|r| *r == decision.primary)
                .expect("core route");
            report.completed += 1;
            if task == Task::EarlyLookup {
                for (score, early) in report
                    .opportunities
                    .iter_mut()
                    .zip(opportunity_predictions(decision.primary))
                {
                    score.observe(*expected == 1, early, elapsed_ms, index);
                }
                continue;
            }
            report.confusion[*expected][predicted] += 1;
            report.exact += usize::from(*expected == predicted);
            let mismatch = (index, *expected, predicted);
            if *expected != predicted
                && report.mismatches.len() < 16
                && !report.mismatches.contains(&mismatch)
            {
                report.mismatches.push(mismatch);
            }
            report.top2 += usize::from(
                Route2D::ALL[*expected] == decision.primary
                    || Route2D::ALL[*expected] == decision.secondary,
            );
            report.evidence +=
                usize::from(matches!(*expected, 1 | 3) == matches!(predicted, 1 | 3));
        }
    }
    report
}

fn embedding_failure(error: LlamaCppRuntimeError) -> &'static str {
    match error {
        LlamaCppRuntimeError::HttpFailed { details } if details.starts_with("status 400:") => {
            "embedding_http_400"
        }
        LlamaCppRuntimeError::HttpFailed { .. } => "embedding_http_failed",
        LlamaCppRuntimeError::InvalidResponsePayload { .. } => "embedding_payload_invalid",
        _ => "embedding_request_failed",
    }
}

fn artifact_paths(server: &Path, model: &Path) -> Result<(PathBuf, PathBuf), &'static str> {
    let server = server
        .canonicalize()
        .map_err(|_| "SKIP_artifacts_unavailable")?;
    let model = model
        .canonicalize()
        .map_err(|_| "SKIP_artifacts_unavailable")?;
    if !server.is_file() || !model.is_file() {
        return Err("SKIP_artifacts_unavailable");
    }
    Ok((server, model))
}

#[derive(Serialize)]
struct FeatureCache {
    schema: &'static str,
    input_version: &'static str,
    embedding_dim: usize,
    records: Vec<FeatureRecord>,
}

#[derive(Serialize)]
struct FeatureRecord {
    index: usize,
    id: String,
    family: String,
    label: usize,
    embedding: Vec<f32>,
}

fn feature_destination(path: &Path) -> Result<PathBuf, &'static str> {
    if !path.is_absolute() {
        return Err("feature_output_requires_absolute_path");
    }
    let parent = path
        .parent()
        .and_then(|p| p.canonicalize().ok())
        .ok_or("feature_output_parent_unavailable")?;
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()
        .map_err(|_| "repository_boundary_unavailable")?;
    if parent.starts_with(repository)
        || parent
            .ancestors()
            .any(|ancestor| ancestor.join(".git").symlink_metadata().is_ok())
    {
        return Err("feature_output_inside_repository");
    }
    let destination = parent.join(path.file_name().ok_or("feature_output_invalid")?);
    if destination.symlink_metadata().is_ok() {
        return Err("feature_output_exists");
    }
    Ok(destination)
}

fn feature_cases_valid(cases: &[(usize, Case)]) -> bool {
    let valid = |s: &str| {
        !s.is_empty()
            && s.len() <= 64
            && s.as_bytes()[0].is_ascii_alphanumeric()
            && s.bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
    };
    let mut ids = std::collections::BTreeSet::new();
    cases.iter().all(|(label, case)| {
        *label <= 1
            && match (&case._id, &case._family) {
                (Some(id), Some(family)) => valid(id) && valid(family) && ids.insert(id.as_str()),
                _ => false,
            }
    })
}

fn collect_features(
    cases: &[(usize, Case)],
    input: Input,
    mut embed: impl FnMut(&str) -> Result<Vec<f32>, &'static str>,
) -> Result<FeatureCache, (&'static str, usize)> {
    let mut records = Vec::with_capacity(cases.len());
    for (index, (label, case)) in cases.iter().enumerate() {
        let vector = embed(&input.text(case)).map_err(|reason| (reason, index))?;
        // Reuse production validation; no old-head prediction metrics are exposed during extraction.
        if classify_bge_small_embedding(&vector, String::new()).is_none() {
            return Err(("invalid_embedding", index));
        }
        records.push(FeatureRecord {
            index,
            id: case._id.clone().ok_or(("feature_id_missing", index))?,
            family: case
                ._family
                .clone()
                .ok_or(("feature_family_missing", index))?,
            label: *label,
            embedding: vector,
        });
    }
    Ok(FeatureCache {
        schema: "retrieval_features_v1",
        input_version: input.version(),
        embedding_dim: 384,
        records,
    })
}

fn write_features(path: &Path, cache: &FeatureCache) -> Result<(), &'static str> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .map_err(|_| "feature_output_unavailable")?;
    serde_json::to_writer(&mut file, cache).map_err(|_| "feature_output_write_failed")?;
    file.flush()
        .and_then(|()| file.sync_all())
        .map_err(|_| "feature_output_write_failed")
}

pub(super) fn run(args: &BenchArgs) -> bool {
    let input = args.encoder_input.unwrap_or_default();
    let task = args.encoder_task.unwrap_or_default();
    println!("encoder task={task:?} input_version={} scope=input_preparation+local_HTTP+encoder+head CPU; benchmark-only unchanged head; new representation validation=unassessed", input.version());
    println!("Timing uses one monotonic observer; startup includes observed health readiness, not app readiness. Existing startup/embed timeouts are finite, not production cancellation guarantees. Not device pixels, provider or model-answer latency.");
    println!("Input preparation is formatting the supplied prompt/prior context. Fixture parsing, canonical history projection, queue/IPC and startup are outside inference samples; 250ms scoring is a host-component diagnostic, not phone routing acceptance or final-answer latency.");
    let prepared = (|| {
        let file = std::fs::File::open(&args.cases_path).map_err(|_| "fixture_unavailable")?;
        let mut raw = String::new();
        file.take(MAX_FIXTURE + 1)
            .read_to_string(&mut raw)
            .map_err(|_| "fixture_unreadable")?;
        if raw.len() as u64 > MAX_FIXTURE {
            return Err("fixture_size_limit");
        }
        cases(&raw, input, task)
    })();
    let cases = match prepared {
        Ok(c) => c,
        Err(reason) => {
            println!("coverage requested=unknown attempted=0 completed=0 skipped=unknown errors=1 outcome={reason}; latency=unavailable");
            return false;
        }
    };
    let export = match args.encoder_features.as_deref() {
        Some(path) => {
            if !feature_cases_valid(&cases) {
                println!(
                    "feature_export_failed=invalid_case_metadata; predictions_not_reported=true"
                );
                return false;
            }
            match feature_destination(path) {
                Ok(path) => Some(path),
                Err(reason) => {
                    println!("feature_export_failed={reason}; predictions_not_reported=true");
                    return false;
                }
            }
        }
        None => None,
    };
    let unavailable = |reason| {
        Report {
            task,
            requested: cases.len() * args.repetitions,
            ..Default::default()
        }
        .print(reason);
        false
    };
    let (Some(server), Some(model)) = (&args.encoder_server, &args.encoder_model) else {
        return unavailable("SKIP_explicit_artifacts_required");
    };
    let (server, model) = match artifact_paths(server, model) {
        Ok(paths) => paths,
        Err(reason) => return unavailable(reason),
    };
    let port = match TcpListener::bind(("127.0.0.1", 0)).and_then(|socket| socket.local_addr()) {
        Ok(addr) => addr.port(),
        Err(_) => return unavailable("SKIP_loopback_port_unavailable"),
    };
    let start = Instant::now();
    let runtime = match LlamaCppRuntime::start_embedding_with_policy_cancellation(
        LlamaCppServerSpec::new(server, model, "127.0.0.1", port, "benchmark-encoder"),
        InferencePolicy::Cpu,
        LlamaCppStartupCancellation::default(),
    ) {
        Ok(runtime) => runtime,
        Err(_) => return unavailable("SKIP_owned_server_startup_failed"),
    };
    // The runtime API probes before spawning. Reject a port-allocation race; never
    // send fixture content to an attached external process.
    if !runtime.is_owned() {
        return unavailable("SKIP_external_attach_rejected");
    }
    println!(
        "startup_observed_readiness_ms={:.3}",
        start.elapsed().as_secs_f64() * 1000.0
    );
    let client = runtime.client();
    if let Some(path) = export {
        let cache = match collect_features(&cases, input, |text| {
            client.embed(text).map_err(embedding_failure)
        }) {
            Ok(cache) => cache,
            Err((reason, index)) => {
                println!("feature_export_failed={reason} case_index_0based={index} completed={index} requested={}; predictions_not_reported=true", cases.len());
                return false;
            }
        };
        return match write_features(&path, &cache) {
            Ok(()) => {
                println!("feature_export_completed={} requested={} predictions_not_reported=true; vectors_written_only_to_explicit_private_artifact", cache.records.len(), cases.len());
                true
            }
            Err(reason) => {
                println!("feature_export_failed={reason}; predictions_not_reported=true");
                false
            }
        };
    }
    let report = suite(&cases, args.repetitions, input, task, |text| {
        client.embed(text).map_err(embedding_failure)
    });
    report.print(if report.complete() {
        "coverage_completed"
    } else {
        "embedding_or_head_failed"
    });
    report.complete()
}

#[cfg(test)]
mod tests {
    use super::*;
    fn synthetic(text: &str) -> Case {
        Case {
            exp: Some("F".into()),
            expected: None,
            prompt: text.into(),
            prev: None,
            _py_norm: None,
            _id: None,
            _family: None,
        }
    }

    #[test]
    fn feature_cache_is_explicit_bounded_and_does_not_serialize_prompt_text() {
        let mut case = synthetic("SYNTHETIC_PROMPT_NOT_FOR_EXPORT");
        case._id = Some("synthetic-case".into());
        case._family = Some("synthetic-family".into());
        let cases = vec![(1, case)];
        assert!(feature_cases_valid(&cases));
        let cache = collect_features(&cases, Input::Literal, |_| Ok(vec![1.0; 384]))
            .unwrap_or_else(|_| panic!("synthetic cache"));
        let value = serde_json::to_value(cache).unwrap();
        assert_eq!(value["schema"], "retrieval_features_v1");
        assert_eq!(value["records"][0]["label"], 1);
        assert_eq!(
            value["records"][0]["embedding"].as_array().unwrap().len(),
            384
        );
        assert!(!value
            .to_string()
            .contains("SYNTHETIC_PROMPT_NOT_FOR_EXPORT"));
        assert!(matches!(
            collect_features(&cases, Input::Literal, |_| Ok(vec![f32::NAN; 384])),
            Err(("invalid_embedding", 0))
        ));
        assert!(!feature_cases_valid(&[(1, synthetic("metadata missing"))]));
        for id in ["_invalid", "-invalid"] {
            let mut case = synthetic("synthetic");
            case._id = Some(id.into());
            case._family = Some("valid-family".into());
            assert!(!feature_cases_valid(&[(1, case)]));
        }
    }

    #[test]
    fn feature_export_refuses_repository_destinations_and_existing_files() {
        use std::sync::atomic::{AtomicU64, Ordering};
        static ID: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "synthetic-router-feature-{}-{}",
            std::process::id(),
            ID.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&root).unwrap();
        struct Fixture(PathBuf);
        impl Drop for Fixture {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let fixture = Fixture(root);
        assert!(feature_destination(Path::new("relative.json")).is_err());
        assert!(feature_destination(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("synthetic-feature.json")
        )
        .is_err());
        let path = feature_destination(&fixture.0.join("features.json")).unwrap();
        let cache = FeatureCache {
            schema: "retrieval_features_v1",
            input_version: Input::Literal.version(),
            embedding_dim: 384,
            records: vec![],
        };
        write_features(&path, &cache).unwrap();
        let original = std::fs::read(&path).unwrap();
        assert!(write_features(&path, &cache).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), original);
        assert!(feature_destination(&path).is_err());
        for directory_marker in [true, false] {
            let other = fixture.0.join(if directory_marker {
                "external-repo"
            } else {
                "external-worktree"
            });
            std::fs::create_dir(&other).unwrap();
            if directory_marker {
                std::fs::create_dir(other.join(".git")).unwrap();
            } else {
                std::fs::write(other.join(".git"), b"synthetic worktree marker").unwrap();
            }
            assert!(feature_destination(&other.join("features.json")).is_err());
        }
    }

    #[test]
    fn opportunity_oracles_are_distinct_from_historical_routes() {
        let raw = r#"[{"expected":"early_lookup","prompt":"synthetic current lookup"},{"expected":"model_led","prompt":"synthetic reasoning"}]"#;
        let parsed = cases(raw, Input::Literal, Task::EarlyLookup).unwrap();
        assert_eq!((parsed[0].0, parsed[1].0), (1, 0));
        assert!(cases(raw, Input::Literal, Task::Routes).is_err());
        assert!(cases(
            r#"[{"exp":"S","prompt":"synthetic lookup"}]"#,
            Input::Literal,
            Task::EarlyLookup
        )
        .is_err());
        assert!(cases(
            r#"[{"exp":"S","expected":"early_lookup","prompt":"synthetic lookup"}]"#,
            Input::Literal,
            Task::EarlyLookup
        )
        .is_err());
        assert!(cases(
            r#"[{"expected":"unsupported_label","prompt":"synthetic lookup"}]"#,
            Input::Literal,
            Task::EarlyLookup
        )
        .is_err());
    }

    #[test]
    fn opportunity_policies_are_fixed_and_keep_research_model_led_in_s_only() {
        assert_eq!(opportunity_predictions(Route2D::DirectFast), [false, false]);
        assert_eq!(
            opportunity_predictions(Route2D::DirectSmart),
            [false, false]
        );
        assert_eq!(opportunity_predictions(Route2D::SearchFast), [true, true]);
        assert_eq!(
            opportunity_predictions(Route2D::ResearchSmart),
            [false, true]
        );
    }

    #[test]
    fn opportunity_score_counts_false_lookups_misses_and_late_correct_decisions() {
        let mut score = OpportunityScore::default();
        score.observe(true, true, 20.0, 0);
        score.observe(false, true, 20.0, 1);
        score.observe(true, false, 20.0, 2);
        score.observe(false, false, 300.0, 3);
        assert_eq!((score.tp, score.tn, score.fp, score.fn_), (1, 1, 1, 1));
        assert_eq!(score.correct_within_budget, 1);
        assert_eq!(score.misses, vec![1, 2]);
        assert_eq!(percent(score.correct_within_budget, 4), "25.00%");
        assert_eq!(percent(0, 0), "N/A");
    }

    #[test]
    fn opportunity_fixture_is_consumed_by_the_real_parser() {
        let raw = include_str!(
            "../../../../../crates/core/tests/fixtures/retrieval_opportunities_v1.json"
        );
        for input in [Input::Legacy, Input::Literal] {
            let parsed = cases(raw, input, Task::EarlyLookup).unwrap();
            assert_eq!(parsed.len(), 60);
            assert_eq!(
                parsed.iter().filter(|(expected, _)| *expected == 1).count(),
                30
            );
            let ids: std::collections::BTreeSet<_> = parsed
                .iter()
                .map(|(_, case)| case._id.as_deref().unwrap())
                .collect();
            assert_eq!(ids.len(), 60);
        }
        assert!(cases(raw, Input::Literal, Task::Routes).is_err());
    }

    #[test]
    fn opportunity_failures_remain_in_requested_fast_correct_denominator() {
        let vector = vec![1.0; 384];
        let route = classify_bge_small_embedding(&vector, String::new())
            .unwrap()
            .primary;
        let expected = usize::from(opportunity_predictions(route)[0]);
        let mut calls = 0;
        let report = suite(
            &[(expected, synthetic("synthetic"))],
            3,
            Input::Literal,
            Task::EarlyLookup,
            |_| {
                calls += 1;
                if calls == 1 {
                    Ok(vector.clone())
                } else {
                    Err("synthetic_transport_failure")
                }
            },
        );
        assert_eq!(
            (report.requested, report.completed, report.errors),
            (3, 1, 1)
        );
        assert_eq!(report.opportunities[0].tp + report.opportunities[0].tn, 1);
        assert_eq!(percent(1, report.requested), "33.33%");
        assert!(!report.complete());
    }

    #[test]
    fn failure_metadata_is_categorical_and_keeps_the_case_index() {
        let reason = embedding_failure(LlamaCppRuntimeError::HttpFailed {
            details: "status 400: synthetic error body must not be logged".into(),
        });
        assert_eq!(reason, "embedding_http_400");
        let mut calls = 0;
        let report = suite(
            &[
                (0, synthetic("synthetic one")),
                (0, synthetic("synthetic two")),
            ],
            2,
            Input::Literal,
            Task::Routes,
            |_| {
                calls += 1;
                if calls == 1 {
                    Ok(vec![1.0; 384])
                } else {
                    Err(reason)
                }
            },
        );
        assert_eq!(report.failure, Some(("embedding_http_400", 1)));
        assert_eq!(
            (
                report.requested,
                report.attempted,
                report.completed,
                report.errors
            ),
            (4, 2, 1, 1)
        );
        assert!(!report.complete());
    }
    #[test]
    fn relative_artifacts_are_resolved_before_server_working_directory_changes() {
        use std::sync::atomic::{AtomicU64, Ordering};
        static ID: AtomicU64 = AtomicU64::new(0);
        let root = PathBuf::from("target").join(format!(
            "synthetic-encoder-paths-{}-{}",
            std::process::id(),
            ID.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all("target").unwrap();
        std::fs::create_dir(&root).unwrap();
        struct Fixture(PathBuf);
        impl Drop for Fixture {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let fixture = Fixture(root);
        std::fs::create_dir_all(fixture.0.join("tools/models")).unwrap();
        std::fs::create_dir(fixture.0.join("models")).unwrap();
        let server = fixture.0.join("tools/synthetic-server");
        let model = fixture.0.join("models/synthetic-bge.gguf");
        let competing_model = fixture.0.join("tools/models/synthetic-bge.gguf");
        std::fs::write(&server, b"synthetic executable; never launched").unwrap();
        std::fs::write(&model, b"intended synthetic model").unwrap();
        std::fs::write(&competing_model, b"wrong synthetic model").unwrap();
        let resolved = artifact_paths(&server, &model).unwrap();
        assert_eq!(resolved.0, server.canonicalize().unwrap());
        assert_eq!(resolved.1, model.canonicalize().unwrap());
        assert_ne!(resolved.1, competing_model.canonicalize().unwrap());
    }

    #[test]
    fn missing_artifacts_are_incomplete_without_launching() {
        let args = crate::parse_args_from(
            [
                "--mode",
                "encoder",
                "--encoder-server",
                "/synthetic-unavailable/server",
                "--encoder-model",
                "/synthetic-unavailable/model",
            ]
            .map(str::to_owned),
        )
        .unwrap();
        assert!(!run(&args));
    }
    #[test]
    fn fixture_and_input_contract() {
        for raw in [
            "broken",
            "[]",
            r#"[{"exp":"X","prompt":"synthetic"}]"#,
            r#"[{"exp":"F","prompt":" "}]"#,
        ] {
            assert!(cases(raw, Input::Literal, Task::Routes).is_err());
        }
        let raw =
            r#"[{"exp":"F","prompt":" synthetic unchanged ","id":"synthetic-1","family":"test"}]"#;
        assert_eq!(
            Input::Literal.text(&cases(raw, Input::Literal, Task::Routes).unwrap()[0].1),
            " synthetic unchanged "
        );
    }
    #[test]
    fn invalid_embedding_never_falls_back() {
        for vector in [vec![], vec![0.0; 384], vec![f32::NAN; 384], vec![1.0; 3]] {
            let report = suite(
                &[(0, synthetic("synthetic"))],
                3,
                Input::Literal,
                Task::Routes,
                |_| Ok(vector.clone()),
            );
            assert_eq!(
                (
                    report.requested,
                    report.attempted,
                    report.completed,
                    report.errors
                ),
                (3, 1, 0, 1)
            );
            assert!(!report.complete());
            assert_eq!(report.failure, Some(("invalid_embedding", 0)));
        }
        assert!(!suite(
            &[(0, synthetic("synthetic"))],
            1,
            Input::Literal,
            Task::Routes,
            |_| Err("malformed_transport")
        )
        .complete());
    }
    #[test]
    fn one_embedding_boundary_reused_across_rounds_quality_is_not_coverage() {
        let vector = vec![1.0; 384];
        let primary = classify_bge_small_embedding(&vector, String::new())
            .unwrap()
            .primary;
        let wrong = Route2D::ALL.iter().position(|r| *r != primary).unwrap();
        let mut calls = 0;
        let report = suite(
            &[(wrong, synthetic("synthetic"))],
            3,
            Input::Literal,
            Task::Routes,
            |_| {
                calls += 1;
                Ok(vector.clone())
            },
        );
        assert_eq!(calls, 3);
        assert!(report.complete());
        assert_eq!(report.exact, 0);
        assert_eq!(
            report.mismatches,
            vec![(
                0,
                wrong,
                Route2D::ALL.iter().position(|r| *r == primary).unwrap()
            )]
        );
        assert_eq!(report.confusion.iter().flatten().sum::<usize>(), 3);
    }
}
