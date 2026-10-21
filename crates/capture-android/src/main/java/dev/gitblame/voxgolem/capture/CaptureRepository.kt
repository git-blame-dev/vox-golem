package dev.gitblame.voxgolem.capture

import android.content.ContentValues
import android.content.Context
import android.database.sqlite.SQLiteDatabase
import android.database.sqlite.SQLiteOpenHelper
import org.json.JSONObject
import java.io.File
import java.io.RandomAccessFile

/** SQLite-backed authoritative capture metadata and app-private PCM spool locations. */
class CaptureRepository(context: Context) {
    private val appContext = context.applicationContext
    private val helper = Database(appContext)
    private val monitor = Any()

    fun list(offset: Int = 0, limit: Int = 50): List<CaptureRecord> {
        require(offset >= 0) { "offset cannot be negative" }
        require(limit in 1..500) { "limit must be between 1 and 500" }
        synchronized(monitor) {
            helper.readableDatabase.rawQuery(
                "SELECT body FROM captures ORDER BY created_at_utc DESC, capture_id DESC LIMIT ? OFFSET ?",
                arrayOf(limit.toString(), offset.toString()),
            ).use { cursor ->
                return buildList { while (cursor.moveToNext()) add(decode(cursor.getString(0))) }
            }
        }
    }

    fun recent(purpose: CapturePurpose, limit: Int): List<CaptureRecord> = synchronized(monitor) {
        require(limit in 1..500) { "limit must be between 1 and 500" }
        helper.readableDatabase.rawQuery(
            "SELECT body FROM captures WHERE purpose = ? ORDER BY created_at_utc DESC, capture_id DESC LIMIT ?", arrayOf(purpose.name, limit.toString()),
        ).use { cursor ->
            buildList {
                while (size < limit && cursor.moveToNext()) {
                    val record = decode(cursor.getString(0))
                    if (record.purpose == purpose) add(record)
                }
            }
        }
    }

    /** Indexed durable archive work; a blocked writer holds no queue of capture bodies. */
    fun dueNoteArchives(nowMillis: Long, limit: Int = 16): List<CaptureRecord> = dueWork("archive", nowMillis, limit)

    fun earliestNoteArchiveDue(): Long? = synchronized(monitor) {
        helper.readableDatabase.rawQuery(
            "SELECT archive_due FROM captures WHERE archive_eligible = 1 ORDER BY archive_due LIMIT 1", null,
        ).use { if (it.moveToFirst()) it.getLong(0) else null }
    }

    /** Archive retry accounting is deliberately independent of recognition's retryCount. */
    fun deferNoteArchive(id: String, nowMillis: Long, waitingForFolder: Boolean) = synchronized(monitor) {
        val db = helper.writableDatabase
        db.beginTransaction()
        try {
            val attempts = db.rawQuery("SELECT archive_attempts FROM captures WHERE capture_id = ?", arrayOf(id)).use {
                if (it.moveToFirst()) it.getInt(0) else return@synchronized
            }
            val nextAttempt = if (waitingForFolder) attempts else (attempts + 1).coerceAtMost(20)
            val delay = (5_000L shl attempts.coerceAtMost(10)).coerceAtMost(3_600_000L)
            db.execSQL("UPDATE captures SET archive_attempts = ?, archive_due = ? WHERE capture_id = ?",
                arrayOf<Any>(nextAttempt, if (waitingForFolder) Long.MAX_VALUE else nowMillis + delay, id))
            db.setTransactionSuccessful()
        } finally { db.endTransaction() }
    }

    fun wakeNoteArchives() = synchronized(monitor) {
        helper.writableDatabase.execSQL("UPDATE captures SET archive_due = 0 WHERE archive_eligible = 1")
    }

    fun dueNoteTransfers(nowMillis: Long, limit: Int = 16): List<CaptureRecord> = dueWork("transfer", nowMillis, limit)

    fun earliestNoteTransferDue(): Long? = synchronized(monitor) {
        helper.readableDatabase.rawQuery(
            "SELECT transfer_due FROM captures WHERE transfer_eligible = 1 ORDER BY transfer_due LIMIT 1", null,
        ).use { if (it.moveToFirst()) it.getLong(0) else null }
    }

    fun dueNoteRecognition(nowMillis: Long, limit: Int = 16): List<CaptureRecord> = dueWork("recognition", nowMillis, limit)
    fun dueNoteOrg(nowMillis: Long, limit: Int = 16): List<CaptureRecord> = dueWork("org", nowMillis, limit)
    fun earliestNoteRecognitionDue(): Long? = earliestWork("recognition")
    fun earliestNoteOrgDue(): Long? = earliestWork("org")

    fun requestNoteRecognition(id: String) = synchronized(monitor) {
        val db = helper.writableDatabase
        db.beginTransaction()
        try {
            val record = db.rawQuery("SELECT body FROM captures WHERE capture_id = ?", arrayOf(id.lowercase())).use {
                if (it.moveToFirst()) decode(it.getString(0)) else return@synchronized
            }
            if (record.purpose == CapturePurpose.NOTE && record.captureState == CaptureState.SAVED &&
                record.recognitionState !in setOf(RecognitionState.FINAL, RecognitionState.NO_SPEECH)) {
                db.execSQL("UPDATE captures SET recognition_requested = 1, recognition_eligible = 1, recognition_due = 0 WHERE capture_id = ?", arrayOf(record.captureId))
            }
            db.setTransactionSuccessful()
        } finally { db.endTransaction() }
    }

