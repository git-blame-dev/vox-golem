package dev.gitblame.voxgolem.assistant.inputtransport

import android.content.Context
import android.content.ContextWrapper
import android.os.Looper
import com.google.android.gms.tasks.Task
import com.google.android.gms.tasks.TaskCompletionSource
import com.google.android.gms.tasks.Tasks
import com.google.android.gms.wearable.CapabilityInfo
import com.google.android.gms.wearable.Node
import dev.gitblame.voxgolem.assistant.AssistantStoreRunner
import dev.gitblame.voxgolem.assistant.replication.ApprovedPeerBinding
import dev.gitblame.voxgolem.assistant.replication.AssistantPeerHello
import dev.gitblame.voxgolem.assistant.replication.AssistantPeerRole
import dev.gitblame.voxgolem.assistant.replication.AssistantPeerSetup
import dev.gitblame.voxgolem.assistant.replication.AssistantPeerSetupOwner
import dev.gitblame.voxgolem.assistant.replication.EnrolledPeerRegistry
import org.json.JSONObject
import org.junit.Assert.*
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TemporaryFolder
import org.junit.runner.RunWith
import org.robolectric.RuntimeEnvironment
import org.robolectric.Shadows.shadowOf
import org.robolectric.annotation.Config
import org.robolectric.annotation.LooperMode
import java.io.File
import java.util.UUID
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit

