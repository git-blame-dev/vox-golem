package dev.gitblame.voxgolem.android.wear.capture

import dev.gitblame.voxgolem.capture.CaptureFormat
import dev.gitblame.voxgolem.capture.CaptureManifest
import dev.gitblame.voxgolem.capture.CaptureProtocol
import dev.gitblame.voxgolem.capture.TranscriptReducer
import dev.gitblame.voxgolem.capture.TranscriptUpdate
import dev.gitblame.voxgolem.capture.TransferState
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class CaptureInteropTest {
    @Test fun sharedWireFormatAcceptsOnlyAlignedContiguousFramesUpToOneHundredMilliseconds() {
        val format = CaptureFormat()
        assertTrue(CaptureProtocol.validateFrame(format, 0, 3_200, null))
        assertTrue(CaptureProtocol.validateFrame(format, 1, 3_200, 0))
        assertFalse(CaptureProtocol.validateFrame(format, 2, 3_200, 0))
        assertFalse(CaptureProtocol.validateFrame(format, 1, 3_202, 0))
    }

    @Test fun sharedManifestMatchesWavLengthHashAndFrameFields() {
        val valid = CaptureManifest(
            captureId = CAPTURE_ID,
            format = CaptureFormat(),
            frameCount = 4,
            pcmByteLength = 8,
            wavByteLength = 52,
            sha256 = "a".repeat(64),
        )
        assertTrue(CaptureProtocol.validateManifest(valid))
        assertFalse(CaptureProtocol.validateManifest(valid.copy(wavByteLength = 51)))
        assertFalse(CaptureProtocol.validateManifest(valid.copy(pcmByteLength = 7)))
    }

    @Test fun actualSharedTranscriptFieldsRejectStaleAndLatePartialsAfterFinal() {
        val reducer = TranscriptReducer(CAPTURE_ID)
        val session = "123e4567-e89b-42d3-a456-426614174001"
        val partial = reducer.apply(TranscriptUpdate(CAPTURE_ID, session, 2, "corrected full hypothesis", false))
        assertEquals("corrected full hypothesis", partial.preview)
        assertEquals(2L, partial.revision)
        assertEquals(null, partial.finalText)

        val stale = reducer.apply(TranscriptUpdate(CAPTURE_ID, session, 1, "old", false))
        assertEquals("corrected full hypothesis", stale.preview)
        val final = reducer.apply(TranscriptUpdate(CAPTURE_ID, session, 3, "final transcript", true))
        assertEquals("final transcript", final.preview)
        assertEquals("final transcript", final.finalText)
        assertEquals("final transcript", reducer.apply(TranscriptUpdate(CAPTURE_ID, session, 4, "late", false)).finalText)
        assertEquals("final transcript", reducer.apply(TranscriptUpdate("123e4567-e89b-42d3-a456-426614174002", session, 5, "wrong capture", false)).finalText)
    }

    @Test fun receiptStagesDoNotRegressAndTransientFailuresCanRetry() {
        assertEquals(TransferState.SENDING, mergeTransferState(TransferState.PENDING, TransferState.SENDING))
        assertEquals(TransferState.PENDING, mergeTransferState(TransferState.SENDING, TransferState.PENDING))
        assertEquals(TransferState.PHONE_STAGED, mergeTransferState(TransferState.PHONE_STAGED, TransferState.SENDING))
        assertEquals(TransferState.PHONE_ARCHIVED, mergeTransferState(TransferState.PHONE_STAGED, TransferState.PHONE_ARCHIVED))
        assertEquals(TransferState.PHONE_ARCHIVED, mergeTransferState(TransferState.PHONE_ARCHIVED, TransferState.PENDING))
    }

    companion object { private const val CAPTURE_ID = "123e4567-e89b-42d3-a456-426614174000" }
}
