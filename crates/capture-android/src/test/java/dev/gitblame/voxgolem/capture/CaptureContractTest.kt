package dev.gitblame.voxgolem.capture

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class CaptureContractTest {
    @Test
    fun protocolRequiresUuidPcm16AndContiguousBoundedFrames() {
        val format = CaptureFormat()
        assertTrue(CaptureProtocol.isValidUuid("123e4567-e89b-42d3-a456-426614174000"))
        assertFalse(CaptureProtocol.isValidUuid("capture-1"))
        assertTrue(CaptureProtocol.validateFormat(format))
        assertFalse(CaptureProtocol.validateFormat(format.copy(channels = 3)))
        assertTrue(CaptureProtocol.validateFrame(format, 0, 3200, null))
        assertFalse(CaptureProtocol.validateFrame(format, 1, 3200, null))
        assertTrue(CaptureProtocol.validateFrame(format, 1, 3200, 0))
        assertFalse(CaptureProtocol.validateFrame(format, 2, 3200, 0))
        assertFalse(CaptureProtocol.validateFrame(format, 1, 3202, 0))
        assertTrue(CaptureProtocol.validatePayloadSize(32 * 1024))
        assertFalse(CaptureProtocol.validatePayloadSize(32 * 1024 + 1))
        assertTrue(CaptureProtocol.validateManifest(CaptureManifest(
            "123e4567-e89b-42d3-a456-426614174000", format, 4, 8, 52, "a".repeat(64),
        )))
        assertFalse(CaptureProtocol.validateManifest(CaptureManifest(
            "123e4567-e89b-42d3-a456-426614174000", format, 4, 10, 54, "a".repeat(64),
        )))
    }

    @Test
    fun reducerReplacesFullTextRejectsStaleUpdatesAndMakesFinalTerminal() {
        val reducer = TranscriptReducer("123e4567-e89b-42d3-a456-426614174000")
        val session = "123e4567-e89b-42d3-a456-426614174001"
        assertEquals(
            "hello world",
            reducer.apply(TranscriptUpdate("123e4567-e89b-42d3-a456-426614174000", session, 2, "hello world", false)).preview,
        )
        assertEquals(
            "hello world",
            reducer.apply(TranscriptUpdate("123e4567-e89b-42d3-a456-426614174000", session, 1, "hello", false)).preview,
        )
        reducer.apply(TranscriptUpdate("123e4567-e89b-42d3-a456-426614174000", session, 3, "final text", true))
        val afterFinal = reducer.apply(TranscriptUpdate("123e4567-e89b-42d3-a456-426614174000", session, 4, "late partial", false))
        assertEquals("final text", afterFinal.finalText)
        assertEquals("final text", afterFinal.preview)
        assertEquals(3L, afterFinal.revision)
    }

    @Test
    fun reducerStartsFreshAttemptsAndRejectsLateUpdatesFromPreviousAttempt() {
        val reducer = TranscriptReducer("123e4567-e89b-42d3-a456-426614174000")
        val firstSession = "123e4567-e89b-42d3-a456-426614174001"
        val secondSession = "123e4567-e89b-42d3-a456-426614174002"

        reducer.beginSession(firstSession)
        reducer.apply(TranscriptUpdate("123e4567-e89b-42d3-a456-426614174000", firstSession, 8, "old attempt", true))
        reducer.beginSession(secondSession)
        assertEquals(
            TranscriptSnapshot(secondSession, 0, null, null),
            reducer.apply(TranscriptUpdate("123e4567-e89b-42d3-a456-426614174000", firstSession, 9, "late old result", false)),
        )
        assertEquals(
            "new attempt",
            reducer.apply(TranscriptUpdate("123e4567-e89b-42d3-a456-426614174000", secondSession, 1, "new attempt", false)).preview,
        )
        assertEquals(
            "new attempt",
            reducer.apply(TranscriptUpdate("123e4567-e89b-42d3-a456-426614174000", secondSession, 1, "duplicate revision", false)).preview,
        )
    }

    @Test fun emptyHypothesesRetainLatestDraftButBlankFinalRemainsAuthoritativeAndTerminal() {
        val id = "123e4567-e89b-42d3-a456-426614174000"
        val session = "123e4567-e89b-42d3-a456-426614174001"
        val reducer = TranscriptReducer(id)
        val latest = "Synthetic oversized draft ".repeat(2_000)
        reducer.beginSession(session)
        reducer.apply(TranscriptUpdate(id, session, 1, "Synthetic earlier draft", false))
        reducer.apply(TranscriptUpdate(id, session, 2, latest, false))
        val emptyPartial = reducer.apply(TranscriptUpdate(id, session, 3, "  ", false))
        assertEquals(latest, emptyPartial.preview)
        assertEquals(null, emptyPartial.finalText)
        val noSpeech = reducer.apply(TranscriptUpdate(id, session, 4, "", true))
        assertEquals(latest, noSpeech.preview)
        assertEquals("", noSpeech.finalText)
        assertEquals(4L, noSpeech.revision)
        assertEquals(noSpeech, reducer.apply(TranscriptUpdate(id, session, 5, "Synthetic late words", false)))
        assertEquals(noSpeech, reducer.apply(TranscriptUpdate(id, session, 6, "Synthetic late final", true)))
        val nextSession = "123e4567-e89b-42d3-a456-426614174002"
        assertEquals(TranscriptSnapshot(nextSession, 0, null, null), reducer.beginSession(nextSession))
    }
}
