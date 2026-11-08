package dev.gitblame.voxgolem.assistant.speech

import dev.gitblame.voxgolem.assistant.AssistantTiming
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.annotation.Config

@RunWith(dev.gitblame.voxgolem.assistant.AssistantStoreRunner::class)
@Config(sdk = [35], manifest = Config.NONE)
class ReplySpeechStreamerTest {
    @Test fun twoChunkReadAheadUsesIdentitySlotsAndFencesCancelledCallbacks() {
        val output = Output(); val streamer = ReplySpeechStreamer(output)
        streamer.onDelta("One two three. ", "en")
        streamer.onDelta("One two three. Four five six. ", "en")
        streamer.onDelta("One two three. Four five six. Seven eight nine. ", "en")
        streamer.onDelta("One two three. Four five six. Seven eight nine. Ten eleven twelve. ", "en")
        assertEquals(listOf("One two three.", "Four five six."), output.texts)
        // Reverse-order completion frees exactly the matching slot.
        val second = output.callbacks[1]
        second(null)
        assertEquals(3, output.texts.size)
        second(null)
        assertEquals(3, output.texts.size)
        var oldSettled = 0
        streamer.onComplete("One two three. Four five six. Seven eight nine. Ten eleven twelve.", "en") { oldSettled++ }
        assertEquals(0, oldSettled)
        streamer.cancel()
        assertEquals(1, oldSettled)
        streamer.reset()
        var replacementSettled = 0
        streamer.onComplete("Replacement synthetic answer.", "en") { replacementSettled++ }
        output.callbacks[2](null)
        output.callbacks[0]("stale_failure")
        assertEquals(0, replacementSettled)
        output.callbacks[3](null)
        assertEquals(1, replacementSettled)
    }

    @Test fun reverseOrderCompletionWaitsForEveryOwnedChunkAndSettlesOnce() {
        val output = Output(); val streamer = ReplySpeechStreamer(output)
        streamer.onDelta("One two three. ", "en")
        var settled = 0
        streamer.onComplete("One two three. Four five six.", "en") { settled++ }
        assertEquals(2, output.callbacks.size)
        output.callbacks[1](null)
        output.callbacks[1](null)
        assertEquals(0, settled)
        assertEquals(0, output.finishes)
        output.callbacks[0](null)
        output.callbacks[0](null)
        assertEquals(1, settled)
        assertEquals(1, output.finishes)
    }
    @Test fun postedFinishCannotCertifyRetirementAndCleanupFailurePropagates() {
        for (failure in listOf(null, "speech_cleanup_failed")) {
            val output = Output().also { it.deferFinish = true }
            val streamer = ReplySpeechStreamer(output)
            var settlements = 0; var outcome: String? = "pending"
            streamer.onComplete("Synthetic final answer.", "en") { settlements++; outcome = it }
            output.callbacks.single()(null)
            output.callbacks.single()(null) // Duplicate after Rust released the handle.
            assertEquals(0, settlements)
            assertEquals(1, output.finishes)
            output.deferredFinish!!(failure)
            output.deferredFinish!!(failure)
            assertEquals(1, settlements)
            assertEquals(failure, outcome)
        }
    }

    @Test fun disableThenEnableDoesNotReplayPendingClauses() {
        val output = Output(); val streamer = ReplySpeechStreamer(output)
        streamer.onDelta("One two three. ", "en")
        streamer.onDelta("One two three. Four five six. ", "en")
        output.enabled = false
        streamer.onDelta("One two three. Four five six. Seven eight nine. ", "en")
        output.enabled = true
        var settlements = 0
        streamer.onComplete("One two three. Four five six. Seven eight nine.", "en") { settlements++ }
        output.callbacks.forEach { it(null) }
        assertEquals(2, output.texts.size)
        assertEquals(1, settlements)
    }

    private class Output : SpeechOutputController {
        override var enabled = true
        val texts = mutableListOf<String>()
        val flushes = mutableListOf<Boolean>()
        val callbacks = mutableListOf<(String?) -> Unit>()
        var stops = 0
        var finishes = 0
        var deferredFinish: ((String?) -> Unit)? = null
        var deferFinish = false
        override fun speakStreamChunk(text: String, language: String?, flush: Boolean, keepOpen: Boolean, timingParent: AssistantTiming?, onDone: (String?) -> Unit) {
            texts += text; flushes += flush; callbacks += onDone
        }
        override fun finishStream() { finishes++ }
        override fun finishStream(onDone: (String?) -> Unit) {
            finishes++
            if (deferFinish) deferredFinish = onDone else onDone(null)
        }
        override fun speak(finalText: String, language: String, timingParent: AssistantTiming?, onTerminal: (String?) -> Unit) { error("Unexpected unbounded speech path") }
        override fun stop() { stops++ }
    }

