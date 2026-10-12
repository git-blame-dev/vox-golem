package dev.gitblame.voxgolem.android.wear.capture

import android.Manifest
import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.content.Context
import android.content.Intent
import android.content.BroadcastReceiver
import android.content.IntentFilter
import android.media.AudioFormat
import android.media.AudioRecord
import android.media.MediaRecorder
import android.os.PowerManager
import android.os.SystemClock
import android.os.Vibrator
import android.os.Handler
import android.os.Looper
import androidx.core.content.ContextCompat
import dev.gitblame.voxgolem.capture.CaptureRecord
import dev.gitblame.voxgolem.capture.CaptureState
import dev.gitblame.voxgolem.assistant.recognition.RecognitionResourceLease
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
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicInteger
import java.util.concurrent.atomic.AtomicReference
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.test.StandardTestDispatcher

@OptIn(ExperimentalCoroutinesApi::class)
@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35])
class CaptureServiceSaveProofTest {
    private val context = RuntimeEnvironment.getApplication()
    private val watch = CaptureRepository(context)
    private val syntheticPcm = byteArrayOf(1, 0, 2, 0, 3, 0, 4, 0)

    private data class PrimedCapture(
        val service: CaptureService,
        val id: String,
        val recorder: AudioRecord,
        val wakeLock: PowerManager.WakeLock,
    )

    // Prime recording resources without microphone startup; the tests drive Stop and, when needed, the read loop.
    private fun primedCapture(): PrimedCapture {
        val service = Robolectric.buildService(CaptureService::class.java).create().get()
        assertTrue(service.session.prepareRecovery())
        val lease = RecognitionResourceLease.process.tryAcquire(RecognitionResourceLease.Kind.WATCH_NOTE_AUDIO)!!
        setField(service, "microphoneLease", lease)
        assertNull(RecognitionResourceLease.process.tryAcquire(RecognitionResourceLease.Kind.CHAT))
        val record = watch.create(16_000, UUID.randomUUID().toString()).copy(captureState = CaptureState.RECORDING)
        watch.save(record)
        service.session.bindCaptureLifetime(record.captureId)
        val writer = PcmWavWriter(watch.pcm(record.captureId), watch.audio(record.captureId), 16_000)
        writer.append(syntheticPcm)
        writer.checkpoint()
        val recorder = AudioRecord.Builder().setAudioSource(MediaRecorder.AudioSource.MIC)
            .setAudioFormat(AudioFormat.Builder().setSampleRate(16_000)
                .setEncoding(AudioFormat.ENCODING_PCM_16BIT).setChannelMask(AudioFormat.CHANNEL_IN_MONO).build())
            .setBufferSizeInBytes(3_200).build()
        recorder.startRecording()
        val wakeLock = (service.getSystemService(Context.POWER_SERVICE) as PowerManager)
            .newWakeLock(PowerManager.PARTIAL_WAKE_LOCK, "Synthetic:SaveProof").apply { acquire(10_000) }
        setField(service, "record", record)
        setField(service, "writer", writer)
        setField(service, "recorder", recorder)
        setField(service, "wakeLock", wakeLock)
        val manager = service.getSystemService(NotificationManager::class.java)
        manager.createNotificationChannel(NotificationChannel("synthetic-save-proof", "Synthetic save proof", NotificationManager.IMPORTANCE_LOW))
        service.startForeground(4101, Notification.Builder(service, "synthetic-save-proof")
            .setSmallIcon(android.R.drawable.ic_btn_speak_now).setContentTitle("Synthetic capture").build())
        return PrimedCapture(service, record.captureId, recorder, wakeLock)
    }

