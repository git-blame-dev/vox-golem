package dev.gitblame.voxgolem.capture

import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class CaptureWireTest {
    private val id = "123e4567-e89b-42d3-a456-426614174000"
    private val session = "123e4567-e89b-42d3-a456-426614174001"
    private val attempt = "123e4567-e89b-42d3-a456-426614174002"

    @Test
    fun acceptsWearLiveHeaderAndRejectsWrongSessionOrFormat() {
        val header = CaptureWire.LiveHeader(
            protocol = CaptureWire.PROTOCOL,
            captureId = id,
            sessionId = session,
            format = CaptureFormat(),
            startedAt = "2026-01-02T03:04:05Z",
        )

        assertTrue(CaptureWire.validLiveStart(header, id, session))
        assertFalse(CaptureWire.validLiveStart(header, id, attempt))
        assertFalse(CaptureWire.validLiveStart(header.copy(format = CaptureFormat(sampleRate = 7_000)), id, session))
    }

    @Test
    fun acceptsExactWearArchiveEnvelopeAndRejectsMismatchedAudioMetadata() {
        val header = CaptureWire.ArchiveHeader(
            protocol = CaptureWire.PROTOCOL,
            captureId = id,
            attemptId = attempt,
            format = CaptureFormat(),
            frameCount = 4,
            wavBytes = 52,
            sha256 = "a".repeat(64),
            finishDate = "2026-01-02",
            timezone = "Etc/UTC",
        )

        assertTrue(CaptureWire.validArchiveHeader(header, id, attempt))
        assertFalse(CaptureWire.validArchiveHeader(header, id, session))
        assertFalse(CaptureWire.validArchiveHeader(header.copy(wavBytes = 53), id, attempt))
        assertFalse(CaptureWire.validArchiveHeader(header.copy(sha256 = "not-a-hash"), id, attempt))
        assertTrue(CaptureWire.validArchiveHeader(header.copy(captureState = CaptureState.INTERRUPTED), id, attempt))
        assertFalse(CaptureWire.validArchiveHeader(header.copy(captureState = CaptureState.RECORDING), id, attempt))
        assertFalse(CaptureWire.validArchiveHeader(header.copy(finishedAtUtc = "not-a-timestamp"), id, attempt))
    }
}
