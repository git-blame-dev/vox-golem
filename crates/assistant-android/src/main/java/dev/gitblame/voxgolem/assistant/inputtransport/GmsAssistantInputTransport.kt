package dev.gitblame.voxgolem.assistant.inputtransport

import android.content.Context
import android.os.Handler
import android.os.Looper
import com.google.android.gms.tasks.Task
import com.google.android.gms.tasks.Tasks
import com.google.android.gms.wearable.CapabilityClient
import com.google.android.gms.wearable.CapabilityInfo
import com.google.android.gms.wearable.ChannelClient
import com.google.android.gms.wearable.MessageClient
import com.google.android.gms.wearable.MessageEvent
import com.google.android.gms.wearable.Wearable
import java.io.DataInputStream
import java.io.DataOutputStream
import java.io.InputStream
import java.io.OutputStream
import java.io.Closeable
import java.util.concurrent.ArrayBlockingQueue
import java.util.concurrent.Executor
import java.util.concurrent.Executors
import java.util.concurrent.ThreadPoolExecutor
import java.util.concurrent.TimeUnit
import java.util.concurrent.ScheduledFuture
import java.util.concurrent.atomic.AtomicInteger

internal interface GmsInputClients {
    fun addMessages(listener: MessageClient.OnMessageReceivedListener): Task<Void>
    fun removeMessages(listener: MessageClient.OnMessageReceivedListener): Task<Boolean>
    fun addChannels(callback: ChannelClient.ChannelCallback): Task<Void>
    fun removeChannels(callback: ChannelClient.ChannelCallback): Task<Boolean>
    fun capability(): Task<CapabilityInfo>
    fun send(node: String, path: String, bytes: ByteArray): Task<Int>
    fun open(node: String, path: String): Task<ChannelClient.Channel>
    fun input(channel: ChannelClient.Channel): Task<InputStream>
    fun output(channel: ChannelClient.Channel): Task<OutputStream>
    fun close(channel: ChannelClient.Channel): Task<Void>
}

private class AndroidGmsInputClients(context: Context) : GmsInputClients {
    private val application = context.applicationContext
    private val messages = Wearable.getMessageClient(application)
    private val channels = Wearable.getChannelClient(application)
    private val capabilities = Wearable.getCapabilityClient(application)
    override fun addMessages(listener: MessageClient.OnMessageReceivedListener) = messages.addListener(listener)
    override fun removeMessages(listener: MessageClient.OnMessageReceivedListener) = messages.removeListener(listener)
    override fun addChannels(callback: ChannelClient.ChannelCallback) = channels.registerChannelCallback(callback)
    override fun removeChannels(callback: ChannelClient.ChannelCallback) = channels.unregisterChannelCallback(callback)
    override fun capability() = capabilities.getCapability(AssistantInputWire.CAPABILITY, CapabilityClient.FILTER_REACHABLE)
    override fun send(node: String, path: String, bytes: ByteArray) = messages.sendMessage(node, path, bytes)
    override fun open(node: String, path: String) = channels.openChannel(node, path)
    override fun input(channel: ChannelClient.Channel) = channels.getInputStream(channel)
    override fun output(channel: ChannelClient.Channel) = channels.getOutputStream(channel)
    override fun close(channel: ChannelClient.Channel) = channels.close(channel)
}

