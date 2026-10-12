package dev.gitblame.voxgolem.assistant.replication

import android.app.job.JobParameters
import android.app.job.JobService
import com.google.android.gms.wearable.CapabilityInfo
import com.google.android.gms.wearable.DataEvent
import com.google.android.gms.wearable.DataEventBuffer
import com.google.android.gms.wearable.Node
import com.google.android.gms.wearable.WearableListenerService
import java.util.concurrent.Future
import java.util.concurrent.atomic.AtomicBoolean

class AssistantReplicationListenerService : WearableListenerService() {
    override fun onDataChanged(events: DataEventBuffer) {
        var count = 0
        for (event in events) {
            if (count++ == 64) break
            if (event.type == DataEvent.TYPE_CHANGED) AssistantReplication.onData(applicationContext, event.dataItem)
        }
    }
    override fun onPeerConnected(node: Node) {
        AssistantReplication.reconcileNow(applicationContext)
    }
    override fun onPeerDisconnected(node: Node) { AssistantReplication.reconcileNow(applicationContext) }
    override fun onCapabilityChanged(info: CapabilityInfo) {
        if (info.name == ReplicationWire.CAPABILITY) AssistantReplication.reconcileNow(applicationContext)
    }
}

class AssistantReplicationJobService : JobService() {
    internal var beginWork: ((() -> Unit) -> Future<*>?) = { done -> AssistantReplication.runJob(applicationContext, done) }
    internal var finishWork: (JobParameters) -> Unit = { jobFinished(it, false) }
    private data class RunningJob(val active: AtomicBoolean, val future: Future<*>)
    private val work = HashMap<Int, RunningJob>()
    override fun onStartJob(params: JobParameters): Boolean {
        val active = AtomicBoolean(true)
        val future = beginWork { if (active.getAndSet(false)) finishWork(params) } ?: return false
        synchronized(work) { work[params.jobId] = RunningJob(active, future) }
        return true
    }
    override fun onStopJob(params: JobParameters): Boolean {
        synchronized(work) { work.remove(params.jobId)?.let { it.active.set(false); it.future.cancel(true) } }
        return true
    }
}
