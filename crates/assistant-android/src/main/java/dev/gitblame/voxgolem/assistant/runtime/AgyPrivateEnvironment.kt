package dev.gitblame.voxgolem.assistant.runtime

import android.content.Context
import java.io.File

/** Access and installation identity must be provisioned externally in app-private storage.
 * Never source either from APK assets. The CLI uses its defaults or externally supplied settings.
 */
internal object AgyPrivateEnvironment {
    fun isProvisioned(context: Context): Boolean {
        val directory = File(context.filesDir, "home/.gemini/antigravity-cli")
        return listOf("antigravity-oauth-token", "installation_id").all { name ->
            File(directory, name).let { it.isFile && it.length() > 0 }
        }
    }

    fun prepare(context: Context): Boolean {
        if (!isProvisioned(context)) return false
        val workspace = File(context.filesDir, "ws")
        check(workspace.isDirectory || workspace.mkdirs()) { "Agy workspace preparation failed" }
        return true
    }
}
