package dev.gitblame.voxgolem.android.wear.speechtest

import android.Manifest
import android.os.SystemClock
import androidx.test.core.app.ActivityScenario
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import org.junit.Assert.*
import org.junit.Assume.assumeTrue
import org.junit.Test
import org.junit.runner.RunWith
import java.util.concurrent.atomic.AtomicReference

@RunWith(AndroidJUnit4::class)
class SuppliedAudioProofTest {
    @Test fun finiteSyntheticPcmUsesTheExactSuppliedAudioIntentAndRetires() {
        val result = runProbe(false)
        assertTrue("Support or synthetic-source proof failed: ${result.code}", result.supportAccepted)
        assertTrue("Known injected content was not recognized: ${result.code}", result.sourceRecognized)
        assertTrue(result.finalAfterEof)
        assertTrue(result.retired)
        // This does not assert no-mic fallback or offline locality. Parent must independently verify both.
    }
    @Test fun explicitCancellationRetiresTheSuppliedSessionWithoutAFinal() {
        val result = runProbe(true)
        assertTrue(result.supportAccepted)
        assertEquals("cancelled", result.code)
        assertFalse(result.sourceRecognized)
        assertFalse(result.finalAfterEof)
        assertTrue(result.retired)
    }
    private fun runProbe(cancel: Boolean): SuppliedAudioProofResult {
        val instrumentation = InstrumentationRegistry.getInstrumentation()
        val context = instrumentation.targetContext
        assumeTrue("Provider proof is never run in the normal data-bearing application", context.packageName.endsWith(".inputproof"))
        val arguments = InstrumentationRegistry.getArguments()
        val language = arguments.getString("proofLanguage", "en-AU")!!
        val provider = arguments.getString("proofProviderPackage", "com.google.android.tts")!!
        val strict = arguments.getString("proofStrict", "false") == "true"
        instrumentation.uiAutomation.grantRuntimePermission(context.packageName, Manifest.permission.RECORD_AUDIO)
        val result = AtomicReference<SuppliedAudioProofResult?>()
        ActivityScenario.launch(SuppliedAudioProofActivity::class.java).use { scenario ->
            scenario.onActivity { it.probe(language, provider, strict, cancel, result::set) }
            val deadline = SystemClock.elapsedRealtime() + 35_000
            while (result.get() == null && SystemClock.elapsedRealtime() < deadline) SystemClock.sleep(25)
            return result.get() ?: throw AssertionError("Synthetic supplied-audio proof did not settle")
        }
    }
}
