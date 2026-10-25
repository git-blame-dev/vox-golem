package dev.gitblame.voxgolem.assistant.replication

import android.app.job.JobParameters
import android.content.Context
import android.content.ContextWrapper
import android.content.pm.PackageManager
import android.net.Uri
import com.google.android.gms.wearable.DataItem
import com.google.android.gms.wearable.DataItemAsset
import com.google.android.gms.wearable.MessageEvent
import dev.gitblame.voxgolem.assistant.AssistantStore
import dev.gitblame.voxgolem.assistant.AssistantStoreRunner
import dev.gitblame.voxgolem.assistant.runtime.PhoneRelayWire
import org.junit.After
import org.junit.Assert.*
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TemporaryFolder
import org.junit.runner.RunWith
import org.robolectric.Robolectric
import org.robolectric.RuntimeEnvironment
import org.robolectric.Shadows.shadowOf
import org.robolectric.annotation.Config
import org.robolectric.util.ReflectionHelpers
import java.io.File
import java.util.UUID
import java.util.concurrent.CountDownLatch
import java.util.concurrent.FutureTask
import java.util.concurrent.TimeUnit

@RunWith(AssistantStoreRunner::class)
@Config(sdk = [24, 35], manifest = Config.NONE)
class ReplicationLifecycleTest {
    @get:Rule val folder = TemporaryFolder()
    private val testOwners = mutableSetOf<AssistantReplication.Owner>()

    @After fun retireTestOwners() {
        val executors = testOwners.map { ReflectionHelpers.getField<java.util.concurrent.ThreadPoolExecutor>(it, "executor") }
        for (executor in executors) {
            runCatching { executor.shutdown() }
        }
        for (executor in executors) {
            if (!executor.awaitTermination(30, TimeUnit.SECONDS)) {
                executor.shutdownNow()
                executor.awaitTermination(30, TimeUnit.SECONDS)
            }
        }
    }

    @Test fun jobStopCancelsOnlyItsWorkAndIgnoresLateCompletion() {
        val service = Robolectric.buildService(AssistantReplicationJobService::class.java).create().get()
        val parameters = ReflectionHelpers.newInstance(JobParameters::class.java)
        val future = FutureTask<Unit> {}
        var complete: (() -> Unit)? = null
        var finished = 0
        service.beginWork = { callback -> complete = callback; future }
        service.finishWork = { finished++ }
        assertTrue(service.onStartJob(parameters))
        assertTrue(service.onStopJob(parameters))
        assertTrue(future.isCancelled)
        complete!!.invoke()
        assertEquals(0, finished)
        service.onDestroy()
    }

    @Test fun normalJobCompletesExactlyOnceAndRejectsUnavailableWork() {
        val service = Robolectric.buildService(AssistantReplicationJobService::class.java).create().get()
        val parameters = ReflectionHelpers.newInstance(JobParameters::class.java)
        var complete: (() -> Unit)? = null
        var finished = 0
        service.beginWork = { callback -> complete = callback; FutureTask<Unit> {} }
        service.finishWork = { finished++ }
        assertTrue(service.onStartJob(parameters))
        complete!!.invoke(); complete!!.invoke()
        assertEquals(1, finished)
        service.beginWork = { null }
        assertFalse(service.onStartJob(parameters))
        service.onDestroy()
    }

