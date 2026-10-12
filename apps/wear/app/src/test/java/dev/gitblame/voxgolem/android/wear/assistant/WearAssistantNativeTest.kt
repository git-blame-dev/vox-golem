package dev.gitblame.voxgolem.android.wear.assistant

import org.junit.Assert.*
import org.junit.Test

/** Runs the actual JNI binary on the Linux JVM, without credentials or provider traffic. */
class WearAssistantNativeTest {
    private fun request(id: String) = """{"requestId":"$id","instructions":"Synthetic policy","messages":[{"role":"user","content":"Synthetic 🧪 question"}]}"""

    @Test fun readyCanCancelBeforeAnyCredentialOrNetworkWork() {
        val id = "synthetic-jni-cancel"
        var ready = 0
        val callback = object : DeltaCallback {
            override fun onReady() { ready++; WearAssistantNative.cancel(id) }
            override fun onDelta(text: String) { fail("Cancelled request must not stream") }
        }
        repeat(2) {
            assertEquals("""{"status":"cancelled"}""",
                WearAssistantNative.respond(id, "/synthetic/missing/auth.json", request(id), callback))
        }
        assertEquals(2, ready)
    }

    @Test fun thrownReadyExceptionBecomesBoundedFailureAndIsCleared() {
        val throwing = object : DeltaCallback {
            override fun onReady() { throw IllegalStateException("Synthetic callback exception") }
            override fun onDelta(text: String) { fail("Failed callback must not stream") }
        }
        assertEquals("""{"status":"error","code":"callback_failed"}""",
            WearAssistantNative.respond("synthetic-jni-failure", "/synthetic/missing/auth.json", request("synthetic-jni-failure"), throwing))
        WearAssistantNative.cancel("synthetic-unknown")
        readyCanCancelBeforeAnyCredentialOrNetworkWork()
    }

    @Test fun invalidAndOversizedInputNeverCallsReady() {
        val callback = object : DeltaCallback {
            override fun onReady() { fail("Invalid input must not register") }
            override fun onDelta(text: String) { fail("Invalid input must not stream") }
        }
        for (input in listOf("invalid JSON", "🧪".repeat(384 * 1024 / 4 + 1), request("synthetic-mismatched-id"))) {
            assertEquals("""{"status":"error","code":"invalid_request"}""",
                WearAssistantNative.respond("synthetic-expected-id", "/synthetic/missing/auth.json", input, callback))
        }
    }
}
