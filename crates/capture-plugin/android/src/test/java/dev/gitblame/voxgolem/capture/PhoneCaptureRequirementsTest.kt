package dev.gitblame.voxgolem.capture

import android.Manifest
import app.tauri.annotation.TauriPlugin
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment
import org.robolectric.Shadows.shadowOf
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35])
class PhoneCaptureRequirementsTest {
    @Test fun runtimeFailureDistinguishesConsentPermissionsAndForegroundAdmission() {
        val application = RuntimeEnvironment.getApplication()
        val preferences = application.getSharedPreferences(PhoneCaptureCoordinator.PREFERENCES, android.content.Context.MODE_PRIVATE)
        preferences.edit().clear().commit()
        assertEquals("phone-capture-not-enabled", NativePhoneRequirements.runtimeUnavailableCode(application))
        preferences.edit().putBoolean(PhoneCaptureCoordinator.KEY_OPTED_IN, true).commit()
        val shadow = shadowOf(application)
        val required = NativePhoneRequirements.required()
        shadow.grantPermissions(*required.map { it.permission }.toTypedArray())
        assertEquals("connected-device-service-not-ready", NativePhoneRequirements.runtimeUnavailableCode(application))
        for (requirement in required) {
            shadow.denyPermissions(requirement.permission)
            assertEquals(requirement.error, NativePhoneRequirements.runtimeUnavailableCode(application))
            shadow.grantPermissions(requirement.permission)
        }
        preferences.edit().putString(PhoneCaptureCoordinator.KEY_RUNTIME_ERROR, "connected-device-service-start-denied").commit()
        assertEquals("connected-device-service-start-denied", NativePhoneRequirements.runtimeUnavailableCode(application))
    }
    @Test fun api24Api31Api33AndApi36OnlyRequestSupportedPhonePermissions() {
        val audio = Manifest.permission.RECORD_AUDIO
        val bluetooth = Manifest.permission.BLUETOOTH_CONNECT
        val notifications = Manifest.permission.POST_NOTIFICATIONS
        assertEquals(listOf(audio), NativePhoneRequirements.required(24).map { it.permission })
        assertEquals(listOf(audio, bluetooth), NativePhoneRequirements.required(31).map { it.permission })
        for (sdk in listOf(33, 36)) {
            assertEquals(listOf(audio, bluetooth, notifications), NativePhoneRequirements.required(sdk).map { it.permission })
        }
        val declared = CapturePlugin::class.java.getAnnotation(TauriPlugin::class.java)!!.permissions
            .associate { it.alias to it.strings.toList() }
        assertEquals(NativePhoneRequirements.required(36).associate { it.alias to listOf(it.permission) }, declared)
    }

    @Test
    @Config(sdk = [24, 31, 33, 35])
    fun revocationUsesTheSameAliasPermissionAndNativeFailureCode() {
        val application = RuntimeEnvironment.getApplication()
        val shadow = shadowOf(application)
        val required = NativePhoneRequirements.required()
        shadow.grantPermissions(*required.map { it.permission }.toTypedArray())
        assertTrue(NativePhoneRequirements.missing(application).isEmpty())
        for (requirement in required) {
            shadow.denyPermissions(requirement.permission)
            assertEquals(listOf(requirement), NativePhoneRequirements.missing(application))
            shadow.grantPermissions(requirement.permission)
        }
    }
}
