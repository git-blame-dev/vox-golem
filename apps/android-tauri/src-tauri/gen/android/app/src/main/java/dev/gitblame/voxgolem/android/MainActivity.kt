package dev.gitblame.voxgolem.android

import android.app.KeyguardManager
import android.content.Context
import android.os.Build
import android.os.Bundle
import android.os.PowerManager
import android.view.WindowManager
import android.webkit.WebView
import androidx.activity.enableEdgeToEdge
import dev.gitblame.voxgolem.assistant.phone.PhoneStartupTiming
import dev.gitblame.voxgolem.assistant.phone.PhoneStartupTimingOwner

class MainActivity : TauriActivity(), PhoneStartupTimingOwner {
  private var activeWebView: WebView? = null
  override val startupTiming by lazy { PhoneStartupTiming(this) }

  override fun onWebViewCreate(webView: WebView) {
    super.onWebViewCreate(webView)
    activeWebView = webView
    startupTiming.webViewCreated()
  }

  override fun onCreate(savedInstanceState: Bundle?) {
    startupTiming.activityCreated()
    enableEdgeToEdge()
    super.onCreate(savedInstanceState)
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
