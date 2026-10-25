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
    if !decision.cancel && decision.speak.is_none() {
        return;
    }
    if decision.cancel {
        if let Ok(mut pending) = state.native_speech_pending.lock() {
            pending.take();
        }
        cancel_physical(state);
    }
    if let Some(request) = decision.speak {
        #[cfg(feature = "app-benchmark")]
        super::app_benchmark::mark("reply_speech_admitted");
        if let Ok(mut pending) = state.native_speech_pending.lock() {
            *pending = Some(request);
        }
        start_worker(app, state);
    }
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
    pending: &Mutex<Option<SpeechRequest>>,
    running: &AtomicBool,
) -> Option<SpeechRequest> {
    let Ok(mut pending) = pending.lock() else {
        running.store(false, Ordering::Release);
        return None;
    };
    if let Some(request) = pending.take() {
        if reply.valid(request.generation) {
            return Some(request);
        }
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
            if let Ok(mut pending) = self.state.native_speech_pending.lock() {
                pending.take();
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
        if let Ok(mut pending) = state.native_speech_pending.lock() {
            pending.take();
        }
        state.native_speech_running.store(false, Ordering::Release);
        return;
    };
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
                if let Ok(mut pending) = state.native_speech_pending.lock() {
                    pending.take();
                }
                latch.armed = false;
                state.native_speech_running.store(false, Ordering::Release);
                publish(&app, &state);
            }
            return;
        };
        loop {
            // Keep the request IN the replaceable slot while old physical work
            // retires: taking it first would create a second pending request.
            if !await_physical_retirement(&state) {
                latch.armed = false;
                stop_after_tracking_failure(&app, &state);
                return;
            }
            let (request, playback_id) = {
                let Ok(reply) = state.native_reply.lock() else {
                    return;
                };
                let request = next_request(
                    &reply,
                    &state.native_speech_pending,
                    &state.native_speech_running,
                );
                let Some(request) = request else {
                    latch.armed = false;
                    publish(&app, &state);
                    return;
                };
                let Ok(id) = state.tts_audio_playback.reserve(PlaybackPurpose::Speech) else {
                    continue;
                };
                if super::register_tts_playback(&state.tts_playback, id).is_err() {
                    let _ = state
                        .tts_audio_playback
                        .cancel_owned(id, PlaybackPurpose::Speech);
                    continue;
                }
                publish(&app, &state);
                (request, id)
            };
            let ticket = super::SpeechPlaybackTicket {
                playback: state.tts_audio_playback.clone(),
                playback_id,
            };
            // Parent helper must accept an already registered matching ticket and
            // recheck this policy generation AFTER its async tts_operation_lock wait.
            let result = tauri::async_runtime::block_on(super::speak_local_tts_from_state(
                request.text,
                playback_id,
                &state,
                Some(request.generation),
            ));
            {
                let Ok(_reply) = state.native_reply.lock() else {
                    return;
                };
                drop(ticket);
                let _ = super::finish_tts_playback_state(&state.tts_playback, playback_id);
                // Read the current owner, never publish an old worker's cached idle.
                publish(&app, &state);
            }
            if result.is_err() {
                #[cfg(feature = "app-benchmark")]
                super::app_benchmark::mark("reply_speech_failed");
                log_category(&state, super::diagnostics::TtsEvent::NativeReplyIncomplete);
            }
            // A cancelled helper result is not physical native-engine completion.
            // Keep this worker's update/work leases until the real call retires.
        }
    });
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
        if let Ok(mut pending) = state.native_speech_pending.lock() {
            pending.take();
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
        .native_speech_pending
        .lock()
        .map(|pending| pending.is_some())
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
    fn one_worker_and_replaceable_pending_slot() {
        let running = AtomicBool::new(false);
        let mut reply = ReplySpeech::default();
        let pending = Mutex::new(None);
        reply.begin("old", true);
        *pending.lock().unwrap() = reply.text("old", "obsolete\n").speak;
        assert!(claim_worker(&running));
        assert!(!claim_worker(&running));
        reply.begin("new", true);
        *pending.lock().unwrap() = reply.text("new", "replacement\n").speak;
        assert!(!claim_worker(&running));
        assert_eq!(
            next_request(&reply, &pending, &running).unwrap().text,
            "replacement"
        );
        assert!(running.load(Ordering::Acquire));
        assert!(next_request(&reply, &pending, &running).is_none());
        assert!(!running.load(Ordering::Acquire));
        assert!(claim_worker(&running));
    }

    #[test]
    fn stale_pending_does_not_start_and_idle_handoff_can_restart() {
        let running = AtomicBool::new(true);
        let mut reply = ReplySpeech::default();
        reply.begin("old", true);
        let pending = Mutex::new(reply.text("old", "obsolete\n").speak);
        reply.reset();
        assert!(next_request(&reply, &pending, &running).is_none());
        assert!(!running.load(Ordering::Acquire));
        assert!(pending.lock().unwrap().is_none());
        reply.begin("new", true);
        *pending.lock().unwrap() = reply.settle("new", Some("committed")).speak;
        assert!(claim_worker(&running));
        assert_eq!(
            next_request(&reply, &pending, &running).unwrap().text,
            "committed"
        );
    }
}
