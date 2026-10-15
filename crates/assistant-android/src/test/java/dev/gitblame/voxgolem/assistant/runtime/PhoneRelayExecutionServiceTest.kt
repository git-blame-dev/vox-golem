package dev.gitblame.voxgolem.assistant.runtime

import android.app.Notification
import android.app.NotificationManager
import android.content.ComponentName
import android.content.ContextWrapper
import android.content.Intent
import android.content.pm.ServiceInfo
import android.os.Build
import android.os.Looper
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.Robolectric
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment
import org.robolectric.Shadows.shadowOf
import org.robolectric.annotation.Config
import org.robolectric.shadows.ShadowLog
import org.robolectric.shadows.ShadowPowerManager
import java.time.Duration
import java.util.concurrent.CountDownLatch
import java.util.concurrent.ExecutionException
import java.util.concurrent.Executor
import java.util.concurrent.Executors
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicInteger
import java.util.concurrent.atomic.AtomicBoolean

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [26, 34], manifest = Config.NONE)
class PhoneRelayExecutionServiceTest {
    private inner class Fixture(denyStart: Boolean = false) : AutoCloseable {
        val launched = CountDownLatch(1)
        val done = CountDownLatch(1)
        val cancellations = AtomicInteger()
        val cancelledWhileProtected = AtomicBoolean()
        var intent: Intent? = null
        var lease: AutoCloseable? = null
        val controller = Robolectric.buildService(PhoneRelayExecutionService::class.java).create()
        val service = controller.get()
        val context = object : ContextWrapper(RuntimeEnvironment.getApplication()) {
            override fun startForegroundService(service: Intent): ComponentName {
                intent = service
                launched.countDown()
                if (denyStart) throw IllegalStateException("SYNTHETIC_PRIVATE_START_FAILURE")
                return ComponentName(this, PhoneRelayExecutionService::class.java)
            }
        }
        val executor = Executors.newSingleThreadExecutor()
        val acquisition = executor.submit<AutoCloseable> {
            try {
                PhoneRelayExecution.acquire(context, PhoneRelayExecution.MAX_EXECUTION_MS) {
                    cancelledWhileProtected.set(ShadowPowerManager.getLatestWakeLock()?.isHeld == true)
                    cancellations.incrementAndGet()
                }.also { lease = it }
            } finally { done.countDown() }
        }
        init { assertTrue(launched.await(5, TimeUnit.SECONDS)) }
        fun promote(startId: Int = 1): AutoCloseable {
            assertEquals(android.app.Service.START_NOT_STICKY, service.onStartCommand(intent, 0, startId))
            return acquisition.get(5, TimeUnit.SECONDS)
        }
        override fun close() {
            if (!acquisition.isDone) acquisition.cancel(true)
            assertTrue(done.await(5, TimeUnit.SECONDS))
            lease?.close()
            shadowOf(Looper.getMainLooper()).idle()
            controller.destroy()
            executor.shutdownNow()
        }
    }

    @Test fun promotionIsConfirmedBeforeAdmissionAndProtectionLastsThroughOutcomeUntilRetired() {
        Fixture().use { fixture ->
            assertFalse(fixture.acquisition.isDone)
            assertNull(shadowOf(fixture.service).lastForegroundNotification)
            val lease = fixture.promote()
            val notification = requireNotNull(shadowOf(fixture.service).lastForegroundNotification)
            assertEquals("Paired watch assistant", notification.extras.getString(Notification.EXTRA_TITLE))
            assertEquals("Processing a request from your paired watch", notification.extras.getString(Notification.EXTRA_TEXT))
            assertTrue(notification.flags and Notification.FLAG_ONGOING_EVENT != 0)
            val channel = fixture.context.getSystemService(NotificationManager::class.java).getNotificationChannel(PhoneRelayExecutionService.CHANNEL)
            assertEquals(NotificationManager.IMPORTANCE_LOW, channel.importance)
            if (Build.VERSION.SDK_INT >= 34) assertEquals(ServiceInfo.FOREGROUND_SERVICE_TYPE_SPECIAL_USE, fixture.service.foregroundServiceType)
            val admitted = AtomicInteger()
            lateinit var retired: () -> Unit
            val worker = AssistantRequestWorker(Executor { it.run() }, object : ProviderNativeCalls {
                override fun respond(requestId: String, authPath: String, requestJson: String, callback: ProviderDeltaCallback): String {
                    assertNotNull(shadowOf(fixture.service).lastForegroundNotification)
                    admitted.incrementAndGet()
                    callback.onReady()
                    return """{"status":"complete","text":"Synthetic answer"}"""
                }
                override fun cancel(requestId: String) {}
                override fun whenRetired(requestId: String, action: () -> Unit) { retired = action }
            })
            assertTrue(worker.submit("synthetic-owned-request", "", "{}", object : AssistantRequestListener {
                override fun onDelta(requestId: String, text: String) {}
                override fun onOutcome(requestId: String, outcomeJson: String) {}
                override fun onRetired(requestId: String) { lease.close() }
            }))
            assertEquals(1, admitted.get())
            assertTrue(ShadowPowerManager.getLatestWakeLock().isHeld)
            assertFalse(shadowOf(fixture.service).isForegroundStopped)
            fixture.service.onTaskRemoved(Intent())
            assertFalse(shadowOf(fixture.service).isStoppedBySelf)
            retired()
            shadowOf(Looper.getMainLooper()).idle()
            assertFalse(ShadowPowerManager.getLatestWakeLock().isHeld)
            assertTrue(shadowOf(fixture.service).isForegroundStopped)
            assertTrue(shadowOf(fixture.service).isStoppedBySelf)
            assertEquals(0, fixture.cancellations.get())
            lease.close()
            worker.close()
        }
    }

