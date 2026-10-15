package dev.gitblame.voxgolem.assistant.runtime

interface ProviderDeltaCallback {
    /** Cancellation registration is established; this does not assert model/process readiness. */
    fun onReady()
    /** Receipt near HTTP dispatch, before response headers; JNI crossing is included in observation time. */
    fun onRequestDispatched() {}
    fun onDelta(text: String)
    fun onStatus(status: String) {}
    fun onFirstText(milliseconds: Long) {}
}

interface ProviderNativeCalls {
    fun respond(requestId: String, authPath: String, requestJson: String, callback: ProviderDeltaCallback): String
    fun cancel(requestId: String)
    fun prepareSearch(requestJson: String, callback: ProviderDeltaCallback): String =
        """{"evidenceContext":null,"sourceFooter":"","sources":[]}"""
    /** A returned outcome need not mean an external process has physically retired. */
    fun whenRetired(requestId: String, retired: () -> Unit) { retired() }
}

object AssistantProviderNative : ProviderNativeCalls {
    init { System.loadLibrary("wear_assistant_native") }
    external fun observeAuth(authPath: String): String
    external override fun respond(requestId: String, authPath: String, requestJson: String, callback: ProviderDeltaCallback): String
    external override fun cancel(requestId: String)
    external fun nativePrepareSearch(requestJson: String, callback: ProviderDeltaCallback): String
    override fun prepareSearch(requestJson: String, callback: ProviderDeltaCallback): String =
        nativePrepareSearch(requestJson, callback)
}
