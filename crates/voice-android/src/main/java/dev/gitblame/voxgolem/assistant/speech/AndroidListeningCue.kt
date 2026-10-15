package dev.gitblame.voxgolem.assistant.speech

import android.content.Context
import android.media.AudioAttributes
import android.media.SoundPool
import android.util.Log
import dev.gitblame.voxgolem.voice.R
import dev.gitblame.voxgolem.assistant.AssistantTiming
import dev.gitblame.voxgolem.assistant.AssistantTiming.Operation
import dev.gitblame.voxgolem.assistant.AssistantTiming.Stage

interface ListeningCue : AutoCloseable {
    fun timing(parent: AssistantTiming?) {}
    fun play()
    fun finish() { stop() }
    fun stop()
    override fun close()
}

/** Two preloaded microphone cues; one pending start survives cold load until finish/stop/close. */
class AndroidListeningCue internal constructor(
    context: Context,
    createPool: (AudioAttributes) -> ListeningCueSoundPool,
) : ListeningCue {
    constructor(context: Context) : this(context, ::AndroidCueSoundPool)

    private val lock = Any()
    private var pool: ListeningCueSoundPool? = null
    private var sound = 0
    private var endSound = 0
    private var stream = 0
    private var ready = false
    private var endReady = false
    private var pendingStart = false
    private var parentTiming: AssistantTiming? = null
    private var startTiming: AssistantTiming? = null
    private val loadTiming = AssistantTiming.begin(context.applicationContext, Operation.MICROPHONE_CUE).also { it.mark(Stage.CREATE_REQUESTED) }

    override fun timing(parent: AssistantTiming?) { synchronized(lock) { parentTiming = parent } }

    init {
        synchronized(lock) {
            try {
                val attributes = AndroidLocalTtsFactory.speechAttributes()
                val created = createPool(attributes)
                pool = created
                created.setLoadListener { loadedSound, status ->
                    synchronized(lock) {
                        if (pool === created) {
                            when (loadedSound) {
                                sound -> if (status == 0) {
                                    ready = true
                                    loadTiming.finish(Stage.CUE_LOAD_READY)
                                    if (pendingStart && sound != 0) {
                                        pendingStart = false
                                        try {
                                            stopStream(created)
                                            stream = created.play(sound)
                                            if (stream == 0) fail("play")
                                            else startTiming?.finish(Stage.CUE_PLAY_SUBMITTED)
                                        } catch (_: RuntimeException) {
                                            fail("play")
                                        }
                                    }
                                } else fail("load")
                                endSound -> if (status == 0) endReady = true else fail("load")
                            }
                        }
                    }
                }
                sound = created.load(context.applicationContext, R.raw.start_listening)
                if (sound == 0) fail("load")
                if (pool === created) {
                    endSound = created.load(context.applicationContext, R.raw.stop_listening)
                    if (endSound == 0) fail("load")
                }
            } catch (_: RuntimeException) {
                fail("initialization")
            }
        }
    }

    override fun play() = playSound(false)
    override fun finish() = playSound(true)

    private fun playSound(ending: Boolean) {
        synchronized(lock) {
            val active = pool ?: return
            try {
                stopStream(active)
                if (ending) {
                    startTiming?.finish(Stage.CANCELLED)
                    pendingStart = false
                    if (!endReady) return
                    stream = active.play(endSound)
                } else {
                    startTiming?.finish(Stage.REPLACED)
                    startTiming = (parentTiming ?: loadTiming).fork(Operation.MICROPHONE_CUE).also { it.mark(Stage.CUE_PLAY_REQUESTED) }
                    if (!ready) {
                        pendingStart = true
                        return
                    }
                    pendingStart = false
                    stream = active.play(sound)
                    if (stream != 0) startTiming?.finish(Stage.CUE_PLAY_SUBMITTED)
                }
                if (stream == 0) fail("play")
            } catch (_: RuntimeException) {
                fail("play")
            }
        }
    }

    override fun stop() {
        synchronized(lock) {
            pendingStart = false
            startTiming?.finish(Stage.CANCELLED)
            val active = pool ?: return
            try { stopStream(active) } catch (_: RuntimeException) { fail("stop") }
        }
    }

    override fun close() { synchronized(lock) { release() } }

    private fun stopStream(active: ListeningCueSoundPool) {
        if (stream != 0) active.stop(stream)
        stream = 0
    }

    private fun fail(stage: String) {
        startTiming?.finish(Stage.FAILED)
        loadTiming.finish(Stage.FAILED)
        Log.w(TAG, "Listening cue unavailable ($stage)")
        release()
    }

    private fun release() {
        startTiming?.finish(Stage.CANCELLED)
        loadTiming.finish(Stage.CANCELLED)
        pendingStart = false
        val retired = pool ?: return
        pool = null
        ready = false
        endReady = false
        sound = 0
        endSound = 0
        stream = 0
        try { retired.setLoadListener(null) } catch (_: RuntimeException) {
            Log.w(TAG, "Listening cue listener cleanup failed")
        }
        try { retired.release() } catch (_: RuntimeException) {
            Log.w(TAG, "Listening cue release failed")
        }
    }

    private companion object { const val TAG = "AndroidListeningCue" }
}

internal interface ListeningCueSoundPool {
    fun setLoadListener(listener: ((Int, Int) -> Unit)?)
    fun load(context: Context, resource: Int): Int
    fun play(sound: Int): Int
    fun stop(stream: Int)
    fun release()
}

private class AndroidCueSoundPool(attributes: AudioAttributes) : ListeningCueSoundPool {
    private val pool = SoundPool.Builder().setMaxStreams(1).setAudioAttributes(attributes).build()
    override fun setLoadListener(listener: ((Int, Int) -> Unit)?) {
        pool.setOnLoadCompleteListener(if (listener == null) null else SoundPool.OnLoadCompleteListener { _, sound, status ->
            listener(sound, status)
        })
    }
    override fun load(context: Context, resource: Int) = pool.load(context, resource, 1)
    override fun play(sound: Int) = pool.play(sound, 1f, 1f, 1, 0, 1f)
    override fun stop(stream: Int) = pool.stop(stream)
    override fun release() = pool.release()
}
