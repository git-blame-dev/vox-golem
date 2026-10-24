package dev.gitblame.voxgolem.assistant.phone

import java.io.File
import java.io.RandomAccessFile
import java.nio.file.Files
import java.util.UUID
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(manifest = Config.NONE, sdk = [35])
class PhoneBenchmarkProfileTest {
    private class Fixture(val root: File) {
        val data = root.resolve("data").apply { mkdir() }
        val cache = root.resolve("cache").apply { mkdir() }
        val lockRoot = root.resolve("no_backup").apply { mkdir() }
        val profiles = PhoneBenchmarkProfile(data, cache, lockRoot)
        fun freshProfile(
            deleteRoot: ((File) -> Boolean)? = null,
            deletePreferences: ((String) -> Boolean)? = null
        ): PhoneBenchmarkProfile = if (deleteRoot != null) {
            PhoneBenchmarkProfile(data, cache, lockRoot, deleteRoot = deleteRoot, deletePreferences = deletePreferences)
        } else {
            PhoneBenchmarkProfile(data, cache, lockRoot, deletePreferences = deletePreferences)
        }
        fun reserve(): String = UUID.randomUUID().toString().also { profiles.reserve(it) }
        fun exited() = profiles.close()
        fun web(token: String) = data.resolve("app_webview_synthetic_phone_${token.replace("-", "")}")
        fun webCache(token: String) = cache.resolve("WebView_synthetic_phone_${token.replace("-", "")}")
        fun webNestedCache(token: String) = cache.resolve("WebView/synthetic_phone_${token.replace("-", "")}")
        fun prefs(token: String, name: String = "assistant-phone-preferences") =
            data.resolve("shared_prefs/synthetic-phone-ask-$token-$name.xml")
        fun native(token: String) = profiles.nativeDirectory(token)
        fun recoverFailure(instance: PhoneBenchmarkProfile = freshProfile()) = phoneCleanupReceipt { instance.cleanupPending() }
    }

    private fun fixture(action: Fixture.() -> Unit) {
        val root = Files.createTempDirectory("synthetic-session-test").toFile()
        try {
            val f = Fixture(root)
            try { f.action() } finally { f.profiles.close() }
        } finally { root.deleteRecursively() }
    }

    @Test fun heldProducerLeaseBlocksAnotherSessionAndCleanupBeforeResourcesAreCreated() = fixture {
        val token = reserve()
        assertThrows(IllegalStateException::class.java) { profiles.reserve(UUID.randomUUID().toString()) }
        assertThrows(IllegalStateException::class.java) { freshProfile().reserve(UUID.randomUUID().toString()) }
        assertEquals("producer_not_exited", recoverFailure().getString("cleanup_failure_reason"))
        assertEquals("producer_not_exited", phoneCleanupReceipt { freshProfile().cleanup(token) }.getString("cleanup_failure_reason"))
        assertTrue(lockRoot.resolve("phone-benchmark.lock").isFile)
        assertTrue(cache.listFiles()!!.isEmpty())
    }

    @Test fun reservationCreatesOnlyLockFileAndUsesPhoneExclusiveNames() = fixture {
        val token = reserve()
        assertEquals("synthetic-phone-ask-$token", native(token).name)
        assertFalse(native(token).exists()); assertFalse(web(token).exists()); assertFalse(prefs(token).exists())
        assertEquals(listOf("phone-benchmark.lock"), lockRoot.listFiles()!!.map { it.name })
        assertEquals(
            setOf("assistant-phone-preferences", "voxgolem_google_tts_voice_cache", "voxgolem_google_stt_support_cache"),
            PhoneBenchmarkProfile.PREFERENCE_NAMES
        )
    }

