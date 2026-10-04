package dev.gitblame.voxgolem.capture

import android.app.Activity
import android.Manifest
import android.content.Intent
import android.media.MediaPlayer
import android.net.Uri
import android.provider.DocumentsContract
import androidx.activity.result.ActivityResult
import app.tauri.annotation.ActivityCallback
import app.tauri.annotation.Command
import app.tauri.annotation.InvokeArg
import app.tauri.annotation.PermissionCallback
import app.tauri.annotation.Permission
import app.tauri.annotation.TauriPlugin
import app.tauri.plugin.Invoke
import app.tauri.plugin.JSObject
import app.tauri.plugin.JSArray
import app.tauri.plugin.Plugin
import java.io.File
import java.time.Instant
import java.time.Duration
import org.json.JSONObject

@TauriPlugin(
    permissions = [
        Permission(alias = "phoneAudio", strings = [Manifest.permission.RECORD_AUDIO]),
        Permission(alias = "phoneConnectedDevice", strings = [Manifest.permission.BLUETOOTH_CONNECT]),
        Permission(alias = "phoneNotifications", strings = [Manifest.permission.POST_NOTIFICATIONS]),
    ],
)
class CapturePlugin(private val activity: Activity) : Plugin(activity) {
    private val repository by lazy { CaptureRepository(activity) }
    private val destinationStore by lazy { SafNotesStore(activity) }
    private val phoneCoordinator by lazy { PhoneCaptureCoordinator.shared(activity.applicationContext) }
    private var player: MediaPlayer? = null
    @Volatile private var activityVisible = false

    override fun onResume() {
        activityVisible = true
    }

    override fun onPause() {
        activityVisible = false
    }

    override fun load(webView: android.webkit.WebView) {
        // Enabling is always explicit; loading the plugin never starts a foreground service.
    }

    @Command
    fun snapshot(invoke: Invoke) {
        try {
            invoke.resolve(snapshotValue())
        } catch (error: Exception) {
            invoke.reject(error.message ?: "Cannot load voice note history")
        }
    }

