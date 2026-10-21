// Written into dx's generated project by build.sh. The only Kotlin in the app: Back goes to the page
// first, links and payment URIs open outside the app, and text can be copied.
package dev.dioxus.main

import android.content.ClipData
import android.content.ClipboardManager
import android.content.Intent
import android.net.ConnectivityManager
import android.net.Network
import android.net.Uri
import android.os.Bundle
import android.webkit.JavascriptInterface
import android.webkit.WebView
import androidx.activity.OnBackPressedCallback

typealias BuildConfig = ai.obscurify.android.BuildConfig

class MainActivity : WryActivity() {
    private var web: WebView? = null
    // Back is the page's call: it closes the open screen, and only on the chat leaves the app.
    override val handleBackNavigation = false

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        getSystemService(ConnectivityManager::class.java).registerDefaultNetworkCallback(object : ConnectivityManager.NetworkCallback() {
            override fun onAvailable(network: Network) { web?.post { web?.evaluateJavascript("window.obsOnline && obsOnline()", null) } }
        })
        onBackPressedDispatcher.addCallback(this, object : OnBackPressedCallback(true) {
            override fun handleOnBackPressed() {
                val w = web ?: return finish()
                w.evaluateJavascript("window.obsBack ? obsBack() : false") { handled -> if (handled != "true") finish() }
            }
        })
    }

    override fun onWebViewCreate(webView: WebView) {
        web = webView
        webView.addJavascriptInterface(object {
            // https links, and bitcoin:/lightning:/monero: payment URIs for a wallet app.
            @JavascriptInterface fun open(url: String) {
                try { startActivity(Intent(Intent.ACTION_VIEW, Uri.parse(url))) } catch (_: Exception) {}
            }
            @JavascriptInterface fun copy(text: String) {
                getSystemService(ClipboardManager::class.java).setPrimaryClip(ClipData.newPlainText("Obscurify", text))
            }
        }, "android")
    }
}
