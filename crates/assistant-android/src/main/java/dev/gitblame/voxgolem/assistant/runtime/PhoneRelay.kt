package dev.gitblame.voxgolem.assistant.runtime

import android.content.Context
import android.content.pm.PackageManager
import android.os.PowerManager
import com.google.android.gms.tasks.Tasks
import com.google.android.gms.wearable.ChannelClient
import com.google.android.gms.wearable.Wearable
import dev.gitblame.voxgolem.assistant.replication.AssistantPeerSetupOwner
import java.io.InputStream
import java.io.OutputStream
import java.util.concurrent.TimeUnit

interface PhoneRelayClient {
    fun respond(requestId: String, requestJson: String, callback: ProviderDeltaCallback): String
    fun cancel(requestId: String)
    fun whenRetired(requestId: String, retired: () -> Unit)
}

internal class PhoneRelayProvider(private val relay: PhoneRelayClient) : ProviderNativeCalls {
    override fun respond(requestId: String, authPath: String, requestJson: String, callback: ProviderDeltaCallback): String =
        relay.respond(requestId, requestJson, callback)
    override fun cancel(requestId: String) = relay.cancel(requestId)
    override fun whenRetired(requestId: String, retired: () -> Unit) = relay.whenRetired(requestId, retired)
}

internal fun isWatch(context: Context): Boolean = context.packageManager?.hasSystemFeature(PackageManager.FEATURE_WATCH) == true
internal fun watchPhoneRelay(context: Context): ProviderNativeCalls? =
    if (isWatch(context)) PhoneRelayProvider(GmsPhoneRelayClient(context.applicationContext ?: context)) else null

/** Only Data Layer package/signature isolated channels, bound to a freshly verified enrolled peer. */
internal interface PhoneRelayConnection {
    val nodeId: String
    val path: String
    val identity: Any get() = this
    fun input(): InputStream
    fun output(): OutputStream
    /** Must promptly unblock stream I/O, without waiting for remote retirement. */
    fun close()
}

internal class PhoneRelayRoute(val nodeId: String, val stillApproved: () -> Boolean)
internal interface PhoneRelayPlatform {
    val watch: Boolean
    fun route(expectedNode: String? = null): PhoneRelayRoute?
    fun open(route: PhoneRelayRoute, path: String): PhoneRelayConnection
    fun acquireExecution(timeoutMs: Long, cancel: () -> Unit): AutoCloseable
}

private class GmsRelayPlatform(private val context: Context) : PhoneRelayPlatform {
    override val watch get() = isWatch(context)
    override fun route(expectedNode: String?): PhoneRelayRoute? {
        val setup = (context.applicationContext as? AssistantPeerSetupOwner)?.assistantPeerSetup ?: return null
        val route = setup.selectedRoute(expectedNode) ?: return null
        return PhoneRelayRoute(route.nodeId, route::stillApproved)
    }
    override fun open(route: PhoneRelayRoute, path: String): PhoneRelayConnection =
        GmsRelayConnection(context, Tasks.await(Wearable.getChannelClient(context).openChannel(route.nodeId, path), 5, TimeUnit.SECONDS))
    override fun acquireExecution(timeoutMs: Long, cancel: () -> Unit): AutoCloseable = PhoneRelayExecution.acquire(context, timeoutMs, cancel)
}

/** CPU suspension protection only, not process-lifetime or remote hardware-retirement evidence. */
internal class PhoneRelayWakeLease(context: Context, timeoutMs: Long) : AutoCloseable {
    private var wake: PowerManager.WakeLock? = null
    init {
        require(timeoutMs > 0)
        val manager = requireNotNull(context.getSystemService(Context.POWER_SERVICE) as? PowerManager)
        val acquired = manager.newWakeLock(PowerManager.PARTIAL_WAKE_LOCK, "VoxGolem:PhoneRelayExecution")
        acquired.setReferenceCounted(false)
        try {
            acquired.acquire(timeoutMs)
            wake = acquired
        } catch (failure: RuntimeException) {
            if (acquired.isHeld) acquired.release()
            throw failure
        }
    }
    @Synchronized override fun close() {
        val acquired = wake ?: return
        wake = null
        if (acquired.isHeld) acquired.release()
    }
}

private class GmsRelayConnection(context: Context, private val channel: ChannelClient.Channel) : PhoneRelayConnection {
    private val client = Wearable.getChannelClient(context)
    override val nodeId get() = channel.nodeId
    override val path get() = channel.path
    override val identity get() = channel
    override fun input(): InputStream = Tasks.await(client.getInputStream(channel), 5, TimeUnit.SECONDS)
    override fun output(): OutputStream = Tasks.await(client.getOutputStream(channel), 5, TimeUnit.SECONDS)
    override fun close() { client.close(channel) }
}

class GmsPhoneRelayClient(context: Context) : PhoneRelayClient {
    private val relay = PhoneRelayBridge.runtime(context)
    override fun respond(requestId: String, requestJson: String, callback: ProviderDeltaCallback): String = relay.respond(requestId, requestJson, callback)
    override fun cancel(requestId: String) = relay.cancel(requestId)
    override fun whenRetired(requestId: String, retired: () -> Unit) = relay.whenRetired(requestId, retired)
}

object PhoneRelayBridge {
    private var instance: PhoneRelayRuntime? = null
    @Synchronized internal fun runtime(context: Context): PhoneRelayRuntime = instance ?: run {
        val application = context.applicationContext ?: context
        PhoneRelayRuntime(GmsRelayPlatform(application)) { AssistantRuntime.router(application) }.also { instance = it }
    }

    /** SDK 19.0.0 exposes the modern overload on WearableListenerService (verified from its AAR). */
    fun onChannelOpened(context: Context, channel: ChannelClient.Channel) {
        // Opening-side notifications must not close the watch's locally owned outgoing channel.
        if (isWatch(context) || !PhoneRelayWire.validPath(channel.path)) return
        runtime(context).accept(GmsRelayConnection(context, channel))
    }
}
