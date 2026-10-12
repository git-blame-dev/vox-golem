package dev.gitblame.voxgolem.assistant.runtime

import dev.gitblame.voxgolem.assistant.AssistantStoreRunner
import dev.gitblame.voxgolem.assistant.AssistantStore
import org.junit.Assert.*
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TemporaryFolder
import org.junit.runner.RunWith
import org.robolectric.annotation.Config
import org.robolectric.RuntimeEnvironment
import java.io.File
import java.nio.file.Files
import java.nio.file.StandardCopyOption
import java.util.concurrent.Executor
import java.util.ArrayDeque
import org.json.JSONObject

@RunWith(AssistantStoreRunner::class)
@Config(sdk = [24, 35], manifest = Config.NONE)
class ProviderAuthSourceTest {
    @get:Rule val folder = TemporaryFolder()
    private val primary get() = File(folder.root, "assistant-auth.json")
    private val legacy get() = File(folder.root, "codex-auth.json")
    private fun envelope(access: String = "synthetic-access", expiry: Long = System.currentTimeMillis() + 3_600_000,
        metadata: Boolean = false) = """{${if (metadata) "\"_assistant\":{\"version\":1,\"installationId\":\"00000000-0000-4000-8000-000000000001\",\"generation\":\"7\"}," else ""}"openai":{"access":"$access","expires":$expiry}}"""

    @Test fun missingInvalidAndExpiredFilesUseTheRealRustParserWithoutMutation() {
        val source = ProviderAuthSource(primary, legacy)
        assertEquals(ProviderAuthReason.MISSING, source.status().reason)
        assertFalse(primary.exists()); assertFalse(legacy.exists())
        for ((contents, reason) in listOf("not-json" to ProviderAuthReason.INVALID,
            envelope(expiry = 1) to ProviderAuthReason.EXPIRED,
            envelope(access = "") to ProviderAuthReason.INVALID)) {
            primary.writeText(contents)
            val before = primary.readBytes(); val modified = primary.lastModified()
            repeat(5) { assertEquals(reason, source.status().reason) }
            assertArrayEquals(before, primary.readBytes()); assertEquals(modified, primary.lastModified())
        }
    }

    @Test fun validMetadataTakesPrecedenceAndMetadataOnlyStoreDoesNotMaskLegacyInjection() {
        val source = ProviderAuthSource(primary, legacy)
        legacy.writeText(envelope("synthetic-legacy"))
        assertEquals(ProviderAuthReason.READY, source.status().reason)
        assertEquals(legacy.path, source.pathForProvider())
        primary.writeText("""{"_assistant":{"version":1,"installationId":"00000000-0000-4000-8000-000000000001","generation":"0"}}""")
        assertEquals(ProviderAuthReason.READY, source.status().reason)
        assertEquals(legacy.path, source.pathForProvider())
        primary.writeText(envelope("synthetic-new", metadata = true))
        val before = primary.readBytes(); val oldBefore = legacy.readBytes()
        assertEquals(ProviderAuthReason.READY, source.status().reason)
        assertEquals(primary.path, source.pathForProvider())
        // Expiry on the selected modern file is not hidden by an older valid copy.
        primary.writeText(envelope(expiry = 1, metadata = true))
        assertEquals(ProviderAuthReason.EXPIRED, source.status().reason)
        assertEquals(primary.path, source.pathForProvider())
        primary.writeBytes(before)
        repeat(5) { source.status() }
        assertArrayEquals(before, primary.readBytes()); assertArrayEquals(oldBefore, legacy.readBytes())
    }

