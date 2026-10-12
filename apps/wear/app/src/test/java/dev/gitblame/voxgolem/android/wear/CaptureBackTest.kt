package dev.gitblame.voxgolem.android.wear

import android.content.Intent
import android.Manifest
import android.content.BroadcastReceiver
import android.content.Context
import android.content.IntentFilter
import android.os.Handler
import android.os.Looper
import androidx.core.content.ContextCompat
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.test.StandardTestDispatcher
import dev.gitblame.voxgolem.android.wear.capture.CaptureService
import dev.gitblame.voxgolem.android.wear.capture.CaptureSignals
import dev.gitblame.voxgolem.capture.CaptureRecord
import dev.gitblame.voxgolem.capture.CaptureRepository
import dev.gitblame.voxgolem.capture.CaptureState
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.Robolectric
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment
import org.robolectric.Shadows.shadowOf
import org.robolectric.annotation.Config
import java.util.UUID

@OptIn(ExperimentalCoroutinesApi::class)
@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35])
class CaptureBackTest {
    @Test fun backDuringPreparationSendsExplicitSaveStopAndDoesNotInventSavedFeedback() {
        val context = RuntimeEnvironment.getApplication()
        val repository = CaptureRepository(context)
        val id = UUID.randomUUID().toString()
        repository.save(CaptureRecord(id, createdAtUtc = "2026-10-06T00:00:00Z", captureState = CaptureState.RECORDING))
        val controller = Robolectric.buildActivity(CaptureActivity::class.java, Intent(Intent.ACTION_VIEW)).create().start().resume()
        val activity = controller.get()
        awaitCaptureIdle(activity)
        activity.onBackPressedDispatcher.onBackPressed()
        awaitCaptureIdle(activity)
        val commands = mutableListOf<Intent>()
        while (true) commands += shadowOf(context).nextStartedService ?: break
        assertEquals("manual_stop", commands.single { it.action == CaptureService.ACTION_STOP }.getStringExtra(CaptureService.EXTRA_STOP_REASON))
        assertFalse(activity.isFinishing)
        assertNull(activity.presentation.savedFeedbackCaptureId)
        repository.update(id) { it.copy(captureState = CaptureState.INTERRUPTED) }
        controller.pause().stop().destroy()
    }

    @Test fun duplicateBackThenLifecycleInterruptionUsesTheFirstManualStopAndCommitsOneSavedWav() {
        assertExitCommitsOneSavedWav(repeatedBack = true, finish = false)
    }

    @Test fun nativeHomeWithoutBackCommitsOneDurableSavedWav() {
        assertExitCommitsOneSavedWav(repeatedBack = false, finish = false)
    }

    @Test fun nativeFinishWithoutBackCommitsOneDurableSavedWav() {
        assertExitCommitsOneSavedWav(repeatedBack = false, finish = true)
    }

    @Test fun repeatedBackThenNativeFinishStillCommitsOneDurableSavedWav() {
        assertExitCommitsOneSavedWav(repeatedBack = true, finish = true)
    }

    @Test fun nativeFinishAfterStartIssuedButBeforeFirstSnapshotStillSavesTheWav() {
        assertExitCommitsOneSavedWav(repeatedBack = false, finish = true, beforeIdentity = true)
    }

