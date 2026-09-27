package dev.ymemo.ymemo_mobile.widget

import android.appwidget.AppWidgetManager
import android.appwidget.AppWidgetProvider
import android.content.Context
import android.widget.RemoteViews
import dev.ymemo.ymemo_mobile.R

/**
 * The write bar: a home-screen row that is nothing but a way into an empty memo.
 *
 * It draws no memo text at all, so it is the one widget that has nothing to hide while the
 * vault is locked — tapping it unlocks first and lands in the new memo afterwards, which is
 * what someone who tapped a write bar wanted either way.
 */
class QuickWriteWidget : AppWidgetProvider() {

    override fun onUpdate(context: Context, manager: AppWidgetManager, ids: IntArray) {
        ids.forEach { update(context, manager, it) }
    }

    companion object {
        fun update(context: Context, manager: AppWidgetManager, widgetId: Int) {
            val views = RemoteViews(context.packageName, R.layout.widget_quick_write)
            // Set here, not left to the layout: the launcher resolves a layout's strings in the
            // system's language, and these follow the app's (see Locales).
            val text = Locales.localized(context)
            views.setTextViewText(R.id.quick_hint, text.getString(R.string.widget_quick_hint))
            views.setContentDescription(R.id.quick_photo, text.getString(R.string.widget_photo_memo))
            views.setContentDescription(R.id.quick_new, text.getString(R.string.widget_new_memo))
            views.setOnClickPendingIntent(
                R.id.quick_root,
                Launch.pending(context, widgetId, Launch.NEW_MEMO),
            )
            views.setOnClickPendingIntent(
                R.id.quick_new,
                Launch.pending(context, widgetId, Launch.NEW_MEMO),
            )
            views.setOnClickPendingIntent(
                R.id.quick_photo,
                Launch.pending(context, widgetId, Launch.NEW_PHOTO_MEMO),
            )
            manager.updateAppWidget(widgetId, views)
        }
    }
}
