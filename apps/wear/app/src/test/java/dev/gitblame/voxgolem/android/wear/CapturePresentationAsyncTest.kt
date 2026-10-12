package dev.gitblame.voxgolem.android.wear

import android.os.Looper
import dev.gitblame.voxgolem.capture.CaptureRecord
import dev.gitblame.voxgolem.capture.CaptureState
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.launch
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.Shadows.shadowOf
import org.robolectric.annotation.Config
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicInteger

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35])
class CapturePresentationAsyncTest {
    private val old = CaptureRecord("123e4567-e89b-42d3-a456-426614174090", createdAtUtc = "2026-10-06T00:00:00Z", captureState = CaptureState.RECORDING)
    private val fresh = old.copy(captureId = "123e4567-e89b-42d3-a456-426614174091")
    @Test fun refreshesCoalesceAndRejectTheInflightGenerationBeforeApplyingNewestIdentity() {
        val entered = CountDownLatch(1)
        val release = CountDownLatch(1)
        val reads = AtomicInteger()
        val changes = mutableListOf<String?>()
        val scope = CoroutineScope(SupervisorJob() + Dispatchers.Main.immediate)
        lateinit var presentation: CapturePresentation
        presentation = CapturePresentation(scope, {
            assertNotEquals(Looper.getMainLooper(), Looper.myLooper())
            if (reads.incrementAndGet() == 1) { entered.countDown(); check(release.await(5, TimeUnit.SECONDS)); old } else fresh
        }, { null }, { emptyList() }, { changes += presentation.snapshot.active?.captureId }, { fail("Recording must not show SAVED") })
        try {
            presentation.resume()
            assertTrue(entered.await(2, TimeUnit.SECONDS))
            presentation.signal("ready", fresh.captureId, false, null)
            repeat(20) { presentation.refresh(false) }
            release.countDown(); settle(presentation)
            assertEquals(2, reads.get())
            assertEquals(listOf(fresh.captureId), changes)
        } finally { release.countDown(); presentation.close(); scope.cancel() }
    }

    @Test fun sixHundredMillisecondReadsPublishReadinessAndDurableSavedWhileFiveHundredMillisecondPollingContinues() {
        val scope = CoroutineScope(SupervisorJob() + Dispatchers.Main.immediate)
        val entered = java.util.concurrent.LinkedBlockingQueue<Int>()
        val release = java.util.concurrent.Semaphore(0)
        val reads = AtomicInteger()
        val changes = mutableListOf<String?>()
        lateinit var presentation: CapturePresentation
        presentation = CapturePresentation(scope, {
            val index = reads.getAndIncrement()
            entered.put(index)
            check(release.tryAcquire(5, TimeUnit.SECONDS))
            if (index < 2) old.copy(observedFrames = index + 1L, previewText = "Synthetic revision $index") else null
        }, { old.copy(captureState = CaptureState.SAVED) }, { emptyList() }, {
            changes += presentation.snapshot.active?.previewText
        }, {})
        try {
            presentation.resume()
            val polling = scope.launchPolling(presentation)
            repeat(3) { index ->
                assertEquals(index, entered.poll(2, TimeUnit.SECONDS))
                shadowOf(Looper.getMainLooper()).idleFor(java.time.Duration.ofMillis(500))
                presentation.signal(null, null, false, null)
                shadowOf(Looper.getMainLooper()).idleFor(java.time.Duration.ofMillis(100))
                release.release()
                val deadline = System.nanoTime() + TimeUnit.SECONDS.toNanos(2)
                while (changes.size <= index && System.nanoTime() < deadline) {
                    shadowOf(Looper.getMainLooper()).idle(); Thread.sleep(1)
                }
                assertEquals("A routine poll must not obsolete the completed 600ms read", index + 1, changes.size)
                if (index < 2) {
                    assertEquals("Synthetic revision $index", presentation.snapshot.active?.previewText)
                    assertTrue(presentation.backgroundListening)
                }
            }
            assertEquals(old.captureId, presentation.savedFeedbackCaptureId)
            val state = android.os.Bundle().also(presentation::save)
            assertEquals(android.os.SystemClock.elapsedRealtime() + 4_000, state.getLong("savedFeedbackDeadlineMs"))
            polling.cancel()
        } finally { presentation.close(); scope.cancel(); release.release(10) }
    }