/** Foreground dynamic listeners only. No DataItems, files, capture records or background service. */
class GmsAssistantInputTransport internal constructor(
    private val clients: GmsInputClients,
    private val foreground: CallerForegroundSignal,
    private val peers: SourcePeerBinding,
    private val onControl: (String, InputFrame) -> Unit,
    private val authorizeAudio: (String, String) -> Boolean,
    private val onAudio: (String, String, ByteArray) -> Boolean,
    private val onSourceEnded: (String, String) -> Unit,
    private val onFailure: (String, String) -> Unit,
    private val terminalMillis: Long = AssistantInputWire.IDLE_MILLIS,
) : AssistantAudioTransport, AutoCloseable {
    constructor(context: Context, foreground: CallerForegroundSignal, peers: SourcePeerBinding,
        onControl: (String, InputFrame) -> Unit, authorizeAudio: (String, String) -> Boolean,
        onAudio: (String, String, ByteArray) -> Boolean, onSourceEnded: (String, String) -> Unit,
        onFailure: (String, String) -> Unit,
    ) : this(AndroidGmsInputClients(context), foreground, peers, onControl, authorizeAudio, onAudio, onSourceEnded, onFailure)
    private val main = Handler(Looper.getMainLooper())
    private val controlWorker = ThreadPoolExecutor(1, 1, 0, TimeUnit.MILLISECONDS, ArrayBlockingQueue(32),
        { work -> Thread(work, "assistant-input-control").apply { isDaemon = true } }, ThreadPoolExecutor.AbortPolicy())
    private val timer = Executors.newSingleThreadScheduledExecutor { Thread(it, "assistant-input-timeout").apply { isDaemon = true } }
    private val generation = AtomicInteger()
    private val channelLock = Any()
    private var terminalTimeout: ScheduledFuture<*>? = null
    @Volatile private var closed = false
    @Volatile private var channel: ChannelClient.Channel? = null
    @Volatile private var io: Thread? = null
    @Volatile private var queue: BoundedPcmQueue? = null
    private var outgoing: InputFrame? = null
    private val messageListener = MessageClient.OnMessageReceivedListener { event -> receive(event) }
    private val channelCallback = object : ChannelClient.ChannelCallback() {
        override fun onChannelOpened(channel: ChannelClient.Channel) { main.post { receive(channel) } }
    }
    init {
        requireInputMain()
        clients.addMessages(messageListener)
            .addOnSuccessListener(Executor { it.run() }) { if (closed) clients.removeMessages(messageListener) }
            .addOnFailureListener { failRegistration() }
        clients.addChannels(channelCallback)
            .addOnSuccessListener(Executor { it.run() }) { if (closed) clients.removeChannels(channelCallback) }
            .addOnFailureListener { failRegistration() }
    }
    private fun failRegistration() { main.post { if (!closed) close() } }
    override fun control(node: String, frame: InputFrame) {
        requireInputMain()
        check(!closed && (foreground.isForeground() || frame.kind in setOf(InputKind.STOP, InputKind.CANCEL, InputKind.ERROR)))
        check(peers.accepts(node, frame.target, frame.source))
        if (frame.kind == InputKind.START) outgoing = frame
        val encoded = AssistantInputWire.encode(frame)
        val epoch = generation.get()
        controlWorker.execute {
            try {
                if (frame.kind == InputKind.START) requireCapability(node)
                check(!closed && (frame.kind != InputKind.START || generation.get() == epoch))
                await(clients.send(node, AssistantInputWire.CONTROL_PATH, encoded))
                if (frame.kind in setOf(InputKind.FINAL, InputKind.ERROR)) retireTerminalChannel(node, frame.session)
            }
            catch (_: Exception) { main.post { if (!closed) onFailure(node, frame.session) } }
        }
    }
    private fun receive(event: MessageEvent) {
        // Copy before leaving the SDK callback; reject oversized control before decoding.
        if (event.path != AssistantInputWire.CONTROL_PATH || event.data.size > AssistantInputWire.MAX_CONTROL_BYTES) return
        val node = event.sourceNodeId
        val frame = runCatching { AssistantInputWire.decode(event.data) }.getOrNull() ?: return
        main.post {
            if (closed || !foreground.isForeground() || !peers.accepts(node, frame.source, frame.target)) return@post
            if (frame.kind != InputKind.START) { onControl(node, frame); return@post }
            runCatching { controlWorker.execute {
                if (runCatching { requireCapability(node) }.isSuccess) main.post {
                    if (!closed && foreground.isForeground() && peers.accepts(node, frame.source, frame.target)) onControl(node, frame)
                }
            } }
        }
    }
    override fun openAudio(node: String, session: String, result: (Boolean) -> Unit) {
        requireInputMain(); check(!closed && io?.isAlive != true && channel == null)
        val start = outgoing ?: error("Assistant input START required")
        check(start.session == session && foreground.isForeground() && peers.accepts(node, start.target, start.source))
        val epoch = generation.incrementAndGet()
        val chunks = BoundedPcmQueue(); queue = chunks
        io = Thread({
            var current: ChannelClient.Channel? = null
            var adopted = false
            var endedNormally = false
            try {
                requireCapability(node)
                current = await(clients.open(node, AssistantInputWire.audioPath(session))) { clients.close(it) }
                adopted = adopt(current, epoch)
                if (!adopted) return@Thread
                val output = await(clients.output(current)) { it.close() }
                var markerWritten = false
                withBoundedStreamClose(current, DataOutputStream(output)) { stream ->
                    main.post { if (generation.get() == epoch && !closed) result(true) }
                    val deadline = System.nanoTime() + AssistantInputWire.MAX_SESSION_MILLIS * 1000000
                    while (!chunks.cancelled && generation.get() == epoch) {
                        check(System.nanoTime() < deadline)
                        val pcm = chunks.poll(50)
                        if (pcm == null) { if (chunks.drained()) break else continue }
                        bounded(current) { PcmChannelWire.write(stream, pcm) }
                    }
                    if (!chunks.cancelled && generation.get() == epoch) {
                        bounded(current) { PcmChannelWire.end(stream) }
                        markerWritten = true
                    }
                }
                endedNormally = markerWritten && !chunks.cancelled && generation.get() == epoch && !closed
            } catch (_: Exception) {
                main.post { if (!closed && generation.get() == epoch) { result(false); onFailure(node, session) } }
            } finally {
                current?.let {
                    if (endedNormally) retainUntilTerminal(it, epoch) else abort(it, adopted)
                }
            }
        }, "assistant-input-send").apply { isDaemon = true; start() }
    }
    override fun audio(pcm: ByteArray): Boolean = queue?.offer(pcm) ?: false
    override fun finishAudio() { requireInputMain(); queue?.finish() }
    override fun cancelAudio() {
        requireInputMain(); generation.incrementAndGet(); queue?.cancel(); queue = null
        channel?.let { abort(it, true) }; io?.interrupt()
    }
    private fun receive(incoming: ChannelClient.Channel) {
        requireInputMain()
        // The global SDK callback also receives Note/archive and other app-owned channels.
        if (incoming.path != AssistantInputWire.PREFIX.removeSuffix("/") && !incoming.path.startsWith(AssistantInputWire.PREFIX)) return
        if (channel == incoming) return
        val session = AssistantInputWire.sessionFromPath(incoming.path)
        if (closed || !foreground.isForeground() || session == null || io?.isAlive == true || channel != null ||
            !authorizeAudio(incoming.nodeId, session)) { clients.close(incoming); return }
        val epoch = generation.incrementAndGet()
        if (!adopt(incoming, epoch)) { clients.close(incoming); return }
        io = Thread({
            var endedNormally = false
            try {
                val input = await(clients.input(incoming)) { it.close() }
                var markerRead = false
                withBoundedStreamClose(incoming, DataInputStream(input)) { stream ->
                    val deadline = System.nanoTime() + AssistantInputWire.MAX_SESSION_MILLIS * 1000000
                    var bytes = 0L
                    while (!closed && generation.get() == epoch) {
                        check(System.nanoTime() < deadline)
                        val pcm = bounded(incoming) { PcmChannelWire.read(stream) }
                        if (pcm == null) {
                            markerRead = true
                            main.post { if (!closed && generation.get() == epoch) onSourceEnded(incoming.nodeId, session) }
                            break
                        }
                        bytes += pcm.size; check(bytes <= AssistantInputWire.SAMPLE_RATE * 2L * 60)
                        // Bounded dispatcher handoff prevents a blocked main thread from buffering unbounded PCM.
                        val accepted = java.util.concurrent.CompletableFuture<Boolean>()
                        main.post {
                            accepted.complete(!closed && generation.get() == epoch && foreground.isForeground() &&
                                authorizeAudio(incoming.nodeId, session) && onAudio(incoming.nodeId, session, pcm))
                        }
                        check(accepted.get(250, TimeUnit.MILLISECONDS))
                    }
                }
                endedNormally = markerRead && generation.get() == epoch && !closed
            } catch (_: Exception) { main.post { if (!closed && generation.get() == epoch) onFailure(incoming.nodeId, session) } }
            finally {
                if (endedNormally) retainUntilTerminal(incoming, epoch) else abort(incoming, true)
            }
        }, "assistant-input-receive").apply { isDaemon = true; start() }
    }
    private fun adopt(incoming: ChannelClient.Channel, epoch: Int): Boolean = synchronized(channelLock) {
        if (closed || generation.get() != epoch || channel != null) false
        else { channel = incoming; true }
    }
    private fun detach(current: ChannelClient.Channel): Boolean = synchronized(channelLock) {
        if (channel !== current) false else {
            channel = null; terminalTimeout?.cancel(false); terminalTimeout = null; true
        }
    }
    private fun abort(current: ChannelClient.Channel, adopted: Boolean) {
        if (detach(current) || !adopted) clients.close(current)
    }
    private fun retainUntilTerminal(current: ChannelClient.Channel, epoch: Int) {
        synchronized(channelLock) {
            if (channel !== current || closed || generation.get() != epoch) return
            terminalTimeout?.cancel(false)
            terminalTimeout = timer.schedule({
                if (detach(current)) {
                    clients.close(current)
                    main.post {
                        val session = AssistantInputWire.sessionFromPath(current.path)
                        if (!closed && generation.get() == epoch && session != null) onFailure(current.nodeId, session)
                    }
                }
            }, terminalMillis, TimeUnit.MILLISECONDS)
        }
    }
    private fun retireTerminalChannel(node: String, session: String) {
        channel?.takeIf { it.nodeId == node && AssistantInputWire.sessionFromPath(it.path) == session }
            ?.let { abort(it, true) }
    }
    private fun <T : Closeable, R> withBoundedStreamClose(current: ChannelClient.Channel, stream: T, action: (T) -> R): R {
        try { return action(stream) } finally { bounded(current) { stream.close() } }
    }
    private fun <T> bounded(channel: ChannelClient.Channel, action: () -> T): T {
        val epoch = generation.get()
        val timeout = timer.schedule({
            abort(channel, true)
            main.post {
                val session = AssistantInputWire.sessionFromPath(channel.path)
                if (!closed && generation.get() == epoch && session != null) onFailure(channel.nodeId, session)
            }
        }, 1000, TimeUnit.MILLISECONDS)
        try { return action() } finally { timeout.cancel(false) }
    }
    private fun requireCapability(node: String) {
        val reachable = await(clients.capability())
        check(reachable.nodes.any { it.id == node }) { "assistant-input-peer-unavailable" }
    }
    private fun <T> await(task: Task<T>, lateClose: (T) -> Unit = {}): T {
        try { return Tasks.await(task, 2, TimeUnit.SECONDS) }
        catch (error: Exception) {
            task.addOnSuccessListener(Executor { it.run() }) { runCatching { lateClose(it) } }
            if (error is InterruptedException) Thread.currentThread().interrupt()
            throw error
        }
    }
    override fun close() {
        requireInputMain(); if (closed) return
        closed = true; cancelAudio()
        clients.removeMessages(messageListener); clients.removeChannels(channelCallback)
        controlWorker.shutdownNow(); timer.shutdownNow()
    }
}
