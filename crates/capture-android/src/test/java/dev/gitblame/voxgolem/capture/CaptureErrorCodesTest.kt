package dev.gitblame.voxgolem.capture

import org.junit.Assert.*
import org.junit.Test

class CaptureErrorCodesTest {
    @Test fun privateExceptionTextAndUnknownHistoricalCodesNeverBecomePublicDiagnosticCodes() {
        val failures = listOf(
            java.io.IOException("Synthetic /storage/synthetic-person/private-note.wav could not be opened"),
            SecurityException("Synthetic content://synthetic.private/tree/private-document access denied"),
            IllegalStateException("synthetic-private-document-id"),
        )
        failures.forEach { failure ->
            assertEquals("capture-operation-failed", CaptureErrorCodes.safe(failure.message))
            assertEquals("transcription-unavailable", CaptureErrorCodes.safe(failure.message, "transcription-unavailable"))
        }
        assertEquals("speech-recognizer-7", CaptureErrorCodes.safe("speech-recognizer-7"))
        assertEquals("notes-folder-not-selected", CaptureErrorCodes.safe("notes-folder-not-selected"))
        assertEquals("capture-operation-failed", CaptureErrorCodes.safe("speech-recognizer-7-private-note"))
    }
}