    @Test fun earlyClauseAndCanonicalMismatchFenceDeferredOldCallback() {
        val output = Output()
        val streamer = ReplySpeechStreamer(output)
        streamer.onDelta("One two three. ", "en")
        assertEquals(listOf("One two three."), output.texts)
        var settled = 0
        streamer.onComplete("Different canonical answer.", "en") { settled++ }
        assertEquals(1, output.stops)
        assertEquals(listOf("One two three.", "Different canonical answer."), output.texts)
        output.callbacks[0]("stale failure")
        assertEquals(0, settled)
        output.callbacks[1](null)
        output.callbacks[1](null)
        assertEquals(1, settled)
        assertEquals(1, output.finishes)
    }

    @Test fun resetAndCancelFenceDelayedCallbacksAndNeverReplay() {
        val output = Output(); val streamer = ReplySpeechStreamer(output)
        streamer.onDelta("One two three. ", "en")
        var oldSettled = 0
        streamer.onComplete("One two three.", "en") { oldSettled++ }
        streamer.reset()
        assertEquals(1, oldSettled)
        var newSettled = 0
        streamer.onComplete("New synthetic answer.", "en") { newSettled++ }
        output.callbacks[0](null)
        assertEquals(0, newSettled)
        output.callbacks[1](null)
        assertEquals(1, newSettled)
        streamer.cancel()
        streamer.onDelta("Never replay this. ", "en")
        assertEquals(2, output.texts.size)
        streamer.close()
    }

    @Test fun realRustBoundsFloodAndOnlyTwoSdkChunksAreInFlight() {
        val output = Output(); val streamer = ReplySpeechStreamer(output)
        var text = "One two three. "
        streamer.onDelta(text, "en")
        repeat(1000) { text += "more; "; streamer.onDelta(text, "en") }
        assertEquals(2, output.texts.size)
        var settled = 0
        streamer.onComplete(text, "en") { settled++ }
        assertEquals(0, settled)
        var index = 0
        while (index < output.callbacks.size) output.callbacks[index++](null)
        assertEquals(1, settled)
        assertTrue(output.texts.sumOf { it.toByteArray(Charsets.UTF_8).size } <= 600)
        assertTrue(output.texts.sumOf { it.split(' ').size } <= 100)
        assertTrue(output.texts.size <= 100)
    }

    @Test fun shrinkCorrectionAndFinalParagraphUseSharedUtf8Cap() {
        val output = Output(); val streamer = ReplySpeechStreamer(output)
        streamer.onDelta("One two three. ", "en")
        streamer.onDelta("Changed", "en")
        assertEquals(1, output.stops)
        streamer.onComplete("Changed canonical answer.", "en") {}
        assertEquals("Changed canonical answer.", output.texts.last())
        output.callbacks.last()(null)
        assertEquals("hello world", FirstParagraphSpeech.extract("\uFEFFhello\tworld\nignored"))
        assertEquals("", FirstParagraphSpeech.extract("é".repeat(301)))
        assertTrue(FirstParagraphSpeech.extract(List(110) { "word" }.joinToString(" ")).split(' ').size <= 100)
    }

    @Test fun nativeSettledHandlesRemainBoundedUntilPlaybackAcknowledges() {
        val owners = List(64) { RustReplySpeech() }
        try {
            val chunks = owners.map { it.apply(2, "Synthetic final answer.").chunks.single() }
            assertThrows(SpeechPolicyUnavailable::class.java) { RustReplySpeech() }
            owners.zip(chunks).forEach { (owner, chunk) ->
                assertTrue(owner.complete(chunk)!!.idle)
                assertNull(owner.complete(chunk))
            }
            RustReplySpeech().close()
        } finally { owners.forEach { it.close() } }
    }

    @Test fun nativeAdmissionsReleaseOnExplicitClose() {
        repeat(200) { RustReplySpeech().use { it.apply(2, "Synthetic final answer.") } }
        repeat(200) { RustReplySpeech().close() }
    }

    @Test fun disabledAdmissionCannotBeEnabledUntilNextReset() {
        val output = Output().also { it.enabled = false }
        val streamer = ReplySpeechStreamer(output)
        output.enabled = true
        streamer.onDelta("One two three. ", "en")
        var settled = 0
        streamer.onComplete("One two three.", "en") { settled++ }
        assertTrue(output.texts.isEmpty())
        assertEquals(1, settled)
        streamer.reset()
        streamer.onComplete("Next synthetic answer.", "en") { settled++ }
        assertEquals(listOf("Next synthetic answer."), output.texts)
        output.callbacks.single()(null)
        assertEquals(2, settled)
    }

