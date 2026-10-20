package dev.gitblame.voxgolem.assistant.speech

import dev.gitblame.voxgolem.assistant.AssistantTiming

interface SpeechOutputController {
    fun speakStreamChunk(
        text: String,
        language: String?,
        flush: Boolean,
        keepOpen: Boolean = false,
        timingParent: AssistantTiming? = null,
        onDone: (String?) -> Unit = {}
    )
    fun finishStream()
    fun speak(
        finalText: String,
        language: String,
        timingParent: AssistantTiming? = null,
        onTerminal: (String?) -> Unit
    )
    fun stop()
}

fun LocalSpeechOutput.asController(): SpeechOutputController = object : SpeechOutputController {
    override fun speakStreamChunk(text: String, language: String?, flush: Boolean, keepOpen: Boolean, timingParent: AssistantTiming?, onDone: (String?) -> Unit) {
        this@asController.speakStreamChunk(text, language, flush, keepOpen, timingParent, onDone)
    }
    override fun finishStream() {
        this@asController.finishStream()
    }
    override fun speak(finalText: String, language: String, timingParent: AssistantTiming?, onTerminal: (String?) -> Unit) {
        this@asController.speak(finalText, language, timingParent, onTerminal)
    }
    override fun stop() {
        this@asController.stop()
    }
}

object FirstParagraphSpeech {
    fun extract(text: String, maxChars: Int = 600): String {
        if (text.isBlank()) return ""
        var start = 0
        while (start < text.length && (text[start] == '\r' || text[start] == '\n' || text[start].isWhitespace())) {
            start++
        }
        if (start >= text.length) return ""
        var end = start
        while (end < text.length && text[end] != '\r' && text[end] != '\n') {
            end++
        }
        val paragraph = text.substring(start, end).trim()
        if (paragraph.isEmpty()) return ""
        if (paragraph.length <= maxChars) return paragraph
        var sentenceCut = -1
        for (i in (maxChars - 1) downTo 0) {
            if (paragraph[i] in ".!?;:" && (i + 1 >= paragraph.length || paragraph[i + 1].isWhitespace())) {
                sentenceCut = i + 1
                break
            }
        }
        if (sentenceCut > 0) {
            return paragraph.substring(0, sentenceCut).trim()
        }
        var cut = maxChars
        while (cut > 0 && !paragraph[cut].isWhitespace()) {
            cut--
        }
        if (cut <= 0) cut = maxChars
        return paragraph.substring(0, cut).trim()
    }
}

