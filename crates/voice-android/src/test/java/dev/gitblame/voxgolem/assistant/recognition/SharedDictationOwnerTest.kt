package dev.gitblame.voxgolem.assistant.recognition

import android.os.Looper
import android.os.Handler
import android.os.Message
import android.content.pm.ApplicationInfo
import dev.gitblame.voxgolem.assistant.AssistantTiming
import dev.gitblame.voxgolem.assistant.AssistantTiming.Kind
import dev.gitblame.voxgolem.assistant.AssistantTiming.Operation
import dev.gitblame.voxgolem.assistant.AssistantTiming.Stage
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment
import org.robolectric.Shadows.shadowOf
import org.robolectric.annotation.Config
import org.robolectric.annotation.LooperMode
import org.robolectric.shadows.ShadowLog
import java.time.Duration

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35], manifest = Config.NONE)
@LooperMode(LooperMode.Mode.PAUSED)
class SharedDictationOwnerTest {
    private class Factory : LocalRecognizerFactory {
        var creates = 0
        var supported = true
        var route: RecognitionRoute? = null
        var onCheck: (() -> Unit)? = null
        val policies = mutableListOf<Boolean>()
        val sessions = mutableListOf<Session>()
        val timings = mutableListOf<AssistantTiming>()
        override fun available() = true
        override fun create(): LocalRecognizerSession = Session().also { creates++; sessions += it }
        override fun create(timing: AssistantTiming): LocalRecognizerSession { timings += timing; return create() }
        override fun createForPolicy(networkFallback: Boolean): LocalRecognizerSession { policies += networkFallback; return create() }
        override fun createForPolicy(networkFallback: Boolean, timing: AssistantTiming): LocalRecognizerSession {
            timings += timing; return createForPolicy(networkFallback)
        }
        inner class Session : LocalRecognizerSession {
            override val route: RecognitionRoute? get() = this@Factory.route
            lateinit var events: LocalRecognizerEvents
            var started = false
            var destroyed = false
            var rejectDestroy = false
            var destroyFailures = 0
            var destroyCalls = 0
            var beforeDestroy: (() -> Unit)? = null
            var stops = 0
            var checkedLanguage = ""
            var startedLanguage = ""
            override fun checkLanguage(language: String, result: (Boolean) -> Unit) { checkedLanguage = language; onCheck?.invoke(); result(supported) }
            override fun start(language: String, events: LocalRecognizerEvents) { this.events = events; started = true; startedLanguage = language }
            override fun stop() { stops++ }
            override fun cancel() {}
            override fun destroy() {
                assertEquals(Looper.getMainLooper(), Looper.myLooper())
                beforeDestroy?.invoke(); destroyCalls++
                if (rejectDestroy || destroyFailures-- > 0) error("Synthetic destroy failure")
                destroyed = true
            }
        }
    }
    private val factory = Factory()
    private val leases = RecognitionResourceLease()
    private var permission = true
    private val owner = SharedDictationOwner(factory, { permission }, leases)
    private fun drain() = shadowOf(Looper.getMainLooper()).idle()

    private fun timedOwner(events: MutableList<AssistantTiming.Event>, clock: () -> Long = { 0L }): SharedDictationOwner {
        val context = RuntimeEnvironment.getApplication()
        context.applicationInfo.flags = context.applicationInfo.flags or ApplicationInfo.FLAG_DEBUGGABLE
        return SharedDictationOwner(factory, { permission }, leases, beginTiming = {
            AssistantTiming.begin(context, Operation.RECOGNITION, clock, AssistantTiming.Sink { events += it })
        })
    }