    @Test fun listenerServiceAndAdmissionFilterIdentityHintsAndSessionChannelsWithoutLeaking() {
        val a = UUID(0, 1).toString()
        val b = UUID(0, 2).toString()
        val sessionUuid = UUID(0, 3).toString()

        // 1. DataItem admission & rejection
        val validIdentity = fakeDataItem("wear://synthetic-node${ReplicationWire.IDENTITY_PATH}", ByteArray(64))
        assertTrue(AssistantReplication.acceptsData(validIdentity))
        val oversizedIdentity = fakeDataItem("wear://synthetic-node${ReplicationWire.IDENTITY_PATH}", ByteArray(2049))
        assertFalse(AssistantReplication.acceptsData(oversizedIdentity))
        val v2HistoryItem = fakeDataItem("wear://synthetic-node${ReplicationWire.path(a, b, "hint")}", ByteArray(32))
        assertFalse(AssistantReplication.acceptsData(v2HistoryItem))
        val v1HistoryItem = fakeDataItem("wear://synthetic-node/voxgolem/assistant/v1/$a/$b/records", ByteArray(32))
        assertFalse(AssistantReplication.acceptsData(v1HistoryItem))

        val fixture = Fixture("synthetic-listener-routing")
        val owner = AssistantReplication.Owner(
            fixture.context,
            fixture.context.assistantPeerAssociationProvider,
            fixture::transport,
            { false },
        ).also { testOwners += it }

        withOwner(owner) {
            val service = Robolectric.buildService(AssistantReplicationListenerService::class.java).create().get()

            ReflectionHelpers.setStaticField(AssistantReplication::class.java, "status", AssistantReplicationStatus.IDLE)
            AssistantReplication.onData(fixture.context, v2HistoryItem)
            assertEquals(AssistantReplicationStatus.REJECTED, AssistantReplication.status)

            ReflectionHelpers.setStaticField(AssistantReplication::class.java, "status", AssistantReplicationStatus.IDLE)
            AssistantReplication.onData(fixture.context, v1HistoryItem)
            assertEquals(AssistantReplicationStatus.REJECTED, AssistantReplication.status)

            // 2. Message routing: reconcile-hint vs oversized vs chatgpt-access
            val validHintBytes = ReplicationWire.encodeHint(b, a)
            val validHintMsg = fakeMessage("synthetic-watch-node", ReplicationWire.HINT_PATH, validHintBytes)
            assertTrue(AssistantReplication.acceptsMessage(validHintMsg))
            val oversizedHintMsg = fakeMessage("synthetic-watch-node", ReplicationWire.HINT_PATH, ByteArray(ReplicationWire.MAX_HINT_BYTES + 1))
            assertFalse(AssistantReplication.acceptsMessage(oversizedHintMsg))
            val emptyNodeHintMsg = fakeMessage("", ReplicationWire.HINT_PATH, validHintBytes)
            assertFalse(AssistantReplication.acceptsMessage(emptyNodeHintMsg))

            ReflectionHelpers.setStaticField(AssistantReplication::class.java, "status", AssistantReplicationStatus.IDLE)
            service.onMessageReceived(oversizedHintMsg)
            assertEquals(AssistantReplicationStatus.REJECTED, AssistantReplication.status)

            val pendingHints = ReflectionHelpers.getField<LinkedHashMap<String, ByteArray>>(owner, "pendingHints")
            withBlockedOwner(owner, 0) {
                service.onMessageReceived(validHintMsg)
                assertArrayEquals(validHintBytes, pendingHints["synthetic-watch-node"])
            }

            val chatGptOwnerPrev = ReflectionHelpers.getStaticField<ChatGptAccessSync.Owner?>(ChatGptAccessSync::class.java, "owner")
            var chatGptReceivedKind: String? = null
            val customChatGptOwner = ChatGptAccessSync.Owner(
                fixture.context,
                fixture.context.assistantPeerSetup,
                executor = java.util.concurrent.Executor { runnable ->
                    chatGptReceivedKind = "dispatched"
                    runnable.run()
                },
            )
            ReflectionHelpers.setStaticField(ChatGptAccessSync::class.java, "owner", customChatGptOwner)
            try {
                ReflectionHelpers.setStaticField(AssistantReplication::class.java, "status", AssistantReplicationStatus.IDLE)
                service.onMessageReceived(fakeMessage("synthetic-watch-node", ChatGptAccessProtocol.PREFIX + "hint", "{}".toByteArray()))
                assertEquals("dispatched", chatGptReceivedKind)
                assertEquals(AssistantReplicationStatus.IDLE, AssistantReplication.status)
            } finally {
                ReflectionHelpers.setStaticField(ChatGptAccessSync::class.java, "owner", chatGptOwnerPrev)
            }

            // 3. Channel routing: phone-relay vs replication session vs invalid/rejected
            val validSessionChannel = FakeGmsMailbox.FakeChannel(
                "synthetic-watch-node",
                ReplicationWire.sessionPath(sessionUuid),
                FakeGmsMailbox.InMemoryPipe(fixture.mailbox),
                FakeGmsMailbox.InMemoryPipe(fixture.mailbox),
                fixture.mailbox,
            )
            assertTrue(AssistantReplication.acceptsChannel(validSessionChannel))
            val emptyNodeSessionChannel = FakeGmsMailbox.FakeChannel(
                "",
                ReplicationWire.sessionPath(sessionUuid),
                FakeGmsMailbox.InMemoryPipe(fixture.mailbox),
                FakeGmsMailbox.InMemoryPipe(fixture.mailbox),
                fixture.mailbox,
            )
            assertFalse(AssistantReplication.acceptsChannel(emptyNodeSessionChannel))

            // Invalid empty-node session channel is rejected.
            ReflectionHelpers.setStaticField(AssistantReplication::class.java, "status", AssistantReplicationStatus.IDLE)
            service.onChannelOpened(emptyNodeSessionChannel)
            assertEquals(AssistantReplicationStatus.REJECTED, AssistantReplication.status)

            // Valid session channel when nativeAvailable=false or waiting for peer is closed without leaking.
            val closedBefore = fixture.mailbox.closedChannels
            service.onChannelOpened(validSessionChannel)
            owner.submit {}!!.get(30, TimeUnit.SECONDS)
            assertEquals(closedBefore + 1, fixture.mailbox.closedChannels)

            // Valid session channel when queue is full is closed immediately by transport().closeChannel.
            val busySessionChannel = FakeGmsMailbox.FakeChannel(
                "synthetic-watch-node",
                ReplicationWire.sessionPath(UUID(0, 4).toString()),
                FakeGmsMailbox.InMemoryPipe(fixture.mailbox),
                FakeGmsMailbox.InMemoryPipe(fixture.mailbox),
                fixture.mailbox,
            )
            withBlockedOwner(owner, 16) {
                val closedBeforeBusy = fixture.mailbox.closedChannels
                service.onChannelOpened(busySessionChannel)
                assertEquals(closedBeforeBusy + 1, fixture.mailbox.closedChannels)
            }

            // PhoneRelay channel routing preservation: PhoneRelayWire.PREFIX + uuid is routed to PhoneRelayBridge, not AssistantReplication.
            shadowOf(RuntimeEnvironment.getApplication().packageManager).setSystemFeature(PackageManager.FEATURE_WATCH, true)
            try {
                ReflectionHelpers.setStaticField(AssistantReplication::class.java, "status", AssistantReplicationStatus.IDLE)
                val relayChannel = FakeGmsMailbox.FakeChannel(
                    "synthetic-watch-node",
                    PhoneRelayWire.PREFIX + UUID.randomUUID().toString(),
                    FakeGmsMailbox.InMemoryPipe(fixture.mailbox),
                    FakeGmsMailbox.InMemoryPipe(fixture.mailbox),
                    fixture.mailbox,
                )
                val closedBeforeRelay = fixture.mailbox.closedChannels
                service.onChannelOpened(relayChannel)
                assertEquals(AssistantReplicationStatus.IDLE, AssistantReplication.status)
                assertEquals(closedBeforeRelay, fixture.mailbox.closedChannels)
            } finally {
                shadowOf(RuntimeEnvironment.getApplication().packageManager).setSystemFeature(PackageManager.FEATURE_WATCH, false)
            }

            service.onDestroy()
        }
    }

