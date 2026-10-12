package dev.gitblame.voxgolem.assistant.recognition

import android.os.Looper
import dev.gitblame.voxgolem.assistant.inputtransport.SuppliedAudioProviderGate
import dev.gitblame.voxgolem.assistant.speech.SpeechAdmission
import dev.gitblame.voxgolem.assistant.speech.SpeechSelection
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment
import org.robolectric.Shadows.shadowOf
import org.robolectric.annotation.Config
import org.robolectric.annotation.LooperMode
import org.robolectric.shadows.ShadowSpeechRecognizer

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35], manifest = Config.NONE)
@LooperMode(LooperMode.Mode.PAUSED)
class RecordedFactoryAdmissionTest {
    @Test fun recordingFailuresHaveClearUiTextWithoutPromotingNoMatchOrHidingRawDiagnostics() {
        assertNull(recognitionUiError("recognition_error_7"))
        assertTrue(recognitionUiError("recorded_input_provider_unproven")!!.contains("validated"))
        assertTrue(recognitionUiError("recording_failed")!!.contains("Nothing was sent"))
        assertEquals("synthetic_error", recognitionUiError("synthetic_error"))
    }
    @Test fun enabledButUnprovenNeverAllocatesEitherSdkMicOrAppRecorder() {
        ShadowSpeechRecognizer.setIsOnDeviceRecognitionAvailable(true)
        var opens = 0
        val factory = AndroidLocalRecognizerFactory(RuntimeEnvironment.getApplication(), recordedAudio = RecordedAudioFactory {
            opens++; error("Must not open unproven audio")
        }, enableRecording = { true })
        val owner = SharedDictationOwner(factory, { true }, RecognitionResourceLease(), configuration = {
            SpeechAdmission(SpeechSelection(language = "en-US"), false)
        })
        owner.setForeground(true); owner.start("en-US", purpose = VoiceCapturePurpose.ASK)
        shadowOf(Looper.getMainLooper()).idle()
        assertEquals("recorded_input_provider_unproven", owner.snapshot().error)
        assertEquals(0, opens); assertNull(ShadowSpeechRecognizer.getLatestSpeechRecognizer())
        owner.close(); shadowOf(Looper.getMainLooper()).idle()
    }
    @Test @Config(sdk = [24, 32]) fun apiBelow33CannotBeApprovedByAPermissiveGate() {
        val factory = AndroidLocalRecognizerFactory(RuntimeEnvironment.getApplication(),
            recordedAudio = RecordedAudioFactory { error("Must not open unsupported audio") },
            suppliedAudioGate = SuppliedAudioProviderGate { true }, enableRecording = { true })
        assertFalse(factory.recordingAvailable("en-US", true))
        assertFalse(factory.availableForEntry("en-US", true))
        assertNull(ShadowSpeechRecognizer.getLatestSpeechRecognizer())
    }
    @Test fun unselectedRecordingLeavesLegacyVoiceAvailableWithoutClaimingAnArchive() {
        ShadowSpeechRecognizer.setIsOnDeviceRecognitionAvailable(true)
        val factory = AndroidLocalRecognizerFactory(RuntimeEnvironment.getApplication(), false,
            recordedAudio = RecordedAudioFactory { error("Legacy path must not create archive") })
        assertFalse(factory.recordingEnabled()); assertFalse(factory.recordingAvailable("en-US", false))
        assertTrue(factory.availableForEntry("en-US", false))
        val session = factory.create()
        assertFalse(session.archiveExpected); assertNull(session.recordedAudio); session.destroy()
    }
}
