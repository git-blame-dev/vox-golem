package dev.gitblame.voxgolem.assistant.inputtransport

import android.os.Bundle
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [24, 35], manifest = Config.NONE)
class SegmentedRecognitionCallbacksTest {
    @Test fun javaBridgeLoadsOnOldApiAndDispatchesBothOptionalCallbacks() {
        var received: Bundle? = null
        var ended = false
        val callback = object : AssistantSegmentedListener() {
            override fun segment(results: Bundle) { received = results }
            override fun segmentedEnd() { ended = true }
            override fun onReadyForSpeech(params: Bundle?) {}
            override fun onBeginningOfSpeech() {}
            override fun onRmsChanged(rmsdB: Float) {}
            override fun onBufferReceived(buffer: ByteArray?) {}
            override fun onEndOfSpeech() {}
            override fun onError(error: Int) {}
            override fun onResults(results: Bundle?) {}
            override fun onPartialResults(partialResults: Bundle?) {}
            override fun onEvent(eventType: Int, params: Bundle?) {}
        }
        val results = Bundle().apply { putString("synthetic", "segment") }
        callback.onSegmentResults(results)
        callback.onEndOfSegmentedSession()
        assertSame(results, received)
        assertTrue(ended)
    }
}
