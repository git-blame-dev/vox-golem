package dev.gitblame.voxgolem.assistant.runtime

import android.content.SharedPreferences
import dev.gitblame.voxgolem.assistant.replication.AssistantApplication
import dev.gitblame.voxgolem.assistant.replication.AssistantReplication
import dev.gitblame.voxgolem.assistant.replication.ChatGptAccessSync
import org.junit.Assert.assertFalse
import org.junit.Assert.assertThrows
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RuntimeEnvironment
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import java.io.File

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35], manifest = Config.NONE, application = AssistantBackgroundAdmissionTest.IsolatedApplication::class)
class AssistantBackgroundAdmissionTest {
    class IsolatedApplication : AssistantApplication() {
        override val backgroundServicesEnabled = false
        override fun getSharedPreferences(name: String, mode: Int): SharedPreferences = error("Unexpected normal preferences")
        override fun getNoBackupFilesDir(): File = error("Unexpected normal store")
    }

    @Test fun isolatedApplicationAndGlobalOwnerAdmissionNeverOpenNormalStorage() {
        val app = RuntimeEnvironment.getApplication() as IsolatedApplication
        assertFalse(dev.gitblame.voxgolem.voice.backgroundServicesAdmitted(app))
        AssistantReplication.start(app, app.assistantPeerAssociationProvider)
        ChatGptAccessSync.start(app)
        assertThrows(IllegalStateException::class.java) { AssistantRuntime.get(app) }
    }
}
