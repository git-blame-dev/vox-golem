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

#[test]
fn owner_admission_captures_permission_policy_and_mode_without_root_state() {
    let runtime = owner();
    let config = default_voice_pipeline_config();
    let startup = Arc::new(Mutex::new(StartupStatePayload::Ready {
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
    }));
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
