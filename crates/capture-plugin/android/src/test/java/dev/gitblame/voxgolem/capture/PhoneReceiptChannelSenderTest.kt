package dev.gitblame.voxgolem.capture

import android.os.Parcel
import com.google.android.gms.tasks.Task
import com.google.android.gms.tasks.TaskCompletionSource
import com.google.android.gms.tasks.Tasks
import com.google.android.gms.wearable.ChannelClient
import org.json.JSONObject
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment
import org.robolectric.annotation.Config
import java.io.ByteArrayOutputStream
import java.io.OutputStream
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35])
class PhoneReceiptChannelSenderTest {
    private val id = "123e4567-e89b-42d3-a456-426614174010"
    private class Channel(private val node: String, private val route: String) : ChannelClient.Channel {
        override fun getNodeId() = node
        override fun getPath() = route
        override fun describeContents() = 0
        override fun writeToParcel(parcel: Parcel, flags: Int) = Unit
    }
    private inner class Sdk : PhoneReceiptChannels {
        val requests = java.util.concurrent.CopyOnWriteArrayList<Pair<String, String>>()
        val outputs = java.util.concurrent.ConcurrentHashMap<String, ByteArrayOutputStream>()
        val closed = CountDownLatch(1)
        val outputRequested = CountDownLatch(1)
        val inputRequested = CountDownLatch(1)
        val retirements = CaptureChannelRetirementOwner(1)
        val opens = PhoneReceiptOpenRegistry()
        val closeRequests = java.util.concurrent.CopyOnWriteArrayList<ChannelClient.Channel>()
        var opening: Task<ChannelClient.Channel>? = null
        var stream: Task<OutputStream>? = null
        var acknowledgement: Task<java.io.InputStream>? = null
        var onClose: () -> Unit = {}
        override fun open(nodeId: String, path: String): Task<ChannelClient.Channel> {
            requests += nodeId to path
            return opening ?: Tasks.forResult(Channel(nodeId, path))
        }
        override fun output(channel: ChannelClient.Channel): Task<OutputStream> {
            outputRequested.countDown()
            return stream ?: Tasks.forResult(outputs.getOrPut(channel.nodeId) { ByteArrayOutputStream() })
        }
        override fun input(channel: ChannelClient.Channel): Task<java.io.InputStream> {
            inputRequested.countDown()
            return acknowledgement ?: Tasks.forResult(byteArrayOf(CaptureReceiptChannel.CONSUMED.toByte()).inputStream())
        }
        @Volatile var closeResult: Task<Void> = Tasks.forResult(null)
        @Volatile var throwOnClose = false
        override fun close(channel: ChannelClient.Channel): Task<Void> {
            closeRequests += channel
            if (throwOnClose) throw IllegalStateException("Synthetic synchronous close failure")
            onClose(); closed.countDown(); return closeResult
        }
        fun sender(timeoutMs: Long = 15_000) = PhoneReceiptChannelSender(this, timeoutMs, retirements, opens)
    }