class SentenceStreamAccumulator(
    private val hardSplitChars: Int = 140,
) {
    private var consumedLength = 0
    private var hasEmittedClause = false
    var isFirstParagraphComplete: Boolean = false
        private set

    fun reset() {
        consumedLength = 0
        hasEmittedClause = false
        isFirstParagraphComplete = false
    }

    fun onDelta(fullTextSoFar: String): List<String> {
        if (isFirstParagraphComplete) return emptyList()
        val emitted = ArrayList<String>()

        var pStart = 0
        while (pStart < fullTextSoFar.length && (fullTextSoFar[pStart] == '\r' || fullTextSoFar[pStart] == '\n' || fullTextSoFar[pStart].isWhitespace())) {
            pStart++
        }
        if (pStart >= fullTextSoFar.length) return emitted

        var pEnd = pStart
        var foundParagraphBreak = false
        while (pEnd < fullTextSoFar.length) {
            val ch = fullTextSoFar[pEnd]
            if (ch == '\r' || ch == '\n') {
                foundParagraphBreak = true
                break
            }
            pEnd++
        }

        if (consumedLength < pStart) {
            consumedLength = pStart
        }

        val activeLimit = if (foundParagraphBreak) pEnd else fullTextSoFar.length
        while (consumedLength < activeLimit) {
            val unconsumed = fullTextSoFar.substring(consumedLength, activeLimit)
            if (unconsumed.isEmpty()) break
            val splitEnd = findNextBoundary(unconsumed, hasEmittedClause, hardSplitChars)
            if (splitEnd == null) {
                if (foundParagraphBreak) {
                    val tail = unconsumed.trim()
                    if (tail.isNotEmpty()) {
                        emitted += tail
                        hasEmittedClause = true
                    }
                    consumedLength = pEnd
                    isFirstParagraphComplete = true
                }
                break
            }
            var nextOffset = splitEnd
            while (nextOffset < unconsumed.length && unconsumed[nextOffset].isWhitespace()) {
                nextOffset++
            }
            val rawClause = unconsumed.substring(0, splitEnd).trim()
            consumedLength += nextOffset
            if (rawClause.isNotEmpty()) {
                emitted += rawClause
                hasEmittedClause = true
            }
        }

        if (foundParagraphBreak && !isFirstParagraphComplete) {
            if (consumedLength < pEnd) {
                val tail = fullTextSoFar.substring(consumedLength, pEnd).trim()
                if (tail.isNotEmpty()) {
                    emitted += tail
                    hasEmittedClause = true
                }
            }
            consumedLength = pEnd
            isFirstParagraphComplete = true
        }

        return emitted
    }

    fun flushRemainder(finalText: String): List<String> {
        val emitted = ArrayList<String>()
        emitted += onDelta(finalText)
        if (!isFirstParagraphComplete) {
            var pStart = 0
            while (pStart < finalText.length && (finalText[pStart] == '\r' || finalText[pStart] == '\n' || finalText[pStart].isWhitespace())) {
                pStart++
            }
            var pEnd = pStart
            while (pEnd < finalText.length && finalText[pEnd] != '\r' && finalText[pEnd] != '\n') {
                pEnd++
            }
            if (consumedLength < pEnd) {
                val tail = finalText.substring(consumedLength, pEnd).trim()
                if (tail.isNotEmpty()) {
                    emitted += tail
                }
                consumedLength = pEnd
            }
            isFirstParagraphComplete = true
        }
        return emitted
    }

    private fun findNextBoundary(unconsumed: String, hasEmitted: Boolean, hardLimit: Int): Int? {
        var hardBoundary: Int? = null
        var i = 0
        val minimumWords = if (hasEmitted) 1 else 3
        fun wordCount(end: Int): Int {
            return unconsumed.substring(0, end).trim()
                .split(Regex("\\s+"))
                .count { word -> word.any { it.isLetterOrDigit() } }
        }
        while (i < unconsumed.length) {
            val ch = unconsumed[i]
            val nextIsWhitespace = i + 1 < unconsumed.length && unconsumed[i + 1].isWhitespace()
            val atEnd = i == unconsumed.lastIndex
            val decimal = if (ch == '.' && i > 0 && unconsumed[i - 1].isDigit()) {
                val tokenStart = unconsumed.lastIndexOfAny(charArrayOf(' ', '\t', '\n'), i - 1) + 1
                (!atEnd && unconsumed[i + 1].isDigit()) ||
                    (atEnd && '.' !in unconsumed.substring(tokenStart, i))
            } else false
            val sentenceBoundary = ch in ".!?:;" && !decimal && (atEnd || nextIsWhitespace)

            if (sentenceBoundary && wordCount(i + 1) >= minimumWords) return i + 1
            if (ch == ',' && nextIsWhitespace && wordCount(i + 1) >= 5) return i + 1
            if (ch.isWhitespace() && (i <= hardLimit || hardBoundary == null) && wordCount(i) >= minimumWords) {
                hardBoundary = i
            }
            if (i >= hardLimit && hardBoundary != null) return hardBoundary
            i++
        }
        return null
    }
}

class ReplySpeechStreamer(private val output: SpeechOutputController) {
    private val accumulator = SentenceStreamAccumulator()
    private var streamedChunksInTurn = 0
    private var pendingStreamChunksInTurn = 0
    private var generationCompletedForTurn = false
    private var streamFinishedForTurn = false
    private var settledForTurn = false
    private var onSettledCallback: ((String?) -> Unit)? = null

