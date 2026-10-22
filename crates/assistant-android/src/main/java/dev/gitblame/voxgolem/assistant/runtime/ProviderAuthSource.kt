package dev.gitblame.voxgolem.assistant.runtime

import android.content.Context
import org.json.JSONObject
import java.io.File

enum class ProviderAuthReason { MISSING, READY, EXPIRED, INVALID, UNAUTHORIZED, FORBIDDEN }
data class ProviderAuthStatus(val reason: ProviderAuthReason, val generation: String)
internal data class ProviderAuthSelection(val path: String, val status: ProviderAuthStatus)

/** Read-only view of the single credential source: the app-managed device-code file. */
internal class ProviderAuthSource(
    private val managed: File,
    private val managedAllowed: () -> Boolean = { true },
    private val observe: (String) -> String = AssistantProviderNative::observeAuth,
) {
    constructor(context: Context) : this(ChatGptAuth.managedFile(context),
        { dev.gitblame.voxgolem.assistant.replication.ChatGptAccessSync.managedAllowed(context) })

    private var rejected: Pair<String, ProviderAuthReason>? = null

    @Synchronized fun selection(): ProviderAuthSelection {
        val path = managed.path
        val observation = if (!managedAllowed()) JSONObject().put("code", "auth_unavailable") else JSONObject(observe(path))
        val revision = observation.optString("revision").takeUnless { observation.isNull("revision") }
        val generation = revision?.let { "$path:$it" } ?: ""
        val reason = when (observation.optString("code")) {
            "auth_expired" -> ProviderAuthReason.EXPIRED
            "auth_unavailable" -> if (managed.exists()) ProviderAuthReason.INVALID else ProviderAuthReason.MISSING
            "", "null" -> if (rejected?.first == generation) rejected!!.second else ProviderAuthReason.READY
            else -> ProviderAuthReason.INVALID
        }
        if (rejected?.first != generation) rejected = null
        return ProviderAuthSelection(path, ProviderAuthStatus(reason, generation))
    }

    @Synchronized fun status(): ProviderAuthStatus = selection().status

    fun pathForProvider(): String = managed.path

    @Synchronized fun observeProviderResult(path: String, revision: String, httpStatus: Int) {
        if (revision.isNotEmpty() && revision.length <= 512) observeProviderResult("$path:$revision", httpStatus)
    }

    @Synchronized fun observeProviderResult(generation: String, httpStatus: Int) {
        val current = status()
        if (generation.isEmpty() || generation != current.generation) return
        when (httpStatus) {
            401 -> rejected = generation to ProviderAuthReason.UNAUTHORIZED
            403 -> rejected = generation to ProviderAuthReason.FORBIDDEN
            in 200..299 -> rejected = null
        }
    }
}
