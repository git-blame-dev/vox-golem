package dev.gitblame.voxgolem.capture

import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import java.time.Instant

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [24, 35], manifest = Config.NONE)
class CaptureUtcTimeTest {
    @Test fun canonicalUtcDeadlinesMatchInstantMillisIncludingNanosecondTruncationAndPreEpochFloor() {
        val timestamps = listOf(
            "2026-01-02T03:04:05Z", "2026-01-02T03:04:05.1Z", "2026-01-02T03:04:05.12Z",
            "2026-01-02T03:04:05.123Z", "2026-01-02T03:04:05.123456Z", "2026-01-02T03:04:05.123456789Z",
            "2026-01-02T03:04:05.000000001Z", "1969-12-31T23:59:59.123456789Z",
            "1969-12-31T23:59:59.999999999Z", "1500-01-02T03:04:05.123Z", "2024-02-29T00:00:00Z",
        )
        timestamps.forEach { assertEquals(it, Instant.parse(it).toEpochMilli(), CaptureUtcTime.parseMillis(it)) }
    }

    @Test fun formatterRoundTripsPersistedMillisecondValuesAndParserRejectsNonCanonicalOrInvalidDates() {
        listOf(-1_000L, -877L, -1L, 0L, 1L, 123L, 1_767_323_045_678L).forEach {
            assertEquals(it, CaptureUtcTime.parseMillis(CaptureUtcTime.formatMillis(it)))
        }
        listOf("", "synthetic-invalid", "2026-02-29T00:00:00Z", "2026-01-02T25:04:05Z",
            "2026-01-02T03:04:05Z trailing", "2026-01-02T03:04:05.1234567890Z",
            "2026-01-02T03:04:05+01:00").forEach { assertNull(it, CaptureUtcTime.parseMillis(it)) }
    }
}
