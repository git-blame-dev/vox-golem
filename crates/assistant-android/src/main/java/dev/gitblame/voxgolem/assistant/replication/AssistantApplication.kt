package dev.gitblame.voxgolem.assistant.replication

import android.app.Application
import dev.gitblame.voxgolem.assistant.runtime.AssistantRuntime

/** Shared optional shell base. Framework events need no Activity, WebView or provider engine. */
open class AssistantApplication : Application(), AssistantPeerAssociationOwner, AssistantPeerSetupOwner,
    dev.gitblame.voxgolem.voice.AndroidBackgroundAdmissionOwner {
    override val backgroundServicesEnabled: Boolean get() = true
    override val assistantPeerSetup by lazy { AssistantPeerSetup(this) }
    override val assistantPeerAssociationProvider = PeerAssociationProvider { node -> assistantPeerSetup.replicaForNode(node) }

    override fun onCreate() {
        super.onCreate()
        if (!backgroundServicesEnabled) return
        AssistantReplication.start(this, assistantPeerAssociationProvider, AssistantRuntime::refreshIfStarted)
        ChatGptAccessSync.start(this)
    }
}