    @Test fun replacementAttachCannotRecoverOrDeleteAnOldStoppingWritersSpoolBeforeItsLateRead() {
        val capture = primedCapture()
        val readStarted = CountDownLatch(1)
        val releaseRead = CountDownLatch(1)
        val failure = AtomicReference<Throwable?>()
        ShadowAudioRecord.setSourceProvider {
            object : ShadowAudioRecord.AudioRecordSource {
                override fun readInByteArray(audioData: ByteArray, offset: Int, size: Int, blocking: Boolean): Int {
                    readStarted.countDown()
                    check(releaseRead.await(5, TimeUnit.SECONDS)) { "Synthetic old read was not released" }
                    syntheticPcm.copyInto(audioData, offset)
                    return syntheticPcm.size
                }
            }
        }
        val recording = field(capture.service, "record") as CaptureRecord
        val thread = Thread({
            runCatching {
                RecordingSession::class.java.getDeclaredMethod("recordLoop", CaptureRecord::class.java)
                    .apply { isAccessible = true }.invoke(capture.service.session, recording)
            }.onFailure(failure::set)
        }, "synthetic-old-service-read")
        setField(capture.service, "startedElapsed", SystemClock.elapsedRealtime())
        setField(capture.service, "captureThread", thread)
        val replacementController = Robolectric.buildService(CaptureService::class.java).create()
        val replacement = replacementController.get()
        val dispatcher = StandardTestDispatcher()
        replacement.preparationDispatcher = dispatcher
        try {
            thread.start()
            assertTrue(readStarted.await(5, TimeUnit.SECONDS))
            stop(capture)
            capture.service.onDestroy()
            assertNotNull(CaptureLifetime.current())
            replacement.onStartCommand(Intent(CaptureService.ACTION_ATTACH), 0, 1)
            dispatcher.scheduler.runCurrent()
            shadowOf(Looper.getMainLooper()).idle()
            assertEquals(CaptureState.STOPPING, watch.get(capture.id)!!.captureState)
            assertArrayEquals(syntheticPcm, watch.pcm(capture.id).readBytes())
            assertFalse(watch.audio(capture.id).exists())
            assertTrue(signals("recovered").isEmpty())
            assertTrue(signals("saved").isEmpty())
            assertFalse(watch.retryInterruptedFinalization(capture.id))

            releaseRead.countDown()
            thread.join(5_000)
            assertFalse(thread.isAlive)
            assertNull(failure.get())
            assertNull(CaptureLifetime.current())
            val saved = watch.get(capture.id)!!
            assertEquals(CaptureState.SAVED, saved.captureState)
            assertEquals(8L, saved.durableFrames)
            assertArrayEquals(syntheticPcm + syntheticPcm, watch.audio(capture.id).readBytes().drop(44).toByteArray())
            assertEquals(1, signals("saved").size)
            assertTrue(signals("ready").isEmpty())
            assertResourcesReleased(capture)
        } finally {
            releaseRead.countDown()
            if (thread.state != Thread.State.NEW) thread.join(5_000)
            replacementController.destroy()
            capture.service.onDestroy()
            ShadowAudioRecord.clearSource()
        }
    }

    private fun setField(service: CaptureService, name: String, value: Any?) {
        val owner: Any = if (name == "wakeLock") service else service.session
        owner.javaClass.getDeclaredField(name).apply { isAccessible = true }.set(owner, value)
    }

    private fun field(service: CaptureService, name: String): Any? {
        val owner: Any = if (name == "wakeLock") service else service.session
        return owner.javaClass.getDeclaredField(name).apply { isAccessible = true }.get(owner)
    }

    private fun stop(capture: PrimedCapture, reason: String = "manual_stop") {
        capture.service.onStartCommand(Intent(CaptureService.ACTION_STOP)
            .putExtra(CaptureService.EXTRA_STOP_REASON, reason), 0, 1)
    }

    private fun signals(state: String) = shadowOf(context).broadcastIntents.filter {
        it.action == CaptureSignals.SIGNAL_ACTION && it.getStringExtra("state") == state
    }

