package dev.gitblame.voxgolem.android

import android.app.KeyguardManager
import android.content.Context
import android.os.Build
import android.os.Bundle
import android.os.PowerManager
import android.view.WindowManager
import android.webkit.WebView
import androidx.activity.enableEdgeToEdge

class MainActivity : TauriActivity() {
  private var activeWebView: WebView? = null

  override fun onWebViewCreate(webView: WebView) {
    super.onWebViewCreate(webView)
    activeWebView = webView
  }

  override fun onCreate(savedInstanceState: Bundle?) {
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
