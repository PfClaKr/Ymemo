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
