package dev.ymemo.ymemo_mobile

import android.content.Context
import android.content.Intent
import android.net.Uri
import android.net.wifi.WifiManager
import android.os.Bundle
import android.view.WindowManager
import dev.ymemo.ymemo_mobile.widget.Launch
import dev.ymemo.ymemo_mobile.widget.WidgetStore
import dev.ymemo.ymemo_mobile.widget.Widgets
import io.flutter.embedding.android.FlutterActivity
import io.flutter.embedding.engine.FlutterEngine
import io.flutter.plugin.common.MethodChannel
import java.io.File

/**
 * The things Dart cannot do for itself.
 *
 * **Where the sync daemon's executable is.** Since Android 10 an app may only execute a binary
 * from its native library directory, so syncthing ships as `libsyncthing.so` in `jniLibs/` and
 * runs from wherever the installer unpacked it. `applicationInfo.nativeLibraryDir` is
 * per-install and per-ABI and no plugin exposes it. `null` means this build has no daemon, and
 * Dart then runs local-only rather than failing.
 *
 * **Hiding the window from the app switcher** (`FLAG_SECURE`). Android screenshots an app as
 * it leaves, and that thumbnail would show whatever memo was open. The flag follows the
 * "lock when the app is left" setting: someone who turned that off has chosen convenience,
 * and hiding their thumbnail anyway would be deciding for them.
 *
 * **Opening a link.** The update notice points at the release page, which needs an intent.
 *
 * **Hearing the LAN pairing broadcast.** The wifi stack drops packets that are not addressed
 * to this device unless a multicast lock is held, which would silently cost us every pairing
 * request from the other device. The lock is held only while the pairing screen is open — it
 * keeps the wifi radio from sleeping, so it is not something to leave on.
 *
 * **The home-screen widgets.** They are drawn by the launcher while the app is closed, so
 * they read a snapshot the app publishes rather than the vault (see `widget/Snapshot.kt`);
 * `widgetPublish` is Dart handing over a new one. In the other direction a tapped widget
 * starts this activity with an action in its extras, which Dart collects with
 * `takeWidgetAction` at startup and is handed through `widgetAction` while it is running.
 */
class MainActivity : FlutterActivity() {
    private val channelName = "dev.ymemo/native"
    private var multicastLock: WifiManager.MulticastLock? = null
    private var channel: MethodChannel? = null

    /**
     * What a widget or launcher shortcut asked for, until Dart comes to collect it.
     *
     * Read off the launch intent here rather than pushed at Dart, because at this point the
     * Dart side has not run a line: `takeWidgetAction` is the first thing it asks for.
     */
    private var pendingAction: Map<String, String>? = null

    override fun configureFlutterEngine(flutterEngine: FlutterEngine) {
        super.configureFlutterEngine(flutterEngine)
        channel = MethodChannel(flutterEngine.dartExecutor.binaryMessenger, channelName).also {
            it.setMethodCallHandler { call, result ->
                when (call.method) {
                    "syncBinaryPath" -> result.success(syncBinaryPath())
                    "openUrl" -> result.success(openUrl(call.arguments as? String))
                    "setSecure" -> {
                        setSecure(call.arguments as? Boolean ?: false)
                        result.success(null)
                    }
                    "acquireMulticastLock" -> result.success(acquireMulticastLock())
                    "releaseMulticastLock" -> {
                        releaseMulticastLock()
                        result.success(null)
                    }
                    "widgetPublish" -> {
                        publishToWidgets(call.arguments as? String)
                        result.success(null)
                    }
                    "takeWidgetAction" -> {
                        result.success(pendingAction)
                        pendingAction = null
                    }
                    else -> result.notImplemented()
                }
            }
        }
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        pendingAction = readAction(intent)
    }

    /**
     * A widget tapped while the app is already open. `singleTop` means this rather than a
     * second copy of the activity, and Dart is told at once — nothing will ask for it again.
     */
    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        setIntent(intent)
        val action = readAction(intent) ?: return
        val sink = channel
        if (sink == null) {
            pendingAction = action
        } else {
            sink.invokeMethod("widgetAction", action)
        }
    }

    override fun onDestroy() {
        // A lock outliving the activity would hold the radio awake for nothing.
        releaseMulticastLock()
        channel = null
        super.onDestroy()
    }

    /**
     * The action carried by a widget tap or a launcher shortcut, as `{action, id}`.
     *
     * Widgets put it in the extras: a collection widget's rows share one PendingIntent
     * template and can differ only by the extras of their fill-in intent. Shortcuts cannot
     * carry extras at all, so those arrive as actions of their own and are mapped here.
     *
     * Consumed as it is read. Android hands the same intent back when it recreates the
     * activity after the process was killed, and "new memo" happening twice would leave an
     * empty memo behind every time.
     */
    private fun readAction(intent: Intent?): Map<String, String>? {
        if (intent == null) return null
        val name = intent.getStringExtra(Launch.EXTRA_ACTION)
            ?: when (intent.action) {
                Launch.SHORTCUT_NEW_MEMO -> Launch.NEW_MEMO
                Launch.SHORTCUT_NEW_PHOTO_MEMO -> Launch.NEW_PHOTO_MEMO
                else -> null
            }
            ?: return null
        val id = intent.getStringExtra(Launch.EXTRA_ID) ?: ""
        intent.removeExtra(Launch.EXTRA_ACTION)
        intent.removeExtra(Launch.EXTRA_ID)
        intent.action = Intent.ACTION_MAIN
        return mapOf("action" to name, "id" to id)
    }

    /** Stores the snapshot Dart just built and redraws whatever is on the home screen. */
    private fun publishToWidgets(json: String?) {
        if (json == null) return
        WidgetStore.publish(applicationContext, json)
        Widgets.refreshAll(applicationContext)
    }

    private fun syncBinaryPath(): String? {
        val file = File(applicationInfo.nativeLibraryDir, "libsyncthing.so")
        // Packaged with useLegacyPackaging, so it is a real file on disk; a build without the
        // daemon simply has nothing here.
        return if (file.canExecute()) file.absolutePath else null
    }

    /** Turns the screenshot/app-switcher block on or off. Must run on the UI thread. */
    private fun setSecure(secure: Boolean) {
        runOnUiThread {
            if (secure) {
                window.addFlags(WindowManager.LayoutParams.FLAG_SECURE)
            } else {
                window.clearFlags(WindowManager.LayoutParams.FLAG_SECURE)
            }
        }
    }

    /** Hands a URL to the browser. False when there is nothing on the device to open it. */
    private fun openUrl(url: String?): Boolean {
        if (url.isNullOrEmpty()) return false
        return try {
            startActivity(Intent(Intent.ACTION_VIEW, Uri.parse(url)))
            true
        } catch (e: Exception) {
            false
        }
    }

    /** Returns whether the lock is held; pairing is worth attempting either way. */
    private fun acquireMulticastLock(): Boolean {
        if (multicastLock?.isHeld == true) return true
        return try {
            val wifi = applicationContext.getSystemService(Context.WIFI_SERVICE) as WifiManager
            val lock = wifi.createMulticastLock("ymemo-pairing")
            lock.setReferenceCounted(false)
            lock.acquire()
            multicastLock = lock
            true
        } catch (e: Exception) {
            // No wifi service, or the permission was refused: broadcasts may still arrive.
            false
        }
    }

    private fun releaseMulticastLock() {
        multicastLock?.let { if (it.isHeld) it.release() }
        multicastLock = null
    }
}