    @Test fun coalescedHintsKeepLatestPayloadPerPeerWithoutFillingExecutorQueue() {
        val a = UUID(0, 10).toString()
        val b = UUID(0, 20).toString()
        val fixture = Fixture("synthetic-coalesced-hints")
        var controllersCreated = 0
        val owner = AssistantReplication.Owner(
            fixture.context,
            fixture.context.assistantPeerAssociationProvider,
            {
                controllersCreated++
                fixture.transport()
            },
            { false },
        ).also { testOwners += it }

        val entered = CountDownLatch(1)
        val release = CountDownLatch(1)
        val blocker = requireNotNull(owner.submit {
            entered.countDown()
            assertTrue(release.await(30, TimeUnit.SECONDS))
        })
        assertTrue(entered.await(30, TimeUnit.SECONDS))

        try {
            // Flood 50 hints for the same peer while Owner is blocked on the single worker thread.
            var lastBytes = ByteArray(0)
            repeat(50) { index ->
                lastBytes = ReplicationWire.encodeHint(b, a).let {
                    // Valid UTF-8 bytes differing per iteration
                    """{"version":3,"kind":"reconcile_hint","sender":"$b","target":"$a","seq":$index}""".toByteArray(Charsets.UTF_8)
                }
                owner.onHint("synthetic-watch-node", lastBytes)
            }
            val executor = ReflectionHelpers.getField<java.util.concurrent.ThreadPoolExecutor>(owner, "executor")
            // Only 1 drain runnable should be queued in the executor!
            assertEquals(1, executor.queue.size)
            val pendingHints = ReflectionHelpers.getField<LinkedHashMap<String, ByteArray>>(owner, "pendingHints")
            assertEquals(1, pendingHints.size)
            assertArrayEquals(lastBytes, pendingHints["synthetic-watch-node"])

            // Flooding distinct peers beyond MAX_PEERS (8) rejects the 9th peer without queueing.
            for (peerIdx in 1..ReplicationWire.MAX_PEERS) {
                owner.onHint("synthetic-watch-node-$peerIdx", lastBytes)
            }
            assertEquals(ReplicationWire.MAX_PEERS, pendingHints.size)
            ReflectionHelpers.setStaticField(AssistantReplication::class.java, "status", AssistantReplicationStatus.IDLE)
            owner.onHint("synthetic-watch-node-overflow", lastBytes)
            assertEquals(AssistantReplicationStatus.REJECTED, AssistantReplication.status)
            assertEquals(ReplicationWire.MAX_PEERS, pendingHints.size)
            assertEquals(1, executor.queue.size)
        } finally {
            release.countDown()
            blocker.get(30, TimeUnit.SECONDS)
            owner.submit {}!!.get(30, TimeUnit.SECONDS)
        }
    }

