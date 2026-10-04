package dev.gitblame.voxgolem.capture

import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertThrows
import org.junit.Test
import java.nio.charset.StandardCharsets.UTF_8

class OrgExportTest {
    @Test
    fun formatsSyntheticMultilineTranscriptAndPreservesInboxBytesIncludingBom() {
        val entry = OrgExport.formatEntry("  Review   the\nproject\tchecklist  ", "2026-10-03")
        assertArrayEquals(
            "* TODO Review the project checklist\nSCHEDULED: <2026-10-03 Sat>\n\n".toByteArray(UTF_8),
            entry,
        )
        val original = byteArrayOf(0xef.toByte(), 0xbb.toByte(), 0xbf.toByte()) + "* Existing\r\n\u2603".toByteArray(UTF_8)
        val prepended = OrgExport.prepend(entry, original)
        assertArrayEquals(byteArrayOf(0xef.toByte(), 0xbb.toByte(), 0xbf.toByte()), prepended.copyOfRange(0, 3))
        assertArrayEquals(entry, prepended.copyOfRange(3, 3 + entry.size))
        assertArrayEquals(original.copyOfRange(3, original.size), prepended.copyOfRange(3 + entry.size, prepended.size))
    }

    @Test
    fun collapsesAllWhitespaceWithoutChangingOriginalEncodingContract() {
        assertArrayEquals(
            "* TODO one two three\nSCHEDULED: <2026-01-01 Thu>\n\n".toByteArray(UTF_8),
            OrgExport.formatEntry("one\r\ntwo\u2003three", "2026-01-01"),
        )
    }

    @Test
    fun rejectsEmptyTranscriptAndInvalidFinishDateInsteadOfCreatingAnEmptyTask() {
        assertThrows(IllegalArgumentException::class.java) { OrgExport.formatEntry(" \r\n\t", "2026-10-03") }
        assertThrows(IllegalArgumentException::class.java) { OrgExport.formatEntry("synthetic task", "2026-02-30") }
    }
}