    @Test fun productionDeliverySendsOversizedReceiptOnlyToExactWatchAndPreservesFullV1Json() {
        val sdk = Sdk()
        val repository = CaptureRepository(RuntimeEnvironment.getApplication())
        val text = "Synthetic private watch one transcript ".repeat(2_000)
        val record = CaptureRecord(id, createdAtUtc = "2026-10-01T00:00:00Z", captureState = CaptureState.SAVED,
            recognitionState = RecognitionState.FINAL, finalText = text, sha256 = "a".repeat(64), transcriptVersion = 3)
        val messages = mutableListOf<String>()
        val delivery = PhoneCaptureDelivery(repository, { node, _, _ -> messages += node }, sdk.sender()::send)
        repository.admitPhoneCapture(record, "synthetic-watch-one")
        try {
            delivery.sendReceipt("synthetic-watch-one", record, "PHONE_ARCHIVED")
            val done = CountDownLatch(1); delivery.barrier { done.countDown() }
            assertTrue(done.await(5, TimeUnit.SECONDS))
            assertEquals(listOf("synthetic-watch-one" to CaptureReceiptChannel.path(id)), sdk.requests)
            assertFalse(sdk.outputs.containsKey("synthetic-watch-two"))
            assertTrue(messages.isEmpty())
            val bytes = CaptureReceiptChannel.read(sdk.outputs.getValue("synthetic-watch-one").toByteArray().inputStream())
            assertTrue(bytes.size > CaptureProtocol.MAX_MESSAGE_BYTES)
            val receipt = JSONObject(String(bytes, Charsets.UTF_8))
            assertEquals(CaptureProtocol.PROTOCOL, receipt.getString("protocol"))
            assertEquals(id, receipt.getString("captureId"))
            assertEquals(text, receipt.getString("finalText"))
            assertEquals(3, receipt.getInt("transcriptVersion"))
        } finally { delivery.shutdown() }
    }
    @Test fun timedOutOpenClosesLateChannelAndDoesNotAllocateOutput() {
        val sdk = Sdk()
        val late = TaskCompletionSource<ChannelClient.Channel>(); sdk.opening = late.task
        val worker = Thread { runCatching { sdk.sender(20).send("synthetic-watch-one", id, byteArrayOf(1)) } }
        worker.start(); worker.join(2_000)
        assertFalse(worker.isAlive)
        late.setResult(Channel("synthetic-watch-one", CaptureReceiptChannel.path(id)))
        assertTrue(sdk.closed.await(2, TimeUnit.SECONDS))
        assertEquals(1L, sdk.outputRequested.count)
    }
    @Test fun productionDeliveryOverResultLimitSendsOnlySafeSmallFailureToExactNode() {
        val sdk = Sdk()
        val repository = CaptureRepository(RuntimeEnvironment.getApplication())
        val messages = java.util.concurrent.CopyOnWriteArrayList<Pair<String, JSONObject>>()
        val delivery = PhoneCaptureDelivery(repository,
            { node, _, bytes -> messages += node to JSONObject(String(bytes, Charsets.UTF_8)) }, sdk.sender()::send)
        try {
            val record = CaptureRecord(id, createdAtUtc = "2026-10-01T00:00:00Z", captureState = CaptureState.SAVED,
                recognitionState = RecognitionState.FINAL, finalText = "合".repeat(CaptureProtocol.MAX_CHANNEL_RESULT_BYTES), sha256 = "a".repeat(64))
            repository.admitPhoneCapture(record, "synthetic-watch-one")
            delivery.sendReceipt("synthetic-watch-one", record, "PHONE_ARCHIVED")
            val done = CountDownLatch(1); delivery.barrier { done.countDown() }
            assertTrue(done.await(5, TimeUnit.SECONDS))
            assertTrue(sdk.requests.isEmpty())
            assertEquals("synthetic-watch-one", messages.single().first)
            assertEquals("final-transcript-exceeds-result-limit", messages.single().second.getString("errorCode"))
            assertFalse(messages.single().second.has("finalText"))
        } finally { delivery.shutdown() }
    }
    @Test fun timedOutOutputRetiresLateStreamWithoutWritingPrivateBytes() {
        val sdk = Sdk()
        val late = TaskCompletionSource<OutputStream>(); sdk.stream = late.task
        val worker = Thread { runCatching { sdk.sender(20).send("synthetic-watch-one", id, byteArrayOf(1)) } }
        worker.start(); worker.join(2_000)
        assertFalse(worker.isAlive)
        val closed = CountDownLatch(1)
        val stream = object : ByteArrayOutputStream() { override fun close() { closed.countDown() } }
        late.setResult(stream)
        assertTrue(closed.await(2, TimeUnit.SECONDS))
        assertEquals(0, stream.size())
    }
    @Test fun senderKeepsChannelUntilReceiptConsumedAndClosesLateAcknowledgementAfterTimeout() {
        val sdk = Sdk()
        val pending = TaskCompletionSource<java.io.InputStream>(); sdk.acknowledgement = pending.task
        val failure = java.util.concurrent.atomic.AtomicReference<Throwable?>()
        val worker = Thread { runCatching {
            sdk.sender(1_000).send("synthetic-watch-one", id, byteArrayOf(1, 2, 3))
        }.onFailure(failure::set) }
        worker.start()
        assertTrue(sdk.inputRequested.await(2, TimeUnit.SECONDS))
        assertEquals(1L, sdk.closed.count)
        assertArrayEquals(byteArrayOf(1, 2, 3), CaptureReceiptChannel.read(sdk.outputs.getValue("synthetic-watch-one").toByteArray().inputStream()))
        pending.setResult(byteArrayOf(CaptureReceiptChannel.CONSUMED.toByte()).inputStream())
        worker.join(2_000)
        assertFalse(worker.isAlive)
        assertNull(failure.get())
        assertTrue(sdk.closed.await(2, TimeUnit.SECONDS))

        val lateSdk = Sdk()
        val late = TaskCompletionSource<java.io.InputStream>(); lateSdk.acknowledgement = late.task
        val timedOut = Thread { runCatching { lateSdk.sender(20).send("synthetic-watch-one", id, byteArrayOf(1)) } }
        timedOut.start(); timedOut.join(2_000)
        assertFalse(timedOut.isAlive)
        val readerClosed = CountDownLatch(1)
        late.setResult(object : java.io.ByteArrayInputStream(byteArrayOf(1)) { override fun close() { readerClosed.countDown() } })
        assertTrue(readerClosed.await(2, TimeUnit.SECONDS))
        assertTrue(lateSdk.closed.await(2, TimeUnit.SECONDS))
    }
    @Test fun deadlineRetiresStalledWriteAndSenderRejectsSizeBeforeSdkAllocation() {
        val sdk = Sdk()
        val unblock = CountDownLatch(1)
        sdk.onClose = { unblock.countDown() }
        sdk.stream = Tasks.forResult(object : OutputStream() {
            override fun write(value: Int) { check(unblock.await(2, TimeUnit.SECONDS)); error("Synthetic closed channel") }
        })
        val worker = Thread { runCatching { sdk.sender(30).send("synthetic-watch-one", id, byteArrayOf(1)) } }
        worker.start(); worker.join(2_000)
        assertFalse(worker.isAlive)
        assertTrue(sdk.closed.await(2, TimeUnit.SECONDS))
        val before = sdk.requests.toList()
        assertThrows(IllegalArgumentException::class.java) {
            sdk.sender().send("synthetic-watch-one", id, ByteArray(CaptureProtocol.MAX_CHANNEL_RESULT_BYTES + 1))
        }
        assertEquals(before, sdk.requests)
    }
    @Test fun deadlineRetiresBlockedAcknowledgementReadWithoutFalseSuccess() {
        val sdk = Sdk()
        val unblock = CountDownLatch(1)
        sdk.onClose = { unblock.countDown() }
        sdk.acknowledgement = Tasks.forResult(object : java.io.InputStream() {
            override fun read(): Int { check(unblock.await(2, TimeUnit.SECONDS)); throw java.io.IOException("Synthetic closed acknowledgement stream") }
        })
        val failure = java.util.concurrent.atomic.AtomicReference<Throwable?>()
        val worker = Thread { runCatching {
            sdk.sender(30).send("synthetic-watch-one", id, byteArrayOf(1))
        }.onFailure(failure::set) }
        worker.start(); worker.join(2_000)
        assertFalse(worker.isAlive)
        assertNotNull(failure.get())
        assertTrue(sdk.closed.await(2, TimeUnit.SECONDS))
    }
    @Test fun wrongSdkTargetNeverReceivesPayload() {
        val sdk = Sdk().apply { opening = Tasks.forResult(Channel("synthetic-watch-two", CaptureReceiptChannel.path(id))) }
        val failure = java.util.concurrent.atomic.AtomicReference<Throwable?>()
        val worker = Thread { runCatching {
            sdk.sender().send("synthetic-watch-one", id, byteArrayOf(1))
        }.onFailure(failure::set) }
        worker.start(); worker.join(2_000)
        assertFalse(worker.isAlive)
        assertTrue(failure.get() is IllegalStateException)
        assertTrue(sdk.outputs.isEmpty())
    }
    @Test fun asynchronousCloseFailureRetainsQuotaUntilActualTaskRecoveryAndDoesNotOpenAnotherChannel() {
        val sdk = Sdk()
        sdk.closeResult = Tasks.forException(IllegalStateException("Synthetic /private/channel close failure"))
        val worker = Thread { sdk.sender().send("synthetic-watch-one", id, byteArrayOf(1)) }
        worker.start(); worker.join(2_000); assertFalse(worker.isAlive)
        await { sdk.closeRequests.size == 3 }
        val exact = sdk.closeRequests.first()
        assertEquals(1, sdk.retirements.size)
        val failed = java.util.concurrent.atomic.AtomicReference<Throwable?>()
        val blocked = Thread { runCatching { sdk.sender().send("synthetic-watch-one", id, byteArrayOf(2)) }.onFailure(failed::set) }
        blocked.start(); blocked.join(2_000)
        assertTrue(failed.get() is IllegalStateException)
        assertEquals(1, sdk.requests.size)
        assertTrue(sdk.closeRequests.all { it === exact })
        sdk.closeResult = Tasks.forResult(null)
        sdk.retirements.retryFailedRetirements()
        await { sdk.retirements.size == 0 }
        assertTrue(sdk.closeRequests.all { it === exact })
    }
    @Test fun synchronousCloseFailureAndMatchingCallbackRetireOnlyTheFailedNativeOwner() {
        val sdk = Sdk().apply { throwOnClose = true }
        val worker = Thread { sdk.sender().send("synthetic-watch-one", id, byteArrayOf(1)) }
        worker.start(); worker.join(2_000); assertFalse(worker.isAlive)
        await { sdk.closeRequests.size == 3 }
        val old = sdk.closeRequests.first()
        assertEquals(1, sdk.retirements.size)
        sdk.opens.confirmedClosed(old)
        await { sdk.retirements.size == 0 }
        sdk.throwOnClose = false
        val pending = TaskCompletionSource<java.io.InputStream>()
        sdk.acknowledgement = pending.task
        val replacement = Thread { runCatching { sdk.sender(2_000).send("synthetic-watch-one", id, byteArrayOf(2)) } }
        replacement.start()
        await { sdk.requests.size == 2 && sdk.retirements.size == 1 }
        sdk.opens.confirmedClosed(old)
        assertEquals(1, sdk.retirements.size)
        pending.setResult(byteArrayOf(CaptureReceiptChannel.CONSUMED.toByte()).inputStream())
        replacement.join(2_000); assertFalse(replacement.isAlive)
        await { sdk.retirements.size == 0 }
    }
    private fun await(condition: () -> Boolean) {
        val deadline = System.nanoTime() + TimeUnit.SECONDS.toNanos(5)
        while (!condition()) { check(System.nanoTime() < deadline) { "Synthetic receipt retirement did not complete" }; Thread.sleep(1) }
    }

