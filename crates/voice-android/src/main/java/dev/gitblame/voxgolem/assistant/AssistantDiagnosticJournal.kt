package dev.gitblame.voxgolem.assistant

import android.content.Context
import java.io.ByteArrayOutputStream
import java.io.File
import java.util.Locale
import java.util.UUID
import java.util.concurrent.ConcurrentLinkedQueue
import java.util.concurrent.Executor
import java.util.concurrent.Executors
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicInteger
import java.util.concurrent.atomic.AtomicLong

/**
 * Best-effort bounded local telemetry. Detailed validation, clocks and disk work belong to the worker.
 * The process singleton exclusively owns its files; reconcile disk lengths on startup and after failures.
 * Cleanup runs on the next drain, not while the process is idle or stopped.
 */
class AssistantDiagnosticJournal(
    private val directory: () -> File,
    private val worker: Executor,
    private val wallMillis: () -> Long = System::currentTimeMillis,
    private val retentionMillis: Long = 90L * 24 * 60 * 60 * 1000,
    private val maxTotalBytes: Long = 8L * 1024 * 1024,
    private val maxFileBytes: Long = 256L * 1024,
    private val capacity: Int = 256,
    private val maxFiles: Int = 128,
    private val append: (File, ByteArray) -> Unit = { file, bytes -> file.appendBytes(bytes) },
) {
    data class Metadata(
        val engine: String? = null, val voice: String? = null, val language: String? = null,
        val model: String? = null, val entry: String? = null,
        val onDevice: Boolean? = null, val networkRequired: Boolean? = null, val cacheHit: Boolean? = null,
        val webEnabled: Boolean? = null,
        val bytes: Long? = null, val count: Int? = null, val sdkError: Int? = null,
        val requestedLanguage: String? = null,
        val route: String? = null,
        val warm: Boolean? = null,
        val searchMs: Long? = null,
        val firstTextMs: Long? = null,
        val completedMs: Long? = null,
    )
    private data class Record(val operation: AssistantTiming.Operation, val run: Int, val parent: Int?,
        val observed: Long, val event: AssistantTiming.Event? = null, val metadata: Metadata? = null)
    private val queue = ConcurrentLinkedQueue<Record>()
    private val queued = AtomicInteger()
    private val scheduled = AtomicBoolean()
    private val dropped = AtomicLong()
    // Initialized by the drain, never by an offer (UUID generation may access platform entropy).
    private var session: String? = null
    private var sequence = 0L
    private var fileIndex = 0L
    private data class StoredFile(val file: File, var bytes: Long, val created: Long)
    private var current: StoredFile? = null
    private var currentDay: Long? = null
    private var resolvedDirectory: File? = null
    private val stored = ArrayList<StoredFile>()
    private var lastCleanupDay: Long? = null

    init {
        require(capacity > 0 && maxFiles > 0 && retentionMillis in 0..90L * 86_400_000L &&
            maxFileBytes > 0 && maxTotalBytes >= maxFileBytes)
    }

    fun offer(event: AssistantTiming.Event) = enqueue(Record(event.operation, event.run, event.parentRun,
        event.observedNanos, event = event))

    fun isIdle(): Boolean = queued.get() == 0 && !scheduled.get()

    fun offerMetadata(operation: AssistantTiming.Operation, run: Int, parentRun: Int?, observedNanos: Long,
        metadata: Metadata) {
        // Constant-time admission bounds memory too; detailed token validation remains on the worker.
        if (listOf(metadata.engine, metadata.voice, metadata.language, metadata.requestedLanguage, metadata.model, metadata.entry, metadata.route)
                .any { it != null && it.length > 256 }) {
            dropped.incrementAndGet()
            return
        }
        enqueue(Record(operation, run, parentRun, observedNanos, metadata = metadata))
    }

    private fun enqueue(record: Record) {
        while (true) {
            val size = queued.get()
            if (size >= capacity) { dropped.incrementAndGet(); return }
            if (queued.compareAndSet(size, size + 1)) break
        }
        queue.add(record)
        schedule()
    }

    private fun schedule() {
        if (!scheduled.compareAndSet(false, true)) return
        try { worker.execute { drain() } }
        catch (_: RuntimeException) {
            // A rejected executor does not cause an unbounded retry loop. The next offer retries.
            scheduled.set(false)
            repeat(capacity) { if (queue.poll() != null) { queued.decrementAndGet(); dropped.incrementAndGet() } }
        }
    }

    private fun drain() {
        try {
            val batch = ArrayList<Record>()
            for (index in 0 until capacity) {
                val record = queue.poll() ?: break
                queued.decrementAndGet()
                batch.add(record)
            }
            if (batch.isNotEmpty()) {
                val lost = dropped.getAndSet(0)
                var completed = 0
                try { persist(batch, lost) { completed += it } }
                catch (_: Exception) {
                    dropped.addAndGet((if (completed == 0) lost else 0) + batch.size - completed)
                    current?.file?.let { file -> runCatching { if (file.exists() && file.length() == 0L) file.delete() } }
                    // Reconcile actual lengths after any uncertain/partial write before accepting more disk work.
                    current = null
                    resolvedDirectory = null
                    stored.clear()
                }
            }
        } finally {
            scheduled.set(false)
            if (queue.isNotEmpty()) schedule()
        }
    }

    private fun persist(batch: List<Record>, lost: Long, completed: (Int) -> Unit) {
        val now = wallMillis().coerceAtLeast(0)
        val day = now / 86_400_000L
        var dir = resolvedDirectory
        if (dir == null) {
            dir = directory().canonicalFile
            check(dir.isDirectory || dir.mkdirs())
            stored.clear()
            stored.addAll(ownedFiles(dir).map {
                StoredFile(it, it.length(), FILE_NAME.matchEntire(it.name)!!.groupValues[1].toLong())
            })
            stored.toList().filter { it.bytes == 0L }.forEach { file ->
                if (runCatching { file.file.delete() }.getOrDefault(false)) stored.remove(file)
            }
            resolvedDirectory = dir
            lastCleanupDay = null
        }
        if (lastCleanupDay != day || stored.any { it.created > now || now - it.created > retentionMillis }) {
            // Creation time is immutable in the name: appends/crashes cannot extend retention through mtime.
            // Future files are evicted on rollback (privacy over retaining an uncertain-age diagnostic).
            // Rotate on *any* day change, including rollback, so old files are never appended under a new date.
            if (currentDay != day) current = null
            stored.toList().filter { it.bytes > maxFileBytes || it.created > now ||
                now - it.created > retentionMillis }.forEach { remove(it) }
            lastCleanupDay = day
        }
        if (session == null) session = UUID.randomUUID().toString()
        val encoded = batch.mapIndexed { index, record ->
            encode(record, if (index == 0) lost else 0).toByteArray(Charsets.UTF_8).also {
                check(it.size.toLong() <= maxFileBytes)
            }
        }
        var index = 0
        while (index < batch.size) {
            var target = current
            if (target == null) {
                while (stored.size >= maxFiles) {
                    remove(stored.minWith(compareBy<StoredFile> { it.created }.thenBy { it.file.name }))
                }
                target = StoredFile(File(dir, "assistant-timing-$now-$session-${++fileIndex}.jsonl"), 0, now)
                current = target
                currentDay = day
                stored.add(target)
            }
            val output = ByteArrayOutputStream()
            var records = 0
            while (index < batch.size) {
                val data = encoded[index]
                if (target.bytes + output.size() + data.size > maxFileBytes) break
                output.write(data)
                index++
                records++
            }
            if (records == 0) { current = null; continue }
            var total = stored.sumOf { it.bytes }
            for (file in stored.sortedWith(compareBy<StoredFile> { it.created }.thenBy { it.file.name })) {
                if (total + output.size() <= maxTotalBytes) break
                // A pending append owns this file. If it must be evicted, restart this group in a new file.
                if (file === target) continue
                total -= file.bytes
                remove(file)
            }
            if (total + output.size() > maxTotalBytes) {
                remove(target)
                index -= records
                continue
            }
            append(target.file, output.toByteArray())
            target.bytes += output.size()
            completed(records)
        }
    }

    private fun remove(file: StoredFile) {
        check(!file.file.exists() || file.file.delete())
        stored.remove(file)
        if (current === file) current = null
    }

    private fun ownedFiles(dir: File): List<File> = dir.listFiles()?.filter {
        it.isFile && FILE_NAME.matchEntire(it.name)?.groupValues?.get(1)?.toLongOrNull() != null &&
            it.canonicalFile.parentFile == dir.canonicalFile &&
            it.canonicalFile == it.absoluteFile
    } ?: emptyList()

    private fun encode(record: Record, lost: Long): String {
        val fields = ArrayList<String>()
        fun value(key: String, value: Any?) {
            if (value == null) return
            // Strings reaching here are fixed vocabulary or strictly validated ASCII/BCP47 tokens.
            fields += "\"$key\":" + if (value is String) "\"$value\"" else value.toString()
        }
        value("schema", 1); value("session", session); value("sequence", ++sequence)
        value("operation", record.operation.name); value("run", record.run); value("parentRun", record.parent)
        value("observedNanos", record.observed); value("dropped", lost)
        record.event?.let {
            value("kind", it.kind.name); value("stage", it.stage?.name); value("elapsedNanos", it.elapsedNanos)
        }
        record.metadata?.let {
            value("kind", "METADATA")
            value("engine", it.engine?.takeIf { s -> s == "com.google.android.tts" })
            value("voice", it.voice?.takeIf { s -> s.length <= 128 && VOICE.matches(s) })
            value("language", it.language?.takeIf { s -> s.length <= 64 && LANGUAGE.matches(s) &&
                Locale.forLanguageTag(s).toLanguageTag() == s && s != "und" })
            value("requestedLanguage", it.requestedLanguage?.takeIf { s -> s.length <= 64 && LANGUAGE.matches(s) &&
                Locale.forLanguageTag(s).toLanguageTag() == s && s != "und" })
            value("model", it.model?.takeIf { s -> s in MODELS })
            value("entry", it.entry?.takeIf { s -> s in ENTRIES })
            value("onDevice", it.onDevice); value("networkRequired", it.networkRequired); value("cacheHit", it.cacheHit)
            value("webEnabled", it.webEnabled)
            value("route", it.route?.takeIf { s -> ROUTE.matches(s) })
            value("warm", it.warm)
            value("search_ms", it.searchMs?.takeIf { n -> n >= 0L })
            value("first_text_ms", it.firstTextMs?.takeIf { n -> n >= 0L })
            value("completed_ms", it.completedMs?.takeIf { n -> n >= 0L })
            value("bytes", it.bytes?.takeIf { n -> n in 0..1_073_741_824L })
            value("count", it.count?.takeIf { n -> n in 0..1_000_000 })
            // Android SpeechRecognizer errors 1..15; TextToSpeech errors -1..-9, success 0.
            value("sdkError", it.sdkError?.takeIf { n -> n in -9..15 })
        }
        return fields.joinToString(",", "{", "}\n")
    }

    companion object {
        private val FILE_NAME = Regex("assistant-timing-(0|[1-9][0-9]{0,18})-[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}-[1-9][0-9]*\\.jsonl")
        private val VOICE = Regex("[a-z]{2,3}-[a-z]{2}(?:-x-[a-z]{3}-(?:local|network)|-language)")
        private val LANGUAGE = Regex("[A-Za-z]{2,8}(?:-[A-Za-z0-9]{1,8})*")
        private val ROUTE = Regex("[A-Za-z0-9_-]{1,32}")
        private val ENTRIES = setOf("ask", "talk", "chat", "manual")
        // Explicit aliases/server IDs from AgyProvider and platform/custom_openai; never arbitrary server names.
        private val MODELS = setOf("agy-opus-high", "claude-opus-5-5-high", "opus-5.5", "opus5.5",
            "agy-sonnet-high", "claude-sonnet-5-5-high", "sonnet-5.5", "sonnet5.5",
            "agy-sonnet-low", "claude-sonnet-5-5-low", "gemini-nano",
            "agy-flash-none", "gemini-3.8-flash-none", "agy-flash-low",
            "gemini-3.8-flash-low", "agy-flash-high", "gemini-3.8-flash-high", "3.8-flash-high",
            "custom-sol-high", "custom-luna-none", "custom-luna-low", "local-fast",
            "gpt-6.1-sol", "gpt-6-luna", "gpt-5.6-luna")
        @Volatile private var instance: AssistantDiagnosticJournal? = null
        fun forContext(context: Context): AssistantDiagnosticJournal = instance ?: synchronized(this) {
            instance ?: run {
                val app = context.applicationContext
                AssistantDiagnosticJournal(
                    { File(app.getExternalFilesDir(null) ?: app.noBackupFilesDir, "diagnostics/assistant-timing") },
                    Executors.newSingleThreadExecutor { task -> Thread(task, "assistant-diagnostics").apply { isDaemon = true } },
                ).also { instance = it }
            }
        }
    }
}