    private fun startAfterRecovery(service: CaptureService) {
        val dispatcher = StandardTestDispatcher()
        service.preparationDispatcher = dispatcher
        service.onStartCommand(Intent(CaptureService.ACTION_START), 0, 1)
        dispatcher.scheduler.runCurrent()
        shadowOf(Looper.getMainLooper()).idle()
    }

    private fun assertResourcesReleased(capture: PrimedCapture) {
        assertEquals(AudioRecord.STATE_UNINITIALIZED, capture.recorder.state)
        assertFalse(capture.wakeLock.isHeld)
        assertNull(field(capture.service, "writer"))
        assertNull(field(capture.service, "recorder"))
        assertNull(field(capture.service, "wakeLock"))
        assertNull(field(capture.service, "record"))
        assertNull(field(capture.service, "microphoneLease"))
        assertNull(RecognitionResourceLease.process.current())
        assertTrue(shadowOf(capture.service).isForegroundStopped)
        assertTrue(shadowOf(capture.service).isStoppedBySelf)
    }

    @Test fun assistantAdmissionRejectsNoteBeforeRecorderOrCaptureRecordAllocation() {
        shadowOf(context).grantPermissions(Manifest.permission.RECORD_AUDIO)
        val chat = RecognitionResourceLease.process.tryAcquire(RecognitionResourceLease.Kind.CHAT)!!
        val service = Robolectric.buildService(CaptureService::class.java).create().get()
        try {
            service.onStartCommand(Intent(CaptureService.ACTION_START), 0, 1)
            assertNull(field(service, "recorder")); assertNull(field(service, "record"))
            assertSame(chat, RecognitionResourceLease.process.current())
            assertTrue(shadowOf(service).isStoppedBySelf)
        } finally { service.onDestroy(); chat.close() }
    }

    @Test @Config(shadows = [RetirementFailureAudioRecordShadow::class])
    fun repeatedReleaseFailureRetainsExactNoteOwnerUntilLaterBoundedRetirementRetry() {
        RetirementFailureAudioRecordShadow.failRelease = true
        RetirementFailureAudioRecordShadow.released.clear()
        val capture = primedCapture()
        val owner = RecognitionResourceLease.process.current()!!
        try {
            stop(capture)
            capture.service.onDestroy()
            assertSame(capture.recorder, field(capture.service, "recorder"))
            assertSame(owner, RecognitionResourceLease.process.current())
            assertFalse(RecognitionResourceLease.process.retryFailedRetirement())
            assertNull(RecognitionResourceLease.process.tryAcquire(RecognitionResourceLease.Kind.CHAT))
            assertTrue(RetirementFailureAudioRecordShadow.released.all { it === capture.recorder })
            RetirementFailureAudioRecordShadow.failRelease = false
            assertTrue(RecognitionResourceLease.process.retryFailedRetirement())
            assertNull(field(capture.service, "recorder"))
            assertNull(field(capture.service, "microphoneLease"))
            val replacement = RecognitionResourceLease.process.tryAcquire(RecognitionResourceLease.Kind.CHAT)!!
            try {
                capture.service.onDestroy()
                assertSame(replacement, RecognitionResourceLease.process.current())
                assertFalse(RecognitionResourceLease.process.retryFailedRetirement())
            } finally { replacement.close() }
        } finally {
            RetirementFailureAudioRecordShadow.failRelease = false
            capture.service.onDestroy()
            RecognitionResourceLease.process.retryFailedRetirement()
        }
    }

    @Test @Config(shadows = [LateNotePermissionAudioRecordShadow::class, DeniedNoteAudioRecordBuilderShadow::class])
    fun permissionRevokedAfterWriterAndLiveSetupPreventsRecorderAllocationAndRetainsTheFailedSpool() {
        assertLatePermissionFailure(revokeBeforeAllocation = true)
    }

