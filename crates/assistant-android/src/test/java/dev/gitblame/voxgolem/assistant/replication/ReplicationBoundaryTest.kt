package dev.gitblame.voxgolem.assistant.replication

import com.google.android.gms.tasks.TaskCompletionSource
import org.json.JSONArray
import org.json.JSONObject
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment
import org.robolectric.annotation.Config
import java.io.EOFException
import java.util.ArrayDeque
import java.util.UUID
import java.util.concurrent.Executors
import java.util.concurrent.TimeUnit
import java.util.concurrent.TimeoutException
import java.util.concurrent.atomic.AtomicInteger
import kotlin.random.Random

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [24, 35], manifest = Config.NONE)
class ReplicationBoundaryTest {
    private val local = "00000000-0000-4000-8000-000000000001"
    private val peer = "00000000-0000-4000-8000-000000000002"
    private val session = "00000000-0000-4000-8000-00000000001a"
    private val token = "00000000-0000-4000-8000-00000000002b"
    private val transfer = "00000000-0000-4000-8000-00000000003c"

    @Test fun duplicateKeysMalformedUtf8DeepJsonAndOutOfRangeSequenceAreRejectedBeforeNative() {
        for (raw in listOf("{\"version\":1,\"version\":2}", "{\"nested\":{\"x\":1,\"x\":2}}", "[1]", "{} trailing")) {
            assertThrows(Exception::class.java) { ReplicationWire.decode(raw.toByteArray()) }
        }
        assertThrows(Exception::class.java) { ReplicationWire.decode(byteArrayOf(0x7b, 0xc0.toByte(), 0x7d)) }
        assertThrows(Exception::class.java) { ReplicationWire.decode(("{\"a\":".repeat(34) + "0" + "}".repeat(34)).toByteArray()) }
        assertThrows(Exception::class.java) { ReplicationWire.idKey(JSONObject().put("replica", peer).put("sequence", "18446744073709551616")) }
        assertThrows(Exception::class.java) { ReplicationWire.idKey(JSONObject().put("replica", peer).put("sequence", "01")) }
    }

    @Test fun coalescedHintAndSessionPathValidationEnforceBoundsAndIdentity() {
        val encoded = ReplicationWire.encodeHint(peer, local)
        assertTrue(encoded.size <= ReplicationWire.MAX_HINT_BYTES)
        ReplicationWire.validateHint(encoded, local, peer)

        // Oversized (> 256 bytes), wrong version, wrong kind, extra field, or mismatched sender/target must reject.
        val oversized = ("{\"version\":3,\"kind\":\"reconcile_hint\",\"sender\":\"$peer\",\"target\":\"$local\",\"pad\":\"" + "x".repeat(260) + "\"}").toByteArray()
        assertTrue(oversized.size > ReplicationWire.MAX_HINT_BYTES)
        assertThrows(IllegalArgumentException::class.java) { ReplicationWire.validateHint(oversized, local, peer) }
        assertThrows(IllegalArgumentException::class.java) {
            ReplicationWire.validateHint(
                JSONObject().put("version", 2).put("kind", "reconcile_hint").put("sender", peer).put("target", local).toString().toByteArray(),
                local,
                peer,
            )
        }
        assertThrows(IllegalArgumentException::class.java) {
            ReplicationWire.validateHint(
                JSONObject().put("version", 3).put("kind", "hint").put("sender", peer).put("target", local).toString().toByteArray(),
                local,
                peer,
            )
        }
        assertThrows(IllegalArgumentException::class.java) {
            ReplicationWire.validateHint(
                JSONObject().put("version", 3).put("kind", "reconcile_hint").put("sender", peer).put("target", local).put("extra", 1).toString().toByteArray(),
                local,
                peer,
            )
        }
        assertThrows(IllegalArgumentException::class.java) { ReplicationWire.validateHint(encoded, peer, peer) }
        assertThrows(IllegalArgumentException::class.java) { ReplicationWire.validateHint(encoded, local, local) }

        val validPath = ReplicationWire.sessionPath(session)
        assertEquals("/voxgolem/assistant/v2/session/$session", validPath)
        assertEquals(session, ReplicationWire.parseSessionPath(validPath))
        assertTrue(ReplicationWire.validSessionPath(validPath))
        for (invalid in listOf(
            "/voxgolem/assistant/v1/session/$session",
            "/voxgolem/assistant/v2/session/",
            "/voxgolem/assistant/v2/session/not-a-uuid",
            "/voxgolem/assistant/v2/session/${session.uppercase()}",
            "/voxgolem/assistant/v2/session/$session/extra",
            ReplicationWire.HINT_PATH,
        )) {
            assertNull(ReplicationWire.parseSessionPath(invalid))
            assertFalse(ReplicationWire.validSessionPath(invalid))
        }
        assertThrows(IllegalArgumentException::class.java) { ReplicationWire.sessionPath("not-a-uuid") }
    }

