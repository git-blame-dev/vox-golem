package dev.gitblame.voxgolem.assistant.replication

import android.app.job.JobScheduler
import android.content.Context
import android.content.ContextWrapper
import dev.gitblame.voxgolem.assistant.AssistantStoreRunner
import dev.gitblame.voxgolem.assistant.AssistantStore
import dev.gitblame.voxgolem.assistant.runtime.ChatGptAuth
import org.json.JSONObject
import org.junit.Assert.*
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TemporaryFolder
import org.junit.runner.RunWith
import org.robolectric.RuntimeEnvironment
import org.robolectric.annotation.Config
import org.robolectric.util.ReflectionHelpers
import java.io.File
import java.util.UUID
import java.util.concurrent.CompletableFuture
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit

@RunWith(AssistantStoreRunner::class)
@Config(sdk = [24, 35], manifest = Config.NONE)
class ReplicationStatusReadTest {
    @get:Rule val folder = TemporaryFolder()
    private val testOwners = mutableSetOf<AssistantReplication.Owner>()

    @org.junit.After fun retireTestOwners() {
        val executors = testOwners.map { ReflectionHelpers.getField<java.util.concurrent.ThreadPoolExecutor>(it, "executor") }
        val failures = mutableListOf<Throwable>()
        var interruption: InterruptedException? = null
        for (executor in executors) {
            try { executor.shutdown() } catch (error: Exception) { failures += error }
        }
        for (executor in executors) {
            try {
                if (executor.awaitTermination(30, TimeUnit.SECONDS)) continue
                failures += AssertionError("Replication work must finish before Robolectric tears down native SQLite")
            } catch (error: Exception) {
                failures += error
                if (error is InterruptedException) interruption = error
            }
            try {
                executor.shutdownNow()
                if (!executor.awaitTermination(30, TimeUnit.SECONDS)) {
                    failures += AssertionError("Replication executor did not terminate after forced shutdown")
                }
            } catch (error: Exception) {
                failures += error
                if (error is InterruptedException) interruption = error
            }
        }
        interruption?.let { error ->
            failures.filter { it !== error }.forEach(error::addSuppressed)
            Thread.currentThread().interrupt()
            throw error
        }
        if (failures.isNotEmpty()) {
            val failure = AssertionError("Replication owner cleanup failed")
            failures.forEach(failure::addSuppressed)
            throw failure
        }
    }

    @Test fun startupSchedulingAndPersistedBackoffWaitForExistingExecutorEvenWhenQueueIsFull() {
        for (fullQueue in listOf(false, true)) {
            val fixture = Fixture("synthetic-deferred-startup-$fullQueue")
            fixture.scheduler.cancelAll()
            val owner = AssistantReplication.Owner(fixture.context, fixture.context.assistantPeerAssociationProvider,
                fixture::transport, { false }).also { testOwners += it }
            // Seed a persisted retry before blocking the process owner.
            owner.state.failed(System.currentTimeMillis())
            withBlockedOwner(owner, if (fullQueue) 16 else 0) {
                withOwner(owner) {
                    val jobsBefore = fixture.scheduler.allPendingJobs.toList()
                    AssistantReplication.start(fixture.context, fixture.context.assistantPeerAssociationProvider)
                    assertEquals(jobsBefore, fixture.scheduler.allPendingJobs)
                    assertTrue(fixture.localSdk.calls.isEmpty())
                }
            }
            assertNotNull(fixture.scheduler.getPendingJob(AssistantReplication.PERIODIC_JOB))
            assertNotNull(fixture.scheduler.getPendingJob(AssistantReplication.RETRY_JOB))
            owner.state.close()
        }
    }