    @Test fun configuredRecognitionTimingUsesCapturedPolicyAndRetainsItsRunThroughSdkRetirement() {
        val context = RuntimeEnvironment.getApplication()
        context.applicationInfo.flags = context.applicationInfo.flags or ApplicationInfo.FLAG_DEBUGGABLE
        val events = mutableListOf<AssistantTiming.Event>()
        var admission = dev.gitblame.voxgolem.assistant.speech.SpeechAdmission(
            dev.gitblame.voxgolem.assistant.speech.SpeechSelection(language = "en-AU"), false)
        val configured = SharedDictationOwner(factory, { true }, leases, configuration = { admission }, beginTiming = {
            AssistantTiming.begin(context, Operation.RECOGNITION, sink = AssistantTiming.Sink { events += it })
        })
        factory.onCheck = { admission = admission.copy(selection = admission.selection.copy(language = "en-GB"), networkFallback = true) }
        configured.setForeground(true); configured.start("en-US"); drain()
        val sdk = factory.sessions.single()
        assertEquals(listOf(false), factory.policies)
        assertEquals("en-AU", sdk.checkedLanguage); assertEquals("en-AU", sdk.startedLanguage)
        assertEquals(events.first().run, factory.timings.single().run)
        sdk.events.final("Synthetic configured final"); drain()
        assertEquals(Stage.SETTLED, events.single { it.kind == Kind.END }.stage)
        assertTrue(sdk.destroyed); assertNull(leases.current())
        configured.close(); drain()
    }

    @Test fun timingStartsBeforeMainQueueAndEndsOnlyAfterFinalSdkRetirementWithoutPrivateValues() {
        var now = 1_000L
        val events = mutableListOf<AssistantTiming.Event>()
        val timed = timedOwner(events) { now }
        factory.route = RecognitionRoute("synthetic.private.provider", "en-AU")
        timed.setForeground(true); timed.start("en-AU", "Synthetic private retained words")
        assertEquals(listOf(Stage.REQUESTED), events.filter { it.kind == Kind.MARK }.map { it.stage })
        now += 100; drain()
        val sdk = factory.sessions.single()
        assertEquals(events.first().run, factory.timings.single().run)
        assertEquals(100L, events.single { it.kind == Kind.MARK && it.stage == Stage.WORKER_STARTED }.elapsedNanos)
        sdk.events.ready()
        assertFalse(events.any { it.stage == Stage.READY_MAIN_DELIVERED })
        now += 50; drain()
        assertEquals(150L, events.single { it.kind == Kind.MARK && it.stage == Stage.READY_MAIN_DELIVERED }.elapsedNanos)
        sdk.beforeDestroy = { now += 75 }
        sdk.events.final("Synthetic private final words")
        assertFalse(events.any { it.stage == Stage.FINAL_MAIN_DELIVERED })
        drain()
        assertEquals(150L, events.single { it.kind == Kind.MARK && it.stage == Stage.FINAL_MAIN_DELIVERED }.elapsedNanos)
        assertEquals(225L, events.single { it.kind == Kind.MARK && it.stage == Stage.SESSION_RETIRED }.elapsedNanos)
        assertEquals(Stage.SETTLED, events.single { it.kind == Kind.END }.stage)
        assertTrue(sdk.destroyed); assertNull(leases.current())
        assertEquals("Synthetic private retained words Synthetic private final words", timed.snapshot().draft)
        val diagnostics = events.toString()
        for (privateValue in listOf("en-AU", "synthetic.private.provider", "Synthetic private retained words", "Synthetic private final words")) {
            assertFalse(diagnostics.contains(privateValue))
        }
        timed.close(); drain()
        assertEquals(1, events.count { it.kind == Kind.END })
    }

    @Test fun timingCancellationBeforeAdmissionAndObsoleteCallbacksCannotAttachToReplacementRun() {
        val events = mutableListOf<AssistantTiming.Event>()
        val timed = timedOwner(events)
        timed.setForeground(true); timed.start("en-US"); timed.stop(); drain()
        val cancelledRun = events.first().run
        assertEquals(Stage.CANCELLED, events.single { it.run == cancelledRun && it.kind == Kind.END }.stage)
        assertFalse(events.any { it.run == cancelledRun && it.stage == Stage.WORKER_STARTED })
        assertEquals(0, factory.creates)
        timed.start("en-US"); drain(); val old = factory.sessions.single()
        timed.stop(); timed.start("en-US"); drain()
        val replacementRun = factory.timings.last().run
        val beforeLateCallbacks = events.toList()
        old.events.ready(); old.events.final("Synthetic obsolete final"); old.events.error("synthetic_private_error"); drain()
        assertEquals(beforeLateCallbacks, events)
        assertFalse(events.any { it.run == replacementRun && it.stage == Stage.READY_MAIN_DELIVERED })
        assertEquals(DictationPhase.LISTENING, timed.snapshot().phase)
        timed.close(); drain()
        assertEquals(3, events.count { it.kind == Kind.END })
    }

