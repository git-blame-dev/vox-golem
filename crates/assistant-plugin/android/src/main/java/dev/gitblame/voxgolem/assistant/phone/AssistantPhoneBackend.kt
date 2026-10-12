package dev.gitblame.voxgolem.assistant.phone

import android.content.Context
import org.json.JSONObject

class AssistantPhoneFailure(val code: String, val boundedMessage: String) : RuntimeException()

/** Transport boundary only; request/history ownership stays in AssistantRuntime. */
interface AssistantPhoneBackend {
    /** Serialize operations on the existing engine actor; reply only after canonical projection/IO. */
    fun command(name: String, arguments: JSONObject, reply: (Result<Any?>) -> Unit)
    /** Exactly one correlated terminal callback, after save or definitive retirement. Never poll busy. */
    fun submit(clientRequestId: String, prompt: String, update: (JSONObject) -> Unit, terminal: (Result<Any?>) -> Unit)
    fun submit(clientRequestId: String, prompt: String, update: (JSONObject) -> Unit, terminal: (Result<Any?>) -> Unit, searchMode: String?) {
        submit(clientRequestId, prompt, update, terminal)
    }
    fun acknowledge(clientRequestId: String, providerRequestId: String, revision: String, accepted: Boolean): Boolean
    fun cancelDelivery(clientRequestId: String)
    fun foreground(visible: Boolean)
}

object AssistantPhoneBindings {
    private var processBackend: EnginePhoneBackend? = null
    @Synchronized fun get(context: Context): EnginePhoneBackend {
        return processBackend ?: EnginePhoneBackend(context.applicationContext).also { processBackend = it }
    }
}
