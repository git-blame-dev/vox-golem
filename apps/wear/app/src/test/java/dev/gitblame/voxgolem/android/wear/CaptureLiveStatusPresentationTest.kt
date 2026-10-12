package dev.gitblame.voxgolem.android.wear

import dev.gitblame.voxgolem.capture.CaptureRecord
import dev.gitblame.voxgolem.capture.LiveTranscriptionState
import org.junit.Assert.*
import org.junit.Test

class CaptureLiveStatusPresentationTest {
    private val record = CaptureRecord("123e4567-e89b-42d3-a456-426614174090", createdAtUtc = "2026-01-02T03:04:05Z")

    @Test fun recordingIsNotDescribedAsRecognizerReadinessOrFinalText() {
        val connecting = captureLiveStatusText(record.copy(liveTranscriptionState = LiveTranscriptionState.CONNECTING))
        assertTrue(connecting.contains("Recording"))
        assertTrue(connecting.contains("Connecting phone"))
        assertFalse(connecting.contains("recognizer ready"))
        val ready = captureLiveStatusText(record.copy(liveTranscriptionState = LiveTranscriptionState.PROVIDER_READY))
        assertTrue(ready.contains("waiting for words"))
        assertFalse(ready.contains("Transcription saved"))
    }

    @Test fun phoneFailureShowsOnlySafeCodesAndDoesNotHideAudioRetention() {
        val blocked = captureLiveStatusText(record.copy(liveTranscriptionState = LiveTranscriptionState.UNAVAILABLE,
            liveErrorCode = "record-audio-permission-required"))
        assertTrue(blocked.contains("audio retained"))
        assertTrue(blocked.contains("record-audio-permission-required"))
        assertFalse(captureLiveStatusText(record.copy(liveErrorCode = "/synthetic/private/path")).contains("/synthetic"))
    }
}
