package dev.gitblame.voxgolem.assistant.speech

import android.os.Looper
import android.content.pm.ApplicationInfo
import dev.gitblame.voxgolem.assistant.AssistantTiming
import dev.gitblame.voxgolem.assistant.AssistantTiming.Kind
import dev.gitblame.voxgolem.assistant.AssistantTiming.Operation
import dev.gitblame.voxgolem.assistant.AssistantTiming.Stage
import dev.gitblame.voxgolem.assistant.recognition.RecognitionResourceLease
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment
import org.robolectric.Shadows.shadowOf
import org.robolectric.annotation.Config
import org.robolectric.annotation.LooperMode
import java.util.Locale
import java.time.Duration

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35], manifest = Config.NONE)
@LooperMode(LooperMode.Mode.PAUSED)
class LocalSpeechOutputTest {
    private class Factory : LocalTtsFactory {
        var creates = 0
        lateinit var ready: (Boolean) -> Unit
        val sdk = Session()
        override fun create(onReady: (Boolean) -> Unit): LocalTtsSession { creates++; ready = onReady; return sdk }
        class Session : LocalTtsSession {
            override var provider: String? = null
            var offered = listOf(LocalVoice("synthetic-local", Locale.US, false, true))
            var spoken = mutableListOf<String>()
            var queueModes = mutableListOf<Int>()
            var terminals = mutableListOf<(String?) -> Unit>()
            var warmups = mutableListOf<LocalVoice>()
            lateinit var finished: (String?) -> Unit
            var stops = 0
            var shutdown = false
            var rejectShutdown = false
            var shutdownFailures = 0
            var shutdownCalls = 0
            var beforeShutdown: (() -> Unit)? = null
            override fun voices() = offered
            override fun warmup(voice: LocalVoice) {
                warmups += voice
            }
            override fun speak(text: String, voice: LocalVoice, onTerminal: (String?) -> Unit): Boolean =
                speak(text, voice, android.speech.tts.TextToSpeech.QUEUE_FLUSH, null, onTerminal)
            override fun speak(
                text: String,
                voice: LocalVoice,
                queueMode: Int,
                timing: AssistantTiming?,
                onTerminal: (String?) -> Unit,
            ): Boolean {
                spoken += text
                queueModes += queueMode
                terminals += onTerminal
                finished = onTerminal
                return true
            }
            override fun stop() { stops++ }
            override fun shutdown() {
                assertEquals(Looper.getMainLooper(), Looper.myLooper())
                beforeShutdown?.invoke(); shutdownCalls++
                if (rejectShutdown || shutdownFailures-- > 0) error("Synthetic shutdown failure")
                shutdown = true
            }
        }
    }
    private class Focus : SpeechAudioFocus {
        var requests = 0
        var releases = 0
        var granted = true
        lateinit var lost: () -> Unit
        override fun acquire(onLoss: () -> Unit): Boolean { requests++; lost = onLoss; return granted }
        override fun release() { releases++ }
    }
    private class ProviderFactory : LocalTtsFactory {
        val initial = Factory.Session()
        val alternate = Factory.Session().apply { provider = "synthetic-alternate" }
        val opened = mutableListOf<String?>()
        val callbacks = mutableListOf<(Boolean) -> Unit>()
        override fun providers() = listOf(null, "synthetic-alternate")
        override fun create(onReady: (Boolean) -> Unit) = create(null, onReady)
        override fun create(provider: String?, onReady: (Boolean) -> Unit): LocalTtsSession {
            opened += provider; callbacks += onReady
            return if (provider == null) initial else alternate
        }
    }
    private val factory = Factory()
    private val focus = Focus()
    private val output = LocalSpeechOutput(factory, focus)
    private fun drain() = shadowOf(Looper.getMainLooper()).idle()
    private fun enable() { output.setForeground(true); output.setEnabled(true); drain() }

    private fun timedOutput(events: MutableList<AssistantTiming.Event>, leases: RecognitionResourceLease): LocalSpeechOutput {
        val context = RuntimeEnvironment.getApplication()
        context.applicationInfo.flags = context.applicationInfo.flags or ApplicationInfo.FLAG_DEBUGGABLE
        return LocalSpeechOutput(factory, focus, leases = leases,
            beginTiming = { AssistantTiming.begin(context, it, sink = AssistantTiming.Sink { event -> events += event }) })
    }

    @Test fun catalogueProjectionIsReusedAcrossWarmSpeechPhasesAndRetirement() {
        enable(); output.prepare("en-US"); drain(); factory.ready(true); drain()
        val options = output.options()
        val phases = mutableListOf<SpeechPhase>()
        val subscription = output.subscribe { state ->
            phases += state.phase
            assertSame(options, state.options)
        }
        drain()
        output.speak("Synthetic cached reply", "en-US") {}; drain()
        factory.sdk.finished(null); drain()
        output.setForeground(false); drain()
        assertTrue(phases.containsAll(listOf(SpeechPhase.CHECKING, SpeechPhase.SPEAKING, SpeechPhase.IDLE)))
        assertEquals("synthetic-local", options.ttsVoices.single().name)
        subscription.close(); output.close(); drain()
    }

