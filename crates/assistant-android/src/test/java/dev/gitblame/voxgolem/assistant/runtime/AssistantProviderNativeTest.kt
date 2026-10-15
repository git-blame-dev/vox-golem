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

    @Test fun nativePrepareSearchReturnsStructuredEmptyJsonWhenWebDisabled() {
        val statuses = mutableListOf<String>()
        val callback = object : ProviderDeltaCallback {
            override fun onReady() {}
            override fun onDelta(text: String) {}
            override fun onStatus(status: String) { statuses.add(status) }
        }
        val request = """{"requestId":"synthetic-jni-search-off","prompt":"What is the weather?","webEnabled":false}"""
        val json = AssistantProviderNative.prepareSearch(request, callback)
        assertEquals("""{"evidenceContext":null,"sourceFooter":"","sources":[]}""", json)
        assertTrue(statuses.isEmpty())
    }
}
