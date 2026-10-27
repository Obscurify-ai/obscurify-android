// Written into dx's generated project by build.sh. The only Kotlin in the app: Back goes to the page
// first, links and payment URIs open outside the app, text can be copied, and Code Mode's sandbox.
//
// Code Mode runs the model's code in a WebView of its own, never in the app's: Android puts the
// app's JavaScript bridges (wry's `ipc` and our `android`) into every frame of the app's WebView, so
// code there could drive the app. The sandbox WebView has no bridges, loads only the server's
// /sandbox/ pages (whose CSP gives them an opaque origin and no network), refuses every other
// request at the native layer, and talks to the app over one native message port. Pages the model
// builds open full screen in a WebView made the same way.
package dev.dioxus.main

import android.app.Dialog
import android.content.ClipData
import android.content.ClipboardManager
import android.content.Intent
import android.graphics.Color
import android.net.ConnectivityManager
import android.net.Network
import android.net.Uri
import android.os.Bundle
import android.view.Gravity
import android.view.ViewGroup
import android.webkit.JavascriptInterface
import android.webkit.WebMessage
import android.webkit.WebMessagePort
import android.webkit.WebResourceRequest
import android.webkit.WebResourceResponse
import android.webkit.WebSettings
import android.webkit.WebView
import android.webkit.WebViewClient
import android.widget.Button
import android.widget.LinearLayout
import android.widget.TextView
import androidx.activity.OnBackPressedCallback
import org.json.JSONObject
import java.io.ByteArrayInputStream

typealias BuildConfig = ai.obscurify.android.BuildConfig

class MainActivity : WryActivity() {
    private var web: WebView? = null
    private var sandbox: WebView? = null
    private var sandboxPort: WebMessagePort? = null
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
            // Code Mode: (re)start the sandbox from `base`'s /sandbox/ pages; it answers {type: "ready"}.
            @JavascriptInterface fun sandboxStart(base: String) { runOnUiThread { startSandbox(base) } }
            // A JSON message for the sandbox ({type: "run" | "tool_result" | "reset", ...}).
            @JavascriptInterface fun sandboxPost(json: String) { runOnUiThread { sandboxPort?.postMessage(WebMessage(json)) } }
            // Shows a page the model built, full screen.
            @JavascriptInterface fun openPage(base: String, title: String, html: String) { runOnUiThread { showPage(base, title, html) } }
        }, "android")
    }

    /** A WebView with no bridges that loads only `base`/sandbox/ and refuses every other request. */
    private fun lockedWebView(base: String, onLoaded: (WebView) -> Unit): WebView {
        val allowed = "${base.trimEnd('/')}/sandbox/"
        val w = WebView(this)
        w.settings.apply {
            javaScriptEnabled = true
            domStorageEnabled = false
            databaseEnabled = false
            allowFileAccess = false
            allowContentAccess = false
            javaScriptCanOpenWindowsAutomatically = false
            setSupportMultipleWindows(false)
            cacheMode = WebSettings.LOAD_DEFAULT
        }
        w.webViewClient = object : WebViewClient() {
            override fun shouldOverrideUrlLoading(view: WebView, request: WebResourceRequest) =
                !request.url.toString().startsWith(allowed)
            override fun shouldInterceptRequest(view: WebView, request: WebResourceRequest): WebResourceResponse? {
                val url = request.url.toString()
                if (url.startsWith(allowed) || url.startsWith("data:") || url.startsWith("blob:")) return null
                return WebResourceResponse("text/plain", "utf-8", 403, "Blocked", emptyMap(), ByteArrayInputStream(ByteArray(0)))
            }
            override fun onPageFinished(view: WebView, url: String) {
                if (url.startsWith(allowed)) onLoaded(view)
            }
        }
        return w
    }

    private fun startSandbox(base: String) {
        sandboxPort?.close()
        sandboxPort = null
        sandbox?.let { (it.parent as? ViewGroup)?.removeView(it); it.destroy() }
        val w = lockedWebView(base) { view ->
            val ports = view.createWebMessageChannel()
            ports[0].setWebMessageCallback(object : WebMessagePort.WebMessageCallback() {
                override fun onMessage(port: WebMessagePort, message: WebMessage) {
                    val data = message.data ?: return
                    web?.evaluateJavascript("window.obsSandbox && obsSandbox(${JSONObject.quote(data)})", null)
                }
            })
            sandboxPort = ports[0]
            // The page's origin is opaque, so it can't be named; this WebView loads nothing else.
            view.postWebMessage(WebMessage("{\"type\":\"init\"}", arrayOf(ports[1])), Uri.parse("*"))
        }
        // Kept in the window (1 px, invisible) so its timers aren't paused.
        w.alpha = 0f
        addContentView(w, ViewGroup.LayoutParams(1, 1))
        sandbox = w
        w.loadUrl("${base.trimEnd('/')}/sandbox/runner.html")
    }

    private fun showPage(base: String, title: String, html: String) {
        val dialog = Dialog(this, android.R.style.Theme_Black_NoTitleBar_Fullscreen)
        val message = JSONObject().put("type", "render").put("html", html).toString()
        var renders = 0
        // A page that reloads itself is sent again, a few times at most.
        val page = lockedWebView(base) { view ->
            if (renders++ < 5) view.postWebMessage(WebMessage(message), Uri.parse("*"))
        }
        val bar = LinearLayout(this).apply {
            orientation = LinearLayout.HORIZONTAL
            gravity = Gravity.CENTER_VERTICAL
            setBackgroundColor(Color.BLACK)
            setPadding(32, 16, 16, 16)
        }
        bar.addView(TextView(this).apply {
            text = title.ifBlank { "Page" }
            setTextColor(Color.WHITE)
            textSize = 16f
            maxLines = 1
        }, LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1f))
        bar.addView(Button(this).apply {
            text = "Close"
            isAllCaps = false
            setTextColor(Color.parseColor("#00D97E"))
            setBackgroundColor(Color.TRANSPARENT)
            setOnClickListener { dialog.dismiss() }
        })
        val layout = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            setBackgroundColor(Color.BLACK)
        }
        layout.addView(bar, LinearLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT))
        layout.addView(page, LinearLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, 0, 1f))
        dialog.setContentView(layout)
        dialog.setOnDismissListener { page.destroy() }
        dialog.show()
        page.loadUrl("${base.trimEnd('/')}/sandbox/artifact.html")
    }
}
