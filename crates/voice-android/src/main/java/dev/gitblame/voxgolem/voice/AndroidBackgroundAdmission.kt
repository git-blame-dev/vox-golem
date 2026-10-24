package dev.gitblame.voxgolem.voice

import android.content.Context

/** Shell effect scope; ordinary applications admit their existing background owners. */
interface AndroidBackgroundAdmissionOwner {
    val backgroundServicesEnabled: Boolean
}

fun backgroundServicesAdmitted(context: Context): Boolean =
    (context.applicationContext as? AndroidBackgroundAdmissionOwner)?.backgroundServicesEnabled != false
