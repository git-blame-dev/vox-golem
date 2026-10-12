package dev.gitblame.voxgolem.android.wear

import android.content.Intent
import dev.gitblame.voxgolem.android.wear.capture.CaptureSignals
import dev.gitblame.voxgolem.android.wear.capture.CaptureSnapshot
import dev.gitblame.voxgolem.capture.CaptureRecord
import dev.gitblame.voxgolem.capture.CaptureRepository
import dev.gitblame.voxgolem.capture.CaptureState
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.Robolectric
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment
import org.robolectric.Shadows.shadowOf
import org.robolectric.annotation.Config
import java.util.UUID

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35])
class CaptureRefreshTest {
    @Test fun recordingPollAndSignalsKeepRecentSnapshotButTerminalRefreshReloadsIt() {
        val repository = CaptureRepository(RuntimeEnvironment.getApplication())
        val recent = repository.save(CaptureRecord(
            UUID.randomUUID().toString(), createdAtUtc = "2026-10-04T12:30:00Z", captureState = CaptureState.SAVED,
        ))
        val active = repository.save(CaptureRecord(
            UUID.randomUUID().toString(), createdAtUtc = "2026-10-04T12:31:00Z", captureState = CaptureState.RECORDING,
        ))
        val controller = Robolectric.buildActivity(CaptureActivity::class.java, Intent(Intent.ACTION_VIEW)).create()
        val activity = controller.get()
        val initial = snapshot(activity)
        repository.update(recent.captureId) { it.copy(lastErrorCode = "synthetic-completion-error") }
        repository.update(active.captureId) { it.copy(previewText = "synthetic live revision") }

        refresh(activity, false)
        assertSame(initial.recent, snapshot(activity).recent)
        assertEquals("synthetic live revision", snapshot(activity).active!!.previewText)

        CaptureSignals.failed(activity, "Synthetic attention signal")
        shadowOf(android.os.Looper.getMainLooper()).idle()
        assertSame(initial.recent, snapshot(activity).recent)

        repository.update(active.captureId) { it.copy(captureState = CaptureState.INTERRUPTED, lastErrorCode = "synthetic-save-error") }
        refresh(activity, false)
        assertNull(snapshot(activity).active)
        assertEquals("synthetic-completion-error", snapshot(activity).recent.single { it.captureId == recent.captureId }.lastErrorCode)
        assertEquals("synthetic-save-error", snapshot(activity).recent.single { it.captureId == active.captureId }.lastErrorCode)
        controller.destroy()
    }

    private fun refresh(activity: CaptureActivity, reloadRecent: Boolean) {
        activity.presentation.refresh(reloadRecent)
        awaitCaptureIdle(activity)
    }

    private fun snapshot(activity: CaptureActivity): CaptureSnapshot {
        awaitCaptureIdle(activity)
        return activity.presentation.snapshot
    }
}
