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
    /**
     * The folder this sits in — a folder's parent, a memo's folder — and `""` for the top
     * level. Published for one reason: a list widget can be pointed at a single folder.
     */
    val parent: String,
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
    /**
     * The rows one list widget draws: folders first, then memos, the order the app's own
     * list uses.
     *
     * [WidgetStore.EVERYTHING] is what a widget nobody has configured shows, and it is not
     * the same as picking the top-level folder: it is the **top-level folders plus every
     * memo**, wherever it is filed, which is what this widget has always drawn and what a
     * shortcut surface wants. Naming a folder narrows it to that folder's own subfolders and
     * its own memos, which is what the app's folder screen shows.
     */
    fun rows(folder: String, picks: Set<String> = emptySet()): List<Pair<Boolean, Entry>> {
        if (folder == WidgetStore.PICKED) {
            // Just the chosen memos, in the order the snapshot lists them (most recently
            // edited first). No folders: someone who named the memos is not browsing.
            return memos.filter { picks.contains(it.id) }.map { false to it }
        }
        if (folder == WidgetStore.EVERYTHING) {
            return folders.filter { it.parent.isEmpty() }.map { true to it } +
                memos.map { false to it }
        }
        return folders.filter { it.parent == folder }.map { true to it } +
            memos.filter { it.parent == folder }.map { false to it }
    }

    /**
     * What a widget set to `id` is actually showing.
     *
     * A folder deleted on another device leaves a widget pointing at nothing, and an empty
     * square forever is the worst answer to that; it falls back to the whole vault, which is
     * also what the widget looked like before anyone configured it.
     */
    fun resolveFolder(id: String, picks: Set<String> = emptySet()): String = when {
        id == WidgetStore.EVERYTHING -> id
        // A pick list whose memos have all been deleted would leave a square that can never
        // show anything again; the whole vault is the same answer a lost folder gets.
        id == WidgetStore.PICKED -> if (memos.any { picks.contains(it.id) }) id else WidgetStore.EVERYTHING
        folders.any { it.id == id } -> id
        else -> WidgetStore.EVERYTHING
    }

    /** The chosen folder itself, or null for [WidgetStore.EVERYTHING] and a deleted one. */
    fun folder(id: String): Entry? =
        if (id == WidgetStore.EVERYTHING) null else folders.firstOrNull { it.id == id }

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
    private const val KEY_LIST_FOLDER_PREFIX = "list_folder_"
    private const val KEY_LIST_PICKS_PREFIX = "list_picks_"
    private const val KEY_LIST_COLOR_PREFIX = "list_color_"
    private const val KEY_LIST_ALPHA_PREFIX = "list_alpha_"

    /** The memo id a sticky widget follows the most recent memo under. */
    const val MOST_RECENT = ""

    /**
     * A list widget showing the whole vault rather than one folder.
     *
     * `"*"` and not `""`, because `""` is a real answer here — it is the top level, and a
     * widget set to it should show the top level's own memos and nothing else. This is the
     * default, so a widget placed before there was anything to configure keeps drawing what
     * it always drew.
     */
    const val EVERYTHING = "*"

    /**
     * A list widget showing named memos rather than a folder.
     *
     * Which ones is [`listPicks`], kept apart from this because a folder and a pick list are
     * different questions — someone who switches to a folder and back should find their picks
     * where they left them.
     */
    const val PICKED = "+"

    /** A list widget drawn in the launcher's own light/dark chrome rather than a paper color. */
    const val THEME_COLOR = ""

    /** Below this a widget is a smudge on the wallpaper rather than a widget. */
    const val MIN_ALPHA = 20

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
                    // Absent in a snapshot written before folders could be picked; the top
                    // level is the reading that keeps such a widget showing what it showed.
                    parent = o.optString("parent"),
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

    // ---- One list widget's settings: which folder, what color, how solid. ----

    fun listFolder(context: Context, widgetId: Int): String =
        prefs(context).getString(KEY_LIST_FOLDER_PREFIX + widgetId, EVERYTHING) ?: EVERYTHING

    fun listColor(context: Context, widgetId: Int): String =
        prefs(context).getString(KEY_LIST_COLOR_PREFIX + widgetId, THEME_COLOR) ?: THEME_COLOR

    /** Percent, [MIN_ALPHA]..100. */
    fun listAlpha(context: Context, widgetId: Int): Int =
        prefs(context).getInt(KEY_LIST_ALPHA_PREFIX + widgetId, 100).coerceIn(MIN_ALPHA, 100)

    /** The memos a [PICKED] widget shows; empty for every other kind. */
    fun listPicks(context: Context, widgetId: Int): Set<String> =
        prefs(context).getStringSet(KEY_LIST_PICKS_PREFIX + widgetId, emptySet()) ?: emptySet()

    fun setList(
        context: Context,
        widgetId: Int,
        folder: String,
        picks: Set<String>,
        color: String,
        alpha: Int,
    ) {
        prefs(context).edit()
            .putStringSet(KEY_LIST_PICKS_PREFIX + widgetId, picks)
            .putString(KEY_LIST_FOLDER_PREFIX + widgetId, folder)
            .putString(KEY_LIST_COLOR_PREFIX + widgetId, color)
            .putInt(KEY_LIST_ALPHA_PREFIX + widgetId, alpha.coerceIn(MIN_ALPHA, 100))
            .apply()
    }

    fun forgetList(context: Context, widgetId: Int) {
        prefs(context).edit()
            .remove(KEY_LIST_PICKS_PREFIX + widgetId)
            .remove(KEY_LIST_FOLDER_PREFIX + widgetId)
            .remove(KEY_LIST_COLOR_PREFIX + widgetId)
            .remove(KEY_LIST_ALPHA_PREFIX + widgetId)
            .apply()
    }
}
