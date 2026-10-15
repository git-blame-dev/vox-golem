package dev.gitblame.voxgolem.android.wear.transport

import android.content.Context
import com.google.android.gms.tasks.Task
import com.google.android.gms.tasks.Tasks
import com.google.android.gms.wearable.CapabilityClient
import com.google.android.gms.wearable.CapabilityInfo
import com.google.android.gms.wearable.ChannelClient
import com.google.android.gms.wearable.Wearable
import dev.gitblame.voxgolem.capture.CaptureProtocol
import dev.gitblame.voxgolem.capture.CaptureChannelLifetime
import dev.gitblame.voxgolem.capture.CaptureChannelRetirementOwner
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
    init { CaptureWearListenerService.observeCaptureClosures(context.applicationContext) }
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
internal class CaptureChannelOwnership(
    private val sdk: CaptureWearSdk,
    private val timeoutMs: Long = 10_000,
    private val retirements: CaptureChannelRetirementOwner = processRetirements,
) : AutoCloseable {
    private val lock = Any()
    private var retired = false
    private var lifetime: CaptureChannelLifetime? = null
    val isRetired: Boolean get() = synchronized(lock) { retired || lifetime?.isRetired == true }

    fun open(nodeId: String, path: String): ChannelClient.Channel {
        retirements.retryFailedRetirements()
        val owner = synchronized(lock) {
            check(!retired && lifetime == null)
            lateinit var reserved: CaptureChannelLifetime
            reserved = retirements.reserve(15 * 60_000L) { channels.entries.removeAll { it.value === reserved } }
                ?: error("capture-retirement-capacity-unavailable")
            lifetime = reserved
            reserved
        }
        val task = try { sdk.open(nodeId, path) } catch (failure: Exception) {
            owner.nativeAcquisitionFailed(); close(); throw failure
        }
        task.addOnCompleteListener(DIRECT) { completed ->
            if (!completed.isSuccessful) owner.nativeAcquisitionFailed()
            else {
                val acquired = completed.result
                channels[acquired] = owner
                owner.acquiredNative { confirmed -> sdk.close(acquired).addOnCompleteListener(DIRECT) { confirmed(it.isSuccessful) } }
            }
        }
        return try {
            Tasks.await(task, timeoutMs, TimeUnit.MILLISECONDS).also { check(!isRetired) }
        } catch (error: Exception) { close(); throw error }
    }
    fun output(channel: ChannelClient.Channel): OutputStream {
        check(!isRetired)
        val owner = synchronized(lock) { lifetime } ?: error("capture-channel-not-open")
        owner.beginStreamAcquisition()
        val task = try { sdk.output(channel) } catch (failure: Exception) {
            owner.streamAcquisitionCompleted(null); close(); throw failure
        }
        task.addOnCompleteListener(DIRECT) { owner.streamAcquisitionCompleted(if (it.isSuccessful) it.result else null) }
        return try {
            Tasks.await(task, timeoutMs, TimeUnit.MILLISECONDS).also { check(!isRetired) }
        } catch (error: Exception) { close(); throw error }
    }
    override fun close() {
        val owned = synchronized(lock) {
            if (retired) return
            retired = true
            lifetime
        }
        owned?.close()
    }
    companion object {
        private val DIRECT = Executor { it.run() }
        private val processRetirements = CaptureChannelRetirementOwner(32)
        private val channels = java.util.concurrent.ConcurrentHashMap<ChannelClient.Channel, CaptureChannelLifetime>()
        fun confirmNativeRetired(channel: ChannelClient.Channel) { channels[channel]?.confirmNativeRetired() }
    }
}