    @Test fun multiChunkRecordBatchAssembliesAndEnforcesExplicitBatchEndAndWireBounds() {
        val connection = FakeMemorySessionConnection(session)
        // Build a synthetic record > 64 KiB so it spans multiple <= 64 KiB chunks.
        val largeRecord = syntheticRecord(1, "s".repeat(150_000))
        val largeBytes = largeRecord.toString().toByteArray(Charsets.UTF_8)
        assertTrue(largeBytes.size > ReplicationWire.MAX_CHUNK_BYTES * 2)
        val largeEntry = ReplicationWire.descriptor(largeRecord)
        val unavailableId = JSONObject().put("replica", peer).put("sequence", "2")
        val unavailableRequest = JSONObject().put("id", unavailableId).put("body_key", JSONObject.NULL)

        ReplicationWire.writeRecordBatch(
            connection = connection,
            sender = peer,
            target = local,
            session = session,
            token = token,
            transfer = transfer,
            bodies = listOf(PreparedRecordTransfer(largeEntry, largeBytes)),
            unavailable = JSONArray().put(unavailableId),
        )
        assertTrue(connection.writtenChunks.size >= 3)
        assertTrue(connection.writtenChunks.all { it.size in 1..ReplicationWire.MAX_CHUNK_BYTES })
        assertEquals("batch_end", connection.writtenFrames.last().getString("kind"))

        val recordsFrame = connection.readFrame()
        val expected = JSONArray().put(largeEntry).put(unavailableRequest)
        val batch = ReplicationWire.readRecordBatch(
            connection = connection,
            recordsFrame = recordsFrame,
            session = session,
            local = local,
            peer = peer,
            expectedEntries = expected,
            phase = "records",
        )
        assertEquals(1, batch.records.length())
        assertEquals(largeRecord.toString(), batch.records.getJSONObject(0).toString())
        assertEquals(1, batch.verifiedEntries.length())
        assertEquals(1, batch.ids.length())
        assertEquals(1, batch.unavailable.length())
        assertTrue(connection.frames.isEmpty() && connection.chunks.isEmpty())

        // Truncated stream before batch_end must reject and never return assembled records.
        val truncated = FakeMemorySessionConnection(session)
        ReplicationWire.writeRecordBatch(
            connection = truncated,
            sender = peer,
            target = local,
            session = session,
            token = token,
            transfer = transfer,
            bodies = listOf(PreparedRecordTransfer(largeEntry, largeBytes)),
            unavailable = JSONArray(),
        )
        truncated.frames.removeLast() // Drop batch_end frame.
        val truncatedRecordsFrame = truncated.readFrame()
        assertThrows(EOFException::class.java) {
            ReplicationWire.readRecordBatch(
                connection = truncated,
                recordsFrame = truncatedRecordsFrame,
                session = session,
                local = local,
                peer = peer,
                expectedEntries = JSONArray().put(largeEntry),
                phase = "records",
            )
        }

        // Mismatched batch_end byte total must reject even after all chunks are read.
        val mismatchedEnd = FakeMemorySessionConnection(session)
        ReplicationWire.writeRecordBatch(
            connection = mismatchedEnd,
            sender = peer,
            target = local,
            session = session,
            token = token,
            transfer = transfer,
            bodies = listOf(PreparedRecordTransfer(largeEntry, largeBytes)),
            unavailable = JSONArray(),
        )
        mismatchedEnd.frames.removeLast()
        mismatchedEnd.writeFrame(
            ReplicationWire.sessionFrame(
                peer,
                local,
                session,
                token,
                "batch_end",
                JSONObject().put("transfer", transfer).put("records", 1).put("bytes", largeBytes.size - 1),
            )
        )
        val mismatchedRecordsFrame = mismatchedEnd.readFrame()
        assertThrows(IllegalArgumentException::class.java) {
            ReplicationWire.readRecordBatch(
                connection = mismatchedEnd,
                recordsFrame = mismatchedRecordsFrame,
                session = session,
                local = local,
                peer = peer,
                expectedEntries = JSONArray().put(largeEntry),
                phase = "records",
            )
        }
    }

