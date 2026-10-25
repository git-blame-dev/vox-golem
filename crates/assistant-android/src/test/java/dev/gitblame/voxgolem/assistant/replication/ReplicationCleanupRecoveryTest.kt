package dev.gitblame.voxgolem.assistant.replication

import android.content.Context
import android.content.ContextWrapper
import android.content.pm.PackageManager
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
import org.robolectric.Shadows.shadowOf
import org.robolectric.annotation.Config
import java.io.EOFException
import java.io.File
import java.util.UUID
import java.util.concurrent.Executors
import java.util.concurrent.TimeUnit

@RunWith(AssistantStoreRunner::class)
@Config(sdk = [24, 35], manifest = Config.NONE)
class ReplicationCleanupRecoveryTest {
    @get:Rule val folder = TemporaryFolder()

    @Test fun connectedSessionRevocationAndReEnrollmentConvergeCleanlyWithoutStaleSessionState() {
        assertNotNull("The focused Gradle task supplies the actual host JNI library", System.getProperty("assistant.native.library"))
        val gateway = DomainGateway(AssistantDomainNative::apply)
        val phoneContext = isolated("synthetic-revocation-phone", watch = false)
        val watchContext = isolated("synthetic-revocation-watch", watch = true)
        val worker = Executors.newSingleThreadExecutor()
        try {
            worker.submit {
                AssistantStore(phoneContext, gateway).use { phoneStore ->
                    AssistantStore(watchContext, gateway).use { watchStore ->
                        val phoneReplica = phoneStore.replica()
                        val watchReplica = watchStore.replica()
                        phoneStore.createConversation("Synthetic conversation before mid-sync revocation")

                        val phoneRegistry = EnrolledPeerRegistry(phoneContext)
                        val watchRegistry = EnrolledPeerRegistry(watchContext)
                        val phoneState = DurableReplicationState(phoneContext)
                        val watchState = DurableReplicationState(watchContext)
                        try {
                            // Repeated offline revocation and re-enrollment across > MAX_PEERS never blocks enrollment.
                            for (idx in 1..10) {
                                val retired = UUID(0, 1000L + idx).toString()
                                phoneRegistry.enroll(retired)
                                phoneRegistry.revoke(retired)
                            }
                            phoneRegistry.enroll(watchReplica)
                            watchRegistry.enroll(phoneReplica)
                            assertEquals(setOf(watchReplica), phoneState.enrolledReplicas())
                            assertEquals(setOf(phoneReplica), watchState.enrolledReplicas())

                            var dropOnRecordRequest = true
                            lateinit var watchController: ReplicationController
                            val phoneController = ReplicationController(
                                NativeReplicationStore(phoneStore),
                                NativeReplicationDomain(gateway),
                                phoneState,
                                object : ReplicationTransport {
                                    override fun reachable() = listOf(PeerNode("synthetic-watch-node", "Synthetic watch"))
                                    override fun openSession(nodeId: String, session: String): ReplicationSessionConnection {
                                        val pipe = InMemorySessionPipe(
                                            session = session,
                                            onFrame = { _, frame ->
                                                if (dropOnRecordRequest && frame.getString("kind") == "record_request") {
                                                    dropOnRecordRequest = false
                                                    // Revoke peer mid-sync and abort the connected session.
                                                    phoneRegistry.revoke(watchReplica)
                                                    throw EOFException("Synthetic mid-session revocation")
                                                }
                                            },
                                        )
                                        val responderConn = pipe.endpoint("synthetic-watch-node", "synthetic-phone-node")
                                        val responderThread = Thread {
                                            watchController.accept(responderConn)
                                        }.apply { isDaemon = true; start() }
                                        return pipe.endpoint("synthetic-phone-node", "synthetic-watch-node") {
                                            responderThread.join(5_000)
                                        }
                                    }
                                },
                                { node -> if (node == "synthetic-watch-node") watchReplica else null },
                                role = AssistantPeerRole.PHONE,
                            )
                            watchController = ReplicationController(
                                NativeReplicationStore(watchStore),
                                NativeReplicationDomain(gateway),
                                watchState,
                                object : ReplicationTransport {
                                    override fun reachable() = listOf(PeerNode("synthetic-phone-node", "Synthetic phone"))
                                },
                                { node -> if (node == "synthetic-phone-node") phoneReplica else null },
                                role = AssistantPeerRole.WATCH,
                            )

                            // 1. Mid-session revocation aborts sync without importing or acknowledging.
                            assertFalse(phoneController.reconcile())
                            assertTrue(phoneState.enrolledReplicas().isEmpty())
                            assertEquals(0, watchStore.snapshot().getJSONArray("records").length())
                            assertEquals(1, phoneStore.publications(watchReplica).length())

                            // 2. While revoked, reconcile does nothing.
                            assertTrue(phoneController.reconcile())
                            assertEquals(0, watchStore.snapshot().getJSONArray("records").length())

                            // 3. Re-enrolling the peer reconciles cleanly over a fresh connected session.
                            watchStore.createConversation("Synthetic watch conversation after re-enrollment")
                            phoneRegistry.enroll(watchReplica)
                            assertEquals(setOf(watchReplica), phoneState.enrolledReplicas())
                            assertTrue(phoneController.reconcile())
                            assertEquals(2, phoneStore.snapshot().getJSONArray("records").length())
                            assertEquals(2, watchStore.snapshot().getJSONArray("records").length())
                            assertEquals(0, phoneStore.publications(watchReplica).length())
                            assertEquals(0, watchStore.publications(phoneReplica).length())
                        } finally {
                            phoneState.close()
                            watchState.close()
                            phoneRegistry.close()
                            watchRegistry.close()
                        }
                    }
                }
            }.get(60, TimeUnit.SECONDS)
        } finally {
            worker.shutdownNow()
        }
    }

    private fun isolated(name: String, watch: Boolean): Context {
        val root = folder.newFolder(name)
        return object : ContextWrapper(RuntimeEnvironment.getApplication()) {
            init {
                shadowOf(packageManager).setSystemFeature(PackageManager.FEATURE_WATCH, watch)
            }
            override fun getApplicationContext(): Context = this
            override fun getNoBackupFilesDir(): File = root
        }
    }
}
