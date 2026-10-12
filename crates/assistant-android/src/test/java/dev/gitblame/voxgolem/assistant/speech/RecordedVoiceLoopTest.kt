package dev.gitblame.voxgolem.assistant.speech

import android.os.Looper
import dev.gitblame.voxgolem.assistant.AssistantTiming
import dev.gitblame.voxgolem.assistant.recognition.*
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
class RecordedVoiceLoopTest {
    private val artifact = RecordedAudioResult("123e4567-e89b-42d3-a456-426614174000", "b".repeat(64), 4)
    private class Session : LocalRecognizerSession {
        lateinit var events: LocalRecognizerEvents
        override val archiveExpected = true
        override var recordedAudio: RecordedAudioResult? = null
        override fun checkLanguage(language: String, result: (Boolean) -> Unit) = result(true)
        override fun start(language: String, events: LocalRecognizerEvents) { this.events = events }
        override fun stop() {}
        override fun cancel() {}
        override fun destroy() {}
    }
    private class Factory : LocalRecognizerFactory {
        val entries = mutableListOf<VoiceCapturePurpose>()
        val sessions = mutableListOf<Session>()
        override fun available() = true
        override fun recordingEnabled() = true
        override fun create() = Session().also { sessions += it }
        override fun createForEntry(purpose: VoiceCapturePurpose, language: String, networkFallback: Boolean,
            timing: AssistantTiming, recording: Boolean): LocalRecognizerSession {
            assertTrue(recording); entries += purpose; return create()
        }
    }
    private class Engine : EnginePort {
        var audio: RecordedAudioResult? = null
        val submissions = mutableListOf<Pair<String, RecordedAudioResult?>>()
        override fun setDraft(text: String) {}
        override fun setRecordedAudio(audio: RecordedAudioResult?) { this.audio = audio }
        override fun sendFinal(text: String, onTerminal: (GenerationResult) -> Unit) { submissions += text to audio }
        override fun stop() {}
    }
    @Test fun askAndTalkUseTheirPurposeAndOnlyFinalCarriesTheSavedArtifactToSubmission() {
        for (policy in VoiceSessionPolicy.entries) {
            val factory = Factory(); val engine = Engine()
            val dictation = SharedDictationOwner(factory, { true }, RecognitionResourceLease(), configuration = {
                SpeechAdmission(SpeechSelection(language = "en-US"), false)
            })
            val output = LocalSpeechOutput(object : LocalTtsFactory {
                override fun create(onReady: (Boolean) -> Unit): LocalTtsSession = error("Disabled output must not initialize TTS")
            }, object : SpeechAudioFocus {
                override fun acquire(onLoss: () -> Unit) = true
                override fun release() {}
            })
            val loop = ConversationLoopController(dictation, output, engine)
            loop.setForeground(true); loop.start("en-US", policy = policy); shadowOf(Looper.getMainLooper()).idle()
            assertEquals(listOf(if (policy == VoiceSessionPolicy.SINGLE_TURN) VoiceCapturePurpose.ASK else VoiceCapturePurpose.TALK), factory.entries)
            val session = factory.sessions.single()
            session.events.partial("Synthetic partial"); shadowOf(Looper.getMainLooper()).idle()
            assertTrue(engine.submissions.isEmpty()); assertNull(engine.audio)
            session.recordedAudio = artifact
            session.events.final("Synthetic final"); session.events.final("Synthetic duplicate")
            shadowOf(Looper.getMainLooper()).idle()
            assertEquals(listOf("Synthetic final" to artifact), engine.submissions)
            loop.close(); output.close(); dictation.close(); shadowOf(Looper.getMainLooper()).idle()
        }
    }
    @Test fun finalWithoutRequiredSavedArtifactNeverSubmits() {
        val factory = Factory()
        val dictation = SharedDictationOwner(factory, { true }, RecognitionResourceLease(), configuration = {
            SpeechAdmission(SpeechSelection(language = "en-US"), false)
        })
        dictation.setForeground(true); dictation.start("en-US", purpose = VoiceCapturePurpose.DRAFT)
        shadowOf(Looper.getMainLooper()).idle()
        factory.sessions.single().events.final("Synthetic final without audio")
        shadowOf(Looper.getMainLooper()).idle()
        assertEquals(DictationPhase.ERROR, dictation.snapshot().phase)
        assertEquals("recording_save_failed", dictation.snapshot().error)
        assertNull(dictation.snapshot().recordedAudio)
        dictation.close(); shadowOf(Looper.getMainLooper()).idle()
    }
}
