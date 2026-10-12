package dev.gitblame.voxgolem.assistant.speech

import android.content.*
import android.os.IBinder
import android.speech.tts.TextToSpeech
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment
import org.robolectric.annotation.Config
import java.util.concurrent.Executor

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35], manifest = Config.NONE)
class ExplicitTtsProviderContextTest {
    @Test @Config(sdk = [24, 33], manifest = Config.NONE) fun olderSdkGuardUsesOnlyAvailableBindingTypesAndStillRejectsFallback() {
        var bindings = 0
        val base = object : ContextWrapper(RuntimeEnvironment.getApplication()) {
            override fun bindService(service: Intent, connection: ServiceConnection, flags: Int): Boolean { bindings++; return true }
        }
        val guarded = ExplicitTtsProviderContext.create(base, "synthetic.selected.tts")
        val connection = object : ServiceConnection {
            override fun onServiceConnected(name: ComponentName?, service: IBinder?) {}
            override fun onServiceDisconnected(name: ComponentName?) {}
        }
        assertSame(guarded, guarded.applicationContext)
        assertFalse(guarded.bindService(Intent(TextToSpeech.Engine.INTENT_ACTION_TTS_SERVICE).setPackage("synthetic.fallback.tts"), connection, Context.BIND_AUTO_CREATE))
        assertTrue(guarded.bindService(Intent(TextToSpeech.Engine.INTENT_ACTION_TTS_SERVICE).setPackage("synthetic.selected.tts"), connection, Context.BIND_AUTO_CREATE))
        assertEquals(1, bindings)
    }
    @Test fun explicitProviderCannotBindDefaultOrFallbackServicesThroughAnyPublicBindingOverload() {
        val targets = mutableListOf<String?>()
        val base = object : ContextWrapper(RuntimeEnvironment.getApplication()) {
            override fun bindService(service: Intent, connection: ServiceConnection, flags: Int): Boolean { targets += service.`package`; return true }
            override fun bindService(service: Intent, flags: Int, executor: Executor, connection: ServiceConnection): Boolean { targets += service.`package`; return true }
            override fun bindService(service: Intent, flags: Context.BindServiceFlags, executor: Executor, connection: ServiceConnection): Boolean { targets += service.`package`; return true }
        }
        val guarded = ExplicitTtsProviderContext.create(base, "synthetic.selected.tts")
        assertSame(guarded, guarded.applicationContext)
        val connection = object : ServiceConnection {
            override fun onServiceConnected(name: ComponentName?, service: IBinder?) {}
            override fun onServiceDisconnected(name: ComponentName?) {}
        }
        val executor = Executor { it.run() }
        for (provider in listOf("synthetic.selected.tts", "synthetic.fallback.tts")) {
            val intent = Intent(TextToSpeech.Engine.INTENT_ACTION_TTS_SERVICE).setPackage(provider)
            val expected = provider == "synthetic.selected.tts"
            assertEquals(expected, guarded.bindService(intent, connection, Context.BIND_AUTO_CREATE))
            assertEquals(expected, guarded.bindService(intent, Context.BIND_AUTO_CREATE, executor, connection))
            assertEquals(expected, guarded.bindService(intent, Context.BindServiceFlags.of(Context.BIND_AUTO_CREATE.toLong()), executor, connection))
        }
        assertEquals(List(3) { "synthetic.selected.tts" }, targets)
        assertFalse(guarded.bindService(Intent(TextToSpeech.Engine.INTENT_ACTION_TTS_SERVICE), connection, Context.BIND_AUTO_CREATE))
    }
}
