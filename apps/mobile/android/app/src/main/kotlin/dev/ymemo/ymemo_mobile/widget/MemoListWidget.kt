package dev.ymemo.ymemo_mobile.widget

import android.app.PendingIntent
import android.appwidget.AppWidgetManager
import android.appwidget.AppWidgetProvider
import android.content.Context
import android.content.Intent
import android.widget.RemoteViews
import dev.ymemo.ymemo_mobile.R

/**
 * The folders and the most recent memos, with a way to start one without opening the app.
 *
 * Left alone it is deliberately **not** a copy of the app's root screen. That screen shows
 * the top level only, which on a home screen would hide every memo that was ever filed in a
 * folder; a widget is a shortcut surface, so it lists the folders to go into and then every
 * memo, most recently edited first.
 *
 * It can be narrowed to one folder, and given a paper color and an opacity of its own, from
 * [ListConfigureActivity] — the gear on the header, or the launcher's own "reconfigure" on
 * Android 12 and up. Those settings are **per widget** (two list widgets can show two
 * different folders) and live in `WidgetStore`, not in the vault: a home screen belongs to
 * one device, and nothing here is worth waking the other devices for.
 */
class MemoListWidget : AppWidgetProvider() {

    override fun onUpdate(context: Context, manager: AppWidgetManager, ids: IntArray) {
        ids.forEach {
            manager.notifyAppWidgetViewDataChanged(it, R.id.list_items)
            update(context, manager, it)
        }
    }

    /** Android keeps no per-widget storage of its own, so the settings are dropped here. */
    override fun onDeleted(context: Context, ids: IntArray) {
        ids.forEach { WidgetStore.forgetList(context, it) }
    }

    companion object {
        fun update(context: Context, manager: AppWidgetManager, widgetId: Int) {
            val snapshot = WidgetStore.read(context)
            val folder = snapshot.folder(snapshot.resolveFolder(WidgetStore.listFolder(context, widgetId)))
            val chrome = Chrome.of(context, WidgetStore.listColor(context, widgetId))
            val views = RemoteViews(context.packageName, R.layout.widget_list)

            views.setInt(R.id.list_card, "setColorFilter", chrome.card)
            // The card is an ImageView, so this is `ImageView.setImageAlpha` and it scales
            // the whole card — a shape drawable has no alpha channel of its own to set.
            views.setInt(R.id.list_card, "setImageAlpha", WidgetStore.listAlpha(context, widgetId) * 255 / 100)
            views.setInt(R.id.list_divider, "setBackgroundColor", chrome.divider)
            views.setInt(R.id.list_mark, "setColorFilter", chrome.icon)
            views.setInt(R.id.list_settings, "setColorFilter", chrome.icon)
            views.setInt(R.id.list_photo, "setColorFilter", chrome.icon)
            views.setInt(R.id.list_add, "setColorFilter", chrome.icon)
            views.setTextColor(R.id.list_title, chrome.ink)
            views.setTextColor(R.id.list_empty, chrome.muted)

            // A widget set to one folder says which folder it is, since that is the whole
            // reason it is not showing everything. A folder deleted elsewhere leaves the id
            // pointing at nothing, and the vault's name is the honest heading for what is
            // then on screen: the whole vault again, which is what `rows` falls back to.
            views.setTextViewText(
                R.id.list_title,
                folder?.title?.ifEmpty { context.getString(R.string.widget_list_label) }
                    ?: snapshot.vaultName.ifEmpty { context.getString(R.string.widget_list_label) },
            )
            views.setTextViewText(
                R.id.list_empty,
                context.getString(
                    if (snapshot.hidden) R.string.widget_locked else R.string.widget_empty
                ),
            )

            // The heading opens what the widget is showing: the folder if it has one.
            views.setOnClickPendingIntent(
                R.id.list_header,
                if (folder != null) {
                    Launch.pending(context, widgetId, Launch.OPEN_FOLDER, folder.id)
                } else {
                    Launch.pending(context, widgetId, Launch.OPEN_LIST)
                },
            )
            views.setOnClickPendingIntent(
                R.id.list_settings,
                reconfigure(context, widgetId),
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
            // two list widgets from sharing one factory (and so one scroll position, and now
            // one folder).
            val rows = Intent(context, MemoListService::class.java).apply {
                putExtra(AppWidgetManager.EXTRA_APPWIDGET_ID, widgetId)
                data = android.net.Uri.parse(toUri(Intent.URI_INTENT_SCHEME))
            }
            views.setRemoteAdapter(R.id.list_items, rows)
            views.setEmptyView(R.id.list_items, R.id.list_empty)
            views.setPendingIntentTemplate(R.id.list_items, Launch.rowTemplate(context, widgetId))

            manager.updateAppWidget(widgetId, views)
        }

        /**
         * Reopens the settings for a widget already on the home screen.
         *
         * The gear exists because the launcher's own "reconfigure" is Android 12 and up, and
         * a widget whose color could be chosen once and never again would be a trap.
         */
        private fun reconfigure(context: Context, widgetId: Int): PendingIntent {
            val intent = Intent(context, ListConfigureActivity::class.java).apply {
                action = AppWidgetManager.ACTION_APPWIDGET_CONFIGURE
                putExtra(AppWidgetManager.EXTRA_APPWIDGET_ID, widgetId)
                // Without a distinct data uri every widget's gear is the same PendingIntent
                // and they all reconfigure whichever one was placed first.
                data = android.net.Uri.parse("ymemo://widget/list/$widgetId")
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
