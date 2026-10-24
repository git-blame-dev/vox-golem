//! Desktop adapters around the existing capture, conversation and output owners.
use super::*;
use voxgolem_core::voice_pipeline::{VoicePipelineAction, VoicePipelineEvent};

#[derive(Clone, Copy, Debug)]
pub(super) struct VoiceAdmissionToken {
    pub capture_id: u64,
    pub epoch: u64,
    pub utterance: u64,
}

struct VoiceCompletionGates<'a> {
    _completion: MutexGuard<'a, ()>,
    _lifecycle: MutexGuard<'a, ()>,
}

fn lock_voice_completion(state: &AppState) -> Result<VoiceCompletionGates<'_>, String> {
    let completion = state
        .completion_lifecycle_lock
        .lock()
        .map_err(|_| String::from("completion lifecycle unavailable"))?;
    let lifecycle = state
        .lifecycle_gate
        .lock()
        .map_err(|_| String::from("voice lifecycle unavailable"))?;
    Ok(VoiceCompletionGates {
        _completion: completion,
        _lifecycle: lifecycle,
    })
}

pub(super) struct VoiceEffects {
    capture_id: Option<u64>,
    starting: bool,
    clip: voxgolem_audio::buffers::RollingAudioBuffer,
    partial: Option<PartialTranscriptionEventPayload>,
    confidence: Option<f32>,
    last_confidence: Instant,
    error: Option<String>,
}

impl VoiceEffects {
    pub(super) fn new() -> Self {
        Self {
            capture_id: None,
            starting: false,
            clip: voxgolem_audio::buffers::RollingAudioBuffer::new(12_800)
                .expect("fixed wake clip window is nonzero"),
            partial: None,
            confidence: None,
            last_confidence: Instant::now(),
            error: None,
        }
    }
}

pub(super) fn monotonic_ms(state: &AppState) -> u64 {
    u64::try_from(state.voice_clock_origin.elapsed().as_millis()).unwrap_or(u64::MAX)
}

pub(super) fn notify(app: &tauri::AppHandle, revision: Option<u64>) {
    if let Some(revision) = revision {
        if app
            .emit_to(
                "main",
                "desktop-runtime-changed",
                serde_json::json!({ "revision": revision }),
            )
            .is_err()
        {
            if let Ok(mut observer) = app.state::<AppState>().native_observation.lock() {
                observer.publication_failed(revision);
            }
        }
    }
}

pub(super) fn publish_voice(app: &tauri::AppHandle, error: Option<&str>) {
    let state = app.state::<AppState>();
    if let Some(error) = error {
        let Ok(mut effects) = state.voice_effects.lock() else {
            return;
        };
        effects.error = Some(error.to_owned());
    }
    let Ok(projection) = build_voice_projection(&state) else {
        return;
    };
    let revision = state
        .native_observation
        .lock()
        .ok()
        .and_then(|mut observer| observer.voice(projection).ok().flatten());
    notify(app, revision);
}

pub(super) fn build_voice_projection(
    state: &AppState,
) -> Result<desktop_observation::VoiceProjection, String> {
    let phase =
        current_runtime_phase(&state.voice_pipeline_state).unwrap_or(RuntimePhasePayload::Error);
    let before = {
        let effects = state
            .voice_effects
            .lock()
            .map_err(|_| String::from("voice effects unavailable"))?;
        (effects.capture_id, effects.starting)
    };
    // Admission closes immediately on Stop, but neither that receipt nor a fresh
    // renderer read proves physical retirement. Query owners without effects or
    // canonical locks; never wait or join a voice worker to build a projection.
    let current = state.native_voice.current_capture_id();
    let retired = current.is_none()
        && state.native_voice.pending_workers() == 0
        && state.microphone_capture.wait_for_retirement(Duration::ZERO);
    let effects = state
        .voice_effects
        .lock()
        .map_err(|_| String::from("voice effects unavailable"))?;
    // A start/replacement crossing the owner queries is conservatively busy.
    let pending = !retired || before != (effects.capture_id, effects.starting);
    Ok(desktop_observation::VoiceProjection {
        capture_id: if pending || effects.starting {
            current.or(effects.capture_id)
        } else {
            None
        },
        state: if effects.starting {
            desktop_observation::VoiceState::Starting
        } else if pending {
            desktop_observation::VoiceState::Active
        } else if effects.error.is_some() {
            desktop_observation::VoiceState::Error
        } else {
            desktop_observation::VoiceState::Stopped
        },
        runtime_phase: phase,
        auto_stop_on_silence: state.voice_auto_stop.load(Ordering::Acquire),
        auto_start_available: state.voice_autostart_available.load(Ordering::Acquire),
        wake_confidence: effects.confidence,
        partial_transcript: effects.partial.clone(),
        error: effects.error.clone(),
    })
}

