package dev.gitblame.voxgolem.assistant.speech

import dev.gitblame.voxgolem.assistant.recognition.*
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35], manifest = Config.NONE)
class MicrophoneCueControllerTest {
    private class Cue : ListeningCue {
        var starts = 0; var ends = 0; var closed = false
        override fun play() { check(!closed); starts++ }
        override fun finish() { check(!closed); ends++ }
        override fun stop() {}
        override fun close() { closed = true }
    }
    private fun ready(id: Long) = DictationState(DictationPhase.LISTENING, utterance = id, readyForSpeech = true)
    @Test fun readyAndNormalFinalEachPlayOnceButNeverAfterFinishOrNewUtterance() {
        val cue = Cue(); val controller = MicrophoneCueController(cue); controller.foreground(true)
        controller.onDictation(ready(1)); controller.onDictation(ready(1))
        controller.finish(1)
        assertEquals(0, cue.ends)
        controller.onDictation(ready(1))
        controller.onDictation(DictationState(DictationPhase.FINISHING, utterance = 1))
        assertEquals(0, cue.ends)
        controller.onDictation(DictationState(DictationPhase.FINAL, utterance = 1))
        controller.onDictation(DictationState(DictationPhase.FINAL, utterance = 1))
        assertEquals(1, cue.starts); assertEquals(1, cue.ends)
        controller.onDictation(DictationState(DictationPhase.CHECKING, utterance = 2))
        controller.onDictation(ready(1)); controller.onDictation(DictationState(DictationPhase.FINAL, utterance = 1))
        controller.onDictation(ready(2)); controller.onDictation(DictationState(DictationPhase.FINAL, utterance = 2))
        assertEquals(2, cue.starts); assertEquals(2, cue.ends)
    }
    @Test fun cancelBeforeReadinessBackgroundAndCloseFenceLateFeedback() {
        val cue = Cue(); val controller = MicrophoneCueController(cue); controller.foreground(true)
        controller.onDictation(DictationState(DictationPhase.LISTENING, utterance = 1))
        controller.cancel(); controller.onDictation(ready(1)); controller.finish(1)
        assertEquals(0, cue.starts); assertEquals(0, cue.ends)
        controller.onDictation(ready(2)); controller.foreground(false); controller.onDictation(ready(3)); controller.finish(2)
        controller.foreground(true); controller.onDictation(ready(2)); controller.onDictation(ready(3))
        assertEquals(1, cue.starts); assertEquals(0, cue.ends)
        controller.onDictation(ready(4))
        assertEquals(2, cue.starts); assertEquals(0, cue.ends)
        controller.close(); controller.onDictation(ready(5)); controller.finish(4)
        assertEquals(2, cue.starts); assertEquals(0, cue.ends); assertTrue(cue.closed)
    }
    @Test fun failedRecognitionDoesNotPlayAnEndOrReviveAReadyCue() {
        val cue = Cue(); val controller = MicrophoneCueController(cue); controller.foreground(true)
        controller.onDictation(ready(1))
        controller.onDictation(DictationState(DictationPhase.ERROR, utterance = 1, error = "recognition_error_7"))
        controller.onDictation(ready(1))
        assertEquals(1, cue.starts); assertEquals(0, cue.ends)
    }
    @Test fun finishBeforeQueuedReadySuppressesStartButOnlyRetiredFinalCanPlayEnd() {
        val cue = Cue(); val controller = MicrophoneCueController(cue); controller.foreground(true)
        controller.onDictation(DictationState(DictationPhase.LISTENING, utterance = 1))
        controller.finish(1)
        controller.onDictation(ready(1))
        controller.onDictation(DictationState(DictationPhase.FINISHING, utterance = 1))
        assertEquals(0, cue.starts); assertEquals(0, cue.ends)
        controller.onDictation(DictationState(DictationPhase.FINAL, "Synthetic final", utterance = 1))
        assertEquals(0, cue.starts); assertEquals(1, cue.ends)
        controller.cancel()
        controller.onDictation(DictationState(DictationPhase.FINAL, "Synthetic duplicate", utterance = 1))
        assertEquals(1, cue.ends)
    }
    @Test fun coldStartSoundPoolPendingStartPlaysOnLoadCompletionUnlessFinishedOrStoppedEarly() {
        val context = org.robolectric.RuntimeEnvironment.getApplication()
        var loadListener: ((Int, Int) -> Unit)? = null
        val played = mutableListOf<Int>()
        val pool = object : ListeningCueSoundPool {
            override fun setLoadListener(listener: ((Int, Int) -> Unit)?) { loadListener = listener }
            override fun load(context: android.content.Context, resource: Int): Int =
                if (resource == dev.gitblame.voxgolem.voice.R.raw.start_listening) 11 else 12
            override fun play(sound: Int): Int { played += sound; return 1 }
            override fun stop(stream: Int) {}
            override fun release() {}
        }
        val controller = MicrophoneCueController(AndroidListeningCue(context) { pool })
        controller.foreground(true)
        controller.onDictation(ready(1))
        assertTrue(played.isEmpty())
        loadListener!!(11, 0)
        assertEquals(listOf(11), played)
        controller.close()

        var secondListener: ((Int, Int) -> Unit)? = null
        val secondPlayed = mutableListOf<Int>()
        val secondPool = object : ListeningCueSoundPool {
            override fun setLoadListener(listener: ((Int, Int) -> Unit)?) { secondListener = listener }
            override fun load(context: android.content.Context, resource: Int): Int =
                if (resource == dev.gitblame.voxgolem.voice.R.raw.start_listening) 21 else 22
            override fun play(sound: Int): Int { secondPlayed += sound; return 1 }
            override fun stop(stream: Int) {}
            override fun release() {}
        }
        val secondController = MicrophoneCueController(AndroidListeningCue(context) { secondPool })
        secondController.foreground(true)
        secondController.onDictation(ready(1))
        secondController.finish(1)
        secondListener!!(21, 0)
        assertTrue(secondPlayed.isEmpty())
        secondController.close()
    }
}