    @Test fun timingWatchdogAndLeaseDenialPreserveExistingAdmissionAndTimeoutPolicy() {
        val events = mutableListOf<AssistantTiming.Event>()
        val timed = timedOwner(events)
        val capture = leases.tryAcquire(RecognitionResourceLease.Kind.CAPTURE_LIVE)!!
        timed.setForeground(true); timed.start("en-US"); drain()
        val deniedRun = events.first().run
        assertTrue(events.any { it.run == deniedRun && it.stage == Stage.LEASE_DENIED })
        assertEquals(Stage.FAILED, events.single { it.run == deniedRun && it.kind == Kind.END }.stage)
        assertSame(capture, leases.current()); assertEquals(0, factory.creates)
        capture.close(); timed.start("en-US"); drain()
        val timedRun = factory.timings.single().run
        factory.sessions.single().events.partial("Synthetic private recoverable words"); drain()
        shadowOf(Looper.getMainLooper()).idleFor(Duration.ofSeconds(29))
        assertFalse(events.any { it.run == timedRun && it.stage == Stage.TIMED_OUT })
        shadowOf(Looper.getMainLooper()).idleFor(Duration.ofSeconds(1))
        assertTrue(events.any { it.run == timedRun && it.stage == Stage.TIMED_OUT })
        assertEquals(Stage.FAILED, events.single { it.run == timedRun && it.kind == Kind.END }.stage)
        assertEquals("recognition_timeout", timed.snapshot().error)
        assertEquals("Synthetic private recoverable words", timed.snapshot().draft)
        assertNull(leases.current()); timed.close(); drain()
    }

    @Test fun timingRetirementFailureKeepsExactSpanOpenUntilExistingLeaseRecoverySucceeds() {
        val events = mutableListOf<AssistantTiming.Event>()
        val timed = timedOwner(events)
        timed.setForeground(true); timed.start("en-US"); drain()
        val sdk = factory.sessions.single(); sdk.rejectDestroy = true
        timed.stop(); drain()
        assertTrue(events.any { it.stage == Stage.RETIREMENT_FAILED })
        assertFalse(events.any { it.kind == Kind.END || it.stage == Stage.SESSION_RETIRED })
        assertNotNull(leases.current())
        sdk.rejectDestroy = false
        assertTrue(leases.retryFailedRetirement()); drain()
        assertNull(leases.current()); assertTrue(sdk.destroyed)
        assertEquals(1, events.count { it.kind == Kind.MARK && it.stage == Stage.SESSION_RETIRED })
        assertEquals(Stage.CANCELLED, events.single { it.kind == Kind.END }.stage)
        sdk.events.ready(); sdk.events.final("Synthetic stale final"); drain()
        assertEquals(1, events.count { it.kind == Kind.END })
        timed.close(); drain()
    }

    @Test fun factoryConstructorTimingIsDisabledByDefaultEvenInDebugRuntime() {
        val context = RuntimeEnvironment.getApplication()
        context.applicationInfo.flags = context.applicationInfo.flags or ApplicationInfo.FLAG_DEBUGGABLE
        ShadowLog.clear()
        owner.setForeground(true); owner.start("en-US"); drain()
        factory.sessions.single().events.error("synthetic_private_error"); drain()
        assertTrue(ShadowLog.getLogsForTag(AssistantTiming.TAG).isEmpty())
        assertEquals("synthetic_private_error", owner.snapshot().error)
        owner.close(); drain()
    }

    @Test fun recognitionErrorRetainsFailedOutcomeAcrossSdkRetirementRecovery() {
        assertFailedRetirementOutcome { it.error("synthetic_recognition_failure") }
    }

    @Test fun finalWithFailedCleanupCannotBecomeSuccessfulOrCancelledAfterRecovery() {
        assertFailedRetirementOutcome { it.final("Synthetic authoritative final") }
    }

