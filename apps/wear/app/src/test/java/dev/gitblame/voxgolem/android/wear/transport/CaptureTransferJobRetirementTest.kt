package dev.gitblame.voxgolem.android.wear.transport

import android.app.job.JobParameters
import android.os.Looper
import com.google.android.gms.tasks.TaskCompletionSource
import com.google.android.gms.wearable.CapabilityInfo
import dev.gitblame.voxgolem.capture.CaptureRecord
import dev.gitblame.voxgolem.capture.CaptureRepository
import dev.gitblame.voxgolem.capture.CaptureState
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.Robolectric
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment
import org.robolectric.shadow.api.Shadow
import org.robolectric.Shadows.shadowOf
import org.robolectric.annotation.Config
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicInteger

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35])
class CaptureTransferJobRetirementTest {
    @Test fun separatelyUnmarshalledStopParametersRetireTheSameSchedulerJobsArchiveImmediately() {
        val context = RuntimeEnvironment.getApplication()
        val id = "123e4567-e89b-42d3-a456-426614174083"
        val watch = dev.gitblame.voxgolem.android.wear.capture.CaptureRepository(context)
        val wav = watch.audio(id)
        wav.parentFile!!.mkdirs()
        val retained = ByteArray(44) { 1 }
        wav.writeBytes(retained)
        CaptureRepository(context).save(CaptureRecord(id, createdAtUtc = "2026-10-06T00:00:00Z", captureState = CaptureState.SAVED,
            finalizedByteLength = 44, localAudioPath = wav.absolutePath, sha256 = "a".repeat(64)))
        val sdk = FakeCaptureWearSdk()
        val writing = CountDownLatch(1)
        val unblock = CountDownLatch(1)
        sdk.outputTask = com.google.android.gms.tasks.Tasks.forResult(object : java.io.OutputStream() {
            override fun write(value: Int) { writing.countDown(); check(unblock.await(5, TimeUnit.SECONDS)); error("Synthetic retired archive") }
        })
        sdk.onChannelClose = { unblock.countDown() }
        val controller = Robolectric.buildService(CaptureTransferJob::class.java).create()
        val service = controller.get()
        service.transportFactory = { WearCaptureTransport(context, sdk) }
        val finished = AtomicInteger()
        service.finishJob = { finished.incrementAndGet() }
        val parameters = parameters(0x5648)
        val stopParameters = parameters(0x5648)
        assertNotSame(parameters, stopParameters)
        assertEquals(parameters.jobId, stopParameters.jobId)
        try {
            service.onStartJob(parameters)
            assertTrue(writing.await(2, TimeUnit.SECONDS))
            assertTrue(service.onStopJob(stopParameters))
            assertEquals("Stop must retire resources immediately despite a different Binder parameter object", 0L, sdk.closed.count)
            assertTrue(sdk.closed.await(2, TimeUnit.SECONDS))
            shadowOf(Looper.getMainLooper()).idle()
            assertEquals(0, finished.get())
            assertArrayEquals(retained, wav.readBytes())
            assertEquals(1, watch.get(id)!!.retryCount)
            assertEquals(dev.gitblame.voxgolem.capture.TransferState.SENDING, watch.get(id)!!.transferState)
        } finally { unblock.countDown(); controller.destroy() }
    }

