package dev.gitblame.voxgolem.capture

import org.junit.Assert.*
import org.junit.Test
import org.junit.Before
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment
import org.robolectric.annotation.Config
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35])
class CapturePhoneOriginTest {
    private val context = RuntimeEnvironment.getApplication()
    private val id = "123e4567-e89b-42d3-a456-426614174060"
    @Before fun setUp() { context.deleteDatabase("captures.db") }
    private fun record() = CaptureRecord(id, createdAtUtc = "2026-10-01T00:00:00Z")

    @Test fun originAndCaptureAdmissionAreAtomicAcrossRepositoryInstancesAndColdReopen() {
        assertNull(CaptureRepository(context).get(id))
        val begin = CountDownLatch(1)
        val results = java.util.concurrent.ConcurrentHashMap<String, CaptureRecord>()
        val failures = java.util.concurrent.CopyOnWriteArrayList<Throwable>()
        val workers = listOf("synthetic-watch-a", "synthetic-watch-b").map { node -> Thread {
            try {
                check(begin.await(2, TimeUnit.SECONDS))
                CaptureRepository(context).admitPhoneCapture(record(), node)?.let { results[node] = it }
            } catch (failure: Throwable) { failures += failure }
        } }
        workers.forEach { it.start() }; begin.countDown(); workers.forEach { it.join(5_000) }
        assertTrue(workers.none { it.isAlive }); assertTrue(failures.isEmpty())
        assertEquals(1, results.size)
        val cold = CaptureRepository(context)
        assertEquals(results.keys.single(), cold.phoneOrigin(id))
        assertEquals(record(), cold.get(id))
        assertNull(cold.admitPhoneCapture(record(), "synthetic-reinstalled-node"))
        assertEquals(results.keys.single(), CaptureRepository(context).phoneOrigin(id))
        context.openOrCreateDatabase("captures.db", 0, null).use { db ->
            db.rawQuery("SELECT body FROM captures WHERE capture_id = ?", arrayOf(id)).use {
                assertTrue(it.moveToFirst()); assertFalse(it.getString(0).contains("synthetic-watch"))
                assertFalse(it.getString(0).contains("source_node_id"))
            }
        }
    }
    @Test fun bindingFailureRollsBackCaptureAndLegacyRecordsCannotAcquireAnOrigin() {
        val repository = CaptureRepository(context)
        assertNull(repository.phoneOrigin(id))
        context.openOrCreateDatabase("captures.db", 0, null).use { db ->
            db.execSQL("CREATE TRIGGER synthetic_origin_failure BEFORE INSERT ON phone_capture_origins BEGIN SELECT RAISE(ABORT, 'synthetic origin failure'); END")
            assertThrows(android.database.sqlite.SQLiteException::class.java) { repository.admitPhoneCapture(record(), "synthetic-watch-a") }
            assertNull(repository.get(id)); assertNull(repository.phoneOrigin(id))
            db.execSQL("DROP TRIGGER synthetic_origin_failure")
        }
        repository.save(record())
        assertNull(repository.admitPhoneCapture(record(), "synthetic-watch-a"))
        assertNull(repository.admitPhoneCapture(record(), "synthetic-watch-b"))
        assertNull(CaptureRepository(context).phoneOrigin(id))
        assertEquals(record(), repository.get(id))
    }
}
