package dev.gitblame.voxgolem.capture

import android.net.Uri
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.Executors

/** Owns archive deduplication and serialized verified archive/export work, not recognition admission. */
internal class PhoneCaptureArchive(
    private val repository: CaptureRepository,
    private val archiveWav: (CaptureRecord) -> Uri,
    private val exportOrg: (String) -> OrgExportResult,
    private val delivery: PhoneCaptureDelivery,
) {
    private val archives = Executors.newSingleThreadExecutor()
    private val archiving = ConcurrentHashMap.newKeySet<String>()

    fun queue(captureId: String, sourceNodeId: String) {
        if (repository.get(captureId)?.purpose != CapturePurpose.NOTE) return
        if (!archiving.add(captureId)) return
        archives.execute {
            try { archiveAndExport(captureId, sourceNodeId) }
            finally { archiving.remove(captureId) }
        }
    }

    private fun archiveAndExport(captureId: String, sourceNodeId: String) {
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
        }
    }

    fun maybeExport(record: CaptureRecord) {
        if (record.purpose != CapturePurpose.NOTE || record.orgState == OrgState.EXPORTED || record.captureState != CaptureState.SAVED || record.recognitionState != RecognitionState.FINAL ||
            record.finalText.isNullOrBlank() || record.archiveState != ArchiveState.VERIFIED || record.archivedAudioUri == null) return
        archives.execute {
            val latest = repository.get(record.captureId) ?: return@execute
            if (latest.purpose != CapturePurpose.NOTE || latest.orgState == OrgState.EXPORTED) return@execute
            runCatching { exportOrg(record.captureId) }.onFailure {
                repository.update(record.captureId) {
                    it.copy(orgState = OrgState.RETRYABLE_ERROR, lastErrorStage = "org", lastErrorCode = "org-export-failed")
                }
            }
        }
    }

    fun barrier(done: () -> Unit) { archives.execute(done) }
    fun shutdown() { archives.shutdownNow() }
}