    @Test fun staleEntriesInAnyOwnedLocationBlockReservationAfterProducerExit() = fixture {
        val stale = UUID.randomUUID().toString()
        val locations = listOf(
            native(stale) to true,
            web(stale) to true,
            webCache(stale) to true,
            webNestedCache(stale) to true,
            prefs(stale) to false,
            File(prefs(stale).path + ".bak") to false
        )
        for ((entry, isDir) in locations) {
            if (isDir) entry.mkdirs() else entry.apply { parentFile.mkdirs(); writeText("synthetic") }
            assertThrows(IllegalStateException::class.java) { freshProfile().reserve(UUID.randomUUID().toString()) }
            assertTrue(freshProfile().cleanupPending().getBoolean("storage_cleanup_verified"))
        }
    }

    @Test fun externalFileLockExcludesProducerAndResetAndLockInodeSurvivesCleanup() = fixture {
        RandomAccessFile(lockRoot.resolve("phone-benchmark.lock"), "rw").use { file ->
            file.channel.lock().use {
                assertThrows(IllegalStateException::class.java) { reserve() }
                assertEquals("producer_not_exited", recoverFailure().getString("cleanup_failure_reason"))
            }
        }
        val token = reserve(); native(token).mkdir(); exited()
        assertTrue(freshProfile().cleanupPending().getBoolean("cleanup_verified"))
        assertTrue(lockRoot.resolve("phone-benchmark.lock").isFile)
    }

    @Test fun recoverAllOwnedResourcesButNeverOrdinaryOrSharedSyntheticAskData() = fixture {
        val token = reserve()
        web(token).mkdir(); webCache(token).mkdir(); webNestedCache(token).mkdirs(); native(token).mkdir()
        native(token).resolve("assistant.db").writeText("synthetic")
        native(token).resolve("fixture.wav").writeText("synthetic")
        PhoneBenchmarkProfile.PREFERENCE_NAMES.forEach { name ->
            prefs(token, name).apply { parentFile.mkdirs(); writeText("synthetic") }
            File(prefs(token, name).path + ".bak").writeText("synthetic")
        }
        val keep = listOf(
            data.resolve("app_webview/Default/keep"),
            cache.resolve("WebView/Default/keep"),
            data.resolve("no_backup/assistant.db"),
            data.resolve("no_backup/chatgpt/keep"),
            data.resolve("shared_prefs/assistant-phone-preferences.xml"),
            cache.resolve("synthetic-ask-${UUID.randomUUID()}/keep")
        )
        keep.forEach { it.parentFile.mkdirs(); it.writeText("synthetic") }
        exited()
        val result = freshProfile().cleanupPending()
        assertTrue(result.getBoolean("cleanup_verified")); assertTrue(result.getBoolean("storage_cleanup_verified"))
        assertFalse(result.getBoolean("native_cleanup_verified")); assertTrue(result.getBoolean("producer_exit_verified"))
        assertFalse(web(token).exists()); assertFalse(webCache(token).exists()); assertFalse(webNestedCache(token).exists())
        assertFalse(native(token).exists()); assertFalse(prefs(token).exists())
        keep.forEach { assertTrue(it.isFile) }
    }

    @Test fun normalCleanupRequiresExitedProducerAndMatchingTokenThenDeletesLateRoots() = fixture {
        val token = reserve()
        assertThrows(IllegalStateException::class.java) { profiles.cleanup(token) }
        web(token).mkdir()
        webCache(token).mkdir()
        native(token).mkdir()
        exited()
        assertThrows(IllegalStateException::class.java) { freshProfile().cleanup(UUID.randomUUID().toString()) }
        assertTrue(web(token).exists()); assertTrue(native(token).exists())
        val cleaned = freshProfile().cleanup(token)
        assertEquals("cleaned", cleaned.getString("benchmark_result"))
        assertTrue(cleaned.getBoolean("storage_cleanup_verified"))
        assertFalse(web(token).exists()); assertFalse(webCache(token).exists()); assertFalse(native(token).exists())
    }

    @Test fun observeChecksRealWebViewInMemoryWithoutDiskMutationOrLazyCacheRoots() = fixture {
        val token = reserve()
        assertFalse(profiles.observe(token))
        web(token).mkdir()
        assertTrue(profiles.observe(token))
        assertTrue(cache.listFiles()!!.isEmpty())
        assertEquals(listOf("phone-benchmark.lock"), lockRoot.listFiles()!!.map { it.name })
    }