    @Test fun pollingCachedStatusDoesNoSdkOrJobWorkWhileStartupAndIdentityCallbacksEnrollAutonomously() {
        assertNotNull(System.getProperty("assistant.native.library"))
        val fixture = Fixture("synthetic-status-startup")
        val owner = AssistantReplication.Owner(fixture.context, fixture.context.assistantPeerAssociationProvider,
            fixture::transport, { true }).also { testOwners += it }
        withOwner(owner) {
            val credentialFile = ChatGptAuth.managedFile(fixture.context).apply { writeText("Synthetic managed credential bytes") }
            val credentialBefore = credentialFile.readBytes()
            AssistantReplication.start(fixture.context, fixture.context.assistantPeerAssociationProvider)
            owner.submit {}!!.get(30, TimeUnit.SECONDS)
            assertEquals(PeerPairingReason.WAITING_FOR_PEER, fixture.context.assistantPeerSetup.state().reason)
            assertNotNull(fixture.scheduler.getPendingJob(AssistantReplication.RETRY_JOB))
            val callsBefore = fixture.localSdk.calls.toList()
            val jobsBefore = fixture.scheduler.allPendingJobs.toList()
            val now = System.currentTimeMillis()
            val retryBefore = owner.state.retryDelay(now)
            repeat(20) { assertEquals(PeerPairingReason.WAITING_FOR_PEER, readStatus(fixture.context).reason) }
            owner.submit {}!!.get(30, TimeUnit.SECONDS)
            assertEquals("Reading status must not query, publish or clean up SDK data", callsBefore, fixture.localSdk.calls)
            assertEquals(jobsBefore, fixture.scheduler.allPendingJobs)
            assertEquals(retryBefore, owner.state.retryDelay(now))
            assertArrayEquals(credentialBefore, credentialFile.readBytes())

            owner.submit {
                GmsReplicationTransport(fixture.remoteSdk, budget()).publishIdentity(UUID(0, 501).toString(), AssistantPeerRole.WATCH)
            }!!.get(30, TimeUnit.SECONDS)
            AssistantReplication.onData(fixture.context, fixture.mailbox.events.last())
            owner.submit {}!!.get(30, TimeUnit.SECONDS)
            assertEquals(PeerPairingReason.ENROLLED, fixture.context.assistantPeerSetup.state().reason)
            assertTrue(fixture.localSdk.calls.size > callsBefore.size)
            val enrolledCalls = fixture.localSdk.calls.toList()
            val enrolledJobs = fixture.scheduler.allPendingJobs.toList()
            val pendingSync = owner.state.load(UUID(0, 501).toString())!!.toString()
            repeat(20) {
                val cached = readStatus(fixture.context)
                assertEquals(PeerPairingReason.ENROLLED, cached.reason)
                assertFalse(cached.credentialsAllowed)
                assertNotNull(cached.associationId)
            }
            owner.submit {}!!.get(30, TimeUnit.SECONDS)
            assertEquals(enrolledCalls, fixture.localSdk.calls)
            assertEquals(enrolledJobs, fixture.scheduler.allPendingJobs)
            assertArrayEquals(credentialBefore, credentialFile.readBytes())
            assertEquals(pendingSync, owner.state.load(UUID(0, 501).toString())!!.toString())
        }
    }

    @Test fun coldStatusResolutionPreservesCachedDiagnosticWithoutSchedulingBootstrapJobs() {
        val fixture = Fixture("synthetic-status-cold")
        EnrolledPeerRegistry(fixture.context).use {
            it.saveSetup("status", JSONObject().put("reason", "DISCOVERY_FAILED").put("credentials", false))
        }
        withOwner(null) {
            val jobsBefore = fixture.scheduler.allPendingJobs.toList()
            repeat(5) { assertEquals(PeerPairingReason.DISCOVERY_FAILED, readStatus(fixture.context).reason) }
            assertTrue(fixture.localSdk.calls.isEmpty())
            assertEquals(jobsBefore, fixture.scheduler.allPendingJobs)
        }
    }

