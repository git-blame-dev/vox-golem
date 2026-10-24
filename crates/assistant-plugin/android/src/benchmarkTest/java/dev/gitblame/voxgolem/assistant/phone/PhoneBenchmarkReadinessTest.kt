package dev.gitblame.voxgolem.assistant.phone

import dev.gitblame.voxgolem.assistant.runtime.AssistantState
import dev.gitblame.voxgolem.assistant.runtime.ProviderAuthReason
import dev.gitblame.voxgolem.assistant.runtime.ProviderAuthStatus
import dev.gitblame.voxgolem.benchmark.android.BenchmarkVoiceContract
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.annotation.Config

@RunWith(PhoneAssistantRunner::class)
@Config(sdk = [35], manifest = Config.NONE)
class PhoneBenchmarkReadinessTest {
    private fun projection(available: Boolean) = PhoneSnapshotEncoder.encode(
        AssistantState(model = "custom-luna-none"), null,
        PhonePreferences.defaults().put("instant", "custom-luna-none"), emptyMap(), providerAvailable = available)

    @Test fun unavailableProductionProjectionPreventsAskForMissingOrExpiredCredentials() {
        val admission = PhoneBenchmarkAdmission(BenchmarkVoiceContract.legacy("Synthetic input"))
        for (reason in listOf(ProviderAuthReason.MISSING, ProviderAuthReason.EXPIRED)) {
            val state = AssistantState(credential = ProviderAuthStatus(reason, "synthetic-private-revision-not-for-export"))
            assertEquals(reason.name.lowercase(), PhoneSyntheticVoiceBenchmark.safeCredentialReason(state))
            val available = PhoneSyntheticVoiceBenchmark.selectedModelAvailable(projection(false))
            assertFalse(available)
            assertFalse(admission.allowsClick(available, true))
        }
    }

    @Test fun selectedAvailabilityComesFromProductionProjectionNotForcedModelPresence() {
        assertTrue(PhoneSyntheticVoiceBenchmark.selectedModelAvailable(projection(true)))
        assertFalse(PhoneSyntheticVoiceBenchmark.selectedModelAvailable(projection(false)))
        val other = projection(true)
        other.getJSONObject("preferences").put("instant", "custom-sol-high")
        assertFalse(PhoneSyntheticVoiceBenchmark.selectedModelAvailable(other))
    }

    @Test fun credentialOutputIsOnlyAnAllowlistedCategoryNeverRevisionOrCredentialData() {
        assertEquals("unknown", PhoneSyntheticVoiceBenchmark.safeCredentialReason(AssistantState()))
        for (reason in ProviderAuthReason.entries) {
            val value = PhoneSyntheticVoiceBenchmark.safeCredentialReason(AssistantState(
                credential = ProviderAuthStatus(reason, "synthetic-sensitive-revision-not-for-export")))
            assertEquals(reason.name.lowercase(), value)
            assertFalse(value.contains("synthetic"))
        }
    }
}
