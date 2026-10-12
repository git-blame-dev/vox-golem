package dev.gitblame.voxgolem.assistant.phone

import android.os.Looper
import dev.gitblame.voxgolem.assistant.recognition.LocalRecognizerEvents
import dev.gitblame.voxgolem.assistant.recognition.LocalRecognizerFactory
import dev.gitblame.voxgolem.assistant.recognition.LocalRecognizerSession
import dev.gitblame.voxgolem.assistant.recognition.RecognitionResourceLease
import dev.gitblame.voxgolem.assistant.recognition.SharedDictationOwner
import dev.gitblame.voxgolem.assistant.runtime.AssistantRuntime
import org.json.JSONObject
import org.junit.After
import org.junit.Assert.*
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment
import org.robolectric.Shadows.shadowOf
import org.robolectric.annotation.Config
import org.robolectric.annotation.LooperMode
import org.robolectric.shadows.ShadowSpeechRecognizer
import java.time.Duration

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35], manifest = Config.NONE)
@LooperMode(LooperMode.Mode.PAUSED)
class PhoneShortcutSessionTest {
    private class Backend : PhoneSpeechBackend {
        var busy = false
        val drafts = mutableListOf<String>()
        var stops = 0
        override fun busy() = busy
        override fun saveSpeechDraft(text: String) { drafts += text }
        override fun stopAll() { stops++ }
        override fun nativePreferences() = JSONObject().put("tts_enabled", false)
        override var speechStatus: JSONObject? = null
        override var onPreferencesChanged: ((Boolean) -> Unit)? = null
    }
    private class Factory : LocalRecognizerFactory {
        var delaySupport = false
        val sessions = mutableListOf<Session>()
        override fun available() = true
        override fun create(): LocalRecognizerSession = Session().also { sessions += it }
        inner class Session : LocalRecognizerSession {
            lateinit var support: (Boolean) -> Unit
            lateinit var events: LocalRecognizerEvents
            var starts = 0
            var stops = 0
            var destroyed = false
            override fun checkLanguage(language: String, result: (Boolean) -> Unit) { support = result; if (!delaySupport) result(true) }
            override fun start(language: String, events: LocalRecognizerEvents) { this.events = events; starts++ }
            override fun stop() { stops++ }
            override fun cancel() {}
            override fun destroy() { destroyed = true }
        }
    }
    private val backend = Backend()
    private val factory = Factory()
    private val leases = RecognitionResourceLease()
    private val events = mutableListOf<JSONObject>()
    private lateinit var phone: PhoneSpeechSession
    private fun drain() = shadowOf(Looper.getMainLooper()).idle()
    private fun advance() = shadowOf(Looper.getMainLooper()).idleFor(Duration.ofMillis(301))
    private fun start() { phone.start("synthetic-session", false, "Ignored old conversation draft", { events += JSONObject(it.toString()) }, "11111111-1111-4111-8111-111111111111"); drain() }
    private fun kind(kind: String) = events.filter { it.getString("kind") == kind }

    @Before fun prepare() {
        AssistantRuntime.configureCaptureResourceAdmission(true)
        ShadowSpeechRecognizer.setIsOnDeviceRecognitionAvailable(true)
        phone = PhoneSpeechSession(RuntimeEnvironment.getApplication(), backend, SharedDictationOwner(factory, { true }, leases))
        phone.foreground(true); drain()
    }
    @After fun close() { phone.close(); drain(); AssistantRuntime.configureCaptureResourceAdmission(false); NewVoiceShortcutLauncher.actions.background() }