    @Test fun catalogueProviderReplacementAndRemovalInvalidateButUnchangedRefreshReusesProjection() {
        var providers = listOf("synthetic-first")
        val catalog = object : LocalTtsFactory {
            override fun providers() = providers
            override fun create(onReady: (Boolean) -> Unit): LocalTtsSession = error("No SDK needed")
        }
        val speech = LocalSpeechOutput(catalog, focus)
        speech.refreshProviders(); drain()
        val first = speech.options()
        speech.refreshProviders(); drain(); assertSame(first, speech.options())
        providers = listOf("synthetic-second")
        speech.refreshProviders(); drain()
        val replaced = speech.options()
        assertNotSame(first, replaced); assertEquals(providers, replaced.ttsProviders)
        providers = emptyList()
        speech.refreshProviders(); drain()
        assertNotSame(replaced, speech.options()); assertTrue(speech.options().ttsProviders.isEmpty())
        speech.close(); drain()
    }

    @Test fun catalogueInventoryReplacementAndLruEvictionPreserveBoundsAndLiveSelection() {
        var selected = "synthetic-provider-0"
        var voices = listOf(LocalVoice("synthetic-original", Locale.US, false, true))
        lateinit var ready: (Boolean) -> Unit
        lateinit var session: Factory.Session
        val catalog = object : LocalTtsFactory {
            override fun providers() = listOf(selected)
            override fun create(onReady: (Boolean) -> Unit): LocalTtsSession {
                ready = onReady
                session = Factory.Session().apply { provider = selected; offered = voices }
                return session
            }
        }
        val speech = LocalSpeechOutput(catalog, focus,
            configuration = { SpeechAdmission(SpeechSelection(ttsProvider = selected), false) })
        fun prepare() {
            speech.setForeground(false); drain()
            speech.setForeground(true); speech.prepare("en-US"); drain(); ready(true); drain()
        }
        speech.setEnabled(true); prepare()
        val original = speech.options()
        voices = listOf(LocalVoice("synthetic-replacement", Locale.US, false, true))
        prepare()
        assertNotSame(original, speech.options())
        assertEquals("synthetic-replacement", speech.options().ttsVoices.single().name)
        assertEquals("synthetic-replacement", session.warmups.single().name)
        voices = emptyList(); prepare()
        assertTrue(speech.options().ttsVoices.isEmpty())
        for (index in 0..8) {
            selected = "synthetic-provider-$index"
            voices = (0..128).map { LocalVoice("synthetic-voice-${it.toString().padStart(3, '0')}", Locale.US, false, true) }
            prepare()
        }
        val bounded = speech.options()
        assertEquals(1024, bounded.ttsVoices.size)
        assertEquals(8, bounded.ttsVoices.map { it.provider }.distinct().size)
        assertFalse(bounded.ttsVoices.any { it.provider == "synthetic-provider-0" })
        assertFalse(bounded.ttsVoices.any { it.name == "synthetic-voice-128" })
        assertEquals("synthetic-voice-000", session.warmups.single().name)
        assertEquals(0, focus.requests)
        speech.close(); drain()
    }

    @Test fun ownerTimingPreservesWarmCacheIdleAdmissionAndSeparateReplySettlement() {
        val events = mutableListOf<AssistantTiming.Event>()
        val leases = RecognitionResourceLease()
        val speech = timedOutput(events, leases)
        speech.setForeground(true); speech.setEnabled(true); speech.prepare("en-US")
        drain(); factory.ready(true); drain()
        val preparation = events.filter { it.operation == Operation.PREPARATION }
        assertEquals(Stage.SETTLED, preparation.last().stage)
        assertNull(leases.current()); assertEquals(0, focus.requests); assertEquals(1, factory.creates)
        var completions = 0
        speech.speak("Synthetic first reply", "en-US") { completions++ }; drain()
        val firstTerminal = factory.sdk.finished
        firstTerminal(null); drain()
        assertEquals(1, completions); assertNull(leases.current()); assertEquals(1, focus.releases)
        speech.speak("Synthetic second reply", "en-US") { completions++ }; drain()
        val beforeStaleCallback = events.toList()
        firstTerminal(null); drain()
        assertEquals(beforeStaleCallback, events)
        assertNotNull(leases.current()); assertEquals(SpeechPhase.SPEAKING, speech.snapshot().phase)
        factory.sdk.finished(null); drain()
        val replies = events.filter { it.operation == Operation.SPEECH }.groupBy { it.run }.values
        assertEquals(2, replies.size)
        for (reply in replies) {
            assertEquals(listOf(Stage.REQUESTED, Stage.ADMITTED, Stage.READY_REUSED, Stage.LOCAL_ROUTE_SELECTED,
                Stage.LEASE_ACQUIRED, Stage.FOCUS_ACQUIRED, Stage.SETTLED), reply.filter { it.kind == Kind.MARK }.map { it.stage })
            assertEquals(1, reply.count { it.kind == Kind.END })
        }
        assertEquals(2, completions); assertEquals(1, factory.creates)
        assertEquals(2, focus.requests); assertEquals(2, focus.releases); assertNull(leases.current())
        speech.close(); drain(); assertTrue(factory.sdk.shutdown)
    }

    @Test fun pendingPreparationReuseAndBackgroundCloseTimingWithoutLateReadinessOrReplay() {
        val events = mutableListOf<AssistantTiming.Event>()
        val leases = RecognitionResourceLease()
        val speech = timedOutput(events, leases)
        speech.setForeground(true); speech.setEnabled(true); speech.prepare("en-US"); drain()
        speech.speak("Synthetic pending reply", "en-US") {}; drain()
        assertEquals(1, factory.creates)
        assertEquals(Stage.REPLACED, events.last { it.operation == Operation.PREPARATION }.stage)
        assertTrue(events.any { it.operation == Operation.SPEECH && it.stage == Stage.PENDING_REUSED })
        speech.setForeground(false); drain()
        assertEquals(Stage.CANCELLED, events.last().stage)
        assertTrue(events.any { it.stage == Stage.SESSION_RETIRED })
        val retired = events.toList()
        factory.ready(true); drain(); speech.setForeground(true); drain()
        assertEquals(retired, events)
        assertTrue(factory.sdk.shutdown); assertTrue(factory.sdk.spoken.isEmpty())
        assertEquals(0, focus.requests); assertNull(leases.current())
        speech.close(); drain()
    }