    @Test fun invalidRemovedOversizedAndUnreadablePrimaryNeverFallsBackToValidLegacy() {
        legacy.writeText(envelope("synthetic-legacy"))
        val legacyBefore = legacy.readBytes()
        val source = ProviderAuthSource(primary, legacy)
        val invalid = listOf("not-json", "{}", envelope(access = ""), envelope(access = "synthetic\\nheader"),
            """{"openai":{"access":"synthetic-access","accountId":"synthetic\raccount","expires":4102444800000}}""",
            "x".repeat(65_537),
            """{"_assistant":{"version":1,"installationId":"00000000-0000-4000-8000-000000000001","generation":"1"}}""",
            """{"_assistant":{"version":1,"generation":"0"}}""",
            """{"_assistant":{"version":2,"installationId":"00000000-0000-4000-8000-000000000001","generation":"0"}}""",
            """{"_assistant":{"version":1,"installationId":"not-an-id","generation":"0"}}""",
            """{"_assistant":{"version":1,"installationId":"00000000-0000-4000-8000-000000000001","generation":"0"},"openai":null}""")
        for (contents in invalid) {
            primary.writeText(contents)
            val before = primary.readBytes()
            assertEquals(ProviderAuthReason.INVALID, source.status().reason)
            assertEquals(primary.path, source.pathForProvider())
            assertArrayEquals(before, primary.readBytes()); assertArrayEquals(legacyBefore, legacy.readBytes())
        }
        primary.writeText(envelope(metadata = true))
        val before = primary.readBytes()
        val permissions = Files.getPosixFilePermissions(primary.toPath())
        try {
            Files.setPosixFilePermissions(primary.toPath(), emptySet())
            assertEquals(ProviderAuthReason.INVALID, source.status().reason)
            assertEquals(primary.path, source.pathForProvider())
            assertTrue(Files.getPosixFilePermissions(primary.toPath()).isEmpty())
        } finally { Files.setPosixFilePermissions(primary.toPath(), permissions) }
        assertArrayEquals(before, primary.readBytes()); assertArrayEquals(legacyBefore, legacy.readBytes())
    }

    @Test fun preflightPathAndStatusStayPinnedWhenLaterProjectionReselectsLegacy() {
        primary.writeText(envelope("synthetic-primary", metadata = true))
        legacy.writeText(envelope("synthetic-legacy"))
        var switchAfterPreflight = false
        val source = ProviderAuthSource(primary, legacy) { path ->
            val result = AssistantProviderNative.observeAuth(path)
            if (switchAfterPreflight && path == primary.path) {
                switchAfterPreflight = false
                primary.writeText("""{"_assistant":{"version":1,"installationId":"00000000-0000-4000-8000-000000000001","generation":"0"}}""")
            }
            result
        }
        var requestedPath: String? = null
        val native = object : ProviderNativeCalls {
            override fun respond(requestId: String, authPath: String, requestJson: String, callback: ProviderDeltaCallback): String {
                callback.onReady(); requestedPath = authPath
                return """{"status":"error","code":"auth_unavailable"}"""
            }
            override fun cancel(requestId: String) = Unit
        }
        val models = object : AssistantModelStorage {
            override fun read() = "custom-sol-high"
            override fun write(value: String) = Unit
        }
        val immediate = Executor { it.run() }
        AssistantStore(RuntimeEnvironment.getApplication()).use { store ->
            val engine = AssistantEngine(store, immediate, AssistantRequestWorker(immediate, native), source::pathForProvider, models, source)
            engine.resume(); engine.setDraft("Synthetic path pinning")
            switchAfterPreflight = true
            engine.send()
            assertEquals(primary.path, requestedPath)
            assertEquals(legacy.path, source.selection().path)
            assertEquals("Synthetic path pinning", engine.snapshot().draft)
            engine.background()
        }
    }

    @Test fun queuedProviderUsesReplacementRevisionAndLateRejectionCannotPoisonNextReplacement() {
        class Queue : Executor {
            val tasks = ArrayDeque<Runnable>()
            override fun execute(command: Runnable) { tasks.add(command) }
            fun drain() { while (tasks.isNotEmpty()) tasks.removeFirst().run() }
        }
        fun replace(access: String) {
            val replacement = File(folder.root, "synthetic-external-replacement").apply { writeText(envelope(access, metadata = true)) }
            Files.move(replacement.toPath(), primary.toPath(), StandardCopyOption.REPLACE_EXISTING, StandardCopyOption.ATOMIC_MOVE)
        }
        for (replaceBeforeSettlement in listOf(false, true)) {
            replace("synthetic-admission-A")
            val source = ProviderAuthSource(primary, legacy)
            val io = Queue(); val provider = Queue()
            var usedRevision: String? = null
            var usedPath: String? = null
            val native = object : ProviderNativeCalls {
                override fun respond(requestId: String, authPath: String, requestJson: String, callback: ProviderDeltaCallback): String {
                    callback.onReady()
                    usedPath = authPath
                    usedRevision = JSONObject(AssistantProviderNative.observeAuth(authPath)).getString("revision")
                    return JSONObject().put("status", "error").put("code", "unauthorized").put("authRevision", usedRevision).toString()
                }
                override fun cancel(requestId: String) = Unit
            }
            val models = object : AssistantModelStorage {
                override fun read() = "custom-sol-high"
                override fun write(value: String) = Unit
            }
            AssistantStore(RuntimeEnvironment.getApplication()).use { store ->
                val engine = AssistantEngine(store, io, AssistantRequestWorker(provider, native), source::pathForProvider, models, source)
                engine.resume(); engine.setDraft("Synthetic queued request"); io.drain()
                val admissionRevision = source.status().generation
                engine.send(); io.drain(); assertTrue(engine.snapshot().busy)
                replace("synthetic-provider-B")
                val b = source.status().generation
                assertNotEquals(admissionRevision, b)
                provider.drain()
                assertEquals(primary.path, usedPath)
                assertNotNull(usedRevision)
                if (replaceBeforeSettlement) replace("synthetic-current-C")
                io.drain()
                assertEquals(if (replaceBeforeSettlement) ProviderAuthReason.READY else ProviderAuthReason.UNAUTHORIZED,
                    engine.snapshot().credential?.reason)
                assertEquals("Synthetic queued request", engine.snapshot().draft)
                if (!replaceBeforeSettlement) replace("synthetic-current-C")
                engine.refresh(); io.drain()
                assertEquals(ProviderAuthReason.READY, engine.snapshot().credential?.reason)
                source.observeProviderResult(b, 401)
                assertEquals(ProviderAuthReason.READY, source.status().reason)
                engine.background(); io.drain()
            }
        }
    }

