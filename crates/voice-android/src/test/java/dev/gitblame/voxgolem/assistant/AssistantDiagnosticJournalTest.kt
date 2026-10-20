package dev.gitblame.voxgolem.assistant

import org.junit.Assert.*
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TemporaryFolder
import java.io.File
import java.util.concurrent.Executor

class AssistantDiagnosticJournalTest {
    @get:Rule val temporary = TemporaryFolder()
    private class Worker : Executor {
        val tasks = ArrayDeque<Runnable>()
        override fun execute(command: Runnable) { tasks.add(command) }
        fun drain() { while (tasks.isNotEmpty()) tasks.removeFirst().run() }
    }
    private fun event() = AssistantTiming.Event(AssistantTiming.Operation.SPEECH, 7, 3,
        AssistantTiming.Kind.MARK, AssistantTiming.Stage.ON_START, 123, 456)
    private fun text(dir: File) = dir.listFiles()!!.filter { it.extension == "jsonl" }.joinToString("") { it.readText() }
    private fun owned(dir: File, created: Long, index: Int) = File(dir,
        "assistant-timing-$created-00000000-0000-0000-0000-000000000000-$index.jsonl")

    @Test fun boundedQueueDoesNoCallerIoAndReportsOverflowWithMonotonicFields() {
        val worker = Worker()
        val dir = temporary.newFolder()
        var lookups = 0
        val journal = AssistantDiagnosticJournal({ lookups++; dir }, worker, { 1000L }, capacity = 2)
        repeat(5) { journal.offer(event()) }
        assertEquals(0, lookups)
        assertEquals(1, worker.tasks.size)
        assertFalse(journal.isIdle())
        worker.drain()
        assertTrue(journal.isIdle())
        val output = text(dir)
        assertEquals(2, output.lines().count { it.isNotEmpty() })
        assertTrue(output.contains("\"dropped\":3"))
        assertTrue(output.contains("\"elapsedNanos\":123"))
        assertTrue(output.contains("\"observedNanos\":456"))
        assertTrue(output.contains("\"parentRun\":3"))
    }

    @Test fun storageUsesFixedVocabularyNumericAggregatesAndExistingBoundedQueue() {
        val worker = Worker(); val dir = temporary.newFolder()
        var lookups = 0
        val journal = AssistantDiagnosticJournal({ lookups++; dir }, worker, { 1000L }, capacity = 2)
        val metrics = mapOf("sqlite_read" to AssistantDiagnosticJournal.Metric(2, 30, 20),
            "synthetic private SQL" to AssistantDiagnosticJournal.Metric(1, 5, 5),
            "sqlite_write" to AssistantDiagnosticJournal.Metric(0, 0, 0),
            "record_encode" to AssistantDiagnosticJournal.Metric(0, 1, 1),
            "native_dispatch" to AssistantDiagnosticJournal.Metric(1, -1, 0))
        repeat(3) { journal.offerStorage(it + 10, 7, 123,
            AssistantDiagnosticJournal.Storage(AssistantDiagnosticJournal.StorageCall.PROJECTION,
                AssistantDiagnosticJournal.StorageOutcome.ERROR, metrics)) }
        assertEquals(0, lookups)
        worker.drain()
        val output = text(dir)
        assertEquals(2, output.lines().count { it.isNotEmpty() })
        listOf("\"schema\":1", "\"kind\":\"STORAGE\"", "\"operation\":\"STORAGE\"", "\"parentRun\":7",
            "\"call\":\"projection\"", "\"outcome\":\"error\"", "\"dropped\":1",
            "\"sqlite_read\":{\"count\":2,\"total_ns\":30,\"max_ns\":20}").forEach { assertTrue(output.contains(it)) }
        assertTrue(output.contains("\"sqlite_write\":{\"count\":0,\"total_ns\":0,\"max_ns\":0}"))
        listOf("private", "record_encode", "native_dispatch", "native_store_wait").forEach { assertFalse(output.contains(it)) }
    }

    @Test fun metadataRejectsUnsafeAndUnknownValuesRatherThanTruncating() {
        val worker = Worker(); val dir = temporary.newFolder()
        val journal = AssistantDiagnosticJournal({ dir }, worker, { 1000L })
        journal.offerMetadata(AssistantTiming.Operation.SPEECH, 1, null, 99,
            AssistantDiagnosticJournal.Metadata(engine = "com.google.android.tts", voice = "en-us-x-tpf-local",
                language = "en-US", model = "private-server-name", entry = "ask",
                bytes = -1, count = Int.MAX_VALUE, sdkError = 999, networkRequired = false))
        journal.offerMetadata(AssistantTiming.Operation.SPEECH, 1, null, 100,
            AssistantDiagnosticJournal.Metadata(voice = "en-US-private@example.invalid", language = "bad language",
                engine = "private.engine", entry = "private entry"))
        worker.drain()
        val output = text(dir)
        assertTrue(output.contains("en-us-x-tpf-local"))
        assertTrue(output.contains("\"language\":\"en-US\""))
        assertTrue(output.contains("\"networkRequired\":false"))
        listOf("private", "sdkError", "bytes", "count", "bad language").forEach { assertFalse(output.contains(it)) }
    }

