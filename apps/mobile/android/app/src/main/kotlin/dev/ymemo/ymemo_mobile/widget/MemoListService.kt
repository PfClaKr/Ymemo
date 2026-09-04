package dev.ymemo.ymemo_mobile.widget

import android.content.Context
import android.content.Intent
import android.widget.RemoteViews
import android.widget.RemoteViewsService
import dev.ymemo.ymemo_mobile.R

/** Supplies the rows of [MemoListWidget]; the launcher binds to it to scroll the list. */
class MemoListService : RemoteViewsService() {
    override fun onGetViewFactory(intent: Intent): RemoteViewsFactory =
        MemoListFactory(applicationContext)
}

private class MemoListFactory(private val context: Context) : RemoteViewsService.RemoteViewsFactory {

    /** Read once per `notifyAppWidgetViewDataChanged`, so a row cannot change mid-scroll. */
    private var rows: List<Pair<Boolean, Entry>> = emptyList()

    override fun onCreate() = onDataSetChanged()

    override fun onDataSetChanged() {
        val snapshot = WidgetStore.read(context)
        rows = if (snapshot.hidden) emptyList() else snapshot.rows
    }

    override fun onDestroy() {
        rows = emptyList()
    }

    override fun getCount() = rows.size

    override fun getViewAt(position: Int): RemoteViews {
        val (isFolder, entry) = rows[position]
        val views = RemoteViews(context.packageName, R.layout.widget_list_item)

        views.setInt(R.id.row_stripe, "setColorFilter", Palette.swatch(entry.color))
        views.setInt(R.id.row_icon, "setColorFilter", Palette.ink(entry.color))
        views.setImageViewResource(
            R.id.row_icon,
            if (isFolder) R.drawable.ic_widget_folder else R.drawable.ic_widget_note,
        )
        views.setTextViewText(
            R.id.row_title,
            entry.title.ifEmpty { context.getString(R.string.widget_untitled) },
        )
        views.setTextViewText(R.id.row_body, entry.preview)
        views.setViewVisibility(
            R.id.row_body,
            if (entry.preview.isEmpty()) android.view.View.GONE else android.view.View.VISIBLE,
        )

        views.setOnClickFillInIntent(
            R.id.row_root,
            Launch.fillIn(if (isFolder) Launch.OPEN_FOLDER else Launch.OPEN_MEMO, entry.id),
        )
        return views
    }

    /** Nothing to show while a row is being fetched: the snapshot is already in memory. */
    override fun getLoadingView(): RemoteViews? = null

    override fun getViewTypeCount() = 1

    override fun getItemId(position: Int) = position.toLong()

    override fun hasStableIds() = false
}
