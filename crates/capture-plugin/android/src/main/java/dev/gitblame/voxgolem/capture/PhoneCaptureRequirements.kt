package dev.gitblame.voxgolem.capture

import android.Manifest
import android.content.Context
import android.content.pm.PackageManager
import android.os.Build

/** Phone-only catalogue shared by visible permission requests and native runtime admission. */
internal object NativePhoneRequirements {
    const val AUDIO_ALIAS = "phoneAudio"
    const val CONNECTED_DEVICE_ALIAS = "phoneConnectedDevice"
    const val NOTIFICATIONS_ALIAS = "phoneNotifications"
    const val AUDIO_PERMISSION = Manifest.permission.RECORD_AUDIO
    const val CONNECTED_DEVICE_PERMISSION = Manifest.permission.BLUETOOTH_CONNECT
    const val NOTIFICATIONS_PERMISSION = Manifest.permission.POST_NOTIFICATIONS
    const val AUDIO_ERROR = "record-audio-permission-required"
    const val CONNECTED_DEVICE_ERROR = "bluetooth-connect-permission-required"
    const val NOTIFICATIONS_ERROR = "post-notifications-permission-required"

    data class Requirement(val alias: String, val permission: String, val minimumSdk: Int, val error: String)

    private val catalogue = listOf(
        Requirement(AUDIO_ALIAS, AUDIO_PERMISSION, 24, AUDIO_ERROR),
        Requirement(CONNECTED_DEVICE_ALIAS, CONNECTED_DEVICE_PERMISSION, 31, CONNECTED_DEVICE_ERROR),
        Requirement(NOTIFICATIONS_ALIAS, NOTIFICATIONS_PERMISSION, 33, NOTIFICATIONS_ERROR),
    )

    fun required(sdk: Int = Build.VERSION.SDK_INT): List<Requirement> = catalogue.filter { sdk >= it.minimumSdk }
    fun missing(context: Context): List<Requirement> = required().filter {
        context.checkSelfPermission(it.permission) != PackageManager.PERMISSION_GRANTED
    }
    fun isWatch(context: Context): Boolean = context.packageManager.hasSystemFeature("android.hardware.type.watch")

    fun runtimeUnavailableCode(context: Context): String {
        val preferences = context.getSharedPreferences(PhoneCaptureCoordinator.PREFERENCES, Context.MODE_PRIVATE)
        if (!preferences.getBoolean(PhoneCaptureCoordinator.KEY_OPTED_IN, false)) return "phone-capture-not-enabled"
        missing(context).firstOrNull()?.let { return it.error }
        return preferences.getString(PhoneCaptureCoordinator.KEY_RUNTIME_ERROR, null)?.let { CaptureErrorCodes.safe(it) }
            ?: "connected-device-service-not-ready"
    }
}