    fun noteRecognitionRequested(id: String): Boolean = synchronized(monitor) {
        helper.readableDatabase.rawQuery("SELECT recognition_requested FROM captures WHERE capture_id = ?", arrayOf(id.lowercase())).use {
            it.moveToFirst() && it.getInt(0) != 0
        }
    }

    private fun dueWork(stage: String, nowMillis: Long, limit: Int): List<CaptureRecord> = synchronized(monitor) {
        require(stage in setOf("archive", "transfer", "recognition", "org") && limit in 1..100)
        val malformed = mutableListOf<Pair<String, String>>()
        val records = helper.readableDatabase.rawQuery("SELECT capture_id, body FROM captures WHERE ${stage}_eligible = 1 AND ${stage}_due <= ? " +
            "ORDER BY ${stage}_due, created_at_utc, capture_id LIMIT ?", arrayOf(nowMillis.toString(), limit.toString())).use {
            buildList {
                while (it.moveToNext()) {
                    val record = runCatching { decode(it.getString(1)) }.getOrElse { _ ->
                        malformed.add(it.getString(0) to it.getString(1)); null
                    }
                    if (record != null) add(record)
                }
            }
        }
        // Preserve malformed canonical metadata for repair. Only its indexed retry admission moves.
        malformed.forEach { (id, body) -> helper.writableDatabase.execSQL(
            "UPDATE captures SET ${stage}_due = ? WHERE capture_id = ? AND body = ?",
            arrayOf<Any>(nowMillis.coerceAtMost(Long.MAX_VALUE - 60_000L) + 60_000L, id, body)) }
        records
    }

    fun deferNoteRecognitionDiscovery(record: CaptureRecord, nowMillis: Long) = update(record.captureId) { current ->
        val sameAttempt = current.recognitionSessionId == record.recognitionSessionId ||
            (record.recognitionState == RecognitionState.RUNNING && current.recognitionState == RecognitionState.QUEUED && current.recognitionSessionId == null)
        if (current.transcriptVersion != record.transcriptVersion || !sameAttempt ||
            current.captureState != CaptureState.SAVED || current.recognitionState in setOf(RecognitionState.FINAL, RecognitionState.NO_SPEECH)) current
        else current.copy(nextRetryAtUtc = CaptureUtcTime.formatMillis(nowMillis + 30_000L),
            lastErrorStage = "recognition", lastErrorCode = "recognition-work-discovery-failed")
    }

    fun wakeRuntimeRecognition() = synchronized(monitor) {
        helper.writableDatabase.execSQL("UPDATE captures SET recognition_due = 0 WHERE recognition_waiting_runtime = 1 AND recognition_eligible = 1")
    }

    private fun earliestWork(stage: String): Long? = synchronized(monitor) {
        require(stage in setOf("recognition", "org"))
        helper.readableDatabase.rawQuery("SELECT ${stage}_due FROM captures WHERE ${stage}_eligible = 1 ORDER BY ${stage}_due LIMIT 1", null).use {
            if (it.moveToFirst()) it.getLong(0) else null
        }
    }

    fun deferNoteOrg(id: String, transcriptVersion: Long, nowMillis: Long, waitingForConfiguration: Boolean) = synchronized(monitor) {
        val db = helper.writableDatabase
        db.beginTransaction()
        try {
            db.rawQuery("SELECT body, org_attempts FROM captures WHERE capture_id = ?", arrayOf(id)).use {
                if (!it.moveToFirst() || decode(it.getString(0)).transcriptVersion != transcriptVersion) return@synchronized
                val attempts = it.getInt(1)
                val delay = (5_000L shl attempts.coerceIn(0, 10)).coerceAtMost(3_600_000L)
                db.execSQL("UPDATE captures SET org_attempts = ?, org_due = ? WHERE capture_id = ?",
                    arrayOf<Any>(if (waitingForConfiguration) attempts else (attempts + 1).coerceAtMost(20), if (waitingForConfiguration) Long.MAX_VALUE else nowMillis + delay, id))
            }
            db.setTransactionSuccessful()
        } finally { db.endTransaction() }
    }

    fun wakeNoteOrg() = synchronized(monitor) {
        helper.writableDatabase.execSQL("UPDATE captures SET org_due = 0 WHERE org_eligible = 1")
    }

    fun wakeNoteDelivery(id: String) = synchronized(monitor) {
        helper.writableDatabase.execSQL("UPDATE captures SET archive_due = 0, org_due = 0 WHERE capture_id = ?", arrayOf(id.lowercase()))
    }

