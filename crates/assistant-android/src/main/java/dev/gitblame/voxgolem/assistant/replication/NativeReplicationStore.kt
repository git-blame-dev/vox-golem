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
        if (plan.getString("base_revision") != plan.getJSONObject("snapshot").getString("revision")) runCatching { onCommitted() }
    }
    override fun acknowledge(peer: String, receipts: JSONArray) = store.acknowledge(peer, receipts)
    override fun durableReceipts(ids: JSONArray, checkBudget: () -> Unit): JSONArray {
        val wanted = ReplicationWire.arrayObjects(ids).map(ReplicationWire::idKey).toMutableSet()
        val result = JSONArray()
        var after: JSONObject? = null
        repeat(512) {
            checkBudget()
            val page = store.receiptsToPublish(128, after)
            for (receipt in ReplicationWire.arrayObjects(page)) {
                if (wanted.remove(ReplicationWire.idKey(receipt.getJSONObject("operation")))) result.put(receipt)
            }
            if (wanted.isEmpty()) return result
            check(page.length() > 0) { "Assistant committed receipt missing" }
            after = page.getJSONObject(page.length() - 1).getJSONObject("operation")
        }
        error("Assistant committed receipt page limit")
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
