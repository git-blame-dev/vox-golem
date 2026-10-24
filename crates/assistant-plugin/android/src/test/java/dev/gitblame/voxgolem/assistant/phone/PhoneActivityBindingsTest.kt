package dev.gitblame.voxgolem.assistant.phone

import android.app.Activity
import android.webkit.WebView
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.Robolectric
import org.robolectric.annotation.Config

@RunWith(PhoneAssistantRunner::class)
@Config(sdk = [35], manifest = Config.NONE)
class PhoneActivityBindingsTest {
    @Test fun normalLaunchHasNoOverrideAndInstalledLaunchCannotFallBackToGlobalBackend() {
        val registry = PhoneActivityBindingRegistry()
        val activity = Robolectric.buildActivity(Activity::class.java).get()
        registry.beforeCreate(activity)
        assertNull(registry.get(activity))
        var constructions = 0
        val binding = object : PhoneActivityBinding {
            override val backend: EnginePhoneBackend get() = error("Not accessed by binding admission")
            override val speech: PhoneSpeechSession get() = error("Not accessed by binding admission")
            override fun webViewCreated(webView: WebView) = Unit
            override fun admitVoice(policy: String, draft: String, loop: Boolean, shortcut: String?) = Unit
            override fun admitPrompt(requestId: String, prompt: String) = Unit
            override fun terminal(requestId: String, result: Result<Any?>) = Unit
        }
        registry.install { constructions++; binding }
        assertThrows(IllegalStateException::class.java) { registry.get(activity) }
        registry.beforeCreate(activity)
        assertSame(binding, registry.get(activity))
        assertEquals(1, constructions)
        assertThrows(IllegalStateException::class.java) { registry.beforeCreate(activity) }
        registry.remove(activity)
        assertThrows(IllegalStateException::class.java) { registry.get(activity) }
    }
}