    @Test fun backgroundStartDenialFailsClosedReleasesWakeAndRedactsPlatformError() {
        Fixture(denyStart = true).use { fixture ->
            try { fixture.acquisition.get(5, TimeUnit.SECONDS); fail("Expected safe admission failure") }
            catch (failure: ExecutionException) {
                assertEquals("Phone relay execution unavailable", failure.cause?.message)
                assertNull(failure.cause?.cause)
            }
            assertFalse(ShadowPowerManager.getLatestWakeLock().isHeld)
            assertNull(shadowOf(fixture.service).lastForegroundNotification)
            assertEquals(1, fixture.cancellations.get())
        }
    }

    @Test fun absentPromotionTimesOutAndLateStartCannotAdmitWork() {
        Fixture().use { fixture ->
            try { fixture.acquisition.get(6, TimeUnit.SECONDS); fail("Expected promotion timeout") }
            catch (failure: ExecutionException) { assertEquals("Phone relay execution unavailable", failure.cause?.message) }
            fixture.service.onStartCommand(fixture.intent, 0, 1)
            assertNull(shadowOf(fixture.service).lastForegroundNotification)
            assertTrue(shadowOf(fixture.service).isStoppedBySelf)
            assertFalse(ShadowPowerManager.getLatestWakeLock().isHeld)
        }
    }

    @Test fun unexpectedServiceDestructionCancelsBeforeReleasingProtectionExactlyOnce() {
        Fixture().use { fixture ->
            val lease = fixture.promote()
            fixture.service.onDestroy()
            assertEquals(1, fixture.cancellations.get())
            assertTrue(fixture.cancelledWhileProtected.get())
            assertFalse(ShadowPowerManager.getLatestWakeLock().isHeld)
            lease.close()
            assertEquals(1, fixture.cancellations.get())
        }
    }

    @Test fun hardDeadlineCancelsAndReportsUnconfirmedRetirementRatherThanKeepingServiceForever() {
        Fixture().use { fixture ->
            ShadowLog.clear()
            val lease = fixture.promote()
            shadowOf(Looper.getMainLooper()).idleFor(Duration.ofMillis(PhoneRelayExecution.MAX_EXECUTION_MS + 1))
            assertEquals(1, fixture.cancellations.get())
            assertFalse(ShadowPowerManager.getLatestWakeLock().isHeld)
            assertTrue(shadowOf(fixture.service).isForegroundStopped)
            assertTrue(ShadowLog.getLogsForTag("PhoneRelay").any { it.msg == "relay_execution_deadline" })
            lease.close()
            assertEquals(1, fixture.cancellations.get())
        }
    }

    @Test fun staleNormalCleanupCannotStopNewOperationOnSameServiceInstance() {
        Fixture().use { first ->
            val lease = first.promote()
            lease.close() // Cleanup is posted, not run yet.
            val nextLaunch = CountDownLatch(1)
            var nextIntent: Intent? = null
            val context = object : ContextWrapper(first.context) {
                override fun startForegroundService(service: Intent): ComponentName {
                    nextIntent = service; nextLaunch.countDown()
                    return ComponentName(this, PhoneRelayExecutionService::class.java)
                }
            }
            val executor = Executors.newSingleThreadExecutor()
            try {
                val acquisition = executor.submit<AutoCloseable> { PhoneRelayExecution.acquire(context, 215_000) { fail("No service loss expected") } }
                assertTrue(nextLaunch.await(5, TimeUnit.SECONDS))
                first.service.onStartCommand(nextIntent, 0, 2)
                val next = acquisition.get(5, TimeUnit.SECONDS)
                try {
                    shadowOf(Looper.getMainLooper()).idle()
                    assertFalse(shadowOf(first.service).isForegroundStopped)
                    assertTrue(ShadowPowerManager.getLatestWakeLock().isHeld)
                } finally { next.close() }
            } finally { executor.shutdownNow() }
        }
    }
}