pub(super) async fn start_capture(
    app: tauri::AppHandle,
    state: &AppState,
    capture_id: u64,
    device_id: Option<String>,
) -> Result<StartNativeMicrophonePayload, String> {
    let operation = begin_native_microphone_operation(&state.update_installation_gate)?;
    state
        .voice_autostart_available
        .store(false, Ordering::Release);
    ensure_app_running(&state.exit_cleanup_started)?;
    ensure_voice_input_ready(state)?;
    let frame_app = app.clone();
    let failure_app = app.clone();
    let ingress = state.native_voice.start(
        capture_id,
        Arc::new(move |frame| process_frame(&frame_app, frame)),
        Arc::new(move |capture_id, error| {
            #[cfg(feature = "app-benchmark")]
            app_benchmark::native_voice_failure(match error.as_str() {
                "Native microphone processing fell behind" => "overflow",
                "Native microphone frame delivery stalled" => "stall",
                "Native microphone capture failed" => "capture",
                _ => "processing",
            });
            let _ = error;
            capture_failed(&failure_app, capture_id)
        }),
    )?;
    state
        .voice_input_reset_pending
        .store(true, Ordering::Release);
    if let Ok(mut effects) = state.voice_effects.lock() {
        *effects = VoiceEffects::new();
        effects.capture_id = Some(capture_id);
        effects.starting = true;
    }
    publish_voice(&app, None);
    let capture = Arc::clone(&state.microphone_capture);
    let runtime = Arc::clone(&state.native_voice);
    let sample_rate = state.voice_pipeline_config.sample_rate_hz();
    let first = AtomicBool::new(false);
    let (result, _operation) = spawn_runtime_blocking(move || {
        let result = capture.start(
            capture_id,
            device_id,
            sample_rate,
            NATIVE_MICROPHONE_FRAME_SAMPLES,
            Box::new(move |samples| {
                if !first.swap(true, Ordering::AcqRel) {
                    #[cfg(feature = "app-benchmark")]
                    app_benchmark::mark("first_capture_frame");
                }
                let sample_count = samples.len();
                if ingress.try_push(samples).is_ok() {
                    #[cfg(feature = "app-benchmark")]
                    app_benchmark::native_frame_admitted(sample_count);
                }
                let _ = sample_count;
            }),
            Box::new(move |_terminal| {
                #[cfg(feature = "app-benchmark")]
                app_benchmark::mark("capture_terminal_failure");
                let _ = runtime.fail(capture_id, String::from("Native microphone input failed"));
                // The voice worker owns terminal publication; this callback never joins it.
            }),
        );
        (result, operation)
    })
    .await
    .map_err(|_| String::from("Native microphone startup worker failed"))?;
    let started = match result {
        Ok(started) => started,
        Err(_) => {
            let _ = state.native_voice.stop(Some(capture_id));
            publish_voice(&app, Some("Native microphone startup failed"));
            return Err(String::from("Native microphone startup failed"));
        }
    };
    if let Ok(mut effects) = state.voice_effects.lock() {
        if effects.capture_id == Some(capture_id) {
            effects.starting = false;
        }
    }
    publish_voice(&app, None);
    prepare_cues(&app);
    #[cfg(feature = "app-benchmark")]
    app_benchmark::automatic_diagnostic(Arc::clone(&state.microphone_capture));
    Ok(StartNativeMicrophonePayload {
        fell_back_to_default: started.fell_back_to_default,
    })
}

pub(super) async fn stop_capture(
    app: tauri::AppHandle,
    state: &AppState,
    capture_id: u64,
) -> Result<(), String> {
    // Invalidate before waiting for either physical owner. A stale stop is harmless.
    state
        .voice_autostart_available
        .store(false, Ordering::Release);
    retire_capture_turn(state, capture_id)?;
    state.native_voice.request_stop(Some(capture_id))?;
    let relevant = state
        .voice_effects
        .lock()
        .map(|effects| effects.capture_id == Some(capture_id))
        .unwrap_or(false);
    let voice = Arc::clone(&state.native_voice);
    let capture = Arc::clone(&state.microphone_capture);
    let result = spawn_runtime_blocking(move || {
        let deadline = Instant::now() + Duration::from_secs(8);
        let voice_result = voice.stop(Some(capture_id));
        let capture_result = capture
            .stop(capture_id)
            .map_err(|_| String::from("Native microphone retirement failed"));
        if relevant {
            if voice.wait_for_retirement(deadline.saturating_duration_since(Instant::now()))
                && capture.wait_for_retirement(deadline.saturating_duration_since(Instant::now()))
            {
                Ok(())
            } else {
                Err(String::from("Native microphone retirement is pending"))
            }
        } else {
            voice_result.and(capture_result).map(|_| ())
        }
    })
    .await
    .map_err(|_| String::from("Native microphone retirement worker failed"))?;
    if state
        .voice_effects
        .lock()
        .map(|effects| effects.capture_id == Some(capture_id))
        .unwrap_or(false)
    {
        publish_voice(
            &app,
            result
                .as_ref()
                .err()
                .map(|_| "Native microphone retirement is pending"),
        );
    }
    result
}

