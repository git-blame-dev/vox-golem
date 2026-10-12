package dev.gitblame.voxgolem.assistant.replication

import android.app.Application
import dev.gitblame.voxgolem.assistant.runtime.AssistantRuntime

/** Shared optional shell base. Framework events need no Activity, WebView or provider engine. */
open class AssistantApplication : Application(), AssistantPeerAssociationOwner, AssistantPeerSetupOwner {
    override val assistantPeerSetup by lazy { AssistantPeerSetup(this) }
    override val assistantPeerAssociationProvider = PeerAssociationProvider { node -> assistantPeerSetup.replicaForNode(node) }

    override fun onCreate() {
        super.onCreate()
        AssistantReplication.start(this, assistantPeerAssociationProvider, AssistantRuntime::refreshIfStarted)
    }
}
