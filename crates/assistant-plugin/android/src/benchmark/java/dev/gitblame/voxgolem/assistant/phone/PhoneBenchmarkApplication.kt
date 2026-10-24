package dev.gitblame.voxgolem.assistant.phone

/** No persistent toggle: the fixed same-signer instrumentation must reserve this process first. */
class PhoneBenchmarkApplication : dev.gitblame.voxgolem.assistant.replication.AssistantApplication() {
    override val backgroundServicesEnabled: Boolean get() = !PhoneSyntheticVoiceBenchmark.processReserved()
}
