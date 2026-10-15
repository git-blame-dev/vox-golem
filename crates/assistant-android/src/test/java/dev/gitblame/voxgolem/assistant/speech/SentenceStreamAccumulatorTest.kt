package dev.gitblame.voxgolem.assistant.speech

import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

class SentenceStreamAccumulatorTest {
    @Test fun shortThreeWordSentenceEmitsAtDeltaEnd() {
        val accumulator = SentenceStreamAccumulator()
        assertEquals(listOf("It is fine."), accumulator.onDelta("It is fine."))
        assertTrue(accumulator.onDelta("It is fine.").isEmpty())
    }

    @Test fun firstClauseRequiresThreeSpeakableWords() {
        val accumulator = SentenceStreamAccumulator()
        assertTrue(accumulator.onDelta("Quite extraordinary!").isEmpty())
        assertEquals(listOf("Quite extraordinary! Indeed."), accumulator.onDelta("Quite extraordinary! Indeed."))
        assertEquals(listOf("Yes!"), accumulator.onDelta("Quite extraordinary! Indeed. Yes!"))
    }

    @Test fun colonSemicolonAndNewlineEmitThreeWordClauses() {
        for (ending in listOf(":", ";", "\n", "!", "?")) {
            val accumulator = SentenceStreamAccumulator()
            assertEquals(listOf("One two three${ending.trim()}"), accumulator.onDelta("One two three$ending"))
        }
    }

    @Test fun commaNeedsFiveWordsAndFollowingWhitespaceAcrossDeltas() {
        val accumulator = SentenceStreamAccumulator()
        assertTrue(accumulator.onDelta("One two three four five,").isEmpty())
        assertEquals(listOf("One two three four five,"), accumulator.onDelta("One two three four five, "))
        assertTrue(SentenceStreamAccumulator().onDelta("One two three four, ").isEmpty())
    }

    @Test fun cumulativeDeltasAndRepeatedFlushPreserveExactOrderWithoutDuplicateTail() {
        val accumulator = SentenceStreamAccumulator()
        val spoken = mutableListOf<String>()
        spoken += accumulator.onDelta("It is")
        spoken += accumulator.onDelta("It is fine.")
        spoken += accumulator.onDelta("It is fine. Yes! Final")
        spoken += accumulator.flushRemainder("It is fine. Yes! Final tail")
        spoken += accumulator.flushRemainder("It is fine. Yes! Final tail")
        assertEquals(listOf("It is fine.", "Yes!", "Final tail"), spoken)
        accumulator.reset()
        assertEquals(listOf("It is fine."), accumulator.onDelta("It is fine."))
    }

    @Test fun pendingMarkdownDoesNotSplitInsideLinkOrCode() {
        val accumulator = SentenceStreamAccumulator()
        assertTrue(accumulator.onDelta("Read this [short: useful").isEmpty())
        assertTrue(accumulator.onDelta("Read this [short: useful guide](https://example.invalid/path;").isEmpty())
        assertEquals(listOf("Read this short: useful guide."), accumulator.onDelta(
            "Read this [short: useful guide](https://example.invalid/path;)."))
        val code = SentenceStreamAccumulator()
        assertTrue(code.onDelta("Try this `one; two").isEmpty())
        assertEquals(listOf("Try this one; two."), code.onDelta("Try this `one; two`."))
    }

    @Test fun decimalAtDeltaEndWaitsForContinuation() {
        val accumulator = SentenceStreamAccumulator()
        assertTrue(accumulator.onDelta("Value is 3.").isEmpty())
        assertEquals(listOf("Value is 3.14."), accumulator.onDelta("Value is 3.14."))
    }

    @Test fun longPlainTextHasBoundedWhitespaceSplitAndSanitizedFinalTail() {
        val accumulator = SentenceStreamAccumulator(hardSplitChars = 24)
        val text = "one two three four five six seven eight"
        val spoken = accumulator.onDelta(text) + accumulator.flushRemainder(text)
        assertTrue(spoken.size > 1)
        assertEquals(text, spoken.joinToString(" "))
        assertEquals(listOf("A final tail"), SentenceStreamAccumulator().flushRemainder(
            "**A final tail**\nSources:\nhttps://example.invalid"))
    }
}
