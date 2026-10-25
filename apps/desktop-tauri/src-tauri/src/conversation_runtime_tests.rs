use super::*;
use voxgolem_core::assistant::{
    AssistantPolicy, Content, InstantModel, InstantOutcome, Stage, StageResult,
};
use voxgolem_core::conversation::{
    AppliedOutcome, SettlementIntent, SettlementResult, UpdateResult,
};

#[test]
fn early_cancel_new_chat_invalidates_queued_preparation_before_any_durable_work() {
    let temp = tempfile::tempdir().unwrap();
    let _scope = test_storage::ScopedRoot::new(temp.path().to_path_buf());
    let fixture = NativeAdmissionFixture::new();
    let lease = fixture
        .state
        .reserve_preparation("synthetic-before-new-chat")
        .unwrap();
    let (_, reset_guard) = fixture.state.begin_reset_for(None, || Ok(())).unwrap();
    drop(reset_guard);
    let (snapshot, sequence, path, draft) = {
        let mut durable = fixture.state.durable_store.lock().unwrap();
        let path = durable.store_path().unwrap();
        let store = durable.ensure_store().unwrap();
        (
            store.snapshot().unwrap(),
            allocation_sequence(&path),
            path,
            store.draft(None).unwrap(),
        )
    };
    assert!(fixture
        .state
        .reserve_preparation("synthetic-premature-successor")
        .is_err());
    let result = fixture.state.admit_prepared(
        fixture.state.prompt_admission_resources(),
        "Synthetic old queued prompt",
        CompletionSource::Typed,
        lease,
    );
    assert_cancelled_preparation_payload("synthetic-before-new-chat", result);
    fixture.assert_unadmitted();
    let mut durable = fixture.state.durable_store.lock().unwrap();
    let store = durable.ensure_store().unwrap();
    assert_eq!(store.snapshot().unwrap(), snapshot);
    assert_eq!(store.draft(None).unwrap(), draft);
    assert!(store.selection().unwrap().conversation.is_none());
    assert_eq!(allocation_sequence(&path), sequence);
    drop(durable);
    assert!(fixture.state.preparing_prompt.lock().unwrap().is_none());
    let successor = fixture
        .state
        .admit(
            fixture.state.prompt_admission_resources(),
            "synthetic-after-new-chat",
            "Synthetic new prompt",
            CompletionSource::Typed,
        )
        .unwrap();
    drop(successor);
}

fn allocation_sequence(path: &std::path::Path) -> String {
    let output = std::process::Command::new("python3")
        .args(["-c", "import sqlite3,sys; c=sqlite3.connect(sys.argv[1]); print(c.execute('SELECT last_sequence FROM history_state WHERE singleton=1').fetchone()[0])"])
        .arg(path).output().unwrap();
    assert!(output.status.success());
    String::from_utf8(output.stdout).unwrap()
}

#[test]
fn early_cancel_failed_reset_preserves_queued_preparation_and_explicit_stop() {
    let temp = tempfile::tempdir().unwrap();
    let _scope = test_storage::ScopedRoot::new(temp.path().to_path_buf());
    for fail_selection in [false, true] {
        for stop in [false, true] {
            let fixture = NativeAdmissionFixture::new();
            let path = {
                let mut durable = fixture.state.durable_store.lock().unwrap();
                durable.ensure_store().unwrap();
                durable.store_path().unwrap()
            };
            let lease = fixture
                .state
                .reserve_preparation("synthetic-failed-reset")
                .unwrap();
            let epoch = fixture
                .state
                .prompt_publication_epoch
                .load(Ordering::SeqCst);
            let wrong = voxgolem_core::ipc::ExecutionId {
                session_id: 1.into(),
                generation: 2.into(),
            };
            if fail_selection {
                synthetic_sql(&path, "CREATE TRIGGER synthetic_reject_selection BEFORE UPDATE ON history_local BEGIN SELECT RAISE(ABORT, 'synthetic selection failure'); END");
            }
            let reset = fixture
                .state
                .begin_reset_for((!fail_selection).then_some(&wrong), || Ok(()));
            if fail_selection {
                synthetic_sql(&path, "DROP TRIGGER synthetic_reject_selection");
            }
            assert!(reset.is_err());
            assert_eq!(
                fixture
                    .state
                    .prompt_publication_epoch
                    .load(Ordering::SeqCst),
                epoch
            );
            assert!(!lease.ticket.cancelled.load(Ordering::Acquire));
            if stop {
                assert!(fixture
                    .state
                    .cancel_if_active("synthetic-failed-reset", |_| {})
                    .unwrap());
            }
            let result = fixture.state.admit_prepared(
                fixture.state.prompt_admission_resources(),
                "Synthetic legitimate queued prompt",
                CompletionSource::Typed,
                lease,
            );
            if stop {
                assert_cancelled_preparation_payload("synthetic-failed-reset", result);
            } else {
                drop(result.unwrap());
            }
            fixture.assert_unadmitted();
        }
    }
}

fn synthetic_sql(path: &std::path::Path, sql: &str) {
    let output = std::process::Command::new("python3")
        .args(["-c", "import sqlite3,sys; c=sqlite3.connect(sys.argv[1]); c.execute(sys.argv[2]); c.commit()"])
        .arg(path).arg(sql).output().unwrap();
    assert!(output.status.success(), "synthetic SQL setup failed");
}

