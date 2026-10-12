package dev.gitblame.voxgolem.assistant.phone

import app.tauri.annotation.Command
import org.junit.Assert.*
import org.junit.Test

class AssistantCommandBoundaryTest {
    @Test fun nativeCommandsExposeConversationLinkButNoCredentialManagement() {
        val commands = AssistantPlugin::class.java.declaredMethods.filter { it.getAnnotation(Command::class.java) != null }.map { it.name }.toSet()
        assertTrue("assistant_pairing_status" in commands)
        assertTrue("get_app_snapshot" in commands)
        assertFalse("credential_status" in commands)
        assertFalse("import_access_credential" in commands)
        assertFalse("retry_credential_transfer" in commands)
        assertTrue(commands.none { it.contains("credential") })
    }
}