    private fun assertFailedRetirementOutcome(terminal: (LocalRecognizerEvents) -> Unit) {
        val events = mutableListOf<AssistantTiming.Event>()
        val timed = timedOwner(events)
        timed.setForeground(true); timed.start("en-US"); drain()
        val sdk = factory.sessions.single(); sdk.rejectDestroy = true
        terminal(sdk.events); drain()
        assertFalse(events.any { it.kind == Kind.END })
        assertEquals("recognition_cleanup_failed", timed.snapshot().error)
        sdk.rejectDestroy = false
        assertTrue(leases.retryFailedRetirement()); drain()
        assertNull(leases.current())
        assertEquals(Stage.FAILED, events.single { it.kind == Kind.END }.stage)
        sdk.events.final("Synthetic obsolete final"); drain()
        assertEquals(1, events.count { it.kind == Kind.END })
        timed.close(); drain()
    }

    @Test fun configuredLanguageAndFallbackAreImmutableForTheAdmittedUtteranceAndUnsupportedNextLanguageNeverStarts() {
        var admission = dev.gitblame.voxgolem.assistant.speech.SpeechAdmission(
            dev.gitblame.voxgolem.assistant.speech.SpeechSelection(language = "en-AU"), false)
        val configured = SharedDictationOwner(factory, { true }, leases, configuration = { admission })
        factory.onCheck = { admission = admission.copy(selection = admission.selection.copy(language = "en-GB"), networkFallback = true) }
        configured.setForeground(true); configured.start("en-US"); drain()
        assertEquals("en-AU", factory.sessions.single().checkedLanguage)
        assertEquals("en-AU", factory.sessions.single().startedLanguage)
        assertEquals(listOf(false), factory.policies)
        configured.stop(); drain(); factory.onCheck = null; factory.supported = false
        configured.start("en-US"); drain()
        assertEquals("en-GB", factory.sessions.last().checkedLanguage)
        assertFalse(factory.sessions.last().started)
        assertEquals(listOf(false, true), factory.policies)
        assertEquals("local_language_unavailable", configured.snapshot().error)
        configured.close(); drain()
    }

    @Test fun listeningReadinessRequiresProviderReadyAndCannotLeakAcrossRetirement() {
        owner.setForeground(true); owner.start("en-AU"); drain()
        assertEquals(DictationPhase.LISTENING, owner.snapshot().phase)
        assertFalse(owner.snapshot().readyForSpeech)
        val old = factory.sessions.single()
        old.events.ready(); drain()
        assertTrue(owner.snapshot().readyForSpeech)
        owner.endUtterance(); old.events.ready(); drain()
        assertEquals(DictationPhase.FINISHING, owner.snapshot().phase)
        assertFalse(owner.snapshot().readyForSpeech)
        owner.stop(); drain()
        owner.start("en-AU"); drain()
        old.events.ready(); drain()
        assertFalse(owner.snapshot().readyForSpeech)
        val current = factory.sessions.last()
        current.events.ready(); owner.stop(); drain()
        assertEquals(DictationPhase.IDLE, owner.snapshot().phase)
        assertFalse(owner.snapshot().readyForSpeech)
        owner.close(); drain()
    }

