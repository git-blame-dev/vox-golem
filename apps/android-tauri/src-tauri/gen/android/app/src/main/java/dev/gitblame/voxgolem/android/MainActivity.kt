package dev.gitblame.voxgolem.android

import android.app.KeyguardManager
import android.content.Context
import android.graphics.Color
import android.os.Build
import android.os.Bundle
import android.os.PowerManager
import android.view.WindowManager
import android.webkit.WebView
import androidx.activity.enableEdgeToEdge
import androidx.activity.SystemBarStyle
import dev.gitblame.voxgolem.assistant.phone.PhoneStartupTiming
import dev.gitblame.voxgolem.assistant.phone.PhoneStartupTimingOwner
import dev.gitblame.voxgolem.assistant.phone.PhoneUiThemeOwner

class MainActivity : TauriActivity(), PhoneStartupTimingOwner, PhoneUiThemeOwner,
    dev.gitblame.voxgolem.capture.CaptureAdmissionOwner {
  override val captureAdmissionEnabled: Boolean
    get() = !dev.gitblame.voxgolem.assistant.phone.PhoneActivityBindings.isIsolated(this)
  private var activeWebView: WebView? = null
  private var lastUiTheme: String? = null
  override val startupTiming by lazy { PhoneStartupTiming(this) }

  override fun applyUiTheme(theme: String) {
    val canonical = dev.gitblame.voxgolem.assistant.phone.PhonePreferences.normalizeUiTheme(theme) ?: return
    if (isFinishing || isDestroyed || canonical == lastUiTheme) return
    val dark = canonical != "light"
    val bars = SystemBarStyle.auto(Color.TRANSPARENT, Color.TRANSPARENT) { dark }
    enableEdgeToEdge(statusBarStyle = bars, navigationBarStyle = bars)
    lastUiTheme = canonical
  }

  override fun onWebViewCreate(webView: WebView) {
    super.onWebViewCreate(webView)
    activeWebView = webView
    dev.gitblame.voxgolem.assistant.phone.PhoneActivityBindings.webViewCreated(this, webView)
    startupTiming.webViewCreated()
  }

  override fun onCreate(savedInstanceState: Bundle?) {
    dev.gitblame.voxgolem.assistant.phone.PhoneActivityBindings.beforeCreate(this)
    startupTiming.activityCreated()
    enableEdgeToEdge()
    // The isolated WebView must not restore an ordinary task's renderer state.
    super.onCreate(if (dev.gitblame.voxgolem.assistant.phone.PhoneActivityBindings.isIsolated(this)) null else savedInstanceState)
    if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O_MR1) {
      setShowWhenLocked(true)
      setTurnScreenOn(true)
    } else {
      @Suppress("DEPRECATION")
      window.addFlags(
        WindowManager.LayoutParams.FLAG_SHOW_WHEN_LOCKED or
        WindowManager.LayoutParams.FLAG_TURN_SCREEN_ON
      )
    }
  }

  override fun onDestroy() {
    startupTiming.close()
    super.onDestroy()
  }

  override fun onPause() {
    super.onPause()
    val pm = getSystemService(Context.POWER_SERVICE) as? PowerManager
    val km = getSystemService(Context.KEYGUARD_SERVICE) as? KeyguardManager
    val screenOffOrLocked = pm?.isInteractive == false || km?.isKeyguardLocked == true
    if (screenOffOrLocked) {
      activeWebView?.onResume()
    }
  }
}