    @Test fun malformedOwnedNamesAndWrongEntryTypesFailClosedWithoutDeletingValidSiblings() = fixture {
        val valid = reserve(); native(valid).mkdir(); web(valid).mkdir(); exited()
        val malformed = listOf(
            cache.resolve("synthetic-phone-ask-not-a-uuid") to true,
            data.resolve("app_webview_synthetic_phone_short") to true,
            cache.resolve("WebView_synthetic_phone_nothex") to true,
            cache.resolve("WebView/synthetic_phone_bad") to true,
            data.resolve("shared_prefs/synthetic-phone-ask-$valid-unknown.xml") to false
        )
        for ((bad, isDir) in malformed) {
            if (isDir) bad.mkdirs() else bad.apply { parentFile.mkdirs(); writeText("synthetic") }
            assertEquals("marker_namespace_invalid", recoverFailure().getString("cleanup_failure_reason"))
            assertTrue(native(valid).exists()); assertTrue(web(valid).exists())
            bad.deleteRecursively()
        }
        val wrongTypes = listOf(
            cache.resolve("synthetic-phone-ask-${UUID.randomUUID()}") to false,
            data.resolve("app_webview_synthetic_phone_${UUID.randomUUID().toString().replace("-", "")}") to false,
            prefs(UUID.randomUUID().toString()) to true
        )
        for ((bad, isDir) in wrongTypes) {
            if (isDir) bad.mkdirs() else bad.apply { parentFile.mkdirs(); writeText("synthetic") }
            assertEquals("owned_path_invalid", recoverFailure().getString("cleanup_failure_reason"))
            assertTrue(native(valid).exists()); assertTrue(web(valid).exists())
            bad.deleteRecursively()
        }
        assertTrue(freshProfile().cleanupPending().getBoolean("cleanup_verified"))
    }

    @Test fun aggregateBudgetCoversNativePrefsAndAllDiscoveredSessionsBeforeDeletion() = fixture {
        val first = reserve(); native(first).mkdir(); exited()
        RandomAccessFile(native(first).resolve("synthetic-large"), "rw").use { it.setLength(40L * 1024 * 1024) }
        val second = UUID.randomUUID().toString()
        prefs(second).apply { parentFile.mkdirs() }
        RandomAccessFile(prefs(second), "rw").use { it.setLength(25L * 1024 * 1024) }
        assertEquals("profile_budget_exceeded", recoverFailure().getString("cleanup_failure_reason"))
        assertTrue(native(first).exists()); assertTrue(prefs(second).exists())
    }

    @Test fun entryBudgetIsSharedAcrossNativeAndWebViewRoots() = fixture {
        val token = reserve(); native(token).mkdir(); web(token).mkdir(); exited()
        repeat(5_000) { native(token).resolve("synthetic-$it").createNewFile() }
        repeat(5_000) { web(token).resolve("synthetic-$it").createNewFile() }
        assertEquals("profile_budget_exceeded", recoverFailure().getString("cleanup_failure_reason"))
        assertTrue(native(token).exists()); assertTrue(web(token).exists())
    }

    @Test fun discoveredSessionCountIsBoundedBeforeDeletion() = fixture {
        repeat(9) { native(UUID.randomUUID().toString()).mkdir() }
        assertEquals("profile_budget_exceeded", recoverFailure().getString("cleanup_failure_reason"))
        assertEquals(9, cache.listFiles()!!.size)
    }

    @Test fun missingNativeStorageIsIdempotentButMalformedResourceParentsAreNotAbsence() = fixture {
        data.resolve("shared_prefs").writeText("synthetic-not-directory")
        assertEquals("owned_path_invalid", recoverFailure().getString("cleanup_failure_reason"))
        data.resolve("shared_prefs").delete()
        assertEquals("not_started", freshProfile().cleanupPending().getString("benchmark_result"))
    }