    @Test @Config(shadows = [LateNotePermissionAudioRecordShadow::class, DeniedNoteAudioRecordBuilderShadow::class])
    fun allocationTimeSecurityExceptionRetiresPreparedResourcesWithoutSavedOrReadyFeedback() {
        assertLatePermissionFailure(revokeBeforeAllocation = false)
    }

    private fun assertLatePermissionFailure(revokeBeforeAllocation: Boolean) {
        shadowOf(context).grantPermissions(Manifest.permission.RECORD_AUDIO)
        LateNotePermissionAudioRecordShadow.bufferRequests.set(0)
        DeniedNoteAudioRecordBuilderShadow.allocationAttempts.set(0)
        val controller = Robolectric.buildService(CaptureService::class.java).create()
        val service = controller.get()
        val observedStates = java.util.concurrent.ConcurrentLinkedQueue<String>()
        val receiver = object : BroadcastReceiver() {
            override fun onReceive(context: Context?, intent: Intent?) {
                intent?.getStringExtra("state")?.let(observedStates::add)
            }
        }
        ContextCompat.registerReceiver(context, receiver, IntentFilter(CaptureSignals.SIGNAL_ACTION),
            null, Handler(Looper.getMainLooper()), ContextCompat.RECEIVER_NOT_EXPORTED)
        val preparedSpool = AtomicReference<java.io.File>()
        LateNotePermissionAudioRecordShadow.afterLiveSetup = {
            val preparing = watch.records().single()
            assertEquals(CaptureState.PREPARING, preparing.captureState)
            assertNotNull(field(service, "writer"))
            assertNotNull(field(service, "liveTransport"))
            assertNotNull(field(service, "microphoneLease"))
            val spool = watch.pcm(preparing.captureId)
            assertTrue(spool.isFile)
            preparedSpool.set(spool)
            if (revokeBeforeAllocation) shadowOf(context).denyPermissions(Manifest.permission.RECORD_AUDIO)
        }
        try {
            startAfterRecovery(service)
            assertEquals(2, LateNotePermissionAudioRecordShadow.bufferRequests.get())
            assertEquals("Revocation detected before the SDK builder must prevent allocation", if (revokeBeforeAllocation) 0 else 1,
                DeniedNoteAudioRecordBuilderShadow.allocationAttempts.get())
            val failed = watch.records().single()
            assertEquals(CaptureState.FAILED, failed.captureState)
            assertEquals("microphone", failed.lastErrorStage)
            assertEquals("permission-revoked", failed.lastErrorCode)
            assertTrue(preparedSpool.get().isFile)
            assertEquals(0L, preparedSpool.get().length())
            assertEquals(preparedSpool.get().absolutePath, failed.localAudioPath)
            assertFalse(watch.audio(failed.captureId).exists())
            assertNull(failed.finalizedByteLength)
            assertNull(failed.sha256)
            assertEquals(listOf("failed"), observedStates.toList())
            for (name in listOf("writer", "liveTransport", "recorder", "captureThread", "microphoneLease", "record", "wakeLock")) {
                assertNull("Prepared resource $name must be retired", field(service, name))
            }
            assertNull(RecognitionResourceLease.process.current())
            assertTrue(service.session.retired)
            assertTrue(shadowOf(service).isForegroundStopped)
            assertTrue(shadowOf(service).isStoppedBySelf)
        } finally {
            context.unregisterReceiver(receiver)
            LateNotePermissionAudioRecordShadow.afterLiveSetup = {}
            controller.destroy()
        }
    }

