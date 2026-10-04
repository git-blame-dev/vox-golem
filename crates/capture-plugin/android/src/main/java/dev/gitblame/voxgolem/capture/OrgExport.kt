package dev.gitblame.voxgolem.capture

import java.nio.charset.StandardCharsets.UTF_8
import java.text.ParsePosition
import java.text.SimpleDateFormat
import java.io.ByteArrayInputStream
import java.io.ByteArrayOutputStream
import java.io.InputStream
import java.io.OutputStream
import java.util.Locale
import java.util.TimeZone

object OrgExport {
    fun formatEntry(finalText: String, finishedLocalDate: String): ByteArray {
        val date = parseLocalDate(finishedLocalDate)
        val headline = collapseWhitespace(finalText)
        require(headline.isNotEmpty()) { "An empty transcript cannot be exported" }
        require(!headline.contains('\u0000')) { "Transcript contains an invalid NUL character" }
        val scheduleDate = SimpleDateFormat("yyyy-MM-dd EEE", Locale.ENGLISH).apply { timeZone = UTC }.format(date)
        return "* TODO $headline\nSCHEDULED: <$scheduleDate>\n\n".toByteArray(UTF_8)
    }

    fun prepend(entry: ByteArray, existing: ByteArray): ByteArray {
        val output = ByteArrayOutputStream(entry.size + existing.size)
        writePrepended(entry, ByteArrayInputStream(existing), output)
        return output.toByteArray()
    }

    internal fun writePrepended(entry: ByteArray, existing: InputStream, output: OutputStream) {
        val prefix = ByteArray(3)
        var prefixLength = 0
        while (prefixLength < prefix.size) {
            val count = existing.read(prefix, prefixLength, prefix.size - prefixLength)
            if (count < 0) break
            prefixLength += count
        }
        val hasBom = prefixLength == 3 && prefix[0] == 0xef.toByte() && prefix[1] == 0xbb.toByte() && prefix[2] == 0xbf.toByte()
        if (hasBom) output.write(prefix) else output.write(entry)
        if (hasBom) output.write(entry) else if (prefixLength > 0) output.write(prefix, 0, prefixLength)
        existing.copyTo(output)
    }

    private fun collapseWhitespace(text: String): String {
        val result = StringBuilder(text.length)
        var pendingSpace = false
        for (character in text) {
            if (character.isWhitespace() || Character.isSpaceChar(character)) {
                pendingSpace = result.isNotEmpty()
            } else {
                if (pendingSpace) result.append(' ')
                result.append(character)
                pendingSpace = false
            }
        }
        return result.toString()
    }

    private fun parseLocalDate(value: String): java.util.Date {
        require(Regex("^[0-9]{4}-[0-9]{2}-[0-9]{2}$").matches(value) && !value.startsWith("0000-")) {
            "Finish-local-date must be yyyy-MM-dd"
        }
        val parser = SimpleDateFormat("yyyy-MM-dd", Locale.ROOT).apply {
            isLenient = false
            timeZone = UTC
        }
        val position = ParsePosition(0)
        val parsed = parser.parse(value, position)
        require(parsed != null && position.index == value.length) { "Invalid finish-local-date" }
        return parsed
    }

    private val UTC: TimeZone get() = TimeZone.getTimeZone("UTC")
}
