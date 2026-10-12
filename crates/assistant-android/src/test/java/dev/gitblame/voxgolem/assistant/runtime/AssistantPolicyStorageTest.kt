package dev.gitblame.voxgolem.assistant.runtime

import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35], manifest = Config.NONE)
class AssistantPolicyStorageTest {
    @Test fun installationPolicyDefaultsOnAndSurvivesNewStorageOwners() {
        val context = RuntimeEnvironment.getApplication()
        val storage = NativeAssistantPolicyStorage(context)
        assertEquals(AssistantPolicy(), storage.read())
        storage.write(AssistantPolicy(webEnabled = false, speechNetworkFallback = false))
        assertEquals(AssistantPolicy(false, false), NativeAssistantPolicyStorage(context).read())
        storage.write(AssistantPolicy(true, false))
        assertEquals(AssistantPolicy(true, false), NativeAssistantPolicyStorage(context).read())
        val selected = AssistantPolicy(false, true, "en-AU", "synthetic.tts", "synthetic-au-local")
        storage.write(selected)
        assertEquals(selected, NativeAssistantPolicyStorage(context).read())
        val shared = dev.gitblame.voxgolem.voice.InstallationPreferences(context)
        assertEquals(selected.selection(), shared.read().selection())
        shared.write(dev.gitblame.voxgolem.voice.InstallationSettings(false, false, "de-DE"))
        assertEquals(AssistantPolicy(false, false, "de-DE"), storage.read())
        storage.write(selected)
        val oversized = "界".repeat(171)
        assertThrows(IllegalArgumentException::class.java) { storage.write(selected.copy(speechTtsProvider = oversized)) }
        assertThrows(IllegalArgumentException::class.java) { storage.write(selected.copy(speechTtsVoice = oversized)) }
        assertEquals(selected, NativeAssistantPolicyStorage(context).read())
        assertThrows(IllegalArgumentException::class.java) { storage.write(selected.copy(speechLanguage = "en--AU")) }
        assertEquals(selected, NativeAssistantPolicyStorage(context).read())
    }
}
