//! Explicitly compiled, isolated software-input benchmark. Never a shipping IPC surface.
use super::*;
use std::collections::{BTreeMap, HashSet};
use std::sync::{LazyLock, OnceLock};
const CAPABILITY: &str = "voxgolem:app-bench:synthetic-input-only:v1";
#[cfg(not(test))]
use voxgolem_platform::search::RunEvent;

#[derive(Clone, Serialize)]
pub(crate) struct Observation {
    stage: &'static str,
    flow: u64,
    round: u64,
    elapsed_us: u64,
}

#[derive(Default)]
struct Trace {
    flow: u64,
    round: u64,
    events: Vec<Observation>,
    once: HashSet<&'static str>,
    dropped: u64,
    readable_pages: HashSet<String>,
    tool_failures: u64,
    frame_emissions: u64,
    last_emission: Option<Instant>,
    max_emission_gap_us: u64,
    max_emit_call_us: u64,
    burst_emissions: u64,
    commands: BTreeMap<&'static str, CommandStats>,
}

#[derive(Clone, Default, Serialize)]
struct CommandStats {
    calls: u64,
    total_us: u64,
    max_us: u64,
}

pub(crate) struct CommandObservation {
    name: &'static str,
    started: Instant,
}

// Handler dispatch time includes synchronous bodies/serialization; asynchronous
// bodies continue separately and are not measured by this receipt.
pub(crate) fn command(name: &str) -> CommandObservation {
    let name = match name {
        "ingest_audio_frame" => "ingest_audio_frame",
        "mark_silence" => "mark_silence",
        "record_speech_activity" => "record_speech_activity",
        "clear_completion" => "clear_completion",
        "request_completion" => "request_completion",
        "prepare_voice_cues" => "prepare_voice_cues",
        "save_wake_clip" => "save_wake_clip",
        "get_startup_state" => "get_startup_state",
        "get_app_snapshot" => "get_app_snapshot",
        "get_assistant_settings" => "get_assistant_settings",
        "get_ui_theme" => "get_ui_theme",
        "get_ui_text_size" => "get_ui_text_size",
        "start_native_microphone" => "start_native_microphone",
        "stop_native_microphone" => "stop_native_microphone",
        "record_frontend_runtime_diagnostic" => "record_frontend_runtime_diagnostic",
        _ => "other",
    };
    CommandObservation {
        name,
        started: Instant::now(),
    }
}

impl Drop for CommandObservation {
    fn drop(&mut self) {
        let elapsed = self.started.elapsed().as_micros().min(u128::from(u64::MAX)) as u64;
        if let Ok(mut trace) = TRACE.lock() {
            let stats = trace.commands.entry(self.name).or_default();
            stats.calls += 1;
            stats.total_us = stats.total_us.saturating_add(elapsed);
            stats.max_us = stats.max_us.max(elapsed);
        }
    }
}

static ORIGIN: OnceLock<Instant> = OnceLock::new();
static TRACE: LazyLock<Mutex<Trace>> = LazyLock::new(|| Mutex::new(Trace::default()));
static ENTRY: OnceLock<&'static str> = OnceLock::new();

fn parse_entry(entry: Option<&str>, automatic: bool) -> Result<&'static str, String> {
    match (entry.unwrap_or("voice"), automatic) {
        ("voice", _) => Ok("voice"),
        ("typed", false) => Ok("typed"),
        ("typed", true) => Err("Typed entry cannot queue automatic capture diagnostics".into()),
        _ => Err("Benchmark entry must be voice or typed".into()),
    }
}

fn reset_flow(trace: &mut Trace) -> u64 {
    let flow = trace.flow.saturating_add(1);
    *trace = Trace {
        flow,
        ..Trace::default()
    };
    flow
}

