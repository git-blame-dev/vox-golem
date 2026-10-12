package dev.gitblame.voxgolem.android.wear.transport

import android.content.Context
import com.google.android.gms.tasks.Task
import com.google.android.gms.tasks.Tasks
import com.google.android.gms.wearable.CapabilityClient
import com.google.android.gms.wearable.CapabilityInfo
import com.google.android.gms.wearable.ChannelClient
import com.google.android.gms.wearable.Wearable
import dev.gitblame.voxgolem.capture.CaptureProtocol
import java.io.OutputStream
import java.util.concurrent.Executor
import java.util.concurrent.TimeUnit

internal interface CaptureWearSdk {
    fun phones(): Task<CapabilityInfo>
    fun open(nodeId: String, path: String): Task<ChannelClient.Channel>
    fun output(channel: ChannelClient.Channel): Task<OutputStream>
    fun close(channel: ChannelClient.Channel): Task<Void>
    fun send(nodeId: String, path: String, bytes: ByteArray): Task<Int>
}

internal class GoogleCaptureWearSdk(context: Context) : CaptureWearSdk {
    private val capability = Wearable.getCapabilityClient(context)
    private val channels = Wearable.getChannelClient(context)
    private val messages = Wearable.getMessageClient(context)
    override fun phones() = capability.getCapability(CaptureProtocol.PHONE_CAPABILITY, CapabilityClient.FILTER_REACHABLE)
    override fun open(nodeId: String, path: String) = channels.openChannel(nodeId, path)
    override fun output(channel: ChannelClient.Channel) = channels.getOutputStream(channel)
    override fun close(channel: ChannelClient.Channel) = channels.close(channel)
    override fun send(nodeId: String, path: String, bytes: ByteArray) = messages.sendMessage(nodeId, path, bytes)
}

/** Owns the actual SDK acquisitions, including resources delivered after timeout or cancellation. */
internal class CaptureChannelOwnership(private val sdk: CaptureWearSdk, private val timeoutMs: Long = 10_000) : AutoCloseable {
    private val lock = Any()
    private var retired = false
    private var channel: ChannelClient.Channel? = null
    private var stream: OutputStream? = null
    val isRetired: Boolean get() = synchronized(lock) { retired }

    fun open(nodeId: String, path: String): ChannelClient.Channel {
        check(!isRetired)
        val task = sdk.open(nodeId, path)
        task.addOnSuccessListener(DIRECT) { acquired ->
            val late = synchronized(lock) { if (retired) true else { channel = acquired; false } }
            if (late) runCatching { sdk.close(acquired) }
        }
        return try {
            Tasks.await(task, timeoutMs, TimeUnit.MILLISECONDS).also { check(!isRetired) }
        } catch (error: Exception) { close(); throw error }
    }
    fun output(channel: ChannelClient.Channel): OutputStream {
        check(!isRetired)
        val task = sdk.output(channel)
        task.addOnSuccessListener(DIRECT) { acquired ->
            val late = synchronized(lock) { if (retired) true else { stream = acquired; false } }
            if (late) retireStream(acquired)
        }
        return try {
            Tasks.await(task, timeoutMs, TimeUnit.MILLISECONDS).also { check(!isRetired) }
        } catch (error: Exception) { close(); throw error }
    }
    override fun close() {
        val owned = synchronized(lock) {
            if (retired) return
            retired = true
            (channel to stream).also { channel = null; stream = null }
        }
        // Request native retirement before Java close so Data Layer can interrupt a blocked write.
        owned.first?.let { runCatching { sdk.close(it) } }
        owned.second?.let(::retireStream)
    }
    companion object {
        private val DIRECT = Executor { it.run() }
        private val cleanup = java.util.concurrent.Executors.newFixedThreadPool(2) { runnable ->
            Thread(runnable, "capture-stream-retirement").apply { isDaemon = true }
        }
        private fun retireStream(stream: OutputStream) {
            runCatching { cleanup.execute { runCatching { stream.close() } } }
        }
    }
}
