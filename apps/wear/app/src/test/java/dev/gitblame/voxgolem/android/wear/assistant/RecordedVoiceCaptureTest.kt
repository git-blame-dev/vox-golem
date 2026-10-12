package dev.gitblame.voxgolem.android.wear.assistant

import dev.gitblame.voxgolem.android.wear.capture.CaptureLifetime
import dev.gitblame.voxgolem.assistant.recognition.RecordedAudioRequest
import dev.gitblame.voxgolem.assistant.recognition.VoiceCapturePurpose
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35], manifest = Config.NONE)
class RecordedVoiceCaptureTest {
    @Test fun admittedVoiceOwnsTheExistingCaptureLifetimeUntilActualCloseEvenBeforeMicStart() {
        val capture = recordedVoiceCapture(RuntimeEnvironment.getApplication()).open(
            RecordedAudioRequest(VoiceCapturePurpose.ASK, "en-AU", false))
        try {
            assertNotNull(CaptureLifetime.current())
            assertNull(CaptureLifetime.tryAcquire())
        } finally { capture.close() }
        assertNull(CaptureLifetime.current())
    }
    @Test fun retiringNoteBlocksRecordedVoiceWithoutCreatingAnotherMicrophoneOwner() {
        val note = CaptureLifetime.tryAcquire()!!
        try {
            assertThrows(IllegalStateException::class.java) {
                recordedVoiceCapture(RuntimeEnvironment.getApplication()).open(
                    RecordedAudioRequest(VoiceCapturePurpose.TALK, "en-AU", true))
            }
            assertSame(note, CaptureLifetime.current())
        } finally { note.close() }
    }
}
