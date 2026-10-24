package dev.gitblame.voxgolem.assistant.phone

import dev.gitblame.voxgolem.benchmark.android.BenchmarkVoiceContract

/** Instrumentation budget fence only; canonical speech/turn admission remains in existing owners. */
internal class PhoneBenchmarkAdmission(private val contract: BenchmarkVoiceContract, private val liveAllowed: Boolean = true) {
    private var voice = false
    private var prompt = false
    private var retiring = false
    @Synchronized fun retire() { retiring = true }
    @Synchronized fun isRetiring() = retiring
    @Synchronized fun attachResources(prepare: () -> Unit): Boolean {
        if (retiring) return false
        prepare()
        return true
    }

    @Synchronized fun allowsClick(modelAvailable: Boolean, frontendReady: Boolean) = !retiring && liveAllowed && modelAvailable && frontendReady
    @Synchronized fun voiceAdmissions() = if (voice) 1 else 0
    @Synchronized fun promptAdmissions() = if (prompt) 1 else 0

    @Synchronized fun voice(ready: Boolean, policy: String, draft: String, loop: Boolean, shortcut: String?) {
        check(!retiring && liveAllowed && ready && !voice)
        require(policy == "single_turn" && draft.isEmpty() && !loop && shortcut == null)
        voice = true
    }

    @Synchronized fun prompt(ready: Boolean, finals: Int, requestId: String, text: String) {
        check(!retiring && liveAllowed && ready && voice && finals == 1 && !prompt)
        require(requestId.matches(Regex("voice-[a-f0-9]{8}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{12}")) && contract.matches(text))
        prompt = true
    }
}
