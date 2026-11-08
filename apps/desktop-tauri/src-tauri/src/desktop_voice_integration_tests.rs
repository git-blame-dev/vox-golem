//! Root-owner regressions using synthetic state and real unstarted output tickets.
//! No AppHandle, microphone, audible output, provider, or TTS model is constructed.
//! Idle tests share the TTS owner's test guard to isolate global physical work.

use super::*;
use std::future::{poll_fn, Future};
use std::task::Poll;
use voxgolem_audio::playback::PlaybackPurpose;

fn synthetic_state() -> AppState {
    // Under cfg(test), initialize_app_state uses test_storage's synthetic root,
    // never the user's configuration or conversation database.
    build_nonfatal_config_error_app_state(
        default_voice_pipeline_config(),
        embedded_cue_asset_paths(),
        "synthetic voice integration state".into(),
    )
}

fn reserve_speech(state: &AppState) -> u64 {
    let id = state
        .tts_audio_playback
        .reserve(PlaybackPurpose::Speech)
        .unwrap();
    register_tts_playback(&state.tts_playback, id).unwrap();
    id
}

fn owns_speech(state: &AppState, id: u64) -> bool {
    state
        .tts_audio_playback
        .owns(id, PlaybackPurpose::Speech)
        .unwrap()
}

#[test]
fn recovery_owner_query_requires_exact_request_and_canonical_execution() {
    let state = synthetic_state();
    let execution = voxgolem_core::conversation::ExecutionId {
        session_id: voxgolem_core::conversation::SessionId(17),
        generation: voxgolem_core::conversation::GenerationId(2),
    };
    let prepared = state
        .durable_store
        .lock()
        .unwrap()
        .ensure_store()
        .unwrap()
        .prepare_request(
            voxgolem_conversation_store::Selection {
                conversation: None,
                selected_head: None,
            },
            "Synthetic owner prompt",
            4096,
            true,
        )
        .unwrap();
    let selection = prepared.context.selection;
    let wire = voxgolem_core::ipc::ExecutionId {
        session_id: voxgolem_core::ipc::WireId::from_operation_id(&selection.conversation).unwrap(),
        generation: voxgolem_core::ipc::WireId::from_operation_id(&prepared.request).unwrap(),
    };
    assert_eq!(
        get_desktop_request_owner_from_state(&state, "synthetic-owner", &wire).unwrap(),
        None
    );
    register_active_prompt(
        &state.active_prompt,
        &state.active_prompt_generation,
        "synthetic-owner",
        execution,
        None,
        0,
    )
    .unwrap();
    state.active_prompt.lock().unwrap().as_mut().unwrap().pinned =
        Some(Arc::new(PinnedDesktopRequest {
            context_revision: selection.revision,
            conversation: selection.conversation,
            parent: selection.parent,
            ancestors: selection.ancestor_ids,
            request: prepared.request,
            wire_execution: wire.clone(),
        }));
    assert_eq!(
        get_desktop_request_owner_from_state(&state, "synthetic-owner", &wire).unwrap(),
        Some("renderer")
    );
    state
        .active_prompt
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .native_owned
        .store(true, Ordering::Release);
    assert_eq!(
        get_desktop_request_owner_from_state(&state, "synthetic-owner", &wire).unwrap(),
        Some("native_voice")
    );
    assert_eq!(
        get_desktop_request_owner_from_state(&state, "another-request", &wire).unwrap(),
        None
    );
    for obsolete in [
        voxgolem_core::ipc::ExecutionId {
            session_id: 16_u64.into(),
            ..wire.clone()
        },
        voxgolem_core::ipc::ExecutionId {
            generation: 1_u64.into(),
            ..wire.clone()
        },
    ] {
        assert_eq!(
            get_desktop_request_owner_from_state(&state, "synthetic-owner", &obsolete).unwrap(),
            None
        );
    }
}