#[test]
fn early_cancel_queued_worker_is_visible_before_worker_entry() {
    let temp = tempfile::tempdir().unwrap();
    let _scope = test_storage::ScopedRoot::new(temp.path().to_path_buf());
    let fixture = NativeAdmissionFixture::new();
    // Model the production blocking queue: the closure exists but has not run.
    let preparation = fixture
        .state
        .reserve_preparation("synthetic-queued")
        .unwrap();
    let queued = || {
        fixture.state.admit_prepared(
            fixture.state.prompt_admission_resources(),
            "Synthetic queued draft",
            CompletionSource::Typed,
            preparation,
        )
    };
    let cancelled = fixture
        .state
        .cancel_if_active("synthetic-queued", |_| {})
        .unwrap();
    let result = queued();
    assert!(cancelled, "Stop lost before blocking worker entry");
    assert_cancelled_preparation_payload("synthetic-queued", result);
}

fn assert_cancelled_preparation_payload(
    request_id: &str,
    result: Result<RegisteredAssistantRequest, PreparationError>,
) {
    let error = match result {
        Err(PreparationError::Cancelled) => PreparationError::Cancelled,
        Err(PreparationError::Failed(error)) => panic!("cancel became command error: {error}"),
        Ok(_) => panic!("cancelled preparation registered an execution"),
    };
    // Exercise the exact final-payload conversion used by the dispatcher.
    let payload =
        preparation_failure_payload(request_id, error, Ok(RuntimePhasePayload::Sleeping)).unwrap();
    let json = serde_json::to_value(payload).unwrap();
    assert_eq!(json["kind"], "cancelled");
    assert_eq!(json["request_id"], request_id);
    assert_eq!(
        json["runtime_phase"],
        serde_json::to_value(RuntimePhasePayload::Sleeping).unwrap()
    );
    assert_eq!(
        json.as_object().unwrap().len(),
        3,
        "must not invent execution/answer/disposition"
    );
}

#[test]
fn early_cancel_arbitrates_store_open_failure_without_hiding_uncancelled_errors() {
    let temp = tempfile::tempdir().unwrap();
    let _scope = test_storage::ScopedRoot::new(temp.path().to_path_buf());
    let fixture = NativeAdmissionFixture::new();
    fixture.state.durable_store.lock().unwrap().open_failed = true;
    for cancel in [false, true] {
        let lease = fixture
            .state
            .reserve_preparation("synthetic-open-failure")
            .unwrap();
        if cancel {
            assert!(fixture
                .state
                .cancel_if_active("synthetic-open-failure", |_| {})
                .unwrap());
        }
        let result = fixture.state.admit_prepared(
            fixture.state.prompt_admission_resources(),
            "Synthetic unavailable store",
            CompletionSource::Typed,
            lease,
        );
        if cancel {
            assert_cancelled_preparation_payload("synthetic-open-failure", result);
        } else {
            let error = match result {
                Err(error @ PreparationError::Failed(_)) => error,
                _ => panic!("uncancelled store failure lost its error"),
            };
            let payload = preparation_failure_payload(
                "synthetic-open-failure",
                error,
                Ok(RuntimePhasePayload::Sleeping),
            );
            assert!(
                matches!(payload, Err(ref error) if error == "durable conversation store failed to open")
            );
        }
        assert!(fixture.state.preparing_prompt.lock().unwrap().is_none());
    }
    fixture.assert_unadmitted();
}

#[test]
fn early_cancel_queued_closure_drop_releases_only_its_own_lease() {
    let fixture = NativeAdmissionFixture::new();
    let lease = fixture
        .state
        .reserve_preparation("synthetic-abandoned-queue")
        .unwrap();
    let queued = || {
        fixture.state.admit_prepared(
            fixture.state.prompt_admission_resources(),
            "Synthetic never executed",
            CompletionSource::Typed,
            lease,
        )
    };
    assert!(fixture
        .state
        .cancel_if_active("synthetic-abandoned-queue", |_| {})
        .unwrap());
    drop(queued);
    assert!(fixture.state.preparing_prompt.lock().unwrap().is_none());

    // The retired worker/closure may be dropped after a successor reserves.
    // Pointer identity prevents its destructor from clearing that successor.
    let retired = fixture
        .state
        .reserve_preparation("synthetic-reused-queue")
        .unwrap();
    retired.retire();
    let successor = fixture
        .state
        .reserve_preparation("synthetic-reused-queue")
        .unwrap();
    drop(retired);
    assert!(Arc::ptr_eq(
        fixture
            .state
            .preparing_prompt
            .lock()
            .unwrap()
            .as_ref()
            .unwrap(),
        &successor.ticket
    ));
    drop(successor);
    assert!(fixture.state.preparing_prompt.lock().unwrap().is_none());
}

#[test]
fn early_cancel_dropped_waiter_does_not_retire_the_running_worker_lease() {
    let temp = tempfile::tempdir().unwrap();
    let _scope = test_storage::ScopedRoot::new(temp.path().to_path_buf());
    let fixture = NativeAdmissionFixture::new();
    let reached = Arc::new(std::sync::Barrier::new(2));
    let release = Arc::new(std::sync::Barrier::new(2));
    let (entered, resume) = (reached.clone(), release.clone());
    *fixture.state.preparation_probe.lock().unwrap() = Some(Arc::new(move |stage| {
        if stage == "before_store" {
            entered.wait();
            resume.wait();
        }
    }));
    let lease = fixture
        .state
        .reserve_preparation("synthetic-dropped-waiter")
        .unwrap();
    let (result_tx, result_rx) = std::sync::mpsc::channel();
    std::thread::scope(|scope| {
        let worker = scope.spawn(|| {
            let result = fixture.state.admit_prepared(
                fixture.state.prompt_admission_resources(),
                "Synthetic detached work",
                CompletionSource::Typed,
                lease,
            );
            // Failed delivery drops either the typed cancellation or the
            // registered result/active guard on this worker, not on the waiter.
            let _ = result_tx.send(result);
        });
        reached.wait();
        drop(result_rx);
        assert!(fixture
            .state
            .reserve_preparation("synthetic-successor")
            .is_err());
        assert!(fixture
            .state
            .cancel_if_active("synthetic-dropped-waiter", |_| {})
            .unwrap());
        assert!(fixture.state.preparing_prompt.lock().unwrap().is_some());
        release.wait();
        worker.join().unwrap();
    });
    assert!(fixture.state.preparing_prompt.lock().unwrap().is_none());
    fixture.assert_unadmitted();
}

