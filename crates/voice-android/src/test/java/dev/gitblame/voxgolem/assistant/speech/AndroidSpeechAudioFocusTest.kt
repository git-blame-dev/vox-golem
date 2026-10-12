package dev.gitblame.voxgolem.assistant.speech

import android.content.Intent
import android.media.AudioManager
import android.os.Looper
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment
import org.robolectric.Shadows.shadowOf
import org.robolectric.annotation.Config
import org.robolectric.annotation.LooperMode

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35], manifest = Config.NONE)
@LooperMode(LooperMode.Mode.PAUSED)
class AndroidSpeechAudioFocusTest {
    @Suppress("DEPRECATION")
    @Test fun noisyRouteChangeSignalsStopWithoutForcingSpeakerOrBluetoothAndUnregisters() {
        val context = RuntimeEnvironment.getApplication()
        val audio = context.getSystemService(AudioManager::class.java)
        val focus = AndroidSpeechAudioFocus(context)
        var losses = 0
        assertTrue(focus.acquire { losses++ })
        context.sendBroadcast(Intent(AudioManager.ACTION_AUDIO_BECOMING_NOISY))
        shadowOf(Looper.getMainLooper()).idle(); assertEquals(1, losses)
        assertFalse(audio.isSpeakerphoneOn); assertFalse(audio.isBluetoothScoOn)
        focus.release(); assertNotNull(shadowOf(audio).lastAbandonedAudioFocusRequest)
        context.sendBroadcast(Intent(AudioManager.ACTION_AUDIO_BECOMING_NOISY))
        shadowOf(Looper.getMainLooper()).idle(); assertEquals(1, losses)
    }
    @Test fun deniedFocusIsReleasedAndDoesNotRegisterRouteListener() {
        val context = RuntimeEnvironment.getApplication()
        val audio = context.getSystemService(AudioManager::class.java)
        shadowOf(audio).setNextFocusRequestResponse(AudioManager.AUDIOFOCUS_REQUEST_FAILED)
        val focus = AndroidSpeechAudioFocus(context)
        var losses = 0
        assertFalse(focus.acquire { losses++ })
        context.sendBroadcast(Intent(AudioManager.ACTION_AUDIO_BECOMING_NOISY))
        shadowOf(Looper.getMainLooper()).idle(); assertEquals(0, losses)
    }
}
