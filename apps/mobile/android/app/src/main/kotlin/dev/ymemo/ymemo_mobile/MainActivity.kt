package dev.ymemo.ymemo_mobile

import android.content.Context
import android.content.Intent
import android.net.Uri
import android.net.wifi.WifiManager
import android.view.WindowManager
import io.flutter.embedding.android.FlutterActivity
import io.flutter.embedding.engine.FlutterEngine
import io.flutter.plugin.common.MethodChannel
import java.io.File

/**
 * The four things Dart cannot do for itself.
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
 */
class MainActivity : FlutterActivity() {
    private val channelName = "dev.ymemo/native"
    private var multicastLock: WifiManager.MulticastLock? = null

    override fun configureFlutterEngine(flutterEngine: FlutterEngine) {
        super.configureFlutterEngine(flutterEngine)
        MethodChannel(flutterEngine.dartExecutor.binaryMessenger, channelName)
            .setMethodCallHandler { call, result ->
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
                    else -> result.notImplemented()
                }
            }
    }

    override fun onDestroy() {
        // A lock outliving the activity would hold the radio awake for nothing.
        releaseMulticastLock()
        super.onDestroy()
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
