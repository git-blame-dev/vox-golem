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
    private val managed get() = File(folder.root, "chatgpt-auth.json")
    private fun envelope(access: String = "synthetic-access", expiry: Long = System.currentTimeMillis() + 3_600_000) =
        """{"voxgolemChatgpt":1,"openai":{"access":"$access","expires":$expiry}}"""

    @Test fun signedOutTombstoneAndInvalidManagedFilesAreNeverReadyOrMutated() {
        val source = ProviderAuthSource(managed)
        assertEquals(managed.path, source.selection().path)
        managed.writeText(envelope("synthetic-managed"))
        assertEquals(ProviderAuthReason.READY, source.status().reason)
        val invalid = listOf("""{"voxgolemChatgpt":1,"signedOut":true}""", "{}", "not-json", envelope(access = ""),
            envelope(access = "synthetic\\nheader"), "x".repeat(65_537),
            """{"voxgolemChatgpt":1,"openai":{"access":"synthetic-access","accountId":"synthetic\raccount","expires":4102444800000}}""")
        for (contents in invalid) {
            managed.writeText(contents)
            val before = managed.readBytes()
            assertNotEquals(ProviderAuthReason.READY, source.status().reason)
            assertEquals(managed.path, source.pathForProvider())
            assertArrayEquals(before, managed.readBytes())
        }
    }

    @Test fun unreadableManagedFileIsInvalidWithoutChangingItsPermissions() {
        managed.writeText(envelope())
        val before = managed.readBytes()
        val permissions = Files.getPosixFilePermissions(managed.toPath())
        try {
            Files.setPosixFilePermissions(managed.toPath(), emptySet())
            assertEquals(ProviderAuthReason.INVALID, ProviderAuthSource(managed).status().reason)
            assertTrue(Files.getPosixFilePermissions(managed.toPath()).isEmpty())
        } finally { Files.setPosixFilePermissions(managed.toPath(), permissions) }
        assertArrayEquals(before, managed.readBytes())
    }

    @Test fun watchAdmissionFailsClosedUntilAssociationValidation() {
        managed.writeText(envelope("synthetic-watch-access"))
        var approved = false
        val source = ProviderAuthSource(managed, { approved })
        assertNotEquals(ProviderAuthReason.READY, source.status().reason)
        assertEquals(managed.path, source.pathForProvider())
        approved = true
        assertEquals(ProviderAuthReason.READY, source.status().reason)
        approved = false
        assertNotEquals(ProviderAuthReason.READY, source.status().reason)
        assertEquals(managed.path, source.pathForProvider())
    }

    @Test fun missingInvalidAndExpiredFilesUseTheRealRustParserWithoutMutation() {
        val source = ProviderAuthSource(managed)
        assertEquals(ProviderAuthReason.MISSING, source.status().reason)
        assertFalse(managed.exists())
        for ((contents, reason) in listOf("not-json" to ProviderAuthReason.INVALID,
            envelope(expiry = 1) to ProviderAuthReason.EXPIRED,
            envelope(access = "") to ProviderAuthReason.INVALID)) {
            managed.writeText(contents)
            val before = managed.readBytes(); val modified = managed.lastModified()
            repeat(5) { assertEquals(reason, source.status().reason) }
            assertArrayEquals(before, managed.readBytes()); assertEquals(modified, managed.lastModified())
        }
    }

    @Test fun queuedProviderUsesReplacementRevisionAndLateRejectionCannotPoisonNextReplacement() {
        class Queue : Executor {
            val tasks = ArrayDeque<Runnable>()
            override fun execute(command: Runnable) { tasks.add(command) }
            fun drain() { while (tasks.isNotEmpty()) tasks.removeFirst().run() }
        }
        fun replace(access: String) {
            val replacement = File(folder.root, "synthetic-external-replacement").apply { writeText(envelope(access)) }
            Files.move(replacement.toPath(), managed.toPath(), StandardCopyOption.REPLACE_EXISTING, StandardCopyOption.ATOMIC_MOVE)
        }
        for (replaceBeforeSettlement in listOf(false, true)) {
            replace("synthetic-admission-A")
            val source = ProviderAuthSource(managed)
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
                assertEquals(managed.path, usedPath)
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
        managed.writeText(envelope())
        val source = ProviderAuthSource(managed)
        val oldRevision = source.status().generation
        source.observeProviderResult(oldRevision, 401)
        assertEquals(ProviderAuthReason.UNAUTHORIZED, source.status().reason)
        val replacement = File(folder.root, "synthetic-external-replacement").apply { writeText(envelope("synthetic-replaced")) }
        Files.move(replacement.toPath(), managed.toPath(), StandardCopyOption.REPLACE_EXISTING, StandardCopyOption.ATOMIC_MOVE)
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
        managed.writeText(envelope())
        val backup = File(managed.path + ".bak").apply { writeText("Synthetic retained obsolete backup") }
        val staging = File(folder.root, ".assistant-auth.stage").apply { writeText("Synthetic retained obsolete staging") }
        val files = listOf(managed, backup, staging)
        val before = files.map { it.readBytes() }; val modified = files.map { it.lastModified() }
        val modes = files.map { Files.getPosixFilePermissions(it.toPath()) }
        repeat(5) { assertEquals(ProviderAuthReason.READY, ProviderAuthSource(managed).status().reason) }
        val immediate = Executor { it.run() }
        val source = ProviderAuthSource(managed)
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
