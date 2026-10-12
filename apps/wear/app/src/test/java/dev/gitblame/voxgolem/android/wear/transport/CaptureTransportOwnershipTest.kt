package dev.gitblame.voxgolem.android.wear.transport

import android.os.Parcel
import com.google.android.gms.tasks.Task
import com.google.android.gms.tasks.TaskCompletionSource
import com.google.android.gms.tasks.Tasks
import com.google.android.gms.wearable.CapabilityInfo
import com.google.android.gms.wearable.ChannelClient
import com.google.android.gms.wearable.Node
import dev.gitblame.voxgolem.capture.CaptureProtocol
import dev.gitblame.voxgolem.capture.CaptureRecord
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import java.io.ByteArrayOutputStream
import java.io.DataInputStream
import java.io.OutputStream
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicInteger

internal class FakeCaptureWearSdk : CaptureWearSdk {
    val channel = object : ChannelClient.Channel {
        override fun getNodeId() = "synthetic-phone"
        override fun getPath() = "/synthetic"
        override fun describeContents() = 0
        override fun writeToParcel(parcel: Parcel, flags: Int) = Unit
    }
    val node = object : Node {
        override fun getId() = "synthetic-phone"
        override fun getDisplayName() = "Synthetic receiver"
        override fun isNearby() = true
    }
    val capability = object : CapabilityInfo {
        override fun getName() = CaptureProtocol.PHONE_CAPABILITY
        override fun getNodes() = setOf(node)
    }
    var phoneTask: () -> Task<CapabilityInfo> = { Tasks.forResult(capability) }
    var openTask: Task<ChannelClient.Channel> = Tasks.forResult(channel)
    var outputTask: Task<OutputStream> = Tasks.forResult(ByteArrayOutputStream())
    val opened = CountDownLatch(1)
    val outputRequested = CountDownLatch(1)
    val closed = CountDownLatch(1)
    val stops = CountDownLatch(1)
    val closeCount = AtomicInteger()
    val sent = java.util.Collections.synchronizedList(mutableListOf<String>())
    var onChannelClose: () -> Unit = {}
    override fun phones() = phoneTask()
    override fun open(nodeId: String, path: String): Task<ChannelClient.Channel> { opened.countDown(); return openTask }
    override fun output(channel: ChannelClient.Channel): Task<OutputStream> { outputRequested.countDown(); return outputTask }
    override fun close(channel: ChannelClient.Channel): Task<Void> {
        closeCount.incrementAndGet(); onChannelClose(); closed.countDown(); return Tasks.forResult(null)
    }
    override fun send(nodeId: String, path: String, bytes: ByteArray): Task<Int> {
        sent += path
        if (path == CaptureProtocol.STOP) stops.countDown()
        return Tasks.forResult(1)
    }
}

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35])
class CaptureTransportOwnershipTest {
    private val record = CaptureRecord("123e4567-e89b-42d3-a456-426614174080", createdAtUtc = "2026-10-06T00:00:00Z",
        recognitionSessionId = "123e4567-e89b-42d3-a456-426614174081")

    @Test fun nonNoteClipsCannotOpenLegacyNoteChannels() {
        val sdk = FakeCaptureWearSdk()
        for (purpose in dev.gitblame.voxgolem.capture.CapturePurpose.entries.filter { it != dev.gitblame.voxgolem.capture.CapturePurpose.NOTE }) {
            val clip = record.copy(purpose = purpose)
            val live = LiveCapturePipe(clip, sdk)
            assertThrows(IllegalArgumentException::class.java) { live.start() }
            live.abort()
            val archive = ArchiveTransfer(sdk)
            assertThrows(IllegalArgumentException::class.java) { archive.transfer("synthetic-phone", clip, java.io.File("/synthetic/not-opened.wav")) }
            archive.close()
        }
        assertTrue(sdk.sent.isEmpty())
        assertEquals(1L, sdk.opened.count)
    }

    @Test fun overflowStatusFailureCannotThrowIntoTheRecordingProducer() {
        val sdk = FakeCaptureWearSdk()
        val failures = mutableListOf<String>()
        val live = LiveCapturePipe(record, sdk, onUnavailable = { failures += it; error("Synthetic status persistence failure") })
        repeat(21) { live.offer(byteArrayOf(1, 0)) }
        assertEquals(listOf("live-queue-overflow"), failures)
        assertTrue(sdk.sent.isEmpty())
        live.abort()
    }