    @Test fun duplicateStopDuringFirstReadRetainsPcmWithoutLateReadinessAndFinalizesOneDurableWav() {
        val capture = primedCapture()
        val readStarted = CountDownLatch(1)
        val completeRead = CountDownLatch(1)
        val requestedBytes = AtomicInteger()
        val threadFailure = AtomicReference<Throwable?>()
        val source = object : ShadowAudioRecord.AudioRecordSource {
            override fun readInByteArray(audioData: ByteArray, offset: Int, size: Int, blocking: Boolean): Int {
                requestedBytes.set(size)
                readStarted.countDown()
                check(completeRead.await(5, TimeUnit.SECONDS)) { "Synthetic read was not released" }
                syntheticPcm.copyInto(audioData, offset)
                return syntheticPcm.size
            }
        }
        ShadowAudioRecord.setSourceProvider { source }
        val recording = field(capture.service, "record") as CaptureRecord
        val readThread = Thread({
            runCatching {
                RecordingSession::class.java.getDeclaredMethod("recordLoop", CaptureRecord::class.java)
                    .apply { isAccessible = true }.invoke(capture.service.session, recording)
            }.onFailure(threadFailure::set)
        }, "synthetic-stop-read")
        setField(capture.service, "startedElapsed", SystemClock.elapsedRealtime())
        setField(capture.service, "captureThread", readThread)
        readThread.start()
        try {
            assertTrue("The first AudioRecord read must run before Stop", readStarted.await(5, TimeUnit.SECONDS))
            assertEquals("The first read requests 20 ms of mono PCM16", 640, requestedBytes.get())
            stop(capture)
            stop(capture, "service_destroyed")
            assertEquals(CaptureState.STOPPING, watch.get(capture.id)!!.captureState)
            assertEquals("manual_stop", watch.get(capture.id)!!.stopReason)
            assertTrue(signals("ready").isEmpty())
            assertNotNull("A read that has not retired must keep assistant admission blocked", RecognitionResourceLease.process.current())
            completeRead.countDown()
            readThread.join(5_000)
            assertFalse("Stop/read/finalize lock ordering must not deadlock", readThread.isAlive)
            assertNull(threadFailure.get())
            assertTrue("A read finishing after Stop must not issue microphone-ready feedback", signals("ready").isEmpty())
            assertEquals(false, field(capture.service, "microphoneReady"))
            val saved = CaptureRepository(context).get(capture.id)!!
            val wav = watch.audio(capture.id).readBytes()
            assertEquals(CaptureState.SAVED, saved.captureState)
            assertEquals("manual_stop", saved.stopReason)
            assertEquals(8L, saved.durableFrames)
            assertEquals(wav.size.toLong(), saved.finalizedByteLength)
            assertArrayEquals(syntheticPcm + syntheticPcm, wav.copyOfRange(44, wav.size))
            assertFalse(watch.pcm(capture.id).exists())
            assertNull(saved.finalText)
            assertFalse(saved.completionNotified)
            assertEquals(1, signals("saved").size)
            assertTrue(signals("completed").isEmpty())
            assertFalse(shadowOf(context.getSystemService(Vibrator::class.java)).isVibrating)
            stop(capture, "service_destroyed")
            assertEquals(saved, CaptureRepository(context).get(capture.id))
            assertArrayEquals(wav, watch.audio(capture.id).readBytes())
            assertEquals(1, signals("saved").size)
            assertResourcesReleased(capture)
            shadowOf(context).grantPermissions(Manifest.permission.RECORD_AUDIO)
            val capturesBeforeRetiredStart = watch.records()
            capture.service.onStartCommand(Intent(CaptureService.ACTION_START), 0, 2)
            assertEquals("A retiring service must not open another microphone with its Stop/finalize latches set", capturesBeforeRetiredStart, watch.records())
            assertResourcesReleased(capture)
        } finally {
            completeRead.countDown()
            readThread.join(5_000)
            ShadowAudioRecord.clearSource()
            capture.service.onDestroy()
        }
    }

