package dev.gitblame.voxgolem.assistant.phone

import app.tauri.annotation.Command
import org.junit.Assert.*
import org.junit.Test
import org.json.JSONObject

class AssistantCommandBoundaryTest {
    @Test fun optionalNewChatDraftDoesNotTurnJsonNullIntoText() {
        assertEquals("", EnginePhoneBackend.newConversationDraft(JSONObject()))
        assertEquals("", EnginePhoneBackend.newConversationDraft(JSONObject().put("draft", JSONObject.NULL)))
        assertEquals("", EnginePhoneBackend.newConversationDraft(JSONObject().put("draft", "")))
        assertEquals("null", EnginePhoneBackend.newConversationDraft(JSONObject().put("draft", "null")))
        assertEquals("Synthetic retained draft", EnginePhoneBackend.newConversationDraft(JSONObject().put("draft", "Synthetic retained draft")))
    }
    @Test fun nativeCommandsExposeConversationLinkButNoCredentialManagement() {
        val commands = AssistantPlugin::class.java.declaredMethods.filter { it.getAnnotation(Command::class.java) != null }.map { it.name }.toSet()
        assertTrue("assistant_pairing_status" in commands)
        assertTrue("get_app_snapshot" in commands)
        assertFalse("credential_status" in commands)
        assertFalse("import_access_credential" in commands)
        assertFalse("retry_credential_transfer" in commands)
        assertTrue(commands.none { it.contains("credential") })
        assertFalse("set_search_mode" in commands)
        assertFalse("select_search_mode" in commands)
    }

    @Test fun promptDtoContainsOnlyRequestIdentityPromptAndDeliveryChannel() {
        val fields = SubmitArgs::class.java.declaredFields.filterNot { it.isSynthetic }.map { it.name }.toSet()
        assertEquals(setOf("requestId", "prompt", "updates"), fields)
    }
}
