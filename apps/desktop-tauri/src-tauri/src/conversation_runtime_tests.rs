use super::*;
use voxgolem_core::assistant::{
    AssistantPolicy, Content, InstantModel, InstantOutcome, Stage, StageResult,
};
use voxgolem_core::conversation::{
    AppliedOutcome, SettlementIntent, SettlementResult, UpdateResult,
};

fn owner() -> DesktopConversationRuntime {
    DesktopConversationRuntime::new(
        Arc::new(Mutex::new(())),
        AssistantPolicy {
            instant_model: InstantModel::CustomSolHigh,
            deep_enabled: false,
            review_enabled: false,
            ..Default::default()
        },
    )
}

fn ready_startup() -> StartupStatePayload {
    StartupStatePayload::Ready {
        cue_asset_paths: CueAssetPathsPayload {
            start_listening: "synthetic-start".into(),
            stop_listening: "synthetic-stop".into(),
        },
        runtime_phase: RuntimePhasePayload::Sleeping,
        voice_input_available: false,
        voice_input_error: None,
        initial_silence_timeout_ms: 2500,
        silence_timeout_ms: 750,
        selected_response_profile: ResponseProfilePayload::Fast,
        supported_response_profiles: vec![],
        prompt_cancellation_available: true,
        tts_enabled: false,
        tts_output_gain_db: 0.0,
        capabilities: vec![],
    }
}

struct NativeAdmissionFixture {
    state: Arc<AppState>,
    _ingress: desktop_voice::VoiceIngress,
    utterance: AtomicU64,
    token: desktop_voice_adapter::VoiceAdmissionToken,
}

impl NativeAdmissionFixture {
    fn new() -> Self {
        use voxgolem_core::voice_pipeline::VoicePipelineEvent;
        let config = default_voice_pipeline_config();
        let mut state =
            build_startup_error_app_state(config, "Synthetic fixture initialization".into());
        state.conversation = owner();
        state.telemetry_sink = None;
        *state.startup_state.lock().unwrap() = ready_startup();
        *state.voice_pipeline_state.lock().unwrap() =
            voxgolem_core::voice_pipeline::VoicePipelineState::new(config).unwrap();
        for event in [
            VoicePipelineEvent::StartupValidated,
            VoicePipelineEvent::WakeWordDetected { now_ms: 0 },
            VoicePipelineEvent::RecordListeningFrame {
                frame: vec![0.25; 480],
            },
            VoicePipelineEvent::SpeechDetected { now_ms: 30 },
            VoicePipelineEvent::SilenceCheck { now_ms: 10_000 },
        ] {
            apply_voice_pipeline_transition(&state.voice_pipeline_state, config, event).unwrap();
        }
        state.network_preferences.lock().unwrap().web_enabled = true;
        state.ensure_hydrated().unwrap();
        let ingress = state
            .native_voice
            .start(1, Arc::new(|_| Ok(())), Arc::new(|_, _| {}))
            .unwrap();
        let token = desktop_voice_adapter::VoiceAdmissionToken {
            capture_id: 1,
            epoch: state.native_voice.epoch(),
            utterance: 7,
        };
        Self {
            state: Arc::new(state),
            _ingress: ingress,
            utterance: AtomicU64::new(7),
            token,
        }
    }

    fn resources(&self) -> PromptAdmissionResources<'_> {
        let mut resources = self.state.prompt_admission_resources();
        resources.native_voice = Some((&self.state.native_voice, &self.utterance, self.token));
        resources
    }

    fn assert_unadmitted(&self) {
        assert!(self.state.active_prompt.lock().unwrap().is_none());
        let session = self.state.conversation_session.lock().unwrap();
        assert!(!session.has_active_execution());
        assert!(session.turns().is_empty());
        assert!(session.provider_history().is_empty());
    }
}

