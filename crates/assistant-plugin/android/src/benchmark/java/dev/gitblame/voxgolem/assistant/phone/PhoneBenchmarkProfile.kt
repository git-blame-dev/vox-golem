package dev.gitblame.voxgolem.assistant.phone

import android.content.Context
import android.os.Bundle
import java.io.File
import java.io.RandomAccessFile
import java.nio.channels.FileChannel
import java.nio.channels.FileLock
import java.nio.channels.OverlappingFileLockException
import java.nio.file.FileVisitResult
import java.nio.file.Files
import java.nio.file.LinkOption.NOFOLLOW_LINKS
import java.nio.file.Path
import java.nio.file.SimpleFileVisitor
import java.nio.file.attribute.BasicFileAttributes
import java.util.Locale
import java.util.UUID

internal enum class PhoneCleanupReason {
    ENTRY_PRECONDITION, MARKER_DIRECTORY_INVALID,
    MARKER_NAMESPACE_INVALID, MARKER_FILE_INVALID,
    PRODUCER_NOT_EXITED, OWNED_PATH_INVALID, PROFILE_BUDGET_EXCEEDED,
    OWNED_ROOT_DELETE_FAILED, IO_UNAVAILABLE, UNKNOWN;
    fun wire() = name.lowercase(Locale.ROOT)
}

internal class PhoneCleanupFailure(val reason: PhoneCleanupReason, val facts: Bundle = Bundle()) : IllegalStateException()

@Suppress("DEPRECATION") // Bundle.get retains exact type/presence instead of inventing default facts.
internal fun phoneCleanupReceipt(action: () -> Bundle): Bundle = try { action() } catch (error: Exception) {
    val failure = error as? PhoneCleanupFailure
    Bundle().apply {
        failure?.facts?.let { facts ->
            val count = facts.get("pending_count_class")
            if (count in setOf("none", "one", "multiple", "unknown")) putString("pending_count_class", count as String)
            for (key in listOf("producer_exit_verified", "deletion_started")) {
                val value = facts.get(key)
                if (value is Boolean) putBoolean(key, value)
            }
            val deleted = facts.get("deleted_root_count")
            if (deleted is Int && deleted in 0..3) putInt("deleted_root_count", deleted)
            for ((key, maximum) in mapOf("deleted_owned_directory_count" to 32, "deleted_preference_count" to 24, "deleted_session_count" to 8)) {
                val value = facts.get(key)
                if (value is Int && value in 0..maximum) putInt(key, value)
            }
        }
        putInt("synthetic_protocol", 2); putString("benchmark_result", "failed"); putInt("completed_trials", 0)
        putBoolean("cleanup_verified", false); putBoolean("native_cleanup_verified", false)
        putString("cleanup_failure_reason", (failure?.reason ?: PhoneCleanupReason.ENTRY_PRECONDITION).wire())
    }
}

/**
 * Source-defined phone-exclusive benchmark namespace owner with a process-lifetime FileLock lease.
 * No durable per-session manifest/journal, no caller-supplied paths, and no implicit recovery during reservation.
 */
