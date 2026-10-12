package dev.gitblame.voxgolem.assistant.replication

import android.content.Context
import android.content.ContextWrapper
import android.content.pm.PackageManager
import android.database.sqlite.SQLiteDatabase
import dev.gitblame.voxgolem.assistant.AssistantStore
import dev.gitblame.voxgolem.assistant.AssistantStoreRunner
import org.json.JSONObject
import org.junit.Assert.*
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TemporaryFolder
import org.junit.runner.RunWith
import org.robolectric.RuntimeEnvironment
import org.robolectric.Shadows.shadowOf
import org.robolectric.annotation.Config
import java.io.File
import java.util.concurrent.Executors
import java.util.concurrent.TimeUnit

@RunWith(AssistantStoreRunner::class)
@Config(sdk = [24, 35], manifest = Config.NONE)
class AssistantPeerSetupTest {
    @get:Rule val folder = TemporaryFolder()
    private val a = "00000000-0000-4000-8000-000000000001"
    private val b = "00000000-0000-4000-8000-000000000002"
    private val c = "00000000-0000-4000-8000-000000000003"

    @Test fun oppositeFormFactorsLinkWithoutProviderFilesOrCredentialIdentity() = worker {
        val pair = PairFixture()
        pair.link()
        assertEquals(PeerPairingReason.ENROLLED, pair.left.state().reason)
        assertEquals(PeerPairingReason.ENROLLED, pair.right.state().reason)
        assertFalse(pair.left.state().credentialsAllowed)
        assertFalse(pair.right.state().credentialsAllowed)
        assertEquals(pair.left.state().associationId, pair.right.state().associationId)
        assertEquals(b, pair.left.state().peerInstallationId)
        EnrolledPeerRegistry(pair.leftContext).use { assertEquals(setOf(b), it.replicas()) }
        EnrolledPeerRegistry(pair.rightContext).use { assertEquals(setOf(a), it.replicas()) }
        assertTrue(pair.mailbox.events.all { it.uri.path == ReplicationWire.IDENTITY_PATH })
        assertFalse(File(pair.leftContext.noBackupFilesDir, "assistant-auth.json").exists())
        assertFalse(File(pair.rightContext.noBackupFilesDir, "assistant-auth.json").exists())
    }

    @Test fun reconnectAndReopenPreserveAssociationButNeverUseSavedNodeRoutes() = worker {
        val pair = PairFixture()
        pair.link()
        val before = pair.left.state()
        pair.mailbox.nodes.remove("synthetic-right-node")
        pair.publish(GmsReplicationTransport(pair.mailbox.sdk("synthetic-new-right-node"), budget()), b, "watch")
        val reopened = pair.setup(pair.leftContext, a, pair.leftSdk, false)
        reopened.retry(a, pair.leftIo)
        assertEquals(before, reopened.state())
        assertNull(reopened.replicaForNode("synthetic-right-node"))
        assertEquals(b, reopened.replicaForNode("synthetic-new-right-node"))
        pair.mailbox.unreachableNodes.add("synthetic-new-right-node")
        assertNull(reopened.replicaForNode("synthetic-new-right-node"))
    }