    @Test fun danglingRootAndDescendantSymlinksAreRejected() = fixture {
        val token = UUID.randomUUID().toString()
        Files.createSymbolicLink(native(token).toPath(), root.resolve("missing").toPath())
        assertEquals("owned_path_invalid", recoverFailure().getString("cleanup_failure_reason"))
        Files.delete(native(token).toPath()); native(token).mkdir()
        Files.createSymbolicLink(native(token).resolve("escape").toPath(), root.resolve("missing").toPath())
        assertEquals("owned_path_invalid", recoverFailure().getString("cleanup_failure_reason"))
        assertTrue(native(token).exists())
    }

    @Test fun falseSuccessfulDeleteFailsVerification() = fixture {
        val token = reserve(); native(token).mkdir(); exited()
        val lying = freshProfile(deleteRoot = { true })
        val failure = recoverFailure(lying)
        assertEquals("owned_root_delete_failed", failure.getString("cleanup_failure_reason"))
        assertTrue(native(token).exists()); assertFalse(failure.getBoolean("cleanup_verified"))
    }

    @Test fun postcheckRejectsDirectorySwapToSymlinkDespiteSuccessfulCallback() = fixture {
        val token = reserve(); native(token).mkdir(); exited()
        val foreign = root.resolve("foreign").apply { mkdir(); resolve("keep").writeText("synthetic") }
        val swapping = freshProfile(deleteRoot = { directory ->
            directory.deleteRecursively()
            Files.createSymbolicLink(directory.toPath(), foreign.toPath())
            true
        })
        assertEquals("owned_root_delete_failed", recoverFailure(swapping).getString("cleanup_failure_reason"))
        assertTrue(foreign.resolve("keep").isFile)
        Files.delete(native(token).toPath())
    }

    @Test fun partialDeletionReportsProgressAndRetryDeletesRemainingResources() = fixture {
        val token = reserve(); web(token).mkdir(); native(token).mkdir(); exited()
        val failing = freshProfile(deleteRoot = { if (it == native(token)) false else it.deleteRecursively() })
        val failure = recoverFailure(failing)
        assertEquals(1, failure.getInt("deleted_root_count"))
        assertEquals(1, failure.getInt("deleted_owned_directory_count"))
        assertEquals(0, failure.getInt("deleted_session_count"))
        assertFalse(web(token).exists()); assertTrue(native(token).exists())
        assertTrue(freshProfile().cleanupPending().getBoolean("cleanup_verified"))
        assertFalse(native(token).exists())
    }

    @Test fun postcheckCatchesPreferenceLateWriter() = fixture {
        val token = reserve(); native(token).mkdir(); exited()
        val late = freshProfile(deleteRoot = {
            it.deleteRecursively()
            prefs(token).apply { parentFile.mkdirs(); writeText("synthetic") }
            true
        }, deletePreferences = { true })
        assertEquals("owned_root_delete_failed", recoverFailure(late).getString("cleanup_failure_reason"))
        assertTrue(prefs(token).exists())
    }

    @Test fun receiptsNeverExportPayloadsOrInventFacts() {
        val failure = phoneCleanupReceipt { error("synthetic-private-detail") }
        assertFalse(failure.toString().contains("synthetic-private-detail"))
        assertFalse(failure.containsKey("producer_exit_verified")); assertFalse(failure.containsKey("deletion_started"))
        assertEquals("entry_precondition", failure.getString("cleanup_failure_reason"))
        val facts = android.os.Bundle().apply {
            putString("path", "synthetic-private-detail")
            putString("producer_exit_verified", "true")
            putInt("deleted_owned_directory_count", 33)
            putInt("deleted_preference_count", 25)
            putInt("deleted_session_count", 9)
        }
        val filtered = phoneCleanupReceipt { throw PhoneCleanupFailure(PhoneCleanupReason.UNKNOWN, facts) }
        assertFalse(filtered.toString().contains("synthetic-private-detail"))
        for (key in listOf("producer_exit_verified", "deleted_owned_directory_count", "deleted_preference_count", "deleted_session_count")) {
            assertFalse(filtered.containsKey(key))
        }
    }
}