    @Test fun replacedExternalRevisionClears401AndLateOldRejectionCannotPoisonIt() {
        primary.writeText(envelope(metadata = true))
        val source = ProviderAuthSource(primary, legacy)
        val oldRevision = source.status().generation
        source.observeProviderResult(oldRevision, 401)
        assertEquals(ProviderAuthReason.UNAUTHORIZED, source.status().reason)
        val replacement = File(folder.root, "synthetic-external-replacement").apply { writeText(envelope("synthetic-replaced", metadata = true)) }
        Files.move(replacement.toPath(), primary.toPath(), StandardCopyOption.REPLACE_EXISTING, StandardCopyOption.ATOMIC_MOVE)
        val fresh = source.status()
        assertEquals(ProviderAuthReason.READY, fresh.reason); assertNotEquals(oldRevision, fresh.generation)
        source.observeProviderResult(oldRevision, 401)
        assertEquals(ProviderAuthReason.READY, source.status().reason)
        source.observeProviderResult(fresh.generation, 403)
        assertEquals(ProviderAuthReason.FORBIDDEN, source.status().reason)
        source.observeProviderResult(fresh.generation, 200)
        assertEquals(ProviderAuthReason.READY, source.status().reason)
    }

    @Test fun startupAndRepeatedObservationsNeverRecoverRewriteOrDeleteCredentialSidecars() {
        primary.writeText(envelope(metadata = true)); legacy.writeText(envelope("synthetic-legacy"))
        val backup = File(primary.path + ".bak").apply { writeText("Synthetic retained obsolete backup") }
        val staging = File(folder.root, ".assistant-auth.stage").apply { writeText("Synthetic retained obsolete staging") }
        val files = listOf(primary, legacy, backup, staging)
        val before = files.map { it.readBytes() }; val modified = files.map { it.lastModified() }
        val modes = files.map { Files.getPosixFilePermissions(it.toPath()) }
        repeat(5) { assertEquals(ProviderAuthReason.READY, ProviderAuthSource(primary, legacy).status().reason) }
        val immediate = Executor { it.run() }
        val source = ProviderAuthSource(primary, legacy)
        AssistantStore(RuntimeEnvironment.getApplication()).use { store ->
            val native = object : ProviderNativeCalls {
                override fun respond(requestId: String, authPath: String, requestJson: String, callback: ProviderDeltaCallback): String = error("Startup must not call provider")
                override fun cancel(requestId: String) = Unit
            }
            val models = object : AssistantModelStorage {
                override fun read() = "custom-sol-high"
                override fun write(value: String) = error("Startup must not mutate model")
            }
            val engine = AssistantEngine(store, immediate, AssistantRequestWorker(immediate, native), source::pathForProvider, models, source)
            engine.resume()
            repeat(5) { engine.refresh(); assertEquals(ProviderAuthReason.READY, engine.snapshot().credential?.reason) }
            engine.background()
        }
        files.forEachIndexed { index, file ->
            assertArrayEquals(before[index], file.readBytes()); assertEquals(modified[index], file.lastModified())
            assertEquals(modes[index], Files.getPosixFilePermissions(file.toPath()))
        }
    }
}
