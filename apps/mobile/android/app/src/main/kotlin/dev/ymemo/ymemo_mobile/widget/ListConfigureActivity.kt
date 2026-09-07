package dev.ymemo.ymemo_mobile.widget

import android.app.Activity
import android.appwidget.AppWidgetManager
import android.content.Intent
import android.os.Bundle
import android.view.View
import android.widget.Button
import android.widget.CheckBox
import android.widget.ImageView
import android.widget.LinearLayout
import android.widget.RadioButton
import android.widget.RadioGroup
import android.widget.SeekBar
import android.widget.TextView
import dev.ymemo.ymemo_mobile.R

/**
 * What one memo-list widget shows, and what it looks like: a folder, a paper color, an
 * opacity.
 *
 * Reached three ways — placing the widget, the gear on its header, and the launcher's own
 * "reconfigure" on Android 12 and up. Like the sticky widget's picker it lists the
 * **published snapshot** and not the vault: this screen can be opened from the widget picker
 * on a device where the app has never been unlocked, and it has no key. With nothing
 * published it still offers the color and the opacity, because those need no memos; only the
 * folder list is replaced by the sentence that says to open the app once.
 *
 * The settings are per widget and device-local (`WidgetStore`, a private SharedPreferences
 * file). A home screen belongs to one device — the other devices have their own, arranged
 * their own way — so none of this goes near the vault.
 */
class ListConfigureActivity : Activity() {

    private var widgetId = AppWidgetManager.INVALID_APPWIDGET_ID

    /** Folder id per radio button id, in the order they were added. */
    private val folderIds = mutableListOf<String>()
    private val colorKeys = mutableListOf<String>()

    /** One checkbox per memo, by memo id, for the "memos I pick" answer. */
    private val pickBoxes = linkedMapOf<String, CheckBox>()