    @Test fun permissionGrantedStartUsesProductionBuilderReadLoopAndStopToCreateRecoverableWav() {
        shadowOf(context).grantPermissions(Manifest.permission.RECORD_AUDIO)
        val secondRead = CountDownLatch(1)
        val releaseRead = CountDownLatch(1)
        val reads = AtomicInteger()
        val firstReadSize = AtomicInteger()
        ShadowAudioRecord.setSourceProvider {
            object : ShadowAudioRecord.AudioRecordSource {
                override fun readInByteArray(audioData: ByteArray, offset: Int, size: Int, blocking: Boolean): Int {
                    if (reads.incrementAndGet() == 1) {
                        firstReadSize.set(size)
                        syntheticPcm.copyInto(audioData, offset)
                        return syntheticPcm.size
                    }
                    secondRead.countDown()
                    check(releaseRead.await(5, TimeUnit.SECONDS)) { "Synthetic second read was not released" }
                    return 0
                }
            }
        }
        val controller = Robolectric.buildService(CaptureService::class.java).create()
        val service = controller.get()
        var thread: Thread? = null
        try {
            startAfterRecovery(service)
            assertTrue("Production microphone startup must reach its read loop", secondRead.await(5, TimeUnit.SECONDS))
            thread = field(service, "captureThread") as Thread
            val record = CaptureRepository(context).active()!!
            assertEquals(CaptureState.RECORDING, record.captureState)
            assertEquals(16_000, record.format.sampleRate)
            assertEquals(640, firstReadSize.get())
            assertEquals(1, signals("ready").size)
            assertEquals(record.captureId, signals("ready").single().getStringExtra("captureId"))
            assertEquals(true, field(service, "microphoneReady"))
            val recorder = field(service, "recorder") as AudioRecord
            val wakeLock = field(service, "wakeLock") as PowerManager.WakeLock
            assertTrue(wakeLock.isHeld)
            val primed = PrimedCapture(service, record.captureId, recorder, wakeLock)
            stop(primed, "provisional_motion_lowering")
            releaseRead.countDown()
            thread.join(5_000)
            assertFalse(thread.isAlive)
            val saved = CaptureRepository(context).get(record.captureId)!!
            assertEquals(CaptureState.SAVED, saved.captureState)
            assertEquals(4L, saved.durableFrames)
            assertEquals("provisional_motion_lowering", saved.stopReason)
            assertArrayEquals(syntheticPcm, watch.audio(record.captureId).readBytes().drop(44).toByteArray())
            assertEquals(1, signals("saved").size)
            assertTrue(signals("completed").isEmpty())
            assertResourcesReleased(primed)
        } finally {
            service.onStartCommand(Intent(CaptureService.ACTION_STOP), 0, 2)
            releaseRead.countDown()
            thread?.join(5_000)
            controller.destroy()
            ShadowAudioRecord.clearSource()
        }
    }

    @Test fun finalMetadataCommitFailureReleasesResourcesAndRetainsPcmAndWavForRecoveryWithoutSavedSignal() {
        val capture = primedCapture()
        val db = context.openOrCreateDatabase("captures.db", Context.MODE_PRIVATE, null)
        try {
            db.execSQL("CREATE TRIGGER synthetic_fail_saved BEFORE UPDATE ON captures " +
                "WHEN NEW.capture_state = 'SAVED' BEGIN SELECT RAISE(ABORT, 'synthetic metadata failure'); END")
            stop(capture)
            assertEquals(CaptureState.STOPPING, CaptureRepository(context).get(capture.id)!!.captureState)
            assertArrayEquals(syntheticPcm, watch.pcm(capture.id).readBytes())
            val wav = watch.audio(capture.id).readBytes()
            assertArrayEquals(syntheticPcm, wav.copyOfRange(44, wav.size))
            assertTrue(signals("saved").isEmpty())
            assertTrue(signals("completed").isEmpty())
            assertEquals("Recording retained · could not save capture status", signals("failed").single().getStringExtra("message"))
            assertArrayEquals(longArrayOf(0, 100, 100, 100, 100, 100), shadowOf(context.getSystemService(Vibrator::class.java)).pattern)
            assertResourcesReleased(capture)
            stop(capture)
            assertEquals(1, signals("failed").size)
            db.execSQL("DROP TRIGGER synthetic_fail_saved")
            CaptureRepository(context).recoverInterrupted()
            val recovered = CaptureRepository(context).get(capture.id)!!
            assertEquals(CaptureState.INTERRUPTED, recovered.captureState)
            assertEquals(watch.audio(capture.id).absolutePath, recovered.localAudioPath)
            assertEquals(4L, recovered.durableFrames)
            assertArrayEquals(wav, watch.audio(capture.id).readBytes())
            assertFalse(watch.pcm(capture.id).exists())
            assertFalse(recovered.completionNotified)
        } finally {
            db.execSQL("DROP TRIGGER IF EXISTS synthetic_fail_saved")
            db.close()
            capture.service.onDestroy()
        }
    }