#[tauri::command]
pub(crate) fn benchmark_begin_flow(app_state: tauri::State<'_, AppState>) -> Result<u64, String> {
    ensure_app_running(&app_state.exit_cleanup_started)?;
    // Serialize against admission without modifying canonical or control state.
    let _lifecycle = app_state
        .conversation
        .lifecycle_gate
        .lock()
        .map_err(|_| "Benchmark lifecycle unavailable")?;
    ensure_app_running(&app_state.exit_cleanup_started)?;
    let active = app_state
        .conversation
        .active_prompt
        .lock()
        .map_err(|_| "Benchmark prompt state unavailable")?;
    if active.is_some()
        || current_runtime_phase(&app_state.voice_pipeline_state)? != RuntimePhasePayload::Sleeping
    {
        return Err("Benchmark flow requires an idle sleeping application".into());
    }
    let mut trace = TRACE
        .lock()
        .map_err(|_| "Benchmark observations unavailable")?;
    Ok(reset_flow(&mut trace))
}

pub(crate) fn validate_environment() -> Result<(), String> {
    ORIGIN.get_or_init(Instant::now);
    let entry = match std::env::var("VOXGOLEM_BENCH_ENTRY") {
        Ok(value) => Some(value),
        Err(std::env::VarError::NotPresent) => None,
        Err(_) => return Err("Benchmark entry must be voice or typed".into()),
    };
    let entry = parse_entry(
        entry.as_deref(),
        std::env::var_os("VOXGOLEM_BENCH_AUTO_DIAGNOSTIC_CASE").is_some(),
    )?;
    let _ = ENTRY.set(entry);
    if !matches!(
        std::env::var("VOXGOLEM_BENCH_MODE").as_deref(),
        Ok("offline" | "live")
    ) {
        return Err("Benchmark mode must be explicit".into());
    }
    let root = root()?;
    if let Ok(case) = std::env::var("VOXGOLEM_BENCH_AUTO_DIAGNOSTIC_CASE") {
        if std::env::var("VOXGOLEM_BENCH_MODE").as_deref() != Ok("offline")
            || !matches!(case.as_str(), "quick" | "short_research" | "long_research")
        {
            return Err("Automatic source qualification is offline diagnostic only".into());
        }
    }
    voxgolem_platform::app_benchmark::initialize()
        .map_err(|_| "Benchmark provider mode or allowance invalid")?;
    for name in [
        "HOME",
        "XDG_CONFIG_HOME",
        "XDG_DATA_HOME",
        "XDG_STATE_HOME",
        "XDG_CACHE_HOME",
        "XDG_RUNTIME_DIR",
    ] {
        let path =
            PathBuf::from(std::env::var_os(name).ok_or("Benchmark storage is not isolated")?);
        let resolved = path
            .canonicalize()
            .map_err(|_| "Benchmark storage is unavailable")?;
        if !resolved.starts_with(&root) || resolved == root {
            return Err("Benchmark storage escapes the private run".into());
        }
    }
    let config =
        voxgolem_core::config::default_config_path().map_err(|_| "Benchmark config unavailable")?;
    if !config
        .canonicalize()
        .map_err(|_| "Benchmark config unavailable")?
        .starts_with(root)
    {
        return Err("Benchmark config is not isolated".into());
    }
    mark("process_setup");
    Ok(())
}

fn root() -> Result<PathBuf, String> {
    let path =
        PathBuf::from(std::env::var_os("VOXGOLEM_BENCH_ROOT").ok_or("Benchmark root missing")?);
    if !path.is_absolute() || path.is_symlink() {
        return Err("Benchmark root must be private and absolute".into());
    }
    let resolved = path
        .canonicalize()
        .map_err(|_| "Benchmark root unavailable")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if fs::metadata(&resolved)
            .map_err(|_| "Benchmark root unavailable")?
            .permissions()
            .mode()
            & 0o077
            != 0
        {
            return Err("Benchmark root permissions are not private".into());
        }
    }
    Ok(resolved)
}

pub(crate) fn capture() -> Arc<voxgolem_audio::capture::AudioCaptureService> {
    Arc::new(
        voxgolem_audio::capture::AudioCaptureService::new_benchmark(16_000, 1)
            .expect("fixed benchmark input format"),
    )
}

pub(crate) fn mark(stage: &'static str) {
    mark_at(stage, Instant::now());
}

fn mark_at(stage: &'static str, at: Instant) {
    let elapsed_us = at
        .saturating_duration_since(*ORIGIN.get_or_init(Instant::now))
        .as_micros()
        .min(u128::from(u64::MAX)) as u64;
    if let Ok(mut trace) = TRACE.lock() {
        if trace.events.len() >= 512 {
            trace.dropped += 1;
            return;
        }
        let event = Observation {
            stage,
            flow: trace.flow,
            round: trace.round,
            elapsed_us,
        };
        trace.events.push(event);
    }
}