#[test]
fn native_voice_admission_rejects_stopped_replaced_or_changed_utterance_tokens() {
    let temp = tempfile::tempdir().unwrap();
    let _scope = test_storage::ScopedRoot::new(temp.path().to_path_buf());
    for replace in [false, true] {
        let fixture = NativeAdmissionFixture::new();
        let _ = fixture.state.native_voice.stop(Some(1));
        assert!(fixture
            .state
            .native_voice
            .wait_for_retirement(Duration::from_secs(3)));
        let _replacement = if replace {
            Some(
                fixture
                    .state
                    .native_voice
                    .start(2, Arc::new(|_| Ok(())), Arc::new(|_, _| {}))
                    .unwrap(),
            )
        } else {
            None
        };
        assert!(fixture
            .state
            .admit(
                fixture.resources(),
                "synthetic-retired-voice",
                "Synthetic final transcript",
                CompletionSource::Voice
            )
            .is_err());
        fixture.assert_unadmitted();
    }
    let fixture = NativeAdmissionFixture::new();
    fixture.utterance.fetch_add(1, Ordering::AcqRel);
    assert!(fixture
        .state
        .admit(
            fixture.resources(),
            "synthetic-changed-turn",
            "Synthetic final transcript",
            CompletionSource::Voice
        )
        .is_err());
    fixture.assert_unadmitted();
}

#[test]
fn native_voice_admission_rechecks_reset_or_turn_retirement_inside_canonical_gates() {
    let temp = tempfile::tempdir().unwrap();
    let _scope = test_storage::ScopedRoot::new(temp.path().to_path_buf());
    for reset in [false, true] {
        let fixture = Arc::new(NativeAdmissionFixture::new());
        let publication = fixture.state.prompt_publication_gate.lock().unwrap();
        let lifecycle = fixture.state.lifecycle_gate.lock().unwrap();
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (result_tx, result_rx) = std::sync::mpsc::channel();
        let pending = Arc::clone(&fixture);
        let waiter = std::thread::spawn(move || {
            entered_tx.send(()).unwrap();
            let result = pending
                .state
                .admit(
                    pending.resources(),
                    "synthetic-gated-voice",
                    "Synthetic completed transcript",
                    CompletionSource::Voice,
                )
                .map(drop);
            result_tx.send(result).unwrap();
        });
        entered_rx.recv_timeout(Duration::from_secs(3)).unwrap();
        assert!(result_rx.try_recv().is_err());
        if reset {
            reset_voice_runtime_session(&fixture.state).unwrap();
            assert!(fixture
                .state
                .voice_input_reset_pending
                .load(Ordering::Acquire));
        } else {
            fixture.utterance.fetch_add(1, Ordering::AcqRel);
        }
        drop(lifecycle);
        drop(publication);
        assert_eq!(
            result_rx
                .recv_timeout(Duration::from_secs(3))
                .unwrap()
                .unwrap_err(),
            "voice utterance was retired before admission"
        );
        waiter.join().unwrap();
        fixture.assert_unadmitted();
    }
}

#[test]
fn native_voice_admission_requires_processing_even_when_capture_token_is_current() {
    let temp = tempfile::tempdir().unwrap();
    let _scope = test_storage::ScopedRoot::new(temp.path().to_path_buf());
    let fixture = NativeAdmissionFixture::new();
    apply_voice_pipeline_transition(
        &fixture.state.voice_pipeline_state,
        fixture.state.voice_pipeline_config,
        voxgolem_core::voice_pipeline::VoicePipelineEvent::ResetToIdle,
    )
    .unwrap();
    assert!(fixture
        .state
        .native_voice
        .is_current(fixture.token.capture_id, fixture.token.epoch));
    assert!(fixture
        .state
        .admit(
            fixture.resources(),
            "synthetic-retired-phase",
            "Synthetic completed transcript",
            CompletionSource::Voice,
        )
        .is_err());
    fixture.assert_unadmitted();
}

