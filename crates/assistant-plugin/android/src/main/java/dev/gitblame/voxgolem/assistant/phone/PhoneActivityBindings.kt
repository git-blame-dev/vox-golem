package dev.gitblame.voxgolem.assistant.phone

import android.app.Activity
import android.webkit.WebView
import java.util.WeakHashMap

/** Activity-local platform effects. No production launch installs an override. */
interface PhoneActivityBinding {
    val backend: EnginePhoneBackend
    val speech: PhoneSpeechSession
    fun webViewCreated(webView: WebView)
    fun admitVoice(policy: String, draft: String, loop: Boolean, shortcut: String?)
    fun admitPrompt(requestId: String, prompt: String)
    fun terminal(requestId: String, result: Result<Any?>)
}

/** The optional implementation is compiled only into explicitly opted-in benchmark artifacts. */
object PhoneActivityBindings {
    private val registry = PhoneActivityBindingRegistry()
    internal fun install(factory: (Activity) -> PhoneActivityBinding) = registry.install(factory)
    fun beforeCreate(activity: Activity) = registry.beforeCreate(activity)
    internal fun get(activity: Activity): PhoneActivityBinding? = registry.get(activity)
    fun webViewCreated(activity: Activity, webView: WebView) { get(activity)?.webViewCreated(webView) }
    fun isIsolated(activity: Activity): Boolean = get(activity) != null
    internal fun remove(activity: Activity) = registry.remove(activity)
}

internal class PhoneActivityBindingRegistry {
    private val bindings = WeakHashMap<Activity, PhoneActivityBinding>()
    private var startup: ((Activity) -> PhoneActivityBinding)? = null

    @Synchronized fun install(factory: (Activity) -> PhoneActivityBinding) {
        check(startup == null && bindings.isEmpty())
        startup = factory
    }

    @Synchronized fun beforeCreate(activity: Activity) {
        startup?.let { factory ->
            check(!bindings.containsKey(activity))
            bindings[activity] = factory(activity)
        }
    }

    @Synchronized fun get(activity: Activity): PhoneActivityBinding? {
        check(startup == null || bindings.containsKey(activity))
        return bindings[activity]
    }

    @Synchronized fun remove(activity: Activity) { bindings.remove(activity) }
}
