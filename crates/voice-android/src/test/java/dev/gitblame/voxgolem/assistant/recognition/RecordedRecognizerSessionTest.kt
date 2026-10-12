package dev.gitblame.voxgolem.assistant.recognition

import android.os.Looper
import dev.gitblame.voxgolem.assistant.inputtransport.*
import dev.gitblame.voxgolem.assistant.speech.*
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.Shadows.shadowOf
import org.robolectric.annotation.Config
import org.robolectric.annotation.LooperMode

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35], manifest = Config.NONE)
@LooperMode(LooperMode.Mode.PAUSED)
class RecordedRecognizerSessionTest {
    private class Input : SuppliedInputSession {
        lateinit var events: SuppliedInputEvents
        var finishes = 0
        var closes = 0
        override fun checkSupport(result: (Boolean) -> Unit) = result(true)
        override fun start(events: SuppliedInputEvents) { this.events = events }
        override fun append(pcm: ByteArray) = true
        override fun finish() { finishes++ }
        override fun close() { closes++ }
    }
    private class Audio : RecordedAudioCapture {
        lateinit var stopped: (Result<RecordedAudioResult>) -> Unit
        var finishes = 0
        var closes = 0
        var retirementFails = false
        val recognized = mutableListOf<String?>()
        override fun start(pcm: (ByteArray) -> Boolean, ready: () -> Unit, stopped: (Result<RecordedAudioResult>) -> Unit) {
            this.stopped = stopped; assertTrue(pcm(byteArrayOf(0, 0))); ready()
        }
        override fun finish() { finishes++ }
        override fun recognition(text: String?, error: String?) { recognized += text }
        override fun close() { closes++; if (retirementFails) error("synthetic retirement failure") }
    }
    private val input = Input()
    private val audio = Audio()
    private val artifact = RecordedAudioResult("123e4567-e89b-42d3-a456-426614174000", "a".repeat(64), 4)
    private val session = RecordedRecognizerSession(input, audio, RecognitionRoute("synthetic.provider", "en-US"))
    private val finals = mutableListOf<String>()
    private val errors = mutableListOf<String>()
    private val events = object : LocalRecognizerEvents {
        override fun partial(text: String) {}
        override fun final(text: String) { finals += text }
        override fun error(code: String) { errors += code }
    }
    private fun drain() = shadowOf(Looper.getMainLooper()).idle()

    @Test fun authoritativeTextWaitsForSavedAudioAndSettlesOnce() {
        session.checkLanguage("en-US") { assertTrue(it) }; session.start("en-US", events); drain()
        input.events.partial("Synthetic draft"); input.events.final("Synthetic final")
        assertTrue(finals.isEmpty()); assertNull(session.recordedAudio)
        audio.stopped(Result.success(artifact)); drain()
        assertEquals(listOf("Synthetic final"), finals)
        assertEquals(listOf("Synthetic final"), audio.recognized)
        assertEquals(artifact, session.recordedAudio)
        input.events.final("Synthetic duplicate"); audio.stopped(Result.success(artifact)); drain()
        assertEquals(1, finals.size); assertEquals(1, input.finishes)
        session.destroy(); assertEquals(1, audio.closes)
    }
    @Test fun saveFailureAndCancellationCannotProduceAFinal() {
        session.start("en-US", events)
        input.events.final("Synthetic final")
        audio.stopped(Result.failure(IllegalStateException("Synthetic save failure"))); drain()
        assertEquals(listOf("recording_failed"), errors); assertTrue(finals.isEmpty())
        session.cancel(); audio.stopped(Result.success(artifact)); input.events.final("Synthetic late"); drain()
        assertTrue(finals.isEmpty()); session.destroy()
    }
    @Test fun endpointerStopsOnlyTheRecordingAndSourceFinishWaitsForItsSave() {
        session.start("en-US", events)
        input.events.endOfSpeech(); input.events.endOfSpeech()
        assertEquals(1, audio.finishes); assertEquals(0, input.finishes)
        audio.stopped(Result.success(artifact)); drain()
        assertEquals(1, input.finishes); assertTrue(finals.isEmpty())
        input.events.final("Synthetic final"); assertEquals(1, finals.size); session.destroy()
    }
    @Test fun failedRecorderRetirementKeepsTheSharedEntryLeaseUntilRetry() {
        val leases = RecognitionResourceLease()
        val factory = object : LocalRecognizerFactory {
            override fun available() = true
            override fun create() = session
        }
        val owner = SharedDictationOwner(factory, { true }, leases)
        owner.setForeground(true); owner.start("en-US"); drain()
        audio.retirementFails = true
        owner.stop(); drain()
        assertNotNull(leases.current()); assertEquals("recognition_cleanup_failed", owner.snapshot().error)
        assertTrue(finals.isEmpty())
        audio.retirementFails = false
        assertTrue(leases.retryFailedRetirement()); assertNull(leases.current())
        owner.close(); drain()
    }
    @Test fun noMatchWaitsForAudioSaveAndCannotHideARecordingFailureAsNormalIdle() {
        session.start("en-US", events)
        input.events.error("recognition_error_7")
        assertTrue(errors.isEmpty()); assertEquals(1, audio.finishes)
        audio.stopped(Result.failure(IllegalStateException("Synthetic disk failure"))); drain()
        assertEquals(listOf("recording_failed"), errors); assertTrue(finals.isEmpty())
        session.destroy()
    }
}
