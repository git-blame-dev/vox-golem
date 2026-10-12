package dev.gitblame.voxgolem.assistant.speech

import android.annotation.TargetApi
import android.app.Activity
import android.content.ComponentName
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.os.Build

/** An intent describes navigation, not consent. Only system-supplied caller identity can opt in. */
object VoiceLaunchProvenance {
    enum class Origin { OWN_APP, DEFAULT_HOME, UNVERIFIED }
    private data class Identity(val uid: Int, val packageName: String?)

    fun initial(activity: Activity): Origin = read(activity, false)
    /** Must be called synchronously inside Activity.onNewIntent, never using the initial caller. */
    fun current(activity: Activity): Origin = read(activity, true)

    private fun read(activity: Activity, newIntent: Boolean): Origin {
        if (Build.VERSION.SDK_INT < 35) return Origin.UNVERIFIED
        val identity = try { identity(activity, newIntent) }
        catch (_: RuntimeException) { return Origin.UNVERIFIED }
        catch (_: LinkageError) { return Origin.UNVERIFIED }
        return classify(activity, identity.uid, identity.packageName)
    }

    @TargetApi(35)
    private fun identity(activity: Activity, newIntent: Boolean): Identity {
        val caller = if (newIntent) activity.currentCaller else activity.initialCaller
        return Identity(caller.uid, caller.getPackage())
    }

    @Suppress("DEPRECATION")
    internal fun classify(context: Context, uid: Int, packageName: String?): Origin {
        if (uid < 0 || packageName.isNullOrBlank()) return Origin.UNVERIFIED
        return try {
            val pm = context.packageManager
            if (pm.getApplicationInfo(packageName, 0).uid != uid) return Origin.UNVERIFIED
            if (uid == context.applicationInfo.uid) return Origin.OWN_APP
            val homeIntent = Intent(Intent.ACTION_MAIN).addCategory(Intent.CATEGORY_HOME)
            val home = pm.resolveActivity(homeIntent, PackageManager.MATCH_DEFAULT_ONLY)?.activityInfo
                ?: return Origin.UNVERIFIED
            // A chooser/resolver is not a selected HOME app. Require an actual HOME candidate.
            val component = ComponentName(home.packageName, home.name)
            val declaredHome = pm.queryIntentActivities(homeIntent, PackageManager.MATCH_DEFAULT_ONLY).any {
                it.activityInfo?.let { info -> ComponentName(info.packageName, info.name) == component } == true
            }
            if (declaredHome && home.packageName == packageName && home.applicationInfo.uid == uid)
                Origin.DEFAULT_HOME else Origin.UNVERIFIED
        } catch (_: PackageManager.NameNotFoundException) { Origin.UNVERIFIED }
        catch (_: RuntimeException) { Origin.UNVERIFIED }
    }
}
