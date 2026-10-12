package dev.gitblame.voxgolem.capture

import android.Manifest
import android.content.Context
import android.content.Intent
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.Robolectric
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment
import org.robolectric.Shadows.shadowOf
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35])
class PhoneCaptureRuntimeServiceTest {
    private val context: Context = RuntimeEnvironment.getApplication()
    private val preferences by lazy {
        context.getSharedPreferences(PhoneCaptureCoordinator.PREFERENCES, Context.MODE_PRIVATE)
    }
    private var controller: org.robolectric.android.controller.ServiceController<PhoneCaptureRuntimeService>? = null

    @Before
    fun setUp() {
        preferences.edit().clear().commit()
        shadowOf(RuntimeEnvironment.getApplication()).grantPermissions(
            Manifest.permission.RECORD_AUDIO,
            Manifest.permission.BLUETOOTH_CONNECT,
            Manifest.permission.POST_NOTIFICATIONS,
        )
    }

    @After
    fun tearDown() {
        controller?.destroy()
        controller = null
        preferences.edit().clear().commit()
        assertFalse(PhoneCaptureRuntimeService.isForegroundReady())
    }

    @Test
    fun optedInNullIntentRestartBecomesForegroundReadyAndSticky() {
        preferences.edit().putBoolean(PhoneCaptureCoordinator.KEY_OPTED_IN, true).commit()

        val result = start(null)

        assertEquals(android.app.Service.START_STICKY, result)
        assertTrue(PhoneCaptureRuntimeService.isForegroundReady())
        assertNotNull(shadowOf(controller!!.get()).lastForegroundNotification)
    }

    @Test
    fun nullIntentRestartWithoutOptInDoesNotReviveRuntime() {
        val result = start(null)

        assertEquals(android.app.Service.START_NOT_STICKY, result)
        assertFalse(PhoneCaptureRuntimeService.isForegroundReady())
        assertEquals(null, preferences.getString(PhoneCaptureCoordinator.KEY_RUNTIME_ERROR, null))
    }

    @Test
    fun missingConnectedDevicePermissionStaysNotStickyAndKeepsSpecificError() {
        preferences.edit().putBoolean(PhoneCaptureCoordinator.KEY_OPTED_IN, true).commit()
        shadowOf(RuntimeEnvironment.getApplication()).denyPermissions(Manifest.permission.BLUETOOTH_CONNECT)

        val result = start(Intent(context, PhoneCaptureRuntimeService::class.java))

        assertEquals(android.app.Service.START_NOT_STICKY, result)
        assertFalse(PhoneCaptureRuntimeService.isForegroundReady())
        assertEquals(
            "bluetooth-connect-permission-required",
            preferences.getString(PhoneCaptureCoordinator.KEY_RUNTIME_ERROR, null),
        )
    }

    @Test
    fun runtimeTeardownClearsForegroundReadiness() {
        preferences.edit().putBoolean(PhoneCaptureCoordinator.KEY_OPTED_IN, true).commit()
        assertEquals(android.app.Service.START_STICKY, start(null))
        assertTrue(PhoneCaptureRuntimeService.isForegroundReady())

        controller!!.destroy()
        controller = null

        assertFalse(PhoneCaptureRuntimeService.isForegroundReady())
    }

    private fun start(intent: Intent?): Int {
        controller = Robolectric.buildService(PhoneCaptureRuntimeService::class.java).create()
        return controller!!.get().onStartCommand(intent, 0, 1)
    }
}
