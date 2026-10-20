package dev.gitblame.voxgolem.assistant.speech

import dev.gitblame.voxgolem.assistant.AssistantTiming
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

class ReplySpeechStreamerTest {

    @Test
    fun testFirstParagraphSpeechExtract() {
        val multiParagraph = "First paragraph sentence one. First paragraph sentence two.\n\nSecond paragraph content."
        val extracted = FirstParagraphSpeech.extract(multiParagraph)
        assertEquals("First paragraph sentence one. First paragraph sentence two.", extracted)

        val leadingNewlines = "\n\n   Hello world. How are you?\n\nParagraph 2."
        assertEquals("Hello world. How are you?", FirstParagraphSpeech.extract(leadingNewlines))
    }

    @Test
    fun testSentenceStreamAccumulatorFirstParagraphOnly() {
        val accumulator = SentenceStreamAccumulator()
        val d1 = accumulator.onDelta("Hello there. This is sentence one. ")
        assertTrue(d1.isNotEmpty())

        val d2 = accumulator.onDelta("Hello there. This is sentence one. And sentence two.\n\nSecond paragraph details here.")
        // Once paragraph 1 completes on \n, accumulator marks isFirstParagraphComplete = true and flushes tail
        assertTrue(accumulator.isFirstParagraphComplete)
    }

    @Test
    fun testReplySpeechStreamerFlow() {
        val spokenChunks = ArrayList<String>()
        val output = object : SpeechOutputController {
            override fun speakStreamChunk(text: String, language: String?, flush: Boolean, keepOpen: Boolean, timingParent: AssistantTiming?, onDone: (String?) -> Unit) {
                spokenChunks.add(text)
                onDone(null)
            }
            override fun finishStream() {}
            override fun speak(finalText: String, language: String, timingParent: AssistantTiming?, onTerminal: (String?) -> Unit) {
                spokenChunks.add(finalText)
                onTerminal(null)
            }
            override fun stop() {}
        }

        val streamer = ReplySpeechStreamer(output)
        streamer.onDelta("Yes. That is correct.\n\nAdditional context.", "en-US")
        var settledError: String? = "unset"
        streamer.onComplete("Yes. That is correct.\n\nAdditional context.", "en-US") { err -> settledError = err }

        assertEquals(listOf("Yes. That is correct."), spokenChunks)
        assertEquals(null, settledError)
    }

    @Test
    fun testFirstParagraphSpeechSentenceTruncation() {
        val overBudget = "First complete sentence stays intact. Second sentence exceeds the forty character limit."
        assertEquals("First complete sentence stays intact.", FirstParagraphSpeech.extract(overBudget, maxChars = 50))
    }
}
