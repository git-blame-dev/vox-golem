package dev.gitblame.voxgolem.assistant.phone

import android.app.Activity
import android.os.Looper
import app.tauri.plugin.Invoke
import dev.gitblame.voxgolem.assistant.recognition.*
import dev.gitblame.voxgolem.assistant.runtime.AssistantRuntime
import org.json.JSONObject
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.Robolectric
import org.robolectric.RuntimeEnvironment
import org.robolectric.Shadows.shadowOf
import org.robolectric.annotation.Config
import org.robolectric.annotation.LooperMode
import org.robolectric.shadows.ShadowSpeechRecognizer

@RunWith(PhoneAssistantRunner::class)
@Config(sdk = [35], manifest = Config.NONE)
@LooperMode(LooperMode.Mode.PAUSED)
class PhoneFinishOwnershipTest {
    private class Backend : PhoneSpeechBackend {
        override fun busy() = false
        override fun saveSpeechDraft(text: String) {}
        override fun stopSpeechRequest(requestId: String) {}
        override fun nativePreferences() = JSONObject().put("tts_enabled", false)
        override var speechStatus: JSONObject? = null
        override var onPreferencesChanged: ((Boolean) -> Unit)? = null
    }
    private class Recognizer : LocalRecognizerSession {
        var stops = 0
        var cancelled = false
        var destroyed = false
        override fun checkLanguage(language: String, result: (Boolean) -> Unit) = result(true)
        override fun start(language: String, events: LocalRecognizerEvents) {}
        override fun stop() { stops++ }
        override fun cancel() { cancelled = true }
        override fun destroy() { destroyed = true }
    }
    private fun drain() = shadowOf(Looper.getMainLooper()).idle()
    private fun finish(plugin: AssistantPlugin, args: JSONObject): List<String> {
        val replies = mutableListOf<String>()
        val response: (Long, String) -> Unit = { _, data -> replies += data }
        val mapper = Class.forName("com.fasterxml.jackson.databind.ObjectMapper").getDeclaredConstructor().newInstance()
        val invoke = Invoke::class.java.declaredConstructors.single()
            .newInstance(1L, "finish_assistant_dictation", 1L, 2L, response, args.toString(), mapper) as Invoke
        plugin.finish_assistant_dictation(invoke)
        return replies
    }

    @Test fun staleFinishCannotFinalizeSuccessorButMatchingAndLegacyFinishStillWork() {
        AssistantRuntime.configureCaptureResourceAdmission(true)
        ShadowSpeechRecognizer.setIsOnDeviceRecognitionAvailable(true)
        val sessions = mutableListOf<Recognizer>()
        val factory = object : LocalRecognizerFactory {
            override fun available() = true
            override fun create() = Recognizer().also { sessions += it }
        }
        val phone = PhoneSpeechSession(RuntimeEnvironment.getApplication(), Backend(), SharedDictationOwner(factory, { true }, RecognitionResourceLease()))
        val activity = Robolectric.buildActivity(Activity::class.java).create().get()
        val plugin = AssistantPlugin(activity, phone)
        try {
            phone.foreground(true); drain()
            phone.start("synthetic-old", false, "", {}); drain()
            phone.stop(); drain()
            phone.start("synthetic-next", false, "", {}); drain()
            val next = sessions.last()
            val worker = java.util.concurrent.Executors.newSingleThreadExecutor()
            try {
                val replies = worker.submit<List<String>> { finish(plugin, JSONObject().put("sessionId", "synthetic-old")) }
                    .get(2, java.util.concurrent.TimeUnit.SECONDS)
                assertTrue("Ownership decision must be queued on main", replies.isEmpty())
                drain(); assertEquals(1, replies.size)
            } finally { worker.shutdownNow() }
            assertEquals(0, next.stops); assertFalse(next.cancelled); assertFalse(next.destroyed)
            assertTrue(phone.ownsSession("synthetic-next"))
            for (id in listOf(JSONObject.NULL, 7, "", " ", "é".repeat(65))) {
                val replies = finish(plugin, JSONObject().put("sessionId", id))
                assertTrue(replies.single().contains("invalid_input"))
                assertEquals(0, next.stops)
            }
            assertFalse(finish(plugin, JSONObject().put("sessionId", "é".repeat(64))).single().contains("invalid_input"))
            finish(plugin, JSONObject().put("sessionId", "synthetic-next")); drain()
            assertEquals(1, next.stops)
            assertEquals("finishing", phone.status().getString("dictation"))
            phone.stop(); drain()
            phone.start("synthetic-legacy", false, "", {}); drain()
            finish(plugin, JSONObject()); drain()
            assertEquals(1, sessions.last().stops)
        } finally {
            phone.close(); drain(); activity.finish()
            AssistantRuntime.configureCaptureResourceAdmission(false)
        }
    }
}