    @Test fun oversizedMetadataCannotFillTheQueueWithUnboundedStrings() {
        val worker = Worker(); val dir = temporary.newFolder()
        val journal = AssistantDiagnosticJournal({ dir }, worker, { 1000L }, capacity = 1)
        journal.offerMetadata(AssistantTiming.Operation.SPEECH, 1, null, 0,
            AssistantDiagnosticJournal.Metadata(voice = "x".repeat(257)))
        journal.offer(event()); worker.drain()
        assertEquals(1, text(dir).lineSequence().count { it.isNotBlank() })
        assertTrue(text(dir).contains("\"dropped\":1"))
    }

    @Test fun filesystemFailureRecoversOnNextOfferWithoutEscaping() {
        val worker = Worker(); val dir = temporary.newFolder(); val blocked = File(dir, "blocked").apply { writeText("x") }
        var target = blocked
        val journal = AssistantDiagnosticJournal({ target }, worker, { 1000L })
        journal.offer(event()); worker.drain()
        target = temporary.newFolder()
        journal.offer(event()); worker.drain()
        assertTrue(text(target).contains("\"dropped\":1"))
    }

    @Test fun repeatedFailureAfterFileCreationDoesNotAccumulateEmptyFilesAndReportsRecovery() {
        val worker = Worker(); val dir = temporary.newFolder(); var failing = true
        val journal = AssistantDiagnosticJournal({ dir }, worker, { 1000L }, maxFiles = 3,
            append = { file, bytes ->
                if (failing) { file.createNewFile(); throw java.io.IOException("Synthetic storage exhaustion") }
                file.appendBytes(bytes)
            })
        repeat(12) {
            journal.offer(event()); worker.drain()
            assertTrue(dir.listFiles()!!.size <= 3)
        }
        assertTrue(dir.listFiles()!!.isEmpty())
        failing = false
        journal.offer(event()); worker.drain()
        assertTrue(text(dir).contains("\"dropped\":12"))
        assertEquals(1, dir.listFiles()!!.size)
    }

    @Test fun fileCountIsBoundedIndependentlyOfBytesAndUnrelatedFilesSurvive() {
        val worker = Worker(); val dir = temporary.newFolder()
        repeat(9) { owned(dir, it.toLong(), it + 1).writeText("synthetic\n") }
        val unrelated = File(dir, "user-owned.txt").apply { writeText("keep") }
        val journal = AssistantDiagnosticJournal({ dir }, worker, { 1000L }, maxFiles = 3)
        journal.offer(event()); worker.drain()
        assertEquals(3, dir.listFiles()!!.count { it.extension == "jsonl" })
        assertEquals("keep", unrelated.readText())
    }

    @Test fun rejectedSchedulingIsBoundedAndNextOfferRetries() {
        val worker = Worker(); val dir = temporary.newFolder(); var reject = true
        val executor = Executor { task ->
            if (reject) throw java.util.concurrent.RejectedExecutionException()
            worker.execute(task)
        }
        val journal = AssistantDiagnosticJournal({ dir }, executor, { 1000L })
        journal.offer(event())
        assertEquals(0, worker.tasks.size)
        reject = false
        journal.offer(event()); worker.drain()
        assertTrue(text(dir).contains("\"dropped\":1"))
    }

    @Test fun customModelsAndWebRouteHaveOnlyExplicitTypedValues() {
        val worker = Worker(); val dir = temporary.newFolder()
        val journal = AssistantDiagnosticJournal({ dir }, worker, { 1000L })
        val models = listOf("custom-sol-low", "custom-sol-high", "custom-luna-none", "custom-luna-low",
            "agy-argon-low", "agy-argon-medium", "agy-argon-high", "gemini-4-argon-high",
            "local-fast", "gpt-6.1-sol", "gpt-6-luna", "gpt-5.6-luna")
        models.forEach { model -> journal.offerMetadata(AssistantTiming.Operation.GENERATION, 1, null, 42,
            AssistantDiagnosticJournal.Metadata(model = model, webEnabled = false)) }
        worker.drain()
        val output = text(dir)
        models.forEach { assertTrue(output.contains("\"model\":\"$it\"")) }
        assertEquals(models.size, output.lines().count { it.contains("\"webEnabled\":false") })
    }

