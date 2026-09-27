package dev.ymemo.ymemo_mobile

import android.app.Activity
import android.content.Intent
import android.net.Uri
import android.os.Build
import android.os.Bundle
import dev.ymemo.ymemo_mobile.widget.Launch
import java.io.File
import java.util.UUID

/**
 * "Share to Ymemo" from another app: turns what was shared into a new-memo request for
 * [MainActivity] and gets out of the way.
 *
 * A trampoline rather than an intent filter on MainActivity, because a share starts its target
 * **in the sharing app's task**: a second MainActivity there would be a second Flutter engine
 * over the one vault. Relaunching with the same flags the widgets use hands the request to the
 * running app instead (`onNewIntent`), or starts it once.
 *
 * A shared picture is copied into the app's private cache here, while the read grant the
 * sharing app gave *this* activity is still good; Dart attaches it and deletes the copy. The
 * bytes cannot ride along in the intent — extras are capped well below a phone photo.
 */
class ShareActivity : Activity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        val shared = intent
        if (shared?.action == Intent.ACTION_SEND) {
            val forward = Launch.intent(this, Launch.SHARE).apply {
                shared.getStringExtra(Intent.EXTRA_SUBJECT)?.let { putExtra(Launch.EXTRA_SUBJECT, it) }
                shared.getCharSequenceExtra(Intent.EXTRA_TEXT)?.let {
                    putExtra(Launch.EXTRA_TEXT, it.toString())
                }
                stream(shared)?.let { uri ->
                    copyToCache(uri)?.let { file ->
                        putExtra(Launch.EXTRA_FILE, file.absolutePath)
                        putExtra(Launch.EXTRA_MIME, contentResolver.getType(uri) ?: shared.type ?: "")
                    }
                }
            }
            startActivity(forward)
        }
        finish()
    }

    private fun stream(intent: Intent): Uri? =
        if (Build.VERSION.SDK_INT >= 33) {
            intent.getParcelableExtra(Intent.EXTRA_STREAM, Uri::class.java)
        } else {
            @Suppress("DEPRECATION")
            intent.getParcelableExtra(Intent.EXTRA_STREAM)
        }

    /** The shared file in `cache/shared/`, or null if it could not be read. */
    private fun copyToCache(uri: Uri): File? = runCatching {
        val dir = File(cacheDir, "shared").apply { mkdirs() }
        val file = File(dir, UUID.randomUUID().toString())
        contentResolver.openInputStream(uri)?.use { input ->
            file.outputStream().use { input.copyTo(it) }
        } ?: return null
        file
    }.getOrNull()
}
