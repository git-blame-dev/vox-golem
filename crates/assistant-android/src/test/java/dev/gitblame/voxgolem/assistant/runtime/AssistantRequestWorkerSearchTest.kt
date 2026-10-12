package dev.gitblame.voxgolem.assistant.runtime

import org.junit.Assert.*
import org.junit.Test
import java.util.concurrent.Executor

class AssistantRequestWorkerSearchTest {
    @Test fun firstTextValidationAcceptsOnlyBoundedDurationsAndRetiresEveryRequest() {
        for (duration in listOf(-1L, 0L, 86_400_000L, 86_400_001L)) {
            val measured = mutableListOf<Long>()
            val outcomes = mutableListOf<String>()
            val retired = mutableListOf<String>()
            val native = object : ProviderNativeCalls {
                override fun respond(requestId: String, authPath: String, requestJson: String, callback: ProviderDeltaCallback): String {
                    callback.onReady(); callback.onFirstText(duration)
                    return """{"status":"complete"}"""
                }
                override fun cancel(requestId: String) {}
            }
            val worker = AssistantRequestWorker(Executor { it.run() }, native)
            assertTrue(worker.submit("synthetic-bounded-first-text", "/synthetic/auth", "{}", object : AssistantRequestListener {
                override fun onFirstText(requestId: String, milliseconds: Long) { measured += milliseconds }
                override fun onDelta(requestId: String, text: String) {}
                override fun onOutcome(requestId: String, outcomeJson: String) { outcomes += outcomeJson }
                override fun onRetired(requestId: String) { retired += requestId }
            }))
            val valid = duration in 0..86_400_000
            assertEquals(if (valid) listOf(duration) else emptyList<Long>(), measured)
            assertEquals(if (valid) """{"status":"complete"}""" else """{"status":"error","code":"internal_error"}""", outcomes.single())
            assertEquals(listOf("synthetic-bounded-first-text"), retired)
            worker.close()
        }
    }

    @Test fun cancelledBeforeFirstTextAndOldCallbackDuringReplacementCannotPublishTiming() {
        lateinit var worker: AssistantRequestWorker
        lateinit var old: ProviderDeltaCallback
        val measured = mutableListOf<Pair<String, Long>>()
        val native = object : ProviderNativeCalls {
            override fun respond(requestId: String, authPath: String, requestJson: String, callback: ProviderDeltaCallback): String {
                callback.onReady()
                if (requestId == "synthetic-cancelled-first-text") {
                    old = callback
                    worker.cancel(); callback.onFirstText(123)
                } else {
                    old.onFirstText(999)
                    callback.onFirstText(456)
                }
                return """{"status":"complete"}"""
            }
            override fun cancel(requestId: String) {}
        }
        worker = AssistantRequestWorker(Executor { it.run() }, native)
        fun submit(id: String) = worker.submit(id, "/synthetic/auth", "{}", object : AssistantRequestListener {
            override fun onFirstText(requestId: String, milliseconds: Long) { measured += requestId to milliseconds }
            override fun onDelta(requestId: String, text: String) {}
            override fun onOutcome(requestId: String, outcomeJson: String) { assertEquals("synthetic-replacement-first-text", requestId) }
        })
        assertTrue(submit("synthetic-cancelled-first-text"))
        assertTrue(submit("synthetic-replacement-first-text"))
        assertEquals(listOf("synthetic-replacement-first-text" to 456L), measured)
        worker.close()
    }

    @Test fun providerFirstTextDurationIsFrozenAndDroppedAfterCancellationAndRetirement() {
        lateinit var worker: AssistantRequestWorker
        lateinit var retained: ProviderDeltaCallback
        val measured = mutableListOf<Pair<String, Long>>()
        val native = object : ProviderNativeCalls {
            override fun respond(requestId: String, authPath: String, requestJson: String, callback: ProviderDeltaCallback): String {
                retained = callback
                callback.onReady(); callback.onFirstText(123); callback.onFirstText(999)
                worker.cancel(); callback.onFirstText(500)
                return """{"status":"complete"}"""
            }
            override fun cancel(requestId: String) {}
        }
        worker = AssistantRequestWorker(Executor { it.run() }, native)
        worker.submit("synthetic-first-text", "/synthetic/auth", "{}", object : AssistantRequestListener {
            override fun onFirstText(requestId: String, milliseconds: Long) { measured += requestId to milliseconds }
            override fun onDelta(requestId: String, text: String) {}
            override fun onOutcome(requestId: String, outcomeJson: String) { fail("Cancelled outcome must be dropped") }
        })
        retained.onFirstText(1)
        assertEquals(listOf("synthetic-first-text" to 123L), measured)
    }

    @Test fun statusIsCorrelatedAndLateCallbacksAreDroppedAfterCancelAndRetirement() {
        var retainedCallback: ProviderDeltaCallback? = null
        lateinit var worker: AssistantRequestWorker
        val statuses = mutableListOf<Pair<String, String>>()
        val cancelled = mutableListOf<String>()
        val native = object : ProviderNativeCalls {
            override fun respond(requestId: String, authPath: String, requestJson: String, callback: ProviderDeltaCallback): String {
                retainedCallback = callback
                callback.onReady(); callback.onStatus("Searching")
                worker.cancel(); callback.onStatus("Reading")
                return """{"status":"complete"}"""
            }
            override fun cancel(requestId: String) { cancelled += requestId }
        }
        worker = AssistantRequestWorker(Executor { it.run() }, native)
        assertTrue(worker.submit("synthetic-worker-search", "/synthetic/auth", "{}", object : AssistantRequestListener {
            override fun onDelta(requestId: String, text: String) { fail("Status is not text") }
            override fun onStatus(requestId: String, status: String) { statuses += requestId to status }
            override fun onOutcome(requestId: String, outcomeJson: String) { fail("Cancelled outcome must be dropped") }
        }))
        retainedCallback!!.onStatus("Fallback")
        assertEquals(listOf("synthetic-worker-search" to "Searching"), statuses)
        assertEquals(listOf("synthetic-worker-search"), cancelled)
    }
}