    @Test fun directV1AndV2MigrationsPreserveHistoryOutboxSessionsCleanupAndProviderFiles() = worker {
        for (oldVersion in 1..2) {
            val context = isolated("synthetic-migration-$oldVersion")
            val auth = File(context.noBackupFilesDir, "assistant-auth.json").apply { writeText("Synthetic externally owned metadata and access") }
            val legacy = File(context.filesDir, "codex-auth.json").apply { writeText("Synthetic externally owned legacy access") }
            val authBefore = auth.readBytes(); val legacyBefore = legacy.readBytes()
            AssistantStore(context).use { history ->
                history.createConversation("Synthetic retained migration history")
                val historyBefore = history.snapshot().toString()
                val publicationsBefore = history.publications(b, 128).toString()
                val sequenceBefore = history.allocateId().getString("sequence").toLong()
                val replica = history.replica()
                assertEquals(replica, NativeReplicationStore(history).replica)
                assertEquals(sequenceBefore + 1, history.allocateId().getString("sequence").toLong())
                SQLiteDatabase.openOrCreateDatabase(File(context.noBackupFilesDir, "assistant-replication.db"), null).use { db ->
                    db.execSQL("CREATE TABLE peers (replica TEXT PRIMARY KEY, session TEXT)")
                    db.execSQL("CREATE TABLE cleanup_peers (replica TEXT PRIMARY KEY)")
                    db.execSQL("CREATE TABLE setup (key TEXT PRIMARY KEY, value TEXT NOT NULL)")
                    db.execSQL("CREATE TABLE native_bindings (replica TEXT PRIMARY KEY, peer_installation TEXT NOT NULL, association TEXT NOT NULL, hub TEXT NOT NULL, credentials_allowed INTEGER NOT NULL)")
                    db.execSQL("CREATE TABLE retry (singleton INTEGER PRIMARY KEY, attempts INTEGER NOT NULL, due INTEGER NOT NULL)")
                    db.execSQL("INSERT INTO retry VALUES (1, 2, 1800000030000)")
                    db.execSQL("INSERT INTO peers VALUES (?, ?)", arrayOf(b, "{\"synthetic_session\":true}"))
                    db.execSQL("INSERT INTO cleanup_peers VALUES (?)", arrayOf(c))
                    db.execSQL("INSERT INTO setup VALUES ('status', '{\"reason\":\"INVITATION_PENDING\"}')")
                    db.execSQL("INSERT INTO native_bindings VALUES (?, ?, ?, ?, 1)", arrayOf(b, c, a, a))
                    db.version = oldVersion
                }
                EnrolledPeerRegistry(context).use { registry ->
                    assertNull(registry.setupValue("status"))
                    assertEquals(setOf(b), registry.replicas())
                    assertNull(registry.bindingFor(b, b))
                }
                DurableReplicationState(context).use { state ->
                    assertTrue(state.load(b)!!.getBoolean("synthetic_session"))
                    assertEquals(setOf(c), state.cleanupReplicas())
                    assertEquals(30_000L, state.retryDelay(1_800_000_000_000L))
                }
                assertEquals(historyBefore, history.snapshot().toString())
                assertEquals(publicationsBefore, history.publications(b, 128).toString())
            }
            assertArrayEquals(authBefore, auth.readBytes())
            assertArrayEquals(legacyBefore, legacy.readBytes())
        }
    }

    @Test fun missingOldV2UnknownVersionAndWrongRoleCannotAuthorizeRouting() = worker {
        val pair = PairFixture()
        pair.mailbox.slots.clear()
        assertEquals(PeerPairingReason.WAITING_FOR_PEER, pair.left.retry(a, pair.leftIo).reason)
        for ((version, role) in listOf(1 to null, 2 to "watch", 4 to "watch", 3 to "unknown")) {
            pair.publish(pair.rightIo, b, role, version)
            assertEquals(PeerPairingReason.PEER_UPDATE_REQUIRED, pair.left.retry(a, pair.leftIo).reason)
            assertNull(pair.left.replicaForNode("synthetic-right-node"))
        }
        pair.publish(pair.rightIo, b, "phone")
        assertEquals(PeerPairingReason.WAITING_FOR_PEER, pair.left.retry(a, pair.leftIo).reason)
    }

    @Test fun multipleIdentitiesAndDuplicateRoutesDisableExistingBinding() = worker {
        val pair = PairFixture(); pair.link()
        val other = GmsReplicationTransport(pair.mailbox.sdk("synthetic-other-watch"), budget())
        pair.publish(other, c, "watch")
        assertEquals(PeerPairingReason.MULTIPLE_PEERS, pair.left.retry(a, pair.leftIo).reason)
        assertNull(pair.left.replicaForNode("synthetic-right-node"))
        pair.publish(other, b, "watch")
        assertEquals(PeerPairingReason.MULTIPLE_PEERS, pair.left.retry(a, pair.leftIo).reason)
        assertNull(pair.left.replicaForNode("synthetic-right-node"))
    }

