package dev.gitblame.voxgolem.android.wear.capture

import dev.gitblame.voxgolem.capture.CaptureRecord
import dev.gitblame.voxgolem.capture.CaptureState
import java.time.Instant
import java.time.ZoneId
import java.util.UUID

data class CaptureSnapshot(val active: CaptureRecord?, val recent: List<CaptureRecord>)

object CaptureIdentity {
    fun newId(): String = UUID.randomUUID().toString()
    fun isValid(value: String): Boolean = dev.gitblame.voxgolem.capture.CaptureProtocol.isValidUuid(value)
}

data class FinishDate(val instant: Instant, val localDate: String, val timezone: String)

fun finishDate(now: Instant, zone: ZoneId): FinishDate =
    FinishDate(now, now.atZone(zone).toLocalDate().toString(), zone.id)

fun completedCaptureState(stopReason: String): CaptureState = when (stopReason) {
    "manual_stop", "provisional_motion_lowering" -> CaptureState.SAVED
    else -> CaptureState.INTERRUPTED
}
