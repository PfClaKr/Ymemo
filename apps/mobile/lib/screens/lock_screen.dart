/// The lock screen: unlocking, creating a vault, linking a new device, and the ways back
/// from a forgotten password.
library;

import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

import '../security.dart';
import '../settings.dart';
import '../src/rust/api.dart';
import '../sync.dart';
import '../ui_util.dart';
import 'sync_screen.dart';

/// Lock screen: opens the vault with the master password, creating it if needed.
///
/// Pairing is reachable from here, before any password: a device that has just been installed
/// has to receive the existing vault.json before it can be unlocked at all.
class LockScreen extends StatefulWidget {
  const LockScreen({
    super.key,
    required this.strings,
    required this.sync,
    required this.settings,
    required this.cacheDbPath,
    required this.onUnlocked,
  });

  final FfiStrings strings;
  final SyncController sync;

  /// Read for one thing only: whether biometric unlock was turned on.
  final SettingsStore settings;

  final String cacheDbPath;

  /// Called once the vault is open; the app decides what to show next.
  final Future<void> Function() onUnlocked;

  @override
  State<LockScreen> createState() => _LockScreenState();
}

class _LockScreenState extends State<LockScreen> {
  /// How often `vault.json` is re-read while this screen is up. Cheap — two `exists` checks
  /// — and it has to be a poll: the daemon delivers a paired device's vault whenever it
  /// likes, with no user action to hang the update on.
  static const _probeInterval = Duration(seconds: 3);

  final _password = TextEditingController();
  /// Typed again when a vault is being created. This is the one password in the app that is
  /// never checked against anything before it is used: a slip encrypts every memo under a
  /// password nobody knows, and the recovery code that would rescue it is only issued on the
  /// screen *after* this one. Changing the password already asks twice; creating it did not
  /// — and on a phone, where every character is a tap on glass and shows as a dot.
  final _confirm = TextEditingController();
  /// Whether the password fields are showing their text. The desktop's field has this built
  /// in; here it is a button, off by default.
  bool _reveal = false;

  /// Recovery inputs, only built while the forgotten-password panel is open.
  final _recoveryCode = TextEditingController();
  final _recoveryPassword = TextEditingController();
  final _recoveryConfirm = TextEditingController();

  String? _error;

  /// A plain message rather than a failure — only "everything was deleted, create a new
  /// vault" so far. Kept apart from [_error] because red would make a completed reset read
  /// as a failed one.
  String? _notice;

  bool _busy = false;

  /// Whether `vault.json` is already there. It decides the whole screen: entering a password
  /// versus setting one, and whether there is anything to recover in the first place.
  bool _vaultExists = false;
  bool _hasRecovery = false;

  /// Whether the forgotten-password panel is open, and whether the wipe inside it has been
  /// confirmed once — deleting every memo on the device is not a single tap.
  bool _recovering = false;
  bool _confirmingReset = false;

  /// Whether the first-run screen is still on the choice rather than the password field.
  /// Only ever true while there is no vault; pairing one in flips `_vaultExists` and the
  /// screen becomes the unlock prompt on its own.
  bool _choosing = true;

  /// Whether to draw the fingerprint button: the setting is on, a key is stored, and the
  /// device can actually check one. Resolved once, asynchronously, because all three
  /// questions cross the platform channel.
  bool _biometricReady = false;

  /// So a refused or cancelled prompt is not immediately put up again by a rebuild. The
  /// button stays, and pressing it asks again.
  bool _biometricTried = false;

  Timer? _probe;

  @override
  void initState() {
    super.initState();
    _probeVault();
    _probe = Timer.periodic(_probeInterval, (_) => _probeVault());
    _prepareBiometrics();
    passwordMinChars().then((n) {
      if (mounted) setState(() => _minPassword = n);
    });
  }

  /// Decides whether the fingerprint button belongs on this screen, and offers the prompt
  /// straight away if it does — reaching for a finger is why the setting was turned on, and
  /// making it a two-step (open the app, press a button, then the prompt) would undo that.
  Future<void> _prepareBiometrics() async {
    if (!widget.settings.value.biometricUnlock) return;
    const store = BiometricStore();
    final ready = await store.enrolled && await store.available;
    if (!mounted || !ready) return;
    setState(() => _biometricReady = true);
    await _unlockWithBiometrics();
  }