fn retire_capture_turn(state: &AppState, capture_id: u64) -> Result<(), String> {
    let _lifecycle = state
        .lifecycle_gate
        .lock()
        .map_err(|_| String::from("voice lifecycle unavailable"))?;
    if state
        .native_voice
        .current_capture_id()
        .is_some_and(|current| current != capture_id)
        || state
            .voice_effects
            .lock()
            .map(|effects| effects.capture_id != Some(capture_id))
            .unwrap_or(true)
    {
        return Ok(());
    }
    state.native_voice.request_stop(Some(capture_id))?;
    state
        .native_partial_authority
        .lock()
        .map_err(|_| String::from("partial voice authority unavailable"))?
        .take();
    cancel_voice_cue(state);
    state
        .voice_input_reset_pending
        .store(true, Ordering::Release);
    state
        .partial_transcription
        .lock()
        .map_err(|_| String::from("partial voice state unavailable"))?
        .reset();
    if matches!(
        current_runtime_phase(&state.voice_pipeline_state)?,
        RuntimePhasePayload::Listening | RuntimePhasePayload::Processing
    ) {
        apply_voice_pipeline_transition(
            &state.voice_pipeline_state,
            state.voice_pipeline_config,
            VoicePipelineEvent::ResetToIdle,
        )?;
    }
    if let Ok(mut effects) = state.voice_effects.lock() {
        effects.partial = None;
        effects.confidence = None;
        effects.starting = false;
        effects.clip.clear();
    }
    Ok(())
}

fn capture_failed(app: &tauri::AppHandle, capture_id: u64) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let state = app.state::<AppState>();
        if state
            .voice_effects
            .lock()
            .map(|effects| effects.capture_id != Some(capture_id))
            .unwrap_or(true)
        {
            return;
        }
        let _ = stop_capture(app.clone(), &state, capture_id).await;
        if state
            .voice_effects
            .lock()
            .map(|effects| effects.capture_id == Some(capture_id))
            .unwrap_or(false)
        {
            publish_voice(
                &app,
                Some("Native microphone input stopped; start the microphone to resume"),
            );
        }
    });
}

