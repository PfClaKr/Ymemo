package dev.ymemo.ymemo_mobile.widget

import android.appwidget.AppWidgetManager
import android.appwidget.AppWidgetProvider
import android.content.Context
import android.content.Intent
import android.widget.RemoteViews
import dev.ymemo.ymemo_mobile.R

/**
 * The folders and the most recent memos, with a way to start one without opening the app.
 *
 * Deliberately **not** a copy of the app's root screen. That screen shows the top level
 * only, which on a home screen would hide every memo that was ever filed in a folder; a
 * widget is a shortcut surface, so it lists the folders to go into and then every memo,
 * most recently edited first.
 */
class MemoListWidget : AppWidgetProvider() {

    override fun onUpdate(context: Context, manager: AppWidgetManager, ids: IntArray) {
        ids.forEach {
            manager.notifyAppWidgetViewDataChanged(it, R.id.list_items)
            update(context, manager, it)
        }
    }

    companion object {
        fun update(context: Context, manager: AppWidgetManager, widgetId: Int) {
            val snapshot = WidgetStore.read(context)
            val views = RemoteViews(context.packageName, R.layout.widget_list)

            views.setInt(
                R.id.list_card,
                "setColorFilter",
                context.resources.getColor(R.color.widget_surface, context.theme),
            )
            views.setTextViewText(
                R.id.list_title,
                snapshot.vaultName.ifEmpty { context.getString(R.string.widget_list_label) },
            )
            views.setTextViewText(
                R.id.list_empty,
                context.getString(
                    if (snapshot.hidden) R.string.widget_locked else R.string.widget_empty
                ),
            )

            views.setOnClickPendingIntent(
                R.id.list_header,
                Launch.pending(context, widgetId, Launch.OPEN_LIST),
            )
            views.setOnClickPendingIntent(
                R.id.list_add,
                Launch.pending(context, widgetId, Launch.NEW_MEMO),
            )
            views.setOnClickPendingIntent(
                R.id.list_photo,
                Launch.pending(context, widgetId, Launch.NEW_PHOTO_MEMO),
            )

            // The rows come from MemoListService; the widget id in the data uri is what keeps
            // two list widgets from sharing one factory (and so one scroll position).
            val rows = Intent(context, MemoListService::class.java).apply {
                putExtra(AppWidgetManager.EXTRA_APPWIDGET_ID, widgetId)
                data = android.net.Uri.parse(toUri(Intent.URI_INTENT_SCHEME))
            }
            views.setRemoteAdapter(R.id.list_items, rows)
            views.setEmptyView(R.id.list_items, R.id.list_empty)
            views.setPendingIntentTemplate(R.id.list_items, Launch.rowTemplate(context, widgetId))

            manager.updateAppWidget(widgetId, views)
        }
    }
}