    @Test fun sameRoleIgnoredButOfflineOppositePeerStillCountsForAmbiguity() = worker {
        val pair = PairFixture()
        val extra = GmsReplicationTransport(pair.mailbox.sdk("synthetic-extra-peer"), budget())
        pair.publish(extra, c, "phone")
        assertEquals(PeerPairingReason.ENROLLED, pair.left.retry(a, pair.leftIo).reason)
        pair.mailbox.unreachableNodes.add("synthetic-right-node")
        assertNull(pair.left.replicaForNode("synthetic-right-node"))
        pair.publish(extra, c, "watch")
        assertEquals(PeerPairingReason.MULTIPLE_PEERS, pair.left.retry(a, pair.leftIo).reason)
    }

    @Test fun discoveryFailureIsVisibleAndCannotFallBackToStoredRoute() = worker {
        val pair = PairFixture(); pair.link()
        pair.leftSdk.capabilityError = IllegalStateException("Synthetic SDK discovery failed")
        assertEquals(PeerPairingReason.DISCOVERY_FAILED, pair.left.retry(a, pair.leftIo).reason)
        assertNull(pair.left.replicaForNode("synthetic-right-node"))
        pair.leftSdk.capabilityError = null
        assertEquals(PeerPairingReason.ENROLLED, pair.left.retry(a, pair.leftIo).reason)
    }

    @Test fun identityHintsBootstrapAndOldInvitationPathsAreNeverQueued() = worker {
        val pair = PairFixture()
        assertTrue(AssistantReplication.acceptsData(pair.mailbox.events.last()))
        for (kind in listOf("request", "ack")) {
            pair.leftIo.publish("/voxgolem/assistant/v1/pair/$kind", "{}".toByteArray(), emptyMap())
            assertFalse(AssistantReplication.acceptsData(pair.mailbox.events.last()))
        }
    }

    @Test fun persistedInitialBackoffIsBoundedWithoutAnyEnrolledPeer() {
        val context = isolated("synthetic-headless-backoff")
        val now = 1_800_000_000_000L
        val delay = DurableReplicationState(context).use { it.failed(now, kotlin.random.Random(7)) }
        DurableReplicationState(context).use { state ->
            assertEquals(delay, state.retryDelay(now))
            repeat(12) { assertTrue(state.failed(now, kotlin.random.Random(7)) in 30_000..DurableReplicationState.MAX_BACKOFF) }
        }
    }

    private inner class PairFixture {
        val leftContext = isolated("synthetic-auto-phone")
        val rightContext = isolated("synthetic-auto-watch")
        val mailbox = FakeGmsMailbox(folder.root)
        val leftSdk = mailbox.sdk("synthetic-left-node")
        val rightSdk = mailbox.sdk("synthetic-right-node")
        val leftIo = GmsReplicationTransport(leftSdk, budget())
        val rightIo = GmsReplicationTransport(rightSdk, budget())
        val left = setup(leftContext, a, leftSdk, false)
        val right = setup(rightContext, b, rightSdk, true)
        fun setup(context: Context, replica: String, sdk: GmsReplicationSdk, watch: Boolean): AssistantPeerSetup {
            shadowOf(context.packageManager).setSystemFeature(PackageManager.FEATURE_WATCH, watch)
            return AssistantPeerSetup(context, { replica }) { GmsReplicationTransport(sdk, budget()) }
        }
        fun link() { left.retry(a, leftIo); right.retry(b, rightIo) }
        fun publish(io: GmsReplicationTransport, replica: String, role: String?, version: Int = 3) {
            val hello = JSONObject().put("version", version).put("replica", replica)
            if (version == 2) hello.put("installation", c)
            if (role != null) hello.put("role", role)
            io.publish(ReplicationWire.IDENTITY_PATH, hello.toString().toByteArray(), emptyMap())
        }
        init { publish(leftIo, a, "phone"); publish(rightIo, b, "watch") }
    }
    private fun isolated(name: String): Context {
        val root = folder.newFolder(name)
        val files = folder.newFolder("$name-files")
        return object : ContextWrapper(RuntimeEnvironment.getApplication()) {
            override fun getApplicationContext(): Context = this
            override fun getNoBackupFilesDir(): File = root
            override fun getFilesDir(): File = files
        }
    }
    private fun worker(block: () -> Unit) {
        val executor = Executors.newSingleThreadExecutor()
        try { executor.submit(block).get(60, TimeUnit.SECONDS) } finally { executor.shutdownNow() }
    }
    private fun budget() = BoundedGmsWait(System.nanoTime() + TimeUnit.SECONDS.toNanos(30))
}
