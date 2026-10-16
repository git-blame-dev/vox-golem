package dev.gitblame.voxgolem.assistant.replication

import org.json.JSONObject
import org.junit.Assert.*
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TemporaryFolder
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import java.io.File

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [24, 35], manifest = Config.NONE)
class ChatGptAccessProtocolTest {
    @get:Rule val folder = TemporaryFolder()
    private val now = 1_800_000_000_000L
    private fun credential() = JSONObject().put("voxgolemChatgpt", 1).put("openai", JSONObject()
        .put("type", "oauth").put("access", "synthetic-access-only").put("expires", now + 3_600_000))
    private fun packet(generation: Long = 1, value: Any = credential()) = JSONObject().put("version", 1)
        .put("association", "synthetic-association").put("nonce", "synthetic-nonce")
        .put("generation", generation).put("credential", value).toString().toByteArray()
    private fun decode(bytes: ByteArray, previous: Long = 0) = ChatGptAccessProtocol.packet(bytes,
        "synthetic-association", "synthetic-nonce", previous, now)
    private fun rejects(work: () -> Unit) {
        try { work(); fail("Expected credential rejection") } catch (_: IllegalArgumentException) {}
    }

    @Test fun acceptsOnlyExplicitAccessFieldsAndRejectsRefreshOrIdTokenAtEitherLevel() {
        assertEquals("synthetic-access-only", decode(packet()).getJSONObject("openai").getString("access"))
        for (field in listOf("refresh", "refreshToken", "idToken", "id_token")) {
            val nested = credential().apply { getJSONObject("openai").put(field, "synthetic-forbidden-token") }
            rejects { decode(packet(value = nested)) }
            rejects { decode(packet(value = credential().put(field, "synthetic-forbidden-token"))) }
        }
        rejects { decode(packet(value = credential().apply { getJSONObject("openai").put("access", "synthetic\r\nheader") })) }
        rejects { decode(packet(value = credential().apply { getJSONObject("openai").put("type", "api") })) }
    }

    @Test fun expiredOversizedMalformedAndUnsafeExpiryCannotBePublished() {
        for (expiry in listOf<Any>(now, now - 1, -1, "synthetic-expiry", now + 0.5, Double.MAX_VALUE, now + 31L * 24 * 60 * 60 * 1000)) {
            rejects { decode(packet(value = credential().apply { getJSONObject("openai").put("expires", expiry) })) }
        }
        rejects { decode(ByteArray(ChatGptAccessProtocol.MAX_BYTES + 1)) }
        rejects { decode(packet(value = credential().put("unexpected", JSONObject()))) }
    }

    @Test fun signoutIsATombstoneAndOldGenerationsOrPreviousRequestCannotRestoreAccess() {
        val signedOut = decode(packet(2, JSONObject.NULL), previous = 1)
        assertFalse(signedOut.has("openai"))
        assertTrue(signedOut.getBoolean("signedOut"))
        assertEquals(2, signedOut.getJSONObject(ChatGptAccessProtocol.META).getInt("generation"))
        rejects { decode(packet(1), previous = 2) }
        rejects { decode(packet(2), previous = 2) }
        rejects { decode(JSONObject(packet().toString(Charsets.UTF_8)).put("nonce", "synthetic-old-request").toString().toByteArray()) }
        rejects { decode(JSONObject(packet().toString(Charsets.UTF_8)).put("association", "synthetic-old-association").toString().toByteArray()) }
    }

    @Test fun atomicCommitApprovalFencePreservesPreviousSnapshotOnRevocation() {
        val file = File(folder.root, "chatgpt-auth.json")
        ChatGptAccessProtocol.publish(file, decode(packet()))
        val before = file.readBytes()
        try {
            ChatGptAccessProtocol.publish(file, decode(packet(2, JSONObject.NULL))) { false }
            fail("Expected revoked publication rejection")
        } catch (_: IllegalStateException) {}
        assertArrayEquals(before, file.readBytes())
        assertEquals("synthetic-access-only", ChatGptAccessProtocol.read(file)!!.getJSONObject("openai").getString("access"))
    }

    @Test fun persistenceReceiptsAreCredentialFreeAndBindExactAssociationNonceAndGeneration() {
        val packet = JSONObject(packet().toString(Charsets.UTF_8))
        val receipt = ChatGptAccessProtocol.receiptFor(packet)
        assertEquals(setOf("association", "nonce", "generation"), ChatGptAccessProtocol.keys(receipt))
        assertTrue(ChatGptAccessProtocol.sameReceipt(receipt, ChatGptAccessProtocol.receipt(receipt.toString().toByteArray())))
        for (field in listOf("association", "nonce", "generation")) {
            val wrong = JSONObject(receipt.toString()).put(field, if (field == "generation") 2 else "synthetic-stale")
            assertFalse(ChatGptAccessProtocol.sameReceipt(receipt, wrong))
        }
        rejects { ChatGptAccessProtocol.receipt(JSONObject(receipt.toString()).put("refresh", "synthetic-forbidden-token").toString().toByteArray()) }
    }
}
