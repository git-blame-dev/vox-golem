package dev.gitblame.voxgolem.assistant.runtime

import org.junit.Assert.*
import org.junit.Test

class AssistantProviderNativeTest {
    @Test fun genericJniProviderUsesTheSameRegisteredReadyCancellationProtocol() {
        val id = "synthetic-shared-native-cancel"
        var ready = 0
        val callback = object : ProviderDeltaCallback {
            override fun onReady() { ready++; AssistantProviderNative.cancel(id) }
            override fun onDelta(text: String) { fail("Cancelled provider must not stream") }
        }
        val request = """{"requestId":"$id","instructions":"Synthetic policy","messages":[{"role":"user","content":"Synthetic question"}]}"""
        assertEquals("""{"status":"cancelled"}""", AssistantProviderNative.respond(id, "/synthetic/private/auth.json", request, callback))
        assertEquals(1, ready)
    }
}