    @Test fun stoppedParametersCannotFinishOrCancelTheirReplacementAndOnlyReplacementCompletes() {
        val context = RuntimeEnvironment.getApplication()
        val id = "123e4567-e89b-42d3-a456-426614174082"
        CaptureRepository(context).save(CaptureRecord(id, createdAtUtc = "2026-10-06T00:00:00Z", captureState = CaptureState.SAVED,
            finalizedByteLength = 44, localAudioPath = "/synthetic/audio.wav", sha256 = "a".repeat(64)))
        val sdk = FakeCaptureWearSdk()
        val oldLookup = TaskCompletionSource<CapabilityInfo>()
        val newLookup = TaskCompletionSource<CapabilityInfo>()
        val oldEntered = CountDownLatch(1)
        val newEntered = CountDownLatch(1)
        val calls = AtomicInteger()
        sdk.phoneTask = {
            if (calls.incrementAndGet() == 1) { oldEntered.countDown(); oldLookup.task }
            else { newEntered.countDown(); newLookup.task }
        }
        val controller = Robolectric.buildService(CaptureTransferJob::class.java).create()
        val service = controller.get()
        service.transportFactory = { WearCaptureTransport(context, sdk) }
        val finished = mutableListOf<JobParameters>()
        service.finishJob = { finished += it }
        val old = parameters(901)
        val replacement = parameters(902)
        try {
            assertTrue(service.onStartJob(old)); assertTrue(oldEntered.await(2, TimeUnit.SECONDS))
            // The first attempt persisted backoff before lookup. Make the replacement due independently.
            CaptureRepository(context).update(id) { it.copy(nextRetryAtUtc = null) }
            assertTrue(service.onStartJob(replacement)); assertTrue(newEntered.await(2, TimeUnit.SECONDS))
            assertTrue(service.onStopJob(old))
            shadowOf(Looper.getMainLooper()).idle()
            assertTrue(finished.isEmpty())
            newLookup.setResult(sdk.capability)
            val deadline = System.nanoTime() + TimeUnit.SECONDS.toNanos(5)
            while (finished.isEmpty()) {
                shadowOf(Looper.getMainLooper()).idle()
                check(System.nanoTime() < deadline); Thread.sleep(1)
            }
            assertSame(replacement, finished.single())
            oldLookup.setResult(sdk.capability)
            shadowOf(Looper.getMainLooper()).idle()
            assertEquals(1, finished.size)
        } finally { controller.destroy() }
    }

    @Test fun cancelledRunCannotFinishOrRescheduleWhenItsJobIdIsReusedByAReplacementRun() {
        val context = RuntimeEnvironment.getApplication()
        val id = "123e4567-e89b-42d3-a456-426614174084"
        CaptureRepository(context).save(CaptureRecord(id, createdAtUtc = "2026-10-06T00:00:00Z", captureState = CaptureState.SAVED,
            finalizedByteLength = 44, localAudioPath = "/synthetic/audio.wav", sha256 = "a".repeat(64)))
        val sdk = FakeCaptureWearSdk()
        val oldLookup = TaskCompletionSource<CapabilityInfo>()
        val newLookup = TaskCompletionSource<CapabilityInfo>()
        val oldEntered = CountDownLatch(1)
        val newEntered = CountDownLatch(1)
        val calls = AtomicInteger()
        sdk.phoneTask = {
            if (calls.incrementAndGet() == 1) { oldEntered.countDown(); oldLookup.task }
            else { newEntered.countDown(); newLookup.task }
        }
        val controller = Robolectric.buildService(CaptureTransferJob::class.java).create()
        val service = controller.get()
        service.transportFactory = { WearCaptureTransport(context, sdk) }
        val finished = mutableListOf<JobParameters>()
        service.finishJob = { finished += it }
        val started = parameters(0x5648)
        val stopped = parameters(0x5648)
        val replacement = parameters(0x5648)
        try {
            service.onStartJob(started); assertTrue(oldEntered.await(2, TimeUnit.SECONDS))
            service.onStopJob(stopped)
            CaptureRepository(context).update(id) { it.copy(nextRetryAtUtc = null) }
            service.onStartJob(replacement); assertTrue(newEntered.await(2, TimeUnit.SECONDS))
            oldLookup.setResult(sdk.capability)
            shadowOf(Looper.getMainLooper()).idle()
            assertTrue("The old Run cannot finish the replacement sharing its job ID", finished.isEmpty())
            assertTrue(context.getSystemService(android.app.job.JobScheduler::class.java).allPendingJobs.isEmpty())
            service.onStopJob(parameters(999))
            newLookup.setResult(sdk.capability)
            val deadline = System.nanoTime() + TimeUnit.SECONDS.toNanos(5)
            while (finished.isEmpty()) {
                shadowOf(Looper.getMainLooper()).idle()
                check(System.nanoTime() < deadline); Thread.sleep(1)
            }
            assertSame(replacement, finished.single())
        } finally { controller.destroy() }
    }

    private fun parameters(jobId: Int): JobParameters = Shadow.newInstanceOf(JobParameters::class.java).also {
        org.robolectric.util.ReflectionHelpers.setField(it, "jobId", jobId)
    }
}