#[test]
fn valid_native_voice_admission_preserves_policy_history_source_and_native_ownership() {
    let temp = tempfile::tempdir().unwrap();
    let _scope = test_storage::ScopedRoot::new(temp.path().to_path_buf());
    let fixture = NativeAdmissionFixture::new();
    let history = {
        let mut session = fixture.state.conversation_session.lock().unwrap();
        let execution = session
            .admit("synthetic-prior-turn", "Synthetic prior question")
            .unwrap()
            .execution_id;
        session.update(
            execution,
            Stage::Instant,
            StageResult::Instant(InstantOutcome::Complete(Content::Text(
                "Synthetic prior answer".into(),
            ))),
        );
        assert!(matches!(
            session.settle(execution, SettlementIntent::Success),
            SettlementResult::Applied(AppliedOutcome::Completed(_))
        ));
        session.provider_history()
    };
    let admitted = fixture
        .state
        .admit(
            fixture.resources(),
            "synthetic-valid-voice",
            "Synthetic voice question",
            CompletionSource::Voice,
        )
        .unwrap();
    assert!(admitted.control.native_owned.load(Ordering::Acquire));
    assert!(fixture
        .state
        .active_prompt
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .native_owned
        .load(Ordering::Acquire));
    assert!(matches!(admitted.request.source, CompletionSource::Voice));
    assert_eq!(admitted.request.instant_model, InstantModel::CustomSolHigh);
    assert!(!admitted.request.preferences.deep_enabled);
    assert!(!admitted.request.preferences.review_enabled);
    assert!(admitted.request.web_enabled);
    assert_eq!(admitted.request.prompt, "Synthetic voice question");
    assert_eq!(admitted.request.history, history);
    assert_eq!(
        current_runtime_phase(&fixture.state.voice_pipeline_state).unwrap(),
        RuntimePhasePayload::Executing
    );
    drop(admitted);
}

#[test]
fn canonical_voice_reset_does_not_wait_for_blocked_physical_work_or_accept_its_late_result() {
    let temp = tempfile::tempdir().unwrap();
    let _scope = test_storage::ScopedRoot::new(temp.path().to_path_buf());
    for fail in [false, true] {
        let fixture = NativeAdmissionFixture::new();
        let _ = fixture.state.native_voice.stop(Some(1));
        assert!(fixture
            .state
            .native_voice
            .wait_for_retirement(Duration::from_secs(3)));
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let (late_tx, late_rx) = std::sync::mpsc::channel();
        let release_rx = Mutex::new(release_rx);
        let native = Arc::clone(&fixture.state.native_voice);
        let weak = Arc::downgrade(&native);
        let ingress = native
            .start(
                2,
                Arc::new(move |frame| {
                    entered_tx.send(()).unwrap();
                    let _ = release_rx
                        .lock()
                        .unwrap()
                        .recv_timeout(Duration::from_secs(3));
                    late_tx
                        .send(
                            weak.upgrade()
                                .unwrap()
                                .is_current(frame.capture_id, frame.epoch),
                        )
                        .unwrap();
                    if fail {
                        Err("Synthetic obsolete inference failure".into())
                    } else {
                        Ok(())
                    }
                }),
                Arc::new(|_, _| {}),
            )
            .unwrap();
        let epoch = native.epoch();
        ingress.try_push(vec![0.0; 480]).unwrap();
        entered_rx.recv_timeout(Duration::from_secs(3)).unwrap();
        let (reset_tx, reset_rx) = std::sync::mpsc::channel();
        let state = Arc::clone(&fixture.state);
        let reset = std::thread::spawn(move || {
            reset_tx.send(reset_voice_runtime_session(&state)).unwrap();
        });
        let returned = reset_rx.recv_timeout(Duration::from_secs(1));
        let still_pending = native.pending_workers();
        let retired = !native.is_current(2, epoch);
        release_tx.send(()).unwrap();
        assert!(returned.unwrap().is_ok());
        assert_eq!(still_pending, 1);
        assert!(retired);
        assert!(!late_rx.recv_timeout(Duration::from_secs(3)).unwrap());
        reset.join().unwrap();
        assert!(fixture
            .state
            .voice_input_reset_pending
            .load(Ordering::Acquire));
        assert_eq!(
            current_runtime_phase(&fixture.state.voice_pipeline_state).unwrap(),
            RuntimePhasePayload::Sleeping
        );
        fixture.assert_unadmitted();
        let _ = native.stop(Some(2));
        assert!(native.wait_for_retirement(Duration::from_secs(3)));
    }
}

