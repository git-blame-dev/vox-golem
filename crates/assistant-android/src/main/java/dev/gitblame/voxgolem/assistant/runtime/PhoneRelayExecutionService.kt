package dev.gitblame.voxgolem.assistant.runtime

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.Service
import android.content.Context
import android.content.Intent
import android.content.pm.ServiceInfo
import android.os.Build
import android.os.Handler
import android.os.IBinder
import android.os.Looper
import android.util.Log
import java.util.UUID
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit

/** Operation-scoped process protection; no restart, audio, provider payload, or persistent owner. */
class PhoneRelayExecutionService : Service() {
    private var operation: String? = null
    private var command = 0
    override fun onBind(intent: Intent?): IBinder? = null

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        val token = intent?.getStringExtra(PhoneRelayExecution.TOKEN)
        if (token == null || !PhoneRelayExecution.promote(this, token, startId)) {
            if (operation == null) stopSelf(startId)
        }
        return START_NOT_STICKY
    }

    internal fun promote(token: String, startId: Int) {
        if (Build.VERSION.SDK_INT < 26) throw IllegalStateException("Phone relay execution requires Android 26")
        val manager = getSystemService(NotificationManager::class.java)
        manager.createNotificationChannel(NotificationChannel(CHANNEL, "Paired watch assistant", NotificationManager.IMPORTANCE_LOW))
        val notification = Notification.Builder(this, CHANNEL)
            .setSmallIcon(android.R.drawable.stat_notify_sync_noanim)
            .setContentTitle("Paired watch assistant")
            .setContentText("Processing a request from your paired watch")
            .setCategory(Notification.CATEGORY_SERVICE).setVisibility(Notification.VISIBILITY_PRIVATE)
            .setOngoing(true).setOnlyAlertOnce(true).setShowWhen(false).setLocalOnly(true).build()
        if (Build.VERSION.SDK_INT >= 34) startForeground(NOTIFICATION, notification, ServiceInfo.FOREGROUND_SERVICE_TYPE_SPECIAL_USE)
        else startForeground(NOTIFICATION, notification)
        operation = token
        command = startId
    }

    internal fun finish(token: String) {
        if (operation != token) return // A newer operation may already own this same Service instance.
        operation = null
        stopForeground(STOP_FOREGROUND_REMOVE)
        stopSelf(command)
    }

    override fun onDestroy() {
        PhoneRelayExecution.destroyed(this)
        super.onDestroy()
    }

    companion object {
        internal const val CHANNEL = "assistant-phone-relay"
        private const val NOTIFICATION = 4207
    }
}

/** One process slot, shared with the channel's one-admission owner. Acquire only off the main thread. */
internal object PhoneRelayExecution {
    internal const val TOKEN = "relay-execution-token"
    private const val PROMOTION_MS = 4_000L
    internal const val MAX_EXECUTION_MS = 215_000L
    private val main = Handler(Looper.getMainLooper())
    private val lock = Any()
    private var active: Operation? = null
    private class Operation(val cancel: () -> Unit) {
        val startedNanos = System.nanoTime()
        val token = UUID.randomUUID().toString()
        val ready = CountDownLatch(1)
        var wake: AutoCloseable? = null
        var service: PhoneRelayExecutionService? = null
        var deadline: Runnable? = null
        var promoted = false
        var ended = false
    }

    fun acquire(context: Context, timeoutMs: Long, cancel: () -> Unit): AutoCloseable {
        check(Looper.myLooper() != Looper.getMainLooper()) { "Relay protection requires a background worker" }
        if (Build.VERSION.SDK_INT < 26) throw IllegalStateException("Phone relay execution requires Android 26")
        require(timeoutMs in 1..MAX_EXECUTION_MS)
        val operation = synchronized(lock) {
            check(active == null) { "Phone relay execution unavailable" }
            Operation(cancel).also { active = it }
        }
        try {
            val wake = PhoneRelayWakeLease(context, timeoutMs)
            synchronized(lock) { operation.wake = wake }
            context.startForegroundService(Intent(context, PhoneRelayExecutionService::class.java).putExtra(TOKEN, operation.token))
            check(operation.ready.await(PROMOTION_MS, TimeUnit.MILLISECONDS))
            synchronized(lock) {
                check(active === operation && operation.promoted && !operation.ended)
                val deadline = Runnable {
                    end(operation, cancel = true, reason = "relay_execution_deadline")
                }
                operation.deadline = deadline
                val elapsedMs = TimeUnit.NANOSECONDS.toMillis(System.nanoTime() - operation.startedNanos)
                check(main.postDelayed(deadline, (timeoutMs - elapsedMs).coerceAtLeast(0)))
            }
            return AutoCloseable { end(operation, cancel = false) }
        } catch (failure: Exception) {
            if (failure is InterruptedException) Thread.currentThread().interrupt()
            Log.w("PhoneRelay", "relay_execution_unavailable")
            end(operation, cancel = true)
            // Do not forward platform exceptions, including component names or background-start details.
            throw IllegalStateException("Phone relay execution unavailable")
        }
    }

    fun promote(service: PhoneRelayExecutionService, token: String, startId: Int): Boolean {
        val operation = synchronized(lock) { active?.takeIf { it.token == token && !it.ended } } ?: return false
        try {
            synchronized(lock) {
                if (active !== operation || operation.ended) return false
                service.promote(token, startId)
                operation.service = service
                operation.promoted = true
                operation.ready.countDown() // Provider admission can proceed only after startForeground returns.
            }
            return true
        } catch (_: RuntimeException) {
            Log.w("PhoneRelay", "relay_foreground_promotion_failed")
            end(operation, cancel = true)
            return false
        }
    }

    fun destroyed(service: PhoneRelayExecutionService) {
        val operation = synchronized(lock) { active?.takeIf { it.service === service } } ?: return
        end(operation, cancel = true, reason = "relay_execution_service_lost")
    }

    private fun end(operation: Operation, cancel: Boolean, reason: String? = null) {
        val resources = synchronized(lock) {
            if (operation.ended) return
            operation.ended = true
            if (active === operation) active = null
            operation.deadline?.let(main::removeCallbacks)
            operation.deadline = null
            operation.ready.countDown()
            (operation.wake to operation.service).also { operation.wake = null; operation.service = null }
        }
        if (reason != null) Log.w("PhoneRelay", reason)
        // On service loss/deadline, cancel the exact channel-owned worker before releasing protection.
        // This does not confirm retirement: the channel runtime retains its one-entry quarantine.
        if (cancel) try { operation.cancel() } catch (_: RuntimeException) { Log.w("PhoneRelay", "relay_execution_cancel_failed") }
        try { resources.first?.close() } catch (_: RuntimeException) { Log.w("PhoneRelay", "relay_wake_release_failed") }
        resources.second?.let { service -> main.post { service.finish(operation.token) } }
    }
}