fn process_frame(app: &tauri::AppHandle, frame: desktop_voice::VoiceFrame) -> Result<(), String> {
    let state = app.state::<AppState>();
    let update_guard = begin_update_sensitive_operation(&state.update_installation_gate)?;
    if !state.native_voice.is_current(frame.capture_id, frame.epoch) {
        return Ok(());
    }
    ensure_app_running(&state.exit_cleanup_started)?;
    // Only this ordered worker mutates detector state. Canonical reset merely
    // invalidates admission and requests this reset, without waiting on inference.
    if state
        .voice_input_reset_pending
        .swap(false, Ordering::AcqRel)
    {
        reset_wake_word_runtime(&state.wake_word_runtime)?;
        reset_voice_activity_runtime(&state.voice_activity_runtime)?;
    }
    let phase = {
        let _lifecycle = state
            .lifecycle_gate
            .lock()
            .map_err(|_| String::from("voice lifecycle unavailable"))?;
        if !state.native_voice.is_current(frame.capture_id, frame.epoch)
            || state.reset_in_progress.load(Ordering::Acquire)
        {
            return Ok(());
        }
        ensure_startup_ready_for_prompt(&state.startup_state)?;
        current_runtime_phase(&state.voice_pipeline_state)?
    };
    let (wake, confidence) = if phase == RuntimePhasePayload::Sleeping {
        record_wake_diagnostics_phase(&state, phase.clone());
        process_wake_word_frame(&state.wake_word_runtime, &frame.samples)?
    } else {
        (None, None)
    };
    let speech = if phase == RuntimePhasePayload::Listening {
        process_voice_activity_frame(&state.voice_activity_runtime, &frame.samples)?
    } else {
        false
    };
    let mut clip = None;
    let mut partial_action = partial_transcription::PartialTranscriptionAction::Ignore;
    let mut final_action = VoicePipelineAction::None;
    let token;
    {
        let _lifecycle = state
            .lifecycle_gate
            .lock()
            .map_err(|_| String::from("voice lifecycle unavailable"))?;
        if !state.native_voice.is_current(frame.capture_id, frame.epoch)
            || state.reset_in_progress.load(Ordering::Acquire)
            || current_runtime_phase(&state.voice_pipeline_state)? != phase
        {
            return Ok(());
        }
        ensure_startup_ready_for_prompt(&state.startup_state)?;
        let mut pipeline = state
            .voice_pipeline_state
            .lock()
            .map_err(|_| String::from("voice state unavailable"))?;
        voxgolem_core::voice_pipeline::prepare_audio_frame_update(
            &mut pipeline,
            state.voice_pipeline_config,
            &frame.samples,
            wake.map(|_| frame.sample_end_ms),
            speech.then_some(frame.sample_end_ms),
        )
        .map_err(|_| String::from("Native voice frame was rejected"))?
        .commit();
        #[cfg(feature = "app-benchmark")]
        app_benchmark::native_frame_processed();
        {
            let mut effects = state
                .voice_effects
                .lock()
                .map_err(|_| String::from("voice effects unavailable"))?;
            if phase == RuntimePhasePayload::Sleeping {
                effects.clip.append_frame(&frame.samples);
            }
            if wake.is_some() {
                clip = confidence.map(|score| (effects.clip.as_vec(), score));
                effects.clip.clear();
                effects.partial = None;
                effects.confidence = None;
            } else if phase == RuntimePhasePayload::Sleeping {
                effects.confidence = confidence;
            }
        }
        if wake.is_some() {
            let session = state
                .partial_voice_session
                .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                    (current < 9_007_199_254_740_991).then_some(current + 1)
                })
                .map_err(|_| String::from("Native voice session identity exhausted"))?
                + 1;
            state
                .partial_transcription
                .lock()
                .map_err(|_| String::from("partial state unavailable"))?
                .start_session(session);
            #[cfg(feature = "app-benchmark")]
            app_benchmark::mark_once("wake_detected");
        }
        token = VoiceAdmissionToken {
            capture_id: frame.capture_id,
            epoch: frame.epoch,
            utterance: state.partial_voice_session.load(Ordering::Acquire),
        };
        if wake.is_some() {
            *state
                .native_partial_authority
                .lock()
                .map_err(|_| String::from("partial voice authority unavailable"))? = Some(token);
        }
        if state.voice_auto_stop.load(Ordering::Acquire) {
            final_action = voxgolem_core::voice_pipeline::prepare_silence_check(
                &mut pipeline,
                state.voice_pipeline_config,
                frame.sample_end_ms,
            )
            .map_err(|_| String::from("Native voice endpoint was rejected"))?
            .commit();
        }
        if matches!(
            final_action,
            VoicePipelineAction::FinishedUtterance { .. }
                | VoicePipelineAction::ExpiredWithoutSpeech
        ) {
            state
                .partial_transcription
                .lock()
                .map_err(|_| String::from("partial state unavailable"))?
                .finalize();
            state
                .native_partial_authority
                .lock()
                .map_err(|_| String::from("partial voice authority unavailable"))?
                .take();
            if let Ok(mut effects) = state.voice_effects.lock() {
                effects.partial = None;
            }
            state
                .voice_input_reset_pending
                .store(true, Ordering::Release);
        } else if phase == RuntimePhasePayload::Listening && state.parakeet_runtime().is_some() {
            partial_action = request_partial_snapshot_after_detected_speech(
                &pipeline,
                &mut *state
                    .partial_transcription
                    .lock()
                    .map_err(|_| String::from("partial state unavailable"))?,
                monotonic_ms(&state),
            );
        }
    }
    if wake.is_some() {
        publish_voice(app, None);
        cue(app, voice_cues::CueType::StartListening, token);
        spawn_custom_and_web_prewarm(app);
        if let Some((samples, confidence)) = clip {
            let app = app.clone();
            tauri::async_runtime::spawn(async move {
                let state = app.state::<AppState>();
                if state.native_voice.is_current(token.capture_id, token.epoch) {
                    let _ = save_wake_clip_from_state(samples, confidence, &state).await;
                }
            });
        }
    } else if phase == RuntimePhasePayload::Sleeping {
        let publish = state
            .voice_effects
            .lock()
            .map(|mut effects| {
                if effects.last_confidence.elapsed() < Duration::from_millis(200) {
                    false
                } else {
                    effects.last_confidence = Instant::now();
                    true
                }
            })
            .unwrap_or(false);
        if publish {
            publish_voice(app, None);
        }
    }
    match final_action {
        VoicePipelineAction::FinishedUtterance {
            transcription_input,
        } => {
            {
                let _gates = lock_voice_completion(&state)?;
                if !state.native_voice.is_current(token.capture_id, token.epoch) {
                    return Ok(());
                }
                clear_completion_request_state_locked(&state, false)?;
            }
            publish_voice(app, None);
            cue(app, voice_cues::CueType::StopListening, token);
            spawn_final(app.clone(), token, transcription_input, update_guard);
        }
        VoicePipelineAction::ExpiredWithoutSpeech => publish_voice(app, None),
        _ => {
            if let Some(runtime) = state.parakeet_runtime() {
                spawn_partial_transcription(
                    app.clone(),
                    partial_action,
                    runtime,
                    Arc::clone(&state.partial_transcription),
                    state.voice_pipeline_config.sample_rate_hz(),
                    update_guard,
                );
            }
        }
    }
    Ok(())
}

fn cue(app: &tauri::AppHandle, cue_type: voice_cues::CueType, token: VoiceAdmissionToken) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let state = app.state::<AppState>();
        if !state.native_voice.is_current(token.capture_id, token.epoch) {
            return;
        }
        let ticket = {
            let Ok(_lifecycle) = state.lifecycle_gate.lock() else {
                return;
            };
            if !state.native_voice.is_current(token.capture_id, token.epoch) {
                return;
            }
            let ticket = state
                .tts_audio_playback
                .reserve(voxgolem_audio::playback::PlaybackPurpose::Cue)
                .ok();
            if let Ok(mut current) = state.native_voice_cue.lock() {
                *current = ticket;
            }
            ticket
        };
        if let Some(ticket) = ticket {
            let _ = activate_voice_cue_from_state(ticket, cue_type, &state).await;
        }
    });
}

pub(super) fn cancel_voice_cue(state: &AppState) {
    if let Some(ticket) = state
        .native_voice_cue
        .lock()
        .ok()
        .and_then(|mut ticket| ticket.take())
    {
        let _ = state
            .tts_audio_playback
            .cancel_owned(ticket, voxgolem_audio::playback::PlaybackPurpose::Cue);
    }
}

