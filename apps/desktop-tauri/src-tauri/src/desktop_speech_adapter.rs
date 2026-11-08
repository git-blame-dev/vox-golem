//! Desktop-only effects bridge for the shared typed/voice reply policy.
//!
//! Hooks run under the parent's prompt publication gate. Lock order is execution
//! identity -> reply policy -> pending / TTS state -> observation. The worker never
//! acquires the publication gate. No renderer attachment or visibility gates speech.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use tauri::Manager;
use voxgolem_audio::playback::PlaybackPurpose;
use voxgolem_core::conversation::ExecutionId;

use super::desktop_observation::SpeechProjection;
use super::reply_speech::{Decision, ReplySpeech, SpeechRequest};
use super::{AppState, PromptExecutionEventPayload};
use voxgolem_core::speech_delivery::SpeechDelivery;

const RETIREMENT_WAIT: Duration = Duration::from_secs(8);

/// Call once for newly admitted canonical execution, never for observer reattachment.
/// The parent publication gate establishes that this is the newest admission.
pub(super) fn begin(
    app: &tauri::AppHandle,
    request_id: &str,
    execution: ExecutionId,
    enabled: bool,
) {
    let state = app.state::<AppState>();
    let Ok(mut identity) = state.native_reply_execution.lock() else {
        return;
    };
    if *identity == Some(execution) {
        return;
    }
    let Ok(mut reply) = state.native_reply.lock() else {
        return;
    };
    *identity = Some(execution);
    let decision = reply.begin(request_id, enabled);
    apply(app, &state, decision);
}

pub(super) fn event(
    app: &tauri::AppHandle,
    request_id: &str,
    execution: ExecutionId,
    event: &PromptExecutionEventPayload,
) {
    let state = app.state::<AppState>();
    let Ok(identity) = state.native_reply_execution.lock() else {
        return;
    };
    if *identity != Some(execution) {
        return;
    }
    let Ok(mut reply) = state.native_reply.lock() else {
        return;
    };
    let decision = match event {
        PromptExecutionEventPayload::Text { text } => reply.text(request_id, text),
        PromptExecutionEventPayload::Correction { .. } => reply.corrected(request_id),
        PromptExecutionEventPayload::Cancelled { .. }
        | PromptExecutionEventPayload::Error { .. } => reply.cancel(request_id),
        _ => return,
    };
    apply(app, &state, decision);
}

/// Some(answer) is permitted only after canonical commit, including committed refusal.
/// None retires failed/uncommitted/cancelled execution. Does not consult active_prompt:
/// canonical settlement remains valid after that transient admission slot is removed.
pub(super) fn finish(
    app: &tauri::AppHandle,
    request_id: &str,
    execution: ExecutionId,
    canonical: Option<&str>,
) {
    let state = app.state::<AppState>();
    let Ok(identity) = state.native_reply_execution.lock() else {
        return;
    };
    if *identity != Some(execution) {
        return;
    }
    let Ok(mut reply) = state.native_reply.lock() else {
        return;
    };
    let decision = reply.settle(request_id, canonical);
    apply(app, &state, decision);
}

/// Explicit Stop can still own speech after canonical execution has settled.
pub(super) fn cancel(app: &tauri::AppHandle, request_id: &str) -> bool {
    let state = app.state::<AppState>();
    let Ok(_identity) = state.native_reply_execution.lock() else {
        return false;
    };
    let Ok(mut reply) = state.native_reply.lock() else {
        return false;
    };
    let decision = reply.cancel(request_id);
    let cancelled = decision.cancel;
    apply(app, &state, decision);
    cancelled
}