    @Test fun closedBeforeDelayedOpenAdoptionFreesQuotaEvenWhenNativeCloseAlwaysFailsAndDoesNotRetireReplacement() {
        val sdk = Sdk().apply { closeResult = Tasks.forException(IllegalStateException("Synthetic permanent close failure")) }
        val first = TaskCompletionSource<ChannelClient.Channel>(); sdk.opening = first.task
        val exact = Channel("synthetic-watch-one", CaptureReceiptChannel.path(id))
        val worker = Thread { runCatching { sdk.sender(20).send("synthetic-watch-one", id, byteArrayOf(1)) } }
        worker.start(); worker.join(2_000)
        assertFalse(worker.isAlive); assertEquals(1, sdk.retirements.size)
        sdk.opens.confirmedClosed(exact)
        assertEquals(1, sdk.opens.earlyConfirmationCount)
        first.setResult(exact)
        await { sdk.retirements.size == 0 }
        assertEquals(0, sdk.opens.earlyConfirmationCount)
        assertTrue(sdk.closeRequests.isEmpty()); assertEquals(1L, sdk.outputRequested.count)

        val second = TaskCompletionSource<ChannelClient.Channel>(); sdk.opening = second.task
        val ack = TaskCompletionSource<java.io.InputStream>(); sdk.acknowledgement = ack.task
        val replacement = Channel("synthetic-watch-one", CaptureReceiptChannel.path(id))
        val replacementWorker = Thread { runCatching { sdk.sender(2_000).send("synthetic-watch-one", id, byteArrayOf(2)) } }
        replacementWorker.start(); await { sdk.requests.size == 2 }
        // Same capture path/node is only a cache filter, never a retirement confirmation for this new handle.
        sdk.opens.confirmedClosed(exact)
        second.setResult(replacement)
        assertTrue(sdk.inputRequested.await(2, TimeUnit.SECONDS))
        sdk.opens.confirmedClosed(exact)
        assertEquals(1, sdk.retirements.size)
        sdk.opens.confirmedClosed(replacement)
        ack.setResult(byteArrayOf(CaptureReceiptChannel.CONSUMED.toByte()).inputStream())
        replacementWorker.join(2_000); assertFalse(replacementWorker.isAlive)
        await { sdk.retirements.size == 0 }
    }

    @Test fun preAdoptionConfirmationCacheIsBoundedAndIgnoresUnrelatedCallbacks() {
        val registry = PhoneReceiptOpenRegistry(4)
        val owner = CaptureChannelRetirementOwner(1)
        val lifetime = owner.reserve()!!
        val route = CaptureReceiptChannel.path(id)
        val pending = registry.begin("synthetic-watch-one", route, lifetime)
        repeat(1_000) {
            registry.confirmedClosed(Channel("synthetic-other-watch", route))
            registry.confirmedClosed(Channel("synthetic-watch-one", "$route/unrelated-$it"))
        }
        assertEquals(0, registry.earlyConfirmationCount)
        repeat(100) { registry.confirmedClosed(Channel("synthetic-watch-one", route)) }
        assertEquals(4, registry.earlyConfirmationCount)
        registry.failed(pending); lifetime.nativeAcquisitionFailed()
        assertEquals(0, registry.earlyConfirmationCount)
        await { owner.size == 0 }
    }
}