    @Test fun rejectedStatusReadReturnsUnavailableWithoutSchedulingRetryOrInvokingSdk() {
        val fixture = Fixture("synthetic-status-busy")
        val owner = AssistantReplication.Owner(fixture.context, fixture.context.assistantPeerAssociationProvider,
            fixture::transport, { true }).also { testOwners += it }
        withBlockedOwner(owner, 16) {
            withOwner(owner) {
                val jobsBefore = fixture.scheduler.allPendingJobs.toList()
                assertEquals(PeerPairingReason.UNAVAILABLE, readStatus(fixture.context).reason)
                assertTrue(fixture.localSdk.calls.isEmpty())
                assertEquals(jobsBefore, fixture.scheduler.allPendingJobs)
            }
        }
    }

    @Test fun invalidCachedDiagnosticReturnsStorageFailureWithoutMutatingOrRetrying() {
        val fixture = Fixture("synthetic-status-invalid")
        EnrolledPeerRegistry(fixture.context).use {
            it.saveSetup("status", JSONObject().put("reason", "SYNTHETIC_INVALID_REASON"))
        }
        val owner = AssistantReplication.Owner(fixture.context, fixture.context.assistantPeerAssociationProvider,
            fixture::transport, { true }).also { testOwners += it }
        withOwner(owner) {
            val jobsBefore = fixture.scheduler.allPendingJobs.toList()
            assertEquals(PeerPairingReason.STORAGE_FAILURE, readStatus(fixture.context).reason)
            assertTrue(fixture.localSdk.calls.isEmpty())
            assertEquals(jobsBefore, fixture.scheduler.allPendingJobs)
            EnrolledPeerRegistry(fixture.context).use { assertEquals("SYNTHETIC_INVALID_REASON", it.setupValue("status")!!.getString("reason")) }
        }
    }

    @Test fun setupFailureBeforeOwnerSwapReleasesGatedWorkAndRetiresActualExecutor() {
        val fixture = Fixture("synthetic-setup-failure")
        val owner = AssistantReplication.Owner(fixture.context, fixture.context.assistantPeerAssociationProvider,
            fixture::transport, { false }).also { testOwners += it }
        val previous = ReflectionHelpers.getStaticField<AssistantReplication.Owner?>(AssistantReplication::class.java, "owner")
        val injected = IllegalStateException("Synthetic failure before owner swap")
        try {
            withBlockedOwner(owner, 0) { throw injected }
            fail("Expected injected setup failure")
        } catch (error: IllegalStateException) {
            assertSame(injected, error)
        }
        assertSame(previous, ReflectionHelpers.getStaticField<AssistantReplication.Owner?>(AssistantReplication::class.java, "owner"))
        retireTestOwners()
        val executor = ReflectionHelpers.getField<java.util.concurrent.ThreadPoolExecutor>(owner, "executor")
        assertTrue("The actual owner executor must be retired after failed setup", executor.isTerminated)
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

    private inner class Fixture(name: String) {
        val root = folder.newFolder(name)
        val mailbox = FakeGmsMailbox(folder.root)
        val localSdk = mailbox.sdk("synthetic-status-phone")
        val remoteSdk = mailbox.sdk("synthetic-status-watch")
        fun transport() = GmsReplicationTransport(localSdk, budget())
        val context = StatusContext(root, ::transport)
        val scheduler = context.getSystemService(Context.JOB_SCHEDULER_SERVICE) as JobScheduler
    }

    private class StatusContext(private val root: File, transport: () -> GmsReplicationTransport) :
        ContextWrapper(RuntimeEnvironment.getApplication()), AssistantPeerSetupOwner, AssistantPeerAssociationOwner {
        override fun getApplicationContext(): Context = this
        override fun getNoBackupFilesDir(): File = root
        override val assistantPeerSetup = AssistantPeerSetup(this, { AssistantStore(this).use { it.replica() } }, transport)
        override val assistantPeerAssociationProvider = PeerAssociationProvider(assistantPeerSetup::replicaForNode)
    }

    private fun readStatus(context: Context): PeerPairingState {
        val response = CompletableFuture<PeerPairingState>()
        AssistantReplication.pairingState(context) { response.complete(it) }
        return response.get(30, TimeUnit.SECONDS)
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
    private fun budget() = BoundedGmsWait(System.nanoTime() + TimeUnit.SECONDS.toNanos(30))
}