#[test]
fn owner_admission_captures_permission_policy_and_mode_without_root_state() {
    let runtime = owner();
    let config = default_voice_pipeline_config();
    let startup = Arc::new(Mutex::new(ready_startup()));
    let server = Mutex::new(None);
    let exit_cleanup_started = AtomicBool::new(false);
    let network = Mutex::new(NetworkPreferences {
        web_enabled: true,
        ..Default::default()
    });
    let voice = Arc::new(Mutex::new(
        voxgolem_core::voice_pipeline::VoicePipelineState::new(config).unwrap(),
    ));
    apply_voice_pipeline_transition(
        &voice,
        config,
        voxgolem_core::voice_pipeline::VoicePipelineEvent::StartupValidated,
    )
    .unwrap();
    let request = runtime
        .admit(
            PromptAdmissionResources {
                exit_cleanup_started: &exit_cleanup_started,
                startup_state: &startup,
                opencode_server: &server,
                network_preferences: &network,
                voice_pipeline_state: &voice,
                voice_pipeline_config: config,
                native_voice: None,
            },
            "synthetic-admission",
            "Synthetic current",
            CompletionSource::Typed,
        )
        .unwrap();
    network.lock().unwrap().web_enabled = false;
    assert!(request.request.web_enabled);
    assert_eq!(request.request.instant_model, InstantModel::CustomSolHigh);
    assert_eq!(request.request.prompt, "Synthetic current");
    drop(request);
    assert!(!runtime
        .conversation_session
        .lock()
        .unwrap()
        .has_active_execution());
    assert!(runtime.active_prompt.lock().unwrap().is_none());
}

#[test]
fn canonical_quote_heavy_answers_fit_retention_and_the_checked_desktop_request() {
    let runtime = owner();
    let mut session = runtime.conversation_session.lock().unwrap();
    for index in 0..2 {
        let admission = session
            .admit(
                &format!("synthetic-answer-{index}"),
                &format!("Synthetic question {index}"),
            )
            .unwrap();
        assert!(matches!(
            session.update(
                admission.execution_id,
                Stage::Instant,
                StageResult::Instant(InstantOutcome::Complete(Content::Text(
                    "\"".repeat(100 * 1024)
                )))
            ),
            UpdateResult::Resolved { .. }
        ));
        assert!(matches!(
            session.settle(admission.execution_id, SettlementIntent::Success),
            SettlementResult::Applied(AppliedOutcome::Completed(_))
        ));
    }
    assert_eq!(session.turns().len(), 2);
    let history = session.provider_history();
    let prompt = voxgolem_platform::custom_openai::CustomOpenAiPrompt {
        session_id: "synthetic-desktop-wire".into(),
        prompt: "Synthetic next question".into(),
        history: custom_history(&history),
    };
    let bytes =
        voxgolem_platform::custom_openai::checked_request(&Default::default(), &prompt, None)
            .unwrap();
    assert!(bytes.len() <= voxgolem_platform::custom_openai::MAX_REQUEST_BYTES);
    let wire: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(wire["input"].as_array().unwrap().len(), 3);
    assert_eq!(
        wire["input"][0]["content"][0]["text"],
        "Synthetic question 1"
    );
    assert_eq!(session.provider_history(), history);
}

#[test]
fn owner_cancel_reset_and_late_stage_preserve_physical_and_canonical_fences() {
    let runtime = owner();
    let execution = runtime
        .conversation_session
        .lock()
        .unwrap()
        .admit("synthetic-owner", "Synthetic current")
        .unwrap()
        .execution_id;
    let (generation, cancelled, signal, _) = register_active_prompt(
        &runtime.active_prompt,
        &runtime.active_prompt_generation,
        "synthetic-owner",
        execution,
        None,
        0,
    )
    .unwrap();
    let control = runtime
        .active_prompt
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .clone();
    let repaired = std::cell::Cell::new(0);
    assert!(runtime
        .cancel_if_active("synthetic-owner", |_| repaired.set(repaired.get() + 1))
        .unwrap());
    assert_eq!(repaired.get(), 1);
    assert!(cancelled.load(Ordering::Acquire));
    assert!(*signal.subscribe().borrow());
    assert_eq!(
        runtime
            .accept_stage(
                "synthetic-owner",
                generation,
                execution,
                Stage::Instant,
                StageResult::Instant(InstantOutcome::Complete(Content::Text(
                    "Synthetic late".into()
                )))
            )
            .unwrap(),
        UpdateResult::IgnoredStale
    );
    assert!(!runtime
        .cancel_if_active("synthetic-owner", |_| panic!("retired repair"))
        .unwrap());
    let (_, guard) = runtime.begin_reset(|| Ok(())).unwrap();
    assert!(runtime.reset_in_progress.load(Ordering::Acquire));
    assert!(matches!(
        runtime.publish_terminal(&control),
        Err(PromptControlError::Cancelled)
    ));
    drop(guard);
    assert!(!runtime.reset_in_progress.load(Ordering::Acquire));
    assert!(runtime
        .conversation_session
        .lock()
        .unwrap()
        .turns()
        .is_empty());
}