    /** Native verified archive handoff. Kept separate until live EOF so frame comparison is retained. */
    fun stagePhoneArchive(record: CaptureRecord, nodeId: String) = synchronized(monitor) {
        require(archiveEligible(record))
        val normalized = record.copy(captureId = record.captureId.lowercase())
        val db = helper.writableDatabase
        db.beginTransaction()
        try {
            check(origin(db, normalized.captureId) == nodeId) { "capture-origin-not-authorized" }
            val existing = db.rawQuery("SELECT body FROM phone_pending_archives WHERE capture_id = ?", arrayOf(normalized.captureId)).use {
                if (it.moveToFirst()) {
                    val prior = decode(it.getString(0))
                    check(prior.sha256.equals(record.sha256, true) && prior.finalizedByteLength == record.finalizedByteLength) {
                        "capture-id-content-conflict"
                    }
                    true
                } else false
            }
            val values = ContentValues().apply {
                put("capture_id", normalized.captureId); put("body", encode(normalized).toString())
            }
            if (existing) db.update("phone_pending_archives", values, "capture_id = ?", arrayOf(normalized.captureId))
            else check(db.insertOrThrow("phone_pending_archives", null, values) != -1L) { "Cannot persist verified archive handoff" }
            db.setTransactionSuccessful()
        } finally { db.endTransaction() }
    }

    fun pendingPhoneArchives(limit: Int = 16): List<Pair<CaptureRecord, String>> = synchronized(monitor) {
        require(limit in 1..100)
        helper.readableDatabase.rawQuery("SELECT p.body, o.source_node_id FROM phone_pending_archives p " +
            "JOIN phone_capture_origins o ON o.capture_id = p.capture_id ORDER BY p.capture_id LIMIT ?", arrayOf(limit.toString())).use {
            buildList { while (it.moveToNext()) add(decode(it.getString(0)) to it.getString(1)) }
        }
    }

    fun pendingPhoneArchive(id: String): Pair<CaptureRecord, String>? = synchronized(monitor) {
        helper.readableDatabase.rawQuery("SELECT p.body, o.source_node_id FROM phone_pending_archives p " +
            "JOIN phone_capture_origins o ON o.capture_id = p.capture_id WHERE p.capture_id = ?", arrayOf(id.lowercase())).use {
            if (it.moveToFirst()) decode(it.getString(0)) to it.getString(1) else null
        }
    }

    fun duePhoneArchives(nowMillis: Long, limit: Int = 16): List<Pair<CaptureRecord, String>> = synchronized(monitor) {
        require(limit in 1..100)
        helper.readableDatabase.rawQuery("SELECT p.body, o.source_node_id FROM phone_pending_archives p " +
            "JOIN phone_capture_origins o ON o.capture_id = p.capture_id WHERE p.due <= ? ORDER BY p.due, p.capture_id LIMIT ?",
            arrayOf(nowMillis.toString(), limit.toString())).use {
            buildList { while (it.moveToNext()) add(decode(it.getString(0)) to it.getString(1)) }
        }
    }

    fun earliestPhoneArchiveDue(): Long? = synchronized(monitor) {
        helper.readableDatabase.rawQuery("SELECT p.due FROM phone_pending_archives p JOIN phone_capture_origins o " +
            "ON o.capture_id = p.capture_id ORDER BY p.due LIMIT 1", null).use {
            if (it.moveToFirst()) it.getLong(0) else null
        }
    }

    fun wakePhoneArchives() = synchronized(monitor) {
        helper.writableDatabase.execSQL("UPDATE phone_pending_archives SET due = 0")
    }

    fun wakePhoneArchive(id: String) = synchronized(monitor) {
        helper.writableDatabase.execSQL("UPDATE phone_pending_archives SET due = 0 WHERE capture_id = ?", arrayOf(id.lowercase()))
    }

    fun deferPhoneArchive(id: String, waitingForLive: Boolean) = synchronized(monitor) {
        helper.writableDatabase.execSQL("UPDATE phone_pending_archives SET due = ?, attempts = MIN(attempts + 1, 20) WHERE capture_id = ?",
            arrayOf<Any>(if (waitingForLive) Long.MAX_VALUE else System.currentTimeMillis() + 30_000L, id.lowercase()))
    }

    fun completePhoneArchiveHandoff(id: String) = synchronized(monitor) {
        helper.writableDatabase.delete("phone_pending_archives", "capture_id = ?", arrayOf(id.lowercase()))
    }

    fun get(id: String): CaptureRecord? {
        require(CaptureProtocol.isValidUuid(id)) { "Invalid capture UUID" }
        synchronized(monitor) {
            helper.readableDatabase.rawQuery("SELECT body FROM captures WHERE capture_id = ?", arrayOf(id.lowercase())).use { cursor ->
                return if (cursor.moveToFirst()) decode(cursor.getString(0)) else null
            }
        }
    }

    fun active(purpose: CapturePurpose? = null): CaptureRecord? = synchronized(monitor) {
        helper.readableDatabase.rawQuery(
            "SELECT body FROM captures WHERE capture_state IN ('PREPARING','RECORDING','STOPPING') " +
                "ORDER BY created_at_utc DESC, capture_id DESC",
            null,
        ).use { cursor ->
            var found: CaptureRecord? = null
            while (cursor.moveToNext()) {
                val candidate = decode(cursor.getString(0))
                if (purpose == null || candidate.purpose == purpose) { found = candidate; break }
            }
            found
        }
    }

