package dev.gitblame.voxgolem.assistant.speech

import android.app.Activity
import android.content.Intent
import android.content.ContextWrapper
import android.content.pm.ActivityInfo
import android.content.pm.ApplicationInfo
import android.content.pm.PackageInfo
import android.content.pm.ResolveInfo
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.Robolectric
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment
import org.robolectric.Shadows.shadowOf
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35], manifest = Config.NONE)
class VoiceLaunchProvenanceTest {
    @Suppress("DEPRECATION")
    @Test fun onlyActualOwnUidOrTheResolvedHomeUidAndPackageCanGrantFastEntry() {
        val application = RuntimeEnvironment.getApplication()
        val ownApp = ApplicationInfo().apply { packageName = "synthetic.own"; uid = 22000 }
        val context = object : ContextWrapper(application) {
            override fun getPackageName() = ownApp.packageName
            override fun getApplicationInfo() = ownApp
        }
        val pm = context.packageManager
        val homeApp = ApplicationInfo().apply { packageName = "synthetic.home"; uid = 22001 }
        val foreign = ApplicationInfo().apply { packageName = "synthetic.foreign"; uid = 22002 }
        for (app in listOf(ownApp, homeApp, foreign)) shadowOf(pm).installPackage(PackageInfo().apply {
            packageName = app.packageName; applicationInfo = app
        })
        assertTrue("Own caller UID must be disclosed and valid", context.applicationInfo.uid >= 0)
        assertEquals("Own context package must name the installed fixture", ownApp.packageName, context.packageName)
        assertEquals("Own context and PackageManager must agree on UID", context.applicationInfo.uid,
            pm.getApplicationInfo(context.packageName, 0).uid)
        val home = Intent(Intent.ACTION_MAIN).addCategory(Intent.CATEGORY_HOME)
        shadowOf(pm).addResolveInfoForIntent(home, ResolveInfo().apply {
            isDefault = true
            activityInfo = ActivityInfo().apply { packageName = homeApp.packageName; name = "synthetic.home.HomeActivity"; applicationInfo = homeApp }
        })
        assertEquals(VoiceLaunchProvenance.Origin.OWN_APP,
            VoiceLaunchProvenance.classify(context, context.applicationInfo.uid, context.packageName))
        assertEquals(VoiceLaunchProvenance.Origin.DEFAULT_HOME, VoiceLaunchProvenance.classify(context, homeApp.uid, homeApp.packageName))
        assertEquals(VoiceLaunchProvenance.Origin.UNVERIFIED, VoiceLaunchProvenance.classify(context, foreign.uid, foreign.packageName))
        assertEquals(VoiceLaunchProvenance.Origin.UNVERIFIED, VoiceLaunchProvenance.classify(context, foreign.uid, homeApp.packageName))
        assertEquals(VoiceLaunchProvenance.Origin.UNVERIFIED, VoiceLaunchProvenance.classify(context, foreign.uid, ownApp.packageName))
        assertEquals(VoiceLaunchProvenance.Origin.UNVERIFIED, VoiceLaunchProvenance.classify(context, -1, homeApp.packageName))
        assertEquals(VoiceLaunchProvenance.Origin.UNVERIFIED, VoiceLaunchProvenance.classify(context, homeApp.uid, null))
        assertEquals(VoiceLaunchProvenance.Origin.UNVERIFIED, VoiceLaunchProvenance.classify(context, homeApp.uid, "synthetic.missing"))
    }

    @Test @Config(sdk = [34]) fun olderAndroidCannotUpgradeIntentExtrasOrReferrersIntoCallerConsent() {
        val activity = Robolectric.buildActivity(Activity::class.java, Intent(Intent.ACTION_MAIN)
            .addCategory(Intent.CATEGORY_LAUNCHER).putExtra(Intent.EXTRA_REFERRER_NAME, "android-app://synthetic.home")
            .putExtra("trusted", true)).create().get()
        assertEquals(VoiceLaunchProvenance.Origin.UNVERIFIED, VoiceLaunchProvenance.initial(activity))
        assertEquals(VoiceLaunchProvenance.Origin.UNVERIFIED, VoiceLaunchProvenance.current(activity))
    }

    @Test fun newIntentLookupCannotFallBackToAnInitialCallerOutsideTheCallback() {
        val activity = Robolectric.buildActivity(Activity::class.java).create().get()
        assertEquals(VoiceLaunchProvenance.Origin.UNVERIFIED, VoiceLaunchProvenance.current(activity))
    }
}
