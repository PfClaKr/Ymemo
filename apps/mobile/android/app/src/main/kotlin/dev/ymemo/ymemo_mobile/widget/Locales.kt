package dev.ymemo.ymemo_mobile.widget

import android.content.Context
import android.content.res.Configuration
import java.util.Locale

/**
 * The widgets' strings in the language the app is set to, not the system's.
 *
 * Android resolves a widget's strings, and its settings screens', against the **system**
 * locale — so a phone in English with Ymemo set to Korean in its own settings put English
 * widgets and an English "what should this widget show?" beside a Korean app. The app writes
 * its language into every snapshot (`lang` in `home_widgets.dart`); this reads it back and
 * hands out a context configured for it. "auto" — or no snapshot yet — is the system's.
 */
internal object Locales {
    fun localized(context: Context): Context {
        val lang = WidgetStore.read(context).lang
        if (lang != "ko" && lang != "en") return context
        val config = Configuration(context.resources.configuration)
        config.setLocale(Locale.forLanguageTag(lang))
        return context.createConfigurationContext(config)
    }
}
