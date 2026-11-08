package dev.gitblame.voxgolem.android.wear.assistant

import org.junit.runners.model.FrameworkMethod
import org.robolectric.RobolectricTestRunner
import org.robolectric.internal.bytecode.InstrumentationConfiguration

/** Share JNI declarations across SDK sandboxes, just like the assistant/phone runners. */
class WearAssistantRunner(testClass: Class<*>) : RobolectricTestRunner(testClass) {
    override fun createClassLoaderConfig(method: FrameworkMethod): InstrumentationConfiguration =
        InstrumentationConfiguration.Builder(super.createClassLoaderConfig(method))
            .doNotAcquireClass("dev.gitblame.voxgolem.assistant.AssistantDomainNative")
            .doNotAcquireClass("dev.gitblame.voxgolem.assistant.AssistantStoreNative")
            .doNotAcquireClass("dev.gitblame.voxgolem.assistant.speech.AssistantSpeechNative")
            .doNotAcquireClass("dev.gitblame.voxgolem.assistant.speech.SpeechSelectionNative")
            .doNotAcquireClass("dev.gitblame.voxgolem.assistant.runtime.AssistantProviderNative")
            .doNotAcquireClass("dev.gitblame.voxgolem.assistant.runtime.ProviderNativeCalls")
            .doNotAcquireClass("dev.gitblame.voxgolem.assistant.runtime.ProviderDeltaCallback").build()
}