/// Caller owns reply policy lock. Do not call the root global-reset helper here:
/// it would invalidate the replacement generation included in this very decision.
fn apply(app: &tauri::AppHandle, state: &AppState, decision: Decision) {
    if !decision.cancel && decision.speak.is_none() && !decision.stream_closed {
        return;
    }
    let cancel = decision.cancel;
    let speaks = decision.speak.is_some();
    let accepted = state
        .native_speech_delivery
        .lock()
        .is_ok_and(|mut delivery| delivery.apply(decision).is_ok());
    if cancel || !accepted {
        cancel_physical(state);
    }
    if !accepted {
        if let Ok(mut delivery) = state.native_speech_delivery.lock() {
            delivery.cancel();
        }
        log_category(state, super::diagnostics::TtsEvent::NativeReplyIncomplete);
    } else if speaks {
        #[cfg(feature = "app-benchmark")]
        super::app_benchmark::mark("reply_speech_admitted");
        start_worker(app, state);
    }
    state.native_speech_notify.notify_all();
    publish(app, state);
}

/// Reply lock fences cancellation against new reservation/admission. Only the
/// currently registered speech ticket is cancelled; never globally stop audio/cues.
fn cancel_physical(state: &AppState) {
    if let Ok(Some(id)) = super::cancel_current_tts_playback_state(&state.tts_playback) {
        let _ = state
            .tts_audio_playback
            .cancel_owned(id, PlaybackPurpose::Speech);
    }
    if let Ok(runtime) = state.local_tts_runtime.lock() {
        if let Some(runtime) = runtime.as_ref() {
            runtime.cancel_generation();
        }
    }
}

fn claim_worker(running: &AtomicBool) -> bool {
    running
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_ok()
}

/// Called only while holding reply policy. Clearing running and observing an empty
/// slot are one owner transaction, so an enqueue cannot lose its worker wakeup.
fn next_request(
    reply: &ReplySpeech,
    pending: &Mutex<SpeechDelivery>,
    running: &AtomicBool,
) -> Option<SpeechRequest> {
    let Ok(mut pending) = pending.lock() else {
        running.store(false, Ordering::Release);
        return None;
    };
    while let Some(request) = pending.take_ready() {
        if reply.valid(request.generation) {
            return Some(request);
        }
        pending.complete(request.generation, request.chunk_id);
    }
    running.store(false, Ordering::Release);
    None
}

/// Unexpected worker unwind must not strand the CAS latch forever. No automatic
/// retry: a fresh admitted request can start a worker after the old semaphore drops.
struct WorkerLatch<'a> {
    state: &'a AppState,
    armed: bool,
}

impl Drop for WorkerLatch<'_> {
    fn drop(&mut self) {
        if self.armed {
            let _reply = self
                .state
                .native_reply
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if let Ok(mut pending) = self.state.native_speech_delivery.lock() {
                pending.cancel();
            }
            self.state
                .native_speech_running
                .store(false, Ordering::Release);
        }
    }
}

/// One blocking drain, not one task per request. Its physical/update guards are
/// captured before spawn and survive async helper cancellation and native retirement.
fn start_worker(app: &tauri::AppHandle, state: &AppState) {
    if !claim_worker(&state.native_speech_running) {
        return;
    }
    let Ok(update) = super::begin_update_sensitive_operation(&state.update_installation_gate)
    else {
        if let Ok(mut pending) = state.native_speech_delivery.lock() {
            pending.cancel();
        }
        state.native_speech_running.store(false, Ordering::Release);
        return;
    };
    // Stop optional preparation before waiting for physical retirement. Its fixed
    // generation prevents remaining cues from entering ahead of this reply.
    if let Ok(runtime) = state.local_tts_runtime.lock() {
        if let Some(runtime) = runtime.as_ref() {
            runtime.cancel_generation();
        }
    }
    let work = state.native_voice_work.register();
    let worker = state.native_speech_worker.clone();
    let app = app.clone();
    super::spawn_runtime_blocking(move || {
        let _update = update;
        let _work = work;
        let state = app.state::<AppState>();
        let mut latch = WorkerLatch {
            state: &state,
            armed: true,
        };
        let Ok(_worker) = tauri::async_runtime::block_on(worker.acquire_owned()) else {
            if let Ok(_reply) = state.native_reply.lock() {
                if let Ok(mut pending) = state.native_speech_delivery.lock() {
                    pending.cancel();
                }
                latch.armed = false;
                state.native_speech_running.store(false, Ordering::Release);
                publish(&app, &state);
            }
            return;
        };
        let runtime = state
            .local_tts_runtime
            .lock()
            .ok()
            .and_then(|runtime| runtime.clone());
        if let Some(runtime) = runtime {
            drain_neural(&app, &state, runtime);
        } else {
            stop_after_tracking_failure(&app, &state);
            log_category(&state, super::diagnostics::TtsEvent::NativeReplyIncomplete);
        }
        latch.armed = false;
    });
}