pub(crate) fn mark_once(stage: &'static str) {
    let accepted = TRACE.lock().is_ok_and(|mut trace| trace.once.insert(stage));
    if accepted {
        mark(stage);
    }
}

pub(crate) fn emitted_frame(at: Instant, call_duration: Duration) {
    if let Ok(mut trace) = TRACE.lock() {
        if let Some(previous) = trace.last_emission {
            let gap = at
                .saturating_duration_since(previous)
                .as_micros()
                .min(u128::from(u64::MAX)) as u64;
            trace.max_emission_gap_us = trace.max_emission_gap_us.max(gap);
            if gap < 5000 {
                trace.burst_emissions += 1;
            }
        }
        trace.last_emission = Some(at);
        trace.frame_emissions += 1;
        trace.max_emit_call_us = trace
            .max_emit_call_us
            .max(call_duration.as_micros().min(u128::from(u64::MAX)) as u64);
    }
}

#[cfg(not(test))]
pub(crate) fn model_event(event: &RunEvent<'_>) {
    use voxgolem_platform::custom_openai::tools::ModelEvent;
    match event {
        RunEvent::Model(ModelEvent::RequestDispatched(_)) => {
            if let Ok(mut trace) = TRACE.lock() {
                trace.round += 1;
                trace.once.remove("first_model_text");
            }
            mark("generation_dispatched");
        }
        RunEvent::Model(ModelEvent::TextDelta { text, .. }) if !text.trim().is_empty() => {
            mark_once("first_model_text")
        }
        RunEvent::Model(ModelEvent::AuthRevision(_)) => mark("credential_checked"),
        RunEvent::ToolStarted { name, .. } => mark(if *name == "web_fetch" {
            "read_started"
        } else {
            "search_started"
        }),
        RunEvent::ToolCompleted { outcome, .. } => {
            if let Ok(mut trace) = TRACE.lock() {
                for source in &outcome.sources {
                    if source.kind == voxgolem_core::search::EvidenceKind::PageRead
                        && voxgolem_core::search::has_readable_text(&source.text)
                    {
                        trace.readable_pages.insert(source.url.clone());
                    }
                }
                if outcome.partial || outcome.evidence_unavailable {
                    trace.tool_failures += 1;
                }
            }
            mark("tool_completed");
        }
        RunEvent::ToolFailed { .. } => {
            if let Ok(mut trace) = TRACE.lock() {
                trace.tool_failures += 1;
            }
            mark("tool_failed");
        }
        _ => {}
    }
}

pub(crate) fn frontend_diagnostic(kind: FrontendRuntimeDiagnosticKind, detail: &str) {
    let stage = match detail {
        "benchmark_lifetime_retire" => "frontend_lifetime_retired",
        "benchmark_controls_retire" => "frontend_controls_retired",
        "benchmark_capability_retire" => "frontend_capability_retired",
        "benchmark_audio_error" => "frontend_audio_error",
        "benchmark_cue_error" => "frontend_cue_error",
        _ if matches!(kind, FrontendRuntimeDiagnosticKind::Audio)
            && detail.contains("fell behind") =>
        {
            "frontend_audio_backlog"
        }
        _ if matches!(kind, FrontendRuntimeDiagnosticKind::Audio)
            && detail.contains("Runtime control payload") =>
        {
            "frontend_audio_invalid_status"
        }
        _ if matches!(kind, FrontendRuntimeDiagnosticKind::Audio)
            && detail.starts_with("Live audio error:") =>
        {
            "frontend_audio_failed"
        }
        _ => return,
    };
    mark(stage);
}

#[tauri::command]
pub(crate) async fn benchmark_queue(
    fixture: String,
    app_state: tauri::State<'_, AppState>,
) -> Result<(), String> {
    queue_fixture(fixture, Arc::clone(&app_state.microphone_capture)).await
}

