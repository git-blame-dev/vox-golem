package dev.gitblame.voxgolem.android.wear.capture

import dev.gitblame.voxgolem.capture.CaptureRecord
import dev.gitblame.voxgolem.capture.CaptureState
import dev.gitblame.voxgolem.capture.CapturePurpose
import dev.gitblame.voxgolem.capture.OrgState
import dev.gitblame.voxgolem.capture.CaptureRepository as SharedRepository
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment
import org.robolectric.annotation.Config
import java.time.Instant
import java.util.UUID

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35])
class CaptureQueryTest {
    private val context = RuntimeEnvironment.getApplication()
    private val shared = SharedRepository(context)
    private val watch = CaptureRepository(context)

    @Test fun activeCaptureOlderThanFiveHundredRecentRecordsIsStillFound() {
        val active = shared.save(record(1, CaptureState.RECORDING))
        (2..502).forEach { shared.save(record(it, CaptureState.SAVED)) }

        assertEquals(500, watch.records().size)
        assertFalse(watch.records().any { it.captureId == active.captureId })
        assertEquals(active, watch.active())
    }

    @Test fun recentLimitIsAppliedBeforeDecodingBodies() {
        (1..13).forEach { shared.save(record(it, CaptureState.SAVED)) }
        corruptBody(record(1, CaptureState.SAVED).captureId)

        assertEquals((13 downTo 2).map { record(it, CaptureState.SAVED) }, watch.records(limit = 12))
        assertThrows(org.json.JSONException::class.java) { watch.records() }
    }

    @Test fun purposeFilteringStopsDecodingOnceTheRequestedNotesAreFound() {
        val outsideWindow = shared.save(record(1, CaptureState.SAVED))
        val note = shared.save(record(2, CaptureState.SAVED))
        shared.save(record(3, CaptureState.SAVED).copy(purpose = CapturePurpose.ASK, orgState = OrgState.NONE))
        corruptBody(outsideWindow.captureId)

        assertEquals(listOf(note), watch.records(limit = 1))
        assertThrows(org.json.JSONException::class.java) { watch.records(limit = 2) }
    }

    @Test fun activeQueryFiltersStatesAndDecodesOnlyNewestActiveBody() {
        shared.save(record(1, CaptureState.PREPARING))
        val stopping = shared.save(record(2, CaptureState.STOPPING))
        shared.save(record(3, CaptureState.SAVED))
        corruptBody(record(1, CaptureState.PREPARING).captureId)
        corruptBody(record(3, CaptureState.SAVED).captureId)

        assertEquals(stopping, watch.active())
        shared.update(stopping.captureId) { it.copy(captureState = CaptureState.RECORDING) }
        assertEquals(CaptureState.RECORDING, watch.active()!!.captureState)
    }

    private fun corruptBody(id: String) {
        context.openOrCreateDatabase("captures.db", 0, null).use { database ->
            database.execSQL("UPDATE captures SET body = ? WHERE capture_id = ?", arrayOf("synthetic invalid JSON", id))
        }
    }

    private fun record(number: Int, state: CaptureState) = CaptureRecord(
        captureId = UUID(0x123e4567e89b42d3L, Long.MIN_VALUE + number).toString(),
        createdAtUtc = Instant.parse("2026-10-04T12:30:00Z").plusSeconds(number.toLong()).toString(),
        captureState = state,
    )
}