    /** Domain outboxes avoid unrelated active rows and do not truncate at a history page. */
    fun pendingNoteTransfers(): List<CaptureRecord> = finalizedNotes(includeInterrupted = true).filter { record ->
        record.localAudioPath?.endsWith(".wav") == true &&
            (record.transferState != TransferState.PHONE_ARCHIVED ||
                (record.captureState == CaptureState.SAVED && !record.completionNotified && record.recognitionState != RecognitionState.NO_SPEECH))
    }

    fun savedNotes(): List<CaptureRecord> = finalizedNotes(includeInterrupted = false)

    private fun finalizedNotes(includeInterrupted: Boolean): List<CaptureRecord> = synchronized(monitor) {
        helper.readableDatabase.rawQuery(
            "SELECT body FROM captures WHERE capture_state " +
                (if (includeInterrupted) "IN ('SAVED','INTERRUPTED')" else "= 'SAVED'") +
                " ORDER BY created_at_utc DESC, capture_id DESC", null,
        ).use { cursor -> buildList {
            while (cursor.moveToNext()) decode(cursor.getString(0)).takeIf { it.purpose == CapturePurpose.NOTE }?.let(::add)
        } }
    }

    /** Manual text is terminal authority. This transaction fences all older recognition attempts. */
    fun correctTranscript(id: String, text: String, expectedVersion: Long? = null): CaptureRecord = update(id) { current ->
        require(current.purpose == CapturePurpose.NOTE) { "Only Note transcripts can be edited" }
        require(current.captureState == CaptureState.SAVED) { "Only finalized captures can be edited" }
        require(text.length <= 1_000_000) { "Transcript is too long" }
        require(expectedVersion == null || expectedVersion >= 0) { "Invalid transcript version" }
        if (expectedVersion != null && expectedVersion != current.transcriptVersion) throw CaptureTranscriptConflictException()
        current.copy(finalText = text, previewText = text,
            recognitionState = if (text.isBlank()) RecognitionState.NO_SPEECH else RecognitionState.FINAL,
            recognitionSessionId = null, transcriptVersion = current.transcriptVersion + 1, nextRetryAtUtc = null,
            liveTranscriptionState = if (text.isBlank()) LiveTranscriptionState.UNAVAILABLE else LiveTranscriptionState.COMPLETED,
            liveStatusRevision = current.liveStatusRevision + 1, canTranscribeLive = false,
            liveErrorCode = if (text.isBlank()) "no-speech-detected" else null,
            lastErrorStage = current.lastErrorStage.takeUnless { it in setOf("recognition", "execution", "delivery") },
            lastErrorCode = current.lastErrorCode.takeUnless { current.lastErrorStage in setOf("recognition", "execution", "delivery") },
            orgState = when {
                current.orgState == OrgState.EXPORTED -> OrgState.EXPORTED
                text.isBlank() -> OrgState.WAITING_FOR_TRANSCRIPT
                else -> OrgState.READY
            })
    } ?: throw IllegalArgumentException("Capture was not found")

    fun save(record: CaptureRecord): CaptureRecord = synchronized(monitor) {
        validate(record)
        val normalized = record.copy(captureId = record.captureId.lowercase())
        val db = helper.writableDatabase
        db.rawQuery("SELECT body FROM captures WHERE capture_id = ?", arrayOf(normalized.captureId)).use { cursor ->
            if (cursor.moveToFirst()) {
                val existing = decode(cursor.getString(0))
                check(existing == normalized) { "Capture already exists; use update() for state changes" }
                return@synchronized existing
            }
        }
        check(db.insertWithOnConflict("captures", null, values(normalized), SQLiteDatabase.CONFLICT_ABORT) != -1L) {
            "Cannot persist capture metadata"
        }
        normalized
    }

    /** Phone-local authorization, never serialized in CaptureRecord or replicated over Data Layer. */
    fun phoneOrigin(id: String): String? = synchronized(monitor) {
        require(CaptureProtocol.isValidUuid(id))
        val db = helper.writableDatabase
        ensurePhoneOrigins(db)
        origin(db, id.lowercase())
    }

    fun isPhoneOrigin(id: String, nodeId: String): Boolean = nodeId.isNotBlank() && phoneOrigin(id) == nodeId