    @Test fun focusDenialAndFailedRetirementCloseOnlyTheirOwnTimingWithoutReleasingHeldLease() {
        val events = mutableListOf<AssistantTiming.Event>()
        val leases = RecognitionResourceLease()
        val speech = timedOutput(events, leases)
        speech.setForeground(true); speech.setEnabled(true); focus.granted = false
        speech.speak("Synthetic denied reply", "en-US") {}; drain(); factory.ready(true); drain()
        assertTrue(events.any { it.stage == Stage.FOCUS_DENIED })
        assertEquals(Stage.FAILED, events.last().stage)
        assertTrue(factory.sdk.spoken.isEmpty()); assertNull(leases.current())
        focus.granted = true
        speech.speak("Synthetic admitted reply", "en-US") {}; drain(); factory.ready(true); drain()
        factory.sdk.rejectShutdown = true
        speech.stop(); drain()
        assertEquals(Stage.RETIREMENT_FAILED, events.last().stage)
        assertNotNull(leases.current()); assertEquals("speech_cleanup_failed", speech.snapshot().error)
        val failed = events.toList()
        factory.sdk.finished(null); drain()
        factory.sdk.rejectShutdown = false; speech.stop(); drain()
        assertEquals(failed, events); assertNull(leases.current())
        assertEquals(1, focus.releases)
        speech.close(); drain()
    }

    @Test fun cancelledQueuedSpeechClosesTimingBeforeAnySdkAllocation() {
        val events = mutableListOf<AssistantTiming.Event>()
        val speech = timedOutput(events, RecognitionResourceLease())
        speech.setForeground(true); speech.setEnabled(true); drain()
        speech.speak("Synthetic cancelled reply", "en-US") {}; speech.stop(); drain()
        assertEquals(listOf(Stage.REQUESTED, Stage.CANCELLED), events.filter { it.kind == Kind.MARK }.map { it.stage })
        assertEquals(1, events.count { it.kind == Kind.END })
        assertEquals(0, factory.creates); assertEquals(0, focus.requests)
        speech.close(); drain()
    }

    @Test fun deniedSharedAdmissionIsMeasuredWithoutRequestingFocusOrPreemptingNote() {
        val events = mutableListOf<AssistantTiming.Event>()
        val leases = RecognitionResourceLease()
        val note = leases.tryAcquire(RecognitionResourceLease.Kind.WATCH_NOTE_AUDIO)!!
        val speech = timedOutput(events, leases)
        speech.setForeground(true); speech.setEnabled(true)
        var result: String? = null
        speech.speak("Synthetic blocked reply", "en-US") { result = it }; drain(); factory.ready(true); drain()
        assertEquals("recognition_busy", result)
        assertTrue(events.any { it.stage == Stage.LEASE_DENIED })
        assertEquals(Stage.FAILED, events.last().stage)
        assertSame(note, leases.current()); assertEquals(0, focus.requests); assertTrue(factory.sdk.spoken.isEmpty())
        speech.close(); drain(); note.close()
    }