    @Test fun audioReadErrorStopPreservesRecordedPrefixAsInterruptedWithoutFalseSaveOrCompletion() {
        val capture = primedCapture()
        try {
            stop(capture, "audio_read_error_-3")
            val interrupted = CaptureRepository(context).get(capture.id)!!
            assertEquals(CaptureState.INTERRUPTED, interrupted.captureState)
            assertEquals("audio_read_error_-3", interrupted.stopReason)
            val wav = watch.audio(capture.id).readBytes()
            assertArrayEquals(syntheticPcm, wav.copyOfRange(44, wav.size))
            assertEquals(4L, interrupted.durableFrames)
            assertTrue(signals("saved").isEmpty())
            assertTrue(signals("completed").isEmpty())
            assertEquals(1, signals("failed").size)
            assertResourcesReleased(capture)
        } finally {
            capture.service.onDestroy()
        }
    }

    @Test fun motionStopAndUiInterruptionUseDifferentCompletionStatesButBothRetainDurableWav() {
        for (reason in listOf("provisional_motion_lowering", "ui-interruption", "limit_reached")) {
            val capture = primedCapture()
            try {
                stop(capture, reason)
                val persisted = CaptureRepository(context).get(capture.id)!!
                assertEquals(if (reason == "provisional_motion_lowering") CaptureState.SAVED else CaptureState.INTERRUPTED,
                    persisted.captureState)
                assertEquals(reason, persisted.stopReason)
                assertEquals(4L, persisted.durableFrames)
                assertArrayEquals(syntheticPcm, watch.audio(capture.id).readBytes().drop(44).toByteArray())
                assertFalse(watch.pcm(capture.id).exists())
                assertTrue(watch.pendingTransfers().any { it.captureId == capture.id })
                assertFalse(persisted.completionNotified)
                if (reason == "limit_reached") {
                    assertEquals("capture-limit", persisted.lastErrorStage)
                    assertEquals("limit-reached-review", persisted.lastErrorCode)
                }
                assertResourcesReleased(capture)
            } finally { capture.service.onDestroy() }
        }
        assertEquals(1, signals("saved").size)
        assertEquals(2, signals("failed").size)
        assertTrue(signals("completed").isEmpty())
    }

