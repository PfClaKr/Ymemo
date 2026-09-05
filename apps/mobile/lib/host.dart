// The few things the Android host does that Dart cannot.
//
// One channel, one place. Each call is an optimisation or a platform detail the app can live
// without, so a missing implementation is treated as "not available" rather than an error —
// that is also what keeps the same Dart running on a platform with no host side at all.

import 'package:flutter/services.dart';
import 'package:flutter/widgets.dart';

const _channel = MethodChannel('dev.ymemo/native');

Future<T?> _invoke<T>(String method, [Object? argument]) async {
  try {
    return await _channel.invokeMethod<T>(method, argument);
  } on PlatformException catch (e) {
    debugPrint('$method failed: ${e.message}');
    return null;
  } on MissingPluginException {
    return null;
  }
}

/// Path of the bundled sync daemon, or null when this build ships none.
Future<String?> syncBinaryPath() => _invoke<String>('syncBinaryPath');

/// Wifi multicast lock, without which the stack drops the LAN pairing broadcast. Held only
/// while the pairing screen is open.
Future<void> acquireMulticastLock() => _invoke<bool>('acquireMulticastLock');
Future<void> releaseMulticastLock() => _invoke<void>('releaseMulticastLock');

/// Opens a link in the browser; the app never installs anything itself.
Future<void> openUrl(String url) => _invoke<void>('openUrl', url);

/// Keeps the window out of screenshots and the app switcher.
Future<void> setScreenshotBlock(bool secure) => _invoke<void>('setSecure', secure);

/// Hands the home-screen widgets a fresh snapshot to draw; see [home_widgets.dart].
Future<void> widgetPublish(String snapshot) => _invoke<void>('widgetPublish', snapshot);

/// The widget tap that launched the app, if it was one. Consumed by the call: asking twice
/// returns nothing the second time, which is what keeps a relaunch from repeating it.
Future<Map<Object?, Object?>?> takeWidgetAction() =>
    _invoke<Map<Object?, Object?>>('takeWidgetAction');

/// Whether the current network is one the user is not paying by the byte for. True when
/// there is no network at all, and on a platform with no host side to ask.
Future<bool> isUnmetered() async => await _invoke<bool>('isUnmetered') ?? true;

/// Calls the host pushes at us, by method name.
///
/// One map rather than one `setMethodCallHandler` per feature: the channel keeps a **single**
/// handler, so a second registration would silently unsubscribe the first — which is how a
/// widget tap would quietly stop opening its memo the day something else started listening.
final _incoming = <String, void Function(Object?)>{};
bool _listening = false;

void _listen(String method, void Function(Object?) handler) {
  _incoming[method] = handler;
  if (_listening) return;
  _listening = true;
  _channel.setMethodCallHandler((call) async {
    _incoming[call.method]?.call(call.arguments);
  });
}

/// Called when a widget is tapped while the app is already running, which arrives as
/// `onNewIntent` rather than as a launch.
void onWidgetAction(void Function(Map<Object?, Object?>) handler) {
  _listen('widgetAction', (args) {
    if (args is Map) handler(args);
  });
}

/// Called when the network changes underfoot, with the new answer to [isUnmetered].
void onNetworkChanged(void Function(bool unmetered) handler) {
  _listen('networkChanged', (args) => handler(args as bool? ?? true));
}
