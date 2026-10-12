package dev.gitblame.voxgolem.android.wear.assistant

import android.content.Context
import android.content.SharedPreferences
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment
import org.robolectric.annotation.Config
import java.util.UUID

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35])
class WearVoicePreferencesTest {
    private val context = RuntimeEnvironment.getApplication()
    private val name = "synthetic-voice-preferences-${UUID.randomUUID()}"
    private fun preferences() = context.getSharedPreferences(name, Context.MODE_PRIVATE)
    private fun reopen() = WearVoiceSettings(preferences()).read()

    @Test fun missingKeysEnableBothChoicesAndSaveThemBeforeReopening() {
        assertEquals(WearVoicePreferences(true, true), WearVoicePreferences())
        assertEquals(WearVoicePreferences(true, true), reopen())
        assertEquals(true, preferences().all["read-replies-aloud"])
        assertEquals(true, preferences().all["conversation-mode"])
        assertEquals(WearVoicePreferences(true, true), reopen())
    }

    @Test fun reopeningRetainsExplicitFalseAndTrueIndependently() {
        val settings = WearVoiceSettings(preferences())
        settings.setReadRepliesAloud(false)
        assertEquals(WearVoicePreferences(false, true), reopen())
        assertEquals(false, preferences().all["read-replies-aloud"])
        settings.setConversationMode(false)
        assertEquals(WearVoicePreferences(false, false), reopen())
        assertEquals(false, preferences().all["conversation-mode"])
        settings.setReadRepliesAloud(true)
        assertEquals(WearVoicePreferences(true, false), reopen())
        settings.setConversationMode(true)
        assertEquals(WearVoicePreferences(true, true), reopen())
    }

    @Test fun existingFalseAndUnrelatedModelRemainUntouched() {
        assertTrue(preferences().edit().putBoolean("conversation-mode", false)
            .putString("model", "synthetic-model").commit())
        assertEquals(WearVoicePreferences(true, false), reopen())
        WearVoiceSettings(preferences()).setReadRepliesAloud(false)
        assertEquals(WearVoicePreferences(false, false), reopen())
        assertEquals("synthetic-model", preferences().getString("model", null))
    }

    @Test fun explicitFalseReplyChoiceIsNotMistakenForAMissingKey() {
        assertTrue(preferences().edit().putBoolean("read-replies-aloud", false).commit())
        assertEquals(WearVoicePreferences(false, true), reopen())
        assertEquals(false, preferences().all["read-replies-aloud"])
        assertEquals(true, preferences().all["conversation-mode"])
    }

    @Test fun eachSetterThrowsWhenSynchronousCommitFails() {
        var commits = 0
        val actual = preferences()
        val failed = object : SharedPreferences by actual {
            override fun edit(): SharedPreferences.Editor {
                val editor = actual.edit()
                return object : SharedPreferences.Editor by editor {
                    override fun putBoolean(key: String?, value: Boolean): SharedPreferences.Editor = this
                    override fun commit(): Boolean { commits++; return false }
                    override fun apply() { fail("Must synchronously commit") }
                }
            }
        }
        val settings = WearVoiceSettings(failed)
        assertThrows(IllegalStateException::class.java) { settings.setReadRepliesAloud(false) }
        assertThrows(IllegalStateException::class.java) { settings.setConversationMode(false) }
        assertThrows(IllegalStateException::class.java) { settings.read() }
        assertEquals(3, commits)
    }
}
