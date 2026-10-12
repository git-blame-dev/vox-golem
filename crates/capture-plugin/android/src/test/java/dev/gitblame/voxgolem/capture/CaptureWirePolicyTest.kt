package dev.gitblame.voxgolem.capture

import org.json.JSONObject
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35])
class CaptureWirePolicyTest {
    private fun header() = JSONObject().put("protocol", CaptureProtocol.PROTOCOL)
        .put("captureId", "123e4567-e89b-42d3-a456-426614174000")
        .put("sessionId", "123e4567-e89b-42d3-a456-426614174001")
        .put("sampleRate", 16_000).put("channels", 1).put("bitsPerSample", 16)
        .put("startedAt", "2026-01-02T03:04:05Z")

    @Test fun optionalV1FieldsPreserveLegacyPrivateNotesAndExplicitNewPolicy() {
        val legacy = CaptureWire.liveHeader(header())
        assertEquals(CapturePurpose.NOTE, legacy.purpose)
        assertNull(legacy.requestedLanguage)
        assertFalse(legacy.speechNetworkFallback)
        val allowed = CaptureWire.liveHeader(header().put("requestedLanguage", "en-AU").put("speechNetworkFallback", true))
        assertEquals("en-AU", allowed.requestedLanguage)
        assertTrue(allowed.speechNetworkFallback)
    }

    @Test fun unknownPurposeAndMalformedPrivacyCannotTurnIntoAnExportableNoteOrNetworkConsent() {
        assertThrows(IllegalArgumentException::class.java) { CaptureWire.liveHeader(header().put("purpose", "UNRECOGNIZED")) }
        assertThrows(IllegalArgumentException::class.java) { CaptureWire.liveHeader(header().put("speechNetworkFallback", "true")) }
        assertThrows(IllegalArgumentException::class.java) { CaptureWire.liveHeader(header().put("requestedLanguage", "/synthetic/private")) }
        val chat = CaptureWire.liveHeader(header().put("purpose", "ASK"))
        assertFalse(CaptureWire.validLiveStart(chat, chat.captureId, chat.sessionId))
    }
}