#[test]
fn root_cancellation_retires_streaming_policy_pending_and_unstarted_ticket() {
    let state = synthetic_state();
    assert!(state.active_prompt.lock().unwrap().is_none());
    let speech = {
        let mut reply = state.native_reply.lock().unwrap();
        reply.begin("synthetic-stream", true);
        reply
            .text("synthetic-stream", "Synthetic first paragraph.\n")
            .speak
            .unwrap()
    };
    let generation = speech.generation;
    state
        .native_speech_delivery
        .lock()
        .unwrap()
        .apply(reply_speech::Decision {
            speak: Some(speech),
            ..Default::default()
        })
        .unwrap();
    let ticket = reserve_speech(&state);
    assert!(state.native_reply.lock().unwrap().valid(generation));
    assert!(owns_speech(&state, ticket));

    // Exercise the common root cancellation effect used by Stop/reset/new admission,
    // not a mirror of those effects or a claim that the UI command was exercised.
    cancel_active_tts_generation(&state);

    assert!(!state.native_reply.lock().unwrap().valid(generation));
    assert!(state.native_speech_delivery.lock().unwrap().is_idle());
    assert!(state.tts_playback.lock().unwrap().current_id.is_none());
    assert!(!owns_speech(&state, ticket));
    assert!(state
        .native_reply
        .lock()
        .unwrap()
        .settle("synthetic-stream", Some("Synthetic first paragraph."))
        .speak
        .is_none());
}

#[test]
fn root_cancellation_retires_canonical_fallback_with_no_active_prompt() {
    let state = synthetic_state();
    let speech = {
        let mut reply = state.native_reply.lock().unwrap();
        reply.begin("synthetic-settled", true);
        reply
            .settle("synthetic-settled", Some("Synthetic committed fallback."))
            .speak
            .unwrap()
    };
    let ticket = reserve_speech(&state);
    assert!(state.active_prompt.lock().unwrap().is_none());
    assert!(state.native_reply.lock().unwrap().valid(speech.generation));
    assert!(owns_speech(&state, ticket));

    cancel_active_tts_generation(&state);

    assert!(!state.native_reply.lock().unwrap().valid(speech.generation));
    assert!(state.tts_playback.lock().unwrap().current_id.is_none());
    assert!(!owns_speech(&state, ticket));
}

#[test]
fn canonical_reset_retires_speech_created_after_preliminary_cancellation() {
    let state = synthetic_state();
    cancel_active_tts_generation(&state);
    let speech = {
        let mut reply = state.native_reply.lock().unwrap();
        reply.begin("synthetic-late-admission", true);
        reply
            .text("synthetic-late-admission", "Synthetic late paragraph.\n")
            .speak
            .unwrap()
    };
    let ticket = reserve_speech(&state);
    let (_execution, _reset_guard) = state
        .conversation
        .begin_reset(|| reset_voice_runtime_session(&state))
        .unwrap();
    assert!(!state.native_reply.lock().unwrap().valid(speech.generation));
    assert!(!owns_speech(&state, ticket));
}

#[test]
fn stale_ticket_completion_cannot_clear_replacement_after_root_cancellation() {
    let state = synthetic_state();
    let obsolete = {
        let mut reply = state.native_reply.lock().unwrap();
        reply.begin("synthetic-old", true);
        reply
            .text("synthetic-old", "Old synthetic paragraph.\n")
            .speak
            .unwrap()
    };
    let old_ticket = reserve_speech(&state);
    cancel_active_tts_generation(&state);
    let replacement = {
        let mut reply = state.native_reply.lock().unwrap();
        reply.begin("synthetic-new", true);
        reply
            .settle("synthetic-new", Some("New synthetic committed paragraph."))
            .speak
            .unwrap()
    };
    let new_ticket = reserve_speech(&state);
    assert!(new_ticket > old_ticket);

    finish_tts_playback_state(&state.tts_playback, old_ticket).unwrap();

    assert!(!state
        .native_reply
        .lock()
        .unwrap()
        .valid(obsolete.generation));
    assert!(state
        .native_reply
        .lock()
        .unwrap()
        .valid(replacement.generation));
    ensure_tts_playback_current(&state.tts_playback, new_ticket).unwrap();
    assert!(owns_speech(&state, new_ticket));
    assert!(!owns_speech(&state, old_ticket));
    cancel_active_tts_generation(&state);
}