    @Test fun disabledByDefaultAndStopBeforeInitializationNeverAllocates() {
        output.setForeground(true); output.speak("Synthetic final", "en-US") {}; drain()
        assertEquals(0, factory.creates)
        enable(); output.speak("Synthetic final", "en-US") {}; output.stop(); drain()
        assertEquals(0, factory.creates)
    }
    @Test fun networkAndUninstalledVoicesNeverSpeakOrRequestFocus() {
        enable(); factory.sdk.offered = listOf(LocalVoice("synthetic-network", Locale.US, true, true),
            LocalVoice("synthetic-missing", Locale.US, false, false))
        var result: String? = null
        output.speak("Synthetic final", "en-US") { result = it }; drain(); factory.ready(true); drain()
        assertEquals("local_voice_unavailable", result); assertEquals(0, focus.requests); assertTrue(factory.sdk.spoken.isEmpty())
    }
    @Test fun sdkOptionsOmitOverByteLimitIdsWithoutTruncatingAcceptedOpaqueNames() {
        val exact = "界".repeat(170) + "ab"
        val oversized = "界".repeat(171)
        val offered = Factory.Session().apply { this.offered = listOf(
            LocalVoice(exact, Locale.US, false, true), LocalVoice(oversized, Locale.US, false, true)) }
        lateinit var ready: (Boolean) -> Unit
        val catalog = object : LocalTtsFactory {
            override fun providers() = listOf(null, oversized, exact)
            override fun create(onReady: (Boolean) -> Unit): LocalTtsSession { ready = onReady; return offered }
        }
        val speech = LocalSpeechOutput(catalog, focus)
        speech.setForeground(true); speech.prepare("en-US"); drain(); ready(true); drain()
        val value = speech.options().json()
        assertEquals(listOf(exact), speech.options().ttsProviders)
        assertEquals(1, value.getJSONArray("ttsVoices").length())
        assertEquals(exact, value.getJSONArray("ttsVoices").getJSONObject(0).getString("name"))
        assertEquals(exact, speech.snapshot().route!!.selectedVoice)
        assertEquals(0, focus.requests)
        speech.close(); drain()
    }
    @Test fun explicitProviderVoiceAndLanguageAreCapturedAndMissingVoiceNeverUsesAnotherChoice() {
        val providers = ProviderFactory()
        providers.alternate.offered = listOf(LocalVoice("synthetic-au-local", Locale.forLanguageTag("en-AU"), false, true),
            LocalVoice("synthetic-au-other", Locale.forLanguageTag("en-AU"), false, true))
        var configuration = SpeechAdmission(SpeechSelection("en-AU", "synthetic-alternate", "synthetic-au-local"), false)
        val speech = LocalSpeechOutput(providers, focus, configuration = { configuration })
        speech.setForeground(true); speech.setEnabled(true)
        speech.speak("Synthetic admitted answer", "en-US") {}; drain()
        configuration = configuration.copy(selection = configuration.selection.copy(ttsVoice = "synthetic-missing"))
        providers.callbacks.single()(true); drain()
        assertEquals(listOf("Synthetic admitted answer"), providers.alternate.spoken)
        assertEquals("en-AU", speech.snapshot().route!!.requestedLanguage)
        assertEquals("synthetic-au-local", speech.snapshot().route!!.selectedVoice)
        assertEquals(listOf("synthetic-alternate"), speech.options().ttsProviders)
        assertTrue(speech.options().ttsVoices.any { it.name == "synthetic-au-local" && it.provider == "synthetic-alternate" })
        providers.alternate.finished(null); drain()
        val results = mutableListOf<String?>()
        speech.speak("Synthetic rejected answer", "en-US", results::add); drain(); providers.callbacks.last()(true); drain()
        assertEquals(listOf("unsupported_tts_voice"), results)
        assertEquals(listOf("synthetic-alternate", "synthetic-alternate"), providers.opened)
        assertTrue(providers.initial.spoken.isEmpty()); assertEquals(1, focus.requests)
        speech.close(); drain()
    }
    @Test fun explicitMissingOrFailedProviderCannotFallBackToSystemOrAnotherInstalledEngine() {
        for (missing in listOf(true, false)) {
            val providers = ProviderFactory()
            val speech = LocalSpeechOutput(providers, focus, configuration = {
                SpeechAdmission(SpeechSelection(ttsProvider = if (missing) "synthetic-missing" else "synthetic-alternate"), true)
            })
            val results = mutableListOf<String?>()
            speech.setForeground(true); speech.setEnabled(true); speech.speak("Synthetic answer", "en-US", results::add); drain()
            if (!missing) { providers.callbacks.single()(false); drain() }
            assertEquals(listOf(if (missing) "unsupported_tts_provider" else "speech_initialization_failed"), results)
            assertEquals(if (missing) emptyList<String?>() else listOf("synthetic-alternate"), providers.opened)
            assertTrue(providers.initial.spoken.isEmpty()); assertTrue(providers.alternate.spoken.isEmpty())
            speech.close(); drain()
        }
    }
    @Test fun changingProviderDuringIdlePreparationRetiresOldInitializationAndPublishesOnlyTheNewInventory() {
        val providers = ProviderFactory()
        var configuration = SpeechAdmission()
        val speech = LocalSpeechOutput(providers, focus, configuration = { configuration })
        speech.setForeground(true); speech.prepare("en-US"); drain()
        assertEquals(listOf<String?>(null), providers.opened)
        configuration = configuration.copy(selection = SpeechSelection(ttsProvider = "synthetic-alternate"))
        speech.prepare("en-US"); drain()
        assertEquals(listOf(null, "synthetic-alternate"), providers.opened)
        assertTrue(providers.initial.shutdown)
        providers.callbacks.first()(true); drain()
        assertTrue(speech.options().ttsVoices.isEmpty())
        providers.callbacks.last()(true); drain()
        assertTrue(speech.options().ttsVoices.all { it.provider == "synthetic-alternate" })
        assertEquals(SpeechPhase.IDLE, speech.snapshot().phase)
        assertEquals(0, focus.requests)
        assertTrue(providers.initial.spoken.isEmpty()); assertTrue(providers.alternate.spoken.isEmpty())
        speech.close(); drain()
    }
    @Test fun disablingFallbackRejectsCachedNetworkVoiceOnTheNextPlaybackAdmission() {
        var allowed = true
        factory.sdk.offered = listOf(LocalVoice("synthetic-network", Locale.US, true, true))
        val speech = LocalSpeechOutput(factory, focus, networkFallbackPolicy = { allowed })
        speech.setForeground(true); speech.setEnabled(true)
        speech.speak("Synthetic first", "en-US") {}; drain(); factory.ready(true); drain()
        assertEquals(listOf("Synthetic first"), factory.sdk.spoken)
        factory.sdk.finished(null); drain()
        allowed = false
        val errors = mutableListOf<String?>()
        speech.speak("Synthetic blocked network reply", "en-US", errors::add); drain()
        assertEquals(listOf("local_voice_unavailable"), errors)
        assertEquals(listOf("Synthetic first"), factory.sdk.spoken)
        assertEquals(1, focus.requests)
        speech.close(); drain()
    }
    @Test fun focusLossStopsPlaybackAndLateDoneCannotRestart() {
        enable(); var results = 0
        output.speak("Synthetic final", "en-US") { results++ }; drain(); factory.ready(true); drain()
        assertEquals(listOf("Synthetic final"), factory.sdk.spoken)
        focus.lost(); drain(); factory.sdk.finished(null); drain()
        assertEquals(1, results); assertTrue(factory.sdk.stops > 0); assertEquals(1, focus.releases)
    }
    @Test fun backgroundDuringInitializationIgnoresReadyAndRequiresNewExplicitSpeech() {
        enable(); output.speak("Synthetic final", "en-US") {}; drain()
        output.setForeground(false); drain(); factory.ready(true); drain(); output.setForeground(true); drain()
        assertTrue(factory.sdk.spoken.isEmpty()); assertEquals(0, focus.requests)
    }
    @Test fun deniedFocusDoesNotSpeakAndRepeatedReadyCannotPlayTwice() {
        enable(); focus.granted = false; var error: String? = null
        output.speak("Synthetic final", "en-US") { error = it }; drain(); factory.ready(true); factory.ready(true); drain()
        assertEquals("audio_focus_denied", error); assertTrue(factory.sdk.spoken.isEmpty()); assertEquals(1, focus.requests)
    }
    @Test fun playbackHoldsSharedAdmissionUntilActualTerminalAndDisablingSettlesOnce() {
        val leases = RecognitionResourceLease()
        val speech = LocalSpeechOutput(factory, focus, leases = leases)
        speech.setForeground(true); speech.setEnabled(true); var error: String? = null
        speech.speak("Synthetic final", "en-US") { error = it }; drain(); factory.ready(true); drain()
        assertNull(leases.tryAcquire(RecognitionResourceLease.Kind.CHAT))
        assertNull(leases.tryAcquire(RecognitionResourceLease.Kind.WATCH_NOTE_AUDIO))
        speech.setEnabled(false); drain(); factory.sdk.finished(null); drain()
        assertEquals("speech_disabled", error); assertNull(leases.current())
    }
    @Test fun failedShutdownDoesNotReleasePlaybackLeaseUntilExactOwnerTerminates() {
        val leases = RecognitionResourceLease()
        val speech = LocalSpeechOutput(factory, focus, leases = leases)
        speech.setForeground(true); speech.setEnabled(true); speech.speak("Synthetic final", "en-US") {}
        drain(); factory.ready(true); drain(); factory.sdk.rejectShutdown = true
        speech.stop(); drain()
        assertNotNull(leases.current()); assertEquals("speech_cleanup_failed", speech.snapshot().error)
        factory.sdk.rejectShutdown = false; speech.stop(); drain(); assertNull(leases.current())
    }
    @Test fun availabilityQueryCannotPreemptQueuedOrActivePlayback() {
        enable(); var completed = false
        output.prepare("en-US"); output.speak("Synthetic final", "en-US") { completed = true }; drain()
        factory.ready(true); drain(); output.prepare("en-US"); drain()
        assertEquals(1, factory.creates); assertEquals(SpeechPhase.SPEAKING, output.snapshot().phase)
        factory.sdk.finished(null); drain(); assertTrue(completed)
    }
    @Test fun unresponsiveProviderShutsDownAtItsShorterInitializationDeadline() {
        enable(); var result: String? = null
        output.speak("Synthetic final", "en-US") { result = it }; drain()
        shadowOf(Looper.getMainLooper()).idleFor(Duration.ofSeconds(11))
        factory.ready(true); drain()
        assertEquals("speech_initialization_failed", result); assertTrue(factory.sdk.shutdown); assertTrue(factory.sdk.spoken.isEmpty())
    }
    @Test fun underscoreLocaleSelectsTheSameInstalledVoiceAsBcp47() {
        enable(); output.speak("Synthetic final", "en_US") {}; drain(); factory.ready(true); drain()
        assertEquals(listOf("Synthetic final"), factory.sdk.spoken)
        output.stop(); drain()
    }
    @Test fun malformedLocaleNeverRequestsAudioFocus() {
        enable(); output.speak("Synthetic final", "en---US") {}; drain()
        if (factory.creates > 0) { factory.ready(true); drain() }
        assertEquals(0, focus.requests); assertTrue(factory.sdk.spoken.isEmpty())
    }
    @Test fun preparedForegroundVoiceRemainsWarmWithoutHoldingAudioAdmission() {
        val leases = RecognitionResourceLease()
        val speech = LocalSpeechOutput(factory, focus, leases = leases)
        speech.setForeground(true); speech.setEnabled(true); speech.prepare("en-US")
        drain(); factory.ready(true); drain()
        assertEquals(SpeechPhase.IDLE, speech.snapshot().phase)
        assertTrue(speech.snapshot().localVoiceAvailable)
        assertFalse(factory.sdk.shutdown)
        assertNull(leases.current()); assertEquals(0, focus.requests)
        speech.speak("Synthetic first reply", "en-US") {}; drain()
        assertEquals(1, factory.creates)
        assertEquals(listOf("Synthetic first reply"), factory.sdk.spoken)
        speech.close(); drain()
    }
    @Test fun consecutiveRepliesReuseReadySessionAndReleaseFocusBetweenUtterances() {
        val leases = RecognitionResourceLease()
        val speech = LocalSpeechOutput(factory, focus, leases = leases)
        speech.setForeground(true); speech.setEnabled(true)
        speech.speak("Synthetic first reply", "en-US") {}; drain(); factory.ready(true); drain()
        val oldTerminal = factory.sdk.finished
        oldTerminal(null); drain()
        assertNull(leases.current()); assertEquals(1, focus.releases)
        assertFalse(factory.sdk.shutdown)
        speech.speak("Synthetic second reply", "en-US") {}; drain()
        assertEquals(1, factory.creates)
        assertEquals(listOf("Synthetic first reply", "Synthetic second reply"), factory.sdk.spoken)
        oldTerminal(null); drain()
        assertEquals(SpeechPhase.SPEAKING, speech.snapshot().phase)
        assertNotNull(leases.current())
        factory.sdk.finished(null); drain()
        assertNull(leases.current()); assertEquals(2, focus.releases)
        speech.close(); drain(); assertTrue(factory.sdk.shutdown)
    }
    @Test fun speakingWhilePreparationInitializesReusesItsPendingConnection() {
        enable(); output.prepare("en-US"); drain()
        output.speak("Synthetic reply", "en-US") {}; drain()
        assertEquals(1, factory.creates)
        factory.ready(true); drain()
        assertEquals(listOf("Synthetic reply"), factory.sdk.spoken)
        output.close(); drain()
    }
    @Test fun foregroundIdleStopKeepsPreparationButBackgroundDisposesIt() {
        enable(); output.prepare("en-US"); drain(); factory.ready(true); drain()
        output.stop(); drain()
        assertFalse(factory.sdk.shutdown)
        output.setForeground(false); drain()
        assertTrue(factory.sdk.shutdown)
        output.setForeground(true); drain()
        assertTrue(factory.sdk.spoken.isEmpty())
    }
    @Test fun localAccentFallbackWinsOverExactRegionNetworkVoice() {
        factory.sdk.offered = listOf(LocalVoice("synthetic-network-au", Locale.forLanguageTag("en-AU"), true, true),
            LocalVoice("synthetic-local-us", Locale.US, false, true))
        val speech = LocalSpeechOutput(factory, focus, allowNetworkFallback = true)
        speech.setForeground(true); speech.setEnabled(true); speech.prepare("en-AU")
        drain(); factory.ready(true); drain()
        assertEquals("en-US", speech.snapshot().route?.selectedLanguage)
        assertEquals("configured_variant_unavailable", speech.snapshot().route?.fallbackReason)
        assertFalse(speech.snapshot().route!!.networkRequired)
        assertTrue(speech.snapshot().localVoiceAvailable)
        speech.close(); drain()
    }
    @Test fun permittedNetworkVoiceIsAvailableButNeverAdvertisedAsLocal() {
        factory.sdk.offered = listOf(LocalVoice("synthetic-network", Locale.US, true, true),
            LocalVoice("synthetic-uninstalled", Locale.US, false, false))
        val speech = LocalSpeechOutput(factory, focus, allowNetworkFallback = true)
        speech.setForeground(true); speech.setEnabled(true); speech.speak("Synthetic reply", "en-US") {}
        drain(); factory.ready(true); drain()
        assertEquals(SpeechPhase.SPEAKING, speech.snapshot().phase)
        assertTrue(speech.snapshot().voiceAvailable)
        assertFalse(speech.snapshot().localVoiceAvailable)
        assertTrue(speech.snapshot().route!!.networkRequired)
        assertEquals("no_installed_local_voice", speech.snapshot().route?.fallbackReason)
        speech.close(); drain()
    }
    @Test fun networkPermissionNeverMakesUninstalledVoiceEligible() {
        factory.sdk.offered = listOf(LocalVoice("synthetic-uninstalled", Locale.US, true, false))
        val speech = LocalSpeechOutput(factory, focus, allowNetworkFallback = true)
        speech.setForeground(true); speech.setEnabled(true); speech.prepare("en-US")
        drain(); factory.ready(true); drain()
        assertEquals("local_voice_unavailable", speech.snapshot().error)
        assertFalse(speech.snapshot().voiceAvailable)
        assertEquals(0, focus.requests)
        speech.close(); drain()
    }
    @Test fun alternateInstalledLocalEngineWinsBeforeNetworkFallback() {
        val providers = ProviderFactory()
        providers.initial.offered = listOf(LocalVoice("synthetic-network-au", Locale.forLanguageTag("en-AU"), true, true))
        val speech = LocalSpeechOutput(providers, focus, allowNetworkFallback = true)
        speech.setForeground(true); speech.setEnabled(true); speech.speak("Synthetic reply", "en-AU") {}
        drain(); providers.callbacks[0](true); drain()
        assertEquals(listOf(null, "synthetic-alternate"), providers.opened)
        assertTrue(providers.initial.shutdown); assertEquals(0, focus.requests)
        providers.callbacks[1](true); drain()
        assertEquals(listOf("Synthetic reply"), providers.alternate.spoken)
        assertFalse(speech.snapshot().route!!.networkRequired)
        assertEquals("synthetic-alternate", speech.snapshot().route?.provider)
        speech.close(); drain()
    }
    @Test fun providerTimeoutRetiresOnlyItsSessionAndIgnoresItsLateInitialization() {
        val providers = ProviderFactory()
        val speech = LocalSpeechOutput(providers, focus)
        speech.setForeground(true); speech.setEnabled(true); speech.speak("Synthetic reply", "en-US") {}
        drain(); shadowOf(Looper.getMainLooper()).idleFor(Duration.ofSeconds(10))
        assertEquals(listOf(null, "synthetic-alternate"), providers.opened)
        assertTrue(providers.initial.shutdown)
        providers.callbacks[0](true); drain()
        assertEquals(SpeechPhase.CHECKING, speech.snapshot().phase)
        providers.callbacks[1](true); drain()
        assertEquals(SpeechPhase.SPEAKING, speech.snapshot().phase)
        assertEquals(listOf("Synthetic reply"), providers.alternate.spoken)
        speech.close(); drain()
    }
    @Test fun stopAfterCloseRetriesRetainedTtsWithoutPlayingOrReopeningClosedOwner() {
        val leases = RecognitionResourceLease()
        val speech = LocalSpeechOutput(factory, focus, leases = leases)
        speech.setForeground(true); speech.setEnabled(true); speech.speak("Synthetic final", "en-US") {}
        drain(); factory.ready(true); drain(); factory.sdk.rejectShutdown = true
        speech.close(); drain(); assertNotNull(leases.current())
        assertEquals("speech_cleanup_failed", speech.snapshot().error)
        factory.sdk.rejectShutdown = false; speech.stop(); drain(); assertNull(leases.current())
        factory.sdk.finished(null); speech.speak("Synthetic stale request", "en-US") {}; drain()
        assertEquals(1, factory.creates); assertEquals(listOf("Synthetic final"), factory.sdk.spoken)
    }
    @Test fun closeDuringTerminalProjectionCannotDeliverOldPlaybackSuccess() {
        enable(); var completions = 0; var closeOnIdle = false
        output.subscribe { if (closeOnIdle && it.phase == SpeechPhase.IDLE) { closeOnIdle = false; output.close() } }
        output.speak("Synthetic final", "en-US") { completions++ }; drain(); factory.ready(true); drain()
        closeOnIdle = true; factory.sdk.finished(null); drain()
        assertEquals(0, completions); assertTrue(factory.sdk.shutdown)
    }
    @Test fun newSpeechOwnerRecoversDiscardedClosedOutputWithoutAnOldCompletionOrRestart() {
        val leases = RecognitionResourceLease()
        var oldCompletions = 0
        fun discardOutput() {
            val discarded = LocalSpeechOutput(factory, focus, leases = leases)
            discarded.setForeground(true); discarded.setEnabled(true)
            discarded.speak("Synthetic old final", "en-US") { oldCompletions++ }; drain(); factory.ready(true); drain()
            factory.sdk.shutdownFailures = 1
            discarded.close(); drain()
        }
        discardOutput()
        val held = leases.current()!!
        assertFalse(factory.sdk.shutdown); assertEquals(1, factory.sdk.shutdownCalls)
        factory.sdk.beforeShutdown = {
            val unlocked = java.util.concurrent.CountDownLatch(1)
            Thread { leases.current(); unlocked.countDown() }.start()
            assertTrue(unlocked.await(2, java.util.concurrent.TimeUnit.SECONDS))
        }
        val nextFactory = Factory()
        val nextFocus = Focus()
        val replacement = LocalSpeechOutput(nextFactory, nextFocus, leases = leases)
        replacement.setForeground(true); replacement.setEnabled(true)
        replacement.speak("Synthetic replacement final", "en-US") {}; drain(); nextFactory.ready(true); drain()
        assertTrue(factory.sdk.shutdown); assertEquals(2, factory.sdk.shutdownCalls)
        assertEquals(1, focus.releases); assertEquals(SpeechPhase.SPEAKING, replacement.snapshot().phase)
        assertEquals(listOf("Synthetic replacement final"), nextFactory.sdk.spoken)
        assertNotSame(held, leases.current())
        val admitted = leases.current()
        factory.sdk.finished(null); drain()
        assertEquals(0, oldCompletions); assertSame(admitted, leases.current())
        replacement.stop(); drain(); assertNull(leases.current())
    }

