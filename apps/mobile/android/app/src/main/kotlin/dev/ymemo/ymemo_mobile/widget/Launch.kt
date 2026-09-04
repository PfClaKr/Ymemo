package dev.ymemo.ymemo_mobile.widget

import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import dev.ymemo.ymemo_mobile.MainActivity

/**
 * Every way a widget can reach into the app.
 *
 * The action is carried in **extras**, not in the intent's action, because a collection
 * widget's rows share one `PendingIntent` template and can only differ by the fill-in
 * intent's extras — `PendingIntent.send` merges those but will not replace an action the
 * template already set. Using extras for the buttons too means `MainActivity` has one thing
 * to read instead of two.
 *
 * The launcher shortcuts in `res/xml/shortcuts.xml` are the exception: shortcut intents
 * cannot carry extras, so they arrive as their own actions and are mapped alongside.
 */
internal object Launch {
    const val ACTION_WIDGET = "dev.ymemo.action.WIDGET"
    const val EXTRA_ACTION = "dev.ymemo.extra.ACTION"
    const val EXTRA_ID = "dev.ymemo.extra.ID"

    /** The actions themselves, as Dart receives them (`lib/widgets.dart`). */
    const val NEW_MEMO = "new_memo"
    const val NEW_PHOTO_MEMO = "new_photo_memo"
    const val OPEN_LIST = "open_list"
    const val OPEN_MEMO = "open_memo"
    const val OPEN_FOLDER = "open_folder"

    /** The launcher-shortcut actions, which have no extras to carry the above. */
    const val SHORTCUT_NEW_MEMO = "dev.ymemo.action.NEW_MEMO"
    const val SHORTCUT_NEW_PHOTO_MEMO = "dev.ymemo.action.NEW_PHOTO_MEMO"

    fun intent(context: Context, action: String, id: String = ""): Intent =
        Intent(context, MainActivity::class.java).apply {
            this.action = ACTION_WIDGET
            putExtra(EXTRA_ACTION, action)
            if (id.isNotEmpty()) putExtra(EXTRA_ID, id)
            // CLEAR_TOP with the activity's singleTop launch mode hands the intent to the
            // running app through onNewIntent instead of starting a second copy of it.
            flags = Intent.FLAG_ACTIVITY_NEW_TASK or
                Intent.FLAG_ACTIVITY_CLEAR_TOP or
                Intent.FLAG_ACTIVITY_SINGLE_TOP
        }

    /**
     * A tap that goes straight into the app.
     *
     * `requestCode` has to differ per (widget, action) or Android hands out the same
     * PendingIntent twice and the second widget inherits the first one's extras.
     */
    fun pending(context: Context, widgetId: Int, action: String, id: String = ""): PendingIntent =
        PendingIntent.getActivity(
            context,
            requestCode(widgetId, action),
            intent(context, action, id),
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
        )

    /**
     * The template a collection widget's rows fill in.
     *
     * Mutable, unlike everything else here: a fill-in intent is a modification, and an
     * immutable template would silently drop every row's memo id.
     */
    fun rowTemplate(context: Context, widgetId: Int): PendingIntent =
        PendingIntent.getActivity(
            context,
            requestCode(widgetId, "row"),
            Intent(context, MainActivity::class.java).apply {
                action = ACTION_WIDGET
                flags = Intent.FLAG_ACTIVITY_NEW_TASK or
                    Intent.FLAG_ACTIVITY_CLEAR_TOP or
                    Intent.FLAG_ACTIVITY_SINGLE_TOP
            },
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_MUTABLE,
        )

    /** What one row adds to the template: nothing but which memo or folder it is. */
    fun fillIn(action: String, id: String): Intent =
        Intent().putExtra(EXTRA_ACTION, action).putExtra(EXTRA_ID, id)

    private fun requestCode(widgetId: Int, action: String): Int =
        widgetId * 31 + action.hashCode()
}