    @Test fun controlFrameChunkRecordBatchCountAndIdInvariantsRejectOutOfBoundsPayloads() {
        val smallRecord = syntheticRecord(1, "Synthetic small conversation")
        val smallBytes = smallRecord.toString().toByteArray(Charsets.UTF_8)
        val smallEntry = ReplicationWire.descriptor(smallRecord)

        // Oversized control frame (> 32 KiB) rejects on encode and decode.
        val hugeControl = ReplicationWire.sessionFrame(
            peer,
            local,
            session,
            token,
            "inventory",
            JSONObject().put("pad", "x".repeat(ReplicationWire.MAX_CONTROL_BYTES)),
        )
        assertThrows(IllegalArgumentException::class.java) { ReplicationWire.encode(hugeControl) }
        assertThrows(IllegalArgumentException::class.java) {
            ReplicationWire.decode(hugeControl.toString().toByteArray(Charsets.UTF_8))
        }

        // > 8 records per batch rejects on writeRecordBatch and readRecordBatch.
        val nineTransfers = (1..9).map { seq ->
            val rec = syntheticRecord(seq, "Synthetic record $seq")
            PreparedRecordTransfer(ReplicationWire.descriptor(rec), rec.toString().toByteArray(Charsets.UTF_8))
        }
        assertThrows(IllegalArgumentException::class.java) {
            ReplicationWire.writeRecordBatch(
                FakeMemorySessionConnection(session),
                peer,
                local,
                session,
                token,
                transfer,
                nineTransfers,
                JSONArray(),
            )
        }

        // Oversized single record (> 2 MiB) rejects.
        assertThrows(IllegalArgumentException::class.java) {
            ReplicationWire.writeRecordBatch(
                FakeMemorySessionConnection(session),
                peer,
                local,
                session,
                token,
                transfer,
                listOf(PreparedRecordTransfer(smallEntry, ByteArray(ReplicationWire.MAX_RECORD_BYTES + 1))),
                JSONArray(),
            )
        }

        // Oversized batch (> 4 MiB across <= 8 records each <= 2 MiB) rejects on both write and read.
        val threeLarge = (1..3).map { seq ->
            val rec = syntheticRecord(seq, "Synthetic batch limit $seq")
            PreparedRecordTransfer(ReplicationWire.descriptor(rec), ByteArray(1_500_000))
        }
        assertThrows(IllegalArgumentException::class.java) {
            ReplicationWire.writeRecordBatch(
                FakeMemorySessionConnection(session),
                peer,
                local,
                session,
                token,
                transfer,
                threeLarge,
                JSONArray(),
            )
        }
        val oversizedBatchFrame = ReplicationWire.sessionFrame(
            peer,
            local,
            session,
            token,
            "records",
            JSONObject()
                .put("transfer", transfer)
                .put("records", JSONArray(threeLarge.map { JSONObject().put("entry", it.entry).put("bytes", 1_500_000) }))
                .put("unavailable", JSONArray()),
        )
        assertThrows(IllegalArgumentException::class.java) {
            ReplicationWire.readRecordBatch(
                FakeMemorySessionConnection(session),
                oversizedBatchFrame,
                session,
                local,
                peer,
                JSONArray(threeLarge.map { it.entry }),
                "records",
            )
        }

        // Oversized chunk (> 64 KiB) in body_chunk control frame rejects on readRecordBatch.
        val oversizedChunkRecord = syntheticRecord(1, "y".repeat(70_000))
        val oversizedChunkBytes = oversizedChunkRecord.toString().toByteArray(Charsets.UTF_8)
        val oversizedChunkEntry = ReplicationWire.descriptor(oversizedChunkRecord)
        val badChunkConn = FakeMemorySessionConnection(session)
        badChunkConn.writeFrame(
            ReplicationWire.sessionFrame(
                peer,
                local,
                session,
                token,
                "records",
                JSONObject()
                    .put("transfer", transfer)
                    .put("records", JSONArray().put(JSONObject().put("entry", oversizedChunkEntry).put("bytes", oversizedChunkBytes.size)))
                    .put("unavailable", JSONArray()),
            )
        )
        badChunkConn.writeFrame(
            ReplicationWire.sessionFrame(
                peer,
                local,
                session,
                token,
                "body_chunk",
                JSONObject().put("transfer", transfer).put("index", 0).put("bytes", ReplicationWire.MAX_CHUNK_BYTES + 1),
            )
        )
        val badChunkStart = badChunkConn.readFrame()
        assertThrows(IllegalArgumentException::class.java) {
            ReplicationWire.readRecordBatch(
                badChunkConn,
                badChunkStart,
                session,
                local,
                peer,
                JSONArray().put(oversizedChunkEntry),
                "records",
            )
        }

        // Unrequested or duplicate record IDs reject on readRecordBatch.
        val unrequestedRecord = syntheticRecord(99, "Synthetic unrequested")
        val unrequestedEntry = ReplicationWire.descriptor(unrequestedRecord)
        val unrequestedConn = FakeMemorySessionConnection(session)
        ReplicationWire.writeRecordBatch(
            unrequestedConn,
            peer,
            local,
            session,
            token,
            transfer,
            listOf(PreparedRecordTransfer(unrequestedEntry, unrequestedRecord.toString().toByteArray(Charsets.UTF_8))),
            JSONArray(),
        )
        val unrequestedFrame = unrequestedConn.readFrame()
        assertThrows(IllegalStateException::class.java) {
            ReplicationWire.readRecordBatch(
                unrequestedConn,
                unrequestedFrame,
                session,
                local,
                peer,
                JSONArray().put(smallEntry),
                "records",
            )
        }

        val duplicateFrame = ReplicationWire.sessionFrame(
            peer,
            local,
            session,
            token,
            "records",
            JSONObject()
                .put("transfer", transfer)
                .put("records", JSONArray().put(JSONObject().put("entry", smallEntry).put("bytes", smallBytes.size)))
                .put("unavailable", JSONArray().put(smallEntry.getJSONObject("id"))),
        )
        assertThrows(IllegalArgumentException::class.java) {
            ReplicationWire.readRecordBatch(
                FakeMemorySessionConnection(session),
                duplicateFrame,
                session,
                local,
                peer,
                JSONArray().put(smallEntry),
                "records",
            )
        }
    }