    private fun withBlockedOwner(owner: AssistantReplication.Owner, queueSize: Int, block: () -> Unit) {
        val entered = CountDownLatch(1)
        val release = CountDownLatch(1)
        val accepted = mutableListOf<java.util.concurrent.Future<*>>()
        try {
            accepted += requireNotNull(owner.submit { entered.countDown(); check(release.await(30, TimeUnit.SECONDS)) })
            assertTrue(entered.await(30, TimeUnit.SECONDS))
            repeat(queueSize) { accepted += requireNotNull(owner.submit {}) }
            block()
        } finally {
            release.countDown()
            accepted.forEach { it.get(30, TimeUnit.SECONDS) }
            requireNotNull(owner.submit {}).get(30, TimeUnit.SECONDS)
        }
    }

    private fun withOwner(owner: AssistantReplication.Owner?, block: () -> Unit) {
        val previous = ReflectionHelpers.getStaticField<AssistantReplication.Owner?>(AssistantReplication::class.java, "owner")
        previous?.submit {}?.get(30, TimeUnit.SECONDS)
        ReflectionHelpers.setStaticField(AssistantReplication::class.java, "owner", owner)
        try { block() } finally {
            val installed = ReflectionHelpers.getStaticField<AssistantReplication.Owner?>(AssistantReplication::class.java, "owner")
            ReflectionHelpers.setStaticField(AssistantReplication::class.java, "owner", previous)
            if (installed != null && installed !== previous) testOwners += installed
        }
    }

    private inner class Fixture(name: String) {
        val root = folder.newFolder(name)
        val mailbox = FakeGmsMailbox(folder.root)
        val localSdk = mailbox.sdk("synthetic-phone-node")
        fun transport() = GmsReplicationTransport(localSdk, BoundedGmsWait(System.nanoTime() + TimeUnit.SECONDS.toNanos(30), 50))
        val context = TestContext(root, ::transport)
    }

    private class TestContext(private val root: File, transport: () -> GmsReplicationTransport) :
        ContextWrapper(RuntimeEnvironment.getApplication()), AssistantPeerSetupOwner, AssistantPeerAssociationOwner {
        override fun getApplicationContext(): Context = this
        override fun getNoBackupFilesDir(): File = root
        override val assistantPeerSetup = AssistantPeerSetup(this, { AssistantStore(this).use { it.replica() } }, transport)
        override val assistantPeerAssociationProvider = PeerAssociationProvider(assistantPeerSetup::replicaForNode)
    }

    private fun fakeDataItem(uriString: String, bytes: ByteArray?): DataItem = object : DataItem {
        private var current = bytes
        override fun getUri(): Uri = Uri.parse(uriString)
        override fun getData(): ByteArray? = current
        override fun setData(data: ByteArray?): DataItem { current = data; return this }
        override fun getAssets(): Map<String, DataItemAsset> = emptyMap()
        override fun isDataValid(): Boolean = true
        override fun freeze(): DataItem = this
    }

    private fun fakeMessage(source: String, path: String, bytes: ByteArray): MessageEvent = object : MessageEvent {
        override fun getRequestId(): Int = 1
        override fun getPath(): String = path
        override fun getData(): ByteArray = bytes
        override fun getSourceNodeId(): String = source
    }
}
