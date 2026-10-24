package dev.gitblame.voxgolem.assistant.phone

import dev.gitblame.voxgolem.benchmark.android.BenchmarkVoiceContract
import org.junit.Assert.assertThrows
import org.junit.Test

class PhoneBenchmarkAdmissionTest {
    private val contract = BenchmarkVoiceContract.legacy("Synthetic bounded input")
    private val request = "voice-00000000-0000-4000-8000-000000000001"

    @Test fun delayedFirstAttachmentDuringProviderRetirementCannotInvalidateResourceProof() {
        val admission = PhoneBenchmarkAdmission(contract)
        var preparedSpeech = 0
        var allocatedSdk = 0
        var attachedActive = true
        val main = java.util.ArrayDeque<() -> Unit>()
        main.add { attachedActive = admission.attachResources { preparedSpeech++; allocatedSdk++ } }
        admission.retire() // Main-owner retirement runs before the delayed first Activity transaction.
        val checkedSdk = allocatedSdk
        val providerRetired = run {
            main.removeFirst().invoke() // Released while provider/actor retirement is in progress.
            true
        }
        org.junit.Assert.assertFalse(attachedActive)
        org.junit.Assert.assertEquals(0, preparedSpeech)
        org.junit.Assert.assertEquals(checkedSdk, allocatedSdk)
        org.junit.Assert.assertTrue(providerRetired && checkedSdk == allocatedSdk)
        org.junit.Assert.assertTrue(admission.isRetiring())
    }

    @Test fun retirementIrreversiblyFreezesInputAndRetainsConsumedBudgetEvidence() {
        val admission = PhoneBenchmarkAdmission(contract)
        admission.voice(true, "single_turn", "", false, null)
        admission.retire()
        org.junit.Assert.assertTrue(admission.isRetiring())
        org.junit.Assert.assertFalse(admission.allowsClick(true, true))
        assertThrows(IllegalStateException::class.java) { admission.prompt(true, 1, request, contract.phrase) }
        assertThrows(IllegalStateException::class.java) { admission.voice(true, "single_turn", "", false, null) }
        org.junit.Assert.assertEquals(1, admission.voiceAdmissions())
        org.junit.Assert.assertEquals(0, admission.promptAdmissions())
        admission.retire()
        org.junit.Assert.assertTrue(admission.isRetiring())
    }

    @Test fun readinessBudgetCannotAdmitVoiceOrPromptEvenWhenUiAndInputClaimReady() {
        val admission = PhoneBenchmarkAdmission(contract, liveAllowed = false)
        org.junit.Assert.assertFalse(admission.allowsClick(true, true))
        assertThrows(IllegalStateException::class.java) { admission.voice(true, "single_turn", "", false, null) }
        assertThrows(IllegalStateException::class.java) { admission.prompt(true, 1, request, contract.phrase) }
        org.junit.Assert.assertEquals(0, admission.voiceAdmissions())
        org.junit.Assert.assertEquals(0, admission.promptAdmissions())
    }

    @Test fun unavailableModelOrUnreadyFrontendCannotBeClicked() {
        val admission = PhoneBenchmarkAdmission(contract)
        org.junit.Assert.assertFalse(admission.allowsClick(false, true))
        org.junit.Assert.assertFalse(admission.allowsClick(true, false))
        org.junit.Assert.assertTrue(admission.allowsClick(true, true))
    }

    @Test fun onlyOneOwnedExactFinalCanCrossTheFrontendPromptFence() {
        val admission = PhoneBenchmarkAdmission(contract)
        assertThrows(IllegalStateException::class.java) { admission.prompt(true, 1, request, contract.phrase) }
        admission.voice(true, "single_turn", "", false, null)
        assertThrows(IllegalStateException::class.java) { admission.prompt(true, 0, request, contract.phrase) }
        assertThrows(IllegalArgumentException::class.java) { admission.prompt(true, 1, request, "Synthetic wrong input") }
        assertThrows(IllegalStateException::class.java) { admission.prompt(false, 1, request, contract.phrase) }
        admission.prompt(true, 1, request, contract.phrase)
        assertThrows(IllegalStateException::class.java) { admission.prompt(true, 1, request, contract.phrase) }
        assertThrows(IllegalStateException::class.java) { admission.voice(true, "single_turn", "", false, null) }
    }

    @Test fun loopShortcutOrExistingDraftCannotSpendTheSingleTurnBudget() {
        for ((policy, draft, loop, shortcut) in listOf(
            listOf("repeat_after_reply", "", true, null), listOf("single_turn", "synthetic", false, null),
            listOf("single_turn", "", false, "synthetic-shortcut"))) {
            val admission = PhoneBenchmarkAdmission(contract)
            assertThrows(IllegalArgumentException::class.java) {
                admission.voice(true, policy as String, draft as String, loop as Boolean, shortcut as String?)
            }
            admission.voice(true, "single_turn", "", false, null)
        }
    }
}
