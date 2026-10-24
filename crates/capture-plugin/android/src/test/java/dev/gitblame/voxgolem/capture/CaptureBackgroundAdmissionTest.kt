package dev.gitblame.voxgolem.capture

import android.app.Application
import android.app.Service
import android.content.Intent
import android.content.SharedPreferences
import dev.gitblame.voxgolem.voice.AndroidBackgroundAdmissionOwner
import org.junit.Assert.assertEquals
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.Robolectric
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35], application = CaptureBackgroundAdmissionTest.IsolatedApplication::class)
class CaptureBackgroundAdmissionTest {
    class IsolatedApplication : Application(), AndroidBackgroundAdmissionOwner {
        override val backgroundServicesEnabled = false
        override fun getSharedPreferences(name: String, mode: Int): SharedPreferences = error("Unexpected user capture preferences")
    }

    @Test fun isolatedProcessCannotResumeCaptureRuntimeOrTouchUserPreferences() {
        val controller = Robolectric.buildService(PhoneCaptureRuntimeService::class.java).create()
        assertEquals(Service.START_NOT_STICKY, controller.get().onStartCommand(Intent(), 0, 1))
        controller.destroy()
    }

    @Test fun processIsolationAlsoRejectsCaptureCommandsWithoutAnActivityBinding() {
        val activity = Robolectric.buildActivity(android.app.Activity::class.java).get()
        val responses = mutableListOf<String>()
        val mapper = Class.forName("com.fasterxml.jackson.databind.ObjectMapper").getConstructor().newInstance()
        val sender: (Long, String) -> Unit = { _, value -> responses += value }
        val invoke = app.tauri.plugin.Invoke::class.java.constructors.single()
            .newInstance(1L, "snapshot", 10L, 11L, sender, "{}", mapper) as app.tauri.plugin.Invoke
        CapturePlugin(activity).snapshot(invoke)
        assertEquals("capture-unavailable", org.json.JSONObject(responses.single()).getString("message"))
    }
}