    @Test fun optionalRouteKeepsLegacyFakeAndPositionalStateConstructionCompatible() {
        assertNull(DictationState(DictationPhase.IDLE, "Synthetic draft", null, 42).route)
        owner.setForeground(true); owner.start("en-AU"); drain()
        assertNull(owner.snapshot().route)
        factory.sessions.single().events.final("Synthetic final"); drain()
        assertNull(owner.snapshot().route)
    }
    @Test fun routeIsProjectedWhileCheckingAndRetainedOnUnavailableLanguage() {
        factory.route = RecognitionRoute("synthetic.provider", "en-AU", fallbackReason = "installed_language_unavailable")
        factory.supported = false
        val projections = mutableListOf<DictationState>()
        owner.subscribe { projections += it }; owner.setForeground(true); owner.start("en-AU"); drain()
        assertEquals(factory.route, projections.first { it.phase == DictationPhase.CHECKING }.route)
        assertEquals(factory.route, owner.snapshot().route)
        assertEquals("local_language_unavailable", owner.snapshot().error)
    }
    @Test fun finalErrorAndCancellationCaptureLatestRouteBeforeSdkDisposal() {
        val initial = RecognitionRoute("synthetic.provider", "en-AU", "en-US")
        owner.setForeground(true)
        factory.route = initial; owner.start("en-AU"); drain()
        factory.route = initial.copy(reportedLanguage = "en-GB")
        factory.sessions.last().events.final("Synthetic final"); drain()
        assertEquals(factory.route, owner.snapshot().route)
        assertEquals(DictationPhase.FINAL, owner.snapshot().phase)
        factory.route = initial; owner.start("en-AU"); drain()
        factory.route = initial.copy(localPreferred = false, fallbackReason = "support_metadata_unavailable")
        factory.sessions.last().events.error("synthetic_error"); drain()
        assertEquals(factory.route, owner.snapshot().route)
        factory.route = initial; owner.start("en-AU"); drain()
        factory.route = initial.copy(reportedLanguage = "en-AU")
        owner.stop(); drain()
        assertEquals(factory.route, owner.snapshot().route)
        assertEquals(DictationPhase.IDLE, owner.snapshot().phase)
    }