fn reject_admission(delivery: &Mutex<SpeechDelivery>, request: &SpeechRequest) {
    if let Ok(mut delivery) = delivery.lock() {
        delivery.complete(request.generation, request.chunk_id);
    }
}

/// One output session spans the selected clauses. Delivered PCM can free synthesis
/// capacity while the bounded native source plays; the session/update owner remains
/// live until explicit producer close and actual output retirement.
fn drain_neural(
    app: &tauri::AppHandle,
    state: &AppState,
    runtime: std::sync::Arc<super::tts::LocalTtsRuntime>,
) {
    loop {
        {
            let Ok(_reply) = state.native_reply.lock() else {
                return;
            };
            let Ok(delivery) = state.native_speech_delivery.lock() else {
                return;
            };
            if delivery.facts().pending == 0 {
                state.native_speech_running.store(false, Ordering::Release);
                drop(delivery);
                publish(app, state);
                return;
            }
        }
        if !await_physical_retirement(state) {
            stop_after_tracking_failure(app, state);
            return;
        }
        let (request, runtime_generation) = {
            let Ok(reply) = state.native_reply.lock() else {
                return;
            };
            let Some(request) = next_request(
                &reply,
                &state.native_speech_delivery,
                &state.native_speech_running,
            ) else {
                publish(app, state);
                return;
            };
            (request, runtime.start_generation())
        };
        let Ok(playback_id) = state.tts_audio_playback.reserve(PlaybackPurpose::Speech) else {
            reject_admission(&state.native_speech_delivery, &request);
            log_category(state, super::diagnostics::TtsEvent::NativeReplyIncomplete);
            publish(app, state);
            continue;
        };
        if super::register_tts_playback(&state.tts_playback, playback_id).is_err() {
            let _ = state
                .tts_audio_playback
                .cancel_owned(playback_id, PlaybackPurpose::Speech);
            reject_admission(&state.native_speech_delivery, &request);
            log_category(state, super::diagnostics::TtsEvent::NativeReplyIncomplete);
            publish(app, state);
            continue;
        }
        let ticket = super::SpeechPlaybackTicket {
            playback: state.tts_audio_playback.clone(),
            playback_id,
        };
        let output = super::neural_speech::Session::new(
            state,
            runtime.clone(),
            playback_id,
            true,
            runtime_generation,
        );
        let generation = request.generation;
        let mut next = Some(request);
        let mut failed = false;
        while let Some(request) = next.take() {
            let valid = state
                .native_reply
                .lock()
                .is_ok_and(|reply| reply.valid(generation));
            if !valid {
                failed = true;
                break;
            }
            let produced = tauri::async_runtime::block_on(output.append(&request.text));
            if produced.is_err() {
                failed = true;
                break;
            }
            // Release synthesis capacity; playback remains owned through drain.
            if let Ok(mut delivery) = state.native_speech_delivery.lock() {
                if !delivery.produced(request.generation, request.chunk_id) {
                    failed = true;
                    break;
                }
            }
            match next_neural_clause(state, generation) {
                Ok(request) => next = request,
                Err(()) => {
                    failed = true;
                    break;
                }
            }
        }
        let retired = tauri::async_runtime::block_on(output.finish(failed));
        drop(output);
        {
            let Ok(reply) = state.native_reply.lock() else {
                return;
            };
            if let Ok(mut delivery) = state.native_speech_delivery.lock() {
                delivery.complete_produced(generation);
            }
            if (failed || retired.is_err()) && reply.valid(generation) {
                if let Ok(mut delivery) = state.native_speech_delivery.lock() {
                    delivery.cancel();
                }
                log_category(state, super::diagnostics::TtsEvent::NativeReplyIncomplete);
            }
            drop(ticket);
            let _ = super::finish_tts_playback_state(&state.tts_playback, playback_id);
            publish(app, state);
        }
        if failed || retired.is_err() {
            report_neural_failure(state, generation);
        }
    }
}

