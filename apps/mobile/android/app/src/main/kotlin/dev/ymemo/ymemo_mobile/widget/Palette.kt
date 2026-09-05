package dev.ymemo.ymemo_mobile.widget

import android.content.Context
import dev.ymemo.ymemo_mobile.R

/**
 * The sticky palette, for the one place it has to exist outside Dart.
 *
 * This mirrors `lib/palette.dart`, which in turn mirrors the desktop's `theme.slint`. The
 * core syncs **the key alone** (`"yellow"`, `"pink"`, ...), so a memo colored on the desktop
 * is the same color in this widget only as long as every side maps the key the same way. Add
 * a key here and there, or not at all. An unknown key falls back to yellow rather than
 * failing, so a vault written by a newer version still draws.
 *
 * These do not follow the system's dark theme, unlike the widget chrome in `values/colors.xml`:
 * a note is paper, and paper that turned black at sunset would not read as the same note.
 */
internal object Palette {

    /** The paper a note is written on. */
    fun bg(key: String): Int = when (key) {
        "pink" -> 0xFFFFF2F7.toInt()
        "green" -> 0xFFF2FCEF.toInt()
        "blue" -> 0xFFEFF7FF.toInt()
        "purple" -> 0xFFF8F2FD.toInt()
        else -> 0xFFFFFCE3.toInt()
    }

    /** Title bar: opaque and darker than the body. */
    fun bar(key: String): Int = when (key) {
        "pink" -> 0xFFF7B8CE.toInt()
        "green" -> 0xFFB6E0A8.toInt()
        "blue" -> 0xFFB0D4F2.toInt()
        "purple" -> 0xFFD0B8EC.toInt()
        else -> 0xFFF4E98C.toInt()
    }

    /** Title bar text, kept legible on [bar] for each color. */
    fun ink(key: String): Int = when (key) {
        "pink" -> 0xFF7A3350.toInt()
        "green" -> 0xFF35662F.toInt()
        "blue" -> 0xFF2A5578.toInt()
        "purple" -> 0xFF573377.toInt()
        else -> 0xFF5C5C25.toInt()
    }

    /** Saturated accent, for the stripe down the side of a list row. */
    fun swatch(key: String): Int = when (key) {
        "pink" -> 0xFFFF9FC0.toInt()
        "green" -> 0xFF8FD678.toInt()
        "blue" -> 0xFF7DB8EC.toInt()
        "purple" -> 0xFFB98FE0.toInt()
        else -> 0xFFFFE15C.toInt()
    }
}

/**
 * The colors one list widget is drawn in, once its own setting has been taken into account.
 *
 * Left on [WidgetStore.THEME_COLOR] a widget follows the *system's* light/dark setting, the
 * way the widget chrome in `values/colors.xml` always has — a home screen is the launcher's
 * screen, not the app's. Choosing a paper color takes it out of that, and the ink then has to
 * come from the paper: `values-night`'s pale ink on a pastel card would be white on cream.
 */
internal data class Chrome(
    val card: Int,
    val ink: Int,
    val muted: Int,
    val divider: Int,
    val icon: Int,
) {
    companion object {
        fun of(context: Context, key: String): Chrome {
            if (key == WidgetStore.THEME_COLOR) {
                fun color(id: Int) = context.resources.getColor(id, context.theme)
                return Chrome(
                    card = color(R.color.widget_surface),
                    ink = color(R.color.widget_ink),
                    muted = color(R.color.widget_muted),
                    divider = color(R.color.widget_divider),
                    icon = color(R.color.widget_accent),
                )
            }
            val ink = Palette.ink(key)
            return Chrome(
                card = Palette.bg(key),
                ink = ink,
                muted = alpha(ink, 0x9E),
                divider = alpha(ink, 0x2E),
                icon = ink,
            )
        }

        private fun alpha(color: Int, a: Int): Int = (color and 0x00FFFFFF) or (a shl 24)
    }
}