    val hasStreamedChunks: Boolean get() = streamedChunksInTurn > 0

    fun cancel() {
        settledForTurn = true
        onSettledCallback = null
    }

    fun reset() {
        accumulator.reset()
        streamedChunksInTurn = 0
        pendingStreamChunksInTurn = 0
        generationCompletedForTurn = false
        streamFinishedForTurn = false
        settledForTurn = false
        onSettledCallback = null
    }

    fun onDelta(
        fullTextSoFar: String,
        language: String,
        timingParent: AssistantTiming? = null,
        onFirstClause: () -> Unit = {},
        onError: (String) -> Unit = {},
        onAllChunksCompletedAfterGeneration: () -> Unit = {}
    ) {
        if (settledForTurn || generationCompletedForTurn) return
        val clauses = accumulator.onDelta(fullTextSoFar)
        for (clause in clauses) {
            if (clause.isBlank()) continue
            streamedChunksInTurn++
            pendingStreamChunksInTurn++
            val isFirst = streamedChunksInTurn == 1
            if (isFirst) {
                onFirstClause()
            }
            output.speakStreamChunk(
                text = clause,
                language = language,
                flush = isFirst,
                keepOpen = true,
                timingParent = if (isFirst) timingParent else null
            ) { failure ->
                pendingStreamChunksInTurn = (pendingStreamChunksInTurn - 1).coerceAtLeast(0)
                if (failure != null && !settledForTurn) {
                    settledForTurn = true
                    val settled = onSettledCallback
                    if (settled != null) settled(failure) else onError(failure)
                    return@speakStreamChunk
                }
                if (generationCompletedForTurn && pendingStreamChunksInTurn == 0 && !settledForTurn) {
                    settledForTurn = true
                    onSettledCallback?.invoke(null)
                    onAllChunksCompletedAfterGeneration()
                }
            }
        }
        if (accumulator.isFirstParagraphComplete && streamedChunksInTurn > 0 && !streamFinishedForTurn) {
            streamFinishedForTurn = true
            output.finishStream()
        }
    }

    fun onComplete(
        finalText: String,
        language: String,
        timingParent: AssistantTiming? = null,
        onSpeakingStarted: () -> Unit = {},
        onSettled: (String?) -> Unit
    ) {
        if (settledForTurn) return
        onSettledCallback = onSettled
        val remaining = accumulator.flushRemainder(finalText)
        if (streamedChunksInTurn == 0 && remaining.size <= 1) {
            val single = remaining.firstOrNull()?.trim().orEmpty()
            if (single.isBlank()) {
                settledForTurn = true
                onSettled(null)
                return
            }
            onSpeakingStarted()
            output.speak(single, language, timingParent) { failure ->
                if (!settledForTurn) {
                    settledForTurn = true
                    onSettled(failure)
                }
            }
            return
        }

        if (streamedChunksInTurn == 0) {
            onSpeakingStarted()
        }
        for (clause in remaining) {
            if (clause.isBlank()) continue
            streamedChunksInTurn++
            pendingStreamChunksInTurn++
            val isFirst = streamedChunksInTurn == 1
            output.speakStreamChunk(
                text = clause,
                language = language,
                flush = isFirst,
                keepOpen = true,
                timingParent = if (isFirst) timingParent else null
            ) { failure ->
                pendingStreamChunksInTurn = (pendingStreamChunksInTurn - 1).coerceAtLeast(0)
                if (failure != null && !settledForTurn) {
                    settledForTurn = true
                    onSettled(failure)
                    return@speakStreamChunk
                }
                if (generationCompletedForTurn && pendingStreamChunksInTurn == 0 && !settledForTurn) {
                    settledForTurn = true
                    onSettled(null)
                }
            }
        }
        generationCompletedForTurn = true
        if (!streamFinishedForTurn) {
            streamFinishedForTurn = true
            output.finishStream()
        }
        if (pendingStreamChunksInTurn == 0 && !settledForTurn) {
            settledForTurn = true
            onSettled(null)
        }
    }
}
