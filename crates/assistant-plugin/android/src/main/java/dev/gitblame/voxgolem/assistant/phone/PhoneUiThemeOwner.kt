package dev.gitblame.voxgolem.assistant.phone

import org.json.JSONObject

interface PhoneUiThemeOwner {
    fun applyUiTheme(theme: String)
}

/** Confined to the UI thread alongside native bar appearance updates. */
internal class PhoneUiThemePublication(private val owner: PhoneUiThemeOwner) {
    private var preferencesPublished = false
    private var lastTheme: String? = null

    companion object {
        fun readTheme(result: Result<Any?>, preferences: Boolean): String? {
            val value = result.getOrNull() as? JSONObject ?: return null
            val theme = (if (preferences) value else value.optJSONObject("preferences"))
                ?.optString("ui_theme") ?: return null
            return theme.takeIf { it == "dark" || it == "slate" || it == "light" }
        }
    }

    fun publish(theme: String?, preferences: Boolean) {
        if (theme == null) return
        if (preferences) preferencesPublished = true
        else if (preferencesPublished) return
        if (lastTheme == theme) return
        lastTheme = theme
        owner.applyUiTheme(theme)
    }
}