#[test]
fn mandatory_unpaired_desktop_projection_is_not_silently_discarded() {
    use voxgolem_core::assistant::{ConversationTurn, Role};
    let history = vec![ConversationTurn {
        role: Role::Assistant,
        content: Content::Refusal("Synthetic orphan refusal".into()),
    }];
    let projected = custom_history(&history);
    assert_eq!(projected.len(), 1);
    assert_eq!(
        projected[0].content_type,
        voxgolem_platform::custom_openai::CustomOpenAiContentType::Refusal
    );
}

#[test]
fn desktop_durable_store_persists_canonical_sources_and_hydrates_on_restart_and_clears_on_reset() {
    let temp = tempfile::tempdir().unwrap();
    let _scope = test_storage::ScopedRoot::new(temp.path().to_path_buf());

    let first_runtime = owner();
    assert!(first_runtime.durable_store.lock().unwrap().store.is_none());

    let execution = first_runtime
        .conversation_session
        .lock()
        .unwrap()
        .admit("synthetic-search-turn", "Synthetic search question")
        .unwrap()
        .execution_id;
    let (active_generation, _, _, _) = register_active_prompt(
        &first_runtime.active_prompt,
        &first_runtime.active_prompt_generation,
        "synthetic-search-turn",
        execution,
        None,
        0,
    )
    .unwrap();
    let control = first_runtime
        .active_prompt
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .clone();

    let sources = vec![voxgolem_core::agent_pipeline::Source {
        url: String::from("https://example.test/source-1"),
        title: String::from("Synthetic Source 1"),
    }];
    assert!(first_runtime
        .observe_sources(
            "synthetic-search-turn",
            active_generation,
            execution,
            &sources,
        )
        .unwrap());
    assert!(matches!(
        first_runtime
            .accept_stage(
                "synthetic-search-turn",
                active_generation,
                execution,
                Stage::Instant,
                StageResult::Instant(InstantOutcome::Complete(Content::Text(String::from(
                    "Synthetic grounded answer."
                )))),
            )
            .unwrap(),
        UpdateResult::Resolved { .. }
    ));
    assert_eq!(
        first_runtime
            .commit(
                "synthetic-search-turn",
                active_generation,
                execution,
                &control,
                || {},
            )
            .unwrap(),
        CommitAssistantResult::Committed
    );

    let first_turns = first_runtime
        .conversation_session
        .lock()
        .unwrap()
        .turns()
        .to_vec();
    assert_eq!(first_turns.len(), 1);
    assert_eq!(
        first_turns[0].answer.content,
        Content::Text(String::from("Synthetic grounded answer."))
    );
    assert_eq!(first_turns[0].sources, sources);

    let restarted_runtime = owner();
    assert!(restarted_runtime
        .durable_store
        .lock()
        .unwrap()
        .store
        .is_none());
    restarted_runtime.ensure_hydrated().unwrap();

    let restored_session = restarted_runtime.conversation_session.lock().unwrap();
    assert_eq!(restored_session.turns(), first_turns.as_slice());
    let provider_history = restored_session.provider_history();
    assert_eq!(provider_history.len(), 2);
    assert_eq!(
        provider_history[1].content,
        Content::Text(String::from(
            "Synthetic grounded answer.\n\nSources:\n- https://example.test/source-1"
        ))
    );
    drop(restored_session);

    assert!(restarted_runtime
        .begin_reset(|| Err(String::from("voice reset failed")))
        .is_err());
    assert_eq!(
        restarted_runtime
            .conversation_session
            .lock()
            .unwrap()
            .turns(),
        first_turns.as_slice()
    );
    let after_failed_reset_runtime = owner();
    after_failed_reset_runtime.ensure_hydrated().unwrap();
    assert_eq!(
        after_failed_reset_runtime
            .conversation_session
            .lock()
            .unwrap()
            .turns(),
        first_turns.as_slice()
    );

    let (_, guard) = restarted_runtime.begin_reset(|| Ok(())).unwrap();
    drop(guard);

    let post_reset_runtime = owner();
    post_reset_runtime.ensure_hydrated().unwrap();
    assert!(post_reset_runtime
        .conversation_session
        .lock()
        .unwrap()
        .turns()
        .is_empty());
}
