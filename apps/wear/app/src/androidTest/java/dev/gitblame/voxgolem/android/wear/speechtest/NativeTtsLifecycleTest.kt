package dev.gitblame.voxgolem.android.wear.speechtest

import android.os.SystemClock
import android.net.Uri
import androidx.test.core.app.ActivityScenario
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import androidx.lifecycle.Lifecycle
import dev.gitblame.voxgolem.assistant.recognition.RecognitionResourceLease
import dev.gitblame.voxgolem.assistant.speech.AndroidLocalTtsFactory
import dev.gitblame.voxgolem.assistant.speech.AndroidSpeechAudioFocus
import dev.gitblame.voxgolem.assistant.speech.LocalSpeechOutput
import dev.gitblame.voxgolem.assistant.speech.LocalTtsFactory
import dev.gitblame.voxgolem.assistant.speech.LocalTtsSession
import dev.gitblame.voxgolem.assistant.speech.LocalVoice
import dev.gitblame.voxgolem.assistant.speech.SpeechPhase
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import java.util.concurrent.atomic.AtomicInteger
import java.util.concurrent.atomic.AtomicReference

@RunWith(AndroidJUnit4::class)
class NativeTtsLifecycleTest {
    @Test fun retiredPendingClientDisconnectsAfterDelayedNativeInitialization() {
        val instrumentation = InstrumentationRegistry.getInstrumentation()
        val fixturePackage = instrumentation.context.packageName
        val fixture = Uri.parse("content://$fixturePackage.speechfixture")
        val resolver = instrumentation.targetContext.contentResolver
        val readiness = AtomicInteger()
        var session: LocalTtsSession? = null
        ActivityScenario.launch(NativeTtsHostActivity::class.java).use { scenario ->
            assertEquals(Lifecycle.State.RESUMED, scenario.state)
            resolver.call(fixture, "enable-delayed", null, null)
            try {
                scenario.onActivity { activity ->
                    session = AndroidLocalTtsFactory(activity.applicationContext).create(fixturePackage) { readiness.incrementAndGet() }
                }
                await { resolver.call(fixture, "stats", null, null)?.getInt("creations") == 1 }
                assertEquals(0, readiness.get())
                instrumentation.runOnMainSync { session!!.shutdown() }
                await { (resolver.call(fixture, "stats", null, null)?.getInt("voiceQueries") ?: 0) > 0 }
                await { resolver.call(fixture, "stats", null, null)?.getInt("activeBindings") == 0 }
                assertEquals(0, readiness.get())
            } finally {
                try { instrumentation.runOnMainSync { session?.shutdown() } }
                finally { resolver.call(fixture, "disable", null, null) }
            }
        }
    }
    @Test fun retainedNativeSdkPlaysRepeatedSyntheticRepliesWithoutRebindingOrHoldingIdleAdmission() {
        val instrumentation = InstrumentationRegistry.getInstrumentation()
        val fixturePackage = instrumentation.context.packageName
        val creates = AtomicInteger()
        val nativeVoices = AtomicReference<List<LocalVoice>>(emptyList())
        val completions = AtomicInteger()
        val failure = AtomicReference<String?>()
        val leases = RecognitionResourceLease()
        lateinit var output: LocalSpeechOutput
        val fixture = Uri.parse("content://$fixturePackage.speechfixture")
        ActivityScenario.launch(NativeTtsHostActivity::class.java).use { scenario ->
            assertEquals(Lifecycle.State.RESUMED, scenario.state)
            instrumentation.targetContext.contentResolver.call(fixture, "enable", null, null)
            try {
                scenario.onActivity { activity ->
                    val native = AndroidLocalTtsFactory(activity.applicationContext)
                    val factory = object : LocalTtsFactory {
                        override fun providers() = listOf(fixturePackage)
                        override fun create(onReady: (Boolean) -> Unit) = create(fixturePackage, onReady)
                        override fun create(provider: String?, onReady: (Boolean) -> Unit): LocalTtsSession {
                            creates.incrementAndGet()
                            val session = native.create(fixturePackage, onReady)
                            return object : LocalTtsSession by session {
                                override fun voices(): List<LocalVoice> = session.voices().also(nativeVoices::set)
                            }
                        }
                    }
                    output = LocalSpeechOutput(factory, AndroidSpeechAudioFocus(activity), leases = leases)
                    output.setForeground(true); output.setEnabled(true); output.prepare("en-AU")
                }
                await { output.snapshot().voiceAvailable && output.snapshot().phase == SpeechPhase.IDLE }
                assertEquals(listOf(SyntheticTtsService.VOICE), nativeVoices.get().map { it.name })
                assertEquals("en-GB", output.snapshot().route?.selectedLanguage)
                assertNull(leases.current()); assertEquals(1, creates.get())
                repeat(3) { index ->
                    instrumentation.runOnMainSync {
                        output.speak("Synthetic reply ${index + 1}", "en-AU") { error -> failure.set(error); completions.incrementAndGet() }
                    }
                    await { completions.get() == index + 1 }
                    assertNull(failure.get()); assertNull(leases.current())
                    assertEquals(SpeechPhase.IDLE, output.snapshot().phase)
                    assertEquals(1, creates.get())
                }
                instrumentation.runOnMainSync { output.setForeground(false) }
                instrumentation.waitForIdleSync()
                instrumentation.runOnMainSync { output.setForeground(true) }
                instrumentation.waitForIdleSync()
                assertEquals(3, completions.get()); assertEquals(1, creates.get())
                instrumentation.runOnMainSync {
                    output.speak("Synthetic explicitly resumed reply", "en-AU") { error -> failure.set(error); completions.incrementAndGet() }
                }
                await { completions.get() == 4 }
                assertNull(failure.get()); assertEquals(2, creates.get()); assertNull(leases.current())
            } finally {
                try {
                    instrumentation.runOnMainSync { output.close() }
                    instrumentation.waitForIdleSync()
                } finally {
                    instrumentation.targetContext.contentResolver.call(fixture, "disable", null, null)
                }
            }
        }
    }

    private fun await(condition: () -> Boolean) {
        val deadline = SystemClock.elapsedRealtime() + 30_000
        while (!condition()) {
            assertTrue("Native synthetic TTS did not settle within its bound", SystemClock.elapsedRealtime() < deadline)
            SystemClock.sleep(20)
        }
    }
}
