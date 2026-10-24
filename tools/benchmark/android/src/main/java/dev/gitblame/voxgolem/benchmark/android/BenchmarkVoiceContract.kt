package dev.gitblame.voxgolem.benchmark.android

import org.json.JSONObject
import java.util.Locale

/** Frozen fixture data, not a second speech or turn policy. */
class BenchmarkVoiceContract private constructor(val phrase: String, val answer: String,
    private val aliases: Map<String, String>, private val legacy: Boolean = false) {
    private val expected = tokens(phrase)
    private fun tokens(text: String): List<String> = if (legacy) text.lowercase(Locale.ROOT)
        .replace(Regex("[^\\p{L}\\p{N}\\s]"), " ").trim().split(Regex("\\s+"))
        else Regex("[a-z]+|[0-9]+|[×*]").findAll(text.lowercase(Locale.ROOT)).map { aliases[it.value] ?: it.value }.toList()
    private fun bounded(text: String) = text.length <= if (legacy) 128 else 1024
    fun matches(text: String) = bounded(text) && tokens(text) == expected
    fun isPrefix(text: String) = bounded(text) && tokens(text).let { it.isNotEmpty() && it == expected.take(it.size) }
    fun wordCount(text: String) = if (bounded(text)) tokens(text).count { it.isNotEmpty() } else -1

    companion object {
        fun legacy(phrase: String) = BenchmarkVoiceContract(phrase, "", emptyMap(), legacy = true)
        fun parse(value: JSONObject): BenchmarkVoiceContract {
            require(value.keys().asSequence().toSet() == setOf("version", "scenario_id", "spoken_prompt", "expected_answer", "web_enabled", "token_aliases"))
            require(value.opt("version") == 1 && value.opt("scenario_id") == "arithmetic" && value.opt("web_enabled") == false)
            val phrase = value.getString("spoken_prompt")
            val answer = value.getString("expected_answer")
            require(phrase.length in 1..512 && answer.matches(Regex("[0-9]{1,8}")))
            val raw = value.getJSONObject("token_aliases")
            require(raw.length() <= 16)
            val aliases = raw.keys().asSequence().associateWith { key ->
                require(key.matches(Regex("[a-z0-9×*]{1,16}")))
                raw.getString(key).also { require(it.matches(Regex("[a-z]{1,32}"))) }
            }
            return BenchmarkVoiceContract(phrase, answer, aliases)
        }
    }
}
