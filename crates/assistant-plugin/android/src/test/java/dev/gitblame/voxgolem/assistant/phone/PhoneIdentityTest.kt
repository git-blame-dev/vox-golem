package dev.gitblame.voxgolem.assistant.phone

import org.json.JSONObject
import org.junit.Assert.*
import org.junit.Test

class PhoneIdentityTest {
    @Test fun distributedIdentityRetainsFullUnsignedSequence() {
        val value = JSONObject(PhoneIdentity.native("11111111-1111-4111-8111-111111111111:18446744073709551615"))
        assertEquals("18446744073709551615", value.getString("sequence"))
    }
    @Test fun rejectsNoncanonicalAndOutOfRangeIdentitiesBeforeActorSelection() {
        for (id in listOf("1", "11111111-1111-4111-8111-111111111111:01", "11111111-1111-4111-8111-111111111111:0", "11111111-1111-4111-8111-111111111111:18446744073709551616")) {
            try { PhoneIdentity.native(id); fail("must reject invalid identity") }
            catch (failure: AssistantPhoneFailure) { assertEquals("invalid_input", failure.code) }
        }
    }
    @Test fun authExpiryAndModelForbiddenHaveDistinctSafeMessages() {
        val expired = EnginePhoneBackend.providerFailure("auth_expired")
        val forbidden = EnginePhoneBackend.providerFailure("forbidden")
        assertTrue(expired.boundedMessage.contains("expired"))
        assertFalse(forbidden.boundedMessage.contains("expired"))
        assertEquals("unavailable", forbidden.code)
    }
}
