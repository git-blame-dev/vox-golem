package dev.gitblame.voxgolem.android.wear

import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment
import org.robolectric.annotation.Config
import org.robolectric.shadows.ShadowLog
import org.robolectric.shadows.ShadowSpeechRecognizer
import org.robolectric.shadows.ShadowTextToSpeech

// Keep SDK 32 in its own worker: Robolectric 4.14 cannot cold-switch from
// this SDK to 35 without colliding on the native runtime font ZIP filesystem.
@RunWith(RobolectricTestRunner::class)
@Config(sdk = [32])
class StandaloneCapabilityProbeLegacyApiTest {
    @Test fun olderApiProbeReportsUnsupportedWithoutCreatingEngines() {
        val recognizer = ShadowSpeechRecognizer.getLatestSpeechRecognizer()
        val tts = ShadowTextToSpeech.getLastTextToSpeechInstance()
        StandaloneCapabilityProbe(RuntimeEnvironment.getApplication()).run()
        assertSame(recognizer, ShadowSpeechRecognizer.getLatestSpeechRecognizer())
        assertSame(tts, ShadowTextToSpeech.getLastTextToSpeechInstance())
        assertTrue(ShadowLog.getLogsForTag(StandaloneCapabilityProbe.TAG).any { it.msg.contains("supportedProbeApi") })
    }
}
