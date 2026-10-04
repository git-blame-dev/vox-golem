package dev.gitblame.voxgolem.android.wear.capture

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import java.time.Instant
import java.time.ZoneId

class CaptureModelTest {
    @Test fun durableCaptureIdentityIsUuidAndEachLaunchIdentityIsFresh() {
        val first = CaptureIdentity.newId()
        val second = CaptureIdentity.newId()
        assertTrue(CaptureIdentity.isValid(first))
        assertTrue(CaptureIdentity.isValid(second))
        assertFalse(first == second)
        assertFalse(CaptureIdentity.isValid("capture-1"))
    }

    @Test fun finishDayUsesCaptureInstantAndWatchTimezone() {
        val result = finishDate(Instant.parse("2026-10-03T00:30:00Z"), ZoneId.of("America/Los_Angeles"))
        assertEquals("2026-10-02", result.localDate)
        assertEquals("America/Los_Angeles", result.timezone)
    }
}
