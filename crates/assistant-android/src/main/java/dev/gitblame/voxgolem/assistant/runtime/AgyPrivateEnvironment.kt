package dev.gitblame.voxgolem.assistant.runtime

import android.content.Context
import java.io.File

/** Access and installation identity must be provisioned externally in app-private storage.
 * Never source either from APK assets. The CLI uses its defaults or externally supplied settings.
 */
internal object AgyPrivateEnvironment {
    private const val EMPTY_ENTRIES_JSON = "{\"entries\":[]}"
    private const val VOX_ASSISTANT_AGENT_MD = """---
name: vox-assistant
description: Pure direct voice and chat assistant
excludeDefaultComponents: true
inheritCustomizations: false
inheritMcp: false
enable_write_tools: false
enable_mcp_tools: false
enable_subagent_tools: false
disabledTools:
  - read_url_content
  - read_browser_page
  - list_browser_pages
  - open_browser_url
  - click_browser_pixel
  - browser_move_mouse
  - browser_mouse_down
  - browser_mouse_up
  - browser_scroll_dom
  - browser_drag
  - browser_press_key
  - browser_key_down
  - browser_key_up
  - browser_Input_text
  - browser_select_option
  - capture_browser_screenshot
  - capture_browser_console_logs
  - execute_browser_javascript
  - resize_browser_window
  - view_file
  - view_file_outline
  - view_code_item
  - grep_search
  - find_by_name
  - list_dir
  - write_to_file
  - replace_file_content
  - multi_replace_file_content
  - sed_file
  - run_command
  - manage_task
  - invoke_subagent
  - define_subagent
  - manage_subagents
  - send_message
  - schedule
  - ask_question
  - search_marketplace
---
You are VoxGolem, a fast, direct voice and chat assistant.
Answer the user directly and concisely in a single pass.
Do not use built-in file, shell, browser, or subagent tools.
When webEvidence is provided in the input payload, synthesize your answer directly from that evidence without calling search_web.
"""

    fun isProvisioned(context: Context): Boolean {
        val directory = File(context.filesDir, "home/.gemini/antigravity-cli")
        return listOf("antigravity-oauth-token", "installation_id").all { name ->
            File(directory, name).let { it.isFile && it.length() > 0 }
        }
    }

    private fun writeIfChanged(file: File, content: String) {
        if (file.isFile && file.length() == content.toByteArray(Charsets.UTF_8).size.toLong()) {
            if (file.readText(Charsets.UTF_8) == content) {
                return
            }
        }
        val dir = file.parentFile
        if (dir != null && !dir.exists()) {
            check(dir.mkdirs() || dir.isDirectory) { "Failed to create directory: ${dir.absolutePath}" }
        }
        file.writeText(content, Charsets.UTF_8)
    }

    fun prepare(context: Context): Boolean {
        if (!isProvisioned(context)) return false
        val workspace = File(context.filesDir, "ws")
        check(workspace.isDirectory || workspace.mkdirs()) { "Agy workspace preparation failed" }

        val geminiConfigDir = File(context.filesDir, "home/.gemini/config")
        writeIfChanged(File(geminiConfigDir, "skills.json"), EMPTY_ENTRIES_JSON)
        writeIfChanged(File(geminiConfigDir, "plugins.json"), EMPTY_ENTRIES_JSON)

        val agentsDir = File(workspace, ".agents")
        val agentSubDir = File(agentsDir, "agents")
        check(agentSubDir.isDirectory || agentSubDir.mkdirs()) { "Failed to create agent dir: ${agentSubDir.absolutePath}" }

        writeIfChanged(File(agentsDir, "skills.json"), EMPTY_ENTRIES_JSON)
        writeIfChanged(File(agentsDir, "rules.json"), EMPTY_ENTRIES_JSON)
        writeIfChanged(File(agentSubDir, "vox-assistant.md"), VOX_ASSISTANT_AGENT_MD)

        return true
    }
}