    /** Only a new capture can bind. verifiedNewArchive is native verified-body evidence, never a wire field. */
    fun admitPhoneCapture(initial: CaptureRecord, sourceNodeId: String, verifiedNewArchive: Boolean = false): CaptureRecord? = synchronized(monitor) {
        validate(initial)
        require(sourceNodeId.isNotBlank() && sourceNodeId.length <= 256)
        val normalized = initial.copy(captureId = initial.captureId.lowercase())
        val db = helper.writableDatabase
        db.beginTransaction()
        try {
            ensurePhoneOrigins(db)
            val bound = origin(db, normalized.captureId)
            val existing = db.rawQuery("SELECT body FROM captures WHERE capture_id = ?", arrayOf(normalized.captureId)).use {
                if (it.moveToFirst()) decode(it.getString(0)) else null
            }
            if (existing != null) {
                if (bound != sourceNodeId || existing.purpose != initial.purpose ||
                    existing.requestedLanguage != initial.requestedLanguage || existing.speechNetworkFallback != initial.speechNetworkFallback) return@synchronized null
                db.setTransactionSuccessful()
                return@synchronized existing
            }
            if (bound != null) return@synchronized null
            // A START message is not evidence of ownership of orphaned pre-existing local audio.
            val priorFiles = File(File(appContext.filesDir, "captures"), normalized.captureId).listFiles()
            if (!verifiedNewArchive && priorFiles?.any { it.isFile } == true) return@synchronized null
            check(db.insertOrThrow("captures", null, values(normalized)) != -1L)
            check(db.insertOrThrow("phone_capture_origins", null, ContentValues().apply {
                put("capture_id", normalized.captureId); put("source_node_id", sourceNodeId)
            }) != -1L)
            db.setTransactionSuccessful()
            normalized
        } finally { db.endTransaction() }
    }

    private fun ensurePhoneOrigins(db: SQLiteDatabase) {
        db.execSQL("CREATE TABLE IF NOT EXISTS phone_capture_origins (capture_id TEXT PRIMARY KEY NOT NULL, source_node_id TEXT NOT NULL)")
    }
    private fun origin(db: SQLiteDatabase, id: String): String? =
        db.rawQuery("SELECT source_node_id FROM phone_capture_origins WHERE capture_id = ?", arrayOf(id)).use {
            if (it.moveToFirst()) it.getString(0) else null
        }

    fun update(id: String, transform: (CaptureRecord) -> CaptureRecord): CaptureRecord? = synchronized(monitor) {
        require(CaptureProtocol.isValidUuid(id)) { "Invalid capture UUID" }
        val db = helper.writableDatabase
        db.beginTransaction()
        try {
            val row = db.rawQuery("SELECT body, recognition_requested, recognition_due FROM captures WHERE capture_id = ?", arrayOf(id.lowercase())).use { cursor ->
                if (cursor.moveToFirst()) Triple(decode(cursor.getString(0)), cursor.getInt(1) != 0, cursor.getLong(2)) else null
            } ?: return@synchronized null
            val current = row.first
            val updated = transform(current)
            require(updated.captureId.equals(current.captureId, ignoreCase = true)) { "update cannot change capture identity" }
            require(updated.purpose == current.purpose && updated.requestedLanguage == current.requestedLanguage &&
                updated.speechNetworkFallback == current.speechNetworkFallback) { "update cannot change captured speech policy or purpose" }
            validate(updated)
            val normalized = updated.copy(captureId = current.captureId)
            val persisted = values(normalized).apply {
                val consumed = normalized.recognitionState == RecognitionState.FINAL || normalized.recognitionState == RecognitionState.NO_SPEECH ||
                    (normalized.recognitionState == RecognitionState.RUNNING && normalized.recognitionSessionId != current.recognitionSessionId)
                if (consumed) put("recognition_requested", 0)
                else if (row.second && normalized.purpose == CapturePurpose.NOTE && normalized.captureState == CaptureState.SAVED) {
                    put("recognition_eligible", 1)
                    // Pre-admission failures retain the request, but must respect their durable backoff.
                    if (normalized.nextRetryAtUtc == current.nextRetryAtUtc) put("recognition_due", row.third)
                }
                if (normalized.transcriptVersion != current.transcriptVersion) {
                    put("org_due", 0L); put("org_attempts", 0)
                }
            }
            db.update("captures", persisted, "capture_id = ?", arrayOf(current.captureId))
            db.setTransactionSuccessful()
            normalized
        } finally {
            db.endTransaction()
        }
    }

    fun recoverInterrupted(purpose: CapturePurpose? = null): List<CaptureRecord> = synchronized(monitor) {
        val db = helper.writableDatabase
        db.beginTransaction()
        try {
            val active = db.rawQuery(
                "SELECT body FROM captures WHERE capture_state IN ('PREPARING','RECORDING','STOPPING')",
                null,
            ).use { cursor -> buildList { while (cursor.moveToNext()) add(decode(cursor.getString(0))) } }
            val recovered = active.filter { purpose == null || it.purpose == purpose }.map { record ->
                val expectedSpool = spoolFile(record.captureId).canonicalFile
                val recordedSpool = record.localAudioPath?.let(::File)?.canonicalFile
                val invalidSpoolPath = recordedSpool != null && recordedSpool != expectedSpool
                val spool = if (invalidSpoolPath) expectedSpool else recordedSpool ?: expectedSpool
                val alignment = record.format.channels * record.format.bitsPerSample / 8
                if (spool.isFile && spool.length() % alignment != 0L) {
                    RandomAccessFile(spool, "rw").use { file ->
                        file.setLength(file.length() - file.length() % alignment)
                        file.fd.sync()
                    }
                }
                val durableFrames = if (spool.isFile) spool.length() / alignment else 0L
                record.copy(
                    captureState = CaptureState.INTERRUPTED,
                    durableFrames = durableFrames,
                    observedFrames = maxOf(record.observedFrames, durableFrames),
                    localAudioPath = spool.absolutePath.takeIf { spool.exists() },
                    stopReason = record.stopReason ?: "process-recovery",
                    lastErrorStage = "capture-recovery",
                    lastErrorCode = if (invalidSpoolPath) "invalid-spool-path" else "interrupted",
                ).also { db.update("captures", values(it), "capture_id = ?", arrayOf(it.captureId)) }
            }
            db.setTransactionSuccessful()
            recovered
        } finally {
            db.endTransaction()
        }
    }