async fn queue_fixture(
    fixture: String,
    capture: Arc<voxgolem_audio::capture::AudioCaptureService>,
) -> Result<(), String> {
    if !matches!(
        fixture.as_str(),
        "quick" | "short_research" | "long_research"
    ) {
        return Err("Unknown benchmark fixture".into());
    }
    // Bind before filesystem work; a stop/restart must not redirect this clip to
    // the newer capture. The capture owner revalidates the same ID at admission.
    let id = capture
        .benchmark_capture_id()
        .map_err(|_| "Benchmark input unavailable")?
        .ok_or("Benchmark capture has not started")?;
    let root = root()?;
    let fixtures = root
        .join("fixtures")
        .canonicalize()
        .map_err(|_| "Benchmark fixtures missing")?;
    if !fixtures.starts_with(root) {
        return Err("Benchmark fixtures escape run storage".into());
    }
    let path = fixtures.join(format!("{fixture}.pcm"));
    if path.is_symlink()
        || path
            .canonicalize()
            .map_err(|_| "Benchmark fixture missing")?
            .parent()
            != Some(fixtures.as_path())
    {
        return Err("Benchmark fixture is not owned".into());
    }
    let samples = spawn_runtime_blocking(move || -> Result<Vec<f32>, String> {
        use std::io::Read;
        let file = File::open(path).map_err(|_| "Benchmark fixture unavailable")?;
        let mut bytes = Vec::new();
        file.take(3_840_001)
            .read_to_end(&mut bytes)
            .map_err(|_| "Benchmark fixture read failed")?;
        if bytes.is_empty() || bytes.len() > 3_840_000 || bytes.len() % 4 != 0 {
            return Err("Benchmark fixture size invalid".into());
        }
        let samples: Vec<f32> = bytes
            .chunks_exact(4)
            .map(|b| f32::from_le_bytes(b.try_into().expect("four bytes")))
            .collect();
        if samples.iter().any(|v| !v.is_finite() || v.abs() > 1.0) {
            return Err("Benchmark fixture samples invalid".into());
        }
        Ok(samples)
    })
    .await
    .map_err(|_| "Benchmark fixture worker failed")??;
    {
        let mut trace = TRACE
            .lock()
            .map_err(|_| "Benchmark observations unavailable")?;
        reset_flow(&mut trace);
    }
    let (completed, completion) = tokio::sync::oneshot::channel();
    let mut completed = Some(completed);
    capture
        .queue_benchmark_clip(
            id,
            samples,
            Box::new(move |boundary| {
                use voxgolem_audio::capture::BenchmarkAudioBoundary;
                match boundary {
                    BenchmarkAudioBoundary::Started(at) => mark_at("source_samples_started", at),
                    BenchmarkAudioBoundary::Completed(at) => {
                        mark_at("source_samples_completed", at);
                        if let Some(completed) = completed.take() {
                            let _ = completed.send(());
                        }
                    }
                }
            }),
        )
        .map_err(|_| "Benchmark clip was not admitted")?;
    // One completion receipt avoids repeatedly interrupting the WebView with
    // WebDriver evaluations while the real-time source is playing.
    tokio::time::timeout(Duration::from_secs(65), completion)
        .await
        .map_err(|_| "Benchmark source completion timed out")?
        .map_err(|_| "Benchmark source retired before completion")?;
    Ok(())
}

pub(crate) fn automatic_diagnostic(capture: Arc<voxgolem_audio::capture::AudioCaptureService>) {
    static STARTED: AtomicBool = AtomicBool::new(false);
    if let Ok(case) = std::env::var("VOXGOLEM_BENCH_AUTO_DIAGNOSTIC_CASE") {
        if !STARTED.swap(true, Ordering::AcqRel) {
            tauri::async_runtime::spawn(async move {
                if queue_fixture(case, capture).await.is_err() {
                    mark("automatic_source_retired");
                }
            });
        }
    }
}

