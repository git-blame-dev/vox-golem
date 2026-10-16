package dev.gitblame.voxgolem.assistant.runtime

import org.json.JSONObject
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import java.util.ArrayDeque
import java.util.concurrent.Executor
import java.util.concurrent.RejectedExecutionException

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [24, 35], manifest = Config.NONE)
class ChatGptAuthTest {
    private class Fixture {
        data class Scheduled(val work: Runnable, val due: Long, var removed: Boolean = false)
        var now = 0L
        val queue = ArrayDeque<Runnable>()
        val scheduled = mutableListOf<Scheduled>()
        val calls = mutableListOf<String>()
        val events = mutableListOf<String>()
        var changes = 0
        var rejectNext = false
        var respond: (String) -> String = { """{"phase":"pending","userCode":"TEST-0001"}""" }
        val owner = ChatGptAuth.Owner(Executor {
            if (rejectNext) { rejectNext = false; throw RejectedExecutionException("synthetic_queue_full") }
            queue.add(it)
        }, { action ->
            calls.add(action); events.add(action); respond(action)
        }, { work, delay ->
            val entry = Scheduled(work, now + delay).also(scheduled::add)
            val remove: () -> Unit = { entry.removed = true }
            remove
        }, { now }, { changes++ }, { events.add("stop_assistant") })
        fun drain() { while (queue.isNotEmpty()) queue.removeFirst().run() }
        fun live() = scheduled.filterNot { it.removed }
        fun tick() {
            val entry = live().single()
            entry.removed = true
            now = maxOf(now, entry.due)
            entry.work.run()
            drain()
        }
        fun command(action: String): MutableList<JSONObject> = mutableListOf<JSONObject>().also { results ->
            owner.command(action, results::add)
        }
    }

    @Test fun nativeCompletionRefreshesAndSyncsWithoutAnotherUiReadOrActivityResume() {
        val f = Fixture()
        val result = f.command("start")
        f.drain()
        assertEquals("pending", result.single().getString("phase"))
        assertEquals(1, f.live().size)
        // No UI owner or Activity exists. The process watch observes Rust's terminal state.
        f.respond = { """{"phase":"connected"}""" }
        f.tick()
        assertEquals(listOf("start", "status"), f.calls)
        assertEquals(1, f.changes)
        assertTrue(f.live().isEmpty())
        assertTrue(f.queue.isEmpty())
    }

    @Test fun uiReadsCoalesceAndReplaceTheSingleScheduledCheck() {
        val f = Fixture()
        f.command("start"); f.drain()
        val stale = f.live().single().work
        val a = f.command("status")
        val b = f.command("status")
        assertEquals(1, f.queue.size)
        assertTrue(f.live().isEmpty())
        f.drain()
        assertEquals(listOf("start", "status"), f.calls)
        assertEquals("pending", a.single().getString("phase"))
        assertEquals("pending", b.single().getString("phase"))
        assertEquals(1, f.live().size)
        stale.run() // Already-dequeued Handler callbacks are fenced, not merely removed.
        assertTrue(f.queue.isEmpty())
        assertEquals(1, f.live().size)
    }

    @Test fun cancelStopsWatchAndFencesAlreadyDequeuedCallbacks() {
        val f = Fixture()
        f.command("start"); f.drain()
        val late = f.live().single().work
        f.respond = { """{"phase":"signed_out"}""" }
        f.command("cancel")
        late.run()
        f.drain()
        late.run()
        assertEquals(listOf("start", "cancel"), f.calls)
        assertTrue(f.live().isEmpty())
        assertTrue(f.queue.isEmpty())
        assertEquals(0, f.changes)
    }

    @Test fun rejectedStatusAdmissionRetriesWithinTheSameBoundedWatchLifetime() {
        val f = Fixture()
        f.command("start"); f.drain()
        f.rejectNext = true
        f.tick()
        assertEquals(listOf("start"), f.calls)
        assertEquals(1, f.live().size)
        f.respond = { """{"phase":"connected"}""" }
        f.tick()
        assertEquals(listOf("start", "status"), f.calls)
        assertEquals(1, f.changes)
        assertTrue(f.live().isEmpty())
    }

    @Test fun coalescedUiCallbacksHaveABoundedAdmissionBudget() {
        val f = Fixture()
        f.command("start"); f.drain()
        val admitted = (1..4).map { f.command("status") }
        val rejected = f.command("status")
        assertEquals("auth_busy", rejected.single().getString("error"))
        assertEquals(1, f.queue.size)
        f.drain()
        assertTrue(admitted.all { it.single().getString("phase") == "pending" })
        assertEquals(listOf("start", "status"), f.calls)
        assertEquals(1, f.live().size)
    }

    @Test fun signoutCancelsAssistantBeforeNativeLogoutAndLateConnectedCannotPublish() {
        val f = Fixture()
        f.command("start"); f.drain()
        f.respond = { action ->
            if (action == "status") {
                f.command("sign_out") // Arrives while the old native status read is executing.
                """{"phase":"connected"}"""
            } else """{"phase":"signed_out"}"""
        }
        val staleUi = f.command("status")
        f.drain()
        assertEquals("auth_cancelled", staleUi.single().getString("error"))
        assertEquals(listOf("start", "status", "stop_assistant", "sign_out"), f.events)
        assertEquals(1, f.changes) // Sign-out only; no stale connected snapshot/sync.
        assertTrue(f.live().isEmpty())
    }

    @Test fun watchDeadlineNeverExtendsOnUiReadsAndNeverPollsIdleOrAfterDeadline() {
        val f = Fixture()
        assertTrue(f.live().isEmpty())
        f.command("start"); f.drain()
        f.now = ChatGptAuth.Owner.MAX_WATCH_MILLIS / 2
        f.command("status"); f.drain()
        assertEquals(1, f.live().size)
        f.now = ChatGptAuth.Owner.MAX_WATCH_MILLIS
        f.tick()
        assertTrue(f.live().isEmpty())
        assertTrue(f.queue.isEmpty())
        val count = f.calls.size
        f.command("status"); f.drain()
        assertEquals(count + 1, f.calls.size) // Deliberate UI read still works, no new lifetime.
        assertTrue(f.live().isEmpty())
    }

    @Test fun terminalErrorStopsWatchWithoutPublishingAConnectedSnapshot() {
        val f = Fixture()
        f.command("start"); f.drain()
        f.respond = { """{"phase":"error","error":"synthetic-private-error"}""" }
        f.tick()
        assertEquals(0, f.changes)
        assertTrue(f.live().isEmpty())
    }

    @Test fun publicBridgeNeverPassesNativeCredentialsUrlsOrErrorBodiesThrough() {
        val pending = ChatGptAuth.publicStatus("""{"phase":"pending","userCode":"TEST-0001","verificationUrl":"https://synthetic.invalid","access":"synthetic-secret"}""")
        assertEquals(setOf("phase", "userCode", "verificationUrl"), pending.keys().asSequence().toSet())
        assertEquals(ChatGptAuth.VERIFICATION_URL, pending.getString("verificationUrl"))
        val error = ChatGptAuth.publicStatus("""{"phase":"error","error":"synthetic-secret","refresh":"synthetic-refresh"}""")
        assertFalse(error.toString().contains("synthetic-secret"))
        assertEquals(setOf("phase", "error"), error.keys().asSequence().toSet())
        val connected = ChatGptAuth.publicStatus(JSONObject().put("phase", "connected").put("access", "synthetic-secret").toString())
        assertEquals(setOf("phase"), connected.keys().asSequence().toSet())
    }
}