    fun createPcmSpool(captureId: String): File {
        require(CaptureProtocol.isValidUuid(captureId)) { "Invalid capture UUID" }
        val directory = File(File(appContext.filesDir, "captures"), captureId.lowercase())
        if (!directory.exists() && !directory.mkdirs()) throw IllegalStateException("Cannot create capture directory")
        return File(directory, "audio.pcm.part")
    }

    private fun spoolFile(captureId: String) = File(File(File(appContext.filesDir, "captures"), captureId), "audio.pcm.part")

    private fun validate(record: CaptureRecord) {
        require(CaptureProtocol.isValidUuid(record.captureId)) { "Invalid capture UUID" }
        require(record.schemaVersion == 1) { "Unsupported capture schema version" }
        require(CaptureProtocol.validateFormat(record.format)) { "Unsupported capture format" }
        require(record.observedFrames >= 0 && record.durableFrames >= 0 && record.lastRevision >= 0 && record.transcriptVersion >= 0 && record.liveStatusRevision >= 0)
        require(CaptureProtocol.validateRequestedLanguage(record.requestedLanguage)) { "Invalid requested speech language" }
    }

    private fun values(record: CaptureRecord) = ContentValues().apply {
        put("capture_id", record.captureId)
        put("created_at_utc", record.createdAtUtc)
        put("capture_state", record.captureState.name)
        put("purpose", record.purpose.name)
        put("archive_eligible", if (archiveEligible(record)) 1 else 0)
        putAll(transferProjection(record))
        putAll(phoneProjection(record))
        put("body", encode(record).toString())
    }

    private class Database(context: Context) : SQLiteOpenHelper(context, "captures.db", null, 2) {
        override fun onCreate(db: SQLiteDatabase) {
            db.execSQL("CREATE TABLE captures (capture_id TEXT PRIMARY KEY NOT NULL, created_at_utc TEXT NOT NULL, capture_state TEXT NOT NULL, body TEXT NOT NULL)")
            db.execSQL("CREATE INDEX captures_created ON captures(created_at_utc DESC, capture_id DESC)")
            addWorkColumns(db)
        }
        override fun onUpgrade(db: SQLiteDatabase, oldVersion: Int, newVersion: Int) {
            check(oldVersion == 1 && newVersion == 2)
            addWorkColumns(db)
            // Android API 24 does not guarantee SQLite JSON functions. Decode using legacy defaults.
            db.rawQuery("SELECT capture_id, body FROM captures", null).use { cursor ->
                while (cursor.moveToNext()) {
                    val record = decode(cursor.getString(1))
                    db.execSQL("UPDATE captures SET purpose = ?, archive_eligible = ? WHERE capture_id = ?",
                        arrayOf(record.purpose.name, if (archiveEligible(record)) 1 else 0, cursor.getString(0)))
                    db.update("captures", transferProjection(record), "capture_id = ?", arrayOf(record.captureId))
                    db.update("captures", phoneProjection(record), "capture_id = ?", arrayOf(record.captureId))
                }
            }
        }

