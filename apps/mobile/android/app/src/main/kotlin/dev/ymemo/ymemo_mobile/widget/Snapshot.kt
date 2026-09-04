package dev.ymemo.ymemo_mobile.widget

import android.content.Context
import android.util.Log
import org.json.JSONObject

/**
 * What the widgets draw, and why it is a copy rather than the real thing.
 *
 * A widget is drawn by the launcher at moments the app has no say in — while it is closed,
 * while it is locked, seconds after a reboot. It cannot open the vault (that needs the
 * master key) and it must not read `ymemo.db` either: `Vault::rebuild()` clears that cache
 * and re-materializes it from the logs on every merge, so a widget reading it mid-rebuild
 * would show an empty list. Instead the app **publishes** this snapshot whenever the memos
 * on screen change, and the widgets only ever read the last one published.
 *
 * It is therefore a second plaintext copy of some memo text on the device, next to the
 * plaintext cache SECURITY.md already describes — bodies truncated to a preview, in the
 * app's private directory, never synced and never backed up (`allowBackup="false"`). It is
 * emptied whenever the vault is closed, so a locked app leaves nothing on the home screen.
 */
internal data class Entry(
    val id: String,
    val title: String,
    val preview: String,
    val color: String,
)

internal data class Snapshot(
    /** The vault's name, as the list widget's heading. Empty until it is named. */
    val vaultName: String,
    /** True once the vault has been closed: there is nothing to draw, only "tap to unlock". */
    val hidden: Boolean,
    val folders: List<Entry>,
    /** Every memo, most recently edited first. */
    val memos: List<Entry>,
) {
    /** Folders first, then memos — the order the app's own list draws them in. */
    val rows: List<Pair<Boolean, Entry>>
        get() = folders.map { true to it } + memos.map { false to it }

    companion object {
        /** What a device that has never unlocked the app shows. */
        val EMPTY = Snapshot(vaultName = "", hidden = true, folders = emptyList(), memos = emptyList())
    }
}

/**
 * Where the snapshot and each sticky widget's chosen memo are kept.
 *
 * SharedPreferences rather than a file: the app writes it over the method channel, so the
 * path never has to be agreed on twice, and the widgets read it without any of Flutter
 * having started.
 */
internal object WidgetStore {
    private const val PREFS = "dev.ymemo.widget"
    private const val KEY_SNAPSHOT = "snapshot"
    private const val KEY_NOTE_PREFIX = "note_memo_"

    /** The memo id a sticky widget follows the most recent memo under. */
    const val MOST_RECENT = ""

    private fun prefs(context: Context) =
        context.getSharedPreferences(PREFS, Context.MODE_PRIVATE)

    /** Called from Dart through `MainActivity`; the JSON shape is `lib/widgets.dart`. */
    fun publish(context: Context, json: String) {
        prefs(context).edit().putString(KEY_SNAPSHOT, json).apply()
    }

    fun read(context: Context): Snapshot {
        val json = prefs(context).getString(KEY_SNAPSHOT, null) ?: return Snapshot.EMPTY
        return try {
            parse(JSONObject(json))
        } catch (e: Exception) {
            // A snapshot written by a newer version, or a half-written one. Showing the
            // locked face is wrong but harmless; showing a crashed widget is not.
            Log.w("YmemoWidget", "unreadable snapshot: ${e.message}")
            Snapshot.EMPTY
        }
    }

    private fun parse(root: JSONObject): Snapshot {
        fun entries(name: String): List<Entry> {
            val array = root.optJSONArray(name) ?: return emptyList()
            return (0 until array.length()).map { i ->
                val o = array.getJSONObject(i)
                Entry(
                    id = o.optString("id"),
                    title = o.optString("title"),
                    preview = o.optString("preview"),
                    color = o.optString("color", "yellow"),
                )
            }
        }
        return Snapshot(
            vaultName = root.optString("vault"),
            hidden = root.optBoolean("hidden", true),
            folders = entries("folders"),
            memos = entries("memos"),
        )
    }

    fun noteMemo(context: Context, widgetId: Int): String =
        prefs(context).getString(KEY_NOTE_PREFIX + widgetId, MOST_RECENT) ?: MOST_RECENT

    fun setNoteMemo(context: Context, widgetId: Int, memoId: String) {
        prefs(context).edit().putString(KEY_NOTE_PREFIX + widgetId, memoId).apply()
    }

    /** Android does not clean these up when a widget is removed, so the provider does. */
    fun forgetNote(context: Context, widgetId: Int) {
        prefs(context).edit().remove(KEY_NOTE_PREFIX + widgetId).apply()
    }
}