#[test]
fn early_cancel_after_atomic_handoff_targets_active_not_preparing() {
    let temp = tempfile::tempdir().unwrap();
    let _scope = test_storage::ScopedRoot::new(temp.path().to_path_buf());
    let fixture = NativeAdmissionFixture::new();
    let lease = fixture
        .state
        .reserve_preparation("synthetic-handoff")
        .unwrap();
    let registered = fixture
        .state
        .admit_prepared(
            fixture.state.prompt_admission_resources(),
            "Synthetic handed off",
            CompletionSource::Typed,
            lease,
        )
        .unwrap();
    assert!(fixture.state.preparing_prompt.lock().unwrap().is_none());
    assert!(fixture
        .state
        .cancel_if_active("synthetic-handoff", |_| {})
        .unwrap());
    assert!(registered.cancelled.load(Ordering::Acquire));
    drop(registered);
    fixture.assert_unadmitted();
}

#[test]
fn early_cancel_before_store_wait_prevents_registration() {
    let temp = tempfile::tempdir().unwrap();
    let _scope = test_storage::ScopedRoot::new(temp.path().to_path_buf());
    let fixture = NativeAdmissionFixture::new();
    let reached = Arc::new(std::sync::Barrier::new(2));
    let release = Arc::new(std::sync::Barrier::new(2));
    let (entered, resume) = (reached.clone(), release.clone());
    *fixture.state.preparation_probe.lock().unwrap() = Some(Arc::new(move |stage| {
        if stage == "before_store" {
            entered.wait();
            resume.wait();
        }
    }));
    std::thread::scope(|scope| {
        let preparation = scope.spawn(|| {
            fixture.state.admit(
                fixture.state.prompt_admission_resources(),
                "synthetic-preparing",
                "Synthetic pending draft",
                CompletionSource::Typed,
            )
        });
        reached.wait();
        let cancelled = fixture
            .state
            .cancel_if_active("synthetic-preparing", |_| {});
        release.wait();
        let result = preparation.join().unwrap();
        assert!(cancelled.unwrap());
        assert!(result.is_err());
    });
    fixture.assert_unadmitted();
}

#[test]
fn early_cancel_during_sqlite_preparation_keeps_draft_without_dispatch() {
    check_cancelled_sqlite_preparation(true);
}

#[test]
fn early_cancel_wins_even_when_blocked_sqlite_preparation_times_out() {
    check_cancelled_sqlite_preparation(false);
}

fn check_cancelled_sqlite_preparation(allow_prepare_to_finish: bool) {
    use std::io::{BufRead, Write};
    let temp = tempfile::tempdir().unwrap();
    let _scope = test_storage::ScopedRoot::new(temp.path().to_path_buf());
    let fixture = NativeAdmissionFixture::new();
    let path = {
        let mut durable = fixture.state.durable_store.lock().unwrap();
        durable.ensure_store().unwrap();
        durable.store_path().unwrap()
    };
    // Independent connection holds the actual SQLite writer lock, not a mock
    // store. The worker must finish its atomic preparation after cancellation.
    let mut blocker = std::process::Command::new("python3")
        .args(["-u", "-c", "import sqlite3,sys; c=sqlite3.connect(sys.argv[1]); c.execute('BEGIN IMMEDIATE'); print('locked'); sys.stdin.readline(); c.rollback()"])
        .arg(path).stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped()).spawn().unwrap();
    let mut line = String::new();
    std::io::BufReader::new(blocker.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    assert_eq!(line.trim(), "locked");
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    *fixture.state.preparation_probe.lock().unwrap() = Some(Arc::new(move |stage| {
        if stage == "before_prepare" {
            entered_tx.send(()).unwrap();
        }
    }));
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let lease = fixture
        .state
        .reserve_preparation("synthetic-sqlite")
        .unwrap();
    std::thread::scope(|scope| {
        let preparation = scope.spawn(|| {
            let result = fixture.state.admit_prepared(
                fixture.state.prompt_admission_resources(),
                "Synthetic blocked draft",
                CompletionSource::Typed,
                lease,
            );
            done_tx.send(()).unwrap();
            result
        });
        entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(done_rx.recv_timeout(Duration::from_millis(100)).is_err());
        assert!(fixture.state.active_prompt.lock().unwrap().is_none());
        let unrelated = fixture.state.cancel_if_active("synthetic-other", |_| {});
        let cancelled = fixture.state.cancel_if_active("synthetic-sqlite", |_| {});
        if !allow_prepare_to_finish {
            // The real Store's busy timeout fails preparation while this
            // independent connection still owns the SQLite writer lock.
            done_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        }
        // Release SQL even if a cancellation assertion fails.
        blocker
            .stdin
            .take()
            .unwrap()
            .write_all(b"release\n")
            .unwrap();
        assert!(blocker.wait().unwrap().success());
        let result = preparation.join().unwrap();
        assert!(!unrelated.unwrap());
        assert!(cancelled.unwrap());
        assert_cancelled_preparation_payload("synthetic-sqlite", result);
    });
    fixture.assert_unadmitted();
    assert!(fixture.state.preparing_prompt.lock().unwrap().is_none());
    let mut durable = fixture.state.durable_store.lock().unwrap();
    let store = durable.ensure_store().unwrap();
    let selection = store.selection().unwrap();
    if allow_prepare_to_finish {
        assert!(selection.conversation.is_some());
        assert_eq!(
            store.draft(selection.conversation.as_ref()).unwrap().text,
            "Synthetic blocked draft"
        );
    } else {
        assert!(selection.conversation.is_none());
        assert!(store.draft(None).unwrap().text.is_empty());
    }
}

