package dev.gitblame.voxgolem.capture

import android.app.Activity
import org.json.JSONObject
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.Robolectric
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35])
class CapturePluginContractTest {
    @Test
    fun emptySnapshotRetainsRequiredNullableFieldsForFrontendValidation() {
        val activity = Robolectric.buildActivity(Activity::class.java).setup().get()
        val plugin = CapturePlugin(activity)
        val method = CapturePlugin::class.java.getDeclaredMethod("snapshotValue").apply { isAccessible = true }
        val snapshot = JSONObject((method.invoke(plugin) as JSONObject).toString())

        assertTrue(snapshot.has("notesFolderUri"))
        assertTrue(snapshot.isNull("notesFolderUri"))
        assertTrue(snapshot.has("error"))
        assertTrue(snapshot.isNull("error"))
        assertTrue(snapshot.getBoolean("setupRequired"))
        assertTrue(snapshot.getJSONArray("captures").length() == 0)
    }

    @Test
    fun pendingRecordRetainsEveryNullableFieldWithoutPromotingPreviewToFinal() {
        val activity = Robolectric.buildActivity(Activity::class.java).setup().get()
        val plugin = CapturePlugin(activity)
        val record = CaptureRecord(
            captureId = "123e4567-e89b-42d3-a456-426614174030",
            createdAtUtc = "2026-01-02T03:04:05Z",
            previewText = "synthetic preview",
        )
        val method = CapturePlugin::class.java.getDeclaredMethod("recordValue", CaptureRecord::class.java)
            .apply { isAccessible = true }
        val result = JSONObject((method.invoke(plugin, record) as JSONObject).toString())

        for (field in listOf("finishedAtUtc", "finalText", "archivedAudioUri", "error")) {
            assertTrue("Missing required field $field", result.has(field))
            assertTrue("Expected null field $field", result.isNull(field))
        }
        assertFalse(result.isNull("previewText"))
        assertTrue(result.getString("previewText") == "synthetic preview")
    }
}
