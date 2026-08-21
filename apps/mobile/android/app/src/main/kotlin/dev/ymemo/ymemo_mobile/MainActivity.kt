package dev.ymemo.ymemo_mobile

import android.content.Context
import android.net.wifi.WifiManager
import io.flutter.embedding.android.FlutterActivity
import io.flutter.embedding.engine.FlutterEngine
import io.flutter.plugin.common.MethodChannel
import java.io.File

/**
 * The two things Dart cannot do for itself.
 *
 * **Where the sync daemon's executable is.** Since Android 10 an app may only execute a binary
 * from its native library directory, so syncthing ships as `libsyncthing.so` in `jniLibs/` and
 * runs from wherever the installer unpacked it. `applicationInfo.nativeLibraryDir` is
 * per-install and per-ABI and no plugin exposes it. `null` means this build has no daemon, and
 * Dart then runs local-only rather than failing.
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
