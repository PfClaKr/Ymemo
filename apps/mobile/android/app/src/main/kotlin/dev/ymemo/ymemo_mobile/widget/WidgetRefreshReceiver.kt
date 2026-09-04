package dev.ymemo.ymemo_mobile.widget

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent

/**
 * Redraws the widgets on the two occasions the app is not running to do it itself.
 *
 * **After an update.** A replaced package leaves every widget on the launcher's "updating"
 * placeholder until something hands it new `RemoteViews`, and with `updatePeriodMillis="0"`
 * nothing would until the app was next opened.
 *
 * **After a language change.** What the widgets draw around the memo text — the heading, the
 * locked notice — comes from the `res/values` string resources and so follows the *system*
 * language, which no snapshot carries and nothing else would notice had changed.
 *
 * Neither needs the vault: both redraw from the last published snapshot, which is the whole
 * point of publishing one.
 */
class WidgetRefreshReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        when (intent.action) {
            Intent.ACTION_MY_PACKAGE_REPLACED, Intent.ACTION_LOCALE_CHANGED ->
                Widgets.refreshAll(context.applicationContext)
        }
    }
}
