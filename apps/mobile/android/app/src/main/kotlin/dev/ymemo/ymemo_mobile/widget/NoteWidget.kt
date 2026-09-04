package dev.ymemo.ymemo_mobile.widget

import android.appwidget.AppWidgetManager
import android.appwidget.AppWidgetProvider
import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.widget.RemoteViews
import dev.ymemo.ymemo_mobile.R

/**
 * One memo, pinned to the home screen in its own paper color.
 *
 * Which memo is chosen when the widget is added ([NoteConfigureActivity]) and can be changed
 * from the widget itself. The default is not a memo at all but "whatever was edited last",
 * which is the one a note on the home screen is most often wanted for.
 *
 * The card and its title bar are `ImageView`s recolored with `setColorFilter`: RemoteViews
 * cannot hand a shape drawable a color, and a flat `setBackgroundColor` would square off the
 * rounded corners.
 */
class NoteWidget : AppWidgetProvider() {

    override fun onUpdate(context: Context, manager: AppWidgetManager, ids: IntArray) {
        ids.forEach { update(context, manager, it) }
    }

    /** Android keeps no per-widget storage of its own, so the chosen memo is dropped here. */
    override fun onDeleted(context: Context, ids: IntArray) {
        ids.forEach { WidgetStore.forgetNote(context, it) }
    }

    companion object {
        fun update(context: Context, manager: AppWidgetManager, widgetId: Int) {
            val snapshot = WidgetStore.read(context)
            val chosen = WidgetStore.noteMemo(context, widgetId)
            val memo = when {
                snapshot.hidden -> null
                chosen == WidgetStore.MOST_RECENT -> snapshot.memos.firstOrNull()
                // A memo deleted on another device leaves the widget pointing at nothing;
                // falling back to the most recent one beats an empty square forever.
                else -> snapshot.memos.firstOrNull { it.id == chosen } ?: snapshot.memos.firstOrNull()
            }

            val color = memo?.color ?: "yellow"
            val views = RemoteViews(context.packageName, R.layout.widget_note)
            views.setInt(R.id.note_paper, "setColorFilter", Palette.bg(color))
            views.setInt(R.id.note_bar, "setColorFilter", Palette.bar(color))
            views.setInt(R.id.note_edit, "setColorFilter", Palette.ink(color))
            views.setInt(R.id.note_pick, "setColorFilter", Palette.ink(color))
            views.setTextColor(R.id.note_title, Palette.ink(color))

            // With no memo to show the widget still has to say something, and the title bar
            // is too narrow for a sentence: the note keeps its own name up there and the
            // reason goes on the paper, where there is room for it.
            views.setTextViewText(
                R.id.note_title,
                when {
                    memo == null -> context.getString(R.string.widget_note_label)
                    memo.title.isNotEmpty() -> memo.title
                    else -> context.getString(R.string.widget_untitled)
                },
            )
            views.setTextViewText(
                R.id.note_body,
                when {
                    snapshot.hidden -> context.getString(R.string.widget_locked)
                    memo == null -> context.getString(R.string.widget_empty)
                    else -> memo.preview
                },
            )

            // Locked or empty, the card still opens the app: that is where both are fixed.
            val open = if (memo != null) {
                Launch.pending(context, widgetId, Launch.OPEN_MEMO, memo.id)
            } else {
                Launch.pending(context, widgetId, Launch.OPEN_LIST)
            }
            views.setOnClickPendingIntent(R.id.note_root, open)
            views.setOnClickPendingIntent(R.id.note_edit, open)
            views.setOnClickPendingIntent(R.id.note_pick, reconfigure(context, widgetId))

            manager.updateAppWidget(widgetId, views)
        }

        /** Reopens the picker for a widget that is already on the home screen. */
        private fun reconfigure(context: Context, widgetId: Int): PendingIntent {
            val intent = Intent(context, NoteConfigureActivity::class.java).apply {
                action = AppWidgetManager.ACTION_APPWIDGET_CONFIGURE
                putExtra(AppWidgetManager.EXTRA_APPWIDGET_ID, widgetId)
                // Without a distinct data uri every widget's picker is the same PendingIntent
                // and they all reconfigure whichever one was placed first.
                data = android.net.Uri.parse("ymemo://widget/$widgetId")
                flags = Intent.FLAG_ACTIVITY_NEW_TASK
            }
            return PendingIntent.getActivity(
                context,
                widgetId,
                intent,
                PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
            )
        }
    }
}