fn report_neural_failure(state: &AppState, generation: u64) {
    // Match preference-publication lock order and recheck after waiting. An old
    // worker must never overwrite a newer admission's service state.
    let Ok(_lifecycle) = state.lifecycle_gate.lock() else {
        return;
    };
    let Ok(mut startup) = state.startup_state.lock() else {
        return;
    };
    let Ok(reply) = state.native_reply.lock() else {
        return;
    };
    if !reply.valid(generation) {
        return;
    }
    if let super::StartupStatePayload::Ready { capabilities, .. }
    | super::StartupStatePayload::WarmingModel { capabilities, .. } = &mut *startup
    {
        if let Some(capability) = capabilities
            .iter_mut()
            .find(|capability| capability.id == "tts")
        {
            capability.state = super::CapabilityStatePayload::Failed;
            capability.reason =
                String::from("Neural TTS speech failed; reconnect the speech service");
            capability.actual_provider = None;
        }
    }
}

fn next_neural_clause(state: &AppState, generation: u64) -> Result<Option<SpeechRequest>, ()> {
    loop {
        let reply = state.native_reply.lock().map_err(|_| ())?;
        let mut delivery = state.native_speech_delivery.lock().map_err(|_| ())?;
        if !reply.valid(generation) {
            return Err(());
        }
        if let Some(request) = delivery.take_ready() {
            return Ok(Some(request));
        }
        if delivery.facts().stream_closed {
            return Ok(None);
        }
        // Release the selector before atomically waiting on the delivery queue:
        // append/cancel can acquire the same lock order and cannot lose this wakeup.
        drop(reply);
        let (delivery, timeout) = state
            .native_speech_notify
            .wait_timeout(delivery, Duration::from_secs(15))
            .map_err(|_| ())?;
        drop(delivery);
        if timeout.timed_out() {
            return Err(());
        }
    }
}

fn await_physical_retirement(state: &AppState) -> bool {
    let mut reported = false;
    loop {
        let started = Instant::now();
        if super::tts::wait_for_physical_retirement(RETIREMENT_WAIT) {
            return true;
        }
        let waited_full_bound = started.elapsed() >= RETIREMENT_WAIT;
        if !reported {
            log_category(
                state,
                super::diagnostics::TtsEvent::NativeReplyAwaitingRetirement,
            );
            reported = true;
        }
        // Tracking poison fails closed immediately. Do not busy-spin on corruption;
        // the underlying physical-work query remains busy and rejects installation.
        if !waited_full_bound {
            return false;
        }
    }
}

fn stop_after_tracking_failure(app: &tauri::AppHandle, state: &AppState) {
    if let Ok(mut reply) = state.native_reply.lock() {
        let _ = reply.reset();
        if let Ok(mut pending) = state.native_speech_delivery.lock() {
            pending.cancel();
        }
        state.native_speech_running.store(false, Ordering::Release);
        publish(app, state);
    }
}

/// Caller owns reply policy, including at completion. Pending remains true for a
/// reserved ticket until helper completion. No activation callback exists here:
/// playing=false deliberately avoids presenting reservation as acoustic activation.
fn publish(app: &tauri::AppHandle, state: &AppState) {
    let pending = state
        .native_speech_delivery
        .lock()
        .map(|pending| !pending.is_idle())
        .unwrap_or(true);
    let playback_id = state
        .tts_playback
        .lock()
        .ok()
        .and_then(|state| state.current_id);
    let projection = SpeechProjection {
        playback_id,
        playing: false,
        pending: pending
            || playback_id.is_some()
            || state.native_speech_running.load(Ordering::Acquire),
    };
    let revision = state
        .native_observation
        .lock()
        .ok()
        .and_then(|mut observation| observation.speech(projection).ok().flatten());
    super::desktop_voice_adapter::notify(app, revision);
}