    @Test fun resetAndCalibrationStillRejectReadsFromThePreviousSemanticGeneration() {
        for (calibrating in listOf(false, true)) {
            val scope = CoroutineScope(SupervisorJob() + Dispatchers.Main.immediate)
            val entered = CountDownLatch(1)
            val release = CountDownLatch(1)
            val reads = AtomicInteger()
            val changes = mutableListOf<String?>()
            lateinit var presentation: CapturePresentation
            presentation = CapturePresentation(scope, {
                if (reads.incrementAndGet() == 1) { entered.countDown(); check(release.await(5, TimeUnit.SECONDS)); old } else fresh
            }, { null }, { emptyList() }, { changes += presentation.snapshot.active?.captureId }, {})
            try {
                presentation.resume(); assertTrue(entered.await(2, TimeUnit.SECONDS))
                if (calibrating) presentation.calibrationChanged(true) else presentation.resetCapture()
                release.countDown(); settle(presentation)
                assertEquals(listOf(fresh.captureId), changes)
            } finally { release.countDown(); presentation.close(); scope.cancel() }
        }
    }

    @Test fun aDifferentObservedCaptureSignalRejectsTheOldInflightSnapshot() {
        val scope = CoroutineScope(SupervisorJob() + Dispatchers.Main.immediate)
        val entered = CountDownLatch(1)
        val release = CountDownLatch(1)
        val reads = AtomicInteger()
        val changes = mutableListOf<String?>()
        lateinit var presentation: CapturePresentation
        presentation = CapturePresentation(scope, {
            when (reads.incrementAndGet()) {
                1 -> old
                2 -> { entered.countDown(); check(release.await(5, TimeUnit.SECONDS)); old.copy(previewText = "Synthetic stale revision") }
                else -> fresh
            }
        }, { null }, { emptyList() }, { changes += presentation.snapshot.active?.captureId }, {})
        try {
            presentation.resume(); settle(presentation)
            assertEquals(old.captureId, presentation.observedCaptureId)
            presentation.refresh(false); assertTrue(entered.await(2, TimeUnit.SECONDS))
            presentation.signal("ready", fresh.captureId, false, null)
            release.countDown(); settle(presentation)
            assertEquals(listOf(old.captureId, fresh.captureId), changes)
            assertEquals(fresh.captureId, presentation.observedCaptureId)
        } finally { release.countDown(); presentation.close(); scope.cancel() }
    }

    private fun CoroutineScope.launchPolling(presentation: CapturePresentation) = launch {
        while (true) { kotlinx.coroutines.delay(500); presentation.refresh(false) }
    }
    @Test fun pauseRetiresInflightReadAndClosedOwnerCannotApplyOrExit() {
        val entered = CountDownLatch(1)
        val release = CountDownLatch(1)
        val changes = AtomicInteger()
        val scope = CoroutineScope(SupervisorJob() + Dispatchers.Main.immediate)
        val presentation = CapturePresentation(scope, { entered.countDown(); check(release.await(5, TimeUnit.SECONDS)); old },
            { old.copy(captureState = CaptureState.SAVED) }, { emptyList() }, { changes.incrementAndGet() }, { fail("Hidden owner cannot exit") })
        try {
            presentation.resume(); assertTrue(entered.await(2, TimeUnit.SECONDS))
            presentation.pause(); release.countDown(); settle(presentation)
            assertNull(presentation.snapshot.active)
            assertEquals(0, changes.get())
            presentation.close(); presentation.refresh(); shadowOf(Looper.getMainLooper()).idle()
            assertEquals(0, changes.get())
        } finally { release.countDown(); presentation.close(); scope.cancel() }
    }

