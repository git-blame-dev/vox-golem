package dev.gitblame.voxgolem.android.wear

import android.os.Looper
import android.os.Vibrator
import dev.gitblame.voxgolem.android.wear.capture.CaptureSignals
import dev.gitblame.voxgolem.capture.CaptureRecord
import dev.gitblame.voxgolem.capture.CaptureState
import dev.gitblame.voxgolem.capture.RecognitionState
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment
import org.robolectric.Shadows.shadowOf
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35])
class CaptureWarningPresentationTest {
    private val quietCodes = listOf("speech-recognizer-7", "no-speech-detected")
    private val actionableCodes = listOf("speech-recognizer-2", "speech-recognizer-retirement-failed",
        "microphone-permission-required", "network-unavailable", "wav-finalize-failed", "metadata-save-failed")
    private val captureId = "00000000-0000-4000-8000-000000000007"

    private fun messages(code: String) = listOf(code, "Audio retained · $code", "Audio saved · transcription pending · $code")

    @Test fun recentRecordAndFeedbackSuppressOnlyTheExactNormalRecognitionCodes() {
        for (code in quietCodes) {
            assertNull(captureNoteErrorText(code))
            for (message in messages(code)) {
                assertTrue(captureHasQuietRecognitionWarning(message))
                assertEquals("Transcription ended · audio retained", captureNoteFeedbackText(message))
            }
        }
        for (code in actionableCodes + listOf("speech-recognizer-70", "no-speech-detected-extra", "synthetic-unknown-error")) {
            assertEquals("Audio retained · $code", captureNoteErrorText(code))
            for (message in messages(code)) {
                assertFalse(captureHasQuietRecognitionWarning(message))
                assertEquals(message, captureNoteFeedbackText(message))
            }
        }
        for (message in listOf("Permission failed · speech-recognizer-7", "Network failure: no-speech-detected",
            "Audio saved · transcription pending · speech-recognizer-7 · metadata-save-failed")) {
            assertFalse("A quiet code must not hide another actionable message", captureHasQuietRecognitionWarning(message))
            assertEquals(message, captureNoteFeedbackText(message))
        }
        assertNull(captureNoteErrorText(null))
        assertEquals("", captureNoteFeedbackText(""))
        assertEquals("Audio and transcription saved", captureNoteFeedbackText("Audio and transcription saved"))
    }

    @Test fun failedSignalProjectionPreservesDiagnosticPartialAudioAndUnsuccessfulRecognitionWithoutExit() {
        for ((index, code) in quietCodes.withIndex()) {
            val record = CaptureRecord(captureId, createdAtUtc = "2000-01-01T00:00:00Z", captureState = CaptureState.SAVED,
                recognitionState = if (index == 0) RecognitionState.FAILED else RecognitionState.NO_SPEECH,
                localAudioPath = "/synthetic/audio/note.wav", finalizedByteLength = 48,
                previewText = "Synthetic unconfirmed words", lastErrorStage = "transcription", lastErrorCode = code)
            val scope = CoroutineScope(SupervisorJob() + Dispatchers.Main.immediate)
            var exits = 0
            val presentation = CapturePresentation(scope, { null }, { record }, { listOf(record) }, {}, { exits++ },
                io = Dispatchers.Main.immediate)
            try {
                presentation.resume()
                shadowOf(Looper.getMainLooper()).idle()
                val message = "Audio saved · transcription pending · $code"
                presentation.signal("failed", captureId, false, message)
                shadowOf(Looper.getMainLooper()).idle()
                assertEquals("The raw presentation signal remains available", message, presentation.feedback)
                assertEquals("Transcription ended · audio retained", captureNoteFeedbackText(presentation.feedback))
                val retained = presentation.snapshot.recent.single()
                assertEquals(record, retained)
                assertEquals(code, retained.lastErrorCode)
                assertEquals("Synthetic unconfirmed words", retained.previewText)
                assertNull(retained.finalText)
                assertFalse(retained.completionNotified)
                assertNull(presentation.savedFeedbackCaptureId)
                assertNull(presentation.savedExitDeadlineMs)
                assertEquals(0, exits)
            } finally { presentation.close(); scope.cancel() }
        }
    }

    @Test fun quietAttentionHasNoWarningHapticButStillBroadcastsTheUnmodifiedFailedDiagnostic() {
        val context = RuntimeEnvironment.getApplication()
        val vibrator = context.getSystemService(Vibrator::class.java)
        try {
            for (code in quietCodes) for (message in messages(code)) {
                vibrator.cancel()
                CaptureSignals.attention(context, message, captureId)
                assertFalse(shadowOf(vibrator).isVibrating)
                val signal = shadowOf(context).broadcastIntents.last()
                assertEquals(CaptureSignals.SIGNAL_ACTION, signal.action)
                assertEquals("failed", signal.getStringExtra("state"))
                assertEquals(message, signal.getStringExtra("message"))
                assertEquals(captureId, signal.getStringExtra("captureId"))
            }
            assertTrue(shadowOf(context).broadcastIntents.none { it.getStringExtra("state") == "completed" })
        } finally { vibrator.cancel() }
    }

    @Test fun networkRetirementPermissionAndPersistenceAttentionKeepTheWarningHapticAndDiagnostic() {
        val context = RuntimeEnvironment.getApplication()
        val vibrator = context.getSystemService(Vibrator::class.java)
        try {
            for (code in actionableCodes) {
                vibrator.cancel()
                val message = "Audio saved · transcription pending · $code"
                CaptureSignals.attention(context, message, captureId)
                assertTrue(shadowOf(vibrator).isVibrating)
                assertArrayEquals(longArrayOf(0, 100, 100, 100, 100, 100), shadowOf(vibrator).pattern)
                val signal = shadowOf(context).broadcastIntents.last()
                assertEquals("failed", signal.getStringExtra("state"))
                assertEquals(message, signal.getStringExtra("message"))
            }
        } finally { vibrator.cancel() }
    }
}