#[tokio::test]
async fn native_speech_rechecks_generation_after_operation_lock_without_retiring_new_ticket() {
    let state = synthetic_state();
    assert!(state.local_tts_runtime.lock().unwrap().is_none());
    let speech = {
        let mut reply = state.native_reply.lock().unwrap();
        reply.begin("synthetic-blocked", true);
        reply
            .text("synthetic-blocked", "Synthetic blocked paragraph.\n")
            .speak
            .unwrap()
    };
    let old_ticket = reserve_speech(&state);
    let operation = state.tts_operation_lock.lock().await;
    let mut speaking = Box::pin(speak_local_tts_from_state(
        speech.text,
        old_ticket,
        &state,
        Some(speech.generation),
    ));
    // Poll the actual future once. Pending proves it passed initial ownership and
    // reached the held operation lock; no sleeps or scheduling assumptions required.
    poll_fn(|context| {
        assert!(matches!(speaking.as_mut().poll(context), Poll::Pending));
        Poll::Ready(())
    })
    .await;

    cancel_active_tts_generation(&state);
    let replacement = {
        let mut reply = state.native_reply.lock().unwrap();
        reply.begin("synthetic-replacement", true);
        reply
            .settle("synthetic-replacement", Some("Synthetic replacement."))
            .speak
            .unwrap()
    };
    let new_ticket = reserve_speech(&state);
    drop(operation);

    let error = tokio::time::timeout(Duration::from_secs(5), speaking)
        .await
        .unwrap()
        .expect_err("retired generation must not reach unavailable runtime");
    assert_eq!(error, "native speech was retired");
    assert!(state
        .native_reply
        .lock()
        .unwrap()
        .valid(replacement.generation));
    ensure_tts_playback_current(&state.tts_playback, new_ticket).unwrap();
    assert!(owns_speech(&state, new_ticket));
    assert!(!owns_speech(&state, old_ticket));
    cancel_active_tts_generation(&state);
}

#[test]
fn update_idle_rejects_native_speech_worker_until_flag_retires() {
    let _tts_serial = tts::test_runtime_guard();
    let state = synthetic_state();
    ensure_update_installation_is_idle(&state).expect("synthetic ready state is idle");
    state.native_speech_running.store(true, Ordering::Release);
    assert!(ensure_update_installation_is_idle(&state).is_err());
    state.native_speech_running.store(false, Ordering::Release);
    ensure_update_installation_is_idle(&state).expect("retired native speech worker is idle");
}

#[test]
fn update_idle_rejects_registered_native_physical_work_until_permit_drops() {
    let _tts_serial = tts::test_runtime_guard();
    let state = synthetic_state();
    ensure_update_installation_is_idle(&state).expect("synthetic ready state is idle");
    let physical_work = state.native_voice_work.register();
    assert!(state.native_voice_work.has_pending());
    assert!(ensure_update_installation_is_idle(&state).is_err());
    drop(physical_work);
    assert!(!state.native_voice_work.has_pending());
    ensure_update_installation_is_idle(&state).expect("retired native physical work is idle");
}

#[tokio::test]
async fn shutdown_cancels_canonical_request_before_blocked_native_retirement() {
    let _tts_serial = tts::test_runtime_guard();
    let state = synthetic_state();
    state
        .conversation_session
        .lock()
        .unwrap()
        .replace_policy(voxgolem_core::assistant::AssistantPolicy {
            instant_model: voxgolem_core::assistant::InstantModel::CustomLunaNone,
            ..Default::default()
        })
        .unwrap();
    let request = state
        .conversation
        .admit(
            state.prompt_admission_resources(),
            "request-1-syntheticshutdown",
            "Synthetic pending provider request",
            CompletionSource::Typed,
        )
        .unwrap();
    let physical = state.native_voice_work.register();
    let mut shutting_down = Box::pin(shutdown_runtime(&state));
    poll_fn(|context| {
        assert!(matches!(
            shutting_down.as_mut().poll(context),
            Poll::Pending
        ));
        Poll::Ready(())
    })
    .await;
    assert!(
        request.cancelled.load(Ordering::Acquire),
        "provider cancellation must not wait for unrelated native work"
    );
    drop(physical);
    tokio::time::timeout(Duration::from_secs(5), shutting_down)
        .await
        .unwrap();
    assert!(state.shutdown_complete.load(Ordering::Acquire));
}