#[test]
fn early_cancel_slot_is_bounded_and_reusable_only_after_worker_finishes() {
    let temp = tempfile::tempdir().unwrap();
    let _scope = test_storage::ScopedRoot::new(temp.path().to_path_buf());
    let fixture = NativeAdmissionFixture::new();
    let reached = Arc::new(std::sync::Barrier::new(2));
    let release = Arc::new(std::sync::Barrier::new(2));
    let (entered, resume) = (reached.clone(), release.clone());
    *fixture.state.preparation_probe.lock().unwrap() = Some(Arc::new(move |stage| {
        if stage == "before_store" {
            entered.wait();
            resume.wait();
        }
    }));
    std::thread::scope(|scope| {
        let preparation = scope.spawn(|| {
            fixture.state.admit(
                fixture.state.prompt_admission_resources(),
                "synthetic-reused",
                "Synthetic cancelled draft",
                CompletionSource::Typed,
            )
        });
        reached.wait();
        assert!(!fixture
            .state
            .cancel_if_active("synthetic-unrelated", |_| {})
            .unwrap());
        assert!(fixture
            .state
            .cancel_if_active("synthetic-reused", |_| {})
            .unwrap());
        let busy = fixture.state.admit(
            fixture.state.prompt_admission_resources(),
            "synthetic-reused",
            "Synthetic replacement",
            CompletionSource::Typed,
        );
        assert!(matches!(busy, Err(ref error) if error == "conversation preparation is busy"));
        release.wait();
        assert!(preparation.join().unwrap().is_err());
    });
    *fixture.state.preparation_probe.lock().unwrap() = None;
    let replacement = fixture
        .state
        .admit(
            fixture.state.prompt_admission_resources(),
            "synthetic-reused",
            "Synthetic replacement",
            CompletionSource::Typed,
        )
        .unwrap();
    assert!(fixture.state.preparing_prompt.lock().unwrap().is_none());
    drop(replacement);
    fixture.assert_unadmitted();
}

#[test]
fn early_cancel_unrelated_and_stale_durable_targets_do_not_invalidate_replacement() {
    let temp = tempfile::tempdir().unwrap();
    let _scope = test_storage::ScopedRoot::new(temp.path().to_path_buf());
    let fixture = NativeAdmissionFixture::new();
    let first = fixture
        .state
        .admit(
            fixture.state.prompt_admission_resources(),
            "synthetic-alias",
            "Synthetic first",
            CompletionSource::Typed,
        )
        .unwrap();
    let old_wire = first
        .control
        .pinned
        .as_ref()
        .unwrap()
        .wire_execution
        .clone();
    drop(first);
    let reached = Arc::new(std::sync::Barrier::new(2));
    let release = Arc::new(std::sync::Barrier::new(2));
    let (entered, resume) = (reached.clone(), release.clone());
    *fixture.state.preparation_probe.lock().unwrap() = Some(Arc::new(move |stage| {
        if stage == "before_store" {
            entered.wait();
            resume.wait();
        }
    }));
    std::thread::scope(|scope| {
        let preparation = scope.spawn(|| {
            fixture.state.admit(
                fixture.state.prompt_admission_resources(),
                "synthetic-alias",
                "Synthetic second",
                CompletionSource::Typed,
            )
        });
        reached.wait();
        // This is the native command's Some(expected) lookup; None returns false
        // without ever invoking correlation-only cancellation.
        assert!(fixture
            .state
            .execution_for_cancel("synthetic-alias", &old_wire)
            .is_none());
        assert!(!fixture
            .state
            .cancel_if_active("synthetic-unrelated", |_| {})
            .unwrap());
        release.wait();
        let replacement = preparation.join().unwrap().unwrap();
        assert!(fixture
            .state
            .execution_for_cancel("synthetic-alias", &old_wire)
            .is_none());
        assert_ne!(
            replacement.control.pinned.as_ref().unwrap().wire_execution,
            old_wire
        );
        drop(replacement);
    });
    fixture.assert_unadmitted();
    assert!(fixture.state.preparing_prompt.lock().unwrap().is_none());
}