@android.annotation.TargetApi(35)
internal class PhoneBenchmarkProfile(
    private val data: File,
    private val cache: File,
    private val lockRoot: File,
    private val deleteRoot: (File) -> Boolean = { root ->
        Files.walkFileTree(root.toPath(), object : SimpleFileVisitor<Path>() {
            override fun visitFile(file: Path, attrs: BasicFileAttributes): FileVisitResult {
                check(attrs.isRegularFile && !Files.isSymbolicLink(file))
                Files.delete(file)
                return FileVisitResult.CONTINUE
            }
            override fun postVisitDirectory(dir: Path, error: java.io.IOException?): FileVisitResult {
                if (error != null) throw error
                Files.delete(dir)
                return FileVisitResult.CONTINUE
            }
        })
        true
    },
    private val deletePreferences: ((String) -> Boolean)? = null
) : AutoCloseable {
    constructor(context: Context) : this(
        File(context.applicationInfo.dataDir),
        context.cacheDir,
        context.noBackupFilesDir,
        deletePreferences = { context.deleteSharedPreferences(it) }
    )

    companion object {
        val PREFERENCE_NAMES = setOf(
            "assistant-phone-preferences",
            "voxgolem_google_tts_voice_cache",
            "voxgolem_google_stt_support_cache"
        )
        private val HEX_32 = Regex("[a-f0-9]{32}")
        private val NATIVE_DIR_REGEX = Regex("synthetic-phone-ask-([a-f0-9-]{36})")
        private val PREF_FILE_REGEX = Regex(
            "synthetic-phone-ask-([a-f0-9-]{36})-(assistant-phone-preferences|voxgolem_google_tts_voice_cache|voxgolem_google_stt_support_cache)\\.xml(?:\\.bak)?"
        )
    }

    private var leaseFile: RandomAccessFile? = null
    private var leaseChannel: FileChannel? = null
    private var leaseLock: FileLock? = null
    private var reservedToken: String? = null

    fun nativeDirectory(token: String): File {
        validToken(token)
        return File(cache, "synthetic-phone-ask-$token").also {
            guard(it.parentFile?.canonicalFile == cache.canonicalFile, PhoneCleanupReason.OWNED_PATH_INVALID)
        }
    }

    private fun exists(file: File): Boolean = try {
        Files.readAttributes(file.toPath(), BasicFileAttributes::class.java, NOFOLLOW_LINKS)
        true
    } catch (_: java.nio.file.NoSuchFileException) { false }

    private fun lockFile(): File {
        guard(exists(lockRoot) && lockRoot.isDirectory && !Files.isSymbolicLink(lockRoot.toPath()), PhoneCleanupReason.MARKER_DIRECTORY_INVALID)
        val file = File(lockRoot, "phone-benchmark.lock")
        guard(!Files.isSymbolicLink(file.toPath()) && (!exists(file) || file.isFile), PhoneCleanupReason.MARKER_FILE_INVALID)
        return file
    }

    private fun acquireProcessLease() {
        guard(leaseLock == null && reservedToken == null, PhoneCleanupReason.ENTRY_PRECONDITION)
        val file = lockFile()
        val raf = RandomAccessFile(file, "rw")
        try {
            val channel = raf.channel
            val lock = try { channel.tryLock() } catch (_: OverlappingFileLockException) { null }
            guard(lock != null, PhoneCleanupReason.ENTRY_PRECONDITION)
            leaseFile = raf
            leaseChannel = channel
            leaseLock = lock
        } catch (error: Throwable) {
            runCatching { raf.close() }
            throw error
        }
    }

    private fun <T> withCleanupLock(action: () -> T): T {
        guard(leaseLock == null, PhoneCleanupReason.PRODUCER_NOT_EXITED)
        val file = lockFile()
        RandomAccessFile(file, "rw").use { raf ->
            val lock = try { raf.channel.tryLock() } catch (_: OverlappingFileLockException) { null }
            guard(lock != null, PhoneCleanupReason.PRODUCER_NOT_EXITED)
            lock!!.use { return action() }
        }
    }

    /** Test-only helper to simulate process death releasing the lifetime OS file lock. */
    @Synchronized override fun close() {
        runCatching { leaseLock?.release() }
        runCatching { leaseChannel?.close() }
        runCatching { leaseFile?.close() }
        leaseLock = null
        leaseChannel = null
        leaseFile = null
        reservedToken = null
    }

    @Synchronized fun reserve(token: String): String {
        validToken(token)
        acquireProcessLease()
        try {
            val discovered = discoverTokens()
            guard(discovered.isEmpty(), PhoneCleanupReason.ENTRY_PRECONDITION)
            guard(resources(token).none { exists(it) }, PhoneCleanupReason.ENTRY_PRECONDITION)
            reservedToken = token
            return suffix(token)
        } catch (error: Throwable) {
            close()
            throw error
        }
    }

    /** Observe the directory created by the real WebView in memory. Never writes to disk. */
    @Synchronized fun observe(token: String): Boolean {
        validToken(token)
        guard(leaseLock?.isValid == true && reservedToken == token, PhoneCleanupReason.ENTRY_PRECONDITION)
        val directory = profile(suffix(token))
        if (!exists(directory)) return false
        guard(directory.isDirectory && !Files.isSymbolicLink(directory.toPath()), PhoneCleanupReason.OWNED_PATH_INVALID)
        // HTTP caches are lazy. The actual suffix-owned data root is the ownership proof.
        inspect(resources(token))
        return true
    }

    @Synchronized fun cleanup(token: String): Bundle = cleanupOperation { facts ->
        validToken(token)
        withCleanupLock {
            facts.putBoolean("producer_exit_verified", true)
            val discovered = discoverTokens()
            guard(discovered.size <= 1 && discovered.all { it == token }, PhoneCleanupReason.MARKER_NAMESPACE_INVALID)
            inspect(resources(token))
            deleteOwned(listOf(token), facts, publishWebCount = true)
            Bundle().apply {
                putInt("synthetic_protocol", 2); putString("benchmark_result", "cleaned")
                putBoolean("cleanup_verified", true); putBoolean("profile_cleaned", true); putInt("completed_trials", 0)
                putBoolean("producer_exit_verified", true); putBoolean("activity_webview_retired", true)
                putBoolean("storage_cleanup_verified", true)
                for (key in listOf("deleted_root_count", "deleted_owned_directory_count", "deleted_preference_count", "deleted_session_count")) {
                    if (facts.containsKey(key)) putInt(key, facts.getInt(key))
                }
            }
        }
    }

    /** Whole-namespace preflight precedes any deletion; reset never claims SDK retirement. */
    @Synchronized fun cleanupPending(): Bundle = cleanupOperation { facts ->
        withCleanupLock {
            facts.putBoolean("producer_exit_verified", true)
            val pending = discoverTokens()
            facts.putString("pending_count_class", when (pending.size) { 0 -> "none"; 1 -> "one"; else -> "multiple" })
            if (pending.isEmpty()) return@withCleanupLock Bundle().apply {
                putInt("synthetic_protocol", 2); putString("benchmark_result", "not_started")
                putInt("completed_trials", 0); putBoolean("cleanup_verified", false)
                putBoolean("profile_cleaned", false); putBoolean("producer_exit_verified", true)
                putBoolean("native_cleanup_verified", false)
            }
            inspect(pending.flatMap { resources(it) })
            deleteOwned(pending, facts, publishWebCount = pending.size == 1)
            Bundle().apply {
                putInt("synthetic_protocol", 2); putString("benchmark_result", "cleaned_after_process_exit")
                putInt("completed_trials", 0); putBoolean("cleanup_verified", true)
                putBoolean("profile_cleaned", true); putBoolean("producer_exit_verified", true)
                putBoolean("native_cleanup_verified", false)
                putBoolean("storage_cleanup_verified", true); putInt("recovered_session_count", pending.size)
                for (key in listOf("deleted_root_count", "deleted_owned_directory_count", "deleted_preference_count", "deleted_session_count")) {
                    if (facts.containsKey(key)) putInt(key, facts.getInt(key))
                }
            }
        }
    }

    private fun suffix(token: String): String = "synthetic_phone_" + token.replace("-", "")

    private fun tokenFromHex32(hex: String): String {
        guard(HEX_32.matches(hex), PhoneCleanupReason.MARKER_NAMESPACE_INVALID)
        val token = "${hex.substring(0, 8)}-${hex.substring(8, 12)}-${hex.substring(12, 16)}-${hex.substring(16, 20)}-${hex.substring(20, 32)}"
        validToken(token)
        return token
    }

    private fun preferenceFiles(token: String): List<File> = PREFERENCE_NAMES.flatMap { name ->
        listOf(
            File(data, "shared_prefs/${nativeDirectory(token).name}-$name.xml"),
            File(data, "shared_prefs/${nativeDirectory(token).name}-$name.xml.bak")
        )
    }

    private fun resources(token: String): List<File> {
        validToken(token)
        val result = listOf(profile(suffix(token))) + cacheRoots(suffix(token)) + nativeDirectory(token) + preferenceFiles(token)
        result.forEachIndexed { index, file ->
            safeParents(file)
            if (exists(file)) guard(if (index < 4) file.isDirectory else file.isFile, PhoneCleanupReason.OWNED_PATH_INVALID)
        }
        return result
    }

    private fun safeParents(file: File) {
        var current: File? = file
        while (current != null && current != data.parentFile && current != cache.parentFile) {
            guard(!Files.isSymbolicLink(current.toPath()), PhoneCleanupReason.OWNED_PATH_INVALID)
            if (current != file && exists(current)) guard(current.isDirectory, PhoneCleanupReason.OWNED_PATH_INVALID)
            current = current.parentFile
        }
    }

    private fun discoverTokens(): List<String> {
        guard(exists(data) && data.isDirectory && !Files.isSymbolicLink(data.toPath()), PhoneCleanupReason.OWNED_PATH_INVALID)
        guard(exists(cache) && cache.isDirectory && !Files.isSymbolicLink(cache.toPath()), PhoneCleanupReason.OWNED_PATH_INVALID)
        val sharedPrefs = File(data, "shared_prefs")
        safeParents(File(sharedPrefs, "probe"))
        val webViewParent = File(cache, "WebView")
        safeParents(File(webViewParent, "probe"))

        val tokens = sortedSetOf<String>()
        var scannedEntries = 0
        fun countEntry() {
            guard(++scannedEntries <= 10_000, PhoneCleanupReason.PROFILE_BUDGET_EXCEEDED)
        }
        fun scanDir(dir: File, visit: (Path, String) -> Unit) {
            if (!exists(dir)) return
            guard(dir.isDirectory && !Files.isSymbolicLink(dir.toPath()), PhoneCleanupReason.OWNED_PATH_INVALID)
            Files.newDirectoryStream(dir.toPath()).use { stream ->
                stream.forEach { path -> visit(path, path.fileName.toString()) }
            }
        }
        fun addHexDirToken(path: Path, name: String, prefix: String) {
            countEntry()
            guard(name.startsWith("${prefix}_"), PhoneCleanupReason.MARKER_NAMESPACE_INVALID)
            val token = tokenFromHex32(name.removePrefix("${prefix}_"))
            guard(!Files.isSymbolicLink(path) && Files.isDirectory(path, NOFOLLOW_LINKS), PhoneCleanupReason.OWNED_PATH_INVALID)
            tokens += token
        }

        scanDir(data) { path, name ->
            if (name.startsWith("app_webview_synthetic_phone")) addHexDirToken(path, name, "app_webview_synthetic_phone")
        }
        scanDir(cache) { path, name ->
            if (name.startsWith("synthetic-phone-ask")) {
                countEntry()
                val match = NATIVE_DIR_REGEX.matchEntire(name) ?: throw PhoneCleanupFailure(PhoneCleanupReason.MARKER_NAMESPACE_INVALID)
                val token = match.groupValues[1]
                validToken(token)
                guard(!Files.isSymbolicLink(path) && Files.isDirectory(path, NOFOLLOW_LINKS), PhoneCleanupReason.OWNED_PATH_INVALID)
                tokens += token
            } else if (name.startsWith("WebView_synthetic_phone")) {
                addHexDirToken(path, name, "WebView_synthetic_phone")
            }
        }
        scanDir(webViewParent) { path, name ->
            if (name.startsWith("synthetic_phone")) addHexDirToken(path, name, "synthetic_phone")
        }
        scanDir(sharedPrefs) { path, name ->
            if (name.startsWith("synthetic-phone-ask")) {
                countEntry()
                val match = PREF_FILE_REGEX.matchEntire(name) ?: throw PhoneCleanupFailure(PhoneCleanupReason.MARKER_NAMESPACE_INVALID)
                val token = match.groupValues[1]
                validToken(token)
                guard(!Files.isSymbolicLink(path) && Files.isRegularFile(path, NOFOLLOW_LINKS), PhoneCleanupReason.OWNED_PATH_INVALID)
                tokens += token
            }
        }

        guard(tokens.size <= 8, PhoneCleanupReason.PROFILE_BUDGET_EXCEEDED)
        return tokens.toList()
    }

    private fun profile(suffix: String): File {
        guard(suffix.matches(Regex("synthetic_phone_[a-f0-9]{32}")), PhoneCleanupReason.OWNED_PATH_INVALID)
        return File(data, "app_webview_$suffix").also {
            guard(it.parentFile?.canonicalFile == data.canonicalFile, PhoneCleanupReason.OWNED_PATH_INVALID)
        }
    }

    private fun cacheRoots(suffix: String): List<File> {
        guard(suffix.matches(Regex("synthetic_phone_[a-f0-9]{32}")), PhoneCleanupReason.OWNED_PATH_INVALID)
        return listOf(
            File(cache, "WebView_$suffix").also {
                guard(it.parentFile?.canonicalFile == cache.canonicalFile, PhoneCleanupReason.OWNED_PATH_INVALID)
            },
            File(cache, "WebView/$suffix").also {
                guard(!Files.isSymbolicLink(File(cache, "WebView").toPath()), PhoneCleanupReason.OWNED_PATH_INVALID)
                guard(it.parentFile?.parentFile?.canonicalFile == cache.canonicalFile, PhoneCleanupReason.OWNED_PATH_INVALID)
            }
        )
    }

    private fun validToken(token: String) {
        val valid = try { UUID.fromString(token).toString() == token } catch (_: IllegalArgumentException) { false }
        guard(valid, PhoneCleanupReason.MARKER_NAMESPACE_INVALID)
    }

    private fun inspect(roots: List<File>) {
        var files = 0
        var bytes = 0L
        val visitor = object : SimpleFileVisitor<Path>() {
            private fun inspectEntry(path: Path, attrs: BasicFileAttributes): FileVisitResult {
                guard(!Files.isSymbolicLink(path) && (attrs.isDirectory || attrs.isRegularFile), PhoneCleanupReason.OWNED_PATH_INVALID)
                guard(++files <= 10_000, PhoneCleanupReason.PROFILE_BUDGET_EXCEEDED)
                if (attrs.isRegularFile) bytes += attrs.size()
                guard(bytes <= 64L * 1024 * 1024, PhoneCleanupReason.PROFILE_BUDGET_EXCEEDED)
                return FileVisitResult.CONTINUE
            }
            override fun preVisitDirectory(dir: Path, attrs: BasicFileAttributes) = inspectEntry(dir, attrs)
            override fun visitFile(file: Path, attrs: BasicFileAttributes) = inspectEntry(file, attrs)
        }
        for (root in roots) {
            safeParents(root)
            if (!exists(root)) continue
            Files.walkFileTree(root.toPath(), visitor)
        }
    }

    private fun guard(valid: Boolean, reason: PhoneCleanupReason) {
        if (!valid) throw PhoneCleanupFailure(reason)
    }

    private fun <T> cleanupOperation(action: (Bundle) -> T): T {
        val facts = Bundle()
        try { return action(facts) }
        catch (error: PhoneCleanupFailure) { throw PhoneCleanupFailure(error.reason, Bundle(facts)) }
        catch (_: java.io.IOException) { throw PhoneCleanupFailure(PhoneCleanupReason.IO_UNAVAILABLE, Bundle(facts)) }
        catch (_: SecurityException) { throw PhoneCleanupFailure(PhoneCleanupReason.IO_UNAVAILABLE, Bundle(facts)) }
        catch (_: Exception) { throw PhoneCleanupFailure(PhoneCleanupReason.UNKNOWN, Bundle(facts)) }
    }

    private fun deleteOwned(tokens: List<String>, facts: Bundle, publishWebCount: Boolean) {
        facts.putBoolean("deletion_started", true)
        for (key in listOf("deleted_owned_directory_count", "deleted_preference_count", "deleted_session_count")) {
            if (!facts.containsKey(key)) facts.putInt(key, 0)
        }
        if (publishWebCount) facts.putInt("deleted_root_count", 0)
        for (token in tokens) {
            var webRoots = 0
            for (root in resources(token).take(4).filter { exists(it) }) {
                guard(root.isDirectory, PhoneCleanupReason.OWNED_PATH_INVALID)
                inspect(listOf(root))
                val deleted = try { deleteRoot(root) } catch (_: Exception) { false }
                guard(deleted && !exists(root), PhoneCleanupReason.OWNED_ROOT_DELETE_FAILED)
                facts.putInt("deleted_owned_directory_count", facts.getInt("deleted_owned_directory_count") + 1)
                if (root != nativeDirectory(token) && publishWebCount) facts.putInt("deleted_root_count", ++webRoots)
            }
            for (name in PREFERENCE_NAMES) {
                val scoped = "${nativeDirectory(token).name}-$name"
                val files = preferenceFiles(token).filter { it.name == "$scoped.xml" || it.name == "$scoped.xml.bak" }
                files.forEach {
                    safeParents(it)
                    guard(!exists(it) || it.isFile, PhoneCleanupReason.OWNED_PATH_INVALID)
                }
                if (files.any { exists(it) }) {
                    val deleted = deletePreferences?.invoke(scoped) ?: files.all { !exists(it) || it.delete() }
                    guard(deleted && files.none { exists(it) }, PhoneCleanupReason.OWNED_ROOT_DELETE_FAILED)
                    facts.putInt("deleted_preference_count", facts.getInt("deleted_preference_count") + 1)
                }
            }
            guard(resources(token).none { exists(it) }, PhoneCleanupReason.OWNED_ROOT_DELETE_FAILED)
            facts.putInt("deleted_session_count", facts.getInt("deleted_session_count") + 1)
        }
        guard( tokens.flatMap { resources(it) }.none { exists(it) } && discoverTokens().isEmpty(), PhoneCleanupReason.OWNED_ROOT_DELETE_FAILED)
    }
}