fn log_category(state: &AppState, category: super::diagnostics::TtsEvent) {
    super::log_tts_runtime_event(&state.telemetry_sink, category);
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn failed_native_admission_releases_delivery_and_successor_can_progress() {
        let running = AtomicBool::new(true);
        let mut reply = ReplySpeech::default();
        let delivery = Mutex::new(SpeechDelivery::new(1));
        let audio = voxgolem_audio::playback::AudioPlaybackService::new();
        delivery
            .lock()
            .unwrap()
            .apply(reply.begin("first", true))
            .unwrap();
        delivery
            .lock()
            .unwrap()
            .apply(reply.settle("first", Some("Synthetic first answer.")))
            .unwrap();
        let request = next_request(&reply, &delivery, &running).unwrap();
        audio.suspend().unwrap();
        assert!(audio.reserve(PlaybackPurpose::Speech).is_err());
        reject_admission(&delivery, &request);
        assert!(delivery.lock().unwrap().is_idle());
        assert!(next_request(&reply, &delivery, &running).is_none());
        assert!(!running.load(Ordering::Acquire));
        audio.resume().unwrap();
        delivery
            .lock()
            .unwrap()
            .apply(reply.begin("next", true))
            .unwrap();
        delivery
            .lock()
            .unwrap()
            .apply(reply.settle("next", Some("Synthetic successor.")))
            .unwrap();
        let next = next_request(&reply, &delivery, &running).unwrap();
        assert!(audio.reserve(PlaybackPurpose::Speech).is_ok());
        reject_admission(&delivery, &next);
        assert!(delivery.lock().unwrap().is_idle());
        assert!(!delivery
            .lock()
            .unwrap()
            .complete(request.generation, request.chunk_id));
    }

    #[test]
    fn one_worker_preserves_every_early_clause_until_ack() {
        let running = AtomicBool::new(false);
        let mut reply = ReplySpeech::early_clauses();
        let pending = Mutex::new(SpeechDelivery::new(1));
        pending
            .lock()
            .unwrap()
            .apply(reply.begin("r", true))
            .unwrap();
        for text in ["One two three. ", "Four five six. ", "Seven eight nine. "] {
            pending
                .lock()
                .unwrap()
                .apply(reply.text("r", text))
                .unwrap();
        }
        assert!(claim_worker(&running));
        assert!(!claim_worker(&running));
        for text in ["One two three.", "Four five six.", "Seven eight nine."] {
            let request = next_request(&reply, &pending, &running).unwrap();
            assert_eq!(request.text, text);
            assert!(pending
                .lock()
                .unwrap()
                .complete(request.generation, request.chunk_id));
        }
        assert!(next_request(&reply, &pending, &running).is_none());
        assert!(!running.load(Ordering::Acquire));
    }
    #[test]
    fn correction_retires_old_delivery_without_dropping_replacement() {
        let running = AtomicBool::new(true);
        let mut reply = ReplySpeech::early_clauses();
        let pending = Mutex::new(SpeechDelivery::new(1));
        pending
            .lock()
            .unwrap()
            .apply(reply.begin("r", true))
            .unwrap();
        pending
            .lock()
            .unwrap()
            .apply(reply.text("r", "Old synthetic clause. "))
            .unwrap();
        let old = next_request(&reply, &pending, &running).unwrap();
        pending
            .lock()
            .unwrap()
            .apply(reply.settle("r", Some("New committed answer.")))
            .unwrap();
        assert!(!pending
            .lock()
            .unwrap()
            .complete(old.generation, old.chunk_id));
        assert_eq!(
            next_request(&reply, &pending, &running).unwrap().text,
            "New committed answer."
        );
    }
}
