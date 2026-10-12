package dev.gitblame.voxgolem.assistant.runtime

interface ProviderDeltaCallback {
    fun onReady()
    fun onDelta(text: String)
    fun onStatus(status: String) {}
    fun onFirstText(milliseconds: Long) {}
}

interface ProviderNativeCalls {
    fun respond(requestId: String, authPath: String, requestJson: String, callback: ProviderDeltaCallback): String
    fun cancel(requestId: String)
}

object AssistantProviderNative : ProviderNativeCalls {
    init { System.loadLibrary("wear_assistant_native") }
    external fun observeAuth(authPath: String): String
    external override fun respond(requestId: String, authPath: String, requestJson: String, callback: ProviderDeltaCallback): String
    external override fun cancel(requestId: String)
}
