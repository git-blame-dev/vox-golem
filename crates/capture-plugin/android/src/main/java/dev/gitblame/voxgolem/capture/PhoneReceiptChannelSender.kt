package dev.gitblame.voxgolem.capture

import android.content.Context
import com.google.android.gms.tasks.Task
import com.google.android.gms.tasks.Tasks
import com.google.android.gms.wearable.ChannelClient
import com.google.android.gms.wearable.Wearable
import java.io.OutputStream
import java.io.InputStream
import java.util.concurrent.Executor
import java.util.concurrent.TimeUnit

internal interface PhoneReceiptChannels {
    fun open(nodeId: String, path: String): Task<ChannelClient.Channel>
    fun output(channel: ChannelClient.Channel): Task<OutputStream>
    fun input(channel: ChannelClient.Channel): Task<InputStream>
    fun close(channel: ChannelClient.Channel): Task<Void>
}

/** Route filters select pending attempts; only exact SDK channel identity confirms retirement. */
internal class PhoneReceiptOpenRegistry(private val confirmationsPerOpen: Int = 16) {
    init { require(confirmationsPerOpen > 0) }
    internal class Pending(val nodeId: String, val path: String, val lifetime: CaptureChannelLifetime) {
        val closed = mutableSetOf<ChannelClient.Channel>()
    }
    private val lock = Any()
    private val pending = mutableSetOf<Pending>()
    private val adopted = mutableMapOf<ChannelClient.Channel, CaptureChannelLifetime>()
    internal val earlyConfirmationCount: Int get() = synchronized(lock) { pending.sumOf { it.closed.size } }
    fun begin(nodeId: String, path: String, lifetime: CaptureChannelLifetime): Pending = synchronized(lock) {
        Pending(nodeId, path, lifetime).also { pending += it }
    }
    fun adopt(open: Pending, channel: ChannelClient.Channel) {
        val alreadyClosed = synchronized(lock) {
            check(pending.remove(open))
            adopted[channel] = open.lifetime
            open.closed.contains(channel).also { open.closed.clear() }
        }
        // Confirm while native acquisition is still pending, before a failed close could be requested.
        if (alreadyClosed) open.lifetime.confirmNativeRetired()
    }
    fun forget(lifetime: CaptureChannelLifetime) = synchronized(lock) {
        pending.removeAll { it.lifetime === lifetime }
        adopted.entries.removeAll { it.value === lifetime }
    }
    fun failed(open: Pending) = synchronized(lock) { pending.remove(open); open.closed.clear() }
    fun confirmedClosed(channel: ChannelClient.Channel) {
        val lifetime = synchronized(lock) {
            adopted[channel].also { known ->
                if (known == null) pending.forEach { open ->
                    if (channel.nodeId == open.nodeId && channel.path == open.path && open.closed.size < confirmationsPerOpen) {
                        open.closed += channel
                    }
                }
            }
        }
        lifetime?.confirmNativeRetired()
    }
}

/** Production sender seam: never publishes a DataItem, even when another watch is connected. */
internal class PhoneReceiptChannelSender(
    private val sdk: PhoneReceiptChannels,
    private val timeoutMs: Long = 15_000,
    private val retirements: CaptureChannelRetirementOwner = processRetirements,
    private val opens: PhoneReceiptOpenRegistry = processOpens,
) {
    fun send(nodeId: String, captureId: String, bytes: ByteArray) {
        require(nodeId.isNotBlank())
        require(bytes.isNotEmpty() && CaptureProtocol.validatePayloadSize(bytes.size, channelResult = true))
        val path = CaptureReceiptChannel.path(captureId)
        retirements.retryFailedRetirements()
        lateinit var lifetime: CaptureChannelLifetime
        lifetime = retirements.reserve(timeoutMs) { opens.forget(lifetime) } ?: error("receipt-retirement-capacity-unavailable")
        val pending = opens.begin(nodeId, path, lifetime)
        try {
            val opening = try { sdk.open(nodeId, path) } catch (failure: Exception) {
                opens.failed(pending)
                lifetime.nativeAcquisitionFailed(); throw failure
            }
            opening.addOnCompleteListener(DIRECT) { task ->
                val acquired = if (task.isSuccessful) task.result else null
                if (acquired == null) { opens.failed(pending); lifetime.nativeAcquisitionFailed() }
                else {
                    opens.adopt(pending, acquired)
                    lifetime.acquiredNative { completed -> sdk.close(acquired).addOnCompleteListener(DIRECT) { completed(it.isSuccessful) } }
                }
            }
            val acquired = Tasks.await(opening, timeoutMs, TimeUnit.MILLISECONDS)
            check(acquired.nodeId == nodeId && acquired.path == path && !lifetime.isRetired)
            lifetime.beginStreamAcquisition()
            val output = try { sdk.output(acquired) } catch (failure: Exception) {
                lifetime.streamAcquisitionCompleted(null); throw failure
            }
            output.addOnCompleteListener(DIRECT) { lifetime.streamAcquisitionCompleted(if (it.isSuccessful) it.result else null) }
            val stream = Tasks.await(output, timeoutMs, TimeUnit.MILLISECONDS)
            check(!lifetime.isRetired)
            CaptureReceiptChannel.write(stream, bytes)
            stream.close()
            // Closing a native channel too early can invalidate the remote receipt read.
            lifetime.beginStreamAcquisition()
            val reading = try { sdk.input(acquired) } catch (failure: Exception) {
                lifetime.streamAcquisitionCompleted(null); throw failure
            }
            reading.addOnCompleteListener(DIRECT) { lifetime.streamAcquisitionCompleted(if (it.isSuccessful) it.result else null) }
            val acknowledgement = Tasks.await(reading, timeoutMs, TimeUnit.MILLISECONDS)
            check(!lifetime.isRetired)
            val consumed = acknowledgement.read()
            check(consumed == CaptureReceiptChannel.CONSUMED && !lifetime.isRetired)
            acknowledgement.close()
        } finally { lifetime.close() }
    }
    companion object {
        private val DIRECT = Executor { it.run() }
        private val processRetirements = CaptureChannelRetirementOwner(10)
        private val processOpens = PhoneReceiptOpenRegistry()
        fun confirmNativeRetired(channel: ChannelClient.Channel) { processOpens.confirmedClosed(channel) }
        fun google(context: Context): PhoneReceiptChannelSender {
            val client = Wearable.getChannelClient(context.applicationContext)
            return PhoneReceiptChannelSender(object : PhoneReceiptChannels {
                override fun open(nodeId: String, path: String) = client.openChannel(nodeId, path)
                override fun output(channel: ChannelClient.Channel) = client.getOutputStream(channel)
                override fun input(channel: ChannelClient.Channel) = client.getInputStream(channel)
                override fun close(channel: ChannelClient.Channel) = client.close(channel)
            })
        }
    }
}
