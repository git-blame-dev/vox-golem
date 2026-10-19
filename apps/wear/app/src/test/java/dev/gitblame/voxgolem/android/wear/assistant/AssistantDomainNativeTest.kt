package dev.gitblame.voxgolem.android.wear.assistant

import dev.gitblame.voxgolem.assistant.AssistantDomainNative
import org.junit.Assert.*
import org.junit.Test

class AssistantDomainNativeTest {
    private val replica = "00000000-0000-4000-8000-000000000001"
    private val operation = """{"version":2,"id":{"replica":"$replica","sequence":"1"},"body":{"kind":"conversation_created","title":"Synthetic 🧪"}}"""
    private val bodyKey = "35d4212614f45e21f4a614a7fbc4b0ad677f6a21f84dae1cf637d2430f01945e"
    private val empty = """{"version":2,"revision":"0","records":[],"retired":[]}"""

    @Test fun bothJniNamesUseSameCanonicalHashWithoutProviderOrCredentials() {
        val request = """{"kind":"record","operation":$operation}"""
        val expected = """{"ok":true,"value":{"body_key":"$bodyKey","operation":$operation}}"""
        assertEquals(expected, AssistantDomainNative.apply(request))
        assertEquals(expected, WearAssistantNative.apply(request))
        val reordered = """{"kind":"record","operation":{"body":{"title":"Synthetic \ud83e\uddea","kind":"conversation_created"},"version":2,"id":{"sequence":"1","replica":"$replica"}}}"""
        assertEquals(expected, AssistantDomainNative.apply(reordered))
    }

    @Test fun forgedSnapshotDigestIsRejectedByActualJniRestore() {
        val snapshot = """{"version":2,"revision":"1","records":[{"body_key":"${"0".repeat(64)}","operation":$operation}],"retired":[]}"""
        assertEquals("""{"ok":false,"error":{"kind":"invalid","field":"body_key_sha256_mismatch"}}""",
            AssistantDomainNative.apply("""{"kind":"view","snapshot":$snapshot}"""))
    }

    @Test fun pureRestoreDoesNotRetainPreviousCalls() {
        val record = """{"body_key":"$bodyKey","operation":$operation}"""
        val response = AssistantDomainNative.apply("""{"kind":"plan","snapshot":$empty,"command":{"kind":"import","expected_revision":"0","records":[$record]}}""")
        assertTrue(response.contains("\"base_revision\":\"0\""))
        assertTrue(response.contains("\"revision\":\"1\""))
        val view = AssistantDomainNative.apply("""{"kind":"view","snapshot":$empty}""")
        assertEquals("""{"ok":true,"value":{"revision":"0","records":[],"retired":[],"conversations":[],"pending":[],"quarantine":[]}}""", view)
    }

    @Test fun strictUnknownFieldFailureDoesNotEchoInput() {
        val response = AssistantDomainNative.apply("""{"kind":"view","snapshot":$empty,"secret":"Synthetic sensitive marker"}""")
        assertEquals("""{"ok":false,"error":{"kind":"invalid","field":"native_request_json"}}""", response)
        assertFalse(response.contains("Synthetic sensitive marker"))
    }

    @Test fun nativeSnapshotByteBoundPrecedesSnapshotDeserialization() {
        val request = """{"kind":"view","snapshot":{${" ".repeat(4 * 1024 * 1024)}}}"""
        assertEquals("""{"ok":false,"error":{"kind":"limit_exceeded","limit":"native_snapshot_bytes"}}""", AssistantDomainNative.apply(request))
    }

    @Test fun rawJniRequestUtf8LimitUsesTheSameBoundedDomainError() {
        val expected = """{"ok":false,"error":{"kind":"limit_exceeded","limit":"native_request_bytes"}}"""
        assertEquals(expected, AssistantDomainNative.apply(" ".repeat(16 * 1024 * 1024 + 1)))
        assertEquals(expected, WearAssistantNative.apply("🧪".repeat(16 * 1024 * 1024 / 4 + 1)))
    }
}
