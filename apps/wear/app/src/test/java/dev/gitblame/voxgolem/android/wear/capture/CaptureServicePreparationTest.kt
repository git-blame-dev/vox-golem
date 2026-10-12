package dev.gitblame.voxgolem.android.wear.capture

import android.Manifest
import android.content.Context
import android.content.Intent
import android.os.Looper
import dev.gitblame.voxgolem.assistant.recognition.RecognitionResourceLease
import dev.gitblame.voxgolem.capture.CaptureState
import dev.gitblame.voxgolem.capture.LiveTranscriptionState
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.test.StandardTestDispatcher
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.Robolectric
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment
import org.robolectric.Shadows.shadowOf
import org.robolectric.annotation.Config
import org.robolectric.shadows.ShadowAudioRecord
import java.util.UUID
import java.util.concurrent.atomic.AtomicReference
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit

@OptIn(ExperimentalCoroutinesApi::class)
@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35])
class CaptureServicePreparationTest {
    private val context = RuntimeEnvironment.getApplication()
    private val repository = CaptureRepository(context)

    @Test fun stopAndDestroyInsideSynchronousRecoveryKeepReplacementAdmissionBlockedUntilWorkerExit() {
        shadowOf(context).grantPermissions(Manifest.permission.RECORD_AUDIO)
        val abandoned = repository.create(16_000, UUID.randomUUID().toString())
        val pcm = byteArrayOf(1, 0, 2, 0)
        repository.pcm(abandoned.captureId).writeBytes(pcm)
        val oldController = Robolectric.buildService(CaptureService::class.java).create()
        val old = oldController.get()
        val dispatcher = StandardTestDispatcher()
        old.preparationDispatcher = dispatcher
        val enteredRecovery = CountDownLatch(1)
        val finishRecovery = CountDownLatch(1)
        val workerFailure = AtomicReference<Throwable?>()
        val oldRepository = CaptureService::class.java.getDeclaredField("repository")
            .apply { isAccessible = true }.get(old) as CaptureRepository
        oldRepository.beforeAdmissionRecovery = {
            enteredRecovery.countDown()
            check(finishRecovery.await(5, TimeUnit.SECONDS)) { "Synthetic recovery was not released" }
        }
        val replacementController = Robolectric.buildService(CaptureService::class.java).create()
        val replacement = replacementController.get()
        replacement.preparationDispatcher = StandardTestDispatcher()
        val worker = Thread({ runCatching { dispatcher.scheduler.runCurrent() }.onFailure(workerFailure::set) },
            "synthetic-blocked-active-recovery")
        val nextController = Robolectric.buildService(CaptureService::class.java).create()
        val next = nextController.get()
        val nextDispatcher = StandardTestDispatcher()
        next.preparationDispatcher = nextDispatcher
        val nextRead = CountDownLatch(1)
        val releaseNextRead = CountDownLatch(1)
        var nextThread: Thread? = null
        try {
            old.onStartCommand(Intent(CaptureService.ACTION_START), 0, 1)
            worker.start()
            assertTrue(enteredRecovery.await(5, TimeUnit.SECONDS))
            old.onStartCommand(Intent(CaptureService.ACTION_STOP), 0, 2)
            oldController.destroy()
            assertNull("No microphone exists during blocked recovery", RecognitionResourceLease.process.current())
            assertNotNull("Actual worker exit, not cancellation, must release capture admission", CaptureLifetime.current())
            replacement.onStartCommand(Intent(CaptureService.ACTION_START), 0, 1)
            assertTrue(replacement.session.retired)
            assertNull(replacement.session.active)
            assertEquals(listOf(abandoned), repository.records())
            assertArrayEquals(pcm, repository.pcm(abandoned.captureId).readBytes())

            finishRecovery.countDown()
            worker.join(5_000)
            assertFalse(worker.isAlive)
            assertNull(workerFailure.get())
            shadowOf(Looper.getMainLooper()).idle()
            assertNull(CaptureLifetime.current())
            assertEquals(CaptureState.INTERRUPTED, repository.get(abandoned.captureId)!!.captureState)
            assertArrayEquals(pcm, repository.pcm(abandoned.captureId).readBytes())
            assertTrue(shadowOf(context).broadcastIntents.none { it.getStringExtra("state") == "ready" })

            // A later, explicit Start can reach the real builder/read loop only after actual retirement.
            ShadowAudioRecord.setSourceProvider {
                object : ShadowAudioRecord.AudioRecordSource {
                    override fun readInByteArray(audioData: ByteArray, offset: Int, size: Int, blocking: Boolean): Int {
                        nextRead.countDown()
                        check(releaseNextRead.await(5, TimeUnit.SECONDS)) { "Synthetic replacement read was not released" }
                        pcm.copyInto(audioData, offset)
                        return pcm.size
                    }
                }
            }
            next.onStartCommand(Intent(CaptureService.ACTION_START), 0, 1)
            nextDispatcher.scheduler.runCurrent()
            shadowOf(Looper.getMainLooper()).idle()
            nextThread = RecordingSession::class.java.getDeclaredField("captureThread")
                .apply { isAccessible = true }.get(next.session) as Thread
            assertTrue(nextRead.await(5, TimeUnit.SECONDS))
            val created = next.session.active!!
            assertEquals(CaptureState.RECORDING, created.captureState)
            assertNotEquals(abandoned.captureId, created.captureId)
            val before = repository.pcm(created.captureId).readBytes()
            dispatcher.scheduler.runCurrent()
            shadowOf(Looper.getMainLooper()).idle()
            val observed = repository.get(created.captureId)!!
            // Host live transport may report unavailability; recovery must not mutate capture/audio ownership.
            assertTrue(observed.liveTranscriptionState in setOf(LiveTranscriptionState.CONNECTING, LiveTranscriptionState.UNAVAILABLE))
            if (observed.liveTranscriptionState == LiveTranscriptionState.UNAVAILABLE) assertEquals("live-stream-failed", observed.liveErrorCode)
            assertEquals(created.copy(liveTranscriptionState = observed.liveTranscriptionState,
                liveErrorCode = observed.liveErrorCode), observed)
            assertArrayEquals(before, repository.pcm(created.captureId).readBytes())
        } finally {
            finishRecovery.countDown()
            if (worker.state != Thread.State.NEW) worker.join(5_000)
            oldController.destroy()
            replacementController.destroy()
            next.onStartCommand(Intent(CaptureService.ACTION_STOP), 0, 2)
            releaseNextRead.countDown()
            nextThread?.join(5_000)
            nextController.destroy()
            ShadowAudioRecord.clearSource()
            shadowOf(Looper.getMainLooper()).idle()
        }
    }