        private fun addWorkColumns(db: SQLiteDatabase) {
            db.execSQL("CREATE TABLE IF NOT EXISTS phone_pending_archives (capture_id TEXT PRIMARY KEY NOT NULL, body TEXT NOT NULL, due INTEGER NOT NULL DEFAULT 0, attempts INTEGER NOT NULL DEFAULT 0)")
            db.execSQL("CREATE INDEX IF NOT EXISTS phone_pending_archives_due ON phone_pending_archives(due, capture_id)")
            db.execSQL("CREATE TABLE IF NOT EXISTS phone_capture_origins (capture_id TEXT PRIMARY KEY NOT NULL, source_node_id TEXT NOT NULL)")
            db.execSQL("ALTER TABLE captures ADD COLUMN purpose TEXT NOT NULL DEFAULT 'NOTE'")
            db.execSQL("ALTER TABLE captures ADD COLUMN archive_eligible INTEGER NOT NULL DEFAULT 0")
            db.execSQL("ALTER TABLE captures ADD COLUMN archive_due INTEGER NOT NULL DEFAULT 0")
            db.execSQL("ALTER TABLE captures ADD COLUMN archive_attempts INTEGER NOT NULL DEFAULT 0")
            db.execSQL("ALTER TABLE captures ADD COLUMN transfer_eligible INTEGER NOT NULL DEFAULT 0")
            db.execSQL("ALTER TABLE captures ADD COLUMN transfer_due INTEGER NOT NULL DEFAULT 0")
            db.execSQL("ALTER TABLE captures ADD COLUMN recognition_eligible INTEGER NOT NULL DEFAULT 0")
            db.execSQL("ALTER TABLE captures ADD COLUMN recognition_due INTEGER NOT NULL DEFAULT 0")
            db.execSQL("ALTER TABLE captures ADD COLUMN recognition_requested INTEGER NOT NULL DEFAULT 0")
            db.execSQL("ALTER TABLE captures ADD COLUMN recognition_waiting_runtime INTEGER NOT NULL DEFAULT 0")
            db.execSQL("ALTER TABLE captures ADD COLUMN org_eligible INTEGER NOT NULL DEFAULT 0")
            db.execSQL("ALTER TABLE captures ADD COLUMN org_due INTEGER NOT NULL DEFAULT 0")
            db.execSQL("ALTER TABLE captures ADD COLUMN org_attempts INTEGER NOT NULL DEFAULT 0")
            db.execSQL("CREATE INDEX captures_purpose_created ON captures(purpose, created_at_utc DESC, capture_id DESC)")
            db.execSQL("CREATE INDEX captures_archive_due ON captures(archive_eligible, archive_due, created_at_utc, capture_id)")
            db.execSQL("CREATE INDEX captures_transfer_due ON captures(transfer_eligible, transfer_due, created_at_utc, capture_id)")
            db.execSQL("CREATE INDEX captures_recognition_due ON captures(recognition_eligible, recognition_due, created_at_utc, capture_id)")
            db.execSQL("CREATE INDEX captures_recognition_runtime ON captures(recognition_waiting_runtime, recognition_eligible)")
            db.execSQL("CREATE INDEX captures_org_due ON captures(org_eligible, org_due, created_at_utc, capture_id)")
        }
    }

