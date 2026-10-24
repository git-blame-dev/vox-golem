package dev.gitblame.voxgolem.android.wear.assistant

import android.content.Intent
import android.os.ParcelFileDescriptor
import dev.gitblame.voxgolem.assistant.recognition.LocalRecognizerEvents
import dev.gitblame.voxgolem.assistant.recognition.LocalRecognizerSession
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35])
class SyntheticAskBenchmarkTest {
    @Test fun retirementPollingRequiresObservationAndTimesOutWithoutSkippingLaterCleanup() {
        var time = 0L
        var observations = 0
        awaitSyntheticRetirement({ ++observations == 3 }, { time }, { time += 10 })
        assertEquals(20L, time)
        val visited = mutableListOf<String>()
        val failure = runCatching {
            runSyntheticCleanup(listOf(
                { awaitSyntheticRetirement({ false }, { time }, { time += 10 }) },
                { visited += "audio" }, { visited += "provider" }, { visited += "store" },
            ))
        }.exceptionOrNull()
        assertEquals("synthetic_tts_retirement_timeout", failure?.message)
        assertEquals(5_020L, time)
        assertEquals(listOf("audio", "provider", "store"), visited)
    }

    @Test fun pcmProvenanceUsesActualBytesAndRejectsInvalidFormatOrBudget() {
        val pcm = ByteArray(32_000)
        val proof = syntheticPcmProvenance(pcm)
        assertEquals(32_000, proof.getInt("fixture_bytes"))
        assertEquals(1_000, proof.getInt("fixture_duration_ms"))
        assertEquals(16_000, proof.getInt("fixture_sample_rate_hz"))
        assertEquals(1, proof.getInt("fixture_channels"))
        assertEquals(16, proof.getInt("fixture_bits_per_sample"))
        val hash = proof.getString("fixture_pcm_sha256")!!
        assertTrue(hash.matches(Regex("[0-9a-f]{64}")))
        assertEquals("0c92bddb4e96f3ea9ec9f0f64a668255a6c15527ac09f6f119cafde60c7c4a39", hash)
        pcm[0] = 1
        assertNotEquals(hash, syntheticPcmProvenance(pcm).getString("fixture_pcm_sha256"))
        assertEquals(10_500, syntheticPcmProvenance(ByteArray(336_000)).getInt("fixture_duration_ms"))
        for (size in listOf(0, 1, 336_002)) {
            assertTrue(runCatching { syntheticPcmProvenance(ByteArray(size)) }.exceptionOrNull() is IllegalArgumentException)
        }
    }

    @Test fun syntheticArgumentsDefaultToCustomAndThreeTrialsAndAllowOnlyCheapRoutes() {
        assertEquals("custom-luna-none" to 3, SyntheticAskBenchmark.parseArguments(android.os.Bundle()))
        for (model in listOf("custom-luna-none", "gemini-nano", "agy-flash-low")) {
            for (trials in listOf("1", "3")) {
                val args = android.os.Bundle().apply { putString("model", model); putString("trials", trials) }
                assertEquals(model to trials.toInt(), SyntheticAskBenchmark.parseArguments(args))
            }
        }
        for (model in listOf("", "custom-sol-high", "agy-flash-high", "GEMINI-NANO")) {
            assertTrue(runCatching {
                SyntheticAskBenchmark.parseArguments(android.os.Bundle().apply { putString("model", model) })
            }.exceptionOrNull() is IllegalArgumentException)
        }
        for (trials in listOf("", "0", "2", "4", "01")) {
            assertTrue(runCatching {
                SyntheticAskBenchmark.parseArguments(android.os.Bundle().apply { putString("trials", trials) })
            }.exceptionOrNull() is IllegalArgumentException)
        }
    }

    @Test fun unknownSyntheticModelIsRejectedBeforeAnyContextOrFilesystemAccess() {
        val untouched = object : android.content.ContextWrapper(null) {
            override fun getCacheDir(): java.io.File = error("unexpected_context_access")
            override fun getApplicationContext(): android.content.Context = error("unexpected_context_access")
        }
        val failure = runCatching {
            dev.gitblame.voxgolem.assistant.runtime.AssistantRuntime.syntheticSession(
                untouched, java.io.File("synthetic-unused"), "custom-sol-high") { error("unexpected_timing") }
        }.exceptionOrNull()
        assertTrue(failure is IllegalArgumentException)
    }

    @Test fun cleanupAttemptsActivityAudioAndProviderStagesAndRetainsTheFirstFailure() {
        for (failed in listOf("activity", "audio", "provider")) {
            val visited = mutableListOf<String>()
            val failure = IllegalStateException("synthetic_$failed")
            var released = false
            val result = runCatching {
                runSyntheticCleanup(listOf("activity", "audio", "provider", "store").map { name ->
                    { visited += name; if (name == failed) throw failure }
                })
                released = true
            }
            assertSame(failure, result.exceptionOrNull())
            assertEquals(listOf("activity", "audio", "provider", "store"), visited)
            assertFalse(released)
        }
    }
    private class Delegate : LocalRecognizerSession {
        var starts = 0
        lateinit var events: LocalRecognizerEvents
        override fun checkLanguage(language: String, result: (Boolean) -> Unit) = result(true)
        override fun start(language: String, events: LocalRecognizerEvents) { starts++; this.events = events }
        override fun stop() = Unit
        override fun cancel() = Unit
        override fun destroy() = Unit
    }
    private class Input(override val eof: Boolean = true, override val isRecording: Boolean = true) : SyntheticAskInput {
        private val pipe = ParcelFileDescriptor.createPipe()
        override val readDescriptor = pipe[0]
        override val writerRetired = true
        override fun configureIntent(intent: Intent) = intent
        override fun startStreaming() = Unit
        override fun stopCapture() = Unit
        override fun close() { pipe.forEach { it.close() } }
    }
    private class Events : LocalRecognizerEvents {
        val finals = mutableListOf<String>()
        val partials = mutableListOf<String>()
        val errors = mutableListOf<String>()
        override fun final(text: String) { finals += text }
        override fun partial(text: String) { partials += text }
        override fun error(code: String) { errors += code }
    }

