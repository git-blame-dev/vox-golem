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
            "SELECT body FROM captures ORDER BY created_at_utc DESC, capture_id DESC", null,
        ).use { cursor ->
            buildList {
                while (size < limit && cursor.moveToNext()) {
                    val record = decode(cursor.getString(0))
                    if (record.purpose == purpose) add(record)
                }
            }
        }
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
            val current = db.rawQuery("SELECT body FROM captures WHERE capture_id = ?", arrayOf(id.lowercase())).use { cursor ->
                if (cursor.moveToFirst()) decode(cursor.getString(0)) else null
            } ?: return@synchronized null
            val updated = transform(current)
            require(updated.captureId.equals(current.captureId, ignoreCase = true)) { "update cannot change capture identity" }
            require(updated.purpose == current.purpose && updated.requestedLanguage == current.requestedLanguage &&
                updated.speechNetworkFallback == current.speechNetworkFallback) { "update cannot change captured speech policy or purpose" }
            validate(updated)
            val normalized = updated.copy(captureId = current.captureId)
            db.update("captures", values(normalized), "capture_id = ?", arrayOf(current.captureId))
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
        put("body", encode(record).toString())
    }

    private class Database(context: Context) : SQLiteOpenHelper(context, "captures.db", null, 1) {
        override fun onCreate(db: SQLiteDatabase) {
            db.execSQL("CREATE TABLE captures (capture_id TEXT PRIMARY KEY NOT NULL, created_at_utc TEXT NOT NULL, capture_state TEXT NOT NULL, body TEXT NOT NULL)")
            db.execSQL("CREATE INDEX captures_created ON captures(created_at_utc DESC, capture_id DESC)")
        }
        override fun onUpgrade(db: SQLiteDatabase, oldVersion: Int, newVersion: Int) {
            throw IllegalStateException("No migration path from capture schema $oldVersion to $newVersion")
        }
    }

    companion object {
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
