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

    fun get(id: String): CaptureRecord? {
        require(CaptureProtocol.isValidUuid(id)) { "Invalid capture UUID" }
        synchronized(monitor) {
            helper.readableDatabase.rawQuery("SELECT body FROM captures WHERE capture_id = ?", arrayOf(id.lowercase())).use { cursor ->
                return if (cursor.moveToFirst()) decode(cursor.getString(0)) else null
            }
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
            validate(updated)
            val normalized = updated.copy(captureId = current.captureId)
            db.update("captures", values(normalized), "capture_id = ?", arrayOf(current.captureId))
            db.setTransactionSuccessful()
            normalized
        } finally {
            db.endTransaction()
        }
    }

    fun recoverInterrupted(): List<CaptureRecord> = synchronized(monitor) {
        val db = helper.writableDatabase
        db.beginTransaction()
        try {
            val active = db.rawQuery(
                "SELECT body FROM captures WHERE capture_state IN ('PREPARING','RECORDING','STOPPING')",
                null,
            ).use { cursor -> buildList { while (cursor.moveToNext()) add(decode(cursor.getString(0))) } }
            val recovered = active.map { record ->
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
        require(record.observedFrames >= 0 && record.durableFrames >= 0 && record.lastRevision >= 0 && record.transcriptVersion >= 0)
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
            )
        }

        private fun JSONObject.nullableString(key: String): String? = if (isNull(key)) null else getString(key)
        private fun JSONObject.nullableLong(key: String): Long? = if (isNull(key)) null else getLong(key)
    }
}
