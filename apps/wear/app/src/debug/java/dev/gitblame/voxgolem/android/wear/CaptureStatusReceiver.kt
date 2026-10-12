package dev.gitblame.voxgolem.android.wear

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import com.google.android.gms.tasks.Tasks
import com.google.android.gms.wearable.Wearable
import dev.gitblame.voxgolem.capture.CaptureRepository
import org.json.JSONArray
import org.json.JSONObject
import java.io.File
import java.util.concurrent.TimeUnit

class CaptureStatusReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        val pending = goAsync()
        Thread {
            try {
                val rows = JSONArray()
                CaptureRepository(context).list(limit = 3).forEach { record ->
                    val audio = record.localAudioPath?.let(::File)
                    rows.put(JSONObject()
                        .put("captureState", record.captureState.name)
                        .put("recognitionState", record.recognitionState.name)
                        .put("transferState", record.transferState.name)
                        .put("archiveState", record.archiveState.name)
                        .put("orgState", record.orgState.name)
                        .put("hasPreview", !record.previewText.isNullOrBlank())
                        .put("hasFinal", !record.finalText.isNullOrBlank())
                        .put("observedFrames", record.observedFrames)
                        .put("audioExists", audio?.isFile == true)
                        .put("audioBytes", audio?.length() ?: 0)
                        .put("expectedAudioBytes", record.finalizedByteLength ?: 0)
                        .putOpt("stopReason", diagnosticCode(record.stopReason))
                        .putOpt("errorStage", diagnosticCode(record.lastErrorStage))
                        .putOpt("errorCode", diagnosticCode(record.lastErrorCode))
                        .put("hasError", record.lastErrorCode != null))
                }
                val nodes = runCatching {
                    Tasks.await(Wearable.getNodeClient(context).connectedNodes, 5, TimeUnit.SECONDS).size
                }.getOrNull()
                pending.resultData = JSONObject().put("recent", rows)
                    .put("connectedCompanionNodes", nodes ?: JSONObject.NULL).toString()
            } catch (_: Exception) {
                pending.resultData = "{\"statusError\":true}"
            } finally { pending.finish() }
        }.start()
    }

    private fun diagnosticCode(value: String?): String? = value?.takeIf {
        it.matches(Regex("[a-z][a-z0-9_-]{0,79}"))
    }
}
