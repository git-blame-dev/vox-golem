package dev.gitblame.voxgolem.assistant.replication

import dev.gitblame.voxgolem.assistant.AssistantDomain
import dev.gitblame.voxgolem.assistant.AssistantStore
import dev.gitblame.voxgolem.assistant.DomainGateway
import org.json.JSONArray
import org.json.JSONObject

internal class NativeReplicationStore(private val store: AssistantStore, private val onCommitted: () -> Unit = {}) : ReplicationStore {
    override val replica = store.replica()
    override fun snapshot() = store.snapshot()
    override fun publications(peer: String, after: JSONObject?, limit: Int) = store.publications(peer, limit, after = after)
    override fun importRecords(input: JSONArray) {
        val plan = store.importRecords(input)
        if (plan.getString("base_revision") != plan.getString("revision")) runCatching { onCommitted() }
    }
    override fun importVerified(input: JSONArray, entries: JSONArray): JSONArray {
        val plan = store.replication(JSONObject().put("kind", "import").put("records", input).put("entries", entries)) as JSONObject
        if (plan.getString("base_revision") != plan.getString("revision")) runCatching { onCommitted() }
        return plan.getJSONArray("receipts")
    }
    override fun acknowledge(peer: String, receipts: JSONArray) = store.acknowledge(peer, receipts)
    override fun nativeRead(request: JSONObject): Any = store.replication(request)
    override fun retainedDescriptors(entries: JSONArray, revision: String?): JSONObject =
        store.replication(JSONObject().put("kind", "retained_descriptors").put("revision", revision ?: JSONObject.NULL)
            .put("entries", entries)) as JSONObject
    override fun durableReceipts(ids: JSONArray, checkBudget: () -> Unit): JSONArray {
        checkBudget()
        return store.replication(JSONObject().put("kind", "receipts").put("ids", ids)) as JSONArray
    }
}

internal class NativeReplicationDomain(gateway: DomainGateway) : ReplicationDomain {
    private val domain = AssistantDomain(gateway)
    override fun inventory(snapshot: JSONObject, cursor: JSONObject) = domain.evaluate("inventory", snapshot.toString(),
        JSONObject().put("request", JSONObject().put("cursor", cursor).put("max_items", ReplicationWire.MAX_ITEMS))) as JSONObject
    override fun reconcile(snapshot: JSONObject, page: JSONObject) = domain.evaluate("reconcile", snapshot.toString(), JSONObject().put("page", page)) as JSONObject
    override fun receipts(snapshot: JSONObject, replica: String, ids: JSONArray) = domain.evaluate("receipts", snapshot.toString(),
        JSONObject().put("recipient", replica).put("operation_ids", ids)) as JSONArray
}
