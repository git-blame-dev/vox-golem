package dev.gitblame.voxgolem.assistant.replication

import com.google.android.gms.tasks.TaskCompletionSource
import org.json.JSONObject
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment
import org.robolectric.annotation.Config
import java.util.concurrent.Executors
import java.util.concurrent.TimeUnit
import java.util.concurrent.TimeoutException
import java.util.concurrent.atomic.AtomicInteger
import kotlin.random.Random

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [24, 35], manifest = Config.NONE)
class ReplicationBoundaryTest {
    private val peer = "00000000-0000-4000-8000-000000000002"

    @Test fun duplicateKeysMalformedUtf8DeepJsonAndOutOfRangeSequenceAreRejectedBeforeNative() {
        for (raw in listOf("{\"version\":1,\"version\":2}", "{\"nested\":{\"x\":1,\"x\":2}}", "[1]", "{} trailing")) {
            assertThrows(Exception::class.java) { ReplicationWire.decode(raw.toByteArray()) }
        }
        assertThrows(Exception::class.java) { ReplicationWire.decode(byteArrayOf(0x7b, 0xc0.toByte(), 0x7d)) }
        assertThrows(Exception::class.java) { ReplicationWire.decode(("{\"a\":".repeat(34) + "0" + "}".repeat(34)).toByteArray()) }
        assertThrows(Exception::class.java) { ReplicationWire.idKey(JSONObject().put("replica", peer).put("sequence", "18446744073709551616")) }
        assertThrows(Exception::class.java) { ReplicationWire.idKey(JSONObject().put("replica", peer).put("sequence", "01")) }
    }

    @Test fun enrollmentAndPendingRetrySurviveReopenButNeverEnrollDiscoveryOrPersistRoutes() {
        val context = RuntimeEnvironment.getApplication()
        EnrolledPeerRegistry(context).use { registry ->
            assertTrue(registry.replicas().isEmpty())
            registry.enroll(peer)
        }
        DurableReplicationState(context).use { state ->
            assertEquals(setOf(peer), state.enrolledReplicas())
            state.save(peer, JSONObject().put("session", "Synthetic session metadata"))
            val delays = (0..20).map { state.failed(1_000_000, Random(it)) }
            assertTrue(delays.all { it in 30_000..DurableReplicationState.MAX_BACKOFF })
        }
        DurableReplicationState(context).use { state ->
            assertEquals("Synthetic session metadata", state.load(peer)!!.getString("session"))
            assertTrue(state.retryDelay(1_000_000) in 30_000..DurableReplicationState.MAX_BACKOFF)
        }
        EnrolledPeerRegistry(context).use { it.revoke(peer) }
        DurableReplicationState(context).use { assertNull(it.load(peer)); assertTrue(it.enrolledReplicas().isEmpty()) }
    }

    @Test fun taskTimeoutDoesNotLeakLateResourcesAndNeverBecomesUnboundedAwait() {
        val task = TaskCompletionSource<String>()
        val cleanup = AtomicInteger()
        val executor = Executors.newSingleThreadExecutor()
        try {
            val result = executor.submit<Boolean> {
                assertThrows(TimeoutException::class.java) {
                    BoundedGmsWait(System.nanoTime() + TimeUnit.SECONDS.toNanos(1), 10).await(task.task) { cleanup.incrementAndGet() }
                }
                true
            }
            assertTrue(result.get(2, TimeUnit.SECONDS))
            task.setResult("Synthetic late resource")
            assertEquals(1, cleanup.get())
        } finally { executor.shutdownNow() }
    }

    @Test fun repeatedOfflineRevocationCannotCreateUnboundedDataItemCleanupBacklog() {
        val context = RuntimeEnvironment.getApplication()
        val identities = (1..9).map { java.util.UUID(0, it.toLong()).toString() }
        EnrolledPeerRegistry(context).use { registry ->
            for (replica in identities.take(8)) { registry.enroll(replica); registry.revoke(replica) }
            assertThrows(IllegalArgumentException::class.java) { registry.enroll(identities.last()) }
        }
        DurableReplicationState(context).use { state ->
            assertEquals(identities.take(8).toSet(), state.cleanupReplicas())
            state.cleanupComplete(identities.first())
        }
        EnrolledPeerRegistry(context).use { it.enroll(identities.last()) }
    }
}
