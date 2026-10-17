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
    fun prewarm(model: String, authPath: String = "") {}
    fun prewarm(model: String, authPath: String, webEnabled: Boolean) { prewarm(model, authPath) }
    fun respond(requestId: String, authPath: String, requestJson: String, callback: ProviderDeltaCallback): String
    fun cancel(requestId: String)
    fun prepareSearch(requestJson: String, callback: ProviderDeltaCallback): String =
        """{"evidenceContext":null,"sourceFooter":"","sources":[]}"""
    /** A returned outcome need not mean an external process has physically retired. */
    fun whenRetired(requestId: String, retired: () -> Unit) { retired() }
}

object AssistantProviderNative : ProviderNativeCalls {
    init { System.loadLibrary("wear_assistant_native") }
    private fun ensureLoaded() {}
    override fun prewarm(model: String, authPath: String) {
        prewarm(model, authPath, true)
    }
    override fun prewarm(model: String, authPath: String, webEnabled: Boolean) {
        if (authPath.isNotEmpty() && !model.startsWith("agy-") && model != "local-fast" && model != GeminiNanoProvider.MODEL_NAME) {
            runCatching { observeAuth(authPath) }
        }
        if (model != "local-fast" && !model.startsWith("agy-")) {
            runCatching { nativePrewarm(model, authPath, webEnabled) }
        }
    }
    external fun nativePrewarm(model: String, authPath: String, webEnabled: Boolean)
    external fun observeAuth(authPath: String): String
    external fun chatGptAuth(authPath: String, action: String): String
    /** Access-only envelope, literal null for signed out, or {error:...}; never JS-facing. */
    external fun exportChatGptAccess(authPath: String): String
    external override fun respond(requestId: String, authPath: String, requestJson: String, callback: ProviderDeltaCallback): String
    external override fun cancel(requestId: String)
    external fun nativePrepareSearch(requestJson: String, callback: ProviderDeltaCallback): String
    override fun prepareSearch(requestJson: String, callback: ProviderDeltaCallback): String =
        nativePrepareSearch(requestJson, callback)
    external fun nativeShouldRunWebSearch(
        webEnabled: Boolean,
        searchMode: String,
        query: String,
        prevUserQuery: String,
    ): Boolean
    fun shouldRunWebSearch(
        webEnabled: Boolean,
        searchMode: String,
        query: String,
        prevUserQuery: String? = null,
    ): Boolean {
        ensureLoaded()
        return nativeShouldRunWebSearch(webEnabled, searchMode, query, prevUserQuery ?: "")
    }
    external fun nativeRoutePrompt2d(
        query: String,
        prevUserQuery: String,
    ): String
    fun routePrompt2d(
        query: String,
        prevUserQuery: String? = null,
    ): String {
        ensureLoaded()
        return nativeRoutePrompt2d(query, prevUserQuery ?: "")
    }
}