    @Test fun nativeProducerReplacesPartialsAccumulatesFinalsAndSubmitsOnlyOnceAfterStop() {
        start(); val first = factory.sessions.single()
        assertTrue(kind("shortcut_ready").isEmpty()); assertTrue(backend.drafts.isEmpty())
        first.events.partial("Synthetic replaced"); drain()
        first.events.partial("Synthetic first"); drain()
        assertEquals(1, kind("shortcut_ready").size)
        assertTrue(backend.drafts.isEmpty())
        phone.shortcutPrepared("synthetic-session")
        assertEquals("Synthetic first", backend.drafts.last())
        first.events.final("Synthetic first final"); drain()
        assertTrue(kind("submit").isEmpty()); assertTrue(first.destroyed)
        advance(); assertEquals(2, factory.sessions.size)
        val second = factory.sessions.last()
        second.events.partial("uncertain"); drain()
        phone.stopAndSendShortcut(); drain(); assertEquals(1, second.stops)
        assertTrue(kind("submit").isEmpty())
        second.events.final("second final"); drain()
        first.events.final("Synthetic stale"); second.events.final("Synthetic duplicate"); phone.stopAndSendShortcut(); drain(); advance()
        val submit = kind("submit").single()
        assertEquals("Synthetic first final second final", submit.getString("text"))
        assertEquals("synthetic-session", submit.getString("sessionId"))
        assertTrue(submit.getString("requestId").startsWith("shortcut-"))
        assertEquals(2, factory.sessions.size)
    }
    @Test fun stopDuringInstalledLanguageAdmissionCannotStartTheMicrophoneLater() {
        factory.delaySupport = true; start()
        val checking = factory.sessions.single()
        phone.stopAndSendShortcut(); drain(); checking.support(true); drain()
        assertEquals(0, checking.starts); assertTrue(checking.destroyed)
        assertEquals("", kind("shortcut_review").single().getString("draft"))
        assertTrue(kind("submit").isEmpty()); assertNull(leases.current())
    }
    @Test fun timeoutRetainsThePartialAndCannotPublishAnUncertainSubmission() {
        start(); val sdk = factory.sessions.single()
        sdk.events.partial("Synthetic uncertain words"); drain()
        phone.stopAndSendShortcut(); drain()
        shadowOf(Looper.getMainLooper()).idleFor(Duration.ofSeconds(6))
        sdk.events.final("Synthetic late final"); drain()
        assertEquals("Synthetic uncertain words", kind("shortcut_review").single().getString("draft"))
        assertTrue(kind("submit").isEmpty()); assertTrue(sdk.destroyed)
    }
    @Test fun backgroundCancelsPendingSegmentRestartAndNeverResumesOrSends() {
        start(); factory.sessions.single().events.final("Synthetic segment"); drain()
        phone.foreground(false); drain(); phone.foreground(true); drain(); advance()
        assertEquals(1, factory.sessions.size); assertTrue(kind("submit").isEmpty())
    }
    @Test fun busyProviderAndExistingCaptureLeaseAreNeverCancelledByShortcutAdmission() {
        backend.busy = true; val stops = backend.stops
        try { start(); fail("existing provider owns admission") } catch (failure: AssistantPhoneFailure) { assertEquals("busy", failure.code) }
        assertEquals(stops, backend.stops); assertTrue(factory.sessions.isEmpty())
        backend.busy = false
        val capture = leases.tryAcquire(RecognitionResourceLease.Kind.CAPTURE_LIVE)!!
        try {
            start(); assertSame(capture, leases.current()); assertTrue(factory.sessions.isEmpty())
            assertTrue(kind("submit").isEmpty()); assertEquals("recognition_busy", kind("shortcut_review").single().getString("reason"))
        } finally { capture.close() }
    }
    @Test fun recognitionFailureStopsWithoutAnotherSessionOrSubmission() {
        start(); val sdk = factory.sessions.single()
        sdk.events.partial("Synthetic retained words"); drain(); sdk.events.error("recognition_error_5"); drain(); advance()
        assertEquals(1, factory.sessions.size); assertTrue(kind("submit").isEmpty())
        assertEquals("Synthetic retained words", kind("shortcut_review").single().getString("draft"))
    }
    @Test fun anOversizedFirstHypothesisCannotReserveAnEmptyConversation() {
        start(); factory.sessions.single().events.partial("é".repeat(16385)); drain()
        assertTrue(kind("shortcut_ready").isEmpty()); assertTrue(kind("submit").isEmpty())
        assertEquals("", kind("shortcut_review").single().getString("draft"))
    }
}