pub(super) fn prepare_cues(app: &tauri::AppHandle) {
    let state = app.state::<AppState>();
    let Some(existing) = state
        .local_tts_runtime
        .lock()
        .ok()
        .and_then(|runtime| runtime.clone())
    else {
        return;
    };
    if state.native_voice.current_capture_id().is_none()
        || state.voice_cue_cache.get().is_some()
        || state
            .native_cue_preparing
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
    {
        return;
    }
    let Ok(update) = begin_update_sensitive_operation(&state.update_installation_gate) else {
        state.native_cue_preparing.store(false, Ordering::Release);
        return;
    };
    let physical = state.native_voice_work.register();
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let _ = prepare_voice_cues_from_runtime(&app.state::<AppState>(), Some(existing)).await;
        let _ = spawn_runtime_blocking(move || {
            let _update = update;
            let _physical = physical;
            loop {
                let started = Instant::now();
                if tts::wait_for_physical_retirement(Duration::from_secs(8))
                    || started.elapsed() < Duration::from_secs(8)
                {
                    break;
                }
            }
        })
        .await;
        app.state::<AppState>()
            .native_cue_preparing
            .store(false, Ordering::Release);
    });
}

pub(super) async fn stop_for_update(
    app: &tauri::AppHandle,
    state: &AppState,
) -> Result<(), String> {
    {
        let _lifecycle = state
            .lifecycle_gate
            .lock()
            .map_err(|_| String::from("voice lifecycle unavailable"))?;
        state.native_voice.request_stop(None)?;
        state
            .voice_autostart_available
            .store(false, Ordering::Release);
        state
            .voice_input_reset_pending
            .store(true, Ordering::Release);
        state
            .native_partial_authority
            .lock()
            .map_err(|_| String::from("partial voice authority unavailable"))?
            .take();
        cancel_voice_cue(state);
    }
    let voice = Arc::clone(&state.native_voice);
    let capture = Arc::clone(&state.microphone_capture);
    let retired = spawn_runtime_blocking(move || {
        let deadline = Instant::now() + Duration::from_secs(8);
        let _ = voice.stop(None);
        let capture_stop = capture.stop_current();
        if capture_stop
            .as_ref()
            .is_err_and(|error| *error != voxgolem_audio::capture::CaptureError::StopTimedOut)
        {
            return false;
        }
        voice.wait_for_retirement(deadline.saturating_duration_since(Instant::now()))
            && capture.wait_for_retirement(deadline.saturating_duration_since(Instant::now()))
    })
    .await
    .unwrap_or(false);
    publish_voice(app, None);
    if retired {
        Ok(())
    } else {
        Err(String::from(
            "Native microphone retirement is pending; update installation was not started",
        ))
    }
}

fn spawn_final(
    app: tauri::AppHandle,
    token: VoiceAdmissionToken,
    input: voxgolem_model::parakeet::ParakeetTranscriptionInput,
    guard: UpdateOperationLease,
) {
    let state = app.state::<AppState>();
    let Ok(permit) = Arc::clone(&state.native_final_asr).try_acquire_owned() else {
        fail_final(&state, token);
        return;
    };
    let Some(runtime) = state.parakeet_runtime() else {
        fail_final(&state, token);
        return;
    };
    let physical = state.native_voice_work.register();
    tauri::async_runtime::spawn(async move {
        let result = spawn_runtime_blocking(move || {
            let _physical = physical;
            let _permit = permit;
            let _guard = guard;
            #[cfg(feature = "app-benchmark")]
            app_benchmark::mark_once("final_asr_started");
            let result = runtime
                .lock()
                .map_err(|_| ())
                .and_then(|mut runtime| runtime.transcribe(&input).map_err(|_| ()))
                .map(|text| text.text().to_owned());
            #[cfg(feature = "app-benchmark")]
            app_benchmark::mark_once("final_asr_completed");
            result
        })
        .await;
        let state = app.state::<AppState>();
        if !state.native_voice.is_current(token.capture_id, token.epoch)
            || state.partial_voice_session.load(Ordering::Acquire) != token.utterance
        {
            return;
        }
        let text = match result {
            Ok(Ok(text)) if !text.trim().is_empty() => text,
            _ => {
                fail_final(&state, token);
                return;
            }
        };
        let request_id = native_request_id(token);
        let result = DesktopStageDispatcher {
            app_state: state,
            app: app.clone(),
        }
        .submit(
            request_id,
            text,
            Some(CompletionSource::Voice),
            None,
            Some(token),
        )
        .await;
        if result.is_err() {
            fail_final(&app.state::<AppState>(), token);
        }
        publish_voice(&app, None);
    });
}

fn fail_final(state: &AppState, token: VoiceAdmissionToken) {
    let Ok(_lifecycle) = state.lifecycle_gate.lock() else {
        return;
    };
    if state.native_voice.is_current(token.capture_id, token.epoch)
        && state.partial_voice_session.load(Ordering::Acquire) == token.utterance
        && current_runtime_phase(&state.voice_pipeline_state).ok()
            == Some(RuntimePhasePayload::Processing)
    {
        // Close admission atomically with the failure's originating turn check.
        // The worker performs terminal publication/retirement, never this caller.
        let _ = state.native_voice.fail(
            token.capture_id,
            String::from("Native final transcription failed"),
        );
    }
}