    @Test fun exactSyntheticTokensRejectAdditionalDigitsWordsUnicodeAndOversizedContent() {
        assertTrue(SyntheticAskContract.matches("Alpha, bravo charlie delta!"))
        listOf("Alpha bravo charlie delta 42", "Alpha bravo charlie delta synthetic extra",
            "Alpha bravo charlie delta Ω", "bravo charlie delta", "Alpha bravo charlie delta" + " ".repeat(200))
            .forEach { assertFalse(SyntheticAskContract.matches(it)) }
    }

    @Test fun missingClosedOrInactivePipeNeverCallsTheRealStartMethod() {
        val delegate = Delegate()
        val absent = SyntheticAskRecognizerSession(delegate, { null }, {}, {})
        assertTrue(runCatching { absent.start("en-AU", Events()) }.isFailure)
        Input(isRecording = false).use { input ->
            val inactive = SyntheticAskRecognizerSession(delegate, { input }, {}, {})
            assertTrue(runCatching { inactive.start("en-AU", Events()) }.isFailure)
        }
        Input().also { input ->
            input.close()
            val closed = SyntheticAskRecognizerSession(delegate, { input }, {}, {})
            assertTrue(runCatching { closed.start("en-AU", Events()) }.isFailure)
        }
        assertEquals(0, delegate.starts)
    }

    @Test fun actualDelegateFinalIsForwardedExactlyOnceOnlyAfterFiniteFixtureEof() {
        Input().use { input ->
            val delegate = Delegate()
            var accepted = 0
            val events = Events()
            SyntheticAskRecognizerSession(delegate, { input }, { accepted++ }, { fail("unexpected rejection") })
                .start("en-AU", events)
            delegate.events.partial("Alpha bravo")
            delegate.events.partial("Synthetic unrelated words")
            delegate.events.final(SyntheticAskContract.PHRASE)
            delegate.events.final(SyntheticAskContract.PHRASE)
            assertEquals(1, accepted)
            assertEquals(listOf(SyntheticAskContract.PHRASE), events.finals)
            assertEquals(listOf("Alpha bravo"), events.partials)
            assertTrue(events.errors.isEmpty())
        }
    }

    @Test fun unexpectedAuthoritativeTextOrPrematureFinalCannotReachTheLoopOrModel() {
        listOf(false to SyntheticAskContract.PHRASE, true to "Synthetic unrelated words").forEach { (eof, text) ->
            Input(eof = eof).use { input ->
                val delegate = Delegate()
                val events = Events()
                var rejected = 0
                SyntheticAskRecognizerSession(delegate, { input }, { fail("unexpected acceptance") }, { rejected++ })
                    .start("en-AU", events)
                delegate.events.final(text)
                delegate.events.final(SyntheticAskContract.PHRASE)
                assertEquals(1, rejected)
                assertTrue(events.finals.isEmpty())
                assertEquals(listOf("synthetic_input_rejected"), events.errors)
            }
        }
    }

    @Test fun finalGuardDiagnosticsDistinguishTextMismatchFromMissingEofWithoutExportingText() {
        for ((eof, text) in listOf(false to SyntheticAskContract.PHRASE, true to "Synthetic other words")) {
            Input(eof = eof).use { input ->
                val delegate = Delegate()
                val observed = mutableListOf<Triple<Boolean, Boolean, Int>>()
                val events = Events()
                SyntheticAskRecognizerSession(delegate, { input }, { fail("unexpected acceptance") }, {},
                    { matched, ended, words -> observed += Triple(matched, ended, words) }).start("en-AU", events)
                delegate.events.final(text)
                assertEquals(listOf(Triple(SyntheticAskContract.matches(text), eof, SyntheticAskContract.wordCount(text))), observed)
                assertTrue(events.finals.isEmpty())
            }
        }
    }

    @Test fun ordinaryAppHasNoBenchmarkOverrideWithoutInstrumentationInstallation() {
        val client = SyntheticAssistantClient()
        assertFalse(SyntheticAskBenchmark.active(client))
        assertNull(SyntheticAskBenchmark.context(client))
        SyntheticAskBenchmark.requireAttached(client)
    }

    @Test fun unauthenticatedInstrumentationCannotArmReadOrRetireASession() {
        val stranger = android.app.Instrumentation()
        assertTrue(runCatching { SyntheticAskBenchmark.prepareTrial(stranger) }.isFailure)
        assertTrue(runCatching { SyntheticAskBenchmark.status(stranger) }.isFailure)
        assertTrue(runCatching { SyntheticAskBenchmark.finishTrial(stranger) }.isFailure)
        assertTrue(runCatching { SyntheticAskBenchmark.close(stranger) }.isFailure)
    }
}