    @Test fun creationDoesNotReadOrRecoverCaptureStorageOnTheMainThread() {
        val abandoned = repository.create(16_000, UUID.randomUUID().toString())
        val controller = Robolectric.buildService(CaptureService::class.java).create()
        try {
            assertEquals(CaptureState.PREPARING, repository.get(abandoned.captureId)!!.captureState)
            assertFalse(shadowOf(controller.get()).isStoppedBySelf)
        } finally { controller.destroy() }
    }

    @Test fun startIsForegroundAndSingleOwnedPreparationBeforeRecoveryAndStopPreventsAdmission() {
        shadowOf(context).grantPermissions(Manifest.permission.RECORD_AUDIO)
        val abandoned = repository.create(16_000, UUID.randomUUID().toString())
        val controller = Robolectric.buildService(CaptureService::class.java).create()
        val service = controller.get()
        val dispatcher = StandardTestDispatcher()
        service.preparationDispatcher = dispatcher
        try {
            service.onStartCommand(Intent(CaptureService.ACTION_START), 0, 1)
            val owner = RecognitionResourceLease.process.current()!!
            assertEquals(RecognitionResourceLease.Kind.WATCH_NOTE_AUDIO, owner.kind)
            assertTrue(service.session.preparing)
            assertNotNull(shadowOf(service).lastForegroundNotification)
            assertEquals(CaptureState.PREPARING, repository.get(abandoned.captureId)!!.captureState)
            service.onStartCommand(Intent(CaptureService.ACTION_START), 0, 2)
            service.onStartCommand(Intent(CaptureService.ACTION_ATTACH), 0, 3)
            assertSame(owner, RecognitionResourceLease.process.current())
            assertFalse(shadowOf(service).isStoppedBySelf)
            service.onStartCommand(Intent(CaptureService.ACTION_STOP), 0, 4)
            dispatcher.scheduler.runCurrent()
            shadowOf(Looper.getMainLooper()).idle()
            assertRetiredWithoutMicrophone(service)
            assertEquals(listOf(abandoned), repository.records())
        } finally { controller.destroy() }
    }

