package dev.gitblame.voxgolem.assistant.inputtransport

import java.util.Locale

/** SDK audio/locale limits shared with peer framing; no transport paths or peer identity. */
object VoiceAudioContract {
    const val MAX_TEXT_BYTES = 8192
    const val PCM_CHUNK_BYTES = 640 // 20 ms, mono signed PCM16LE at 16 kHz
    const val SAMPLE_RATE = 16000

    fun language(value: String): String {
        require(value.length in 2..64 && value.matches(Regex("[A-Za-z]{2,8}(-[A-Za-z0-9]{1,8})*")))
        val locale = Locale.Builder().setLanguageTag(value).build()
        require(locale.language.isNotEmpty() && locale.language != "und" && locale.toLanguageTag() == value)
        return value
    }
}