fn native_request_id(token: VoiceAdmissionToken) -> String {
    let nonce = voxgolem_core::external_correlation::external_id(&format!(
        "voice:{}:{}:{}",
        token.capture_id, token.epoch, token.utterance
    ));
    format!(
        "request-{}-{}",
        current_time_ms().unwrap_or_default(),
        nonce
            .strip_prefix("session-")
            .expect("shared correlation has a fixed prefix")
    )
}

pub(super) fn publish_partial(
    app: &tauri::AppHandle,
    token: VoiceAdmissionToken,
    session_id: u64,
    revision: u64,
    text: &str,
) -> bool {
    let state = app.state::<AppState>();
    let Ok(_gates) = lock_voice_completion(&state) else {
        return false;
    };
    if !state.native_voice.is_current(token.capture_id, token.epoch)
        || state.partial_voice_session.load(Ordering::Acquire) != session_id
        || current_runtime_phase(&state.voice_pipeline_state).ok()
            != Some(RuntimePhasePayload::Listening)
    {
        return false;
    }
    if let Ok(mut effects) = state.voice_effects.lock() {
        effects.partial = Some(PartialTranscriptionEventPayload {
            session_id,
            revision,
            text: text.to_owned(),
        });
    }
    let _ = queue_completion_request_locked(
        CompletionSource::Voice,
        revision,
        Some(session_id),
        text.to_owned(),
        &state,
    );
    publish_voice(app, None);
    true
}

pub(super) fn partial_authority(state: &AppState, session_id: u64) -> Option<VoiceAdmissionToken> {
    state
        .native_partial_authority
        .lock()
        .ok()
        .and_then(|authority| *authority)
        .filter(|token| {
            token.utterance == session_id
                && state.native_voice.is_current(token.capture_id, token.epoch)
        })
}

pub(super) fn begin_reply(
    app: &tauri::AppHandle,
    request_id: &str,
    prompt: &str,
    control: &ActivePrompt,
) -> Result<(), String> {
    let state = app.state::<AppState>();
    let _publication = state
        .prompt_publication_gate
        .lock()
        .map_err(|_| String::from("prompt publication unavailable"))?;
    let current = state
        .active_prompt
        .lock()
        .map_err(|_| String::from("active prompt unavailable"))?
        .as_ref()
        .is_some_and(|active| {
            prompt_event_identity_matches(active, request_id, control.generation)
                && !active.cancelled.load(Ordering::Acquire)
        });
    if !current {
        return Err(String::from("assistant request was retired"));
    }
    let enabled = state
        .startup_state
        .lock()
        .map(|startup| {
            matches!(
                &*startup,
                StartupStatePayload::Ready {
                    tts_enabled: true,
                    ..
                } | StartupStatePayload::WarmingModel {
                    tts_enabled: true,
                    ..
                }
            )
        })
        .unwrap_or(false);
    desktop_speech_adapter::begin(app, request_id, control.assistant_generation, enabled);
    if control.native_owned.load(Ordering::Acquire) {
        let revision = state
            .native_observation
            .lock()
            .map_err(|_| String::from("native observation unavailable"))?
            .start_prompt(desktop_observation::NativePromptProjection {
                request_id: request_id.to_owned(),
                execution_id: desktop_observation::ExecutionId::from(
                    &control
                        .pinned
                        .as_ref()
                        .ok_or_else(|| String::from("prepared identity unavailable"))?
                        .wire_execution,
                ),
                prompt: prompt.to_owned(),
                updates: Vec::new(),
                result: None,
            })
            .map_err(|error| error.to_string())?;
        notify(app, revision);
    }
    Ok(())
}

