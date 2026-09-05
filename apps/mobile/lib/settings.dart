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
//
// Biometric unlock ([BiometricStore]) is a second copy of that same key, under its own name
// and with no expiry, released only once the device says the fingerprint checked out. It is
// deliberately not the session: locking clears the session — that is what the lock button
// means — while opening the lock again is the whole point of the fingerprint.

import 'dart:convert';

import 'package:flutter/widgets.dart';
import 'package:flutter_secure_storage/flutter_secure_storage.dart';
import 'package:local_auth/local_auth.dart';
import 'package:local_auth_android/local_auth_android.dart';

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
      biometricUnlock: s.biometricUnlock,
      updateCheck: s.updateCheck,
      mergeSeconds: s.mergeSeconds,
      watchDelaySeconds: s.watchDelaySeconds,
      rescanSeconds: s.rescanSeconds,
      wifiOnlySync: s.wifiOnlySync,
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

/// The vault key kept for biometric unlock, and the prompt that releases it.
///
/// **A fingerprint cannot derive a key.** There is nothing secret enough in one to build a
/// key from, and it is not something its owner can change. So what this switch really does is
/// keep a copy of the data key on the device — in the same keystore-backed storage the
/// session uses — and put the device's own biometric check in front of reading it. While it
/// is on, the master password no longer protects the memos *on this phone*: anyone whose
/// fingerprint this phone accepts can open the vault. That is the trade "stay unlocked"
/// makes as well; the settings screen says it out loud and both default to off.
///
/// The stored key has no expiry, so it survives locking. Turning the switch off deletes it,
/// and so does resetting the vault — it would otherwise be the key to a vault that is gone.
class BiometricStore {
  const BiometricStore();

  static const _storage = FlutterSecureStorage(
    // Same reasoning as the session: never the plain-SharedPreferences fallback.
    aOptions: AndroidOptions(encryptedSharedPreferences: true),
  );
  static const _keyName = 'biometric_vault_key';
  static final _auth = LocalAuthentication();

  /// Whether this device can check a fingerprint or face **right now**: the hardware is there
  /// and something is enrolled. False is the ordinary answer on an emulator, or on a phone
  /// whose owner has set no screen lock, and the setting says so rather than failing.
  Future<bool> get available async {
    try {
      return await _auth.canCheckBiometrics && await _auth.isDeviceSupported();
    } catch (e) {
      debugPrint('could not ask about biometrics: $e');
      return false;
    }
  }

  /// Whether a key is stored, i.e. the switch was turned on and not turned off again.
  Future<bool> get enrolled async {
    try {
      return await _storage.read(key: _keyName) != null;
    } catch (_) {
      return false;
    }
  }

  /// Puts up the system prompt. True only on a real success — a cancel, a lockout after too
  /// many bad attempts, and a device with nothing enrolled all come back false.
  ///
  /// [title] and [cancel] are passed because the plugin's own defaults are English, and a
  /// dialog reading "Authentication required" over a Korean sentence is worse than either
  /// language alone. The subtitle is dropped: with a title and a reason, "Verify identity"
  /// is a third way of saying the same thing.
  Future<bool> confirm(String reason, {required String title, required String cancel}) async {
    try {
      return await _auth.authenticate(
        localizedReason: reason,
        authMessages: [
          AndroidAuthMessages(signInTitle: title, biometricHint: '', cancelButton: cancel),
        ],
        options: const AuthenticationOptions(
          // No device-PIN fallback: the master password is the fallback, and it is already
          // on the screen behind the prompt.
          biometricOnly: true,
          // The prompt outlives a trip to another app rather than quietly cancelling.
          stickyAuth: true,
        ),
      );
    } catch (e) {
      debugPrint('biometric prompt failed: $e');
      return false;
    }
  }

  /// Stores the key. Called with the vault open, which is the only time there is one to store.
  Future<void> enable(List<int> key) =>
      _storage.write(key: _keyName, value: base64Encode(key));

  Future<void> disable() => _storage.delete(key: _keyName);

  /// The stored key after a successful prompt, or null: nothing stored, a refused prompt, or
  /// an entry the keystore will not decrypt (a restored backup, a changed screen lock). That
  /// last one is dropped on the way out rather than left to fail again every time.
  Future<List<int>?> unlock(String reason, {required String title, required String cancel}) async {
    if (!await enrolled) return null;
    if (!await confirm(reason, title: title, cancel: cancel)) return null;
    try {
      final encoded = await _storage.read(key: _keyName);
      return encoded == null ? null : base64Decode(encoded);
    } catch (e) {
      debugPrint('stored biometric key is unreadable: $e');
      await disable();
      return null;
    }
  }
}
