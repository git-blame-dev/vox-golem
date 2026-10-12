package dev.gitblame.voxgolem.android.wear.assistant

typealias DeltaCallback = dev.gitblame.voxgolem.assistant.runtime.ProviderDeltaCallback
typealias AssistantNativeCalls = dev.gitblame.voxgolem.assistant.runtime.ProviderNativeCalls

object WearAssistantNative : AssistantNativeCalls {
    init { System.loadLibrary("wear_assistant_native") }

    external override fun respond(requestId: String, authPath: String, requestJson: String, callback: DeltaCallback): String
    external override fun cancel(requestId: String)

    external fun apply(request: String): String
}