    @Test fun appendsDoNotRefreshFirstRecordAgeAndNinetyDayCleanupSurvivesRestart() {
        val worker = Worker(); val dir = temporary.newFolder()
        val day = 86_400_000L
        var now = day
        val journal = AssistantDiagnosticJournal({ dir }, worker, { now })
        journal.offer(event()); worker.drain()
        val first = dir.listFiles()!!.single()
        val created = now
        val firstName = first.name
        now += 1000
        journal.offer(event()); worker.drain()
        assertEquals(firstName, first.name)
        assertEquals(2, first.readLines().size)
        now += day
        journal.offer(event()); worker.drain()
        assertEquals(2, dir.listFiles()!!.size)
        assertEquals(firstName, first.name)
        now = created + 90 * day + 1
        journal.offer(event()); worker.drain()
        assertFalse(first.exists())
        val crashLeftover = owned(dir, now, 999)
            .apply { writeText("synthetic".repeat(100)); setLastModified(now) }
        val restarted = AssistantDiagnosticJournal({ dir }, worker, { now },
            maxFileBytes = 512, maxTotalBytes = 1024)
        restarted.offer(event()); worker.drain()
        assertFalse(first.exists())
        assertFalse(crashLeftover.exists())
        assertTrue(dir.listFiles()!!.sumOf { it.length() } <= 1024)
        assertTrue(dir.listFiles()!!.all { it.length() <= 512 })
    }

    @Test fun futureCreationIsEvictedAndRollbackRotatesEvenWithinSameUtcDay() {
        val worker = Worker(); val dir = temporary.newFolder(); var now = 2 * 86_400_000L
        val journal = AssistantDiagnosticJournal({ dir }, worker, { now }, retentionMillis = 1000,
            maxFileBytes = 512, maxTotalBytes = 2048)
        journal.offer(event()); worker.drain()
        val future = dir.listFiles()!!.single()
        now = 86_400_000L
        journal.offer(event()); worker.drain()
        assertFalse(future.exists())
        val second = dir.listFiles()!!.single()
        now -= 1
        journal.offer(event()); worker.drain()
        assertFalse(second.exists())
        assertEquals(1, dir.listFiles()!!.size)
        assertTrue(dir.listFiles()!!.single().name.startsWith("assistant-timing-$now-"))
    }

    @Test fun crashFreshMtimeCannotExtendImmutableCreationAgeOnRestart() {
        val worker = Worker(); val dir = temporary.newFolder()
        val now = 91L * 86_400_000L
        val expired = owned(dir, 0, 1).apply { writeText("synthetic\n"); setLastModified(now) }
        val future = owned(dir, now + 1, 2).apply { writeText("synthetic\n"); setLastModified(0) }
        val unrelated = File(dir, "assistant-timing-legacy.jsonl").apply { writeText("keep") }
        val journal = AssistantDiagnosticJournal({ dir }, worker, { now })
        journal.offer(event()); worker.drain()
        assertFalse(expired.exists())
        assertFalse(future.exists())
        assertTrue(unrelated.exists())
    }

    @Test fun rotationKeepsCompleteJsonLinesAndDailyCleanupRechecksAge() {
        val worker = Worker(); val dir = temporary.newFolder(); var now = 86_400_000L
        val journal = AssistantDiagnosticJournal({ dir }, worker, { now }, retentionMillis = 1000,
            maxFileBytes = 450, maxTotalBytes = 5000)
        repeat(3) { journal.offer(event()) }; worker.drain()
        val firstFiles = dir.listFiles()!!.toList()
        assertTrue("The sample must cross a file boundary", firstFiles.size >= 2)
        assertTrue(firstFiles.all { it.length() <= 450 })
        val lines = firstFiles.flatMap { it.readLines() }
        assertEquals(3, lines.size)
        val sequences = lines.map { Regex("\"sequence\":([0-9]+)").find(it)!!.groupValues[1].toInt() }.sorted()
        assertEquals("Rotation is not a dropped record", listOf(1, 2, 3), sequences)
        firstFiles.forEach { assertTrue(it.readText().endsWith("}\n")) }
        now += 86_400_000L
        journal.offer(event()); worker.drain()
        assertTrue(firstFiles.all { !it.exists() })
        assertEquals(1, dir.listFiles()!!.size)
    }

    @Test fun pruningUsesActualSizesPreservesUnrelatedAndHandlesFutureFiles() {
        val worker = Worker(); val dir = temporary.newFolder(); var now = 10_000L
        val old = owned(dir, 1, 1).apply { writeText("old"); setLastModified(now) }
        val future = owned(dir, 100_000, 2).apply { writeText("x".repeat(700)); setLastModified(0) }
        val unrelated = File(dir, "keep.txt").apply { writeText("keep") }
        val journal = AssistantDiagnosticJournal({ dir }, worker, { now }, retentionMillis = 1000,
            maxTotalBytes = 900, maxFileBytes = 450)
        repeat(6) { journal.offer(event()) }; worker.drain()
        assertFalse(old.exists())
        assertTrue(unrelated.exists())
        val files = dir.listFiles()!!.filter { it.extension == "jsonl" }
        assertTrue(files.sumOf { it.length() } <= 900)
        assertTrue(files.all { it.length() <= 450 })
        assertFalse(future.exists())
        now = 5
        journal.offer(event()); worker.drain()
        assertTrue(text(dir).isNotEmpty())
    }
}