    /** Explicit user action. Runtime permissions are requested only from this visible onboarding command. */
    @Command
    fun enable_capture(invoke: Invoke) {
        if (isWatch()) {
            invoke.reject("Phone capture setup is unavailable on Wear OS")
            return
        }
        if (!activityVisible) {
            invoke.reject("Phone capture can only be enabled from the visible app")
            return
        }
        val aliases = buildList {
            if (activity.checkSelfPermission(Manifest.permission.RECORD_AUDIO) != android.content.pm.PackageManager.PERMISSION_GRANTED) add("phoneAudio")
            if (android.os.Build.VERSION.SDK_INT >= 31 && activity.checkSelfPermission(Manifest.permission.BLUETOOTH_CONNECT) != android.content.pm.PackageManager.PERMISSION_GRANTED) add("phoneConnectedDevice")
            if (android.os.Build.VERSION.SDK_INT >= 33 && activity.checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS) != android.content.pm.PackageManager.PERMISSION_GRANTED) add("phoneNotifications")
        }
        if (aliases.isNotEmpty()) {
            requestPermissionForAliases(aliases.toTypedArray(), invoke, "capturePermissionsGranted")
            return
        }
        resolveEnable(invoke)
    }

    @PermissionCallback
    private fun capturePermissionsGranted(invoke: Invoke) = resolveEnable(invoke)

    @Command
    fun list_captures(invoke: Invoke) {
        try {
            val args = invoke.parseArgs(ListCapturesArgs::class.java)
            require(args.offset >= 0 && args.limit in 1..100) { "Invalid capture page" }
            invoke.resolve(JSObject().put("captures", JSArray(repository.list(args.offset, args.limit).map(::recordValue))))
        } catch (error: Exception) {
            invoke.reject(error.message ?: "Cannot load voice note history")
        }
    }

    @Command
    fun select_notes_folder(invoke: Invoke) {
        val intent = Intent(Intent.ACTION_OPEN_DOCUMENT_TREE).apply {
            addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION or Intent.FLAG_GRANT_WRITE_URI_PERMISSION or Intent.FLAG_GRANT_PERSISTABLE_URI_PERMISSION)
            if (android.os.Build.VERSION.SDK_INT >= 26) putExtra(DocumentsContract.EXTRA_INITIAL_URI, destinationStore.selected()?.treeUri)
        }
        startActivityForResult(invoke, intent, "notesFolderResult")
    }

    @ActivityCallback
    private fun notesFolderResult(invoke: Invoke, result: ActivityResult) {
        if (result.resultCode != Activity.RESULT_OK) {
            invoke.reject("Notes folder selection was cancelled")
            return
        }
        val uri = result.data?.data
        if (uri == null) {
            invoke.reject("No notes folder was selected")
            return
        }
        try {
            val flags = result.data?.flags ?: 0
            val requiredFlags = Intent.FLAG_GRANT_READ_URI_PERMISSION or Intent.FLAG_GRANT_WRITE_URI_PERMISSION
            require(flags and requiredFlags == requiredFlags) { "Notes folder requires read and write access" }
            activity.contentResolver.takePersistableUriPermission(
                uri,
                Intent.FLAG_GRANT_READ_URI_PERMISSION or Intent.FLAG_GRANT_WRITE_URI_PERMISSION,
            )
            destinationStore.select(uri)
            phoneCoordinator.retryPendingCaptures()
            invoke.resolve(snapshotValue())
        } catch (error: Exception) {
            invoke.reject(error.message ?: "Cannot save notes folder permission")
        }
    }

    @Command
    fun play_capture(invoke: Invoke) {
        try {
            val args = invoke.parseArgs(CaptureIdArgs::class.java)
            require(CaptureProtocol.isValidUuid(args.captureId)) { "Invalid capture ID" }
            val record = repository.get(args.captureId) ?: error("Capture was not found")
            require(record.captureState in setOf(CaptureState.SAVED, CaptureState.INTERRUPTED)) { "Recording is not finalized yet" }
            stopPlayer()
            val audioUri = record.archivedAudioUri?.let(Uri::parse)
            val audioFile = record.localAudioPath?.let(::File)
            require(audioUri != null || audioFile?.isFile == true) { "Recording audio is unavailable" }
            player = MediaPlayer().apply {
                if (audioUri != null) setDataSource(activity, audioUri) else setDataSource(audioFile!!.absolutePath)
                setOnCompletionListener { stopPlayer() }
                setOnErrorListener { _, _, _ -> stopPlayer(); true }
                setOnPreparedListener { it.start() }
                prepareAsync()
            }
            invoke.resolve()
        } catch (error: Exception) {
            stopPlayer()
            invoke.reject(error.message ?: "Cannot play recording")
        }
    }

    @Command
    fun stop_playback(invoke: Invoke) {
        stopPlayer()
        invoke.resolve()
    }

    @Command
    fun retry_capture(invoke: Invoke) {
        try {
            check(!isWatch()) { "Phone capture retry is unavailable on Wear OS" }
            val args = invoke.parseArgs(CaptureIdArgs::class.java)
            require(CaptureProtocol.isValidUuid(args.captureId)) { "Invalid capture ID" }
            // PhoneCoordinator owns recognition retry and is provided by the host integration.
            phoneCoordinator.retryCapture(args.captureId)
            invoke.resolve()
        } catch (error: Exception) {
            invoke.reject(error.cause?.message ?: "Capture retry is unavailable")
        }
    }

    @Command
    fun finalize_capture(invoke: Invoke) {
        try {
            val args = invoke.parseArgs(CaptureIdArgs::class.java)
            phoneCoordinator.finalizeCapture(args.captureId)
            invoke.resolve()
        } catch (error: Exception) {
            invoke.reject(error.message ?: "Cannot finalize interrupted capture")
        }
    }

    @Command
    fun update_transcript(invoke: Invoke) {
        try {
            val args = invoke.parseArgs(UpdateTranscriptArgs::class.java)
            require(CaptureProtocol.isValidUuid(args.captureId)) { "Invalid capture ID" }
            require(args.text.length <= 1_000_000) { "Transcript is too long" }
            val updated = repository.update(args.captureId) { record ->
                require(record.captureState == CaptureState.SAVED) { "Only finalized captures can be edited" }
                record.copy(
                    finalText = args.text,
                    previewText = args.text,
                    recognitionState = if (args.text.isBlank()) RecognitionState.NO_SPEECH else RecognitionState.FINAL,
                    transcriptVersion = record.transcriptVersion + 1,
                    orgState = when {
                        record.orgState == OrgState.EXPORTED -> OrgState.EXPORTED
                        args.text.isBlank() -> OrgState.WAITING_FOR_TRANSCRIPT
                        else -> OrgState.READY
                    },
                )
            }
            require(updated != null) { "Capture was not found" }
            if (args.text.isNotBlank()) phoneCoordinator.retryCapture(args.captureId)
            invoke.resolve()
        } catch (error: Exception) {
            invoke.reject(error.message ?: "Cannot update transcript")
        }
    }

    private fun snapshotValue(): JSObject = JSObject().apply {
        val destination = destinationStore.selected()
        val runtimeError = activity.getSharedPreferences(PhoneCaptureCoordinator.PREFERENCES, Activity.MODE_PRIVATE)
            .getString(PhoneCaptureCoordinator.KEY_RUNTIME_ERROR, null)
        val enabled = phoneCoordinator.isEnabled() && PhoneCaptureRuntimeService.isForegroundReady()
        put("available", true)
        put("notesFolderUri", destination?.treeUri?.toString() ?: JSONObject.NULL)
        put("captures", JSArray(repository.list(0, 100).map(::recordValue)))
        put("error", JSONObject.NULL)
        put("setupRequired", !enabled || runtimeError != null)
        put("onboardingState", when {
            runtimeError != null -> "failed"
            !enabled -> "setup-required"
            PhoneCaptureRuntimeService.isForegroundReady() -> "enabled"
            else -> "starting"
        })
        put("onboardingError", runtimeError ?: JSONObject.NULL)
    }

    private fun resolveEnable(invoke: Invoke) {
        val result = phoneCoordinator.enableFromVisibleActivity()
        val response = snapshotValue().apply {
            put("setupRequired", result.state != "enabled")
            put("onboardingState", result.state)
            put("onboardingError", result.error ?: JSONObject.NULL)
        }
        invoke.resolve(response)
    }

    private fun recordValue(record: CaptureRecord): JSObject = JSObject().apply {
        val started = record.startedAtUtc?.let { runCatching { Instant.parse(it) }.getOrNull() }
        val finished = record.finishedAtUtc?.let { runCatching { Instant.parse(it) }.getOrNull() }
        val duration = if (started != null && finished != null) Duration.between(started, finished).toMillis().coerceAtLeast(0)
            else record.durableFrames * 1000 / record.format.sampleRate
        put("captureId", record.captureId)
        put("createdAtUtc", record.createdAtUtc)
        put("finishedAtUtc", record.finishedAtUtc ?: JSONObject.NULL)
        put("durationMs", duration)
        put("captureState", record.captureState.name)
        put("recognitionState", record.recognitionState.name)
        put("transferState", record.transferState.name)
        put("archiveState", record.archiveState.name)
        put("orgState", record.orgState.name)
        put("previewText", record.previewText ?: JSONObject.NULL)
        put("finalText", record.finalText ?: JSONObject.NULL)
        put("archivedAudioUri", record.archivedAudioUri ?: JSONObject.NULL)
        put("error", record.lastErrorCode ?: record.lastErrorStage ?: JSONObject.NULL)
        put("transcriptVersion", record.transcriptVersion)
    }

    private fun stopPlayer() {
        player?.runCatching { stop() }
        player?.release()
        player = null
    }

    private fun isWatch(): Boolean = activity.packageManager.hasSystemFeature("android.hardware.type.watch")

    override fun onDestroy() {
        stopPlayer()
    }
}

@InvokeArg
class CaptureIdArgs {
    lateinit var captureId: String
}

@InvokeArg
class ListCapturesArgs {
    var offset: Int = 0
    var limit: Int = 100
}

@InvokeArg
class UpdateTranscriptArgs {
    lateinit var captureId: String
    lateinit var text: String
}