#[test]
fn early_cancel_failed_admission_releases_preparing_slot() {
    let temp = tempfile::tempdir().unwrap();
    let _scope = test_storage::ScopedRoot::new(temp.path().to_path_buf());
    let fixture = NativeAdmissionFixture::new();
    fixture
        .state
        .exit_cleanup_started
        .store(true, Ordering::Release);
    assert!(fixture
        .state
        .admit(
            fixture.state.prompt_admission_resources(),
            "synthetic-failed",
            "Synthetic failed",
            CompletionSource::Typed
        )
        .is_err());
    assert!(fixture.state.preparing_prompt.lock().unwrap().is_none());
    fixture
        .state
        .exit_cleanup_started
        .store(false, Ordering::Release);
    let registered = fixture
        .state
        .admit(
            fixture.state.prompt_admission_resources(),
            "synthetic-failed",
            "Synthetic recovered",
            CompletionSource::Typed,
        )
        .unwrap();
    drop(registered);
    fixture.assert_unadmitted();
}

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
fn new_chat_retains_saved_conversations_and_their_drafts() {
    let runtime = owner();
    let (conversation, before) = {
        let mut durable = runtime.durable_store.lock().unwrap();
        let store = durable.ensure_store().unwrap();
        let conversation = store
            .create_conversation("Synthetic retained chat".into())
            .unwrap()
            .operation
            .id;
        store
            .select(voxgolem_conversation_store::Selection {
                conversation: Some(conversation.clone()),
                selected_head: None,
            })
            .unwrap();
        store
            .save_draft(Some(&conversation), "Synthetic retained draft", true)
            .unwrap();
        (conversation, store.snapshot().unwrap())
    };
    let (_, reset_guard) = runtime.begin_reset(|| Ok(())).unwrap();
    drop(reset_guard);
    let mut durable = runtime.durable_store.lock().unwrap();
    let store = durable.ensure_store().unwrap();
    assert_eq!(store.snapshot().unwrap(), before);
    assert!(store.selection().unwrap().conversation.is_none());
    assert_eq!(
        store.draft(Some(&conversation)).unwrap().text,
        "Synthetic retained draft"
    );
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
        state.telemetry_sink = new_telemetry_sink(
            voxgolem_core::config::TelemetryConfig {
                enabled: false,
                max_bytes: 2048,
                backup_count: 0,
            },
            false,
        );
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
fn preparation_failure_admits_no_typed_or_native_provider_work_and_preserves_draft() {
    let fixture = NativeAdmissionFixture::new();
    {
        let mut durable = fixture.state.durable_store.lock().unwrap();
        durable
            .ensure_store()
            .unwrap()
            .save_draft(None, "Synthetic pending draft", true)
            .unwrap();
        durable.open_failed = true;
    }
    let mut provider_calls = 0;
    for native in [false, true] {
        let resources = if native {
            fixture.resources()
        } else {
            fixture.state.prompt_admission_resources()
        };
        if let Ok(_request) = fixture.state.admit(
            resources,
            "synthetic-failed-admission",
            "Synthetic pending draft",
            CompletionSource::Typed,
        ) {
            provider_calls += 1;
        }
    }
    assert_eq!(provider_calls, 0);
    fixture.assert_unadmitted();
    let mut durable = fixture.state.durable_store.lock().unwrap();
    durable.open_failed = false;
    assert_eq!(
        durable.ensure_store().unwrap().draft(None).unwrap().text,
        "Synthetic pending draft"
    );
}

#[test]
fn admission_rejects_blank_correlation_before_durable_preparation() {
    for invalid in [" ", "\t", "\r\n", "\u{2003}"] {
        let fixture = NativeAdmissionFixture::new();
        let before = {
            let mut durable = fixture.state.durable_store.lock().unwrap();
            let store = durable.ensure_store().unwrap();
            store
                .save_draft(None, "Synthetic pending draft", true)
                .unwrap();
            assert_eq!(store.allocate_id().unwrap().sequence, "1");
            (store.snapshot().unwrap(), store.selection().unwrap())
        };
        let admitted = fixture.state.admit(
            fixture.resources(),
            invalid,
            "Synthetic pending draft",
            CompletionSource::Voice,
        );
        assert!(
            admitted.is_err(),
            "blank correlation must reject before preparation or provider dispatch"
        );
        fixture.assert_unadmitted();
        let mut durable = fixture.state.durable_store.lock().unwrap();
        let store = durable.ensure_store().unwrap();
        assert_eq!(store.snapshot().unwrap(), before.0);
        assert_eq!(store.selection().unwrap(), before.1);
        assert_eq!(store.draft(None).unwrap().text, "Synthetic pending draft");
        assert_eq!(store.allocate_id().unwrap().sequence, "2");
    }
}

#[test]
fn durable_completion_pins_wire_identity_and_admits_a_followup_from_the_saved_head() {
    let fixture = NativeAdmissionFixture::new();
    let request = fixture
        .state
        .admit(
            fixture.state.prompt_admission_resources(),
            "synthetic-durable-first",
            "Synthetic first prompt",
            CompletionSource::Typed,
        )
        .unwrap();
    let pinned = Arc::clone(request.control.pinned.as_ref().unwrap());
    fixture
        .state
        .accept_stage(
            "synthetic-durable-first",
            request.physical_generation,
            request.request.generation,
            Stage::Instant,
            StageResult::Instant(InstantOutcome::Complete(Content::Text(
                "Synthetic first answer".into(),
            ))),
        )
        .unwrap();
    assert_eq!(
        commit_assistant_request_if_active(
            &fixture.state,
            "synthetic-durable-first",
            request.physical_generation,
            request.request.generation,
            &request.control
        )
        .unwrap(),
        CommitAssistantResult::Committed
    );
    request.guard.finish();
    fixture.state.durable_store.lock().unwrap().open_failed = true;
    assert!(get_app_snapshot_from_state(&fixture.state).is_err());
    assert_eq!(
        cancel_assistant_request(&fixture.state, request.request.generation),
        SettlementResult::IgnoredStale
    );
    assert_eq!(
        commit_assistant_request_if_active(
            &fixture.state,
            "synthetic-durable-first",
            request.physical_generation,
            request.request.generation,
            &request.control
        )
        .unwrap(),
        CommitAssistantResult::Committed
    );
    let payload = settled_prompt_payload(
        "synthetic-durable-first",
        RuntimePhasePayload::Sleeping,
        &request.control,
    )
    .unwrap();
    let serialized = serde_json::to_value(payload).unwrap();
    assert_eq!(serialized["save_disposition"], "saved");
    fixture.state.durable_store.lock().unwrap().open_failed = false;
    assert_eq!(
        serialized["turn"]["execution_id"]["generation"],
        pinned.wire_execution.generation.as_str()
    );
    assert!(!fixture
        .state
        .conversation_session
        .lock()
        .unwrap()
        .has_active_execution());
    let followup = fixture.state.admit(
        fixture.state.prompt_admission_resources(),
        "synthetic-durable-next",
        "Synthetic followup",
        CompletionSource::Typed,
    );
    assert!(
        followup.is_ok(),
        "saved completion must atomically advance the local selected head: {:?}",
        followup.err()
    );
}

#[test]
fn suppressed_completion_is_unsaved_blocks_all_admission_and_requires_targeted_successful_selection(
) {
    let fixture = NativeAdmissionFixture::new();
    let request = fixture
        .state
        .admit(
            fixture.state.prompt_admission_resources(),
            "synthetic-unsaved",
            "Synthetic unsaved prompt",
            CompletionSource::Typed,
        )
        .unwrap();
    let pinned = Arc::clone(request.control.pinned.as_ref().unwrap());
    fixture
        .state
        .accept_stage(
            "synthetic-unsaved",
            request.physical_generation,
            request.request.generation,
            Stage::Instant,
            StageResult::Instant(InstantOutcome::Complete(Content::Text(
                "Synthetic unsaved answer".into(),
            ))),
        )
        .unwrap();
    fixture
        .state
        .durable_store
        .lock()
        .unwrap()
        .ensure_store()
        .unwrap()
        .delete_conversation(pinned.conversation.clone())
        .unwrap();
    assert_eq!(
        commit_assistant_request_if_active(
            &fixture.state,
            "synthetic-unsaved",
            request.physical_generation,
            request.request.generation,
            &request.control
        )
        .unwrap(),
        CommitAssistantResult::Unsaved
    );
    request.guard.finish();
    let payload = settled_prompt_payload(
        "synthetic-unsaved",
        RuntimePhasePayload::Sleeping,
        &request.control,
    )
    .unwrap();
    let value = serde_json::to_value(payload).unwrap();
    assert_eq!(value["kind"], "unsaved");
    assert_eq!(value["unsaved_result"]["reason"], "context_unavailable");
    assert!(value.get("turn").is_none());
    assert!(!fixture
        .state
        .conversation_session
        .lock()
        .unwrap()
        .has_active_execution());
    assert!(fixture.state.begin_reset(|| Ok(())).is_err());
    let (context, history) = {
        let mut durable = fixture.state.durable_store.lock().unwrap();
        read_prefetch_context(durable.ensure_store().unwrap(), "Synthetic followup").unwrap()
    };
    let key = PrefetchKey {
        context,
        history,
        prompt: "Synthetic followup".into(),
        model: InstantModel::CustomSolHigh,
    };
    assert!(!prefetch_result_is_current(
        &fixture.state,
        &key,
        0,
        0,
        &AtomicBool::new(false)
    ));
    assert!(fixture
        .state
        .admit(
            fixture.state.prompt_admission_resources(),
            "synthetic-blocked",
            "Synthetic followup",
            CompletionSource::Typed
        )
        .is_err());
    let wrong = voxgolem_core::ipc::ExecutionId {
        session_id: 1.into(),
        generation: 2.into(),
    };
    let queued = fixture
        .state
        .reserve_preparation("synthetic-queued-discard")
        .unwrap();
    assert!(fixture.state.begin_reset(|| Ok(())).is_err());
    assert!(!queued.ticket.cancelled.load(Ordering::Acquire));
    assert!(fixture
        .state
        .begin_reset_for(Some(&wrong), || Ok(()))
        .is_err());
    assert!(!queued.ticket.cancelled.load(Ordering::Acquire));
    fixture.state.durable_store.lock().unwrap().open_failed = true;
    assert!(fixture
        .state
        .begin_reset_for(Some(&pinned.wire_execution), || Ok(()))
        .is_err());
    assert!(!queued.ticket.cancelled.load(Ordering::Acquire));
    assert!(fixture.state.unsaved_result.lock().unwrap().is_some());
    assert!(fixture
        .state
        .conversation_session
        .lock()
        .unwrap()
        .unresolved_save()
        .is_some());
    fixture.state.durable_store.lock().unwrap().open_failed = false;
    let (_, guard) = fixture
        .state
        .begin_reset_for(Some(&pinned.wire_execution), || Ok(()))
        .unwrap();
    drop(guard);
    assert!(queued.ticket.cancelled.load(Ordering::Acquire));
    let after_reset = fixture
        .state
        .durable_store
        .lock()
        .unwrap()
        .ensure_store()
        .unwrap()
        .snapshot()
        .unwrap();
    let result = fixture.state.admit_prepared(
        fixture.state.prompt_admission_resources(),
        "Synthetic old prompt before discard",
        CompletionSource::Typed,
        queued,
    );
    assert_cancelled_preparation_payload("synthetic-queued-discard", result);
    assert_eq!(
        fixture
            .state
            .durable_store
            .lock()
            .unwrap()
            .ensure_store()
            .unwrap()
            .snapshot()
            .unwrap(),
        after_reset
    );
    assert!(fixture.state.unsaved_result.lock().unwrap().is_none());
    assert!(fixture
        .state
        .conversation_session
        .lock()
        .unwrap()
        .unresolved_save()
        .is_none());
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
fn real_store_quota_failure_preserves_canonical_history_and_installs_one_unsaved_payload() {
    use voxgolem_core::durable_conversation::{Answer, OperationBody, Provenance, TerminalStage};
    let fixture = NativeAdmissionFixture::new();
    let request = fixture
        .state
        .admit(
            fixture.state.prompt_admission_resources(),
            "synthetic-quota-request",
            "Synthetic unsaved quota prompt",
            CompletionSource::Typed,
        )
        .unwrap();
    fixture
        .state
        .accept_stage(
            "synthetic-quota-request",
            request.physical_generation,
            request.request.generation,
            Stage::Instant,
            StageResult::Instant(InstantOutcome::Complete(Content::Text(
                "x".repeat(255 * 1024),
            ))),
        )
        .unwrap();
    let before = {
        let mut durable = fixture.state.durable_store.lock().unwrap();
        let store = durable.ensure_store().unwrap();
        let fill_bytes = 240 * 1024;
        let mut quota_reached = false;
        for index in 0..(voxgolem_conversation_store::MAX_CANONICAL_BYTES / fill_bytes + 4) {
            let conversation = store
                .create_conversation(format!("Synthetic quota owner {index}"))
                .unwrap()
                .operation
                .id;
            let request = store.allocate_id().unwrap();
            let body = OperationBody::TurnCompleted {
                conversation,
                parent: None,
                request,
                prompt: "Synthetic fill".into(),
                answer: Answer::Text {
                    text: "x".repeat(fill_bytes),
                },
                sources: vec![],
                terminal_stage: TerminalStage::Instant,
                provenance: Provenance {
                    requested_model: "synthetic-model".into(),
                    returned_model: None,
                    policy_id: "synthetic-policy".into(),
                    policy_version: "1".into(),
                    instruction_profile_id: "synthetic-instructions".into(),
                    instruction_profile_version: "1".into(),
                    context: vec![],
                },
            };
            match store.complete_body(body) {
                Ok(completed) => assert!(completed.saved),
                Err(voxgolem_conversation_store::Error::Domain(
                    voxgolem_core::durable_conversation::DomainError::LimitExceeded { limit },
                )) if limit == "native_snapshot_bytes" => {
                    quota_reached = true;
                    break;
                }
                Err(error) => panic!("unexpected synthetic fill failure: {error}"),
            }
        }
        assert!(
            quota_reached,
            "bounded fixture must reach the actual store quota"
        );
        store.snapshot().unwrap()
    };
    assert_eq!(
        commit_assistant_request_if_active(
            &fixture.state,
            "synthetic-quota-request",
            request.physical_generation,
            request.request.generation,
            &request.control
        )
        .unwrap(),
        CommitAssistantResult::Unsaved
    );
    request.guard.finish();
    let payload = settled_prompt_payload(
        "synthetic-quota-request",
        RuntimePhasePayload::Sleeping,
        &request.control,
    )
    .unwrap();
    let voxgolem_core::ipc::PromptOutcome::Unsaved { unsaved_result, .. } = payload.outcome else {
        panic!("failed storage must not produce a saved turn");
    };
    assert_eq!(
        unsaved_result.reason(),
        voxgolem_core::ipc::UnsavedReason::StorageError
    );
    assert!(Arc::ptr_eq(
        &unsaved_result,
        fixture
            .state
            .unsaved_result
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
    ));
    assert_eq!(
        fixture
            .state
            .durable_store
            .lock()
            .unwrap()
            .ensure_store()
            .unwrap()
            .snapshot()
            .unwrap(),
        before
    );
    assert!(!fixture
        .state
        .conversation_session
        .lock()
        .unwrap()
        .has_active_execution());
    assert!(fixture
        .state
        .admit(
            fixture.resources(),
            "synthetic-blocked-voice",
            "Synthetic next prompt",
            CompletionSource::Voice
        )
        .is_err());
    assert!(fixture.state.begin_reset(|| Ok(())).is_err());
}

#[test]
fn valid_native_voice_admission_preserves_policy_history_source_and_native_ownership() {
    use voxgolem_core::durable_conversation::{Answer, OperationBody, Provenance, TerminalStage};
    let temp = tempfile::tempdir().unwrap();
    let _scope = test_storage::ScopedRoot::new(temp.path().to_path_buf());
    let fixture = NativeAdmissionFixture::new();
    let history = {
        let mut durable = fixture.state.durable_store.lock().unwrap();
        let store = durable.ensure_store().unwrap();
        let selected = store.selection().unwrap();
        let prepared = store
            .prepare_request(
                selected,
                "Synthetic prior question",
                voxgolem_core::conversation::MAX_COMMITTED_TEXT_BYTES,
                true,
            )
            .unwrap();
        assert!(
            store
                .complete_selected_body_with_authority(
                    OperationBody::TurnCompleted {
                        conversation: prepared.context.selection.conversation,
                        parent: prepared.context.selection.parent,
                        request: prepared.request,
                        prompt: "Synthetic prior question".into(),
                        answer: Answer::Text {
                            text: "Synthetic prior answer".into(),
                        },
                        sources: vec![],
                        terminal_stage: TerminalStage::Instant,
                        provenance: Provenance {
                            requested_model: "synthetic-model".into(),
                            returned_model: None,
                            policy_id: "synthetic-policy".into(),
                            policy_version: "1".into(),
                            instruction_profile_id: "synthetic-instructions".into(),
                            instruction_profile_version: "1".into(),
                            context: prepared.context.selection.ancestor_ids,
                        },
                    },
                    || (
                        (),
                        voxgolem_conversation_store::CompletionDecision::Authorized
                    ),
                    |_, attempt| match attempt {
                        voxgolem_conversation_store::CompletionAttempt::Finished(result) => result,
                        _ => unreachable!(),
                    },
                )
                .unwrap()
                .saved
        );
        read_prefetch_context(store, "Synthetic voice question")
            .unwrap()
            .1
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
    let update_installation_gate = Arc::new(UpdateActivity::default());
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
                update_installation_gate: &update_installation_gate,
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
    use voxgolem_core::durable_conversation::{Answer, OperationBody, Provenance, TerminalStage};
    let temp = tempfile::tempdir().unwrap();
    let mut store =
        voxgolem_conversation_store::Store::open(&temp.path().join("conversations.sqlite"))
            .unwrap();
    let mut session =
        voxgolem_core::conversation::ConversationSession::execution_only(AssistantPolicy {
            deep_enabled: false,
            review_enabled: false,
            ..Default::default()
        });
    for index in 0..2 {
        let prompt = format!("Synthetic question {index}");
        let selected = store.selection().unwrap();
        let prepared = store
            .prepare_request(
                selected,
                &prompt,
                voxgolem_core::conversation::MAX_COMMITTED_TEXT_BYTES,
                true,
            )
            .unwrap();
        let admission = session
            .admit_with_context(
                &format!("synthetic-answer-{index}"),
                &prompt,
                provider_turns(prepared.context.provider_messages),
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
        assert!(
            store
                .complete_selected_body_with_authority(
                    OperationBody::TurnCompleted {
                        conversation: prepared.context.selection.conversation,
                        parent: prepared.context.selection.parent,
                        request: prepared.request,
                        prompt,
                        answer: Answer::Text {
                            text: "\"".repeat(100 * 1024),
                        },
                        sources: vec![],
                        terminal_stage: TerminalStage::Instant,
                        provenance: Provenance {
                            requested_model: "synthetic-model".into(),
                            returned_model: None,
                            policy_id: "synthetic-policy".into(),
                            policy_version: "1".into(),
                            instruction_profile_id: "synthetic-instructions".into(),
                            instruction_profile_version: "1".into(),
                            context: prepared.context.selection.ancestor_ids,
                        },
                    },
                    || (
                        (),
                        voxgolem_conversation_store::CompletionDecision::Authorized
                    ),
                    |_, attempt| match attempt {
                        voxgolem_conversation_store::CompletionAttempt::Finished(result) => result,
                        _ => unreachable!(),
                    },
                )
                .unwrap()
                .saved
        );
    }
    let selected = store.selection().unwrap();
    let prepared = store
        .prepare_request(
            selected,
            "Synthetic next question",
            voxgolem_core::conversation::MAX_COMMITTED_TEXT_BYTES,
            true,
        )
        .unwrap();
    assert_eq!(prepared.context.selection.turns.len(), 2);
    let history = provider_turns(prepared.context.provider_messages);
    let admitted = session
        .admit_with_context(
            "synthetic-answer-2",
            "Synthetic next question",
            history.clone(),
        )
        .unwrap();
    let prompt = voxgolem_platform::custom_openai::CustomOpenAiPrompt {
        session_id: "synthetic-desktop-wire".into(),
        prompt: admitted.prompt,
        history: custom_history(&admitted.history),
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
    assert_eq!(admitted.history, history);
}

#[test]
fn owner_cancel_reset_and_late_stage_preserve_physical_and_canonical_fences() {
    let runtime = owner();
    let execution = runtime
        .conversation_session
        .lock()
        .unwrap()
        .admit_with_context("synthetic-owner", "Synthetic current", Vec::new())
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
    assert!(!runtime
        .conversation_session
        .lock()
        .unwrap()
        .has_active_execution());
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
fn desktop_durable_store_projects_sources_on_restart_without_hydration_and_retains_history_on_reset(
) {
    let temp = tempfile::tempdir().unwrap();
    let _scope = test_storage::ScopedRoot::new(temp.path().to_path_buf());
    let first = NativeAdmissionFixture::new();
    let request = first
        .state
        .admit(
            first.state.prompt_admission_resources(),
            "synthetic-search-turn",
            "Synthetic search question",
            CompletionSource::Typed,
        )
        .unwrap();
    let execution = request.request.generation;
    let active_generation = request.physical_generation;
    let sources = vec![voxgolem_core::agent_pipeline::Source {
        url: String::from("https://example.test/source-1"),
        title: String::from("Synthetic Source 1"),
    }];
    assert!(first
        .state
        .observe_sources(
            "synthetic-search-turn",
            active_generation,
            execution,
            &sources,
        )
        .unwrap());
    assert!(matches!(
        first
            .state
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
        first
            .state
            .commit(
                "synthetic-search-turn",
                active_generation,
                execution,
                &request.control,
                || {},
            )
            .unwrap(),
        CommitAssistantResult::Committed
    );

    request.guard.finish();
    let first_turns = get_app_snapshot_from_state(&first.state)
        .unwrap()
        .transcript;
    assert_eq!(first_turns.len(), 1);
    assert_eq!(
        first_turns[0].answer.content,
        voxgolem_core::ipc::MessageContentKind::Text(String::from("Synthetic grounded answer."))
    );
    assert_eq!(first_turns[0].sources, sources);

    let restarted = NativeAdmissionFixture::new();
    let restored = get_app_snapshot_from_state(&restarted.state).unwrap();
    // Renderer correlation is local to the terminal delivery, never provenance.
    assert_eq!(
        restored.transcript[0].execution_id,
        first_turns[0].execution_id
    );
    assert_eq!(restored.transcript[0].answer, first_turns[0].answer);
    assert_eq!(restored.transcript[0].sources, sources);
    assert!(!restarted
        .state
        .conversation_session
        .lock()
        .unwrap()
        .has_active_execution());
    assert!(restarted
        .state
        .begin_reset(|| Err(String::from("voice reset failed")))
        .is_err());
    assert_eq!(
        get_app_snapshot_from_state(&restarted.state)
            .unwrap()
            .transcript,
        restored.transcript
    );
    let (_, guard) = restarted.state.begin_reset(|| Ok(())).unwrap();
    drop(guard);
    assert!(get_app_snapshot_from_state(&restarted.state)
        .unwrap()
        .transcript
        .is_empty());
    let mut durable = restarted.state.durable_store.lock().unwrap();
    let snapshot = durable.ensure_store().unwrap().snapshot().unwrap();
    assert_eq!(snapshot.records.len(), 2);
    assert!(snapshot.retired.is_empty());
}