    @Test fun wavDestinationConflictKeepsOriginalWavAndSpoolWithoutFalseSaveThenExplicitRepairWorks() {
        val capture = primedCapture()
        val conflicting = "synthetic invalid WAV retained for diagnosis".toByteArray()
        watch.audio(capture.id).writeBytes(conflicting)
        try {
            stop(capture)
            val interrupted = CaptureRepository(context).get(capture.id)!!
            assertEquals(CaptureState.INTERRUPTED, interrupted.captureState)
            assertEquals("capture-finalize", interrupted.lastErrorStage)
            assertEquals("wav-finalize-failed", interrupted.lastErrorCode)
            assertEquals(watch.pcm(capture.id).absolutePath, interrupted.localAudioPath)
            assertArrayEquals(syntheticPcm, watch.pcm(capture.id).readBytes())
            assertArrayEquals(conflicting, watch.audio(capture.id).readBytes())
            assertTrue(signals("saved").isEmpty())
            assertTrue(signals("completed").isEmpty())
            assertResourcesReleased(capture)
            assertTrue(CaptureRepository(context).retryInterruptedFinalization(capture.id))
            val repaired = watch.get(capture.id)!!
            assertEquals(CaptureState.INTERRUPTED, repaired.captureState)
            assertArrayEquals(syntheticPcm, watch.audio(capture.id).readBytes().drop(44).toByteArray())
            assertArrayEquals(conflicting, watch.audio(capture.id).parentFile!!.listFiles()!!
                .single { it.name.startsWith("audio-invalid-") }.readBytes())
            assertFalse(watch.claimCompletionFeedback(capture.id))
        } finally { capture.service.onDestroy() }
    }

    @Test fun privatePathBearingFinalizeExceptionNeverEntersCaptureStatusOrFailureSignal() {
        val capture = primedCapture()
        val original = field(capture.service, "writer") as PcmWavWriter
        original.close()
        val privateParent = java.io.File(context.cacheDir, "synthetic-person-private-output").apply { writeText("Synthetic non-directory") }
        val destination = java.io.File(privateParent, "private-note.wav")
        val replacement = PcmWavWriter(watch.pcm(capture.id), destination, 16_000)
        setField(capture.service, "writer", replacement)
        try {
            // FileNotFoundException includes the synthetic private path, unlike a fixed writer validation error.
            stop(capture)
            val interrupted = watch.get(capture.id)!!
            assertEquals("wav-finalize-failed", interrupted.lastErrorCode)
            assertFalse(interrupted.lastErrorCode!!.contains("synthetic-person"))
            assertFalse(signals("failed").single().getStringExtra("message")!!.contains("synthetic-person"))
            assertTrue(watch.pcm(capture.id).isFile)
            assertTrue(signals("saved").isEmpty())
            assertResourcesReleased(capture)
        } finally { capture.service.onDestroy() }
    }
}

@org.robolectric.annotation.Implements(AudioRecord::class)
class LateNotePermissionAudioRecordShadow {
    companion object {
        val bufferRequests = AtomicInteger()
        var afterLiveSetup: () -> Unit = {}

        @JvmStatic @org.robolectric.annotation.Implementation
        fun native_get_min_buff_size(sampleRate: Int, channelCount: Int, audioFormat: Int): Int {
            assertEquals(16_000, sampleRate)
            assertEquals(1, channelCount)
            assertEquals(AudioFormat.ENCODING_PCM_16BIT, audioFormat)
            if (bufferRequests.incrementAndGet() == 2) afterLiveSetup()
            return 3_200
        }
    }
}

@org.robolectric.annotation.Implements(AudioRecord.Builder::class)
class DeniedNoteAudioRecordBuilderShadow {
    @org.robolectric.annotation.Implementation
    fun build(): AudioRecord {
        allocationAttempts.incrementAndGet()
        shadowOf(RuntimeEnvironment.getApplication()).denyPermissions(Manifest.permission.RECORD_AUDIO)
        throw SecurityException("Synthetic microphone permission revoked at allocation")
    }

    companion object { val allocationAttempts = AtomicInteger() }
}

@org.robolectric.annotation.Implements(AudioRecord::class)
class RetirementFailureAudioRecordShadow {
    @org.robolectric.annotation.RealObject private lateinit var real: AudioRecord
    @org.robolectric.annotation.Implementation
    fun release() {
        released += real
        if (failRelease) throw IllegalStateException("Synthetic private /storage/synthetic-user/voice.wav")
        org.robolectric.shadow.api.Shadow.directlyOn<Void, AudioRecord>(real, AudioRecord::class.java, "release")
    }
    companion object {
        var failRelease = true
        val released = mutableListOf<AudioRecord>()
    }
}