    @Test fun enrollmentAndPendingRetrySurviveReopenButNeverEnrollDiscoveryOrPersistRoutes() {
        val context = RuntimeEnvironment.getApplication()
        EnrolledPeerRegistry(context).use { registry ->
            assertTrue(registry.replicas().isEmpty())
            registry.enroll(peer)
        }
        DurableReplicationState(context).use { state ->
            assertEquals(setOf(peer), state.enrolledReplicas())
            val delays = (0..20).map { state.failed(1_000_000, Random(it)) }
            assertTrue(delays.all { it in 30_000..DurableReplicationState.MAX_BACKOFF })
        }
        DurableReplicationState(context).use { state ->
            assertEquals(setOf(peer), state.enrolledReplicas())
            assertTrue(state.retryDelay(1_000_000) in 30_000..DurableReplicationState.MAX_BACKOFF)
        }
        EnrolledPeerRegistry(context).use { it.revoke(peer) }
        DurableReplicationState(context).use { assertTrue(it.enrolledReplicas().isEmpty()) }
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
        val identities = (1..10).map { UUID(0, it.toLong()).toString() }
        EnrolledPeerRegistry(context).use { registry ->
            for (replica in identities) {
                registry.enroll(replica)
                registry.revoke(replica)
            }
            assertTrue(registry.replicas().isEmpty())
            for (replica in identities.take(ReplicationWire.MAX_PEERS)) {
                registry.enroll(replica)
            }
            assertEquals(identities.take(ReplicationWire.MAX_PEERS).toSet(), registry.replicas())
            assertThrows(IllegalArgumentException::class.java) { registry.enroll(identities.last()) }
            registry.revoke(identities.first())
            registry.enroll(identities[ReplicationWire.MAX_PEERS])
        }
        DurableReplicationState(context).use { state ->
            assertEquals(ReplicationWire.MAX_PEERS, state.enrolledReplicas().size)
            assertTrue(identities[ReplicationWire.MAX_PEERS] in state.enrolledReplicas())
        }
    }

    private fun syntheticRecord(sequence: Int, title: String): JSONObject =
        JSONObject()
            .put("body_key", sequence.toString().padStart(64, '0'))
            .put(
                "operation",
                JSONObject()
                    .put("version", 2)
                    .put("id", JSONObject().put("replica", peer).put("sequence", sequence.toString()))
                    .put("body", JSONObject().put("kind", "conversation_created").put("title", title)),
            )

    private class FakeMemorySessionConnection(
        override val session: String,
        override val nodeId: String = "synthetic-peer-node",
    ) : ReplicationSessionConnection {
        override val path: String = ReplicationWire.sessionPath(session)
        val frames = ArrayDeque<JSONObject>()
        val chunks = ArrayDeque<ByteArray>()
        val writtenFrames = mutableListOf<JSONObject>()
        val writtenChunks = mutableListOf<ByteArray>()

        override fun readFrame(): JSONObject =
            if (frames.isEmpty()) throw EOFException("No frame available") else frames.removeFirst()

        override fun writeFrame(frame: JSONObject) {
            val decoded = ReplicationWire.decode(ReplicationWire.encode(frame))
            frames.addLast(decoded)
            writtenFrames.add(decoded)
        }

        override fun readBytes(length: Int): ByteArray {
            require(length in 1..ReplicationWire.MAX_CHUNK_BYTES)
            val next = if (chunks.isEmpty()) throw EOFException("No chunk available") else chunks.removeFirst()
            require(next.size == length)
            return next
        }

        override fun writeBytes(bytes: ByteArray, offset: Int, length: Int) {
            require(length in 1..ReplicationWire.MAX_CHUNK_BYTES)
            val slice = bytes.copyOfRange(offset, offset + length)
            chunks.addLast(slice)
            writtenChunks.add(slice)
        }

        override fun close() = Unit
    }
}