  /// Opens the vault with the key the fingerprint releases.
  ///
  /// A refusal is silent: the user either cancelled, or their finger was not recognised and
  /// the system prompt has already said so. A key that does **not** open the vault is a
  /// different matter — it is stale, so it is dropped and the password takes over, exactly
  /// as a diverged session key is handled.
  Future<void> _unlockWithBiometrics() async {
    if (_busy) return;
    setState(() {
      _busy = true;
      _biometricTried = true;
      _error = null;
      _notice = null;
    });
    try {
      final key = await const BiometricStore().unlock(
        widget.strings.biometricUnlock,
        title: widget.strings.biometricPrompt,
        cancel: widget.strings.cancel,
      );
      if (key == null) return;
      await vaultOpenWithKey(
        vaultDir: widget.sync.paths.vaultDir,
        cacheDbPath: widget.cacheDbPath,
        key: Uint8List.fromList(key),
      );
      await widget.onUnlocked();
    } catch (e) {
      debugPrint('the stored fingerprint key did not open the vault: $e');
      await const BiometricStore().disable();
      if (mounted) {
        setState(() {
          _biometricReady = false;
          _error = '$e';
        });
      }
    } finally {
      if (mounted) setState(() => _busy = false);
    }
  }

  @override
  void dispose() {
    _probe?.cancel();
    _password.dispose();
    _confirm.dispose();
    _recoveryCode.dispose();
    _recoveryPassword.dispose();
    _recoveryConfirm.dispose();
    super.dispose();
  }

  /// Reads what `vault.json` says, without unlocking anything.
  ///
  /// Pairing runs from this very screen, and a fresh install receives the existing vault
  /// while the screen sits there — so this is what turns "set a password" into "enter the
  /// password" the moment the vault lands, instead of inviting the user to create a second
  /// one with a different salt that could never converge with the first.
  Future<void> _probeVault() async {
    final dir = widget.sync.paths.vaultDir;
    final exists = await vaultExists(vaultDir: dir);
    final hasRecovery = await vaultHasRecoveryCode(vaultDir: dir);
    // Only on a real change: a rebuild every three seconds would be pure waste, and it would
    // land in the middle of typing.
    if (!mounted || (exists == _vaultExists && hasRecovery == _hasRecovery)) return;
    setState(() {
      _vaultExists = exists;
      _hasRecovery = hasRecovery;
    });
  }

  /// Whether the button may be pressed: a password, and — when creating — the same one twice.
  bool get _canSubmit =>
      _password.text.isNotEmpty &&
      (_vaultExists || (!_tooShort(_password.text) && _confirm.text == _password.text));

  /// Shortest new password accepted, from the core; 8 until it has answered.
  int _minPassword = 8;

  /// Only a *new* password is held to it: a vault made with a shorter one still opens.
  bool _tooShort(String password) => password.characters.length < _minPassword;

  Future<void> _unlock() async {
    if (!_canSubmit || _busy) return;
    setState(() {
      _busy = true;
      _error = null;
      _notice = null;
    });
    try {
      // A device that has never had a vault is creating one here; `vaultOpen` does both, and
      // this is the only moment that can be told apart afterwards.
      final creating = !await vaultExists(vaultDir: widget.sync.paths.vaultDir);
      // The same directory the daemon shares (see main), so what arrives is what is opened.
      await vaultOpen(
        vaultDir: widget.sync.paths.vaultDir,
        cacheDbPath: widget.cacheDbPath,
        password: _password.text,
      );
      if (creating) {
        // A vault created right after a reset needs the shared folder back; the daemon is
        // already running by then, so nothing else would put it there.
        try {
          await widget.sync.ensureFolder();
        } catch (e) {
          debugPrint('could not register the shared folder: $e');
        }
        await _showFreshRecoveryCode();
      }
      await widget.onUnlocked();
    } catch (e) {
      // Core errors already arrive in the current language. The password is selected, so a
      // retry is typed straight over it rather than deleted first.
      setState(() => _error = '$e');
      _password.selection = TextSelection(baseOffset: 0, extentOffset: _password.text.length);
    } finally {
      if (mounted) setState(() => _busy = false);
    }
  }