pub(crate) fn write_automatic_diagnostic(app_state: &AppState) {
    if std::env::var_os("VOXGOLEM_BENCH_AUTO_DIAGNOSTIC_CASE").is_none() {
        return;
    }
    let write = || -> Result<(), String> {
        let mut snapshot = snapshot(app_state)?;
        if let Some(value) = snapshot.as_object_mut() {
            value.remove("process_id");
        }
        let bytes = serde_json::to_vec(&snapshot).map_err(|_| "Diagnostic encoding failed")?;
        if bytes.len() > 128 * 1024 {
            return Err("Diagnostic size bound exceeded".into());
        }
        let mut options = OpenOptions::new();
        options.create_new(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options
            .open(root()?.join("driver-free-diagnostic.json"))
            .map_err(|_| "Diagnostic file unavailable")?;
        file.write_all(&bytes)
            .and_then(|()| file.sync_all())
            .map_err(|_| "Diagnostic persistence failed")?;
        Ok(())
    };
    let _ = write();
}

#[tauri::command]
pub(crate) fn benchmark_snapshot(
    app_state: tauri::State<'_, AppState>,
) -> Result<serde_json::Value, String> {
    snapshot(&app_state)
}

fn snapshot(app_state: &AppState) -> Result<serde_json::Value, String> {
    // Never hold diagnostic state while acquiring a production lifecycle lock.
    let (flow, events, dropped, readable_pages, tool_failures, frame_metadata, commands) = {
        let trace = TRACE
            .lock()
            .map_err(|_| "Benchmark observations unavailable")?;
        (
            trace.flow,
            trace.events.clone(),
            trace.dropped,
            trace.readable_pages.len(),
            trace.tool_failures,
            serde_json::json!({"emitted_frames":trace.frame_emissions,
                "max_emission_gap_us":trace.max_emission_gap_us,"max_emit_call_us":trace.max_emit_call_us,
                "burst_emissions_under_5ms":trace.burst_emissions}),
            trace.commands.clone(),
        )
    };
    Ok(serde_json::json!({
        "schema": 1,
        "flow": flow,
        "entry": ENTRY.get().ok_or("Benchmark environment not validated")?,
        "capability": CAPABILITY,
        "process_id": std::process::id(),
        "capture_active": app_state.microphone_capture.is_capturing().unwrap_or(false),
        "phase": current_runtime_phase(&app_state.voice_pipeline_state)?,
        "events": events,
        "dropped": dropped,
        "software_input": true,
        "readable_pages": readable_pages,
        "tool_failures": tool_failures,
        "native_frame_metadata": frame_metadata,
        "handler_call_metadata": commands,
        "generation": voxgolem_platform::app_benchmark::get_counts(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entry_is_explicit_and_automatic_capture_is_voice_only() {
        for (entry, automatic, expected) in [
            (None, false, Some("voice")),
            (None, true, Some("voice")),
            (Some("voice"), true, Some("voice")),
            (Some("typed"), false, Some("typed")),
            (Some("typed"), true, None),
            (Some(""), false, None),
            (Some("other"), false, None),
        ] {
            assert_eq!(parse_entry(entry, automatic).ok(), expected);
        }
    }

    #[test]
    fn flow_reset_retires_all_observations_without_provider_initialization() {
        let mut trace = Trace {
            flow: 7,
            round: 3,
            dropped: 4,
            tool_failures: 2,
            frame_emissions: 9,
            last_emission: Some(Instant::now()),
            max_emission_gap_us: 10,
            max_emit_call_us: 11,
            burst_emissions: 8,
            ..Trace::default()
        };
        trace.events.push(Observation {
            stage: "synthetic",
            flow: 7,
            round: 3,
            elapsed_us: 1,
        });
        trace.once.insert("synthetic");
        trace
            .readable_pages
            .insert("https://example.invalid/synthetic".into());
        trace.commands.insert("other", CommandStats::default());
        // Only a local diagnostic Trace is accepted: provider allowances are not
        // part of this reset and no global initialization is invoked.
        assert_eq!(reset_flow(&mut trace), 8);
        assert_eq!(trace.round, 0);
        assert!(trace.events.is_empty() && trace.once.is_empty());
        assert!(trace.readable_pages.is_empty() && trace.commands.is_empty());
        assert_eq!(
            (trace.dropped, trace.tool_failures, trace.frame_emissions),
            (0, 0, 0)
        );
        assert!(trace.last_emission.is_none());
        assert_eq!(
            (
                trace.max_emission_gap_us,
                trace.max_emit_call_us,
                trace.burst_emissions
            ),
            (0, 0, 0)
        );
        assert_eq!(reset_flow(&mut trace), 9);
    }
}