    @Test fun permissionAndVisibilityAreCheckedBeforeAllocation() {
        owner.start("en-US"); drain(); assertEquals(0, factory.creates)
        owner.setForeground(true); permission = false; owner.start("en-US"); drain()
        assertEquals(0, factory.creates); assertEquals("microphone_permission_required", owner.snapshot().error)
    }
    @Test fun cancelBeforeAdmissionCannotAllocateOrRestart() {
        owner.setForeground(true); owner.start("en-US"); owner.stop(); drain()
        assertEquals(0, factory.creates); assertNull(leases.current())
    }
    @Test fun providerFinalEndsOneSessionAndLateCallbacksCannotReleaseNextOwner() {
        owner.setForeground(true); owner.start("en-US", "Synthetic prefix"); drain()
        val old = factory.sessions.single()
        old.events.partial("Synthetic partial"); drain()
        assertEquals("Synthetic prefix Synthetic partial", owner.snapshot().draft)
        old.events.final("Synthetic final"); drain()
        assertTrue(old.destroyed); assertNull(leases.current())
        owner.start("en-US", owner.snapshot().draft); drain()
        val lease = leases.current()
        old.events.error("late"); old.events.final("late"); drain()
        assertEquals(lease, leases.current()); assertFalse(owner.snapshot().draft.contains("late"))
    }
    @Test fun emptyAutomaticFollowupNoMatchIsIdleButOrdinaryDictationAndRetainedWordsStillFail() {
        owner.setForeground(true)
        owner.start("en-US", "", emptyNoMatchIsIdle = true); drain()
        factory.sessions.last().events.error("recognition_error_7"); drain()
        assertEquals(DictationPhase.IDLE, owner.snapshot().phase)
        assertTrue(owner.snapshot().endedWithoutSpeech); assertNull(owner.snapshot().error)
        assertNull(leases.current()); assertTrue(factory.sessions.last().destroyed)

        owner.start("en-US", ""); drain()
        factory.sessions.last().events.error("recognition_error_7"); drain()
        assertEquals(DictationPhase.ERROR, owner.snapshot().phase)
        assertEquals("recognition_error_7", owner.snapshot().error)

        owner.start("en-US", "", emptyNoMatchIsIdle = true); drain()
        val spoken = factory.sessions.last()
        spoken.events.partial("Synthetic recognized words"); drain()
        spoken.events.partial(""); drain()
        spoken.events.error("recognition_error_7"); drain()
        assertEquals(DictationPhase.ERROR, owner.snapshot().phase)
        assertEquals("Synthetic recognized words", owner.snapshot().draft)

        owner.start("en-US", "Synthetic existing draft", emptyNoMatchIsIdle = true); drain()
        factory.sessions.last().events.error("recognition_error_7"); drain()
        assertEquals(DictationPhase.ERROR, owner.snapshot().phase)
        assertEquals("Synthetic existing draft", owner.snapshot().draft)
        owner.close(); drain()
    }
    @Test fun endUtteranceWaitsForFinalButStopAndTimeoutKeepDraft() {
        owner.setForeground(true); owner.start("en-US"); drain()
        factory.sessions.single().events.partial("Synthetic recoverable"); drain()
        owner.endUtterance(); drain(); assertEquals(1, factory.sessions.single().stops)
        shadowOf(Looper.getMainLooper()).idleFor(Duration.ofSeconds(31))
        assertEquals("Synthetic recoverable", owner.snapshot().draft)
        assertEquals("recognition_timeout", owner.snapshot().error); assertNull(leases.current())
    }
    @Test fun captureLeaseIsNeverPreemptedAndLanguageFailureDoesNotStart() {
        val capture = leases.tryAcquire(RecognitionResourceLease.Kind.CAPTURE_LIVE)!!
        owner.setForeground(true); owner.start("en-US"); drain()
        assertEquals(0, factory.creates); assertEquals("recognition_busy", owner.snapshot().error)
        capture.close(); factory.supported = false; owner.start("en-US"); drain()
        assertFalse(factory.sessions.single().started); assertTrue(factory.sessions.single().destroyed)
        assertEquals("local_language_unavailable", owner.snapshot().error)
    }
    @Test fun backgroundStopsAndForegroundDoesNotResumeListening() {
        owner.setForeground(true); owner.start("en-US"); drain()
        owner.setForeground(false); drain(); owner.setForeground(true); drain()
        assertTrue(factory.sessions.single().destroyed); assertEquals(1, factory.creates)
    }
    @Test fun exactOwnerReleaseAndWatchNoteAdmissionAreMutuallyExclusive() {
        val note = leases.tryAcquire(RecognitionResourceLease.Kind.WATCH_NOTE_AUDIO)!!
        assertNull(leases.tryAcquire(RecognitionResourceLease.Kind.CHAT))
        note.close(); val chat = leases.tryAcquire(RecognitionResourceLease.Kind.CHAT)!!
        note.close(); assertSame(chat, leases.current())
        assertNull(leases.tryAcquire(RecognitionResourceLease.Kind.CAPTURE_REPLAY))
        chat.close()
    }
    @Test fun stoppedSupportCheckCannotStartAfterAnotherOwnerIsAdmitted() {
        lateinit var support: (Boolean) -> Unit
        var starts = 0
        val delayed = object : LocalRecognizerFactory {
            override fun available() = true
            override fun create() = object : LocalRecognizerSession {
                override fun checkLanguage(language: String, result: (Boolean) -> Unit) { support = result }
                override fun start(language: String, events: LocalRecognizerEvents) { starts++ }
                override fun stop() {}
                override fun cancel() { assertEquals(Looper.getMainLooper(), Looper.myLooper()) }
                override fun destroy() { assertEquals(Looper.getMainLooper(), Looper.myLooper()) }
            }
        }
        val checking = SharedDictationOwner(delayed, { permission }, leases)
        checking.setForeground(true); checking.start("en-US"); drain()
        checking.stop(); drain()
        val capture = leases.tryAcquire(RecognitionResourceLease.Kind.CAPTURE_LIVE)!!
        support(true); drain(); assertEquals(0, starts); assertSame(capture, leases.current()); capture.close()
    }
    @Test fun permissionRevokedWhileCheckingPreventsMicrophoneStart() {
        lateinit var support: (Boolean) -> Unit
        var started = false
        val delayed = object : LocalRecognizerFactory {
            override fun available() = true
            override fun create() = object : LocalRecognizerSession {
                override fun checkLanguage(language: String, result: (Boolean) -> Unit) { support = result }
                override fun start(language: String, events: LocalRecognizerEvents) { started = true }
                override fun stop() {}
                override fun cancel() {}
                override fun destroy() {}
            }
        }
        val checking = SharedDictationOwner(delayed, { permission }, leases)
        checking.setForeground(true); checking.start("en-US", "Synthetic draft"); drain()
        permission = false; support(true); drain()
        assertFalse(started); assertEquals("microphone_permission_required", checking.snapshot().error)
        assertEquals("Synthetic draft", checking.snapshot().draft); assertNull(leases.current())
    }
    @Test fun stopReentrantFromAdmissionProjectionPreventsAllocation() {
        owner.setForeground(true)
        var stopped = false
        owner.subscribe { if (!stopped && it.utterance > 0 && it.phase == DictationPhase.IDLE) { stopped = true; owner.stop() } }
        owner.start("en-US"); drain()
        assertEquals(0, factory.creates); assertNull(leases.current())
    }
    @Test fun failedDestroyKeepsLeaseUntilExactSdkOwnerIsActuallyDestroyed() {
        owner.setForeground(true); owner.start("en-US"); drain()
        val session = factory.sessions.single(); session.rejectDestroy = true
        owner.stop(); drain()
        assertNotNull(leases.current()); assertEquals("recognition_cleanup_failed", owner.snapshot().error)
        session.rejectDestroy = false; owner.stop(); drain(); assertNull(leases.current())
    }
    @Test fun exactCaptureHandoffKeepsAdmissionReservedAndOldTokenCannotReleaseReplacement() {
        val replay = leases.tryAcquire(RecognitionResourceLease.Kind.CAPTURE_REPLAY)!!
        val live = replay.handoff(RecognitionResourceLease.Kind.CAPTURE_LIVE)!!
        replay.close(); assertSame(live, leases.current())
        assertNull(replay.handoff(RecognitionResourceLease.Kind.CAPTURE_PROBE))
        assertNull(leases.tryAcquire(RecognitionResourceLease.Kind.CHAT)); live.close()
    }
    @Test fun availabilityObserversRunOutsideMonitorAndHandoffDoesNotNotifyAvailability() {
        var released = 0
        val registration = leases.subscribeAvailable {
            val returned = java.util.concurrent.CountDownLatch(1)
            Thread { leases.current(); returned.countDown() }.start()
            assertTrue(returned.await(2, java.util.concurrent.TimeUnit.SECONDS)); released++
        }
        val replay = leases.tryAcquire(RecognitionResourceLease.Kind.CAPTURE_REPLAY)!!
        val live = replay.handoff(RecognitionResourceLease.Kind.CAPTURE_LIVE)!!
        replay.close(); assertEquals(0, released); live.close(); assertEquals(1, released)
        registration.close()
    }
    @Test fun explicitRetirementRecoveryIsBoundedRunsOutsideMonitorAndCannotTargetReplacement() {
        val failed = leases.tryAcquire(RecognitionResourceLease.Kind.CAPTURE_LIVE)!!
        var attempts = 0
        failed.onRetirementRetry {
            val returned = java.util.concurrent.CountDownLatch(1)
            Thread { leases.current(); returned.countDown() }.start()
            assertTrue(returned.await(2, java.util.concurrent.TimeUnit.SECONDS))
            if (++attempts == 1) false else { failed.close(); true }
        }
        assertFalse(leases.retryFailedRetirement()); assertSame(failed, leases.current())
        assertTrue(leases.retryFailedRetirement()); assertEquals(2, attempts)
        val next = leases.tryAcquire(RecognitionResourceLease.Kind.CHAT)!!
        failed.onRetirementRetry { error("Synthetic stale cleanup registration") }
        assertFalse(leases.retryFailedRetirement()); assertEquals(2, attempts); assertSame(next, leases.current())
        next.close()
    }
    @Test fun recoveryCannotReportConfirmedRetirementWhileOldAdmissionIsStillHeld() {
        val held = leases.tryAcquire(RecognitionResourceLease.Kind.CAPTURE_LIVE)!!
        held.onRetirementRetry { true }
        assertFalse(leases.retryFailedRetirement()); assertSame(held, leases.current())
        held.close()
    }
    @Test fun stopAfterCloseRetriesRetainedSdkButNeverReopensClosedDictation() {
        owner.setForeground(true); owner.start("en-US"); drain()
        val sdk = factory.sessions.single(); sdk.rejectDestroy = true
        owner.close(); drain(); assertNotNull(leases.current())
        assertEquals("recognition_cleanup_failed", owner.snapshot().error)
        sdk.rejectDestroy = false; owner.stop(); drain()
        assertTrue(sdk.destroyed); assertNull(leases.current())
        owner.start("en-US"); drain(); assertEquals(1, factory.creates)
    }
    @Test fun stopDuringLeaseReleaseSuppressesTheOldFinalProjection() {
        owner.setForeground(true); owner.start("en-US"); drain()
        val phases = mutableListOf<DictationPhase>()
        owner.subscribe { phases += it.phase }; drain()
        factory.sessions.single().events.partial("Synthetic retained partial"); drain()
        val registration = leases.subscribeAvailable { owner.stop() }
        factory.sessions.single().events.final("Synthetic cancelled final"); drain()
        registration.close()
        assertFalse(phases.contains(DictationPhase.FINAL))
        assertEquals("Synthetic retained partial", owner.snapshot().draft)
        assertEquals(DictationPhase.IDLE, owner.snapshot().phase)
    }
    @Test fun newDictationOwnerRecoversDiscardedClosedOwnerOnlyAfterExactSdkDestroySucceeds() {
        fun discardOwner(): Factory.Session {
            val discarded = SharedDictationOwner(factory, { true }, leases)
            discarded.setForeground(true); discarded.start("en-US"); drain()
            val sdk = factory.sessions.single(); sdk.destroyFailures = 1
            discarded.close(); drain()
            return sdk
        }
        val oldSdk = discardOwner()
        val held = leases.current()!!
        assertFalse(oldSdk.destroyed); assertEquals(1, oldSdk.destroyCalls)
        oldSdk.beforeDestroy = {
            val unlocked = java.util.concurrent.CountDownLatch(1)
            Thread { leases.current(); unlocked.countDown() }.start()
            assertTrue(unlocked.await(2, java.util.concurrent.TimeUnit.SECONDS))
        }
        val replacement = SharedDictationOwner(factory, { true }, leases)
        replacement.setForeground(true); replacement.start("en-US", "Synthetic replacement draft"); drain()
        assertTrue(oldSdk.destroyed); assertEquals(2, oldSdk.destroyCalls)
        assertEquals(2, factory.creates); assertTrue(factory.sessions.last().started)
        assertEquals(DictationPhase.LISTENING, replacement.snapshot().phase)
        assertNotSame(held, leases.current())
        val admitted = leases.current()
        oldSdk.events.final("Synthetic obsolete final"); oldSdk.events.error("synthetic_obsolete_error"); drain()
        assertSame(admitted, leases.current()); assertEquals("Synthetic replacement draft", replacement.snapshot().draft)
        replacement.stop(); drain(); assertNull(leases.current())
    }
    @Test fun workerRecoveryOfDiscardedOwnerQueuesExactSdkTeardownOnMainBeforeReleasingAdmission() {
        val posted = java.util.concurrent.CountDownLatch(1)
        var trackRecovery = false
        val main = object : Handler(Looper.getMainLooper()) {
            override fun sendMessageAtTime(message: Message, uptimeMillis: Long): Boolean {
                val queued = super.sendMessageAtTime(message, uptimeMillis)
                if (trackRecovery) posted.countDown()
                return queued
            }
        }
        fun discardOwner(): Factory.Session {
            val discarded = SharedDictationOwner(factory, { true }, leases, main)
            discarded.setForeground(true); discarded.start("en-US"); drain()
            val sdk = factory.sessions.single(); sdk.destroyFailures = 1
            discarded.close(); drain(); return sdk
        }
        val sdk = discardOwner()
        val held = leases.current()
        trackRecovery = true
        val completed = java.util.concurrent.atomic.AtomicBoolean(false)
        val worker = Thread { completed.set(leases.retryFailedRetirement()) }
        worker.start(); assertTrue(posted.await(1, java.util.concurrent.TimeUnit.SECONDS))
        assertSame(held, leases.current()); assertFalse(sdk.destroyed)
        drain(); worker.join(1_000)
        assertFalse(worker.isAlive); assertTrue(completed.get())
        assertTrue(sdk.destroyed); assertEquals(2, sdk.destroyCalls); assertNull(leases.current())
    }
}