  /// Issues the new vault's recovery code and shows it before the memo list ever appears.
  ///
  /// A vault whose password is lost on the day it was created is the case this exists for, so
  /// the code is put in front of the user at the one moment they are certainly paying
  /// attention. A vault without a code still works, so a failure here is reported and stepped
  /// over rather than blocking the app somebody just set up.
  Future<void> _showFreshRecoveryCode() async {
    try {
      final code = await vaultIssueRecoveryCode();
      if (!mounted) return;
      await showRecoveryCode(context, widget.strings, code);
    } catch (e) {
      debugPrint('could not issue a recovery code: $e');
    }
  }

  /// Sets a new password from the recovery code, then unlocks with it.
  ///
  /// Only the header is rewritten, so a wrong code costs one Argon2id run and leaves the
  /// vault exactly as it was.
  Future<void> _recover() async {
    if (_recoveryCode.text.isEmpty ||
        _tooShort(_recoveryPassword.text) ||
        _recoveryPassword.text != _recoveryConfirm.text ||
        _busy) {
      return;
    }
    setState(() {
      _busy = true;
      _error = null;
      _notice = null;
    });
    try {
      await vaultResetPasswordWithRecovery(
        vaultDir: widget.sync.paths.vaultDir,
        code: _recoveryCode.text,
        newPassword: _recoveryPassword.text,
      );
      await vaultOpen(
        vaultDir: widget.sync.paths.vaultDir,
        cacheDbPath: widget.cacheDbPath,
        password: _recoveryPassword.text,
      );
      _leaveRecovery();
      await widget.onUnlocked();
    } catch (e) {
      setState(() => _error = '$e');
    } finally {
      if (mounted) setState(() => _busy = false);
    }
  }

  /// Deletes this device's vault and cache: the last way out of a forgotten password.
  ///
  /// The unsharing that has to come first is the core's job (`vaultReset`), not this
  /// screen's — syncthing propagates deletions, and wiping a folder it still carries would
  /// take the other devices' memos with it. Both stored keys go too: they are the data key
  /// of a vault that no longer exists.
  Future<void> _reset() async {
    setState(() {
      _busy = true;
      _error = null;
      _notice = null;
    });
    try {
      await vaultReset(
        vaultDir: widget.sync.paths.vaultDir,
        cacheDbPath: widget.cacheDbPath,
      );
      await const SessionStore().clear();
      await const BiometricStore().disable();
      if (mounted) setState(() => _biometricReady = false);
      _leaveRecovery();
      await _probeVault();
      if (mounted) setState(() => _notice = widget.strings.resetDone);
    } catch (e) {
      if (mounted) setState(() => _error = '$e');
    } finally {
      if (mounted) setState(() => _busy = false);
    }
  }

  /// Closes the panel and empties it; a recovery code must not be left in a field.
  void _leaveRecovery() {
    _recoveryCode.clear();
    _recoveryPassword.clear();
    _recoveryConfirm.clear();
    if (mounted) {
      setState(() {
        _recovering = false;
        _confirmingReset = false;
      });
    }
  }

  @override
  Widget build(BuildContext context) {
    final s = widget.strings;
    return Scaffold(
      appBar: AppBar(
        // No title: the lock screen says what it is. The action is here so a fresh install
        // can pair before it has a vault to unlock.
        backgroundColor: Colors.transparent,
        actions: [SyncButton(strings: s, sync: widget.sync)],
      ),
      body: Center(
        // Scrollable, because the recovery panel plus a keyboard is taller than a phone.
        child: SingleChildScrollView(
          padding: EdgeInsets.fromLTRB(24, 24, 24, 24 + bottomInset(context)),
          child: Column(
            mainAxisSize: MainAxisSize.min,
            children: _recovering
                ? _recoveryPanel(s)
                : (!_vaultExists && _choosing)
                    ? _setupPanel(s)
                    : _passwordPanel(s),
          ),
        ),
      ),
    );
  }

  /// The first screen on a device with no vault: the two ways to start, each saying what
  /// pressing it will do.
  ///
  /// A card rather than a button, because the choice does not undo. Creating a vault on a
  /// device that should have been paired gives it a key of its own and the two never merge —
  /// so that warning belongs on the choice itself, not in a footnote under both.
  List<Widget> _setupPanel(FfiStrings s) => [
        _Wordmark(locked: _vaultExists),
        const SizedBox(height: 20),
        Text(s.setupQuestion, style: Theme.of(context).textTheme.titleSmall),
        const SizedBox(height: 12),
        _SetupChoice(
          title: s.setupNewTitle,
          detail: s.setupNewDetail,
          onTap: () => setState(() => _choosing = false),
        ),
        const SizedBox(height: 10),
        _SetupChoice(
          title: s.setupLinkTitle,
          detail: s.setupLinkDetail,
          // Pairing lives behind the app bar's button on this very screen; sending the user
          // there is the whole point of the card.
          onTap: () => SyncButton.open(context, widget.strings, widget.sync),
        ),
      ];

