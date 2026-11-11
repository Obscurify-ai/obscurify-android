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

import android.Manifest
import android.app.Dialog
import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import android.content.Intent
import android.content.pm.ServiceInfo
import android.graphics.Color
import android.net.ConnectivityManager
import android.net.Network
import android.net.Uri
import android.os.Build
import android.os.Bundle
import android.os.IBinder
import android.os.PowerManager
import android.view.Gravity
import android.view.ViewGroup
import android.view.WindowManager
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

private const val CHANNEL_WORK = "work"
private const val CHANNEL_DONE = "done"
private const val ACTION_STOP = "stop"

private fun channels(c: Context) {
    if (Build.VERSION.SDK_INT < 26) return
    val nm = c.getSystemService(NotificationManager::class.java)
    nm.createNotificationChannel(NotificationChannel(CHANNEL_WORK, "Working on an answer", NotificationManager.IMPORTANCE_LOW))
    nm.createNotificationChannel(NotificationChannel(CHANNEL_DONE, "Answer ready", NotificationManager.IMPORTANCE_DEFAULT))
}

private fun note(c: Context, channel: String, title: String, text: String, ongoing: Boolean): Notification {
    val open = PendingIntent.getActivity(c, 0, Intent(c, MainActivity::class.java).addFlags(Intent.FLAG_ACTIVITY_SINGLE_TOP), PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT)
    @Suppress("DEPRECATION")
    val b = if (Build.VERSION.SDK_INT >= 26) Notification.Builder(c, channel) else Notification.Builder(c)
    return b.setSmallIcon(ai.obscurify.android.R.drawable.ic_work).setContentTitle(title).setContentText(text)
        .setContentIntent(open).setOngoing(ongoing).setAutoCancel(!ongoing).build()
}

/** Keeps the app alive while an answer is being worked on, so it can run for a long time with the phone in a pocket. */
class WorkService : Service() {
    private var lock: PowerManager.WakeLock? = null

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        channels(this)
        val n = note(this, CHANNEL_WORK, "Obscurify is working", "Your answer will be here when it's ready.", true)
        if (Build.VERSION.SDK_INT >= 29) startForeground(1, n, ServiceInfo.FOREGROUND_SERVICE_TYPE_DATA_SYNC) else startForeground(1, n)
        // Stopping goes through here too: Android kills an app whose service stops before it has
        // called startForeground, which a short answer would otherwise do.
        if (intent?.action == ACTION_STOP) {
            @Suppress("DEPRECATION")
            stopForeground(true)
            stopSelf(startId)
            return START_NOT_STICKY
        }
        if (lock == null) {
            lock = getSystemService(PowerManager::class.java).newWakeLock(PowerManager.PARTIAL_WAKE_LOCK, "obscurify:work").apply { acquire(2 * 60 * 60 * 1000L) }
        }
        return START_NOT_STICKY
    }

    override fun onDestroy() {
        lock?.let { if (it.isHeld) it.release() }
        lock = null
        super.onDestroy()
    }
}

class MainActivity : WryActivity() {
    private var web: WebView? = null
    private var sandbox: WebView? = null
    private var sandboxPort: WebMessagePort? = null
    private var onScreen = false
    private var working = false
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

    override fun onResume() { super.onResume(); onScreen = true }
    override fun onPause() { super.onPause(); onScreen = false }

    /** On when an answer starts; off when it ends, saying so if the app isn't on screen. */
    private fun setWorking(on: Boolean) {
        val service = Intent(this, WorkService::class.java)
        if (on && !working) {
            working = true
            if (Build.VERSION.SDK_INT >= 33 && checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS) != android.content.pm.PackageManager.PERMISSION_GRANTED) {
                requestPermissions(arrayOf(Manifest.permission.POST_NOTIFICATIONS), 7)
            }
            try { if (Build.VERSION.SDK_INT >= 26) startForegroundService(service) else startService(service) } catch (_: Exception) {}
        } else if (!on && working) {
            working = false
            try { startService(service.setAction(ACTION_STOP)) } catch (_: Exception) { stopService(service) }
            if (!onScreen) {
                channels(this)
                try { getSystemService(NotificationManager::class.java).notify(2, note(this, CHANNEL_DONE, "Your answer is ready", "Tap to read it.", false)) } catch (_: Exception) {}
            }
        }
    }

    override fun onWebViewCreate(webView: WebView) {
        web = webView
        webView.addJavascriptInterface(object {
            // https links, and bitcoin:/lightning:/monero: payment URIs for a wallet app.
            @JavascriptInterface fun open(url: String) {
                try { startActivity(Intent(Intent.ACTION_VIEW, Uri.parse(url))) } catch (_: Exception) {}
            }
            // A long run shouldn't stop because the screen timed out.
            @JavascriptInterface fun keepAwake(on: Boolean) {
                runOnUiThread { if (on) window.addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON) else window.clearFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON) }
            }
            // A long answer keeps going with the app in the background, and says when it's ready.
            @JavascriptInterface fun working(on: Boolean) { runOnUiThread { setWorking(on) } }
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
                // Code has the internet from the phone (as on the web since 2026-11-11): https loads
                // pass; navigating away still doesn't (shouldOverrideUrlLoading above).
                if (url.startsWith(allowed) || url.startsWith("data:") || url.startsWith("blob:") || url.startsWith("https://")) return null
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
