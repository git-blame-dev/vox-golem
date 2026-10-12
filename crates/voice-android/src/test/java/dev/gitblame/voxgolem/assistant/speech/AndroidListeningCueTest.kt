package dev.gitblame.voxgolem.assistant.speech

import android.content.Context
import android.media.AudioAttributes
import dev.gitblame.voxgolem.voice.R
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35], manifest = Config.NONE)
class AndroidListeningCueTest {
    private val context: Context get() = RuntimeEnvironment.getApplication()

    @Config(sdk = [26, 35])
    @Test fun exactTtsAttributesReachTheNativeCueFactory() {
        val pool = FakePool()
        val cue = AndroidListeningCue(context) { attributes ->
            assertEquals(AudioAttributes.USAGE_ASSISTANT, attributes.usage)
            assertEquals(AndroidLocalTtsFactory.speechAttributes(), attributes)
            pool
        }
        assertEquals(R.raw.start_listening, pool.resource)
        cue.close()
    }

    @Config(sdk = [24])
    @Test fun api24UsesTheSameMediaSpeechFallbackAsTts() {
        AndroidListeningCue(context) { attributes ->
            assertEquals(AudioAttributes.USAGE_MEDIA, attributes.usage)
            assertEquals(AudioAttributes.CONTENT_TYPE_SPEECH, attributes.contentType)
            FakePool()
        }.close()
    }

    @Test fun unreadyRequestsAreDroppedAndStopBeforeLoadNeverProducesALateCue() {
        val pool = FakePool()
        val cue = AndroidListeningCue(context) { pool }
        cue.play()
        cue.stop()
        pool.loaded()
        assertEquals(0, pool.plays)
        cue.play()
        assertEquals(1, pool.plays)
        cue.close()
    }
    @Test fun endCueUsesItsOwnLoadedAssetAndUnreadyOrRetiredRequestsAreNeverQueued() {
        val pool = FakePool()
        val cue = AndroidListeningCue(context) { attributes ->
            assertEquals(AndroidLocalTtsFactory.speechAttributes(), attributes); pool
        }
        assertEquals(listOf(R.raw.start_listening, R.raw.stop_listening), pool.resources)
        cue.finish(); cue.stop()
        pool.listener!!(pool.loadId + 1, 0)
        assertTrue(pool.playedSounds.isEmpty())
        pool.loaded(); cue.play(); cue.finish(); cue.stop()
        assertEquals(listOf(pool.loadId, pool.loadId + 1), pool.playedSounds)
        val retained = pool.listener!!
        cue.close(); retained(pool.loadId + 1, 0); cue.finish()
        assertEquals(2, pool.plays); assertEquals(1, pool.releases)
    }

    @Test fun restartAndStopKeepTheLoadedSoundReusable() {
        val pool = FakePool()
        val cue = AndroidListeningCue(context) { pool }
        pool.loaded()
        cue.play()
        cue.play()
        cue.stop()
        cue.stop()
        cue.play()
        assertEquals(listOf("play:1", "stop:1", "play:2", "stop:2", "play:3"), pool.events)
        assertEquals(2, pool.loads)
        cue.close()
        cue.close()
        assertEquals(1, pool.releases)
    }

    @Test fun closeBeforeLoadIgnoresStaleCallbackAndFutureOperations() {
        val pool = FakePool()
        val cue = AndroidListeningCue(context) { pool }
        val callback = pool.listener!!
        cue.close()
        callback(7, 0)
        cue.play()
        cue.stop()
        cue.close()
        assertEquals(0, pool.plays)
        assertEquals(1, pool.releases)
        assertNull(pool.listener)
    }

    @Test fun unsuccessfulLoadAndUnknownSoundNeverPlay() {
        val pool = FakePool()
        val cue = AndroidListeningCue(context) { pool }
        pool.listener!!(99, 0)
        cue.play()
        assertEquals(0, pool.plays)
        pool.loaded(status = 1)
        cue.play()
        cue.close()
        assertEquals(0, pool.plays)
        assertEquals(1, pool.releases)
    }

    @Test fun constructionAndLoadFailuresAreNonfatalAndReleaseAcquiredResources() {
        AndroidListeningCue(context) { throw IllegalStateException("synthetic factory failure") }.apply {
            play(); stop(); close()
        }
        listOf(FakePool().apply { failLoad = true }, FakePool().apply { loadId = 0 }).forEach { pool ->
            AndroidListeningCue(context) { pool }.apply { play(); stop(); close() }
            assertEquals(1, pool.releases)
            assertEquals(0, pool.plays)
        }
    }

    @Test fun playAndStopFailuresDisablePlaybackAndReleaseOnce() {
        listOf("play", "zero", "stop").forEach { failure ->
            val pool = FakePool()
            val cue = AndroidListeningCue(context) { pool }
            pool.loaded()
            when (failure) {
                "play" -> pool.failPlay = true
                "zero" -> pool.zeroStream = true
            }
            cue.play()
            if (failure == "stop") { pool.failStop = true; cue.stop() }
            cue.play()
            cue.close()
            assertEquals(1, pool.plays)
            assertEquals(1, pool.releases)
        }
    }

    @Test fun listenerCleanupFailureDoesNotPreventNativeRelease() {
        val pool = FakePool()
        val cue = AndroidListeningCue(context) { pool }
        pool.failDetach = true
        pool.failRelease = true
        cue.close()
        cue.close()
        cue.play()
        assertEquals(1, pool.releases)
        assertEquals(0, pool.plays)
    }

    private class FakePool : ListeningCueSoundPool {
        var listener: ((Int, Int) -> Unit)? = null
        var resource = 0
        var loads = 0
        var loadId = 7
        var plays = 0
        var releases = 0
        var failLoad = false
        var failPlay = false
        var zeroStream = false
        var failStop = false
        var failDetach = false
        var failRelease = false
        val events = mutableListOf<String>()
        val resources = mutableListOf<Int>()
        val playedSounds = mutableListOf<Int>()
        override fun setLoadListener(listener: ((Int, Int) -> Unit)?) {
            if (listener == null && failDetach) error("synthetic detach failure")
            this.listener = listener
        }
        override fun load(context: Context, resource: Int): Int {
            loads++; if (loads == 1) this.resource = resource
            resources += resource
            if (failLoad) error("synthetic load failure")
            return if (loadId != 0 && resource == R.raw.stop_listening) loadId + 1 else loadId
        }
        fun loaded(status: Int = 0) { listener!!(loadId, status) }
        override fun play(sound: Int): Int {
            assertTrue(sound == loadId || sound == loadId + 1)
            playedSounds += sound
            plays++
            if (failPlay) error("synthetic play failure")
            events += "play:$plays"
            return if (zeroStream) 0 else plays
        }
        override fun stop(stream: Int) {
            if (failStop) error("synthetic stop failure")
            events += "stop:$stream"
        }
        override fun release() {
            releases++
            if (failRelease) error("synthetic release failure")
        }
    }
}
