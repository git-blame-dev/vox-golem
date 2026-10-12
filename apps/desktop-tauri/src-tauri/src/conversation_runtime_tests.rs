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
                startup_state: &startup,
                opencode_server: &server,
                network_preferences: &network,
                voice_pipeline_state: &voice,
                voice_pipeline_config: config,
            },
            "synthetic-admission",
            "Synthetic current",
            CompletionSource::Typed,
            voxgolem_core::search::Mode::Verify,
        )
        .unwrap();
    network.lock().unwrap().web_enabled = false;
    runtime
        .search
        .set_mode(voxgolem_core::search::Mode::Research)
        .unwrap();
    assert!(request.request.web_enabled);
    assert_eq!(
        request.request.search_mode,
        voxgolem_core::search::Mode::Verify
    );
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
