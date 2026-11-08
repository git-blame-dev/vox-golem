package dev.gitblame.voxgolem.assistant.phone

import org.json.JSONObject
import org.junit.Assert.assertEquals
import org.junit.Test

class PhoneUiThemePublicationTest {
    private class Owner : PhoneUiThemeOwner {
        val applied = mutableListOf<String>()
        override fun applyUiTheme(theme: String) { applied.add(theme) }
    }

    private fun preferences(theme: String): Result<Any?> = Result.success(JSONObject().put("ui_theme", theme))
    private fun snapshot(theme: String): Result<Any?> = Result.success(
        JSONObject().put("preferences", JSONObject().put("ui_theme", theme))
    )

    private fun PhoneUiThemePublication.publishResult(result: Result<Any?>, preferences: Boolean) {
        publish(PhoneUiThemePublication.readTheme(result, preferences), preferences)
    }

    @Test fun initialSnapshotAdoptsEachSupportedTheme() {
        for (theme in listOf("dark", "slate", "light")) {
            val owner = Owner()
            PhoneUiThemePublication(owner).publishResult(snapshot(theme), preferences = false)
            assertEquals(listOf(if (theme == "slate") "dark" else theme), owner.applied)
        }
    }

    @Test fun successfulPreferencesPublishInOrderAndBlockStaleSnapshots() {
        val owner = Owner()
        val publication = PhoneUiThemePublication(owner)
        publication.publishResult(snapshot("dark"), preferences = false)
        publication.publishResult(preferences("light"), preferences = true)
        publication.publishResult(snapshot("dark"), preferences = false)
        publication.publishResult(preferences("slate"), preferences = true)
        assertEquals(listOf("dark", "light", "dark"), owner.applied)
    }

    @Test fun failedWriteDoesNotApplyOrBlockInitialSnapshot() {
        val owner = Owner()
        val publication = PhoneUiThemePublication(owner)
        publication.publishResult(Result.failure(IllegalStateException("synthetic write failure")), preferences = true)
        assertEquals(emptyList<String>(), owner.applied)
        publication.publishResult(snapshot("slate"), preferences = false)
        assertEquals(listOf("dark"), owner.applied)
    }

    @Test fun directLegacyPublicationNormalizesBeforeDeduplicationAndClosesSnapshotGate() {
        val owner = Owner()
        val publication = PhoneUiThemePublication(owner)
        publication.publish("sepia", preferences = true)
        publication.publish("dark", preferences = false)
        publication.publish("slate", preferences = true)
        publication.publish("light", preferences = false)
        assertEquals(listOf("dark"), owner.applied)
    }

    @Test fun invalidResultsAreIgnoredAndDuplicatesStillCloseSnapshotGate() {
        val owner = Owner()
        val publication = PhoneUiThemePublication(owner)
        publication.publishResult(preferences("sepia"), preferences = true)
        publication.publishResult(Result.success(JSONObject()), preferences = true)
        publication.publishResult(Result.success(null), preferences = true)
        assertEquals(emptyList<String>(), owner.applied)
        publication.publishResult(snapshot("light"), preferences = false)
        publication.publishResult(snapshot("light"), preferences = false)
        publication.publishResult(preferences("light"), preferences = true)
        publication.publishResult(snapshot("dark"), preferences = false)
        publication.publishResult(preferences("sepia"), preferences = true)
        assertEquals(listOf("light"), owner.applied)
    }
}
