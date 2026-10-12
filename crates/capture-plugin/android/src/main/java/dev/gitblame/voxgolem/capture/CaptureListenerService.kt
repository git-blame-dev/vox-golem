package dev.gitblame.voxgolem.capture

import com.google.android.gms.wearable.ChannelClient
import com.google.android.gms.wearable.DataEventBuffer
import com.google.android.gms.wearable.MessageEvent
import com.google.android.gms.wearable.WearableListenerService

/** GMS may destroy this dispatch service while accepted channel streams are still active. */
class CaptureListenerService : WearableListenerService() {
    override fun onDataChanged(events: DataEventBuffer) =
        CaptureChannelReceiver.shared(applicationContext).onDataChanged(events)

    override fun onMessageReceived(event: MessageEvent) =
        CaptureChannelReceiver.shared(applicationContext).onMessageReceived(event)

    override fun onChannelOpened(channel: ChannelClient.Channel) =
        CaptureChannelReceiver.shared(applicationContext).onChannelOpened(channel)

    override fun onChannelClosed(channel: ChannelClient.Channel, closeReason: Int, appSpecificErrorCode: Int) =
        PhoneReceiptChannelSender.confirmNativeRetired(channel)

    companion object {
        const val ACTION_STATE = "dev.gitblame.voxgolem.capture.STATE"
    }
}
