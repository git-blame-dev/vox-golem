//! Explicitly compiled, isolated software-input benchmark. Never a shipping IPC surface.
use super::*;
use std::collections::{BTreeMap, HashSet};
use std::sync::atomic::AtomicU64;
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
    native_baseline: NativeAudioCounts,
}

#[derive(Clone, Copy, Default, Serialize)]
struct NativeAudioCounts {
    admitted_frames: u64,
    admitted_samples: u64,
    processed_frames: u64,
    overflows: u64,
    stalls: u64,
    capture_failures: u64,
    processing_failures: u64,
}

#[derive(Default)]
struct NativeAudioCounters {
    admitted_frames: AtomicU64,
    admitted_samples: AtomicU64,
    processed_frames: AtomicU64,
    overflows: AtomicU64,
    stalls: AtomicU64,
    capture_failures: AtomicU64,
    processing_failures: AtomicU64,
}

impl NativeAudioCounters {
    fn snapshot(&self) -> NativeAudioCounts {
        NativeAudioCounts {
            admitted_frames: self.admitted_frames.load(Ordering::Relaxed),
            admitted_samples: self.admitted_samples.load(Ordering::Relaxed),
            processed_frames: self.processed_frames.load(Ordering::Relaxed),
            overflows: self.overflows.load(Ordering::Relaxed),
            stalls: self.stalls.load(Ordering::Relaxed),
            capture_failures: self.capture_failures.load(Ordering::Relaxed),
            processing_failures: self.processing_failures.load(Ordering::Relaxed),
        }
    }
}

static NATIVE_AUDIO: NativeAudioCounters = NativeAudioCounters {
    admitted_frames: AtomicU64::new(0),
    admitted_samples: AtomicU64::new(0),
    processed_frames: AtomicU64::new(0),
    overflows: AtomicU64::new(0),
    stalls: AtomicU64::new(0),
    capture_failures: AtomicU64::new(0),
    processing_failures: AtomicU64::new(0),
};

/// Benchmark-only callback instrumentation: no trace mutex, allocation or events
/// on frame admission. Process-lifetime totals are differenced at flow reset.
pub(crate) fn native_frame_admitted(sample_count: usize) {
    NATIVE_AUDIO.admitted_frames.fetch_add(1, Ordering::Relaxed);
    NATIVE_AUDIO
        .admitted_samples
        .fetch_add(sample_count as u64, Ordering::Relaxed);
}

pub(crate) fn native_frame_processed() {
    NATIVE_AUDIO
        .processed_frames
        .fetch_add(1, Ordering::Relaxed);
}

/// Fixed categorical outcomes only. Unknown categories fail qualification too.
pub(crate) fn native_voice_failure(category: &'static str) {
    let counter = match category {
        "overflow" => &NATIVE_AUDIO.overflows,
        "stall" => &NATIVE_AUDIO.stalls,
        "capture" => &NATIVE_AUDIO.capture_failures,
        _ => &NATIVE_AUDIO.processing_failures,
    };
    counter.fetch_add(1, Ordering::Relaxed);
}

fn native_audio_metadata(trace: &Trace, totals: NativeAudioCounts) -> serde_json::Value {
    let baseline = trace.native_baseline;
    serde_json::json!({
        "schema": 1,
        "admitted_frames": totals.admitted_frames.saturating_sub(baseline.admitted_frames),
        "admitted_samples": totals.admitted_samples.saturating_sub(baseline.admitted_samples),
        "processed_frames": totals.processed_frames.saturating_sub(baseline.processed_frames),
        "pcm_events_emitted": trace.frame_emissions,
        "overflows": totals.overflows.saturating_sub(baseline.overflows),
        "stalls": totals.stalls.saturating_sub(baseline.stalls),
        "capture_failures": totals.capture_failures.saturating_sub(baseline.capture_failures),
        "processing_failures": totals.processing_failures.saturating_sub(baseline.processing_failures),
    })
}

fn command_metadata(trace: &Trace) -> BTreeMap<&'static str, CommandStats> {
    let mut commands = trace.commands.clone();
    // Explicit zero receipts are required by qualification; missing metadata is
    // never interpreted as proof that a forbidden production handler was absent.
    for name in [
        "ingest_audio_frame",
        "mark_silence",
        "record_speech_activity",
        "save_wake_clip",
    ] {
        commands.entry(name).or_default();
    }
    commands
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
        "read_desktop_runtime" => "read_desktop_runtime",
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
        ("voice-arithmetic", false) => Ok("voice-arithmetic"),
        ("voice-arithmetic-windows", false) => Ok("voice-arithmetic-windows"),
        ("typed", false) => Ok("typed"),
        ("typed", true) => Err("Typed entry cannot queue automatic capture diagnostics".into()),
        _ => Err("Benchmark entry must be voice or typed".into()),
    }
}