@RunWith(AssistantStoreRunner::class)
@Config(sdk = [35], manifest = Config.NONE)
@LooperMode(LooperMode.Mode.PAUSED)
class ApplicationAssistantInputPeersTest {
    @get:Rule val folder = TemporaryFolder()
    private val local = UUID(0, 401).toString()
    private val peer = UUID(0, 402).toString()
    private val replica = peer
    private val association = UUID(0, 404).toString()
    private val selection = InputPeerSelection(local, peer, replica, association, AssistantPeerRole.WATCH)
    private class State(var selection: InputPeerSelection?) : InputPeerNativeState {
        override fun selected() = selection
        override fun stillApproved(selection: InputPeerSelection) = this.selection == selection
    }
    private class Sdk(var nodes: Set<String>, var hello: AssistantPeerHello?) : InputPeerSdk {
        val requested = CountDownLatch(1)
        val checkedNodes = mutableListOf<String>()
        var deferred: Task<CapabilityInfo>? = null
        fun capabilityValue() = object : CapabilityInfo {
            override fun getName() = AssistantInputWire.CAPABILITY
            override fun getNodes(): Set<Node> = this@Sdk.nodes.map { nodeId -> object : Node {
                override fun getId() = nodeId
                override fun getDisplayName() = "Synthetic peer"
                override fun isNearby() = true
            } }.toSet()
        }
        override fun capability(): Task<CapabilityInfo> { requested.countDown(); return deferred ?: Tasks.forResult(capabilityValue()) }
        override fun hello(node: String, deadlineNanos: Long): AssistantPeerHello? { checkedNodes += node; return hello }
    }
    private fun await(check: () -> Boolean) {
        val deadline = System.nanoTime() + TimeUnit.SECONDS.toNanos(2)
        while (!check() && System.nanoTime() < deadline) { shadowOf(Looper.getMainLooper()).idle(); Thread.sleep(5) }
        assertTrue(check())
    }
    @Test fun selectedSdkNodeBindsInstallationWithoutUsingInputPayloadAndRevocationFailsClosed() {
        val state = State(selection); val sdk = Sdk(setOf("synthetic-current-node"), AssistantPeerHello(replica, 3, AssistantPeerRole.WATCH))
        val peers = ApplicationAssistantInputPeers(state, sdk)
        try {
            assertNull(peers.selected()); peers.onForeground(true)
            await { peers.selected() != null }
            assertEquals(listOf("synthetic-current-node"), sdk.checkedNodes)
            assertTrue(peers.accepts("synthetic-current-node", peer, local))
            assertFalse(peers.accepts("synthetic-stale-node", peer, local))
            assertFalse(peers.accepts("synthetic-current-node", UUID(0, 499).toString(), local))
            state.selection = null
            assertFalse(peers.accepts("synthetic-current-node", peer, local))
            assertNull(peers.selected())
        } finally { peers.close() }
    }
    @Test fun unknownOrAmbiguousSdkIdentityNeverProducesASelectedRoute() {
        for (sdk in listOf(Sdk(setOf("synthetic-node"), AssistantPeerHello(UUID(0, 499).toString(), 3, AssistantPeerRole.WATCH)),
            Sdk(setOf("synthetic-one", "synthetic-two"), AssistantPeerHello(replica, 3, AssistantPeerRole.WATCH)),
            Sdk(setOf("synthetic-node"), AssistantPeerHello(replica, 3, AssistantPeerRole.PHONE)))) {
            val peers = ApplicationAssistantInputPeers(State(selection), sdk)
            try {
                peers.onForeground(true); assertTrue(sdk.requested.await(2, TimeUnit.SECONDS))
                Thread.sleep(100); shadowOf(Looper.getMainLooper()).idle()
                assertNull(peers.selected()); assertFalse(peers.accepts("synthetic-node", peer, local))
            } finally { peers.close() }
        }
    }
    @Test fun legacyHelloCannotAuthorizeInputEvenWithMatchingInstallationAndReplica() {
        val sdk = Sdk(setOf("synthetic-legacy-node"), AssistantPeerHello(replica, 2, AssistantPeerRole.WATCH))
        val peers = ApplicationAssistantInputPeers(State(selection), sdk)
        try {
            peers.onForeground(true); assertTrue(sdk.requested.await(2, TimeUnit.SECONDS))
            Thread.sleep(100); shadowOf(Looper.getMainLooper()).idle()
            assertNull(peers.selected())
        } finally { peers.close() }
    }
    @Test fun windowExitFencesLateSdkResolutionAndNeverUsesAnExpiredRoute() {
        val sdk = Sdk(setOf("synthetic-node"), AssistantPeerHello(replica, 3, AssistantPeerRole.WATCH))
        val pending = TaskCompletionSource<CapabilityInfo>(); sdk.deferred = pending.task
        var time = 0L
        val peers = ApplicationAssistantInputPeers(State(selection), sdk) { time }
        try {
            peers.onForeground(true); assertTrue(sdk.requested.await(2, TimeUnit.SECONDS))
            peers.onForeground(false); pending.setResult(sdk.capabilityValue())
            Thread.sleep(50); shadowOf(Looper.getMainLooper()).idle(); assertNull(peers.selected())
            sdk.deferred = null; peers.onForeground(true); await { peers.selected() != null }
            time = 3001; assertNull(peers.selected()); assertFalse(peers.accepts("synthetic-node", peer, local))
        } finally { peers.close() }
    }
    @Test fun actualApplicationRegistrySelectsNativeBindingAndRevocationFailsClosed() {
        val root = folder.newFolder("synthetic-conversation-only")
        val context = object : ContextWrapper(RuntimeEnvironment.getApplication()), AssistantPeerSetupOwner {
            override fun getApplicationContext(): Context = this
            override fun getNoBackupFilesDir(): File = root
            override val assistantPeerSetup = AssistantPeerSetup(this)
        }
        val installation = context.assistantPeerSetup.installation()
        EnrolledPeerRegistry(context).use { registry ->
            registry.rememberPeer(ApprovedPeerBinding(replica, association))
            registry.saveSetup("status", JSONObject().put("reason", "ENROLLED").put("association", association)
                .put("installation", peer).put("replica", replica).put("credentials", false))
        }
        val state = ApplicationInputPeerState(context)
        assertEquals(InputPeerSelection(installation, peer, replica, association, AssistantPeerRole.WATCH), state.selected())
        assertFalse(File(root, "assistant-auth.json").exists())
        EnrolledPeerRegistry(context).use { it.revoke(replica) }
        assertNull(state.selected())
    }
}
