package dev.gitblame.voxgolem.assistant.inputtransport

import android.content.Intent
import android.media.AudioFormat
import android.os.Bundle
import android.os.Looper
import android.os.ParcelFileDescriptor
import android.speech.RecognitionListener
import android.speech.RecognizerIntent
import android.speech.SpeechRecognizer
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment
import org.robolectric.Shadows.shadowOf
import org.robolectric.annotation.Config
import org.robolectric.annotation.LooperMode
import org.robolectric.shadows.ShadowSpeechRecognizer
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import java.io.IOException
import java.time.Duration

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35], manifest = Config.NONE)
@LooperMode(LooperMode.Mode.PAUSED)
class AndroidSuppliedAudioInputTest {
    private class FakeSdk : SuppliedRecognizerSdk {
        lateinit var callback: RecognitionListener
        var queried: Intent? = null
        var started: Intent? = null
        var languages: List<String>? = listOf("en-US")
        var closed = false
        var listenerFails = false
        override fun listener(listener: RecognitionListener) { if (listenerFails) error("synthetic listener failure"); callback = listener }
        override fun support(intent: Intent, result: (List<String>?) -> Unit) {
            check(::callback.isInitialized); queried = intent; result(languages)
        }
        override fun start(intent: Intent) { started = intent }
        override fun close() { closed = true }
    }
    private class FakePipe : InputPipe {
        private val ends = ParcelFileDescriptor.createPipe()
        override val readEnd = ends[0]
        override var sourceClosed = false
        var state: InputEofState? = null
        override val eofState get() = state ?: if (sourceClosed) InputEofState.CLOSED else InputEofState.OPEN
        var finishing = false
        var closed = false
        var sourceEnd: ((Boolean) -> Unit)? = null
        override fun onSourceEnd(observer: (Boolean) -> Unit) { sourceEnd = observer }
        override fun start(error: () -> Unit) {}
        override fun append(bytes: ByteArray) = !finishing
        override fun finish() { finishing = true }
        override fun close() { ends.forEach { it.close() }; closed = true }
    }
    private class Events : SuppliedInputEvents {
        val partials = mutableListOf<String>()
        val finals = mutableListOf<String>()
        val errors = mutableListOf<String>()
        var ends = 0
        override fun endOfSpeech() { ends++ }
        override fun partial(text: String) { partials += text }
        override fun final(text: String) { finals += text }
        override fun error(code: String) { errors += code }
    }
    private fun result(text: String) = Bundle().apply { putStringArrayList(SpeechRecognizer.RESULTS_RECOGNITION, arrayListOf(text)) }
    @Test fun oversizedPartialAndCumulativeDraftsFailBeforePublication() {
        for (kind in listOf("partial", "segment", "cumulativePartial", "cumulativeSegment")) {
            val sdk = FakeSdk(); val pipe = FakePipe(); val events = Events()
            val session = AndroidSuppliedAudioSession("en-US", sdk, pipe) { true }
            try {
                session.checkSupport {}; session.start(events)
                if (kind.startsWith("cumulative")) sdk.callback.onSegmentResults(result("a".repeat(4096)))
                val text = if (kind.startsWith("cumulative")) "b".repeat(4096) else "界".repeat(2731)
                if (kind.endsWith("Segment") || kind == "segment") sdk.callback.onSegmentResults(result(text))
                else sdk.callback.onPartialResults(result(text))
                assertEquals(kind, listOf("transcript_limit"), events.errors)
                assertEquals(if (kind.startsWith("cumulative")) 1 else 0, events.partials.size)
                pipe.sourceClosed = true; sdk.callback.onEndOfSegmentedSession()
                assertTrue(events.finals.isEmpty())
            } finally { session.close() }
        }
    }
    @Test fun finalUtf8BudgetAppliesBeforeRetentionInEverySuccessfulEofState() {
        for (state in listOf(InputEofState.OPEN, InputEofState.CLOSING, InputEofState.CLOSED)) {
            for (text in listOf("a".repeat(8193), "界".repeat(2731), " ".repeat(8193))) {
                val sdk = FakeSdk(); val pipe = FakePipe().apply { this.state = state }; val events = Events()
                val session = AndroidSuppliedAudioSession("en-US", sdk, pipe, SuppliedAudioMode.UTTERANCE) { true }
                try {
                    session.checkSupport {}; session.start(events)
                    sdk.callback.onResults(result(text))
                    assertEquals(state.toString(), listOf("transcript_limit"), events.errors)
                    assertEquals(0, events.ends)
                    pipe.state = InputEofState.CLOSED; pipe.sourceEnd!!.invoke(true)
                    sdk.callback.onResults(result("synthetic late final"))
                    assertTrue(events.finals.isEmpty())
                    val pending = session.javaClass.getDeclaredField("pendingFinal").apply { isAccessible = true }
                    assertNull(pending.get(session))
                } finally { session.close() }
            }
        }
    }
    @Test fun exactAsciiAndMultibyteBudgetsPreserveBlankJoinAndNormalEofCompletion() {
        for (text in listOf("a".repeat(8192), "界".repeat(2730) + "ab", "😀".repeat(2048))) {
            val sdk = FakeSdk(); val pipe = FakePipe(); val events = Events()
            val session = AndroidSuppliedAudioSession("en-US", sdk, pipe) { true }
            try {
                session.checkSupport {}; session.start(events)
                sdk.callback.onSegmentResults(result("  "))
                sdk.callback.onSegmentResults(result(text))
                sdk.callback.onPartialResults(result("\t"))
                assertEquals(text, events.partials.last())
                session.finish(); pipe.sourceClosed = true; sdk.callback.onEndOfSegmentedSession()
                assertEquals(listOf(text), events.finals); assertTrue(events.errors.isEmpty())
            } finally { session.close() }
        }
    }
    @Test fun oversizedCallbacksRespectApprovalCloseAndFailedPipeGuards() {
        for (guard in listOf("approval", "close", "pipe")) {
            var approved = true
            val sdk = FakeSdk(); val pipe = FakePipe(); val events = Events()
            val session = AndroidSuppliedAudioSession("en-US", sdk, pipe, SuppliedAudioMode.UTTERANCE) { approved }
            try {
                session.checkSupport {}; session.start(events)
                when (guard) {
                    "approval" -> approved = false
                    "close" -> session.close()
                    "pipe" -> pipe.state = InputEofState.FAILED
                }
                sdk.callback.onResults(result("a".repeat(8193)))
                assertEquals(when (guard) {
                    "approval" -> listOf("supplied_audio_approval_lost")
                    "pipe" -> listOf("audio_pipe_failed")
                    else -> emptyList<String>()
                }, events.errors)
                assertTrue(events.finals.isEmpty()); assertTrue(events.partials.isEmpty())
            } finally { session.close() }
        }
    }
    @Test fun localUtteranceIntentAndFinalUseTheSameSourceAndWaitForActualEof() {
        val sdk = FakeSdk(); val pipe = FakePipe(); val events = Events()
        val session = AndroidSuppliedAudioSession("en-US", sdk, pipe, SuppliedAudioMode.UTTERANCE) { true }
        session.checkSupport { assertTrue(it) }; session.start(events)
        assertSame(sdk.queried, sdk.started)
        assertFalse(sdk.started!!.hasExtra(RecognizerIntent.EXTRA_SEGMENTED_SESSION))
        sdk.callback.onResults(result("Synthetic endpointer final"))
        assertEquals(1, events.ends); assertTrue(events.finals.isEmpty())
        session.finish(); assertTrue(events.finals.isEmpty())
        pipe.sourceClosed = true; pipe.sourceEnd!!.invoke(true)
        assertEquals(listOf("Synthetic endpointer final"), events.finals)
        sdk.callback.onResults(result("Synthetic duplicate")); assertEquals(1, events.finals.size)
        session.close()
    }
    @Test fun localUtteranceEofTimeoutCannotCreateAnAuthoritativeFinal() {
        val sdk = FakeSdk(); val pipe = FakePipe(); val events = Events()
        val session = AndroidSuppliedAudioSession("en-US", sdk, pipe, SuppliedAudioMode.UTTERANCE) { true }
        session.checkSupport {}; session.start(events)
        sdk.callback.onResults(result("Synthetic premature final"))
        shadowOf(Looper.getMainLooper()).idleFor(Duration.ofSeconds(3))
        assertEquals(listOf("source_eof_timeout"), events.errors); assertTrue(events.finals.isEmpty())
        pipe.sourceClosed = true; pipe.sourceEnd!!.invoke(true)
        assertTrue(events.finals.isEmpty()); session.close()
    }
    @Test fun liveApprovalLossBeforeEofCannotCreateAnAuthoritativeFinal() {
        var approved = true
        val sdk = FakeSdk(); val pipe = FakePipe(); val events = Events()
        val session = AndroidSuppliedAudioSession("en-US", sdk, pipe, SuppliedAudioMode.UTTERANCE) { approved }
        try {
            session.checkSupport {}; session.start(events)
            sdk.callback.onResults(result("Synthetic pending final"))
            assertTrue(events.errors.isEmpty()); assertTrue(events.finals.isEmpty())
            approved = false
            assertFalse(session.appendFromWorker(byteArrayOf(0, 0)))
            pipe.sourceClosed = true; pipe.sourceEnd!!.invoke(true)
            assertEquals(listOf("supplied_audio_approval_lost"), events.errors)
            assertTrue(events.finals.isEmpty())
        } finally { session.close() }
    }
    @Test fun exactDescriptorIntentIsQueriedAndStartedWithListenerFirst() {
        val sdk = FakeSdk(); val pipe = FakePipe(); val events = Events()
        val session = AndroidSuppliedAudioSession("en-US", sdk, pipe) { true }
        session.checkSupport { assertTrue(it) }
        assertNull(sdk.started)
        session.start(events)
        assertSame(sdk.queried, sdk.started)
        val intent = sdk.started!!
        assertEquals(RecognizerIntent.ACTION_RECOGNIZE_SPEECH, intent.action)
        assertEquals("en-US", intent.getStringExtra(RecognizerIntent.EXTRA_LANGUAGE))
        assertEquals(1, intent.getIntExtra(RecognizerIntent.EXTRA_AUDIO_SOURCE_CHANNEL_COUNT, -1))
        assertEquals(AudioFormat.ENCODING_PCM_16BIT, intent.getIntExtra(RecognizerIntent.EXTRA_AUDIO_SOURCE_ENCODING, -1))
        assertEquals(16000, intent.getIntExtra(RecognizerIntent.EXTRA_AUDIO_SOURCE_SAMPLING_RATE, -1))
        assertEquals(RecognizerIntent.EXTRA_AUDIO_SOURCE, intent.getStringExtra(RecognizerIntent.EXTRA_SEGMENTED_SESSION))
        assertTrue(intent.getBooleanExtra(RecognizerIntent.EXTRA_PREFER_OFFLINE, false))
        assertSame(pipe.readEnd, intent.getParcelableExtra(RecognizerIntent.EXTRA_AUDIO_SOURCE, ParcelFileDescriptor::class.java))
        sdk.callback.onPartialResults(result("synthetic draft"))
        assertEquals(listOf("synthetic draft"), events.partials)
        session.finish()
        assertTrue(pipe.finishing)
        assertTrue(events.finals.isEmpty())
        pipe.sourceClosed = true
        sdk.callback.onResults(result("synthetic final"))
        assertEquals(listOf("synthetic final"), events.finals)
        session.close()
        sdk.callback.onPartialResults(result("synthetic late"))
        assertEquals(1, events.partials.size)
        assertTrue(pipe.closed && sdk.closed)
    }
    @Test fun exactInstalledLanguageAndDeviceGateRequired() {
        for (languages in listOf(null, emptyList(), listOf("en"), listOf("fr-FR"))) {
            val sdk = FakeSdk().apply { this.languages = languages }; val pipe = FakePipe()
            val session = AndroidSuppliedAudioSession("en-US", sdk, pipe) { true }
            session.checkSupport { assertFalse(it) }
            assertThrows(IllegalStateException::class.java) { session.start(Events()) }
            assertNull(sdk.started); session.close()
        }
        var gate = true
        val sdk = FakeSdk(); val pipe = FakePipe()
        val session = AndroidSuppliedAudioSession("en-US", sdk, pipe) { gate }
        session.checkSupport { assertTrue(it) }; gate = false
        assertThrows(IllegalStateException::class.java) { session.start(Events()) }
        assertNull(sdk.started); session.close()
    }
    @Test fun segmentsRemainDraftUntilDrainedSourceAndTerminalCallback() {
        val sdk = FakeSdk(); val pipe = FakePipe(); val events = Events()
        val session = AndroidSuppliedAudioSession("en-US", sdk, pipe) { true }
        session.checkSupport {}; session.start(events)
        sdk.callback.onSegmentResults(result("synthetic first"))
        sdk.callback.onSegmentResults(result("synthetic second"))
        assertEquals("synthetic first synthetic second", events.partials.last())
        assertTrue(events.finals.isEmpty())
        session.finish(); pipe.sourceClosed = true
        sdk.callback.onEndOfSegmentedSession()
        assertEquals(listOf("synthetic first synthetic second"), events.finals)
        session.close()
    }
    @Test fun prematureFinalAndRemote21ErrorFailWithoutFinal() {
        val sdk = FakeSdk(); val pipe = FakePipe(); val events = Events()
        val session = AndroidSuppliedAudioSession("en-US", sdk, pipe) { true }
        session.checkSupport {}; session.start(events)
        sdk.callback.onResults(result("synthetic premature"))
        assertTrue(events.finals.isEmpty()); assertEquals(listOf("provider_ended_before_source"), events.errors)
        session.close()
        val secondSdk = FakeSdk(); val secondPipe = FakePipe(); val secondEvents = Events()
        val second = AndroidSuppliedAudioSession("en-US", secondSdk, secondPipe) { true }
        second.checkSupport {}; second.start(secondEvents)
        secondSdk.callback.onError(21)
        assertEquals(listOf("recognition_error_21"), secondEvents.errors)
        assertTrue(secondEvents.finals.isEmpty()); second.close()
    }
    @Test fun sdkMethodsRequireMainAndBackgroundCallbacksDispatchToMain() {
        val sdk = FakeSdk(); val pipe = FakePipe(); val events = Events()
        val session = AndroidSuppliedAudioSession("en-US", sdk, pipe) { true }
        session.checkSupport {}; session.start(events)
        val executor = java.util.concurrent.Executors.newSingleThreadExecutor()
        try {
            val failure = executor.submit<Throwable?> { runCatching { session.finish() }.exceptionOrNull() }.get(2, TimeUnit.SECONDS)
            assertTrue(failure is IllegalStateException)
            executor.submit { sdk.callback.onPartialResults(result("synthetic worker draft")) }.get(2, TimeUnit.SECONDS)
            assertTrue(events.partials.isEmpty())
            shadowOf(Looper.getMainLooper()).idle()
            assertEquals(listOf("synthetic worker draft"), events.partials)
        } finally { session.close(); executor.shutdownNow(); assertTrue(executor.awaitTermination(2, TimeUnit.SECONDS)) }
    }
    @Test fun defaultDeviceGateDeniesEvenAvailableProvider() {
        ShadowSpeechRecognizer.setIsOnDeviceRecognitionAvailable(true)
        val factory = AndroidSuppliedAudioFactory(RuntimeEnvironment.getApplication())
        assertFalse(factory.available("en-US"))
        assertThrows(IllegalStateException::class.java) { factory.open("en-US") }
        assertNull(ShadowSpeechRecognizer.getLatestSpeechRecognizer())
    }
    @Test fun listenerSetupFailureStillHasOwnedSessionForRetirement() {
        val sdk = FakeSdk().apply { listenerFails = true }; val pipe = FakePipe()
        val session = AndroidSuppliedAudioSession("en-US", sdk, pipe) { true }
        assertThrows(IllegalStateException::class.java) { session.checkSupport {} }
        session.close()
        assertTrue(pipe.closed && sdk.closed)
        assertNull(sdk.started)
    }
    @Test @Config(sdk = [24, 32]) fun pre33NeverAllocatesRecognizerEvenIfGateClaimsSupport() {
        val factory = AndroidSuppliedAudioFactory(RuntimeEnvironment.getApplication(), SuppliedAudioDeviceGate { true })
        assertFalse(factory.available("en-US"))
        assertThrows(IllegalStateException::class.java) { factory.open("en-US") }
        assertNull(ShadowSpeechRecognizer.getLatestSpeechRecognizer())
    }
    @Test @Config(sdk = [24]) fun defaultPipeConfigurationRejectsUnsupportedApiAsUnavailable() {
        val failure = assertThrows(UnsupportedOperationException::class.java) { AndroidInputPipe().close() }
        assertEquals("supplied_audio_pipe_unavailable", failure.message)
        val factory = AndroidSuppliedAudioFactory(RuntimeEnvironment.getApplication(), SuppliedAudioDeviceGate { true })
        assertFalse(factory.available("en-US"))
        assertThrows(IllegalStateException::class.java) { factory.open("en-US") }
        assertNull(ShadowSpeechRecognizer.getLatestSpeechRecognizer())
    }
    @Test @Config(sdk = [24]) fun injectedFakePipeConfigurationNeedsNoDefaultApi30Operation() {
        var configured = 0
        val pipe = AndroidInputPipe(configureWrite = { configured++ })
        try {
            assertEquals(1, configured)
            assertEquals(InputEofState.OPEN, pipe.eofState)
            assertFalse(pipe.sourceClosed)
        } finally { pipe.close() }
    }
    @Test fun sdkFinalDuringSuccessfulRealPipeCloseWaitsForEofPublication() {
        val sdk = FakeSdk(); val events = Events()
        val eofVisible = CountDownLatch(1); val publish = CountDownLatch(1)
        val main = android.os.Handler(Looper.getMainLooper())
        val pipe = AndroidInputPipe(closeWrite = {
            it.close()
            main.post { sdk.callback.onResults(result("synthetic raced final")) }
            eofVisible.countDown()
            check(publish.await(2, TimeUnit.SECONDS))
        }, configureWrite = {})
        val session = AndroidSuppliedAudioSession("en-US", sdk, pipe) { true }
        try {
            session.checkSupport {}; session.start(events); session.finish()
            assertTrue(eofVisible.await(2, TimeUnit.SECONDS)); assertFalse(pipe.sourceClosed)
            shadowOf(Looper.getMainLooper()).idle()
            assertTrue("EOF success has not been published, so terminal must wait", events.errors.isEmpty())
            assertTrue(events.finals.isEmpty())
            publish.countDown()
            val deadline = System.nanoTime() + TimeUnit.SECONDS.toNanos(2)
            while (!pipe.sourceClosed && System.nanoTime() < deadline) Thread.sleep(5)
            assertTrue(pipe.sourceClosed); shadowOf(Looper.getMainLooper()).idle()
            assertEquals(listOf("synthetic raced final"), events.finals)
        } finally { publish.countDown(); session.close() }
    }
    @Test fun failedRealEofCloseCannotPromotePendingSdkFinal() {
        val sdk = FakeSdk(); val events = Events()
        val closing = CountDownLatch(1); val failClose = CountDownLatch(1)
        val main = android.os.Handler(Looper.getMainLooper())
        val pipe = AndroidInputPipe(closeWrite = {
            main.post { sdk.callback.onResults(result("synthetic rejected pending final")) }
            closing.countDown(); check(failClose.await(2, TimeUnit.SECONDS))
            throw IOException("synthetic EOF close failure")
        }, configureWrite = {})
        val session = AndroidSuppliedAudioSession("en-US", sdk, pipe) { true }
        try {
            session.checkSupport {}; session.start(events); session.finish()
            assertTrue(closing.await(2, TimeUnit.SECONDS)); shadowOf(Looper.getMainLooper()).idle()
            assertTrue(events.errors.isEmpty()); assertTrue(events.finals.isEmpty()); assertFalse(pipe.sourceClosed)
            failClose.countDown()
            val deadline = System.nanoTime() + TimeUnit.SECONDS.toNanos(2)
            while (events.errors.isEmpty() && System.nanoTime() < deadline) { shadowOf(Looper.getMainLooper()).idle(); Thread.sleep(5) }
            assertEquals(listOf("audio_pipe_failed"), events.errors)
            assertEquals(InputEofState.FAILED, pipe.eofState); assertFalse(pipe.sourceClosed)
            assertTrue(events.finals.isEmpty())
        } finally { failClose.countDown(); session.close() }
        assertTrue(sdk.closed)
    }
    @Test fun stalledRealEofPublicationTimesOutAndLateFinalIsFencedAfterSdkCleanup() {
        val sdk = FakeSdk(); val events = Events()
        val closing = CountDownLatch(1); val publish = CountDownLatch(1)
        val main = android.os.Handler(Looper.getMainLooper())
        val pipe = AndroidInputPipe(closeWrite = {
            it.close(); main.post { sdk.callback.onResults(result("synthetic timeout final")) }
            closing.countDown()
            val deadline = System.nanoTime() + TimeUnit.SECONDS.toNanos(2)
            while (publish.count != 0L) {
                try { check(publish.await(deadline - System.nanoTime(), TimeUnit.NANOSECONDS)) }
                catch (_: InterruptedException) { /* Simulate a native close which cannot be interrupted. */ }
            }
        }, configureWrite = {})
        val session = AndroidSuppliedAudioSession("en-US", sdk, pipe) { true }
        try {
            session.checkSupport {}; session.start(events); session.finish()
            assertTrue(closing.await(2, TimeUnit.SECONDS)); shadowOf(Looper.getMainLooper()).idle()
            assertFalse(pipe.sourceClosed); assertTrue(events.finals.isEmpty())
            shadowOf(Looper.getMainLooper()).idleFor(Duration.ofMillis(250))
            assertEquals(listOf("source_eof_timeout"), events.errors)
            assertThrows(IllegalStateException::class.java) { session.close() }
            assertTrue(sdk.closed)
            publish.countDown(); session.close()
            sdk.callback.onResults(result("synthetic late final")); shadowOf(Looper.getMainLooper()).idle()
            assertTrue(events.finals.isEmpty())
        } finally { publish.countDown(); session.close() }
    }
    @Test fun finishRequestAloneNeverCountsAsSuccessfulSourceEof() {
        val sdk = FakeSdk(); val pipe = FakePipe(); val events = Events()
        val session = AndroidSuppliedAudioSession("en-US", sdk, pipe) { true }
        try {
            session.checkSupport {}; session.start(events); session.finish()
            sdk.callback.onResults(result("synthetic genuinely early final"))
            assertEquals(listOf("provider_ended_before_source"), events.errors)
            assertTrue(events.finals.isEmpty()); assertFalse(pipe.sourceClosed)
        } finally { session.close() }
    }
}