    @Test fun policyCapacityFailureIsCategoricalAndDoesNotStopUnownedOutput() {
        val owners = List(64) { RustReplySpeech() }
        try {
            val output = Output(); val streamer = ReplySpeechStreamer(output)
            var failure: String? = null
            streamer.onComplete("Synthetic answer.", "en") { failure = it }
            assertEquals("speech_policy_unavailable", failure)
            assertTrue(output.texts.isEmpty())
            assertEquals(0, output.stops)
        } finally { owners.forEach { it.close() } }
    }

    @Test fun closedSelectionNeverBridgesLaterAppendButShrinkStillCancels() {
        val output = Output(); val streamer = ReplySpeechStreamer(output)
        var failure: String? = null
        val first = "One two three.\n"
        streamer.onDelta(first, "en", onError = { failure = it })
        // Exceeds the native bridge input limit: this must never cross JNI.
        streamer.onDelta(first + "x".repeat(1_048_577), "en", onError = { failure = it })
        assertNull(failure)
        assertEquals(0, output.stops)
        streamer.onDelta("short", "en")
        assertEquals(1, output.stops)
        streamer.onComplete("Different canonical answer.", "en") {}
        assertEquals("Different canonical answer.", output.texts.last())
        output.callbacks.last()(null)
    }

    @Test fun invalidOperationAndOversizeInputRetireHandlesWithoutPayloadErrors() {
        repeat(100) {
            val handle = AssistantSpeechNative.open(true)
            assertTrue(handle != 0L)
            assertNull(AssistantSpeechNative.apply(handle, 99, null))
            assertNull(AssistantSpeechNative.apply(handle, 2, "stale"))
        }
        val output = Output(); val streamer = ReplySpeechStreamer(output)
        var failure: String? = null
        streamer.onComplete("x".repeat(1_048_577), "en") { failure = it }
        assertEquals("speech_policy_unavailable", failure)
        assertEquals(0, output.stops)
        RustReplySpeech().close()
    }

    @Test fun manualParagraphDoesNotConsumeRegistryCapacity() {
        val owners = List(64) { RustReplySpeech() }
        try { assertEquals("Synthetic answer.", FirstParagraphSpeech.extract("Synthetic answer.\nignored")) }
        finally { owners.forEach { it.close() } }
    }

    @Test fun canonicalSuffixAfterDrainedParagraphStartsANewSdkStreamWithoutReplay() {
        val output = Output(); val streamer = ReplySpeechStreamer(output)
        streamer.onDelta("One two three.\n", "en")
        output.callbacks[0](null)
        assertEquals(1, output.finishes)
        var settled = 0
        streamer.onComplete("One two three. Four five six.", "en") { settled++ }
        assertEquals(listOf("One two three.", "Four five six."), output.texts)
        assertEquals(listOf(true, true), output.flushes)
        output.callbacks[1](null)
        assertEquals(1, settled)
        assertEquals(2, output.finishes)
    }

    @Test fun closedParagraphReleasesStreamButWaitsForPlaybackAndCanonicalSettlement() {
        val output = Output(); val streamer = ReplySpeechStreamer(output)
        var settled = 0
        streamer.onDelta("Yes.\nignored", "en")
        assertEquals(listOf("Yes."), output.texts)
        assertEquals(0, output.finishes)
        output.callbacks.single()(null)
        assertEquals(1, output.finishes)
        assertEquals(0, settled)
        streamer.onComplete("Yes.\nignored", "en") { settled++ }
        assertEquals(1, settled)
        assertEquals(1, output.finishes)
    }

    @Test fun synthesisFailureSettlesOnceAndDropsQueuedEffects() {
        val output = Output(); val streamer = ReplySpeechStreamer(output)
        streamer.onDelta("One two three. ", "en")
        streamer.onDelta("One two three. Four five six. ", "en")
        var settled = 0
        var failure: String? = null
        streamer.onComplete("One two three. Four five six. A synthetic tail.", "en") { settled++; failure = it }
        output.callbacks[0]("synthetic_failure")
        output.callbacks[1](null)
        output.callbacks[0](null)
        assertEquals(1, settled)
        assertEquals("synthetic_failure", failure)
        assertEquals(2, output.texts.size)
        assertEquals(1, output.stops)
    }
}
