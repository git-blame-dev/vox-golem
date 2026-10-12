package dev.gitblame.voxgolem.assistant.inputtransport

import android.os.Looper
import android.os.Parcel
import com.google.android.gms.tasks.Task
import com.google.android.gms.tasks.TaskCompletionSource
import com.google.android.gms.tasks.Tasks
import com.google.android.gms.wearable.CapabilityInfo
import com.google.android.gms.wearable.ChannelClient
import com.google.android.gms.wearable.MessageClient
import com.google.android.gms.wearable.MessageEvent
import com.google.android.gms.wearable.Node
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.Shadows.shadowOf
import org.robolectric.annotation.Config
import org.robolectric.annotation.LooperMode
import java.io.ByteArrayInputStream
import java.io.ByteArrayOutputStream
import java.io.DataInputStream
import java.io.InputStream
import java.io.OutputStream
import java.io.IOException
import java.util.UUID
import java.util.concurrent.CopyOnWriteArrayList
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35], manifest = Config.NONE)
@LooperMode(LooperMode.Mode.PAUSED)
class GmsAssistantInputTransportTest {
    private class Channel(private val path: String, private val node: String = "synthetic-peer") : ChannelClient.Channel {
        override fun getNodeId() = node
        override fun getPath() = path
        override fun describeContents() = 0
        override fun writeToParcel(dest: Parcel, flags: Int) {}
    }
    private class DelayedReaderOutput : ByteArrayOutputStream() {
        val halfClosed = CountDownLatch(1)
        @Volatile var discarded = false
        var waitForWholeClose: CountDownLatch? = null
        override fun close() {
            halfClosed.countDown()
            waitForWholeClose?.await(2, TimeUnit.SECONDS)
        }
        fun readAfterHalfClose(): DataInputStream {
            check(!discarded) { "Whole Channel close discarded unread frames" }
            return DataInputStream(ByteArrayInputStream(toByteArray()))
        }
    }
    private class Clients : GmsInputClients {
        lateinit var callback: ChannelClient.ChannelCallback
        lateinit var messages: MessageClient.OnMessageReceivedListener
        val closed = CopyOnWriteArrayList<ChannelClient.Channel>()
        val sent = CountDownLatch(1)
        val requestedOutput = CountDownLatch(1)
        val output = DelayedReaderOutput()
        var outputTask: Task<OutputStream> = Tasks.forResult(output)
        var inputTask: Task<InputStream> = Tasks.forResult(ByteArrayInputStream(byteArrayOf(0, 0)))
        var terminalSendTask: Task<Int>? = null
        private fun done(): Task<Void> = Tasks.forResult(null)
        override fun addMessages(listener: MessageClient.OnMessageReceivedListener): Task<Void> { messages = listener; return done() }
        override fun removeMessages(listener: MessageClient.OnMessageReceivedListener) = Tasks.forResult(true)
        override fun addChannels(callback: ChannelClient.ChannelCallback): Task<Void> { this.callback = callback; return done() }
        override fun removeChannels(callback: ChannelClient.ChannelCallback) = Tasks.forResult(true)
        override fun capability(): Task<CapabilityInfo> = Tasks.forResult(object : CapabilityInfo {
            override fun getName() = AssistantInputWire.CAPABILITY
            override fun getNodes(): Set<Node> = setOf(object : Node {
                override fun getId() = "synthetic-peer"
                override fun getDisplayName() = "Synthetic peer"
                override fun isNearby() = true
            })
        })
        override fun send(node: String, path: String, bytes: ByteArray): Task<Int> {
            sent.countDown()
            return if (AssistantInputWire.decode(bytes).kind in setOf(InputKind.FINAL, InputKind.ERROR))
                terminalSendTask ?: Tasks.forResult(1) else Tasks.forResult(1)
        }
        override fun open(node: String, path: String): Task<ChannelClient.Channel> = Tasks.forResult(Channel(path, node))
        override fun input(channel: ChannelClient.Channel) = inputTask
        override fun output(channel: ChannelClient.Channel): Task<OutputStream> { requestedOutput.countDown(); return outputTask }
        override fun close(channel: ChannelClient.Channel): Task<Void> {
            closed += channel; output.discarded = true; output.waitForWholeClose?.countDown(); return done()
        }
        fun message(frame: InputFrame, node: String = "synthetic-peer") {
            val bytes = AssistantInputWire.encode(frame)
            messages.onMessageReceived(object : MessageEvent {
                override fun getPath() = AssistantInputWire.CONTROL_PATH
                override fun getData() = bytes
                override fun getSourceNodeId() = node
                override fun getRequestId() = 1
            })
        }
    }
    private val local = UUID(0, 101).toString()
    private val peer = UUID(0, 102).toString()
    private val session = UUID(0, 103).toString()
    private fun start() = InputFrame(InputKind.START, session, local, peer, "en-US", 0)
    private fun drain() = shadowOf(Looper.getMainLooper()).idle()
    private fun waitUntil(check: () -> Boolean) {
        val deadline = System.nanoTime() + TimeUnit.SECONDS.toNanos(2)
        while (!check() && System.nanoTime() < deadline) { drain(); Thread.sleep(5) }
        assertTrue(check())
    }
    private fun transport(clients: Clients, authorize: Boolean = false,
        onControl: (String, InputFrame) -> Unit = { _, _ -> }, onEnd: (String, String) -> Unit = { _, _ -> },
        onFailure: (String, String) -> Unit = { _, _ -> }, terminalMillis: Long = AssistantInputWire.IDLE_MILLIS,
    ) = GmsAssistantInputTransport(clients, CallerForegroundSignal { true },
        SourcePeerBinding { node, source, target -> node == "synthetic-peer" && source == peer && target == local },
        onControl, { _, _ -> authorize }, { _, _, _ -> true }, onEnd, onFailure, terminalMillis)