    companion object {
        private val finalizedStates = setOf(CaptureState.SAVED, CaptureState.INTERRUPTED)
        private val retryableRecognitionStates = setOf(RecognitionState.QUEUED, RecognitionState.FAILED, RecognitionState.UNAVAILABLE)
        private val sha256Pattern = Regex("(?i)^[0-9a-f]{64}$")

        private fun phoneProjection(record: CaptureRecord) = ContentValues().apply {
            val saved = record.purpose == CapturePurpose.NOTE && record.captureState == CaptureState.SAVED
            val recognition = saved && (record.recognitionState == RecognitionState.RUNNING ||
                (record.retryCount < CaptureWorkPolicy.MAX_AUTOMATIC_RECOGNITION_ATTEMPTS && record.recognitionState in retryableRecognitionStates))
            put("recognition_eligible", if (recognition) 1 else 0)
            put("recognition_waiting_runtime", if (saved && record.recognitionState == RecognitionState.QUEUED && record.lastErrorStage == "execution") 1 else 0)
            put("recognition_due", if (record.recognitionState == RecognitionState.RUNNING) 0L else record.nextRetryAtUtc?.let {
                CaptureUtcTime.parseMillis(it)
            } ?: 0L)
            val org = saved && record.recognitionState == RecognitionState.FINAL && !record.finalText.isNullOrBlank() &&
                record.archiveState == ArchiveState.VERIFIED && record.archivedAudioUri != null && record.orgState != OrgState.EXPORTED
            put("org_eligible", if (org) 1 else 0)
        }

        private fun transferProjection(record: CaptureRecord) = ContentValues().apply {
            val eligible = record.purpose == CapturePurpose.NOTE &&
                record.captureState in finalizedStates &&
                record.localAudioPath?.endsWith(".wav") == true && record.finalizedByteLength != null &&
                (record.transferState != TransferState.PHONE_ARCHIVED ||
                    (record.captureState == CaptureState.SAVED && !record.completionNotified && record.recognitionState != RecognitionState.NO_SPEECH))
            put("transfer_eligible", if (eligible) 1 else 0)
            put("transfer_due", record.nextRetryAtUtc?.let {
                CaptureUtcTime.parseMillis(it)
            } ?: 0L)
        }

        private fun archiveEligible(record: CaptureRecord): Boolean =
            record.purpose == CapturePurpose.NOTE &&
                record.captureState in finalizedStates &&
                record.localAudioPath?.endsWith(".wav") == true &&
                record.finalizedByteLength?.let { it >= 44 } == true &&
                record.sha256?.matches(sha256Pattern) == true &&
                record.archiveState != ArchiveState.CONFLICT &&
                !(record.archiveState == ArchiveState.VERIFIED && record.archivedAudioUri != null)

        private fun encode(record: CaptureRecord) = JSONObject().apply {
            put("captureId", record.captureId); put("schemaVersion", record.schemaVersion); put("sourceKind", record.sourceKind)
            put("createdAtUtc", record.createdAtUtc); putOpt("startedAtUtc", record.startedAtUtc); putOpt("finishedAtUtc", record.finishedAtUtc)
            putOpt("finishedLocalDate", record.finishedLocalDate); putOpt("timezoneId", record.timezoneId)
            put("captureState", record.captureState.name); putOpt("stopReason", record.stopReason)
            put("observedFrames", record.observedFrames); put("durableFrames", record.durableFrames)
            put("format", JSONObject().put("sampleRate", record.format.sampleRate).put("channels", record.format.channels).put("bitsPerSample", record.format.bitsPerSample))
            putOpt("localAudioPath", record.localAudioPath); putOpt("archivedAudioUri", record.archivedAudioUri)
            putOpt("finalizedByteLength", record.finalizedByteLength); putOpt("sha256", record.sha256)
            put("recognitionState", record.recognitionState.name); putOpt("recognitionSessionId", record.recognitionSessionId)
            put("lastRevision", record.lastRevision); putOpt("previewText", record.previewText); putOpt("finalText", record.finalText)
            putOpt("engineId", record.engineId); putOpt("language", record.language)
            put("transcriptVersion", record.transcriptVersion); putOpt("exportTranscriptVersion", record.exportTranscriptVersion)
            put("transferState", record.transferState.name); put("archiveState", record.archiveState.name); put("orgState", record.orgState.name)
            putOpt("exportDestinationId", record.exportDestinationId); putOpt("lastErrorStage", record.lastErrorStage)
            putOpt("lastErrorCode", record.lastErrorCode); put("retryCount", record.retryCount); putOpt("nextRetryAtUtc", record.nextRetryAtUtc)
            put("completionNotified", record.completionNotified); putOpt("attentionNotifiedCode", record.attentionNotifiedCode)
            put("purpose", record.purpose.name); putOpt("requestedLanguage", record.requestedLanguage)
            put("speechNetworkFallback", record.speechNetworkFallback)
            put("liveTranscriptionState", record.liveTranscriptionState.name); put("liveStatusRevision", record.liveStatusRevision)
            put("canTranscribeLive", record.canTranscribeLive); putOpt("liveErrorCode", record.liveErrorCode)
        }

        private fun decode(json: String): CaptureRecord {
            val value = JSONObject(json)
            val format = value.getJSONObject("format")
            return CaptureRecord(
                captureId = value.getString("captureId"), schemaVersion = value.getInt("schemaVersion"), sourceKind = value.getString("sourceKind"),
                createdAtUtc = value.getString("createdAtUtc"), startedAtUtc = value.nullableString("startedAtUtc"), finishedAtUtc = value.nullableString("finishedAtUtc"),
                finishedLocalDate = value.nullableString("finishedLocalDate"), timezoneId = value.nullableString("timezoneId"),
                captureState = CaptureState.valueOf(value.getString("captureState")), stopReason = value.nullableString("stopReason"),
                observedFrames = value.getLong("observedFrames"), durableFrames = value.getLong("durableFrames"),
                format = CaptureFormat(format.getInt("sampleRate"), format.getInt("channels"), format.getInt("bitsPerSample")),
                localAudioPath = value.nullableString("localAudioPath"), archivedAudioUri = value.nullableString("archivedAudioUri"),
                finalizedByteLength = value.nullableLong("finalizedByteLength"), sha256 = value.nullableString("sha256"),
                recognitionState = RecognitionState.valueOf(value.getString("recognitionState")), recognitionSessionId = value.nullableString("recognitionSessionId"),
                lastRevision = value.getLong("lastRevision"), previewText = value.nullableString("previewText"), finalText = value.nullableString("finalText"),
                engineId = value.nullableString("engineId"), language = value.nullableString("language"), transcriptVersion = value.getLong("transcriptVersion"),
                exportTranscriptVersion = value.nullableLong("exportTranscriptVersion"), transferState = TransferState.valueOf(value.getString("transferState")),
                archiveState = ArchiveState.valueOf(value.getString("archiveState")), orgState = OrgState.valueOf(value.getString("orgState")),
                exportDestinationId = value.nullableString("exportDestinationId"), lastErrorStage = value.nullableString("lastErrorStage"),
                lastErrorCode = value.nullableString("lastErrorCode"), retryCount = value.getInt("retryCount"), nextRetryAtUtc = value.nullableString("nextRetryAtUtc"),
                completionNotified = value.optBoolean("completionNotified", false), attentionNotifiedCode = value.nullableString("attentionNotifiedCode"),
                purpose = CapturePurpose.valueOf(value.optString("purpose", "NOTE")),
                requestedLanguage = value.nullableString("requestedLanguage"),
                speechNetworkFallback = value.opt("speechNetworkFallback") == true,
                liveTranscriptionState = LiveTranscriptionState.valueOf(value.optString("liveTranscriptionState", "NOT_REQUESTED")),
                liveStatusRevision = value.optLong("liveStatusRevision", 0),
                canTranscribeLive = value.opt("canTranscribeLive") == true,
                liveErrorCode = value.nullableString("liveErrorCode"),
            )
        }

        private fun JSONObject.nullableString(key: String): String? = if (isNull(key)) null else getString(key)
        private fun JSONObject.nullableLong(key: String): Long? = if (isNull(key)) null else getLong(key)
    }
}

class CaptureTranscriptConflictException : IllegalStateException("transcript-conflict")