    private fun assertExitCommitsOneSavedWav(repeatedBack: Boolean, finish: Boolean, beforeIdentity: Boolean = false) {
        val context = RuntimeEnvironment.getApplication()
        shadowOf(context).grantPermissions(Manifest.permission.RECORD_AUDIO)
        val secondRead = java.util.concurrent.CountDownLatch(1)
        val releaseRead = java.util.concurrent.CountDownLatch(1)
        val reads = java.util.concurrent.atomic.AtomicInteger()
        val prefix = byteArrayOf(1, 0, 2, 0)
        val lateBuffer = byteArrayOf(3, 0, 4, 0)
        org.robolectric.shadows.ShadowAudioRecord.setSourceProvider {
            object : org.robolectric.shadows.ShadowAudioRecord.AudioRecordSource {
                override fun readInByteArray(bytes: ByteArray, offset: Int, size: Int, blocking: Boolean): Int {
                    if (reads.incrementAndGet() == 1) { prefix.copyInto(bytes, offset); return prefix.size }
                    secondRead.countDown()
                    check(releaseRead.await(5, java.util.concurrent.TimeUnit.SECONDS))
                    lateBuffer.copyInto(bytes, offset)
                    return lateBuffer.size
                }
            }
        }
        val serviceController = Robolectric.buildService(CaptureService::class.java).create()
        val service = serviceController.get()
        val preparation = StandardTestDispatcher()
        service.preparationDispatcher = preparation
        var recordingThread: Thread? = null
        var activityController: org.robolectric.android.controller.ActivityController<CaptureActivity>? = null
        val savedCaptureIds = java.util.concurrent.ConcurrentLinkedQueue<String>()
        val savedProofs = java.util.concurrent.ConcurrentLinkedQueue<Boolean>()
        val repository = CaptureRepository(context)
        val savedReceiver = object : BroadcastReceiver() {
            override fun onReceive(context: Context?, intent: Intent?) {
                if (intent?.getStringExtra("state") == "saved") intent.getStringExtra("captureId")?.let { id ->
                    val committed = repository.get(id)
                    savedProofs.add(committed?.captureState == CaptureState.SAVED && committed.localAudioPath?.let { path ->
                        val wav = java.io.File(path)
                        wav.isFile && wav.length() == committed.finalizedByteLength
                    } == true)
                    savedCaptureIds.add(id)
                }
            }
        }
        ContextCompat.registerReceiver(context, savedReceiver, IntentFilter(CaptureSignals.SIGNAL_ACTION),
            null, Handler(Looper.getMainLooper()), ContextCompat.RECEIVER_NOT_EXPORTED)
        try {
            if (beforeIdentity) {
                activityController = Robolectric.buildActivity(CaptureActivity::class.java, Intent(Intent.ACTION_VIEW)).create().start().resume()
                awaitCaptureIdle(activityController.get())
                activityController.get().requestStart()
                awaitCaptureIdle(activityController.get())
                val start = shadowOf(context).nextStartedService!!
                assertEquals(CaptureService.ACTION_START, start.action)
                service.onStartCommand(start, 0, 1)
            } else service.onStartCommand(Intent(CaptureService.ACTION_START), 0, 1)
            preparation.scheduler.runCurrent()
            assertTrue("Controlled recovery must admit the production recorder before exit", secondRead.await(2, java.util.concurrent.TimeUnit.SECONDS))
            recordingThread = service.session.javaClass.getDeclaredField("captureThread")
                .apply { isAccessible = true }.get(service.session) as Thread
            val id = repository.active()!!.captureId
            if (!beforeIdentity)
                activityController = Robolectric.buildActivity(CaptureActivity::class.java, Intent(Intent.ACTION_VIEW)).create().start().resume()
            checkNotNull(activityController)
            val activity = activityController.get()
            if (beforeIdentity) assertNull(activity.presentation.snapshot.active) else awaitCaptureIdle(activity)
            if (repeatedBack) {
                activity.onBackPressedDispatcher.onBackPressed()
                activity.onBackPressedDispatcher.onBackPressed()
            }
            if (finish) activity.finish()
            activityController.pause().stop()
            val stops = mutableListOf<Intent>()
            while (true) {
                val command = shadowOf(context).nextStartedService ?: break
                if (command.action == CaptureService.ACTION_STOP) stops += command
            }
            val expectedReason = "manual_stop"
            assertEquals(if (repeatedBack) listOf("manual_stop", "manual_stop", "manual_stop") else listOf("manual_stop"),
                stops.map { it.getStringExtra(CaptureService.EXTRA_STOP_REASON) })
            stops.forEach { service.onStartCommand(it, 0, 2) }
            assertEquals(CaptureState.STOPPING, repository.get(id)!!.captureState)
            assertEquals(expectedReason, repository.get(id)!!.stopReason)
            assertNull(activity.presentation.savedFeedbackCaptureId)
            assertTrue(savedCaptureIds.isEmpty())
            assertNotNull(dev.gitblame.voxgolem.assistant.recognition.RecognitionResourceLease.process.current())
            releaseRead.countDown()
            recordingThread.join(5_000)
            assertFalse("The read and writer must actually retire after Stop", recordingThread.isAlive)
            val deadline = System.nanoTime() + java.util.concurrent.TimeUnit.SECONDS.toNanos(5)
            while (!savedCaptureIds.contains(id) || repository.get(id)!!.captureState != CaptureState.SAVED) {
                shadowOf(Looper.getMainLooper()).idle()
                assertTrue("Stop must commit SAVED and deliver its callback on the main looper", System.nanoTime() < deadline)
                Thread.yield()
            }
            shadowOf(Looper.getMainLooper()).idle()
            val saved = repository.get(id)!!
            val wav = java.io.File(saved.localAudioPath!!)
            assertEquals(CaptureState.SAVED, saved.captureState)
            assertEquals(expectedReason, saved.stopReason)
            assertArrayEquals(prefix + lateBuffer, wav.readBytes().drop(44).toByteArray())
            assertEquals(4L, saved.durableFrames)
            assertEquals(52L, wav.length())
            assertEquals(listOf(id), repository.list().map { it.captureId })
            assertEquals(1, java.io.File(context.filesDir, "captures").walkTopDown().count { it.isFile && it.extension == "wav" })
            assertEquals(listOf(true), savedProofs.toList())
            assertEquals(1, savedCaptureIds.size)
            assertEquals(id, savedCaptureIds.peek())
        } finally {
            releaseRead.countDown()
            service.onStartCommand(Intent(CaptureService.ACTION_STOP), 0, 3)
            recordingThread?.join(5_000)
            context.unregisterReceiver(savedReceiver)
            activityController?.destroy()
            serviceController.destroy()
            preparation.scheduler.runCurrent()
            shadowOf(Looper.getMainLooper()).idle()
            org.robolectric.shadows.ShadowAudioRecord.clearSource()
        }
    }
}