fn reset_flow(trace: &mut Trace) -> u64 {
    let flow = trace.flow.saturating_add(1);
    *trace = Trace {
        flow,
        native_baseline: NATIVE_AUDIO.snapshot(),
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
    if cfg!(target_os = "windows") && entry != "voice-arithmetic-windows" {
        return Err("Windows benchmark supports only the offline arithmetic diagnostic".into());
    }
    if matches!(entry, "voice-arithmetic" | "voice-arithmetic-windows")
        && std::env::var("VOXGOLEM_BENCH_MODE").as_deref() != Ok("offline")
    {
        return Err("Arithmetic benchmark has no live allowance".into());
    }
    if !matches!(
        std::env::var("VOXGOLEM_BENCH_MODE").as_deref(),
        Ok("offline" | "live")
    ) {
        return Err("Benchmark mode must be explicit".into());
    }
    let root = root()?;
    if entry == "voice-arithmetic" {
        validate_null_output(&root)?;
    }
    if entry == "voice-arithmetic-windows" {
        validate_windows_storage(&root)?;
    }
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

fn validate_null_output(root: &Path) -> Result<(), String> {
    if !cfg!(target_os = "linux") || Path::new("/dev/snd").exists() {
        return Err("Arithmetic output requires isolated Linux software ALSA".into());
    }
    let path = PathBuf::from(
        std::env::var_os("ALSA_CONFIG_PATH").ok_or("Arithmetic ALSA configuration missing")?,
    );
    if path.is_symlink()
        || path.canonicalize().ok().as_deref() != Some(root.join("alsa-null.conf").as_path())
    {
        return Err("Arithmetic ALSA configuration is not private".into());
    }
    if fs::metadata(&path)
        .map_err(|_| "Arithmetic ALSA configuration unavailable")?
        .len()
        > 64
        || fs::read(&path).map_err(|_| "Arithmetic ALSA configuration unavailable")?
            != b"pcm.!default { type null }\n"
        || std::env::vars_os().any(|(key, _)| {
            let key = key.to_string_lossy().to_ascii_uppercase();
            ["PULSE_", "PIPEWIRE_", "JACK_"]
                .iter()
                .any(|prefix| key.starts_with(prefix))
        })
    {
        return Err("Arithmetic output route is not software-null only".into());
    }
    Ok(())
}

fn validate_windows_storage(root: &Path) -> Result<(), String> {
    if !cfg!(target_os = "windows")
        || std::env::var("VOXGOLEM_BENCH_OUTPUT_ROUTE").as_deref() != Ok("wasapi-render-diagnostic")
    {
        return Err("Windows arithmetic is diagnostic-only native render output".into());
    }
    for (key, folder) in [
        ("APPDATA", "appdata"),
        ("LOCALAPPDATA", "localappdata"),
        ("USERPROFILE", "home"),
        ("TEMP", "temp"),
        ("TMP", "temp"),
        ("WEBVIEW2_USER_DATA_FOLDER", "webview"),
    ] {
        let path = PathBuf::from(std::env::var_os(key).ok_or("Windows private storage missing")?);
        let expected = root
            .join(folder)
            .canonicalize()
            .map_err(|_| "Windows private storage unavailable")?;
        if path.canonicalize().ok().as_ref() != Some(&expected) || !expected.starts_with(root) {
            return Err("Windows storage escapes private run".into());
        }
    }
    Ok(())
}

pub(crate) fn validate_application_id(identifier: &str) -> Result<(), String> {
    if cfg!(target_os = "windows") && identifier != "dev.gitblame.voxgolem.benchmark" {
        return Err("Windows benchmark requires a separate application identifier".into());
    }
    Ok(())
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

// Keep the outbound-route guard available even though native voice has no PCM
// emitter. Any reintroduced benchmark-instrumented emission must increment it.
#[allow(dead_code)]
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
    if !fixture_allowed(ENTRY.get().copied(), &fixture) {
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
    let (
        flow,
        events,
        dropped,
        readable_pages,
        tool_failures,
        frame_metadata,
        commands,
        native_audio,
    ) = {
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
            command_metadata(&trace),
            native_audio_metadata(&trace, NATIVE_AUDIO.snapshot()),
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
        "native_audio": native_audio,
        "handler_call_metadata": commands,
        "reply_speech": speech_metadata(&events, speech_retired(app_state)),
        "tts_actual_provider": app_state.local_tts_runtime.lock().ok()
            .and_then(|runtime| runtime.as_ref().and_then(|runtime| runtime.actual_provider()))
            .map(|provider| match provider {
                tts::TtsActualProvider::Cpu => "cpu",
                tts::TtsActualProvider::Cuda => "cuda",
            }),
        "generation": voxgolem_platform::app_benchmark::get_counts(),
    }))
}

fn fixture_allowed(entry: Option<&str>, fixture: &str) -> bool {
    match entry {
        Some("voice-arithmetic" | "voice-arithmetic-windows") => fixture == "arithmetic",
        Some("voice") => matches!(fixture, "quick" | "short_research" | "long_research"),
        _ => false,
    }
}

fn speech_retired(state: &AppState) -> bool {
    // Worker remains owned through synthesis and synchronous play_owned completion.
    // Do not infer retirement from renderer state or from cancellation receipts.
    !state.native_speech_running.load(Ordering::Acquire)
        && state.native_speech_worker.available_permits() == 1
        && state
            .native_speech_pending
            .lock()
            .is_ok_and(|pending| pending.is_none())
        && state
            .tts_playback
            .lock()
            .is_ok_and(|playback| playback.current_id.is_none())
        && !tts::physical_work_pending()
}

fn speech_metadata(events: &[Observation], retired: bool) -> serde_json::Value {
    let count = |stage| events.iter().filter(|event| event.stage == stage).count();
    serde_json::json!({
        "admitted": count("reply_speech_admitted"),
        "synthesis_valid": count("reply_synthesis_valid"),
        "output_submitted": count("reply_output_submitted"),
        "output_completed": count("reply_output_completed"),
        "failed": count("reply_speech_failed"),
        "retired": retired,
    })
}

#[cfg(test)]
mod tests {
    #[cfg(target_os = "linux")]
    #[test]
    fn generated_arithmetic_environment_loads_real_model_readiness() {
        if std::env::var("VOXGOLEM_BENCH_MODE").as_deref() != Ok("offline") {
            let child = std::process::Command::new(std::env::current_exe().unwrap())
                .env("VOXGOLEM_BENCH_MODE", "offline")
                .args(["--exact", "app_benchmark::tests::generated_arithmetic_environment_loads_real_model_readiness"])
                .output().unwrap();
            assert!(
                child.status.success(),
                "offline production-loader regression failed: {}",
                String::from_utf8_lossy(&child.stdout)
            );
            return;
        }
        let directory = tempfile::tempdir().unwrap();
        let repo = Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(3)
            .unwrap();
        let generated = std::process::Command::new("python3")
            .args(["-B", "-c", r#"
import pathlib,sys,shutil
sys.path.insert(0,str(pathlib.Path(sys.argv[1])/'tools/app-bench'))
import runner
root=pathlib.Path(sys.argv[2]); source=root/'synthetic-source.toml'
asset=root/'synthetic-model.onnx';asset.write_bytes(b'synthetic-not-initialized')
asr=root/'synthetic-asr';asr.mkdir()
source.write_text('\n'.join(f'{k} = "{p}"' for k,p in [('wake_word_model_path',asset),('silero_vad_model',asset),('parakeet_model_dir',asr)]))
run=root/'run';run.mkdir()
runner.isolated_environment(run,source,'http://127.0.0.1:32123/responses',entry='voice-arithmetic',tts_model=asset)
shutil.copytree(run/'config',root/'VoxGolem')
"#])
            .arg(repo).arg(directory.path()).output().unwrap();
        assert!(
            generated.status.success(),
            "synthetic environment generator failed"
        );
        let _scope = super::super::test_storage::ScopedRoot::new(directory.path().into());
        let path = super::super::application_config_path().unwrap();
        let config = voxgolem_core::config::load_runtime_config(Some(&path)).unwrap();
        let capabilities = super::super::configured_capabilities(&config);
        assert!(super::super::capability_available(
            &capabilities,
            "custom_provider"
        ));
        let settings = super::super::resolve_assistant_settings(
            &config,
            &capabilities,
            super::super::ResponseProfilePayload::Fast,
        );
        assert_eq!(settings.instant.as_str(), "custom-luna-none");
        let state = super::super::build_nonfatal_config_error_app_state(
            super::super::default_voice_pipeline_config(),
            super::super::embedded_cue_asset_paths(),
            "synthetic parser state".into(),
        );
        let snapshot = super::super::get_app_snapshot_from_state(&state).unwrap();
        let payload = serde_json::to_vec(&snapshot).unwrap();
        use std::io::Write;
        let mut parser = std::process::Command::new("bun")
            .args(["-e", "import {parseAppSnapshot} from './frontend/app/src/lib/appSnapshot.ts';try{parseAppSnapshot(JSON.parse(await Bun.stdin.text()));}catch(e){console.error(e.message);process.exit(1)}"])
            .current_dir(repo).stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped())
            .spawn().unwrap();
        parser.stdin.take().unwrap().write_all(&payload).unwrap();
        let parsed = parser.wait_with_output().unwrap();
        assert!(
            parsed.status.success(),
            "native snapshot production frontend parser failed: {}",
            String::from_utf8_lossy(&parsed.stderr)
        );
        let effective =
            super::super::chatgpt_auth::configuration(config.custom_openai.as_ref()).unwrap();
        assert_eq!(effective.endpoint, "http://127.0.0.1:32123/responses");
        assert_eq!(effective.auth_path, config.custom_openai.unwrap().auth_path);
        let credential: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&effective.auth_path).unwrap()).unwrap();
        assert!(credential.get("voxgolemChatgpt").is_none());
        assert!(
            voxgolem_platform::custom_openai::observe_custom_openai_auth(&effective.auth_path)
                .status
                .is_ok()
        );
    }
    #[test]
    fn arithmetic_queue_cannot_borrow_legacy_cases_or_typed_admission() {
        assert!(super::fixture_allowed(
            Some("voice-arithmetic"),
            "arithmetic"
        ));
        assert!(!super::fixture_allowed(Some("voice-arithmetic"), "quick"));
        assert!(!super::fixture_allowed(Some("voice"), "arithmetic"));
        assert!(super::fixture_allowed(
            Some("voice-arithmetic-windows"),
            "arithmetic"
        ));
        assert!(!super::fixture_allowed(
            Some("voice-arithmetic-windows"),
            "quick"
        ));
        assert!(!super::fixture_allowed(Some("typed"), "arithmetic"));
        assert!(!super::fixture_allowed(
            Some("voice-arithmetic"),
            "../arithmetic"
        ));
    }

    #[test]
    fn speech_metadata_does_not_turn_error_or_retirement_into_output_success() {
        let events = vec![super::Observation {
            stage: "reply_speech_failed",
            flow: 1,
            round: 0,
            elapsed_us: 1,
        }];
        let metadata = super::speech_metadata(&events, true);
        assert_eq!(metadata["failed"], 1);
        assert_eq!(metadata["output_completed"], 0);
        assert_eq!(metadata["synthesis_valid"], 0);
        assert_eq!(metadata["retired"], true);
        let reset = super::speech_metadata(&[], false);
        assert_eq!(reset["failed"], 0);
        assert_eq!(reset["retired"], false);
    }
    use super::*;

    #[test]
    fn native_audio_metadata_separates_audio_failures_from_trace_loss() {
        let trace = Trace {
            dropped: 7,
            frame_emissions: 2,
            native_baseline: NativeAudioCounts {
                admitted_frames: 100,
                admitted_samples: 48_000,
                processed_frames: 99,
                overflows: 1,
                stalls: 2,
                capture_failures: 3,
                processing_failures: 4,
            },
            ..Trace::default()
        };
        let metadata = native_audio_metadata(
            &trace,
            NativeAudioCounts {
                admitted_frames: 180,
                admitted_samples: 86_400,
                processed_frames: 178,
                overflows: 2,
                stalls: 4,
                capture_failures: 6,
                processing_failures: 8,
            },
        );
        assert_eq!(
            metadata,
            serde_json::json!({
                "schema": 1, "admitted_frames": 80, "admitted_samples": 38_400,
                "processed_frames": 79, "pcm_events_emitted": 2,
                "overflows": 1, "stalls": 2, "capture_failures": 3, "processing_failures": 4,
            })
        );
        assert_eq!(trace.dropped, 7);
    }

    #[test]
    fn zero_handler_metadata_is_explicit_and_retains_observed_calls() {
        let mut trace = Trace::default();
        trace.commands.insert(
            "ingest_audio_frame",
            CommandStats {
                calls: 1,
                total_us: 4,
                max_us: 4,
            },
        );
        let commands = command_metadata(&trace);
        assert_eq!(commands["ingest_audio_frame"].calls, 1);
        for name in ["mark_silence", "record_speech_activity", "save_wake_clip"] {
            assert_eq!(commands[name].calls, 0);
        }
        assert_eq!(trace.commands.len(), 1);
    }

    #[test]
    fn entry_is_explicit_and_automatic_capture_is_voice_only() {
        for (entry, automatic, expected) in [
            (None, false, Some("voice")),
            (None, true, Some("voice")),
            (Some("voice"), true, Some("voice")),
            (Some("voice-arithmetic"), false, Some("voice-arithmetic")),
            (Some("voice-arithmetic"), true, None),
            (
                Some("voice-arithmetic-windows"),
                false,
                Some("voice-arithmetic-windows"),
            ),
            (Some("voice-arithmetic-windows"), true, None),
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
        let metadata = native_audio_metadata(&trace, trace.native_baseline);
        for name in [
            "admitted_frames",
            "admitted_samples",
            "processed_frames",
            "pcm_events_emitted",
            "overflows",
            "stalls",
            "capture_failures",
            "processing_failures",
        ] {
            assert_eq!(metadata[name], 0, "flow reset must retire {name}");
        }
    }
}
