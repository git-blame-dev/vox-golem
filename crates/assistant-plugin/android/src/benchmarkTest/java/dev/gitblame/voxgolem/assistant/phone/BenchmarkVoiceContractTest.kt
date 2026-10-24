package dev.gitblame.voxgolem.assistant.phone

import dev.gitblame.voxgolem.benchmark.android.BenchmarkVoiceContract
import org.json.JSONObject
import org.junit.Assert.*
import org.junit.Test

class BenchmarkVoiceContractTest {
    private fun contract() = BenchmarkVoiceContract.parse(JSONObject()
        .put("version", 1).put("scenario_id", "arithmetic")
        .put("spoken_prompt", "What is fourteen times fifteen? Reply only with the number.")
        .put("expected_answer", "210").put("web_enabled", false)
        .put("token_aliases", JSONObject().put("14", "fourteen").put("15", "fifteen").put("x", "times").put("×", "times").put("*", "times")))

    @Test fun numericAndOperatorAliasesShareTheExactContract() {
        val contract = contract()
        assertTrue(contract.matches("What is 14 × 15? Reply only with the number"))
        assertTrue(contract.isPrefix("What is 14 times"))
        assertEquals(10, contract.wordCount(contract.phrase))
        assertFalse(contract.matches("What is fourteen times fifteen"))
        assertFalse(contract.isPrefix("unrelated synthetic text"))
        assertFalse(contract.matches("a".repeat(1025)))
    }

    @Test fun mixedVersionOrUnexpectedFieldsFailClosed() {
        val value = JSONObject().put("version", 2)
        assertThrows(IllegalArgumentException::class.java) { BenchmarkVoiceContract.parse(value) }
    }
}