pub(super) fn finish_reply(
    app: &tauri::AppHandle,
    request_id: &str,
    execution: voxgolem_core::conversation::ExecutionId,
    wire_execution: &voxgolem_core::ipc::ExecutionId,
    native_owned: bool,
    result: &Result<PromptFinalPayload, String>,
) {
    let state = app.state::<AppState>();
    let Ok(_publication) = state.prompt_publication_gate.lock() else {
        return;
    };
    let speech_enabled = state
        .startup_state
        .lock()
        .map(|startup| {
            matches!(
                &*startup,
                StartupStatePayload::Ready {
                    tts_enabled: true,
                    ..
                } | StartupStatePayload::WarmingModel {
                    tts_enabled: true,
                    ..
                }
            )
        })
        .unwrap_or(false);
    if !native_owned && !speech_enabled {
        // Ordinary typed/TTS-off settlement needs no second history projection.
        desktop_speech_adapter::finish(app, request_id, execution, None);
        return;
    }
    // Use committed content, never a provisional accumulator or a model footer.
    let turn = result
        .as_ref()
        .ok()
        .and_then(|result| match &result.outcome {
            voxgolem_core::ipc::PromptOutcome::Completed { turn, .. } => Some(turn.turn()),
            voxgolem_core::ipc::PromptOutcome::Refused { turn, .. } => Some(turn.turn()),
            _ => None,
        });
    let canonical = turn.map(|turn| match &turn.answer.content {
        voxgolem_core::ipc::MessageContentKind::Text(text)
        | voxgolem_core::ipc::MessageContentKind::Refusal(text) => text.as_str(),
    });
    desktop_speech_adapter::finish(app, request_id, execution, canonical);
    let payload = match result {
        Ok(result) => serde_json::to_value(result).unwrap_or_default(),
        Err(_) => {
            serde_json::json!({"kind":"error","request_id":request_id,"runtime_phase":"sleeping", "error_message":"The native response could not be synchronized"})
        }
    };
    let revision = state
        .native_observation
        .lock()
        .ok()
        .and_then(|mut observer| {
            observer
                .finish_prompt_for_execution(
                    request_id,
                    &desktop_observation::ExecutionId::from(wire_execution),
                    payload,
                )
                .ok()
                .flatten()
        });
    notify(app, revision);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_voice_projection_does_not_acknowledge_a_logically_stopped_blocked_worker() {
        let state = build_nonfatal_config_error_app_state(
            default_voice_pipeline_config(),
            embedded_cue_asset_paths(),
            "synthetic".into(),
        );
        let initial = build_voice_projection(&state).unwrap();
        assert!(matches!(
            initial.state,
            desktop_observation::VoiceState::Stopped
        ));
        assert!(initial.auto_start_available);
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let release = Mutex::new(release_rx);
        let ingress = state
            .native_voice
            .start(
                1,
                Arc::new(move |_| {
                    let _ = entered_tx.send(());
                    let _ = release.lock().unwrap().recv_timeout(Duration::from_secs(5));
                    Ok(())
                }),
                Arc::new(|_, _| {}),
            )
            .unwrap();
        state.voice_effects.lock().unwrap().capture_id = Some(1);
        ingress.try_push(vec![0.0; 480]).unwrap();
        entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        state.native_voice.request_stop(Some(1)).unwrap();
        assert!(state.native_voice.current_capture_id().is_none());
        let pending = build_voice_projection(&state).unwrap();
        state.voice_effects.lock().unwrap().error = Some("Synthetic capture failure".into());
        let pending_error = build_voice_projection(&state).unwrap();
        release_tx.send(()).unwrap();
        assert!(state
            .native_voice
            .wait_for_retirement(Duration::from_secs(5)));
        assert!(
            matches!(pending.state, desktop_observation::VoiceState::Active),
            "closed admission is not physical retirement"
        );
        assert_eq!(pending.capture_id, Some(1));
        assert!(matches!(
            pending_error.state,
            desktop_observation::VoiceState::Active
        ));
        assert_eq!(pending_error.capture_id, Some(1));
        assert_eq!(
            pending_error.error.as_deref(),
            Some("Synthetic capture failure")
        );
        let failed = build_voice_projection(&state).unwrap();
        assert!(matches!(
            failed.state,
            desktop_observation::VoiceState::Error
        ));
        assert_eq!(failed.capture_id, None);
        state.voice_effects.lock().unwrap().error = None;
        let retired = build_voice_projection(&state).unwrap();
        assert!(matches!(
            retired.state,
            desktop_observation::VoiceState::Stopped
        ));
        assert_eq!(retired.capture_id, None);
    }

    #[cfg(feature = "app-benchmark")]
    #[test]
    fn fresh_voice_projection_waits_for_capture_only_software_backend_retirement() {
        let mut state = build_nonfatal_config_error_app_state(
            default_voice_pipeline_config(),
            embedded_cue_asset_paths(),
            "synthetic".into(),
        );
        state.microphone_capture = Arc::new(
            voxgolem_audio::capture::AudioCaptureService::new_benchmark(16_000, 1).unwrap(),
        );
        let id = state.microphone_capture.reserve_id().unwrap();
        state.voice_effects.lock().unwrap().capture_id = Some(id);
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        state
            .microphone_capture
            .start(
                id,
                None,
                16_000,
                480,
                Box::new(move |_| {
                    let _ = entered_tx.send(());
                    let _ = release_rx.recv_timeout(Duration::from_secs(5));
                }),
                Box::new(|_| {}),
            )
            .unwrap();
        entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(state.native_voice.current_capture_id().is_none());
        assert_eq!(state.native_voice.pending_workers(), 0);
        assert_eq!(
            state.microphone_capture.stop(id),
            Err(voxgolem_audio::capture::CaptureError::StopTimedOut)
        );
        let pending = build_voice_projection(&state).unwrap();
        release_tx.send(()).unwrap();
        assert!(state
            .microphone_capture
            .wait_for_retirement(Duration::from_secs(5)));
        assert!(matches!(
            pending.state,
            desktop_observation::VoiceState::Active
        ));
        assert_eq!(pending.capture_id, Some(id));
        let retired = build_voice_projection(&state).unwrap();
        assert!(matches!(
            retired.state,
            desktop_observation::VoiceState::Stopped
        ));
        assert_eq!(retired.capture_id, None);
    }

    #[test]
    fn voice_projection_preserves_starting_selected_capture_and_stale_stop_fences() {
        let state = build_nonfatal_config_error_app_state(
            default_voice_pipeline_config(),
            embedded_cue_asset_paths(),
            "synthetic".into(),
        );
        let _ingress = state
            .native_voice
            .start(1, Arc::new(|_| Ok(())), Arc::new(|_, _| {}))
            .unwrap();
        {
            let mut effects = state.voice_effects.lock().unwrap();
            effects.capture_id = Some(1);
            effects.starting = true;
        }
        let starting = build_voice_projection(&state).unwrap();
        assert!(matches!(
            starting.state,
            desktop_observation::VoiceState::Starting
        ));
        assert_eq!(starting.capture_id, Some(1));
        state.voice_effects.lock().unwrap().starting = false;
        let active = build_voice_projection(&state).unwrap();
        assert!(matches!(
            active.state,
            desktop_observation::VoiceState::Active
        ));
        assert_eq!(active.capture_id, Some(1));
        state.native_voice.stop(Some(1)).unwrap();
        assert!(state
            .native_voice
            .wait_for_retirement(Duration::from_secs(5)));
        let _replacement = state
            .native_voice
            .start(2, Arc::new(|_| Ok(())), Arc::new(|_, _| {}))
            .unwrap();
        retire_capture_turn(&state, 1).unwrap();
        let replacement = build_voice_projection(&state).unwrap();
        assert!(matches!(
            replacement.state,
            desktop_observation::VoiceState::Active
        ));
        assert_eq!(replacement.capture_id, Some(2));
        state.native_voice.stop(None).unwrap();
        assert!(state
            .native_voice
            .wait_for_retirement(Duration::from_secs(5)));
    }

    #[test]
    fn native_final_request_uses_the_actual_dispatcher_id_contract() {
        let token = VoiceAdmissionToken {
            capture_id: 1,
            epoch: 2,
            utterance: 3,
        };
        let id = native_request_id(token);
        validate_prompt_request_id(&id)
            .expect("native final ASR must reach normal dispatcher admission");
        assert_ne!(
            id,
            native_request_id(VoiceAdmissionToken {
                utterance: 4,
                ..token
            })
        );
    }

    #[test]
    fn native_completion_never_holds_lifecycle_while_waiting_for_completion_owner() {
        let state = Arc::new(build_nonfatal_config_error_app_state(
            default_voice_pipeline_config(),
            embedded_cue_asset_paths(),
            "synthetic".into(),
        ));
        let lifecycle = state.lifecycle_gate.lock().unwrap();
        let (entered, ready) = std::sync::mpsc::channel();
        let worker_state = Arc::clone(&state);
        let worker = std::thread::spawn(move || {
            entered.send(()).unwrap();
            let _gates = lock_voice_completion(&worker_state).unwrap();
        });
        ready.recv_timeout(Duration::from_secs(2)).unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        let completion_owned = loop {
            if state.completion_lifecycle_lock.try_lock().is_err() {
                break true;
            }
            if Instant::now() >= deadline {
                break false;
            }
            std::thread::yield_now();
        };
        drop(lifecycle);
        worker.join().unwrap();
        assert!(
            completion_owned,
            "native work must acquire completion before canonical lifecycle"
        );
    }

    #[test]
    fn pending_partial_handoff_resolves_the_new_sessions_origin_not_the_old_worker() {
        let state = build_nonfatal_config_error_app_state(
            default_voice_pipeline_config(),
            embedded_cue_asset_paths(),
            "synthetic".into(),
        );
        let _ingress = state
            .native_voice
            .start(1, Arc::new(|_| Ok(())), Arc::new(|_, _| {}))
            .unwrap();
        let old = VoiceAdmissionToken {
            capture_id: 1,
            epoch: state.native_voice.epoch(),
            utterance: 1,
        };
        *state.native_partial_authority.lock().unwrap() = Some(old);
        let samples = vec![0.1; PARTIAL_TRANSCRIPTION_MINIMUM_SAMPLES];
        let scheduler_owner = new_partial_transcription_scheduler();
        let mut scheduler = scheduler_owner.lock().unwrap();
        scheduler.start_session(1);
        let first = scheduler.request_snapshot(1_000, &samples);
        let partial_transcription::PartialTranscriptionAction::StartSnapshot {
            revision: old_revision,
            ..
        } = first
        else {
            panic!("old snapshot starts");
        };
        state.native_voice.invalidate().unwrap();
        let new = VoiceAdmissionToken {
            capture_id: 1,
            epoch: state.native_voice.epoch(),
            utterance: 2,
        };
        *state.native_partial_authority.lock().unwrap() = Some(new);
        scheduler.start_session(2);
        let _ = scheduler.request_snapshot(2_000, &samples);
        let next = scheduler.complete(2_500, 1, old_revision, "Obsolete synthetic partial".into());
        let partial_transcription::PartialTranscriptionAction::StartSnapshot {
            session_id,
            revision,
            ..
        } = next
        else {
            panic!("pending new session transfers to old worker");
        };
        let authority = partial_authority(&state, session_id)
            .expect("new snapshot has its own capture authority");
        assert_eq!(authority.epoch, new.epoch);
        assert_ne!(authority.epoch, old.epoch);
        assert!(partial_authority(&state, 1).is_none());
        assert!(matches!(
            scheduler.complete(
                3_000,
                session_id,
                revision,
                "Current synthetic partial".into()
            ),
            partial_transcription::PartialTranscriptionAction::PublishText { session_id: 2, .. }
        ));
        state.native_voice.stop(None).unwrap();
    }
}
