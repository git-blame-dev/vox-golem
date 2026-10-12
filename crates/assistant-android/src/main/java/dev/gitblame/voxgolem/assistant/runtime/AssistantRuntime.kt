package dev.gitblame.voxgolem.assistant.runtime

import android.content.Context
import dev.gitblame.voxgolem.assistant.AssistantStore
import dev.gitblame.voxgolem.assistant.AssistantTiming
import java.util.concurrent.Executors

/** Phone and Wear use this process owner; native listeners must not create another store/engine. */
object AssistantRuntime {
    @Volatile private var engine: AssistantEngine? = null
    @Volatile private var captureAdmissionReady = false

    /** Enable only after Note/live/replay/probe owners all use RecognitionResourceLease.process. */
    fun configureCaptureResourceAdmission(ready: Boolean) { captureAdmissionReady = ready }
    fun captureResourceAdmissionReady(): Boolean = captureAdmissionReady

    fun refreshIfStarted() { engine?.refresh() }

    @Synchronized fun get(context: Context): AssistantEngine {
        engine?.let { return it }
        val application = context.applicationContext
        val credentials = ProviderAuthSource(application)
        val io = Executors.newSingleThreadExecutor { command -> Thread(command, "assistant-store") }
        val provider = Executors.newSingleThreadExecutor { command -> Thread(command, "assistant-provider") }
        val modelStorage = object : AssistantModelStorage {
            private val preferences by lazy { application.getSharedPreferences("assistant-runtime", Context.MODE_PRIVATE) }
            override fun read() = preferences.getString("model", "custom-sol-high") ?: "custom-sol-high"
            override fun write(value: String) { check(preferences.edit().putString("model", value).commit()) }
        }
        return AssistantEngine(AssistantStore(application), io, AssistantRequestWorker(provider),
            credentials::pathForProvider, modelStorage, credentials, NativeAssistantPolicyStorage(application),
            beginTiming = { AssistantTiming.begin(application, it) }).also { engine = it }
    }
}