    @Test fun disabledPreparationPublishesInventoryWithoutPrimingOrHoldingResources() {
        val leases = RecognitionResourceLease()
        val speech = LocalSpeechOutput(factory, focus, leases = leases)
        speech.setForeground(true); speech.prepare("en-US"); drain()
        factory.ready(true); drain()
        val state = speech.snapshot()
        assertFalse(state.enabled)
        assertEquals(SpeechPhase.IDLE, state.phase)
        assertNull(state.error)
        assertTrue(state.localVoiceAvailable)
        assertTrue(state.voiceAvailable)
        assertEquals("synthetic-local", speech.options().ttsVoices.single().name)
        assertTrue(factory.sdk.shutdown)
        assertEquals(1, factory.sdk.shutdownCalls)
        assertTrue(factory.sdk.spoken.isEmpty())
        assertNull(leases.current())
        assertEquals(0, focus.requests)
        assertEquals(0, factory.sdk.warmups.size)
        speech.close(); drain()
    }

    @Test fun prepareTriggersWarmupOnReadySession() {
        enable()
        output.prepare("en-US"); drain()
        factory.ready(true); drain()
        assertEquals(listOf(factory.sdk.offered.single()), factory.sdk.warmups)
        output.close(); drain()
    }

    @Test fun preparationPassesExplicitResolvedVoiceAndPlaybackDoesNotPrimeAgain() {
        val australian = LocalVoice("synthetic-au-selected", Locale.forLanguageTag("en-AU"), false, true)
        factory.sdk.offered = listOf(LocalVoice("synthetic-us-first", Locale.US, false, true),
            australian.copy(name = "synthetic-au-other"), australian)
        val speech = LocalSpeechOutput(factory, focus, configuration = {
            SpeechAdmission(SpeechSelection(language = "en-AU", ttsVoice = australian.name), false)
        })
        speech.setForeground(true); speech.setEnabled(true); speech.prepare("en-US"); drain()
        factory.ready(true); drain()
        assertEquals(listOf(australian), factory.sdk.warmups)
        assertEquals(0, focus.requests)
        speech.speakStreamChunk("Synthetic first clause.", "en-US", flush = true); drain()
        assertEquals(listOf(australian), factory.sdk.warmups)
        assertEquals(listOf("Synthetic first clause."), factory.sdk.spoken)
        speech.close(); drain()
    }