  /// The normal way in: type the password, or set one on a device with no vault yet.
  List<Widget> _passwordPanel(FfiStrings s) => [
        _Wordmark(locked: _vaultExists),
        const SizedBox(height: 16),
        if (!_vaultExists)
          Padding(
            padding: const EdgeInsets.only(bottom: 12),
            child: Text(
              s.newVaultHint,
              textAlign: TextAlign.center,
              style: Theme.of(context).textTheme.bodySmall,
            ),
          ),
        TextField(
          controller: _password,
          obscureText: !_reveal,
          decoration: InputDecoration(
            labelText: _vaultExists ? s.masterPassword : s.newPassword,
            suffixIcon: IconButton(
              icon: Icon(_reveal ? Icons.visibility_off : Icons.visibility),
              tooltip: _reveal ? s.hidePassword : s.showPassword,
              onPressed: () => setState(() => _reveal = !_reveal),
            ),
          ),
          textInputAction: _vaultExists ? TextInputAction.go : TextInputAction.next,
          onSubmitted: (_) => _unlock(),
          onChanged: (_) => setState(() {}),
        ),
        // Only when there is no vault yet: unlocking checks the password against the vault,
        // so a typo there costs one more try.
        if (!_vaultExists) ...[
          const SizedBox(height: 8),
          TextField(
            controller: _confirm,
            obscureText: !_reveal,
            decoration: InputDecoration(labelText: s.repeatPassword),
            onSubmitted: (_) => _unlock(),
            onChanged: (_) => setState(() {}),
          ),
          if (_password.text.isNotEmpty && _tooShort(_password.text))
            Padding(
              padding: const EdgeInsets.only(top: 8),
              child: Text(s.passwordTooShort, style: const TextStyle(color: Colors.red)),
            )
          else if (_confirm.text.isNotEmpty && _confirm.text != _password.text)
            Padding(
              padding: const EdgeInsets.only(top: 8),
              child: Text(s.repeatMismatch, style: const TextStyle(color: Colors.red)),
            ),
        ],
        if (_error != null)
          Padding(
            padding: const EdgeInsets.only(top: 8),
            child: Text(_error!, style: const TextStyle(color: Colors.red)),
          ),
        if (_notice != null)
          Padding(
            padding: const EdgeInsets.only(top: 8),
            child: Text(_notice!, textAlign: TextAlign.center),
          ),
        const SizedBox(height: 16),
        FilledButton(
          onPressed: _busy || !_canSubmit ? null : _unlock,
          child: Text(_busy
              ? s.opening
              : _vaultExists
                  ? s.unlock
                  : s.createVault),
        ),
        // Only once the prompt has been offered and dismissed: while it is still up, or on
        // the way to it, a second button for the same thing is just in the way.
        if (_biometricReady && _biometricTried)
          TextButton.icon(
            onPressed: _busy ? null : _unlockWithBiometrics,
            icon: const Icon(Icons.fingerprint),
            label: Text(s.biometricUnlock),
          ),
        // Nothing to recover before a vault exists, and offering it would only confuse.
        if (_vaultExists)
          TextButton(
            onPressed: _busy
                ? null
                : () => setState(() {
                      _recovering = true;
                      _error = null;
                      _notice = null;
                    }),
            child: Text(s.forgotPassword),
          ),
      ];

