package dev.gitblame.voxgolem.assistant.replication

import android.content.Context
import android.content.ContextWrapper
import dev.gitblame.voxgolem.assistant.AssistantDomainNative
import dev.gitblame.voxgolem.assistant.AssistantStore
import dev.gitblame.voxgolem.assistant.AssistantStoreRunner
import dev.gitblame.voxgolem.assistant.DomainGateway
import org.junit.Assert.*
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TemporaryFolder
import org.junit.runner.RunWith
import org.robolectric.RuntimeEnvironment
import org.robolectric.annotation.Config
import java.io.File
import java.util.UUID
import java.util.concurrent.Executors
import java.util.concurrent.TimeUnit

@RunWith(AssistantStoreRunner::class)
@Config(sdk = [24, 35], manifest = Config.NONE)
class ReplicationCleanupRecoveryTest {
    @get:Rule val folder = TemporaryFolder()

    @Test fun runtimeRetryDrainsFullCleanupBacklogAfterSdkRecoveryThenEnrollsWithoutDeletingHistory() {
        assertNotNull("The focused Gradle task supplies the actual host JNI library", System.getProperty("assistant.native.library"))
        val root = folder.newFolder("synthetic-cleanup-recovery")
        val mailbox = FakeGmsMailbox(folder.root)
        val localSdk = mailbox.sdk("synthetic-recovery-phone")
        val remoteSdk = mailbox.sdk("synthetic-recovery-watch")
        fun transport() = GmsReplicationTransport(localSdk, budget())
        val context = object : ContextWrapper(RuntimeEnvironment.getApplication()), AssistantPeerSetupOwner {
            override fun getApplicationContext(): Context = this
            override fun getNoBackupFilesDir(): File = root
            override val assistantPeerSetup = AssistantPeerSetup(this, { AssistantStore(this).use { it.replica() } }, ::transport)
        }
        val peerReplica = UUID(0, 901).toString()
        val worker = Executors.newSingleThreadExecutor()
        try {
            worker.submit {
                val io = transport()
                GmsReplicationTransport(remoteSdk, budget()).publishIdentity(peerReplica, AssistantPeerRole.WATCH)
                AssistantStore(context, DomainGateway(AssistantDomainNative::apply)).use { history ->
                    history.createConversation("Synthetic history survives cleanup")
                    val before = history.snapshot().toString()
                    val localReplica = history.allocateId().getString("replica")
                    val retiredPeers = (0 until ReplicationWire.MAX_PEERS).map { UUID(0, 1000L + it).toString() }
                    EnrolledPeerRegistry(context).use { registry ->
                        for (retired in retiredPeers) {
                            registry.enroll(retired)
                            registry.revoke(retired)
                            io.publish(ReplicationWire.path(localReplica, retired, "hint"), "{}".toByteArray(), emptyMap())
                        }
                    }
                    val owner = AssistantReplication.Owner(context, PeerAssociationProvider(context.assistantPeerSetup::replicaForNode), ::transport, { true })
                    assertEquals(ReplicationWire.MAX_PEERS, owner.state.cleanupReplicas().size)
                    localSdk.deleteError = IllegalStateException("Synthetic SDK deletion unavailable")
                    owner.submit { owner.reconcileBlocking() }!!.get(30, TimeUnit.SECONDS)
                    assertEquals(ReplicationWire.MAX_PEERS, owner.state.cleanupReplicas().size)
                    assertTrue(owner.state.enrolledReplicas().isEmpty())
                    localSdk.deleteError = null
                    owner.submit { owner.reconcileBlocking() }!!.get(30, TimeUnit.SECONDS)
                    assertTrue("Runtime retries must drain cleanup even before peer setup succeeds", owner.state.cleanupReplicas().isEmpty())
                    assertEquals(setOf(peerReplica), owner.state.enrolledReplicas())
                    assertEquals(PeerPairingReason.ENROLLED, context.assistantPeerSetup.state().reason)
                    assertEquals(before, history.snapshot().toString())
                    assertTrue(mailbox.slots.keys.none { uri -> retiredPeers.any { uri.path!!.startsWith("${ReplicationWire.PREFIX}$localReplica/$it/") } })
                }
            }.get(90, TimeUnit.SECONDS)
        } finally { worker.shutdownNow() }
    }

    private fun budget() = BoundedGmsWait(System.nanoTime() + TimeUnit.SECONDS.toNanos(30))
}