    @Test fun directPlaybackAndPlaybackTakingOverPendingPreparationDoNotInsertWarmupAheadOfSpeech() {
        enable()
        output.prepare("en-US"); drain()
        output.speak("Synthetic pending answer.", "en-US") {}; drain()
        factory.ready(true); drain()
        assertTrue(factory.sdk.warmups.isEmpty())
        factory.sdk.finished(null); drain()
        output.speak("Synthetic next answer.", "en-US") {}; drain()
        assertTrue(factory.sdk.warmups.isEmpty())
        output.close(); drain()
    }

    @Test fun disablingBeforeFirstPostedStreamInitializationSettlesWithoutAllocatingSdk() {
        enable()
        val done = mutableListOf<String?>()
        output.speakStreamChunk("Synthetic first sentence.", "en-US", flush = true, keepOpen = true, onDone = done::add)
        output.setEnabled(false)
        drain()
        assertEquals(listOf("speech_cancelled"), done)
        assertEquals(0, factory.creates)
        output.close(); drain()
        assertEquals(1, done.size)
    }

    @Test fun openStreamRetainsLeaseAcrossStarvationAndQueuesLaterBurstExactlyOnce() {
        val leases = RecognitionResourceLease()
        val speech = LocalSpeechOutput(factory, focus, leases = leases)
        speech.setForeground(true); speech.setEnabled(true); drain()
        val done = mutableListOf<String?>()
        speech.speakStreamChunk("Synthetic first sentence.", "en-US", flush = true, keepOpen = true, onDone = done::add)
        drain(); factory.ready(true); drain()
        val firstLease = leases.current()
        factory.sdk.terminals[0](null); drain()
        assertSame(firstLease, leases.current())
        assertEquals(SpeechPhase.SPEAKING, speech.snapshot().phase)
        assertEquals(0, focus.releases)
        speech.speakStreamChunk("Synthetic second sentence.", "en-US", flush = false, onDone = done::add)
        speech.speakStreamChunk("Synthetic third sentence.", "en-US", flush = false, onDone = done::add)
        speech.finishStream(); drain()
        assertEquals(listOf("Synthetic first sentence.", "Synthetic second sentence.", "Synthetic third sentence."), factory.sdk.spoken)
        assertEquals(1, focus.requests)
        factory.sdk.terminals[1](null); factory.sdk.terminals[2](null); drain()
        assertEquals(listOf<String?>(null, null, null), done)
        assertEquals(1, focus.releases)
        assertNull(leases.current())
        speech.close(); drain()
    }

