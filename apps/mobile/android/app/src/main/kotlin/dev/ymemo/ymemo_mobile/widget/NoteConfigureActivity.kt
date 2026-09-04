package dev.ymemo.ymemo_mobile.widget

import android.app.Activity
import android.appwidget.AppWidgetManager
import android.content.Intent
import android.os.Bundle
import android.widget.ListView
import android.widget.SimpleAdapter
import android.widget.TextView
import dev.ymemo.ymemo_mobile.R

/**
 * Asks which memo a sticky widget should show, both when it is added and when its "..."
 * button is tapped later.
 *
 * It lists the published snapshot, not the vault: this screen can be opened from the widget
 * picker while the app has never been unlocked on this device, and it has no key. When there
 * is nothing to list it says so rather than offering an empty list — the answer is to open
 * the app once, and that is what the text says.
 */
class NoteConfigureActivity : Activity() {

    private var widgetId = AppWidgetManager.INVALID_APPWIDGET_ID

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)

        widgetId = intent?.extras?.getInt(
            AppWidgetManager.EXTRA_APPWIDGET_ID,
            AppWidgetManager.INVALID_APPWIDGET_ID,
        ) ?: AppWidgetManager.INVALID_APPWIDGET_ID
        // Backing out has to leave no widget behind, so the cancelled result is set first and
        // only replaced once a memo has actually been chosen.
        setResult(RESULT_CANCELED, Intent().putExtra(AppWidgetManager.EXTRA_APPWIDGET_ID, widgetId))
        if (widgetId == AppWidgetManager.INVALID_APPWIDGET_ID) {
            finish()
            return
        }

        setContentView(R.layout.widget_configure)
        val snapshot = WidgetStore.read(this)
        val list = findViewById<ListView>(R.id.configure_list)

        if (snapshot.hidden || snapshot.memos.isEmpty()) {
            findViewById<TextView>(R.id.configure_empty).visibility = android.view.View.VISIBLE
            list.visibility = android.view.View.GONE
            return
        }

        // "Most recently edited" first, then the memos themselves, newest first as published.
        val choices = listOf(WidgetStore.MOST_RECENT to null) + snapshot.memos.map { it.id to it }
        val rows = choices.map { (_, memo) ->
            mapOf(
                "title" to (memo?.title?.ifEmpty { getString(R.string.widget_untitled) }
                    ?: getString(R.string.widget_configure_recent)),
                "subtitle" to (memo?.preview ?: getString(R.string.widget_configure_recent_hint)),
            )
        }
        list.adapter = SimpleAdapter(
            this,
            rows,
            android.R.layout.simple_list_item_2,
            arrayOf("title", "subtitle"),
            intArrayOf(android.R.id.text1, android.R.id.text2),
        )
        list.setOnItemClickListener { _, _, position, _ -> choose(choices[position].first) }
    }

    private fun choose(memoId: String) {
        WidgetStore.setNoteMemo(this, widgetId, memoId)
        val manager = AppWidgetManager.getInstance(this)
        NoteWidget.update(this, manager, widgetId)
        setResult(RESULT_OK, Intent().putExtra(AppWidgetManager.EXTRA_APPWIDGET_ID, widgetId))
        finish()
    }
}