    @Test fun destructionWhilePreparationIsQueuedFencesLateStartAndRetiresItsLease() {
        shadowOf(context).grantPermissions(Manifest.permission.RECORD_AUDIO)
        val controller = Robolectric.buildService(CaptureService::class.java).create()
        val service = controller.get()
        val dispatcher = StandardTestDispatcher()
        service.preparationDispatcher = dispatcher
        service.onStartCommand(Intent(CaptureService.ACTION_START), 0, 1)
        controller.destroy()
        dispatcher.scheduler.runCurrent()
        shadowOf(Looper.getMainLooper()).idle()
        assertRetiredWithoutMicrophone(service)
        assertTrue(repository.records().isEmpty())
    }

    @Test fun stopAfterWorkerRecoveryButBeforeItsMainContinuationCannotAllocateTheRecorder() {
        shadowOf(context).grantPermissions(Manifest.permission.RECORD_AUDIO)
        val abandoned = repository.create(16_000, UUID.randomUUID().toString())
        val pcm = byteArrayOf(1, 0, 2, 0)
        repository.pcm(abandoned.captureId).writeBytes(pcm)
        val controller = Robolectric.buildService(CaptureService::class.java).create()
        val service = controller.get()
        val dispatcher = StandardTestDispatcher()
        service.preparationDispatcher = dispatcher
        val failure = AtomicReference<Throwable?>()
        try {
            service.onStartCommand(Intent(CaptureService.ACTION_START), 0, 1)
            val worker = Thread({ runCatching { dispatcher.scheduler.runCurrent() }.onFailure(failure::set) },
                "synthetic-admission-recovery")
            worker.start()
            worker.join(5_000)
            assertFalse(worker.isAlive)
            assertNull(failure.get())
            assertEquals(CaptureState.INTERRUPTED, repository.get(abandoned.captureId)!!.captureState)
            assertNull(service.session.active)
            service.onStartCommand(Intent(CaptureService.ACTION_STOP), 0, 2)
            shadowOf(Looper.getMainLooper()).idle()
            dispatcher.scheduler.runCurrent()
            assertRetiredWithoutMicrophone(service)
            assertEquals(1, repository.records().size)
            assertArrayEquals(pcm, repository.pcm(abandoned.captureId).readBytes())
            assertFalse(repository.audio(abandoned.captureId).exists())
        } finally { controller.destroy() }
    }

    @Test fun failedRequiredRecoveryRetainsAbandonedSpoolAndCannotAllocateAnotherCapture() {
        shadowOf(context).grantPermissions(Manifest.permission.RECORD_AUDIO)
        val abandoned = repository.create(16_000, UUID.randomUUID().toString())
        val pcm = byteArrayOf(1, 0, 2, 0)
        repository.pcm(abandoned.captureId).writeBytes(pcm)
        val db = context.openOrCreateDatabase("captures.db", Context.MODE_PRIVATE, null)
        db.execSQL("CREATE TRIGGER synthetic_recovery_failure BEFORE UPDATE ON captures " +
            "WHEN NEW.capture_state = 'INTERRUPTED' BEGIN SELECT RAISE(ABORT, 'synthetic recovery failure'); END")
        val controller = Robolectric.buildService(CaptureService::class.java).create()
        val service = controller.get()
        val dispatcher = StandardTestDispatcher()
        service.preparationDispatcher = dispatcher
        try {
            service.onStartCommand(Intent(CaptureService.ACTION_START), 0, 1)
            dispatcher.scheduler.runCurrent()
            shadowOf(Looper.getMainLooper()).idle()
            assertRetiredWithoutMicrophone(service)
            assertEquals(listOf(abandoned), repository.records())
            assertArrayEquals(pcm, repository.pcm(abandoned.captureId).readBytes())
            assertFalse(repository.audio(abandoned.captureId).exists())
            val failed = shadowOf(context).broadcastIntents.single { it.getStringExtra("state") == "failed" }
            assertEquals("Capture recovery needs attention · audio retained on watch", failed.getStringExtra("message"))
        } finally {
            db.execSQL("DROP TRIGGER synthetic_recovery_failure")
            db.close()
            controller.destroy()
        }
    }

