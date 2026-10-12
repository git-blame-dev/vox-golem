package dev.gitblame.voxgolem.assistant.recognition

import android.os.Handler
import android.os.Looper
import android.os.Message
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.Shadows.shadowOf
import org.robolectric.annotation.Config
import org.robolectric.annotation.LooperMode
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicBoolean

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35], manifest = Config.NONE)
@LooperMode(LooperMode.Mode.PAUSED)
class MainThreadRetirementRetryTest {
    @Test fun mainCallerRetriesInlineWithoutWaitingForAnotherFrame() {
        val main = object : Handler(Looper.getMainLooper()) {
            override fun sendMessageAtTime(message: Message, uptimeMillis: Long): Boolean =
                error("Synthetic main retry must not enqueue itself")
        }
        var attempts = 0
        assertTrue(retryRetirementOnMain(main) {
            assertEquals(Looper.getMainLooper(), Looper.myLooper()); attempts++; true
        })
        assertEquals(1, attempts)
    }

    @Test fun workerTimeoutKeepsAdmissionUntilTheSingleQueuedTeardownActuallyCompletes() {
        val posted = CountDownLatch(1)
        val main = object : Handler(Looper.getMainLooper()) {
            override fun sendMessageAtTime(message: Message, uptimeMillis: Long): Boolean {
                val queued = super.sendMessageAtTime(message, uptimeMillis); posted.countDown(); return queued
            }
        }
        val leases = RecognitionResourceLease()
        val held = leases.tryAcquire(RecognitionResourceLease.Kind.CHAT)!!
        var attempts = 0
        held.onRetirementRetry {
            retryRetirementOnMain(main, timeoutMillis = 50) {
                assertEquals(Looper.getMainLooper(), Looper.myLooper())
                attempts++; held.close(); true
            }
        }
        val completed = AtomicBoolean(true)
        val worker = Thread { completed.set(leases.retryFailedRetirement()) }
        worker.start(); assertTrue(posted.await(1, TimeUnit.SECONDS)); worker.join(1_000)
        assertFalse(worker.isAlive); assertFalse(completed.get())
        assertEquals(0, attempts); assertSame(held, leases.current())
        shadowOf(Looper.getMainLooper()).idle()
        assertEquals(1, attempts); assertNull(leases.current())
        shadowOf(Looper.getMainLooper()).idle(); assertEquals(1, attempts)
    }
}
