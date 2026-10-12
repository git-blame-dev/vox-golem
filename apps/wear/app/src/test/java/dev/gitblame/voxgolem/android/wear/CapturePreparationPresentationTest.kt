package dev.gitblame.voxgolem.android.wear

import android.Manifest
import android.content.Intent
import android.os.Bundle
import androidx.lifecycle.lifecycleScope
import dev.gitblame.voxgolem.android.wear.capture.CaptureService
import dev.gitblame.voxgolem.capture.CaptureRepository
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.Robolectric
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment
import org.robolectric.Shadows.shadowOf
import org.robolectric.annotation.Config
import org.robolectric.util.ReflectionHelpers

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35])
class CapturePreparationPresentationTest {
    private val context = RuntimeEnvironment.getApplication()

    @Test fun issuedStartBeforeIdentityRestoresCancellablePreparationWithoutReplayingStart() {
        val saved = issuedStartState()
        val controller = Robolectric.buildActivity(CaptureActivity::class.java, Intent(Intent.ACTION_MAIN))
            .create(saved).start().resume()
        try {
            val activity = controller.get()
            awaitCaptureIdle(activity)
            assertNull(activity.presentation.snapshot.active)
            assertEquals("Preparing microphone…", activity.presentation.feedback)
            assertTrue(activity.preparingCapture)
            assertTrue("Recreation only observes; it cannot issue another Start", commands().isEmpty())
            activity.onBackPressedDispatcher.onBackPressed()
            awaitCaptureIdle(activity)
            assertFalse(activity.preparingCapture)
            assertEquals(listOf(CaptureService.ACTION_STOP), commands().map { it.action })
            assertNull(activity.presentation.savedFeedbackCaptureId)
            assertTrue("No service or recorder was executed by this UI test", CaptureRepository(context).list().isEmpty())
        } finally { controller.pause().stop().destroy() }
    }

    @Test fun failureAndStopCannotBeRestoredAsPreparationEvenWhenStartWasIssued() {
        for (stopped in listOf(false, true)) {
            val saved = issuedStartState { activity ->
                if (stopped) activity.stopCapture()
                else activity.presentation.signal("failed", null, false, "Synthetic preparation failure")
                awaitCaptureIdle(activity)
                assertFalse(activity.preparingCapture)
                assertNull(activity.presentation.preparationReceipt)
            }
            val controller = Robolectric.buildActivity(CaptureActivity::class.java, Intent(Intent.ACTION_MAIN))
                .create(saved).start().resume()
            try {
                awaitCaptureIdle(controller.get())
                assertFalse(controller.get().preparingCapture)
                assertNotEquals("Preparing microphone…", controller.get().presentation.feedback)
                assertTrue(commands().isEmpty())
            } finally { controller.pause().stop().destroy(); commands() }
        }
    }

    @Test fun preparationProjectionRequiresSavedIssuedConsentAndCannotOverrideSavedOrStoppedFeedback() {
        val issued = issuedStartState()
        val states = listOf(
            Bundle(issued).apply { putBoolean("captureStartIssued", false) },
            Bundle(issued).apply { putBoolean("stopBeforeIdentity", true) },
            Bundle(issued).apply { putString("stopRequestedCaptureId", "synthetic-stopped-capture") },
            Bundle(issued).apply { putString("savedFeedbackCaptureId", "synthetic-saved-capture") },
            Bundle(issued).apply { putString("observedCaptureId", "synthetic-established-capture") },
            Bundle(issued).apply { putLong("launchGeneration", getLong("launchGeneration") + 1) },
            Bundle(issued).apply { remove("capturePreparationGeneration") },
            Bundle(),
        )
        for (state in states) {
            val controller = Robolectric.buildActivity(CaptureActivity::class.java, Intent(Intent.ACTION_MAIN)).create(state)
            try {
                awaitCaptureIdle(controller.get())
                assertFalse(controller.get().preparingCapture)
                assertNotEquals("Preparing microphone…", controller.get().presentation.feedback)
                assertTrue(commands().isEmpty())
            } finally { controller.destroy() }
        }
    }

