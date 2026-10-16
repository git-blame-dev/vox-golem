package dev.gitblame.voxgolem.assistant.runtime

import android.content.Context
import org.json.JSONObject
import java.io.File

enum class ProviderAuthReason { MISSING, READY, EXPIRED, INVALID, UNAUTHORIZED, FORBIDDEN }
data class ProviderAuthStatus(val reason: ProviderAuthReason, val generation: String)
internal data class ProviderAuthSelection(val path: String, val status: ProviderAuthStatus)

/** Read-only selection. Managed native auth outranks externally published imported files. */
internal class ProviderAuthSource(
    private val primary: File,
    private val legacy: File,
    private val managed: File? = null,
    private val managedAllowed: () -> Boolean = { true },
    private val observe: (String) -> String = AssistantProviderNative::observeAuth,
) {
    constructor(context: Context) : this(File(context.applicationContext.noBackupFilesDir, "assistant-auth.json"),
        File(context.applicationContext.filesDir, "codex-auth.json"),
        File(context.applicationContext.noBackupFilesDir, "chatgpt-auth.json"),
        { dev.gitblame.voxgolem.assistant.replication.ChatGptAccessSync.managedAllowed(context) })

    private var rejected: Pair<String, ProviderAuthReason>? = null
    private var selectedPath = primary.path

    @Synchronized fun selection(): ProviderAuthSelection {
        // AtomicFile uses a backup/staging sibling on Android. A publication window must
        // never make an explicit managed choice fall back to imported credentials.
        val preferred = managed?.takeIf { it.exists() || File(it.path + ".bak").exists() || File(it.path + ".new").exists() } ?: primary
        var observation = if (preferred == managed && !managedAllowed()) JSONObject().put("code", "auth_unavailable")
            else JSONObject(observe(preferred.path))
        selectedPath = preferred.path
        // Old metadata-only stores must not mask an externally injected legacy provider file.
        if (preferred == primary && observation.optString("sourceKind") in setOf("absent", "legacy_empty") && legacy.isFile) {
            observation = JSONObject(observe(legacy.path))
            selectedPath = legacy.path
        }
        val revision = observation.optString("revision").takeUnless { observation.isNull("revision") }
        val generation = revision?.let { "$selectedPath:$it" } ?: ""
        val reason = when (observation.optString("code")) {
            "auth_expired" -> ProviderAuthReason.EXPIRED
            "auth_unavailable" -> if (File(selectedPath).exists()) ProviderAuthReason.INVALID else ProviderAuthReason.MISSING
            "", "null" -> if (rejected?.first == generation) rejected!!.second else ProviderAuthReason.READY
            else -> ProviderAuthReason.INVALID
        }
        if (rejected?.first != generation) rejected = null
        return ProviderAuthSelection(selectedPath, ProviderAuthStatus(reason, generation))
    }

    @Synchronized fun status(): ProviderAuthStatus = selection().status

    @Synchronized fun pathForProvider(): String = selectedPath

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