    @Test fun disableFocusLossAndTimeoutSettleEveryActiveStreamCallbackOnce() {
        for (retire in listOf("disable", "focus", "timeout")) {
            val localFactory = Factory()
            val localFocus = Focus()
            val leases = RecognitionResourceLease()
            val speech = LocalSpeechOutput(localFactory, localFocus, timeoutMillis = 50, leases = leases)
            speech.setForeground(true); speech.setEnabled(true); drain()
            val done = mutableListOf<String?>()
            speech.speakStreamChunk("Synthetic active first.", "en-US", flush = true, keepOpen = true, onDone = done::add)
            drain(); localFactory.ready(true); drain()
            speech.speakStreamChunk("Synthetic queued second.", "en-US", flush = false, onDone = done::add); drain()
            when (retire) {
                "disable" -> speech.setEnabled(false)
                "focus" -> localFocus.lost()
                else -> shadowOf(Looper.getMainLooper()).idleFor(Duration.ofMillis(50))
            }
            drain()
            assertEquals(retire, 2, done.size)
            assertTrue(done.all { it != null })
            assertNull(leases.current())
            localFactory.sdk.terminals.forEach { it(null) }; drain()
            assertEquals(retire, 2, done.size)
            speech.close(); drain()
        }
    }

    @Test fun speakStreamChunkHoldsSingleLeaseAndFocusAcrossFlushAndQueuedAddChunks() {
        val leases = RecognitionResourceLease()
        val speech = LocalSpeechOutput(factory, focus, leases = leases)
        speech.setForeground(true); speech.setEnabled(true); drain()
        val done = mutableListOf<String?>()
        speech.speakStreamChunk("First sentence.", "en-US", flush = true, onDone = done::add)
        drain(); factory.ready(true); drain()
        speech.speakStreamChunk("Second sentence.", "en-US", flush = false, onDone = done::add)
        drain()
        assertEquals(listOf("First sentence.", "Second sentence."), factory.sdk.spoken)
        assertEquals(
            listOf(android.speech.tts.TextToSpeech.QUEUE_FLUSH, android.speech.tts.TextToSpeech.QUEUE_ADD),
            factory.sdk.queueModes,
        )
        assertEquals(1, focus.requests)
        assertEquals(0, focus.releases)
        assertNotNull(leases.current())
        factory.sdk.terminals[0](null); drain()
        assertEquals(listOf<String?>(null), done)
        assertEquals(0, focus.releases)
        assertNotNull(leases.current())
        assertEquals(SpeechPhase.SPEAKING, speech.snapshot().phase)
        factory.sdk.terminals[1](null); drain()
        assertEquals(listOf<String?>(null, null), done)
        assertEquals(1, focus.releases)
        assertNull(leases.current())
        assertEquals(SpeechPhase.IDLE, speech.snapshot().phase)
        speech.close(); drain()
    }
}