    @Test fun normalFinishDrainsQueuedFramesThenWritesEofBeforeStopAndOnlyRetiresItsOwnChannel() {
        val sdk = FakeCaptureWearSdk()
        val output = ByteArrayOutputStream()
        sdk.outputTask = Tasks.forResult(output)
        val handle = LiveCapturePipe(record.copy(requestedLanguage = "en-AU", speechNetworkFallback = true), sdk)
        handle.offer(byteArrayOf(1, 0, 2, 0)); handle.offer(byteArrayOf(3, 0))
        handle.start(); handle.finish(); handle.finish()
        assertTrue(sdk.stops.await(5, TimeUnit.SECONDS))
        assertTrue(sdk.closed.await(5, TimeUnit.SECONDS))
        val input = DataInputStream(output.toByteArray().inputStream())
        val header = ByteArray(input.readInt()); input.readFully(header)
        val policy = org.json.JSONObject(String(header, Charsets.UTF_8))
        assertEquals("NOTE", policy.getString("purpose"))
        assertEquals("en-AU", policy.getString("requestedLanguage"))
        assertTrue(policy.getBoolean("speechNetworkFallback"))
        assertEquals(0L, input.readLong()); assertEquals(4, input.readInt())
        val first = ByteArray(4); input.readFully(first); assertArrayEquals(byteArrayOf(1, 0, 2, 0), first)
        assertEquals(1L, input.readLong()); assertEquals(2, input.readInt()); assertEquals(3, input.readUnsignedByte()); assertEquals(0, input.readUnsignedByte())
        assertEquals(-1L, input.readLong()); assertEquals(-1, input.read())
        assertEquals(listOf(CaptureProtocol.START, CaptureProtocol.STOP), sdk.sent)
        handle.abort(); assertEquals(1, sdk.closeCount.get())
    }
    @Test fun timedOutChannelAcquisitionClosesALateChannelWithoutRequestingItsOutput() {
        val sdk = FakeCaptureWearSdk()
        val late = TaskCompletionSource<ChannelClient.Channel>()
        sdk.openTask = late.task
        val ownership = CaptureChannelOwnership(sdk, 20)
        val worker = Thread { runCatching { ownership.open("synthetic-phone", "/synthetic") } }
        worker.start(); worker.join(2_000)
        assertFalse("Never-completing SDK acquisition must be bounded", worker.isAlive)
        assertTrue(ownership.isRetired)
        late.setResult(sdk.channel)
        assertTrue(sdk.closed.await(2, TimeUnit.SECONDS))
        assertEquals(1, sdk.closeCount.get())
        assertEquals(1L, sdk.outputRequested.count)
    }
    @Test fun cancellationRetiresAcquiredChannelAndClosesOutputThatArrivesLater() {
        val sdk = FakeCaptureWearSdk()
        val late = TaskCompletionSource<OutputStream>()
        sdk.outputTask = late.task
        val ownership = CaptureChannelOwnership(sdk, 50)
        val worker = Thread { runCatching { ownership.output(ownership.open("synthetic-phone", "/synthetic")) } }
        worker.start()
        assertTrue(sdk.outputRequested.await(2, TimeUnit.SECONDS))
        ownership.close(); ownership.close(); worker.interrupt(); worker.join(2_000)
        val outputClosed = CountDownLatch(1)
        late.setResult(object : ByteArrayOutputStream() { override fun close() { outputClosed.countDown() } })
        assertTrue(outputClosed.await(2, TimeUnit.SECONDS))
        assertEquals(1, sdk.closeCount.get())
    }
    @Test fun overflowAbortsWithoutSendingNormalStopAndRetiresALateChannel() {
        val sdk = FakeCaptureWearSdk()
        val late = TaskCompletionSource<ChannelClient.Channel>()
        sdk.openTask = late.task
        val handle = LiveCapturePipe(record, sdk)
        handle.start(); assertTrue(sdk.opened.await(2, TimeUnit.SECONDS))
        repeat(21) { handle.offer(byteArrayOf(1, 0)) }
        late.setResult(sdk.channel)
        assertTrue(sdk.closed.await(2, TimeUnit.SECONDS))
        assertFalse(sdk.sent.contains(CaptureProtocol.STOP))
        handle.finish(); handle.abort(); assertEquals(1, sdk.closeCount.get())
    }
    @Test fun finishDeadlineClosesChannelAndUnblocksStalledOutputWithoutFalseEofOrStop() {
        val sdk = FakeCaptureWearSdk()
        val blocked = CountDownLatch(1)
        val unblock = CountDownLatch(1)
        sdk.outputTask = Tasks.forResult(object : OutputStream() {
            override fun write(value: Int) { blocked.countDown(); unblock.await(5, TimeUnit.SECONDS); error("Synthetic closed channel") }
        })
        sdk.onChannelClose = { unblock.countDown() }
        val handle = LiveCapturePipe(record, sdk, finishMs = 30)
        handle.start(); assertTrue(blocked.await(2, TimeUnit.SECONDS))
        handle.finish()
        assertTrue(sdk.closed.await(2, TimeUnit.SECONDS))
        assertFalse(sdk.sent.contains(CaptureProtocol.STOP))
    }

    @Test fun archiveDeadlineRetiresItsBlockedWriteAndRetainsTheLocalArchive() {
        val sdk = FakeCaptureWearSdk()
        val unblock = CountDownLatch(1)
        val blocked = CountDownLatch(1)
        sdk.outputTask = Tasks.forResult(object : OutputStream() {
            override fun write(value: Int) { blocked.countDown(); check(unblock.await(5, TimeUnit.SECONDS)); error("Synthetic closed archive channel") }
        })
        sdk.onChannelClose = { unblock.countDown() }
        val wav = java.io.File(org.robolectric.RuntimeEnvironment.getApplication().cacheDir, "synthetic-archive.wav")
        val bytes = byteArrayOf(1, 0, 2, 0)
        wav.writeBytes(bytes)
        val attempt = ArchiveTransfer(sdk, 100)
        val failed = CountDownLatch(1)
        val worker = Thread { runCatching { attempt.transfer("synthetic-phone", record, wav) }.onFailure { failed.countDown() } }
        worker.start()
        assertTrue(blocked.await(2, TimeUnit.SECONDS))
        assertTrue(sdk.closed.await(2, TimeUnit.SECONDS))
        worker.join(2_000)
        assertFalse(worker.isAlive)
        assertEquals(0L, failed.count)
        assertArrayEquals(bytes, wav.readBytes())
        attempt.close(); assertEquals(1, sdk.closeCount.get())
    }
}
