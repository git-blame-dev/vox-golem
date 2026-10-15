package dev.gitblame.voxgolem.assistant.runtime

import android.content.Context
import android.content.ContextWrapper
import android.content.pm.ApplicationInfo
import java.io.File
import org.junit.Assert.*
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TemporaryFolder
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35], manifest = Config.NONE)
class AndroidProviderReadinessTest {
    @get:Rule val folder = TemporaryFolder()
    @Test fun installedRouteRequiresExecutableAndProvisionedAccessButNeverClaimsLocalServerHealth() {
        val files = folder.newFolder("synthetic-files")
        val native = folder.newFolder("synthetic-native")
        val context = object : ContextWrapper(RuntimeEnvironment.getApplication()) {
            override fun getFilesDir() = files
            override fun getApplicationInfo() = ApplicationInfo().apply { nativeLibraryDir = native.absolutePath }
        }
        assertFalse(AndroidProviderReadiness.inspect(context).getValue("agy-flash-high").available)
        val executable = File(native, "libagy.so").apply { writeText("synthetic executable placeholder, never launched") }
        assertTrue(executable.setExecutable(true))
        assertFalse(AndroidProviderReadiness.inspect(context).getValue("agy-flash-high").available)
        val token = File(files, "home/.gemini/antigravity-cli/antigravity-oauth-token")
        assertTrue(token.parentFile!!.mkdirs()); token.writeText("unmistakably synthetic access fixture")
        assertFalse(AndroidProviderReadiness.inspect(context).getValue("agy-flash-high").available)
        File(token.parentFile, "installation_id").writeText("00000000-0000-4000-8000-000000000042")
        val snapshot = AndroidProviderReadiness.inspect(context)
        assertTrue(snapshot.getValue("agy-flash-high").available)
        assertFalse(snapshot.getValue("local-fast").available)
        token.writeText("")
        assertFalse(AndroidProviderReadiness.inspect(context).getValue("agy-flash-high").available)
        assertTrue(snapshot.getValue("agy-flash-high").available) // Immutable publication, not a live file-backed getter.
    }
}
