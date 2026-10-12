package dev.gitblame.voxgolem.android.wear

import java.math.BigDecimal
import java.math.RoundingMode

internal fun formatVisibleDuration(milliseconds: Long): String {
    require(milliseconds >= 0)
    if (milliseconds < 1000) return "$milliseconds ms"
    val seconds = BigDecimal.valueOf(milliseconds, 3).setScale(2, RoundingMode.HALF_UP)
        .stripTrailingZeros().toPlainString()
    return "$seconds s"
}
