package dev.gitblame.voxgolem.assistant.runtime

import android.content.Context
import android.content.ContextWrapper
import android.content.pm.ApplicationInfo
import java.io.File
import org.robolectric.RuntimeEnvironment

/** Synthetic files only; Process factories never execute this placeholder library. */
internal fun agyTestContext(root: File, provisioned: Boolean = true): Context {
    val files = File(root, "files").apply { check(mkdirs() || isDirectory) }
    val native = File(root, "native").apply { check(mkdirs() || isDirectory) }
    File(native, "libagy.so").apply { writeText("synthetic placeholder, never executed"); check(setExecutable(true)) }
    val context = object : ContextWrapper(RuntimeEnvironment.getApplication()) {
        override fun getApplicationContext(): Context = this
        override fun getFilesDir() = files
        override fun getApplicationInfo() = ApplicationInfo().apply { nativeLibraryDir = native.absolutePath }
    }
    if (provisioned) provisionAgyTestAccess(context)
    return context
}

internal fun provisionAgyTestAccess(context: Context) {
    val directory = File(context.filesDir, "home/.gemini/antigravity-cli").apply { check(mkdirs() || isDirectory) }
    File(directory, "antigravity-oauth-token").writeText("unmistakably synthetic externally provisioned access")
    File(directory, "installation_id").writeText("00000000-0000-4000-8000-000000000042")
}
