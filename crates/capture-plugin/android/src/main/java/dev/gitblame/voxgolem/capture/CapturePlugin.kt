package dev.gitblame.voxgolem.capture

import android.app.Activity
import android.content.Intent
import android.media.MediaPlayer
import android.net.Uri
import android.provider.DocumentsContract
import androidx.activity.result.ActivityResult
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.LifecycleOwner
import androidx.lifecycle.DefaultLifecycleObserver
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

@TauriPlugin(
    permissions = [
        Permission(alias = NativePhoneRequirements.AUDIO_ALIAS, strings = [NativePhoneRequirements.AUDIO_PERMISSION]),
        Permission(alias = NativePhoneRequirements.CONNECTED_DEVICE_ALIAS, strings = [NativePhoneRequirements.CONNECTED_DEVICE_PERMISSION]),
        Permission(alias = NativePhoneRequirements.NOTIFICATIONS_ALIAS, strings = [NativePhoneRequirements.NOTIFICATIONS_PERMISSION]),
    ],
)
class CapturePlugin(private val activity: Activity) : Plugin(activity) {
    private val repository by lazy { CaptureRepository(activity) }
    private val destinationStore by lazy { SafNotesStore(activity) }
    private val phoneCoordinator by lazy { PhoneCaptureCoordinator.shared(activity.applicationContext) }
    private var player: MediaPlayer? = null

    override fun load(webView: android.webkit.WebView) {
        (activity as? LifecycleOwner)?.lifecycle?.addObserver(object : DefaultLifecycleObserver {
            override fun onResume(owner: LifecycleOwner) {
                if (!isWatch() && phoneCoordinator.isEnabled() && !PhoneCaptureRuntimeService.isForegroundReady()) {
                    phoneCoordinator.start()
                }
            }
        })
    }

    @Command
    fun snapshot(invoke: Invoke) {
        try {
            invoke.resolve(snapshotValue())
        } catch (error: Exception) {
            invoke.reject("capture-history-unavailable")
        }
    }

    /** Explicit user action. Runtime permissions are requested only from this visible onboarding command. */
    @Command
    fun enable_capture(invoke: Invoke) {
        if (isWatch()) {
            invoke.reject("Phone capture setup is unavailable on Wear OS")
            return
        }
        if ((activity as? LifecycleOwner)?.lifecycle?.currentState?.isAtLeast(Lifecycle.State.RESUMED) != true) {
            invoke.reject("Phone capture can only be enabled from the visible app")
            return
        }
        val aliases = NativePhoneRequirements.missing(activity).map { it.alias }
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
            invoke.reject("capture-history-unavailable")
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
            destinationStore.select(uri)
            phoneCoordinator.retryPendingCaptures()
            invoke.resolve(snapshotValue())
        } catch (error: Exception) {
            invoke.reject("notes-folder-permission-unavailable")
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
            invoke.reject("recording-playback-unavailable")
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
            invoke.reject("capture-retry-unavailable")
        }
    }

    @Command
    fun finalize_capture(invoke: Invoke) {
        try {
            val args = invoke.parseArgs(CaptureIdArgs::class.java)
            phoneCoordinator.finalizeCapture(args.captureId)
            invoke.resolve()
        } catch (error: Exception) {
            invoke.reject("capture-finalization-unavailable")
        }
    }

    @Command
    fun update_transcript(invoke: Invoke) {
        try {
            val args = invoke.parseArgs(UpdateTranscriptArgs::class.java)
            require(CaptureProtocol.isValidUuid(args.captureId)) { "Invalid capture ID" }
            require(args.text.length <= 1_000_000) { "Transcript is too long" }
            val updated = repository.update(args.captureId) { record ->
                require(record.purpose == CapturePurpose.NOTE) { "Only Note transcripts can be edited" }
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
            invoke.reject("transcript-update-unavailable")
        }
    }

    private fun projectionStatus() = NativeProjectionStatus(
        consentEnabled = phoneCoordinator.hasConsent(),
        foregroundReady = PhoneCaptureRuntimeService.isForegroundReady(),
        runtimeError = activity.getSharedPreferences(PhoneCaptureCoordinator.PREFERENCES, Activity.MODE_PRIVATE)
            .getString(PhoneCaptureCoordinator.KEY_RUNTIME_ERROR, null),
        permissionError = NativePhoneRequirements.missing(activity).firstOrNull()?.error,
    )

    private fun snapshotValue(): JSObject = NativeCaptureProjection.snapshot(destinationStore.selected(), repository.list(0, 100), projectionStatus())

    private fun resolveEnable(invoke: Invoke) {
        val result = phoneCoordinator.enableFromVisibleActivity()
        invoke.resolve(NativeCaptureProjection.snapshot(destinationStore.selected(), repository.list(0, 100), projectionStatus(), result))
    }

    private fun recordValue(record: CaptureRecord): JSObject = NativeCaptureProjection.record(record)

    private fun stopPlayer() {
        player?.runCatching { stop() }
        player?.release()
        player = null
    }

    private fun isWatch(): Boolean = NativePhoneRequirements.isWatch(activity)

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
