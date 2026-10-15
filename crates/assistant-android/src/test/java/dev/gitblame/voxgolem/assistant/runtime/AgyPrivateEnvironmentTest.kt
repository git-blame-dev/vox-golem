package dev.gitblame.voxgolem.assistant.runtime

import android.content.Context
import android.content.ContextWrapper
import android.content.res.AssetManager
import java.io.ByteArrayInputStream
import java.io.ByteArrayOutputStream
import java.io.File
import java.util.concurrent.ThreadPoolExecutor
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicInteger
import java.util.zip.ZipEntry
import java.util.zip.ZipOutputStream
import org.json.JSONObject
import org.junit.Assert.*
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TemporaryFolder
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import org.robolectric.util.ReflectionHelpers
import org.robolectric.util.ReflectionHelpers.ClassParameter

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35], manifest = Config.NONE)
class AgyPrivateEnvironmentTest {
    @get:Rule val folder = TemporaryFolder()
    private val request = """{"model":"agy-flash-high","webEnabled":true,"messages":[{"role":"user","content":"Synthetic question"}]}"""
    private class Callback : ProviderDeltaCallback {
        var registered = 0
        override fun onReady() { registered++ }
        override fun onDelta(text: String) {}
    }
    private class Engine : Process() {
        val input = ByteArrayOutputStream()
        var exited = false
        override fun getOutputStream() = input
        override fun getInputStream() = ByteArrayInputStream((
            """{"event":"step_update","step_update":{"step_type":"agent_response","text_delta":"Synthetic answer"}}""" + "\n" +
            """{"event":"result","result":{"status":"SUCCESS"}}""" + "\n").toByteArray())
        override fun getErrorStream() = ByteArrayInputStream(byteArrayOf())
        override fun exitValue(): Int = if (exited) 0 else throw IllegalThreadStateException()
        override fun waitFor(): Int = exitValue()
        override fun waitFor(timeout: Long, unit: TimeUnit) = exited
        override fun destroy() { exited = true }
        override fun destroyForcibly(): Process { destroy(); return this }
    }

    @Test fun syntheticPackagedAccessAndIdentityCannotProvisionPrivateFilesOrEnableReadiness() {
        val context = agyTestContext(folder.newFolder(), provisioned = false)
        // Deliberately unsafe synthetic test assets live only in this temporary ZIP, never app sources.
        val archive = folder.newFile("synthetic-assets.apk")
        ZipOutputStream(archive.outputStream()).use { zip ->
            for (name in listOf("antigravity-oauth-token", "installation_id", "settings.json")) {
                zip.putNextEntry(ZipEntry("assets/agy/$name"))
                zip.write("unmistakably synthetic forbidden packaged source".toByteArray())
                zip.closeEntry()
            }
        }
        val assets = context.assets
        val cookie = ReflectionHelpers.callInstanceMethod<Int>(assets, "addAssetPath", ClassParameter.from(String::class.java, archive.absolutePath))
        assertTrue("The synthetic asset fixture must actually be available", cookie > 0)
        assets.open("agy/antigravity-oauth-token").use { assertEquals("unmistakably synthetic forbidden packaged source", it.bufferedReader().readText()) }
        val assetAccesses = AtomicInteger()
        val guarded = object : ContextWrapper(context) {
            override fun getApplicationContext(): Context = this
            override fun getAssets(): AssetManager { assetAccesses.incrementAndGet(); return assets }
        }
        val launches = AtomicInteger()
        AgyProviderNative(guarded, { File("/synthetic/agycli") }, { _, _, _ -> launches.incrementAndGet(); Engine() }).use { native ->
            val callback = Callback()
            assertEquals("auth_unavailable", JSONObject(native.respond("synthetic-unprovisioned", "", request, callback)).getString("code"))
            assertEquals(1, callback.registered)
            native.prewarm("agy-flash-high", true)
            val io = native.javaClass.getDeclaredField("io").apply { isAccessible = true }.get(native) as ThreadPoolExecutor
            io.submit { }.get(2, TimeUnit.SECONDS) // Completed prewarm path, not an enqueue acknowledgement.
            assertEquals(0, launches.get()); assertEquals(0, assetAccesses.get())
            val privateDirectory = File(guarded.filesDir, "home/.gemini/antigravity-cli")
            assertFalse(File(privateDirectory, "antigravity-oauth-token").exists())
            assertFalse(File(privateDirectory, "installation_id").exists())
            assertFalse(File(privateDirectory, "settings.json").exists())
            assertFalse(AndroidProviderReadiness.inspect(guarded).getValue("agy-flash-high").available)
        }
    }

    @Test fun externallyProvisionedPrivateAccessAndIdentityPassActualPreparationWithoutAssetReads() {
        val context = agyTestContext(folder.newFolder(), provisioned = false)
        val guarded = object : ContextWrapper(context) {
            override fun getApplicationContext(): Context = this
            override fun getAssets(): AssetManager = error("Private preparation must not consult APK assets")
        }
        val directory = File(context.filesDir, "home/.gemini/antigravity-cli").apply { check(mkdirs()) }
        val token = File(directory, "antigravity-oauth-token").apply { writeText("synthetic external access") }
        val identity = File(directory, "installation_id")
        var launches = 0
        val processes = mutableListOf<Engine>()
        AgyProviderNative(guarded, { File("/synthetic/agycli") }, { _, _, _ ->
            check(File(context.filesDir, "ws").isDirectory)
            launches++; Engine().also { processes += it }
        }).use { native ->
            assertEquals("auth_unavailable", JSONObject(native.respond("synthetic-missing-identity", "", request, Callback())).getString("code"))
            identity.writeText("")
            assertEquals("auth_unavailable", JSONObject(native.respond("synthetic-empty-identity", "", request, Callback())).getString("code"))
            assertEquals(0, launches)
            assertFalse(AndroidProviderReadiness.inspect(guarded).getValue("agy-flash-high").available)
            identity.writeText("00000000-0000-4000-8000-000000000042")
            assertEquals("complete", JSONObject(native.respond("synthetic-provisioned", "", request, Callback())).getString("status"))
            assertEquals(1, launches); assertTrue(processes.single().exited)
            assertTrue(processes.single().input.size() > 0)
            assertTrue(AndroidProviderReadiness.inspect(guarded).getValue("agy-flash-high").available)
            assertEquals("synthetic external access", token.readText())
            assertEquals("00000000-0000-4000-8000-000000000042", identity.readText())
            token.writeText("")
            assertEquals("auth_unavailable", JSONObject(native.respond("synthetic-empty-access", "", request, Callback())).getString("code"))
            assertEquals(1, launches)
        }
    }
}
