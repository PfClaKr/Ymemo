// Device-local preferences and the "stay unlocked" session.
//
// Neither is ever synced. The preferences are a JSON file in the app's private directory,
// read and written by the Rust side (`settings_load` / `settings_save`) so the language is
// decided in the same place the core's error messages are translated.
//
// The session is a different matter. It holds the **key derived from the master password**,
// and while it exists the password buys nothing: whoever can read it can read the memos. That
// is the cost of not typing a password every time, the same one the desktop pays. Two things
// follow from it:
//
//  - The key goes to `flutter_secure_storage` — on Android, EncryptedSharedPreferences behind
//    a key held in the platform keystore — never to a plain file next to the vault.
//  - The expiry is fixed at the moment the password was typed and never extended by use, so
//    "asks again every N days" is true rather than approximately true. Locking, changing the
//    period, or the expiry passing all delete it.

import 'dart:convert';

import 'package:flutter_secure_storage/flutter_secure_storage.dart';

import 'src/rust/api.dart' as ffi;

/// The stored vault key and when it stops being usable.
class Session {
  const Session({required this.key, required this.expiresAt});

  final List<int> key;
  final DateTime expiresAt;

  bool get expired => DateTime.now().isAfter(expiresAt);
}

/// Reads and writes the session in the platform's keystore.
class SessionStore {
  const SessionStore();

  static const _storage = FlutterSecureStorage(
    // Without this the plugin falls back to plain SharedPreferences on older devices, which
    // is exactly what must not happen to this particular value.
    aOptions: AndroidOptions(encryptedSharedPreferences: true),
  );
  static const _keyName = 'vault_key';
  static const _expiryName = 'vault_key_expires_at';

  /// The stored session, or null when there is none, it has expired, or it is unreadable.
  /// An unusable entry is deleted on the way out rather than left to fail again later.
  Future<Session?> read() async {
    try {
      final encoded = await _storage.read(key: _keyName);
      final expiry = await _storage.read(key: _expiryName);
      if (encoded == null || expiry == null) return null;

      final millis = int.tryParse(expiry);
      if (millis == null) {
        await clear();
        return null;
      }
      final session = Session(
        key: base64Decode(encoded),
        expiresAt: DateTime.fromMillisecondsSinceEpoch(millis),
      );
      if (session.expired) {
        await clear();
        return null;
      }
      return session;
    } catch (_) {
      // A keystore that refuses to decrypt (restored backup, changed lock screen) is not an
      // error worth surfacing: it just means typing the password again.
      await clear();
      return null;
    }
  }

  /// Stores the key for [days]; zero days stores nothing at all.
  Future<void> write(List<int> key, int days) async {
    if (days <= 0) {
      await clear();
      return;
    }
    final expiry = DateTime.now().add(Duration(days: days));
    await _storage.write(key: _keyName, value: base64Encode(key));
    await _storage.write(
      key: _expiryName,
      value: '${expiry.millisecondsSinceEpoch}',
    );
  }

  Future<void> clear() async {
    await _storage.delete(key: _keyName);
    await _storage.delete(key: _expiryName);
  }
}

/// The preferences plus the file they came from, so saving does not need the path passed
/// around the widget tree.
class SettingsStore {
  SettingsStore({required this.path, required ffi.FfiSettings initial}) : _current = initial;

  /// `settings.json` in the app's private directory.
  final String path;
  ffi.FfiSettings _current;

  ffi.FfiSettings get value => _current;

  static Future<SettingsStore> load(String path) async {
    return SettingsStore(path: path, initial: await ffi.settingsLoad(path: path));
  }

  /// Writes and keeps what Rust actually stored, which is the sanitized version — an
  /// out-of-range value must not linger on screen as if it had been accepted.
  Future<ffi.FfiSettings> save(ffi.FfiSettings next) async {
    _current = await ffi.settingsSave(path: path, settings: next);
    return _current;
  }

  /// Records that an update check just happened, without touching anything else.
  Future<void> markUpdateChecked() async {
    final s = _current;
    await save(ffi.FfiSettings(
      lang: s.lang,
      unlockDays: s.unlockDays,
      lockOnBackground: s.lockOnBackground,
      updateCheck: s.updateCheck,
      lastUpdateCheck: DateTime.now().millisecondsSinceEpoch,
    ));
  }

  /// Whether an automatic check is due: enabled, and not already done today.
  bool get updateCheckDue {
    if (!_current.updateCheck) return false;
    final last = _current.lastUpdateCheck.toInt();
    return DateTime.now().millisecondsSinceEpoch - last >= const Duration(days: 1).inMilliseconds;
  }
}
