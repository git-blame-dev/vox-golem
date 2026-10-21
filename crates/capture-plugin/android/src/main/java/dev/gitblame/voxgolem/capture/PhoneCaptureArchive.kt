package dev.gitblame.voxgolem.capture

import android.net.Uri
import java.util.concurrent.ScheduledThreadPoolExecutor
import java.util.concurrent.ScheduledFuture
import java.util.concurrent.TimeUnit

/** Owns archive deduplication and serialized verified archive/export work, not recognition admission. */
internal class PhoneCaptureArchive(
    private val repository: CaptureRepository,
    private val archiveWav: (CaptureRecord) -> Uri,
    private val exportOrg: (String) -> OrgExportResult,
    private val delivery: PhoneCaptureDelivery,
) {
    private val archives = ScheduledThreadPoolExecutor(1).apply { removeOnCancelPolicy = true }
    private val monitor = Any()
    private var scheduled: ScheduledFuture<*>? = null
    private var running = false
    private var requested = false
    private var closed = false
    private var configurationRevision = 0L
    private var generation = 0L

    /** Caller holds monitor. A cancelled timer that already entered its callback cannot steal ownership. */
    private fun schedule(delayMillis: Long) {
        val token = ++generation
        scheduled = archives.schedule({ drain(token) }, delayMillis, TimeUnit.MILLISECONDS)
    }

    fun wake(configurationChanged: Boolean = false) {
        if (configurationChanged) {
            synchronized(monitor) { configurationRevision += 1 }
            repository.wakeNoteArchives(); repository.wakeNoteOrg()
        }
        synchronized(monitor) {
            if (closed) return
            requested = true
            if (running) return
            scheduled?.cancel(false)
            schedule(0)
        }
    }

    private fun drain(token: Long) {
        synchronized(monitor) {
            if (closed || token != generation) return
            running = true; requested = false; scheduled = null
        }
        var failureDelayMillis = 0L
        try {
            // Fixed-size pages and due-first ordering prevent newest failing rows starving older work.
            repository.dueNoteArchives(System.currentTimeMillis()).forEach { record ->
                archiveAndExport(record.captureId, repository.phoneOrigin(record.captureId).orEmpty())
            }
            repository.dueNoteOrg(System.currentTimeMillis()).forEach(::processOrg)
        } catch (_: Exception) {
            // Metadata admission failure must not turn an unchanged earliest-due row into a hot loop.
            failureDelayMillis = 1_000L
        } finally {
            synchronized(monitor) {
                running = false
                if (!closed) {
                    val due = if (requested) 0L else runCatching {
                        listOfNotNull(repository.earliestNoteArchiveDue(), repository.earliestNoteOrgDue()).minOrNull()
                    }.getOrElse { System.currentTimeMillis() + 1_000L }
                    if (due != null && due != Long.MAX_VALUE) {
                        schedule((due - System.currentTimeMillis()).coerceAtLeast(failureDelayMillis))
                    }
                }
            }
        }
    }

    private fun archiveAndExport(captureId: String, sourceNodeId: String) {
        val configuration = synchronized(monitor) { configurationRevision }
        val record = repository.get(captureId) ?: return
        if (record.purpose != CapturePurpose.NOTE) return
        if (record.archiveState == ArchiveState.VERIFIED && record.archivedAudioUri != null) {
            maybeExport(record)
            return
        }
        try {
            val uri = archiveWav(record)
            val archived = repository.update(captureId) {
                if (it.sha256 != record.sha256 || it.finalizedByteLength != record.finalizedByteLength) it
                else it.copy(
                    archivedAudioUri = uri.toString(),
                    archiveState = ArchiveState.VERIFIED,
                    transferState = TransferState.PHONE_ARCHIVED,
                    lastErrorStage = if (it.lastErrorStage == "archive") null else it.lastErrorStage,
                    lastErrorCode = if (it.lastErrorStage == "archive") null else it.lastErrorCode,
                )
            } ?: return
            if (archived.archiveState == ArchiveState.VERIFIED) {
                if (sourceNodeId.isNotBlank()) delivery.sendReceipt(sourceNodeId, archived, "PHONE_ARCHIVED", archived.exportDestinationId)
                maybeExport(archived)
            }
        } catch (failure: Exception) {
            val code = when (failure.message) {
                "notes-folder-not-selected" -> "notes-folder-not-selected"
                "staged-recording-missing" -> "staged-recording-missing"
                else -> "archive-failed"
            }
            repository.update(captureId) { current ->
                if (current.archiveState == ArchiveState.VERIFIED) current
                else current.copy(
                    archiveState = if (code == "notes-folder-not-selected") ArchiveState.WAITING_FOR_FOLDER else ArchiveState.RETRYABLE_ERROR,
                    transferState = TransferState.PHONE_STAGED,
                    lastErrorStage = "archive",
                    lastErrorCode = code,
                )
            }
            synchronized(monitor) {
                repository.deferNoteArchive(captureId, System.currentTimeMillis(),
                    code == "notes-folder-not-selected" && configurationRevision == configuration)
            }
        }
    }

    fun maybeExport(record: CaptureRecord) {
        if (record.purpose != CapturePurpose.NOTE || record.orgState == OrgState.EXPORTED || record.captureState != CaptureState.SAVED || record.recognitionState != RecognitionState.FINAL ||
            record.finalText.isNullOrBlank() || record.archiveState != ArchiveState.VERIFIED || record.archivedAudioUri == null) return
        wake()
    }

    private fun processOrg(record: CaptureRecord) {
        val configuration = synchronized(monitor) { configurationRevision }
        // No replacement SAF writer is started while this synchronous effect remains blocked.
        val result = runCatching { exportOrg(record.captureId) }.getOrElse {
            repository.update(record.captureId) { latest ->
                if (latest.transcriptVersion != record.transcriptVersion || latest.orgState == OrgState.EXPORTED) latest
                else latest.copy(orgState = OrgState.RETRYABLE_ERROR, lastErrorStage = "org", lastErrorCode = "org-export-failed")
            }
            OrgExportResult(OrgState.RETRYABLE_ERROR, null, "org-export-failed")
        }
        if (repository.get(record.captureId)?.orgState != OrgState.EXPORTED) synchronized(monitor) {
            repository.deferNoteOrg(record.captureId, record.transcriptVersion, System.currentTimeMillis(),
                result.state in setOf(OrgState.WAITING_FOR_FOLDER, OrgState.NEEDS_REVIEW, OrgState.WAITING_FOR_TRANSCRIPT) &&
                    configurationRevision == configuration)
        }
    }

    fun barrier(done: () -> Unit) { archives.execute(done) }
    fun shutdown() {
        synchronized(monitor) { closed = true; generation += 1; scheduled?.cancel(false) }
        archives.shutdownNow()
    }
}
