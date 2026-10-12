package dev.gitblame.voxgolem.capture

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.Service
import android.content.Intent
import android.content.pm.ServiceInfo
import android.os.Build
import android.os.IBinder

/** User-visible, non-microphone execution lifetime for processing supplied Wear audio. */
class PhoneCaptureRuntimeService : Service() {
    override fun onCreate() {
        super.onCreate()
        val manager = getSystemService(NotificationManager::class.java)
        if (Build.VERSION.SDK_INT >= 26) {
            manager.createNotificationChannel(NotificationChannel(CHANNEL_ID, "Watch dictation", NotificationManager.IMPORTANCE_LOW))
        }
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        foregroundReady = false
        if (NativePhoneRequirements.isWatch(this)) {
            stopSelfResult(startId)
            return START_NOT_STICKY
        }
        val preferences = getSharedPreferences(PhoneCaptureCoordinator.PREFERENCES, MODE_PRIVATE)
        if (!preferences.getBoolean(PhoneCaptureCoordinator.KEY_OPTED_IN, false)) {
            stopSelfResult(startId)
            return START_NOT_STICKY
        }
        val permissionError = missingPermissionError()
        if (permissionError != null) {
            setRuntimeError(permissionError)
            stopSelfResult(startId)
            return START_NOT_STICKY
        }
        try {
            val builder = if (Build.VERSION.SDK_INT >= 26) Notification.Builder(this, CHANNEL_ID) else Notification.Builder(this)
            val notification = builder
                .setSmallIcon(android.R.drawable.ic_btn_speak_now)
                .setContentTitle("Processing watch dictation")
                .setContentText("Supplied watch audio requested for transcription")
                .setOngoing(true)
                .setCategory(Notification.CATEGORY_SERVICE)
                .build()
            if (Build.VERSION.SDK_INT >= 29) {
                startForeground(NOTIFICATION_ID, notification, ServiceInfo.FOREGROUND_SERVICE_TYPE_CONNECTED_DEVICE)
            } else {
                startForeground(NOTIFICATION_ID, notification)
            }
            foregroundReady = true
            PhoneCaptureCoordinator.shared(this).onRuntimeStarted()
            PhoneCaptureCoordinator.shared(this).retryPendingCaptures()
            getSharedPreferences(PhoneCaptureCoordinator.PREFERENCES, MODE_PRIVATE).edit()
                .remove(PhoneCaptureCoordinator.KEY_RUNTIME_ERROR).apply()
        } catch (_: SecurityException) {
            setRuntimeError("connected-device-service-permission-denied")
            stopSelfResult(startId)
            return START_NOT_STICKY
        } catch (_: RuntimeException) {
            // Includes platform background-FGS denial. Never fall back to an invisible service.
            setRuntimeError("connected-device-service-start-denied")
            stopSelfResult(startId)
            return START_NOT_STICKY
        }
        return START_STICKY
    }

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onDestroy() {
        foregroundReady = false
        CaptureChannelReceiver.stopShared()
        PhoneCaptureCoordinator.shared(this).onRuntimeStopped()
        super.onDestroy()
    }

    private fun setRuntimeError(code: String) {
        getSharedPreferences(PhoneCaptureCoordinator.PREFERENCES, MODE_PRIVATE).edit()
            .putString(PhoneCaptureCoordinator.KEY_RUNTIME_ERROR, code)
            .apply()
    }

    private fun missingPermissionError(): String? = NativePhoneRequirements.missing(this).firstOrNull()?.error

    companion object {
        const val ACTION_START = "dev.gitblame.voxgolem.capture.START_PROCESSING"
        @Volatile private var foregroundReady = false
        fun isForegroundReady(): Boolean = foregroundReady
        private const val CHANNEL_ID = "watch-dictation-processing"
        private const val NOTIFICATION_ID = 0x5647
    }
}