    @Test fun preparingStopRemainsDisabledWhenIdentityArrivesAndOldFailureCannotLatchANewCapture() {
        val scope = CoroutineScope(SupervisorJob() + Dispatchers.Main.immediate)
        var current: CaptureRecord? = null
        val presentation = CapturePresentation(scope, { current }, { null }, { emptyList() }, {}, { fail("No durable save") })
        try {
            presentation.beginPreparation(7)
            presentation.resume(); settle(presentation)
            assertEquals(CapturePreparationReceipt(7), presentation.preparationReceipt)
            presentation.requestStop()
            current = old
            presentation.refresh(false); settle(presentation)
            assertEquals(old.captureId, presentation.stopRequestedCaptureId)
            assertFalse(presentation.stopEnabled)
            current = fresh.copy(captureState = CaptureState.STOPPING)
            presentation.signal("failed", old.captureId, false, "Synthetic failed old capture")
            settle(presentation)
            assertEquals(fresh.captureId, presentation.snapshot.active?.captureId)
            assertFalse(presentation.terminalSaveFailure)
            assertNull(presentation.savedFeedbackCaptureId)
        } finally { presentation.close(); scope.cancel() }
    }

    @Test fun definitiveServiceOutcomesRetirePreparationByStateNotByDisplayText() {
        val scope = CoroutineScope(SupervisorJob() + Dispatchers.Main.immediate)
        val presentation = CapturePresentation(scope, { null }, { null }, { emptyList() }, {}, { fail("No durable save") })
        try {
            for (state in listOf("failed", "saved", "completed")) {
                presentation.beginPreparation(7)
                presentation.feedback = "Synthetic unrelated caption"
                assertEquals(CapturePreparationReceipt(7), presentation.preparationReceipt)
                presentation.signal(state, null, false, "Capture status unavailable · audio retained")
                settle(presentation)
                assertNull(presentation.preparationReceipt)
            }
            presentation.beginPreparation(8)
            presentation.requestStop()
            assertNull(presentation.preparationReceipt)
        } finally { presentation.close(); scope.cancel() }
    }

    @Test fun preparationIdentityOffersCancellationBeforeMicrophoneReadinessWithoutClaimingRecording() {
        val scope = CoroutineScope(SupervisorJob() + Dispatchers.Main.immediate)
        var current = old.copy(captureState = CaptureState.PREPARING, observedFrames = 0)
        val presentation = CapturePresentation(scope, { current }, { null }, { emptyList() }, {}, { fail("No durable save") })
        try {
            presentation.beginPreparation(7)
            presentation.resume(); settle(presentation)
            assertNull("An established capture identity replaces the pre-identity receipt", presentation.preparationReceipt)
            assertTrue("Preparation is cancellable before the first microphone buffer", presentation.stopEnabled)
            assertFalse(presentation.backgroundListening)
            assertNull(presentation.microphoneReadyCaptureId)
            presentation.requestStop()
            assertFalse(presentation.stopEnabled)
            current = current.copy(captureState = CaptureState.RECORDING, observedFrames = 1)
            presentation.signal("ready", current.captureId, true, null); settle(presentation)
            assertFalse("Late readiness cannot re-enable a stopped control", presentation.stopEnabled)
            assertFalse(presentation.backgroundListening)
        } finally { presentation.close(); scope.cancel() }
    }

    @Test fun savedBeforeFirstSnapshotStillRequiresDurableSavedIdentityBeforeConfirmation() {
        val scope = CoroutineScope(SupervisorJob() + Dispatchers.Main.immediate)
        val entered = CountDownLatch(1)
        val release = CountDownLatch(1)
        val reads = AtomicInteger()
        val presentation = CapturePresentation(scope, {
            if (reads.incrementAndGet() == 1) { entered.countDown(); check(release.await(5, TimeUnit.SECONDS)); old }
            else null
        }, { id -> old.copy(captureState = CaptureState.SAVED).takeIf { id == old.captureId } }, { emptyList() }, {}, {})
        try {
            presentation.resume(); assertTrue(entered.await(2, TimeUnit.SECONDS))
            presentation.signal("saved", old.captureId, false, null)
            release.countDown(); settle(presentation)
            assertNull(presentation.snapshot.active)
            assertEquals(old.captureId, presentation.savedFeedbackCaptureId)
            assertEquals(old.captureId, presentation.observedCaptureId)
        } finally { release.countDown(); presentation.close(); scope.cancel() }
    }
    private fun settle(presentation: CapturePresentation) {
        val deadline = System.nanoTime() + TimeUnit.SECONDS.toNanos(5)
        while (presentation.refreshing) {
            shadowOf(Looper.getMainLooper()).idle()
            check(System.nanoTime() < deadline)
            Thread.sleep(1)
        }
    }
}