  /// The forgotten-password panel: the recovery code, or starting over.
  List<Widget> _recoveryPanel(FfiStrings s) => [
        Text(s.forgotPassword, style: Theme.of(context).textTheme.titleMedium),
        const SizedBox(height: 16),
        if (_hasRecovery) ...[
          Text(s.recoveryPrompt, style: Theme.of(context).textTheme.bodyMedium),
          const SizedBox(height: 12),
          TextField(
            controller: _recoveryCode,
            autocorrect: false,
            decoration: InputDecoration(labelText: s.recoveryCode),
            textInputAction: TextInputAction.next,
          ),
          const SizedBox(height: 8),
          TextField(
            controller: _recoveryPassword,
            obscureText: true,
            decoration: InputDecoration(labelText: s.newPassword),
            textInputAction: TextInputAction.next,
            onChanged: (_) => setState(() {}),
          ),
          const SizedBox(height: 8),
          // Typed twice, for the same reason the first-run screen asks twice: this password
          // is never checked against anything before it is used, and the header it rewrites
          // is synced — so a slip hands every device a password nobody knows. The recovery
          // code still works afterwards, which is the only reason this is a nuisance rather
          // than a disaster.
          TextField(
            controller: _recoveryConfirm,
            obscureText: true,
            decoration: InputDecoration(labelText: s.repeatPassword),
            onSubmitted: (_) => _recover(),
            onChanged: (_) => setState(() {}),
          ),
          if (_recoveryPassword.text.isNotEmpty && _tooShort(_recoveryPassword.text))
            Padding(
              padding: const EdgeInsets.only(top: 8),
              child: Text(s.passwordTooShort, style: const TextStyle(color: Colors.red)),
            )
          else if (_recoveryConfirm.text.isNotEmpty &&
              _recoveryConfirm.text != _recoveryPassword.text)
            Padding(
              padding: const EdgeInsets.only(top: 8),
              child: Text(s.repeatMismatch, style: const TextStyle(color: Colors.red)),
            ),
          const SizedBox(height: 12),
          FilledButton(
            onPressed: _busy ? null : _recover,
            child: Text(s.resetPassword),
          ),
        ] else
          Text(s.noRecovery, style: Theme.of(context).textTheme.bodyMedium),
        if (_error != null)
          Padding(
            padding: const EdgeInsets.only(top: 8),
            child: Text(_error!, style: const TextStyle(color: Colors.red)),
          ),
        const Divider(height: 40),
        Text(s.resetVaultHint, style: Theme.of(context).textTheme.bodySmall),
        const SizedBox(height: 12),
        // Two taps, and the second one says what it does. The first press only arms the
        // button; nothing is deleted until the confirmation is pressed.
        OutlinedButton(
          onPressed: _busy
              ? null
              : _confirmingReset
                  ? _reset
                  : () => setState(() => _confirmingReset = true),
          style: OutlinedButton.styleFrom(foregroundColor: Colors.red),
          child: Text(_confirmingReset ? s.resetVaultConfirm : s.resetVault),
        ),
        TextButton(
          onPressed: _busy ? null : _leaveRecovery,
          child: Text(s.cancel),
        ),
      ];
}

/// The app's name, with the padlock that says what it is for.
///
/// Material's bundled icon font, not a 🔒: an emoji is drawn by whatever the phone vendor
/// ships, and the desktop had to stop using them for the same reason.
class _Wordmark extends StatelessWidget {
  const _Wordmark({required this.locked});

  /// The padlock only once there is a vault to be locked: on a device setting up for the
  /// first time it said "locked" about nothing, the same thing the desktop's title said.
  final bool locked;

  @override
  Widget build(BuildContext context) => Row(
        mainAxisAlignment: MainAxisAlignment.center,
        children: [
          const Text('Ymemo', style: TextStyle(fontSize: 24)),
          if (locked) ...[
            const SizedBox(width: 8),
            const Icon(Icons.lock_outline, size: 22),
          ],
        ],
      );
}

/// One of the two ways to start on a fresh install: a heading and the sentence saying what
/// choosing it does. Mirrors `SetupChoice` in the desktop's theme.slint.
class _SetupChoice extends StatelessWidget {
  const _SetupChoice({required this.title, required this.detail, required this.onTap});

  final String title;
  final String detail;
  final VoidCallback onTap;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    return Material(
      color: scheme.surfaceContainerHighest,
      borderRadius: BorderRadius.circular(12),
      child: InkWell(
        borderRadius: BorderRadius.circular(12),
        onTap: onTap,
        child: Padding(
          padding: const EdgeInsets.all(14),
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              Text(title, style: Theme.of(context).textTheme.titleMedium),
              const SizedBox(height: 6),
              Text(detail, style: Theme.of(context).textTheme.bodySmall),
            ],
          ),
        ),
      ),
    );
  }
}