    @Test fun permissionRevokedDuringRecoveryPreventsMetadataAndRecorderAllocation() {
        shadowOf(context).grantPermissions(Manifest.permission.RECORD_AUDIO)
        val controller = Robolectric.buildService(CaptureService::class.java).create()
        val service = controller.get()
        val dispatcher = StandardTestDispatcher()
        service.preparationDispatcher = dispatcher
        try {
            service.onStartCommand(Intent(CaptureService.ACTION_START), 0, 1)
            shadowOf(context).denyPermissions(Manifest.permission.RECORD_AUDIO)
            dispatcher.scheduler.runCurrent()
            shadowOf(Looper.getMainLooper()).idle()
            assertRetiredWithoutMicrophone(service)
            assertTrue(repository.records().isEmpty())
        } finally { controller.destroy() }
    }

    @Test fun unresolvedRetirementBlocksRecoveryAndNewRecorderWithoutReleasingThePreviousOwner() {
        shadowOf(context).grantPermissions(Manifest.permission.RECORD_AUDIO)
        val abandoned = repository.create(16_000, UUID.randomUUID().toString())
        val previous = RecognitionResourceLease.process.tryAcquire(RecognitionResourceLease.Kind.WATCH_NOTE_AUDIO)!!
        previous.onRetirementRetry { false }
        val controller = Robolectric.buildService(CaptureService::class.java).create()
        val service = controller.get()
        val dispatcher = StandardTestDispatcher()
        service.preparationDispatcher = dispatcher
        try {
            service.onStartCommand(Intent(CaptureService.ACTION_START), 0, 1)
            dispatcher.scheduler.runCurrent()
            shadowOf(Looper.getMainLooper()).idle()
            assertSame(previous, RecognitionResourceLease.process.current())
            assertTrue(service.session.retired)
            assertNull(service.session.active)
            assertEquals(listOf(abandoned), repository.records())
            assertTrue(shadowOf(service).isForegroundStopped)
            assertTrue(shadowOf(context).broadcastIntents.none { it.getStringExtra("state") == "ready" })
        } finally {
            controller.destroy()
            assertSame(previous, RecognitionResourceLease.process.current())
            previous.close()
        }
    }

    @Test fun attachOnlyRecoversDurableAudioWithoutStartingAMicrophoneOrIssuingSaveFeedback() {
        val abandoned = repository.create(16_000, UUID.randomUUID().toString())
        val pcm = byteArrayOf(1, 0, 2, 0)
        repository.pcm(abandoned.captureId).writeBytes(pcm)
        val controller = Robolectric.buildService(CaptureService::class.java).create()
        val service = controller.get()
        val dispatcher = StandardTestDispatcher()
        service.preparationDispatcher = dispatcher
        try {
            service.onStartCommand(Intent(CaptureService.ACTION_ATTACH), 0, 1)
            dispatcher.scheduler.runCurrent()
            shadowOf(Looper.getMainLooper()).idle()
            dispatcher.scheduler.runCurrent()
            shadowOf(Looper.getMainLooper()).idle()
            val recovered = repository.get(abandoned.captureId)!!
            assertEquals(CaptureState.INTERRUPTED, recovered.captureState)
            assertEquals(2L, recovered.durableFrames)
            assertEquals(48L, recovered.finalizedByteLength)
            assertArrayEquals(pcm, repository.audio(abandoned.captureId).readBytes().drop(44).toByteArray())
            assertRetiredWithoutMicrophone(service)
            assertTrue(shadowOf(service).isStoppedBySelf)
            assertEquals(1, shadowOf(context).broadcastIntents.count { it.getStringExtra("state") == "recovered" })
        } finally { controller.destroy() }
    }

    private fun assertRetiredWithoutMicrophone(service: CaptureService) {
        assertTrue(service.session.retired)
        assertFalse(service.session.preparing)
        assertNull(service.session.active)
        assertNull(RecognitionResourceLease.process.current())
        assertNull(CaptureLifetime.current())
        for (name in listOf("recorder", "writer", "captureThread", "microphoneLease")) {
            assertNull(name, RecordingSession::class.java.getDeclaredField(name)
                .apply { isAccessible = true }.get(service.session))
        }
        assertTrue(shadowOf(service).isForegroundStopped)
        assertTrue(shadowOf(context).broadcastIntents.none { it.getStringExtra("state") in listOf("ready", "saved", "completed") })
    }
}