    @Test fun foreignChannelNamespacesAreUntouchedEvenAfterListenerShutdown() {
        val clients = Clients(); val transport = transport(clients)
        try {
            val note = Channel("/voxgolem/capture/live/synthetic-note")
            val archive = Channel("/voxgolem/capture/archive/synthetic-archive")
            clients.callback.onChannelOpened(note); clients.callback.onChannelOpened(archive); drain()
            assertTrue(clients.closed.isEmpty())
            transport.close()
            clients.callback.onChannelOpened(note); drain()
            assertTrue(clients.closed.isEmpty())
        } finally { transport.close() }
    }
    @Test fun malformedAndUnauthorizedOwnedChannelsAreClosed() {
        val clients = Clients(); val transport = transport(clients)
        try {
            val malformed = Channel(AssistantInputWire.AUDIO_PREFIX + "invalid-session")
            val unauthorized = Channel(AssistantInputWire.audioPath(session))
            clients.callback.onChannelOpened(malformed); clients.callback.onChannelOpened(unauthorized); drain()
            assertTrue(clients.closed.contains(malformed)); assertTrue(clients.closed.contains(unauthorized))
        } finally { transport.close() }
    }
    @Test fun normalOutputHalfClosePreservesDelayedFramesUntilAcceptedTerminal() {
        val clients = Clients()
        lateinit var transport: GmsAssistantInputTransport
        transport = transport(clients, onControl = { _, frame ->
            if (frame.kind == InputKind.FINAL && frame.session == session) transport.cancelAudio()
        })
        try {
            transport.control("synthetic-peer", start()); assertTrue(clients.sent.await(2, TimeUnit.SECONDS))
            var opened = false
            transport.openAudio("synthetic-peer", session) { opened = it }
            waitUntil { opened }
            val pcm = ByteArray(640) { 7 }
            assertTrue(transport.audio(pcm)); transport.finishAudio()
            assertTrue(clients.output.halfClosed.await(2, TimeUnit.SECONDS))
            Thread.sleep(100)
            assertTrue("Normal source EOF must not close the whole Channel", clients.closed.isEmpty())
            val delayed = clients.output.readAfterHalfClose()
            assertArrayEquals(pcm, PcmChannelWire.read(delayed)); assertNull(PcmChannelWire.read(delayed))
            val final = start().copy(kind = InputKind.FINAL, source = peer, target = local, sequence = 1, text = "synthetic final")
            clients.message(final, node = "synthetic-unselected-node")
            clients.message(final.copy(session = UUID(0, 104).toString()))
            drain(); assertTrue(clients.closed.isEmpty())
            clients.message(final)
            drain(); assertEquals(1, clients.closed.size)
        } finally { transport.close() }
    }
    @Test fun normalReceiverRetainsAcceptedHandleUntilLocalTerminalDelivery() {
        val clients = Clients(); var ended = false
        val terminal = TaskCompletionSource<Int>(); clients.terminalSendTask = terminal.task
        val transport = transport(clients, authorize = true, onEnd = { _, _ -> ended = true })
        try {
            val incoming = Channel(AssistantInputWire.audioPath(session))
            clients.callback.onChannelOpened(incoming)
            waitUntil { ended }; Thread.sleep(100)
            assertTrue("Receiver must retain its accepted handle while STT finishes", clients.closed.isEmpty())
            transport.control("synthetic-peer", start().copy(kind = InputKind.FINAL, sequence = 1, text = "synthetic final"))
            Thread.sleep(100); assertTrue(clients.closed.isEmpty())
            terminal.setResult(1)
            waitUntil { clients.closed.contains(incoming) }
        } finally { transport.close() }
    }
    @Test fun cancelledPendingOutputTaskClosesLateStreamButNoForeignChannel() {
        val clients = Clients(); val result = TaskCompletionSource<OutputStream>(); clients.outputTask = result.task
        val transport = transport(clients)
        try {
            transport.control("synthetic-peer", start()); assertTrue(clients.sent.await(2, TimeUnit.SECONDS))
            transport.openAudio("synthetic-peer", session) {}
            assertTrue(clients.requestedOutput.await(2, TimeUnit.SECONDS))
            transport.cancelAudio(); result.setResult(clients.output)
            assertTrue(clients.output.halfClosed.await(2, TimeUnit.SECONDS))
            assertTrue(clients.closed.all { it.path == AssistantInputWire.audioPath(session) })
        } finally { transport.close() }
    }
    @Test fun wholeChannelIsAbortedOnInputFailureAndExplicitCancellation() {
        val failed = Clients().apply { inputTask = Tasks.forException(IOException("synthetic input failure")) }
        val receiver = transport(failed, authorize = true)
        try {
            val own = Channel(AssistantInputWire.audioPath(session))
            failed.callback.onChannelOpened(own); waitUntil { failed.closed.contains(own) }
            assertEquals(1, failed.closed.size)
        } finally { receiver.close() }
        val clients = Clients(); val sender = transport(clients)
        try {
            sender.control("synthetic-peer", start()); assertTrue(clients.sent.await(2, TimeUnit.SECONDS))
            var opened = false; sender.openAudio("synthetic-peer", session) { opened = it }; waitUntil { opened }
            sender.cancelAudio(); waitUntil { clients.output.halfClosed.count == 0L }
            assertEquals(1, clients.closed.size)
        } finally { sender.close() }
    }
    @Test fun retainedHalfClosedChannelHasBoundedTerminalTimeout() {
        val clients = Clients(); var failures = 0
        val sender = transport(clients, onFailure = { _, _ -> failures++ }, terminalMillis = 50)
        try {
            sender.control("synthetic-peer", start()); assertTrue(clients.sent.await(2, TimeUnit.SECONDS))
            var opened = false; sender.openAudio("synthetic-peer", session) { opened = it }; waitUntil { opened }
            sender.finishAudio(); waitUntil { failures == 1 }
            assertEquals(1, clients.closed.size)
        } finally { sender.close() }
    }
    @Test fun stalledStreamHalfCloseHitsOperationDeadlineAndReleasesOwnedHandle() {
        val clients = Clients(); clients.output.waitForWholeClose = CountDownLatch(1)
        var failures = 0
        val sender = transport(clients, onFailure = { _, _ -> failures++ })
        try {
            sender.control("synthetic-peer", start()); assertTrue(clients.sent.await(2, TimeUnit.SECONDS))
            var opened = false; sender.openAudio("synthetic-peer", session) { opened = it }; waitUntil { opened }
            sender.finishAudio(); assertTrue(clients.output.halfClosed.await(2, TimeUnit.SECONDS))
            waitUntil { failures == 1 }
            assertEquals(1, clients.closed.size)
            assertEquals(AssistantInputWire.audioPath(session), clients.closed.single().path)
        } finally { sender.close() }
    }
}
