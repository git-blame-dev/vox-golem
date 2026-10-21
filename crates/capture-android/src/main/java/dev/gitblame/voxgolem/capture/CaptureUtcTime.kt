package dev.gitblame.voxgolem.capture

import android.icu.text.SimpleDateFormat
import android.icu.util.TimeZone
import android.icu.util.GregorianCalendar
import java.text.ParsePosition
import java.util.Date
import java.util.Locale

/** Canonical persisted UTC deadlines, using the API-24 platform ICU parser (no desugaring edge). */
internal object CaptureUtcTime {
    private val utcShape = Regex("[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}(\\.[0-9]{1,9})?Z")
    private fun formatter(fractional: Boolean) = SimpleDateFormat(
        if (fractional) "yyyy-MM-dd'T'HH:mm:ss.SSS'Z'" else "yyyy-MM-dd'T'HH:mm:ss'Z'", Locale.ROOT,
    ).apply {
        calendar = GregorianCalendar(TimeZone.getTimeZone("UTC"), Locale.ROOT).apply { gregorianChange = Date(Long.MIN_VALUE) }
        isLenient = false
    }
    // ICU date formats are mutable. Reuse two per worker, never across threads.
    private val formats = object : ThreadLocal<Pair<SimpleDateFormat, SimpleDateFormat>>() {
        override fun initialValue() = formatter(false) to formatter(true)
    }

    fun parseMillis(value: String): Long? {
        if (!utcShape.matches(value)) return null
        val position = ParsePosition(0)
        val available = checkNotNull(formats.get())
        val parsed = (if (value.indexOf('.') >= 0) available.second else available.first).parse(value, position) ?: return null
        return parsed.time.takeIf { position.index == value.length && position.errorIndex < 0 }
    }

    fun formatMillis(value: Long): String = checkNotNull(formats.get()).second.format(Date(value))
}
