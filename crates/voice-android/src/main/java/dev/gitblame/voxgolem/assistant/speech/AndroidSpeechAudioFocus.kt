package dev.gitblame.voxgolem.assistant.speech

import android.content.Context
import android.content.BroadcastReceiver
import android.content.Intent
import android.content.IntentFilter
import android.media.AudioFocusRequest
import android.media.AudioManager
import android.os.Build
import android.os.Handler
import android.os.Looper

class AndroidSpeechAudioFocus(private val context: Context) : SpeechAudioFocus {
    private val audio = context.getSystemService(AudioManager::class.java)
    private var request: AudioFocusRequest? = null
    private var listener: AudioManager.OnAudioFocusChangeListener? = null
    private var noisy: BroadcastReceiver? = null
    @Suppress("DEPRECATION")
    override fun acquire(onLoss: () -> Unit): Boolean {
        AndroidLocalTtsFactory.requireMain(); release()
        val callback = AudioManager.OnAudioFocusChangeListener { change -> if (change < 0) onLoss() }
        listener = callback
        val result = if (Build.VERSION.SDK_INT >= 26) {
            val focus = AudioFocusRequest.Builder(AudioManager.AUDIOFOCUS_GAIN_TRANSIENT)
                .setAudioAttributes(AndroidLocalTtsFactory.speechAttributes())
                .setWillPauseWhenDucked(true).setAcceptsDelayedFocusGain(false)
                .setOnAudioFocusChangeListener(callback, Handler(Looper.getMainLooper())).build()
            request = focus; audio.requestAudioFocus(focus)
        } else audio.requestAudioFocus(callback, AudioManager.STREAM_MUSIC, AudioManager.AUDIOFOCUS_GAIN_TRANSIENT)
        if (result != AudioManager.AUDIOFOCUS_REQUEST_GRANTED) { release(); return false }
        val receiver = object : BroadcastReceiver() {
            override fun onReceive(context: Context?, intent: Intent?) {
                if (intent?.action == AudioManager.ACTION_AUDIO_BECOMING_NOISY) onLoss()
            }
        }
        try {
            if (Build.VERSION.SDK_INT >= 33) context.registerReceiver(receiver,
                IntentFilter(AudioManager.ACTION_AUDIO_BECOMING_NOISY), Context.RECEIVER_NOT_EXPORTED)
            else context.registerReceiver(receiver, IntentFilter(AudioManager.ACTION_AUDIO_BECOMING_NOISY))
        } catch (failure: RuntimeException) { release(); throw failure }
        noisy = receiver
        return true
    }
    @Suppress("DEPRECATION")
    override fun release() {
        AndroidLocalTtsFactory.requireMain()
        if (Build.VERSION.SDK_INT >= 26) request?.let { audio.abandonAudioFocusRequest(it) }
        else listener?.let { audio.abandonAudioFocus(it) }
        request = null; listener = null
        val receiver = noisy; noisy = null
        receiver?.let { runCatching { context.unregisterReceiver(it) } }
    }
}
