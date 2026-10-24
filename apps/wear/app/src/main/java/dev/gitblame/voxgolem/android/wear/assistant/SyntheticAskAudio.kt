package dev.gitblame.voxgolem.android.wear.assistant

import android.content.Context
import dev.gitblame.voxgolem.assistant.recognition.LocalRecognizerSession
import dev.gitblame.voxgolem.benchmark.android.*

internal typealias SyntheticAskInput = FiniteFixtureInput
internal typealias SyntheticAskAudio = FiniteFixtureAudio
internal typealias SyntheticAskRecognizerFactory = FixtureRecognizerFactory

internal fun SyntheticAskRecognizerFactory(context: Context, pcm: ByteArray, onFinal: () -> Unit,
    onRejected: () -> Unit, onObservedFinal: (Boolean, Boolean, Int) -> Unit = { _, _, _ -> }) =
    FixtureRecognizerFactory(context, pcm, SyntheticAskContract.value, onFinal, onRejected, onObservedFinal)

internal fun SyntheticAskRecognizerSession(delegate: LocalRecognizerSession, source: () -> SyntheticAskInput?,
    onFinal: () -> Unit, onRejected: () -> Unit,
    onObservedFinal: (Boolean, Boolean, Int) -> Unit = { _, _, _ -> }) =
    FixtureRecognizerSession(delegate, source, SyntheticAskContract.value, onFinal, onRejected, onObservedFinal)

internal object SyntheticAskContract {
    const val PHRASE = "Alpha bravo charlie delta."
    val value = BenchmarkVoiceContract.legacy(PHRASE)
    fun matches(text: String) = text.length <= 128 && value.matches(text)
    fun wordCount(text: String) = if (text.length <= 128) value.wordCount(text) else -1
    fun isFixturePrefix(text: String) = text.length <= 128 && value.isPrefix(text)
}
