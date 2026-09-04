package dev.ymemo.ymemo_mobile.widget

import android.appwidget.AppWidgetManager
import android.content.ComponentName
import android.content.Context

/**
 * Redraws every widget on the home screen.
 *
 * All three providers declare `updatePeriodMillis="0"`: Android's own period is capped at
 * half an hour and would wake the app to redraw something that has not changed. What a
 * widget shows changes exactly when the app publishes a new snapshot, so the app says so —
 * `lib/widgets.dart` calls this through the method channel after every reload.
 */
internal object Widgets {

    fun refreshAll(context: Context) {
        val manager = AppWidgetManager.getInstance(context) ?: return
        forEach(context, manager, QuickWriteWidget::class.java) { id ->
            QuickWriteWidget.update(context, manager, id)
        }
        forEach(context, manager, NoteWidget::class.java) { id ->
            NoteWidget.update(context, manager, id)
        }
        forEach(context, manager, MemoListWidget::class.java) { id ->
            // The rows come from a RemoteViewsFactory, which only re-reads the snapshot
            // when it is told to; updating the frame alone would leave the old list in it.
            manager.notifyAppWidgetViewDataChanged(id, dev.ymemo.ymemo_mobile.R.id.list_items)
            MemoListWidget.update(context, manager, id)
        }
    }

    private fun forEach(
        context: Context,
        manager: AppWidgetManager,
        provider: Class<*>,
        block: (Int) -> Unit,
    ) {
        val ids = try {
            manager.getAppWidgetIds(ComponentName(context, provider))
        } catch (e: Exception) {
            return // no launcher that hosts widgets, or none of this kind placed
        }
        ids.forEach(block)
    }
}