    /**
     * Whether the folder question could be asked at all.
     *
     * False while the vault is closed: there is no published folder list then, so the screen
     * cannot offer the folder this widget is already set to — and saving what it *could*
     * offer would quietly reset the widget to "everything". Someone who opened this on a
     * locked phone to change the colour must not lose the folder as the price.
     */
    private var foldersOffered = false

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)

        widgetId = intent?.extras?.getInt(
            AppWidgetManager.EXTRA_APPWIDGET_ID,
            AppWidgetManager.INVALID_APPWIDGET_ID,
        ) ?: AppWidgetManager.INVALID_APPWIDGET_ID
        // Backing out has to leave no widget behind, so the cancelled result is set first and
        // only replaced once the button at the bottom has been pressed.
        setResult(RESULT_CANCELED, Intent().putExtra(AppWidgetManager.EXTRA_APPWIDGET_ID, widgetId))
        if (widgetId == AppWidgetManager.INVALID_APPWIDGET_ID) {
            finish()
            return
        }

        setContentView(R.layout.widget_list_configure)
        val snapshot = WidgetStore.read(this)

        fillFolders(snapshot)
        fillColors()
        fillOpacity()
        updatePreview()

        findViewById<Button>(R.id.configure_done).setOnClickListener { save() }
    }

    /**
     * "Everything", then every folder, each one indented under its parent.
     *
     * The whole tree and not only the top level: a widget is worth pointing at the folder
     * things actually go in, which is rarely one at the root. Depth is drawn with spaces
     * rather than a real tree view — this is a list of at most a few dozen radio buttons, and
     * an expandable tree here would be more machinery than the question deserves.
     */
    private fun fillFolders(snapshot: Snapshot) {
        val group = findViewById<RadioGroup>(R.id.configure_folders)

        // Locked, or never unlocked on this device: no folder list to choose from. Say so and
        // leave the question out entirely rather than offering the one answer that happens to
        // need no data — see [foldersOffered].
        if (snapshot.hidden) {
            findViewById<TextView>(R.id.configure_empty).visibility = View.VISIBLE
            group.visibility = View.GONE
            findViewById<TextView>(R.id.configure_source_label).visibility = View.GONE
            return
        }
        foldersOffered = true

        // The picks have to come along: without them "memos I pick" looks like a choice with
        // nothing behind it, and the screen quietly reopens on "everything" — throwing the
        // widget's real setting away the moment Done is pressed.
        val chosen = snapshot.resolveFolder(
            WidgetStore.listFolder(this, widgetId),
            WidgetStore.listPicks(this, widgetId),
        )
        addChoice(group, folderIds, WidgetStore.EVERYTHING,
            getString(R.string.widget_list_configure_everything), chosen)

        // Walk from the root down, so a child is never offered before its parent. A folder
        // whose ancestry loops is published with an empty parent by the app, so it lands at
        // the top rather than dropping out of the list here.
        fun walk(parent: String, depth: Int) {
            snapshot.folders.filter { it.parent == parent }.forEach { folder ->
                // Indented with padding, not with spaces in the label: spaces are the width of
                // whatever font the device happens to use, a screen reader reads them out, and
                // an ellipsized long name puts the "…" in a different place on every row.
                val button = addChoice(group, folderIds, folder.id,
                    folder.title.ifEmpty { getString(R.string.widget_untitled) }, chosen)
                button.setPadding(
                    button.paddingLeft + depth * INDENT,
                    button.paddingTop,
                    button.paddingRight,
                    button.paddingBottom,
                )
                if (depth < MAX_DEPTH) walk(folder.id, depth + 1)
            }
        }
        walk("", 0)

        // Last, so its checklist is directly underneath it. Between the folders it read as
        // another folder's contents rather than as its own answer.
        addChoice(group, folderIds, WidgetStore.PICKED,
            getString(R.string.widget_list_configure_pick), chosen)
        fillPicks(snapshot)
        // The checklist belongs to one answer, so it comes and goes with it.
        group.setOnCheckedChangeListener { _, _ -> showPicksIfChosen() }
        showPicksIfChosen()
    }

    /**
     * A checkbox per memo, for a widget that shows a handful of named notes rather than a
     * folder — the shopping list and the door code, and nothing else moving around them.
     *
     * The memos are the snapshot's, so the same cap applies as everywhere else: the most
     * recently edited ones. A memo that falls off the end of that, or is deleted, simply
     * stops being drawn; the rest of the picks are unaffected.
     */
    private fun fillPicks(snapshot: Snapshot) {
        val container = findViewById<LinearLayout>(R.id.configure_picks)
        val picked = WidgetStore.listPicks(this, widgetId)
        snapshot.memos.forEach { memo ->
            val box = CheckBox(this).apply {
                id = View.generateViewId()
                text = memo.title.ifEmpty { getString(R.string.widget_untitled) }
                isChecked = picked.contains(memo.id)
                setPadding(paddingLeft, PADDING, paddingRight, PADDING)
                layoutParams = LinearLayout.LayoutParams(
                    LinearLayout.LayoutParams.MATCH_PARENT,
                    LinearLayout.LayoutParams.WRAP_CONTENT,
                )
            }
            container.addView(box)
            pickBoxes[memo.id] = box
        }
    }

    /** Shows the checklist only while "memos I pick" is the answer. */
    private fun showPicksIfChosen() {
        val chosen = chosenValue(
            findViewById(R.id.configure_folders), folderIds, WidgetStore.EVERYTHING,
        )
        findViewById<LinearLayout>(R.id.configure_picks).visibility =
            if (chosen == WidgetStore.PICKED) View.VISIBLE else View.GONE
    }

    /** The launcher's own light/dark chrome, or one of the sticky papers. */
    private fun fillColors() {
        val group = findViewById<RadioGroup>(R.id.configure_colors)
        val chosen = WidgetStore.listColor(this, widgetId)

        addChoice(group, colorKeys, WidgetStore.THEME_COLOR,
            getString(R.string.widget_list_configure_theme), chosen)
        PAPERS.forEach { (key, label) ->
            val button = addChoice(group, colorKeys, key, getString(label), chosen)
            // The swatch is the answer: a row of color names is a row of words about colors.
            button.setBackgroundColor(Palette.bg(key))
            button.setTextColor(Palette.ink(key))
        }
        group.setOnCheckedChangeListener { _, _ -> updatePreview() }
    }

    private fun fillOpacity() {
        val bar = findViewById<SeekBar>(R.id.configure_opacity)
        val value = findViewById<TextView>(R.id.configure_opacity_value)
        val start = WidgetStore.listAlpha(this, widgetId)

        // The bar reads 0..100, so the thumb sits where the number says: at 50% it is half
        // way along. It used to be 0..(100 - MIN_ALPHA) with the value shifted on top, which
        // put the thumb at 37% while the label read 50% — the floor was being spent as a
        // shifted origin instead of as a floor.
        //
        // The floor still holds: dragging below it snaps back, so a widget cannot be pulled
        // all the way to invisible and then hunted for on the wallpaper.
        bar.max = 100
        bar.progress = start
        value.text = getString(R.string.widget_list_configure_percent, start)
        bar.setOnSeekBarChangeListener(object : SeekBar.OnSeekBarChangeListener {
            override fun onProgressChanged(bar: SeekBar, progress: Int, fromUser: Boolean) {
                if (progress < WidgetStore.MIN_ALPHA) {
                    // Re-enters once with the floor, which then takes the branch below.
                    bar.progress = WidgetStore.MIN_ALPHA
                    return
                }
                value.text = getString(R.string.widget_list_configure_percent, progress)
                updatePreview()
            }

            override fun onStartTrackingTouch(bar: SeekBar) = Unit
            override fun onStopTrackingTouch(bar: SeekBar) = Unit
        })
    }

    /**
     * Repaints the card above the questions in the colours currently chosen.
     *
     * Without it the only way to see what 45% looks like is to save, look at the home screen,
     * come back in and drag again — and opacity is exactly the setting nobody gets right
     * first try. The desktop's own opacity slider previews live for the same reason.
     */
    private fun updatePreview() {
        val chrome = Chrome.of(this, chosenValue(
            findViewById(R.id.configure_colors), colorKeys, WidgetStore.THEME_COLOR,
        ))
        val alpha = findViewById<SeekBar>(R.id.configure_opacity).progress

        val card = findViewById<ImageView>(R.id.preview_card)
        card.setColorFilter(chrome.card)
        card.imageAlpha = alpha * 255 / 100

        findViewById<TextView>(R.id.preview_title).apply {
            setTextColor(chrome.ink)
            text = getString(R.string.widget_list_label)
        }
        findViewById<View>(R.id.preview_divider).setBackgroundColor(chrome.divider)
        findViewById<TextView>(R.id.preview_row1).apply {
            setTextColor(chrome.ink)
            text = getString(R.string.widget_list_configure_preview_row)
        }
        findViewById<TextView>(R.id.preview_row2).apply {
            setTextColor(chrome.muted)
            text = getString(R.string.widget_list_configure_preview_row2)
        }
    }

    /** One radio button, remembering which value it stands for by its position. */
    private fun addChoice(
        group: RadioGroup,
        values: MutableList<String>,
        value: String,
        label: String,
        chosen: String,
    ): RadioButton {
        val button = RadioButton(this).apply {
            id = View.generateViewId()
            text = label
            setPadding(paddingLeft, PADDING, paddingRight, PADDING)
            isChecked = value == chosen
            // Full width, or the paper colour behind a swatch stops where its name does and
            // the column reads as a ragged edge rather than a list of colours.
            layoutParams = RadioGroup.LayoutParams(
                RadioGroup.LayoutParams.MATCH_PARENT,
                RadioGroup.LayoutParams.WRAP_CONTENT,
            )
        }
        group.addView(button)
        values.add(value)
        return button
    }

    private fun chosenValue(group: RadioGroup, values: List<String>, fallback: String): String {
        val index = (0 until group.childCount).firstOrNull {
            (group.getChildAt(it) as RadioButton).isChecked
        } ?: return fallback
        return values.getOrElse(index) { fallback }
    }

    private fun save() {
        // Keep what the widget already showed when the question could not be asked.
        val folder = if (foldersOffered) {
            chosenValue(findViewById(R.id.configure_folders), folderIds, WidgetStore.EVERYTHING)
        } else {
            WidgetStore.listFolder(this, widgetId)
        }
        val color = chosenValue(
            findViewById(R.id.configure_colors), colorKeys, WidgetStore.THEME_COLOR,
        )
        val alpha = findViewById<SeekBar>(R.id.configure_opacity).progress
        // Kept even when the answer is a folder, so switching to a folder and back finds the
        // picks where they were left.
        val picks = pickBoxes.filterValues { it.isChecked }.keys.toSet()
        WidgetStore.setList(this, widgetId, folder, picks, color, alpha)

        val manager = AppWidgetManager.getInstance(this)
        // The rows come from a RemoteViewsFactory, which only re-reads when it is told to:
        // updating the frame alone would leave the old folder's memos inside the new heading.
        manager.notifyAppWidgetViewDataChanged(widgetId, R.id.list_items)
        MemoListWidget.update(this, manager, widgetId)

        setResult(RESULT_OK, Intent().putExtra(AppWidgetManager.EXTRA_APPWIDGET_ID, widgetId))
        finish()
    }

    private companion object {
        /** Vertical padding on a radio row, in px at mdpi — plenty at every density. */
        const val PADDING = 24

        /** Deep enough for any folder anyone files memos in, and an end to a looping tree. */
        const val MAX_DEPTH = 8

        /** How far one level of nesting shifts a folder, in px at mdpi. */
        const val INDENT = 40

        /** The palette, in the order the app's own color picker uses. */
        val PAPERS = listOf(
            "yellow" to R.string.widget_color_yellow,
            "pink" to R.string.widget_color_pink,
            "green" to R.string.widget_color_green,
            "blue" to R.string.widget_color_blue,
            "purple" to R.string.widget_color_purple,
        )
    }
}