    @Test fun issuedPreparationSurvivesReadFailureThenNullObservationAndRecreationUntilExplicitStop() {
        val controller = Robolectric.buildActivity(CaptureActivity::class.java, Intent(Intent.ACTION_VIEW))
        val activity = controller.get()
        val failRead = java.util.concurrent.atomic.AtomicBoolean(false)
        val presentation = CapturePresentation(activity.lifecycleScope,
            { if (failRead.get()) error("Synthetic snapshot read failure") else null },
            { null }, { emptyList() }, {}, { fail("No durable save") })
        ReflectionHelpers.setField(activity, "presentation\$delegate", lazy { presentation })
        shadowOf(context).grantPermissions(Manifest.permission.RECORD_AUDIO)
        val saved: Bundle
        try {
            controller.create().start().resume()
            awaitCaptureIdle(activity)
            commands()
            activity.requestStart()
            awaitCaptureIdle(activity)
            assertEquals(listOf(CaptureService.ACTION_START), commands().map { it.action })
            assertTrue(activity.preparingCapture)
            failRead.set(true)
            presentation.refresh(false)
            awaitCaptureIdle(activity)
            assertTrue("An observation exception is not a service terminal outcome", activity.preparingCapture)
            assertEquals("Capture status unavailable · audio retained", presentation.observationError)
            assertEquals("Preparing microphone…\nCapture status unavailable · audio retained",
                capturePreparationText(presentation.observationError))
            presentation.feedback = "Synthetic display-only caption"
            assertTrue("Changing display text cannot retire the issued preparation receipt", activity.preparingCapture)
            assertTrue(commands().isEmpty())
            failRead.set(false)
            presentation.refresh(false)
            awaitCaptureIdle(activity)
            assertNull(presentation.snapshot.active)
            assertNull(presentation.observationError)
            assertTrue("A successful null observation cannot retire an outstanding issued Start", activity.preparingCapture)
            saved = Bundle().also { controller.saveInstanceState(it) }
        } finally {
            ReflectionHelpers.setField(activity, "mChangingConfigurations", true)
            controller.pause().stop().destroy()
            commands()
        }
        val recreated = Robolectric.buildActivity(CaptureActivity::class.java, Intent(Intent.ACTION_MAIN))
            .create(saved).start().resume()
        try {
            awaitCaptureIdle(recreated.get())
            assertTrue(recreated.get().preparingCapture)
            assertTrue(commands().isEmpty())
            recreated.get().stopCapture()
            assertFalse(recreated.get().preparingCapture)
            assertNull(recreated.get().presentation.preparationReceipt)
            assertEquals(listOf(CaptureService.ACTION_STOP), commands().map { it.action })
        } finally { recreated.pause().stop().destroy() }
    }

    private fun issuedStartState(beforeSave: (CaptureActivity) -> Unit = {}): Bundle {
        shadowOf(context).grantPermissions(Manifest.permission.RECORD_AUDIO)
        val controller = Robolectric.buildActivity(CaptureActivity::class.java, Intent(Intent.ACTION_VIEW))
            .create().start().resume()
        try {
            val activity = controller.get()
            awaitCaptureIdle(activity)
            commands()
            activity.requestStart()
            awaitCaptureIdle(activity)
            assertEquals(listOf(CaptureService.ACTION_START), commands().map { it.action })
            assertNull(activity.presentation.snapshot.active)
            assertTrue(activity.preparingCapture)
            beforeSave(activity)
            return Bundle().also { controller.saveInstanceState(it) }
        } finally {
            ReflectionHelpers.setField(controller.get(), "mChangingConfigurations", true)
            controller.pause().stop().destroy()
            commands()
        }
    }

    private fun commands(): List<Intent> = buildList {
        while (true) add(shadowOf(context).nextStartedService ?: break)
    }
}
