package dev.gitblame.voxgolem.assistant.phone

import org.json.JSONObject

object PhoneIdentity {
    fun native(raw: String): String {
        val match = Regex("([0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}):([1-9][0-9]{0,19})").matchEntire(raw)
            ?: throw AssistantPhoneFailure("invalid_input", "Invalid conversation identity")
        val sequence = match.groupValues[2]
        if (sequence.toBigInteger() > "18446744073709551615".toBigInteger()) throw AssistantPhoneFailure("invalid_input", "Invalid conversation identity")
        return JSONObject().put("replica", match.groupValues[1]).put("sequence", sequence).toString()
    }
}
