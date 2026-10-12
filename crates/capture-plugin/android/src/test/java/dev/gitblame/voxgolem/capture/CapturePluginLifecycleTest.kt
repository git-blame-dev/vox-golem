package dev.gitblame.voxgolem.capture

import android.Manifest
import android.content.Context
import android.webkit.WebView
import androidx.activity.ComponentActivity
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
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
class CapturePluginLifecycleTest {
    private val context: Context = RuntimeEnvironment.getApplication()
    private val preferences by lazy { context.getSharedPreferences(PhoneCaptureCoordinator.PREFERENCES, Context.MODE_PRIVATE) }

    @Before fun setUp() {
        preferences.edit().clear().commit()
        context.deleteDatabase("captures.db")
        PhoneCaptureCoordinator.resetSharedForTests()
        shadowOf(RuntimeEnvironment.getApplication()).grantPermissions(
            Manifest.permission.RECORD_AUDIO, Manifest.permission.BLUETOOTH_CONNECT, Manifest.permission.POST_NOTIFICATIONS,
        )
    }

    @After fun tearDown() {
        PhoneCaptureCoordinator.shared(context).shutdownForTests()
        PhoneCaptureCoordinator.resetSharedForTests()
        preferences.edit().clear().commit()
    }

    @Test fun previouslyEnabledRuntimeRestartsOnlyAfterActivityResumes() {
        preferences.edit().putBoolean(PhoneCaptureCoordinator.KEY_OPTED_IN, true).commit()
        val activity = Robolectric.buildActivity(ComponentActivity::class.java).create().start()
        CapturePlugin(activity.get()).load(WebView(activity.get()))
        assertNull(shadowOf(RuntimeEnvironment.getApplication()).nextStartedService)
        activity.resume()
        val start = shadowOf(RuntimeEnvironment.getApplication()).nextStartedService
        assertEquals(PhoneCaptureRuntimeService.ACTION_START, start?.action)
        assertEquals(PhoneCaptureRuntimeService::class.java.name, start?.component?.className)
        activity.pause().stop().destroy()
    }

    @Test fun loadingAfterResumeAlsoRestoresPreviouslyEnabledRuntime() {
        preferences.edit().putBoolean(PhoneCaptureCoordinator.KEY_OPTED_IN, true).commit()
        val activity = Robolectric.buildActivity(ComponentActivity::class.java).create().start().resume()
        CapturePlugin(activity.get()).load(WebView(activity.get()))
        assertEquals(PhoneCaptureRuntimeService.ACTION_START, shadowOf(RuntimeEnvironment.getApplication()).nextStartedService?.action)
        activity.pause().stop().destroy()
    }

    @Test fun activityResumeNeverEnablesCaptureOrRequestsPermissionsWithoutConsent() {
        val activity = Robolectric.buildActivity(ComponentActivity::class.java).create().start()
        CapturePlugin(activity.get()).load(WebView(activity.get()))
        activity.resume()
        assertNull(shadowOf(RuntimeEnvironment.getApplication()).nextStartedService)
        assertEquals(false, preferences.getBoolean(PhoneCaptureCoordinator.KEY_OPTED_IN, false))
        activity.pause().stop().destroy()
    }
}
