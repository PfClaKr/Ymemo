// Ymemo mobile: lock screen, memo list, memo editor.
// `lib/src/rust/` is flutter_rust_bridge codegen output and is committed; see the README.
// After changing api.rs, run `flutter_rust_bridge_codegen generate` first.
//
// No strings are written here. They come from the **same catalog** as the desktop
// (i18n/*.json at the repo root) through `mobileStrings()`, so the UI never drifts from the
// language of the core's error messages. To add one, put a `mobile.*` key in ko.json and
// en.json and a field in FfiStrings in crates/ymemo-ffi; the ymemo-i18n tests check it.

import 'dart:async';
import 'dart:io' show Platform;
import 'dart:math' show max;
import 'dart:ui' as ui;

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:image_picker/image_picker.dart';
import 'package:mobile_scanner/mobile_scanner.dart';
import 'package:path_provider/path_provider.dart';

import 'src/rust/api.dart';
import 'home_widgets.dart' as widgets;
import 'host.dart' as host;
import 'palette.dart';
import 'security.dart';
import 'settings.dart';
import 'src/rust/frb_generated.dart';
import 'sync.dart';

Future<void> main() async {
  WidgetsFlutterBinding.ensureInitialized();
  await RustLib.init();

  // Android 15 draws every app edge to edge whether it asks to or not, so the system bars
  // sit *on top of* the UI. Asking for it explicitly makes older versions behave the same
  // way instead of leaving two layouts to reason about; `_bottomInset` below is what keeps
  // content out from under the gesture bar.
  await SystemChrome.setEnabledSystemUIMode(SystemUiMode.edgeToEdge);

  // Every path the app uses is derived here, once. The vault directory in particular is
  // shared between the two: it is what the daemon syncs and what the vault is opened from,
  // and two spellings of it would mean syncing one directory while reading another.
  final docs = await getApplicationDocumentsDirectory();

  // The core writes its failures to <docs>/ymemo.log from here on. Android sends a process's
  // stderr to /dev/null, so before this every `diag!` in Rust was simply lost on the one
  // platform a bug report comes from. Flutter's own errors go to the same file: `debugPrint`
  // is a swappable function pointer, and everything in the framework routes through it.
  await diagInit(dir: docs.path);
  final flutterPrint = debugPrint;
  debugPrint = (String? message, {int? wrapWidth}) {
    flutterPrint(message, wrapWidth: wrapWidth);
    if (message == null) return;
    // Swallowed on purpose: an error escaping here would be reported through `debugPrint`,
    // which is this function, and a log that cannot write would spin instead of failing
    // quietly. Framework errors need no separate hook — `presentError` prints through this.
    unawaited(diagLog(message: message).catchError((_) {}));
  };

  final settings = await SettingsStore.load('${docs.path}/settings.json');

  // Language before anything is drawn, so the core's error messages and the screens speak
  // the same one. "auto" is the system locale; an unknown value falls back to it anyway.
  await setLanguage(
    code: settings.value.lang == 'auto' ? Platform.localeName : settings.value.lang,
  );

  final sync = SyncController(
    SyncPaths(
      homeDir: '${docs.path}/syncthing',
      vaultDir: '${docs.path}/vault',
    ),
    readTiming: () => (
      watchDelaySeconds: settings.value.watchDelaySeconds,
      rescanSeconds: settings.value.rescanSeconds,
      keepVersionsDays: settings.value.keepVersionsDays,
    ),
    readWifiOnly: () => settings.value.wifiOnlySync,
  );

  runApp(YmemoApp(
    strings: await mobileStrings(),
    sync: sync,
    settings: settings,
    cacheDbPath: '${docs.path}/ymemo.db',
  ));

  // Not awaited: the daemon's first start generates a device key and takes seconds, and the
  // lock screen has nothing to wait for. It comes up **before unlocking** on purpose — a new
  // device pairs and receives vault.json first, otherwise unlocking would create a second
  // vault with a different salt that could never converge with the first.
  unawaited(sync.init());
}

/// The app, and the two things that outlive any single screen: whether the vault is open, and
/// the lifecycle watch that closes it when the app is left.
///
/// Lock state is held here rather than expressed by navigation, because locking has to be
/// able to happen while any screen is on top — including an editor pushed over the list.
class YmemoApp extends StatefulWidget {
  const YmemoApp({
    super.key,
    required this.strings,
    required this.sync,
    required this.settings,
    required this.cacheDbPath,
  });

  final FfiStrings strings;
  final SyncController sync;
  final SettingsStore settings;

  /// Device-local SQLite cache; rebuilt from the logs, never synced.
  final String cacheDbPath;

  @override
  State<YmemoApp> createState() => _YmemoAppState();
}

class _YmemoAppState extends State<YmemoApp> with WidgetsBindingObserver {
  final _navigator = GlobalKey<NavigatorState>();
  final _session = const SessionStore();

  late FfiStrings _strings = widget.strings;
  bool _unlocked = false;
  bool _restoring = true;

  @override
  void initState() {
    super.initState();
    WidgetsBinding.instance.addObserver(this);
    // The app-switcher thumbnail is taken as the app leaves, so the flag has to be on well
    // before that — not at the moment of leaving.
    host.setScreenshotBlock(widget.settings.value.lockOnBackground);
    // Before the first frame: a widget tap is what started the app in the first place, and
    // it has to be waiting when the list (or the lock screen ahead of it) comes up.
    unawaited(widgets.startWidgetRequests());
    _restoreSession();
  }

  @override
  void dispose() {
    WidgetsBinding.instance.removeObserver(this);
    super.dispose();
  }

  @override
  void didChangeAppLifecycleState(AppLifecycleState state) {
    final leaving = state == AppLifecycleState.paused ||
        state == AppLifecycleState.detached ||
        state == AppLifecycleState.hidden;
    if (leaving && _unlocked && widget.settings.value.lockOnBackground) {
      // The session is deliberately **kept**: this closes the vault so the memos are not
      // sitting open behind the app switcher, but it is not the user saying "ask me again".
      // Manual lock is what clears the session, exactly as on the desktop.
      _closeVault();
    }
  }

  /// Opens the vault straight away when a stored key is still valid, so "stay unlocked for N
  /// days" means what it says. Any failure — a diverged key, a keystore that will not decrypt
  /// — drops the session and falls back to the password.
  Future<void> _restoreSession() async {
    final session = await _session.read();
    if (session != null) {
      try {
        await vaultOpenWithKey(
          vaultDir: widget.sync.paths.vaultDir,
          cacheDbPath: widget.cacheDbPath,
          key: Uint8List.fromList(session.key),
        );
        if (mounted) setState(() => _unlocked = true);
      } catch (e) {
        debugPrint('stored key did not open the vault, asking for the password: $e');
        await _session.clear();
      }
    }
    if (mounted) setState(() => _restoring = false);
  }

  /// Switches the language everywhere at once: the core's messages and the screens come from
  /// the same catalog, so one re-read is the whole job.
  Future<void> _applyLanguage(String lang) async {
    await setLanguage(code: lang == 'auto' ? Platform.localeName : lang);
    final strings = await mobileStrings();
    if (mounted) setState(() => _strings = strings);
  }

  /// A password unlock succeeded: keep the key for as long as the settings allow.
  Future<void> _onUnlocked() async {
    try {
      await _session.write(await vaultKey(), widget.settings.value.unlockDays);
    } catch (e) {
      debugPrint('could not store the session key: $e');
    }
    setState(() => _unlocked = true);
  }

  /// The user asked to lock: close the vault **and** forget the key, or the lock button would
  /// mean nothing on the next start.
  Future<void> _lockNow() async {
    await _session.clear();
    await _closeVault();
  }

  Future<void> _closeVault() async {
    try {
      await vaultClose();
    } catch (e) {
      debugPrint('could not close the vault: $e');
    }
    // A locked app that left its memos spread across the home screen would not be locked.
    await widgets.hideWidgets();
    // Whatever was pushed over the list goes with it; an editor left on top would be showing
    // a memo from a vault that is no longer open.
    _navigator.currentState?.popUntil((route) => route.isFirst);
    if (mounted) setState(() => _unlocked = false);
  }

  @override
  Widget build(BuildContext context) {
    return MaterialApp(
      title: 'Ymemo',
      navigatorKey: _navigator,
      theme: ThemeData(
        colorScheme: ColorScheme.fromSeed(seedColor: const Color(0xFFE6D24A)),
        useMaterial3: true,
      ),
      home: _restoring
          // Brief: reading one key out of the keystore. Showing the lock screen first would
          // make an auto-unlock look like a password prompt that flashed past.
          ? const Scaffold(body: Center(child: CircularProgressIndicator()))
          : _unlocked
              ? MemoListScreen(
                  strings: _strings,
                  sync: widget.sync,
                  settings: widget.settings,
                  onLock: _lockNow,
                  onLanguageChanged: _applyLanguage,
                )
              : LockScreen(
                  strings: _strings,
                  sync: widget.sync,
                  settings: widget.settings,
                  cacheDbPath: widget.cacheDbPath,
                  onUnlocked: _onUnlocked,
                ),
    );
  }
}

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

  /// Recovery inputs, only built while the forgotten-password panel is open.
  final _recoveryCode = TextEditingController();
  final _recoveryPassword = TextEditingController();

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
    _recoveryCode.dispose();
    _recoveryPassword.dispose();
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

  Future<void> _unlock() async {
    if (_password.text.isEmpty || _busy) return;
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
      // Core errors already arrive in the current language.
      setState(() => _error = '$e');
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
    if (_recoveryCode.text.isEmpty || _recoveryPassword.text.isEmpty || _busy) return;
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
          padding: EdgeInsets.fromLTRB(24, 24, 24, 24 + _bottomInset(context)),
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
        const _Wordmark(),
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
        const _Wordmark(),
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
          obscureText: true,
          decoration: InputDecoration(
            labelText: _vaultExists ? s.masterPassword : s.newPassword,
          ),
          onSubmitted: (_) => _unlock(),
        ),
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
          onPressed: _busy ? null : _unlock,
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
            onSubmitted: (_) => _recover(),
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

/// Memo list for one folder.
///
/// Folders are navigated into rather than drawn as a tree: a phone has no room for indentation
/// and no hover to expand with, and drilling down also means the screen only ever asks the core
/// for one level, which no cycle can turn into an infinite walk.
class MemoListScreen extends StatefulWidget {
  const MemoListScreen({
    super.key,
    required this.strings,
    required this.sync,
    required this.settings,
    required this.onLock,
    required this.onLanguageChanged,
    this.groupId = '',
    this.groupName = '',
  });

  /// The folder being shown; empty is the top level.
  final String groupId;
  final String groupName;

  final FfiStrings strings;
  final SyncController sync;
  final SettingsStore settings;

  /// Manual lock: closes the vault and forgets the stored key.
  final Future<void> Function() onLock;

  /// Applying a language is app-wide, so the screen only asks for it.
  final Future<void> Function(String) onLanguageChanged;

  @override
  State<MemoListScreen> createState() => _MemoListScreenState();
}

class _MemoListScreenState extends State<MemoListScreen> {
  /// How often logs that have arrived are merged in — the settings screen's "pull
  /// interval", which was fixed at 15 seconds before it became one. The daemon delivers
  /// files whenever it likes; this is what turns them into memos on screen.
  ///
  /// Only half of how long a change takes to appear: the *other* device's watch delay comes
  /// first, and the two add up. Both are in Settings > Advanced.
  Duration get _mergeInterval =>
      Duration(seconds: widget.settings.value.mergeSeconds);

  List<FfiMemo> _memos = [];
  List<FfiGroup> _folders = [];
  Timer? _merge;
  FfiRelease? _update;

  /// What the vault is called, empty until it is named. It comes out of the synced document,
  /// so renaming it here renames it on every paired device.
  String _vaultName = '';

  bool get _atRoot => widget.groupId.isEmpty;

  @override
  void initState() {
    super.initState();
    _reload();
    _merge = Timer.periodic(_mergeInterval, (_) => _mergeNow());
    if (_atRoot) {
      _checkForUpdate(); // one banner, on the screen you always start from
      // Only the root screen answers widget taps: it is the one that is always there, and
      // a request that arrived while a folder was open is not about that folder.
      widgets.pendingWidgetRequest.addListener(_runWidgetRequest);
      // One may already be waiting — tapping a widget is often what opened the app.
      WidgetsBinding.instance.addPostFrameCallback((_) => _runWidgetRequest());
    }
  }

  /// Carries out what a tapped widget asked for, if anything is waiting.
  ///
  /// Whatever is stacked over the list belongs to the last thing the user did in the app,
  /// not to this, so it goes first: arriving from the home screen should look like arriving,
  /// not like landing on top of yesterday's editor.
  Future<void> _runWidgetRequest() async {
    final request = widgets.pendingWidgetRequest.value;
    if (request == null || !mounted) return;
    widgets.pendingWidgetRequest.value = null;
    Navigator.of(context).popUntil((route) => route.isFirst);
    switch (request.action) {
      case widgets.WidgetAction.openList:
        break; // this screen, already on it
      case widgets.WidgetAction.newMemo:
        await _add();
        break;
      case widgets.WidgetAction.newPhotoMemo:
        await _add(withPhoto: true);
        break;
      case widgets.WidgetAction.openMemo:
        await _openMemoById(request.id);
        break;
      case widgets.WidgetAction.openFolder:
        await _openFolderById(request.id);
        break;
    }
  }

  /// Opens a memo the widget named, wherever it is filed. Silently does nothing when it has
  /// been deleted since the snapshot was published, which a widget on another device can do.
  Future<void> _openMemoById(String id) async {
    for (final memo in await memoList()) {
      if (memo.id != id) continue;
      if (mounted) await _open(memo);
      return;
    }
  }

  Future<void> _openFolderById(String id) async {
    for (final folder in await groupList()) {
      if (folder.id != id) continue;
      if (mounted) await _openFolder(folder);
      return;
    }
  }

  /// Asks about a newer release at most once a day, and says nothing unless there is one —
  /// telling someone offline that they are offline is not worth a line of UI.
  Future<void> _checkForUpdate() async {
    if (!widget.settings.updateCheckDue) return;
    await widget.settings.markUpdateChecked();
    try {
      final release = await updateCheck();
      if (mounted) setState(() => _update = release);
    } catch (e) {
      debugPrint('update check failed: $e');
    }
  }

  @override
  void dispose() {
    _merge?.cancel();
    if (_atRoot) widgets.pendingWidgetRequest.removeListener(_runWidgetRequest);
    super.dispose();
  }

  Future<void> _reload() async {
    // Both come from the core, which lifts a cyclic or orphaned folder — and any memo whose
    // folder was deleted elsewhere — to the top level rather than leaving it unreachable.
    final folders = await groupChildren(parentId: widget.groupId);
    final memos = await memosInGroup(groupId: widget.groupId);
    // Re-read on every reload rather than once: a merge can bring a rename from another
    // device, and the heading is where that shows up.
    final name = _atRoot ? await vaultName() : '';
    if (mounted) {
      setState(() {
        _folders = folders;
        _memos = memos;
        _vaultName = name;
      });
    }
    // Every change to a memo or a folder comes back through here, so this is the one place
    // the home screen has to be told about. It skips the write when nothing moved, which is
    // what most of the merge timer's reloads are.
    unawaited(widgets.publishWidgets());
  }

  /// Renames the vault, on every device that shares it.
  Future<void> _renameVault() async {
    final s = widget.strings;
    final name =
        await _askForName(context, s, s.renameVault, _vaultName, label: s.vaultName);
    if (name == null) return;
    // The core trims it and cuts it to length, so show back what was actually stored.
    final stored = await vaultSetName(name: name);
    if (mounted) setState(() => _vaultName = stored);
  }

  /// Asks for a name and creates a folder inside the one on screen.
  Future<void> _newFolder() async {
    final name = await _askForName(context, widget.strings, widget.strings.newGroup, '');
    if (name == null || name.isEmpty) return;
    await groupCreate(name: name, parentId: widget.groupId);
    await _reload();
  }

  Future<void> _openFolder(FfiGroup folder) async {
    await Navigator.of(context).push(MaterialPageRoute(
      builder: (_) => MemoListScreen(
        strings: widget.strings,
        sync: widget.sync,
        settings: widget.settings,
        onLock: widget.onLock,
        onLanguageChanged: widget.onLanguageChanged,
        groupId: folder.id,
        groupName: folder.name,
      ),
    ));
    await _reload(); // it may have been renamed, emptied or filled while we were inside
  }

  /// Recolor, rename or delete, on a long press. Deleting keeps the contents and lifts them
  /// up a level, which the confirmation says out loud — "delete folder" reads like "delete
  /// the memos".
  Future<void> _folderMenu(FfiGroup folder) async {
    final s = widget.strings;
    final action = await showModalBottomSheet<String>(
      context: context,
      builder: (context) => SafeArea(
        child: Column(
          mainAxisSize: MainAxisSize.min,
          children: [
            _colorSection(context, s, folder.color),
            const Divider(height: 1),
            ListTile(
              leading: const Icon(Icons.drive_file_rename_outline),
              title: Text(s.rename),
              onTap: () => Navigator.of(context).pop('rename'),
            ),
            ListTile(
              leading: const Icon(Icons.delete_outline),
              title: Text(s.delete),
              subtitle: Text(s.deleteGroupHint),
              onTap: () => Navigator.of(context).pop('delete'),
            ),
          ],
        ),
      ),
    );
    if (!mounted || action == null) return;

    if (action.startsWith(_colorAction)) {
      // Folders carry the same palette key memos do and sync it the same way, so this shows
      // up on the desktop's tree as the color it was given here.
      await groupSetColor(id: folder.id, color: action.substring(_colorAction.length));
    } else if (action == 'rename') {
      final name = await _askForName(context, s, s.rename, folder.name);
      if (name != null && name.isNotEmpty) await groupRename(id: folder.id, name: name);
    } else if (action == 'delete') {
      await groupDelete(id: folder.id);
    }
    await _reload();
  }

  /// Recolor or move, on a long press. Deleting is the swipe, so it is not repeated here.
  Future<void> _memoMenu(FfiMemo memo) async {
    final s = widget.strings;
    final action = await showModalBottomSheet<String>(
      context: context,
      builder: (context) => SafeArea(
        child: Column(
          mainAxisSize: MainAxisSize.min,
          children: [
            _colorSection(context, s, memo.color),
            const Divider(height: 1),
            ListTile(
              leading: const Icon(Icons.drive_file_move_outline),
              title: Text(s.moveTo),
              onTap: () => Navigator.of(context).pop('move'),
            ),
          ],
        ),
      ),
    );
    if (!mounted || action == null) return;

    if (action.startsWith(_colorAction)) {
      await memoSetColor(id: memo.id, color: action.substring(_colorAction.length));
      await _reload();
    } else if (action == 'move') {
      await _moveMemo(memo);
    }
  }

  /// One row, wearing its palette key: a wash of the color behind it and a saturated stripe
  /// down the leading edge.
  ///
  /// The wash alone is too faint to separate at a glance once the list is long, and the
  /// stripe alone loses to the row's own text; together they are the signal the desktop's
  /// list gives, at a size a thumb scrolls past.
  Widget _tinted(BuildContext context, String color, Widget child) => Container(
        decoration: BoxDecoration(
          color: paletteRow(color, Theme.of(context).colorScheme.surface),
          border: Border(left: BorderSide(color: paletteSwatch(color), width: 5)),
        ),
        child: child,
      );

  /// The palette, at the top of both long-press sheets.
  ///
  /// Picking pops the sheet with the chosen key rather than writing from in here: the sheet's
  /// own context is gone the moment it closes, and one return value keeps every write in the
  /// caller, where the reload already is.
  Widget _colorSection(BuildContext sheetContext, FfiStrings s, String selected) => Padding(
        padding: const EdgeInsets.fromLTRB(16, 12, 16, 8),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Text(s.color, style: Theme.of(sheetContext).textTheme.labelLarge),
            const SizedBox(height: 4),
            ColorSwatches(
              selected: selected,
              onPick: (key) => Navigator.of(sheetContext).pop('$_colorAction$key'),
            ),
          ],
        ),
      );

  /// Moves a memo into another folder, chosen from a flat list of every folder there is.
  Future<void> _moveMemo(FfiMemo memo) async {
    final s = widget.strings;
    final folders = await groupList();
    if (!mounted) return;
    final target = await showModalBottomSheet<String>(
      context: context,
      builder: (context) => SafeArea(
        child: ListView(
          shrinkWrap: true,
          children: [
            ListTile(title: Text(s.moveTo), enabled: false),
            ListTile(
              leading: const Icon(Icons.home_outlined),
              title: Text(s.rootFolder),
              onTap: () => Navigator.of(context).pop(''),
            ),
            for (final folder in folders)
              ListTile(
                leading: const Icon(Icons.folder_outlined),
                title: Text(folder.name),
                onTap: () => Navigator.of(context).pop(folder.id),
              ),
          ],
        ),
      ),
    );
    if (!mounted || target == null) return;
    await memoSetGroup(id: memo.id, groupId: target);
    await _reload();
  }

  /// Folds in whatever the other devices have delivered, then redraws.
  Future<void> _mergeNow() async {
    try {
      await syncRebuild();
    } catch (e) {
      // A merge failure is not worth interrupting note-taking over; the next tick retries.
      debugPrint('merge failed: $e');
      return;
    }
    await _reload();
  }

  /// Creates an empty memo and opens the editor; fewer taps than asking for a title first.
  ///
  /// [withPhoto] goes straight on to the photo picker, which is what the camera button on
  /// the quick-write widget and the launcher shortcut of the same name are for.
  Future<void> _add({bool withPhoto = false}) async {
    final id = await memoUpsert(title: '', body: '');
    if (!_atRoot) await memoSetGroup(id: id, groupId: widget.groupId);
    if (!mounted) return;
    await Navigator.of(context).push(
      MaterialPageRoute(
        builder: (_) => MemoEditScreen(
          strings: widget.strings,
          id: id,
          title: '',
          body: '',
          color: defaultColor,
          pickPhotoOnOpen: withPhoto,
        ),
      ),
    );
    await _reload();
  }

  Future<void> _open(FfiMemo memo) async {
    await Navigator.of(context).push(
      MaterialPageRoute(
        builder: (_) => MemoEditScreen(
          strings: widget.strings,
          id: memo.id,
          title: memo.title,
          body: memo.body,
          color: memo.color,
        ),
      ),
    );
    await _reload();
  }

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      appBar: AppBar(
        // At the top level the title is the vault's name and tapping it renames it; inside a
        // folder it is the folder's, which is renamed from the folder's own menu.
        title: _atRoot
            ? InkWell(
                onTap: _renameVault,
                child: Padding(
                  padding: const EdgeInsets.symmetric(horizontal: 8, vertical: 4),
                  child: Text(
                    _vaultName.isEmpty ? widget.strings.listTitle : _vaultName,
                    overflow: TextOverflow.ellipsis,
                  ),
                ),
              )
            : Text(widget.groupName),
        actions: [
          IconButton(
            icon: const Icon(Icons.create_new_folder_outlined),
            tooltip: widget.strings.newGroup,
            onPressed: _newFolder,
          ),
          // Pairing, settings and the update banner belong to the screen you always start
          // from; merging is about content, so it stays available inside a folder too.
          if (_atRoot) SyncButton(strings: widget.strings, sync: widget.sync),
          IconButton(
            icon: const Icon(Icons.sync),
            tooltip: widget.strings.syncNow,
            // The timer does this every 15s; the button is for when you are waiting on a
            // memo you just wrote on the other device.
            onPressed: _mergeNow,
          ),
          if (_atRoot)
          IconButton(
            icon: const Icon(Icons.settings),
            tooltip: widget.strings.settings,
            onPressed: () async {
              await Navigator.of(context).push(MaterialPageRoute(
                builder: (_) => SettingsScreen(
                  strings: widget.strings,
                  settings: widget.settings,
                  sync: widget.sync,
                  vaultDir: widget.sync.paths.vaultDir,
                  onLock: widget.onLock,
                  onLanguageChanged: widget.onLanguageChanged,
                ),
              ));
              if (mounted) setState(() {}); // the language may have changed
            },
          ),
        ],
      ),
      body: Column(children: [
        if (_update != null)
          UpdateBanner(strings: widget.strings, release: _update!),
        if (_folders.isEmpty && _memos.isEmpty)
          Expanded(
            child: Center(
              child: Text(
                widget.strings.emptyFolder,
                style: Theme.of(context).textTheme.bodyMedium,
              ),
            ),
          )
        else
        Expanded(
          child: ListView.separated(
        // Room for the gesture bar and for the button floating above it, or the last memo
        // in the list is unreachable behind one or the other.
        padding: EdgeInsets.only(bottom: _bottomInset(context) + 88),
        // Folders first, then memos — the same order the desktop's tree draws them in.
        itemCount: _folders.length + _memos.length,
        // Two pastel rows of neighbouring colors have no edge between them, and two of the
        // same color have none at all; the rule is what keeps a long list countable.
        separatorBuilder: (_, __) => const Divider(height: 1, thickness: 1),
        itemBuilder: (context, i) {
          if (i < _folders.length) {
            final folder = _folders[i];
            return _tinted(
              context,
              folder.color,
              ListTile(
                leading: Icon(Icons.folder, color: paletteInk(folder.color)),
                title: Text(folder.name),
                onTap: () => _openFolder(folder),
                onLongPress: () => _folderMenu(folder),
              ),
            );
          }
          final memo = _memos[i - _folders.length];
          return Dismissible(
            key: ValueKey(memo.id),
            direction: DismissDirection.endToStart,
            onDismissed: (_) async {
              await memoDelete(id: memo.id);
              await _reload();
            },
            background: Container(
              color: Colors.red,
              alignment: Alignment.centerRight,
              padding: const EdgeInsets.only(right: 16),
              child: const Icon(Icons.delete, color: Colors.white),
            ),
            child: _tinted(
              context,
              memo.color,
              ListTile(
                title: Text(memo.title.isEmpty ? widget.strings.newMemo : memo.title),
                subtitle: memo.body.isEmpty
                    ? null
                    : Text(memo.body, maxLines: 1, overflow: TextOverflow.ellipsis),
                onTap: () => _open(memo),
                onLongPress: () => _memoMenu(memo),
              ),
            ),
          );
        },
          ),
        ),
      ]),
      floatingActionButton: Padding(
        // Scaffold lifts the button off the bottom of the *window*, which edge to edge puts
        // behind the gesture bar.
        padding: EdgeInsets.only(bottom: _bottomInset(context)),
        child: FloatingActionButton(
          onPressed: _add,
          child: const Icon(Icons.add),
        ),
      ),
    );
  }
}

/// Prefix a long-press sheet returns a chosen palette key under, so one `String?` result can
/// carry both "recolor to this" and the plain actions next to it.
const _colorAction = 'color:';

/// Height of the system navigation bar (or gesture pill) at the bottom of the screen.
///
/// Scrollables add it to their padding rather than being wrapped in a `SafeArea`: the
/// content still scrolls *under* the translucent bar, which is the point of edge to edge,
/// but the last row can be scrolled clear of it instead of ending up underneath.
double _bottomInset(BuildContext context) => MediaQuery.paddingOf(context).bottom;

/// Memo editor: title and body, saved on the way out.
class MemoEditScreen extends StatefulWidget {
  const MemoEditScreen({
    super.key,
    required this.strings,
    required this.id,
    required this.title,
    required this.body,
    required this.color,
    this.pickPhotoOnOpen = false,
  });

  final FfiStrings strings;
  final String id;
  final String title;
  final String body;

  /// Opens the photo picker as soon as the editor is up, for the two ways of starting a
  /// memo that are about a photo rather than about text.
  final bool pickPhotoOnOpen;

  /// Palette key the memo arrived with; the editor wears it the way the desktop's sticky
  /// does, so the same memo looks like the same memo on either device.
  final String color;

  @override
  State<MemoEditScreen> createState() => _MemoEditScreenState();
}

class _MemoEditScreenState extends State<MemoEditScreen> {
  late final TextEditingController _title = TextEditingController(text: widget.title);
  late final TextEditingController _body = TextEditingController(text: widget.body);
  late String _color = widget.color;
  List<FfiAttachment> _photos = [];

  /// Which photo shows its move/resize/detach handles. There is no hovering on a phone, so
  /// a photo has to be tapped before its controls appear; tapping the text puts them away.
  String? _selectedPhoto;

  /// Writes the new palette key straight through and repaints.
  ///
  /// Not batched into `_save` with the text: the color *is* what the screen looks like, and a
  /// swatch that did nothing until you left would read as a broken button.
  Future<void> _setColor(String color) async {
    setState(() => _color = color);
    await memoSetColor(id: widget.id, color: color);
  }

  Future<void> _pickColor() async {
    final chosen = await showModalBottomSheet<String>(
      context: context,
      builder: (context) => SafeArea(
        child: Padding(
          padding: const EdgeInsets.all(16),
          child: Column(
            mainAxisSize: MainAxisSize.min,
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              Text(widget.strings.color, style: Theme.of(context).textTheme.labelLarge),
              const SizedBox(height: 4),
              ColorSwatches(selected: _color, onPick: (key) => Navigator.of(context).pop(key)),
            ],
          ),
        ),
      ),
    );
    if (chosen != null) await _setColor(chosen);
  }

  @override
  void initState() {
    super.initState();
    _reloadPhotos();
    // After the first frame, so the picker's sheet opens over the editor rather than over
    // whatever was still on screen while it was being built.
    if (widget.pickPhotoOnOpen) {
      WidgetsBinding.instance.addPostFrameCallback((_) => _pickSource());
    }
  }

  Future<void> _reloadPhotos() async {
    final list = await attachmentList(memoId: widget.id);
    if (mounted) setState(() => _photos = list);
  }

  /// Picks one photo from the gallery or camera.
  ///
  /// The original bytes go to the core untouched — resizing is deliberately not done — but
  /// the **original pixel size is measured here**, because the core has no image decoder and
  /// that size sets the display aspect ratio.
  Future<void> _addPhoto(ImageSource source) async {
    final picked = await ImagePicker().pickImage(source: source);
    if (picked == null) return;
    final bytes = await picked.readAsBytes();
    final size = await _decodeSize(bytes);
    await attachmentAdd(
      memoId: widget.id,
      data: bytes,
      name: picked.name,
      mime: picked.mimeType ?? '',
      widthPx: size?.width.toInt() ?? 0,
      heightPx: size?.height.toInt() ?? 0,
    );
    final added = await attachmentList(memoId: widget.id);
    if (!mounted) return;
    // Select the new one: it has just landed somewhere on the note and moving it is the
    // next thing anyone does.
    setState(() {
      _photos = added;
      if (added.isNotEmpty) _selectedPhoto = added.last.id;
    });
  }

  /// Original pixel size, or null when decoding fails; the core then assumes 1:1.
  Future<ui.Size?> _decodeSize(Uint8List bytes) async {
    try {
      final codec = await ui.instantiateImageCodec(bytes);
      final frame = await codec.getNextFrame();
      final size = ui.Size(frame.image.width.toDouble(), frame.image.height.toDouble());
      frame.image.dispose();
      codec.dispose();
      return size;
    } catch (_) {
      return null;
    }
  }

  Future<void> _pickSource() async {
    final source = await showModalBottomSheet<ImageSource>(
      context: context,
      builder: (context) => SafeArea(
        child: Column(
          mainAxisSize: MainAxisSize.min,
          children: [
            ListTile(
              leading: const Icon(Icons.photo_library),
              title: Text(widget.strings.photoGallery),
              onTap: () => Navigator.pop(context, ImageSource.gallery),
            ),
            ListTile(
              leading: const Icon(Icons.photo_camera),
              title: Text(widget.strings.photoCamera),
              onTap: () => Navigator.pop(context, ImageSource.camera),
            ),
          ],
        ),
      ),
    );
    if (source != null) await _addPhoto(source);
  }

  @override
  void dispose() {
    _title.dispose();
    _body.dispose();
    super.dispose();
  }

  /// Skips the write when nothing changed; an empty change is pure sync traffic.
  Future<void> _save() async {
    if (_title.text == widget.title && _body.text == widget.body) return;
    await memoUpsert(id: widget.id, title: _title.text, body: _body.text);
  }

  @override
  Widget build(BuildContext context) {
    final base = Theme.of(context);
    final ink = paletteInk(_color);
    // Focus underlines, labels and the caret all come from `colorScheme.primary`, which is
    // the app's yellow accent — the one thing left on a blue or purple sticky that does not
    // belong to it. Swapped for the palette's own ink, for this screen only.
    final sticky = base.copyWith(
      colorScheme: base.colorScheme.copyWith(primary: ink),
      textSelectionTheme: TextSelectionThemeData(
        cursorColor: ink,
        selectionHandleColor: ink,
        selectionColor: ink.withValues(alpha: 0.3),
      ),
    );
    return Theme(
      data: sticky,
      child: PopScope(
      // Going back saves. There is a save button too, but saving never requires it.
      //
      // `canPop: false` and pop by hand **after** saving. With the default the route is
      // gone and the controllers disposed before this callback runs, so reading `_title.text`
      // throws "used after being disposed" and the save fails silently — a real bug seen on
      // the emulator, where going back lost the edit.
      canPop: false,
      onPopInvokedWithResult: (didPop, _) async {
        if (didPop) return;
        // Grab the navigator up front; context cannot be used across the await.
        final navigator = Navigator.of(context);
        await _save();
        navigator.pop();
      },
      child: Scaffold(
        backgroundColor: paletteBg(_color),
        appBar: AppBar(
          // The memo's own title, as the list shows it — the bar said "New memo" over every
          // memo ever opened, including ones written months ago. Fixed at the title it
          // arrived with rather than following the field below it, which is right there.
          title: Text(widget.title.isEmpty ? widget.strings.newMemo : widget.title),
          backgroundColor: paletteBar(_color),
          foregroundColor: ink,
          actions: [
            IconButton(
              icon: const Icon(Icons.palette_outlined),
              tooltip: widget.strings.color,
              onPressed: _pickColor,
            ),
            IconButton(
              icon: const Icon(Icons.add_photo_alternate),
              tooltip: widget.strings.addPhoto,
              onPressed: _pickSource,
            ),
            IconButton(
              icon: const Icon(Icons.check),
              tooltip: widget.strings.save,
              onPressed: () async {
                await _save();
                if (context.mounted) Navigator.of(context).pop();
              },
            ),
          ],
        ),
        body: Padding(
          padding: EdgeInsets.fromLTRB(16, 16, 16, 16 + _bottomInset(context)),
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.stretch,
            children: [
              TextField(
                controller: _title,
                decoration: InputDecoration(labelText: widget.strings.titleHint),
                textInputAction: TextInputAction.next,
              ),
              const SizedBox(height: 12),
              // The note itself: text underneath, photos lying on top of it wherever they
              // were dropped. One surface rather than a column of text followed by a column
              // of pictures — the same arrangement as the desktop sticky, and the position
              // each photo is given here travels to it.
              Expanded(
                child: LayoutBuilder(
                  builder: (context, box) {
                    final canvas = Size(box.maxWidth, box.maxHeight);
                    final baseFont = DefaultTextStyle.of(context).style.fontSize ?? 14.0;
                    return Stack(
                      children: [
                        Positioned.fill(
                          child: TextField(
                            controller: _body,
                            decoration: InputDecoration(
                              hintText: widget.strings.bodyHint,
                              border: InputBorder.none,
                            ),
                            maxLines: null,
                            expands: true,
                            textAlignVertical: TextAlignVertical.top,
                            // Writing puts the photo handles away; they would otherwise sit
                            // over the line being typed.
                            onTap: () => setState(() => _selectedPhoto = null),
                          ),
                        ),
                        for (final photo in _photos)
                          NotePhoto(
                            key: ValueKey(photo.id),
                            strings: widget.strings,
                            attachment: photo,
                            canvas: canvas,
                            baseFont: baseFont,
                            ink: ink,
                            selected: _selectedPhoto == photo.id,
                            onSelect: () => setState(() => _selectedPhoto = photo.id),
                            onChanged: _reloadPhotos,
                          ),
                      ],
                    );
                  },
                ),
              ),
            ],
          ),
        ),
      ),
      ),
    );
  }
}

/// Scans another device's pairing QR and registers it.
///
/// The core validates the format and does the registering (`syncPairWith`), so a change to
/// either leaves Dart alone. This is only ever half the job — the scanned device has to be
/// given this one's code too — which is what the message on the way out says.
class ScanScreen extends StatefulWidget {
  const ScanScreen({super.key, required this.strings, required this.sync});

  final FfiStrings strings;
  final SyncController sync;

  @override
  State<ScanScreen> createState() => _ScanScreenState();
}

/// App-bar button opening the sync screen, with the daemon's state on its face: a spinner
/// while it starts, a struck-through icon when there is nothing to sync with.
class SyncButton extends StatelessWidget {
  const SyncButton({super.key, required this.strings, required this.sync});

  final FfiStrings strings;
  final SyncController sync;

  @override
  Widget build(BuildContext context) {
    return ListenableBuilder(
      listenable: sync,
      builder: (context, _) {
        final Widget state;
        if (sync.starting) {
          state = const SizedBox(
            width: 18,
            height: 18,
            child: CircularProgressIndicator(strokeWidth: 2),
          );
        } else if (sync.running) {
          state = const Icon(Icons.devices);
        } else {
          state = const Icon(Icons.cloud_off);
        }
        // A request only reaches this device while the app is open, so it has to be visible
        // from the screen the user is already on rather than only inside the pairing screen.
        final icon = sync.pending.isEmpty
            ? state
            : Badge(label: Text('${sync.pending.length}'), child: state);
        return IconButton(
          icon: icon,
          tooltip: strings.syncDevices,
          onPressed: () => open(context, strings, sync),
        );
      },
    );
  }

  /// Opens the pairing screen. Shared with the first-run "connect to another device" card,
  /// so the two cannot drift into opening different things.
  static Future<void> open(
      BuildContext context, FfiStrings strings, SyncController sync) {
    return Navigator.of(context).push(
      MaterialPageRoute(builder: (_) => SyncScreen(strings: strings, sync: sync)),
    );
  }
}

/// The app's name, with the padlock that says what it is for.
///
/// Material's bundled icon font, not a 🔒: an emoji is drawn by whatever the phone vendor
/// ships, and the desktop had to stop using them for the same reason.
class _Wordmark extends StatelessWidget {
  const _Wordmark();

  @override
  Widget build(BuildContext context) => const Row(
        mainAxisAlignment: MainAxisAlignment.center,
        children: [
          Text('Ymemo', style: TextStyle(fontSize: 24)),
          SizedBox(width: 8),
          Icon(Icons.lock_outline, size: 22),
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

/// Pairing and the list of paired devices.
///
/// Pairing is **mutual**: adding the other device here only opens this side. Hence the code
/// at the top — the other device has to be given it, by QR from the desktop's window or by
/// typing it in. Until both sides have done their half, syncthing never connects the two.
class SyncScreen extends StatefulWidget {
  const SyncScreen({super.key, required this.strings, required this.sync});

  final FfiStrings strings;
  final SyncController sync;

  @override
  State<SyncScreen> createState() => _SyncScreenState();
}

class _SyncScreenState extends State<SyncScreen> {
  /// How often the shown code is re-read and incoming pairings are collected. The code
  /// rotates once a minute; this is only so the screen never shows a stale one.
  static const _lanPollInterval = Duration(seconds: 1);

  /// How often the device we asked is checked for having answered.
  static const _waitPollInterval = Duration(seconds: 2);

  /// Consecutive polls the peer must look connected before this is called a link.
  ///
  /// One is not enough: while the request is unanswered the peer's handshake completes and is
  /// *then* refused, so `connected` flickers true for a fraction of a second on every retry.
  /// Two polls two seconds apart never straddle that.
  static const _linkedPolls = 2;

  List<FfiSharedDevice> _devices = const [];

  final _lanInput = TextEditingController();
  String? _lanCode;
  String? _lanMessage;
  bool _joining = false;
  Timer? _lanPoll;

  /// The device this one scanned and is waiting to be allowed in by, with the eight
  /// characters its screen is showing. Null when nothing is outstanding.
  String? _waitingPeer;
  String? _waitingCode;
  int _connectedPolls = 0;
  Timer? _waitPoll;

  @override
  void initState() {
    super.initState();
    _reloadDevices();
    _startLan();
  }

  @override
  void dispose() {
    _lanPoll?.cancel();
    _waitPoll?.cancel();
    _lanInput.dispose();
    // Leaves pairing mode: closes the socket and drops the wifi multicast lock. Anything
    // still in flight is finished by the Rust side on its own thread.
    widget.sync.lanStop();
    super.dispose();
  }

  /// Enters pairing mode for as long as this screen is open.
  Future<void> _startLan() async {
    try {
      final code = await widget.sync.lanStart();
      if (!mounted) return;
      setState(() => _lanCode = code);
      if (code != null) {
        _lanPoll = Timer.periodic(_lanPollInterval, (_) => _pollLan());
      }
    } catch (e) {
      if (mounted) setState(() => _lanMessage = '$e');
    }
  }

  /// Refreshes the displayed code and picks up devices that used it. The Rust side has
  /// already registered them; this only has to say so and redraw the list.
  Future<void> _pollLan() async {
    try {
      final code = await widget.sync.lanCode();
      if (code == null) {
        // Backgrounding the app leaves pairing mode; coming back re-enters it. Null again
        // just means the daemon is not up yet, and the next tick tries once more.
        final restarted = await widget.sync.lanStart();
        if (mounted) setState(() => _lanCode = restarted);
        return;
      }
      final paired = await widget.sync.lanPollPaired();
      if (!mounted) return;
      setState(() {
        _lanCode = code;
        if (paired.isNotEmpty) _lanMessage = widget.strings.lanDone;
      });
      if (paired.isNotEmpty) await _reloadDevices();
    } catch (e) {
      debugPrint('lan poll failed: $e');
    }
  }

  /// Joiner side: broadcast for the device showing the typed code.
  Future<void> _joinLan() async {
    final code = _lanInput.text.trim();
    if (code.isEmpty || _joining) return;
    setState(() {
      _joining = true;
      _lanMessage = widget.strings.lanSearching;
    });
    try {
      final peer = await widget.sync.lanJoin(code);
      if (!mounted) return;
      setState(() {
        _lanMessage = peer == null ? widget.strings.lanNotFound : widget.strings.lanDone;
        if (peer != null) _lanInput.clear();
      });
      if (peer != null) await _reloadDevices();
    } catch (e) {
      // A malformed code is rejected by the core, in the user's language.
      if (mounted) setState(() => _lanMessage = '$e');
    } finally {
      if (mounted) setState(() => _joining = false);
    }
  }

  Future<void> _reloadDevices() async {
    try {
      final devices = await widget.sync.devices();
      if (mounted) setState(() => _devices = devices);
    } catch (e) {
      debugPrint('could not list devices: $e');
    }
  }

  Future<void> _copyCode() async {
    final code = widget.sync.pairingCode;
    if (code == null) return;
    await Clipboard.setData(ClipboardData(text: code));
    if (!mounted) return;
    ScaffoldMessenger.of(context).showSnackBar(
      SnackBar(content: Text(widget.strings.copied)),
    );
  }

  Future<void> _scan() async {
    final peer = await Navigator.of(context).push<String>(
      MaterialPageRoute(
        builder: (_) => ScanScreen(strings: widget.strings, sync: widget.sync),
      ),
    );
    if (peer != null) await _startWaiting(peer);
    await _reloadDevices();
  }

  /// Enters the waiting state for a peer that has just been registered.
  ///
  /// Scanning only did this device's half: it is dialling a device that has never heard of
  /// it, and nothing syncs until that device allows the request.
  Future<void> _startWaiting(String peer) async {
    String code = '';
    try {
      code = await widget.sync.verificationCode(peer);
    } catch (e) {
      // Without our own device id there is nothing to derive it from. The request still
      // works; only the code the user would compare is missing.
      debugPrint('could not derive the verification code: $e');
    }
    if (!mounted) return;
    setState(() {
      _waitingPeer = peer;
      _waitingCode = code;
      _connectedPolls = 0;
      _lanMessage = null;
    });
    _waitPoll?.cancel();
    _waitPoll = Timer.periodic(_waitPollInterval, (_) => _checkWaiting());
  }

  void _stopWaiting({String? message}) {
    _waitPoll?.cancel();
    _waitPoll = null;
    if (!mounted) return;
    setState(() {
      _waitingPeer = null;
      _waitingCode = null;
      _connectedPolls = 0;
      if (message != null) _lanMessage = message;
    });
  }

  /// Has the device we asked let us in yet?
  Future<void> _checkWaiting() async {
    final peer = _waitingPeer;
    if (peer == null) return;
    final devices = await widget.sync.devices();
    if (!mounted) return;
    final up = devices.any((d) => d.id == peer && d.connected);
    _connectedPolls = up ? _connectedPolls + 1 : 0;
    setState(() => _devices = devices);
    if (_connectedPolls >= _linkedPolls) {
      _stopWaiting(message: widget.strings.pairConnected);
    }
  }

  Future<void> _approve(FfiPendingDevice device) async {
    try {
      await widget.sync.approveDevice(device.id);
    } catch (e) {
      if (!mounted) return;
      ScaffoldMessenger.of(context).showSnackBar(SnackBar(content: Text('$e')));
    }
    await _reloadDevices();
  }

  Future<void> _reject(FfiPendingDevice device) async {
    try {
      await widget.sync.rejectDevice(device.id);
    } catch (e) {
      debugPrint('could not reject the request: $e');
    }
  }

  Future<void> _unpair(FfiSharedDevice device) async {
    try {
      await widget.sync.unpair(device.id);
    } catch (e) {
      if (!mounted) return;
      ScaffoldMessenger.of(context).showSnackBar(SnackBar(content: Text('$e')));
    }
    await _reloadDevices();
  }

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      appBar: AppBar(
        title: Text(widget.strings.syncDevices),
        actions: [
          IconButton(
            icon: const Icon(Icons.refresh),
            tooltip: widget.strings.syncNow,
            onPressed: _reloadDevices,
          ),
        ],
      ),
      body: ListenableBuilder(
        listenable: widget.sync,
        builder: (context, _) => ListView(
          padding: EdgeInsets.fromLTRB(16, 16, 16, 16 + _bottomInset(context)),
          children: [
            // Requests first: someone is standing at another device waiting for this tap.
            for (final request in widget.sync.pending) ...[
              _requestCard(context, request),
              const SizedBox(height: 12),
            ],
            _status(context),
            if (_waitingPeer != null) ...[
              const Divider(height: 32),
              _waitingSection(context),
            ],
            if (_lanCode != null || _lanMessage != null) ...[
              const Divider(height: 32),
              _lanSection(context),
            ],
            const Divider(height: 32),
            Text(widget.strings.syncDevices,
                style: Theme.of(context).textTheme.titleMedium),
            const SizedBox(height: 8),
            if (_devices.isEmpty)
              Text(widget.strings.noDevices,
                  style: Theme.of(context).textTheme.bodySmall)
            else
              for (final device in _devices)
                ListTile(
                  contentPadding: EdgeInsets.zero,
                  leading: Icon(
                    device.connected ? Icons.link : Icons.link_off,
                    color: device.connected ? Colors.green : null,
                  ),
                  title: Text(device.name.isEmpty ? device.id : device.name),
                  subtitle: Text(device.connected
                      ? widget.strings.connected
                      : widget.strings.disconnected),
                  trailing: IconButton(
                    icon: const Icon(Icons.delete_outline),
                    tooltip: widget.strings.unpair,
                    onPressed: () => _unpair(device),
                  ),
                ),
          ],
        ),
      ),
    );
  }

  /// Pairing over the local network: six digits instead of a 63-character device id.
  ///
  /// Both directions are offered because either device can be the one doing the typing, and
  /// whichever way round it goes, one exchange registers **both** sides.
  Widget _lanSection(BuildContext context) {
    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        Text(widget.strings.lanPairing, style: Theme.of(context).textTheme.titleMedium),
        if (_lanCode != null) ...[
          const SizedBox(height: 8),
          Text(widget.strings.lanMyCode, style: Theme.of(context).textTheme.bodySmall),
          const SizedBox(height: 4),
          Text(
            // Spaced out, because this gets read aloud across a room.
            _lanCode!.split('').join(' '),
            style: const TextStyle(fontSize: 30, letterSpacing: 2, fontFeatures: [ui.FontFeature.tabularFigures()]),
          ),
        ],
        const SizedBox(height: 12),
        Row(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Expanded(
              child: TextField(
                controller: _lanInput,
                keyboardType: TextInputType.number,
                maxLength: 6,
                decoration: InputDecoration(
                  labelText: widget.strings.lanEnterCode,
                  counterText: '',
                ),
                onSubmitted: (_) => _joinLan(),
              ),
            ),
            const SizedBox(width: 12),
            Padding(
              padding: const EdgeInsets.only(top: 8),
              child: FilledButton(
                onPressed: _joining ? null : _joinLan,
                child: Text(widget.strings.lanConnect),
              ),
            ),
          ],
        ),
        if (_lanMessage != null)
          Padding(
            padding: const EdgeInsets.only(top: 4),
            child: Text(_lanMessage!, style: Theme.of(context).textTheme.bodySmall),
          ),
      ],
    );
  }

  /// The top block: what the daemon is doing, and this device's code once it is up.
  /// One incoming request, with the comparison the user is being asked to make.
  ///
  /// A card rather than a dialog: a request can arrive at any moment, and a dialog thrown
  /// over whatever the user was doing is how people tap "allow" without reading it.
  Widget _requestCard(BuildContext context, FfiPendingDevice request) {
    final s = widget.strings;
    final scheme = Theme.of(context).colorScheme;
    return Card(
      color: scheme.secondaryContainer,
      margin: EdgeInsets.zero,
      child: Padding(
        padding: const EdgeInsets.all(16),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Row(
              children: [
                const Icon(Icons.device_unknown, size: 20),
                const SizedBox(width: 8),
                Text(s.pairRequest, style: Theme.of(context).textTheme.titleMedium),
              ],
            ),
            const SizedBox(height: 12),
            // The name is the asking device's own choice, so it never stands in for the id.
            if (request.name.isNotEmpty)
              Text(request.name, style: Theme.of(context).textTheme.bodyLarge),
            Text(s.deviceId, style: Theme.of(context).textTheme.labelSmall),
            SelectableText(
              request.id,
              style: const TextStyle(fontFamily: 'monospace', fontSize: 11),
            ),
            const SizedBox(height: 12),
            Text(s.pairVerify, style: Theme.of(context).textTheme.bodySmall),
            const SizedBox(height: 4),
            Center(
              child: Text(
                request.verificationCode,
                style: const TextStyle(
                  fontFamily: 'monospace',
                  fontSize: 28,
                  fontWeight: FontWeight.bold,
                  letterSpacing: 3,
                ),
              ),
            ),
            const SizedBox(height: 8),
            Text(s.pairRequestHint, style: Theme.of(context).textTheme.bodySmall),
            const SizedBox(height: 8),
            Row(
              mainAxisAlignment: MainAxisAlignment.end,
              children: [
                TextButton(
                  onPressed: () => _reject(request),
                  child: Text(s.reject),
                ),
                const SizedBox(width: 8),
                FilledButton(
                  onPressed: () => _approve(request),
                  child: Text(s.allow),
                ),
              ],
            ),
          ],
        ),
      ),
    );
  }

  /// The other side of the same moment: this device asked, and is waiting to be let in.
  Widget _waitingSection(BuildContext context) {
    final s = widget.strings;
    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        Row(
          children: [
            const SizedBox(
              width: 16, height: 16, child: CircularProgressIndicator(strokeWidth: 2)),
            const SizedBox(width: 12),
            Expanded(
              child: Text(s.pairWaiting, style: Theme.of(context).textTheme.titleMedium),
            ),
          ],
        ),
        const SizedBox(height: 8),
        Text(s.pairWaitingHint, style: Theme.of(context).textTheme.bodySmall),
        if ((_waitingCode ?? '').isNotEmpty) ...[
          const SizedBox(height: 12),
          Text(s.pairVerification, style: Theme.of(context).textTheme.labelSmall),
          Center(
            child: Text(
              _waitingCode!,
              style: const TextStyle(
                fontFamily: 'monospace',
                fontSize: 28,
                fontWeight: FontWeight.bold,
                letterSpacing: 3,
              ),
            ),
          ),
        ],
        const SizedBox(height: 8),
        Align(
          alignment: Alignment.centerRight,
          // The link itself is already registered and keeps retrying; this only takes the
          // panel down for someone who would rather not watch it.
          child: TextButton(
            onPressed: () => _stopWaiting(),
            child: Text(s.pairCancelWait),
          ),
        ),
      ],
    );
  }

  Widget _status(BuildContext context) {
    final sync = widget.sync;
    if (!sync.available) {
      return Text(widget.strings.syncUnavailable);
    }
    if (sync.starting) {
      return Row(
        children: [
          const SizedBox(width: 18, height: 18, child: CircularProgressIndicator(strokeWidth: 2)),
          const SizedBox(width: 12),
          Text(widget.strings.syncStarting),
        ],
      );
    }
    final code = sync.pairingCode;
    if (code == null) {
      // Started and failed: show the core's message rather than a bare "unavailable".
      return Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Text(sync.error ?? widget.strings.syncUnavailable,
              style: const TextStyle(color: Colors.red)),
          const SizedBox(height: 8),
          OutlinedButton(onPressed: sync.start, child: Text(widget.strings.syncNow)),
        ],
      );
    }
    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      children: [
        // A folder held back by "Wi-Fi only" looks exactly like sync being broken, so the
        // reason is said here rather than left to be guessed at.
        if (sync.pausedForMetered)
          Padding(
            padding: const EdgeInsets.only(bottom: 12),
            child: Row(children: [
              const Icon(Icons.pause_circle_outline, size: 18),
              const SizedBox(width: 8),
              Expanded(child: Text(widget.strings.pausedMetered)),
            ]),
          ),
        Text(widget.strings.myCode, style: Theme.of(context).textTheme.titleMedium),
        const SizedBox(height: 8),
        SelectableText(code, style: const TextStyle(fontFamily: 'monospace')),
        const SizedBox(height: 8),
        Wrap(
          spacing: 8,
          children: [
            OutlinedButton.icon(
              onPressed: _copyCode,
              icon: const Icon(Icons.copy, size: 18),
              label: Text(widget.strings.copy),
            ),
            FilledButton.icon(
              onPressed: _scan,
              icon: const Icon(Icons.qr_code_scanner, size: 18),
              label: Text(widget.strings.scanQr),
            ),
          ],
        ),
      ],
    );
  }
}

class _ScanScreenState extends State<ScanScreen> {
  final _controller = MobileScannerController(
    // Only pairing QRs matter, so other barcode formats are ignored: fewer false hits, less battery.
    formats: const [BarcodeFormat.qrCode],
    detectionSpeed: DetectionSpeed.noDuplicates,
  );
  // Stop after the first hit; the camera keeps streaming the same code.
  bool _handled = false;

  @override
  void dispose() {
    _controller.dispose();
    super.dispose();
  }

  Future<void> _onDetect(BarcodeCapture capture) async {
    if (_handled) return;
    final raw = capture.barcodes
        .map((b) => b.rawValue)
        .firstWhere((v) => v != null && v.isNotEmpty, orElse: () => null);
    if (raw == null) return;
    _handled = true;

    final messenger = ScaffoldMessenger.of(context);
    final navigator = Navigator.of(context);
    try {
      final peer = await widget.sync.pairWith(raw);
      await _controller.stop();
      // Pops with the peer id: this device is now dialling one that has never heard of it,
      // and the screen underneath turns that into "waiting to be allowed in".
      navigator.pop(peer);
    } catch (e) {
      // Show the core's message as-is and allow another scan.
      messenger.showSnackBar(SnackBar(content: Text('$e')));
      _handled = false;
    }
  }

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      appBar: AppBar(title: Text(widget.strings.scanQr)),
      body: Stack(
        fit: StackFit.expand,
        children: [
          MobileScanner(
            controller: _controller,
            onDetect: _onDetect,
            // A camera that cannot open (denied, or absent) must not leave a blank screen.
            errorBuilder: (context, error) => Center(
              child: Padding(
                padding: const EdgeInsets.all(24),
                child: Text(
                  '${widget.strings.cameraError}\n\n${error.errorCode.name}',
                  textAlign: TextAlign.center,
                ),
              ),
            ),
          ),
          Align(
            alignment: Alignment.bottomCenter,
            child: Container(
              width: double.infinity,
              color: Colors.black54,
              padding: const EdgeInsets.symmetric(vertical: 16, horizontal: 24),
              child: Text(
                widget.strings.scanHint,
                textAlign: TextAlign.center,
                style: const TextStyle(color: Colors.white),
              ),
            ),
          ),
        ],
      ),
    );
  }
}

/// One photo lying on the note: drag it anywhere, pull its corner to resize, ✕ to detach.
///
/// Nothing is written until the finger lifts — a drag would otherwise leave one entry in the
/// change log per frame. Both numbers that get written are platform-independent: the width
/// in **em**, multiples of the body font, and the position as a **fraction of the note**. A
/// photo half way down a phone screen is half way down the desktop sticky as well.
class NotePhoto extends StatefulWidget {
  const NotePhoto({
    super.key,
    required this.strings,
    required this.attachment,
    required this.canvas,
    required this.baseFont,
    required this.ink,
    required this.selected,
    required this.onSelect,
    required this.onChanged,
  });

  final FfiStrings strings;
  final FfiAttachment attachment;

  /// Size of the note the photo lies on; positions are a fraction of it.
  final Size canvas;

  /// This platform's body font size, which the stored width in em is measured against.
  final double baseFont;
  final Color ink;
  final bool selected;
  final VoidCallback onSelect;
  final Future<void> Function() onChanged;

  @override
  State<NotePhoto> createState() => _NotePhotoState();
}

class _NotePhotoState extends State<NotePhoto> {
  /// Smallest a photo may be pulled; below this the handles cover the picture.
  static const double _minW = 44;
  static const double _handle = 30;

  Uint8List? _bytes;
  bool _missing = false;

  /// Live drag offsets, folded into the stored geometry when the finger lifts.
  double _dx = 0;
  double _dy = 0;
  double _dw = 0;

  @override
  void initState() {
    super.initState();
    _load();
  }

  Future<void> _load() async {
    // Before it syncs there are no bytes; say so rather than showing nothing.
    if (!await attachmentHasBlob(hash: widget.attachment.hash)) {
      if (mounted) setState(() => _missing = true);
      return;
    }
    final bytes = await attachmentBytes(hash: widget.attachment.hash);
    if (mounted) setState(() => _bytes = bytes);
  }

  double get _w {
    final stored = widget.attachment.widthEmMilli / 1000.0 * widget.baseFont;
    return (stored + _dw).clamp(_minW, max(widget.canvas.width, _minW));
  }

  double get _h {
    final a = widget.attachment;
    final ratio = (a.widthPx > 0 && a.heightPx > 0) ? a.heightPx / a.widthPx : 1.0;
    return _w * ratio;
  }

  // Never fully off the note: a photo whose corner cannot be reached cannot be brought back.
  double get _x => (widget.attachment.xPermille / 1000.0 * widget.canvas.width + _dx)
      .clamp(0.0, max(widget.canvas.width - _w, 0.0));
  double get _y => (widget.attachment.yPermille / 1000.0 * widget.canvas.height + _dy)
      .clamp(0.0, max(widget.canvas.height - _h, 0.0));

  /// Stores where the photo ended up. The clamped geometry is what is read back, so what is
  /// saved is where the photo actually is and not where the finger went.
  Future<void> _commit() async {
    await attachmentSetLayout(
      id: widget.attachment.id,
      xPermille: (_x / max(widget.canvas.width, 1) * 1000).round(),
      yPermille: (_y / max(widget.canvas.height, 1) * 1000).round(),
      widthEmMilli: (_w / widget.baseFont * 1000).round(),
    );
    // Cleared without a setState of their own: reloading rebuilds this widget with the
    // stored values the offsets have just been folded into, and clearing them separately
    // would show the old position for one frame.
    _dx = 0;
    _dy = 0;
    _dw = 0;
    await widget.onChanged();
  }

  @override
  Widget build(BuildContext context) {
    final selected = widget.selected;
    return Positioned(
      left: _x,
      top: _y,
      width: _w,
      height: _h,
      child: Stack(
        clipBehavior: Clip.none,
        children: [
          GestureDetector(
            onTap: widget.onSelect,
            onPanStart: (_) => widget.onSelect(),
            onPanUpdate: (d) => setState(() {
              _dx += d.delta.dx;
              _dy += d.delta.dy;
            }),
            onPanEnd: (_) => _commit(),
            child: Container(
              width: _w,
              height: _h,
              decoration: BoxDecoration(
                borderRadius: BorderRadius.circular(6),
                border: Border.all(
                  color: selected ? widget.ink : widget.ink.withValues(alpha: 0.35),
                  width: selected ? 2 : 1,
                ),
              ),
              clipBehavior: Clip.antiAlias,
              child: _picture(),
            ),
          ),
          if (selected) ..._furniture(),
        ],
      ),
    );
  }

  Widget _picture() {
    if (_missing) {
      return Center(
        child: Padding(
          padding: const EdgeInsets.all(4),
          child: Text(
            widget.strings.photoMissing,
            textAlign: TextAlign.center,
            style: Theme.of(context).textTheme.bodySmall,
          ),
        ),
      );
    }
    if (_bytes == null) {
      return const Center(child: CircularProgressIndicator());
    }
    return Image.memory(_bytes!, fit: BoxFit.cover);
  }

  /// Detach and resize, shown only on the selected photo so the picture is not permanently
  /// covered by its own controls. They hang half outside the frame, where a fingertip
  /// reaches them without hiding what it is about to change.
  List<Widget> _furniture() => [
        Positioned(
          right: -_handle / 3,
          top: -_handle / 3,
          child: Semantics(
            label: widget.strings.photoRemove,
            button: true,
            child: GestureDetector(
              onTap: () async {
                await attachmentRemove(id: widget.attachment.id);
                await widget.onChanged();
              },
              child: _chip(const Color(0xFFD64541), Icons.close),
            ),
          ),
        ),
        Positioned(
          right: -_handle / 3,
          bottom: -_handle / 3,
          child: Semantics(
            label: widget.strings.photoSize,
            child: GestureDetector(
              // Width only; the height follows the original aspect ratio.
              onPanUpdate: (d) => setState(() => _dw += d.delta.dx),
              onPanEnd: (_) => _commit(),
              child: _chip(widget.ink, Icons.open_in_full),
            ),
          ),
        ),
      ];

  Widget _chip(Color background, IconData icon) => Container(
        width: _handle,
        height: _handle,
        decoration: BoxDecoration(color: background, shape: BoxShape.circle),
        child: Icon(icon, size: 16, color: Colors.white),
      );
}

/// Asks for a folder name, pre-filled when renaming. Null when the user backs out.
Future<String?> _askForName(
  BuildContext context,
  FfiStrings strings,
  String title,
  String initial, {
  /// What the field is for. Folders are what this dialog was written for, so that stays the
  /// default; the vault's own name goes through it too and must not be labelled a folder.
  String? label,
}) {
  final controller = TextEditingController(text: initial);
  return showDialog<String>(
    context: context,
    builder: (context) => AlertDialog(
      title: Text(title),
      content: TextField(
        controller: controller,
        autofocus: true,
        decoration: InputDecoration(labelText: label ?? strings.folderName),
        onSubmitted: (v) => Navigator.of(context).pop(v.trim()),
      ),
      actions: [
        TextButton(
          onPressed: () => Navigator.of(context).pop(),
          child: Text(strings.cancel),
        ),
        FilledButton(
          onPressed: () => Navigator.of(context).pop(controller.text.trim()),
          child: Text(strings.ok),
        ),
      ],
    ),
  );
}

/// One line saying a newer release exists, above the memo list.
///
/// Never a dialog and never dismissible-by-accident: it is information, and the app it sits
/// on top of is for writing memos, not for updating itself.
class UpdateBanner extends StatelessWidget {
  const UpdateBanner({super.key, required this.strings, required this.release});

  final FfiStrings strings;
  final FfiRelease release;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    return Material(
      color: scheme.secondaryContainer,
      child: InkWell(
        onTap: () => host.openUrl(release.url),
        child: Padding(
          padding: const EdgeInsets.symmetric(horizontal: 16, vertical: 10),
          child: Row(
            children: [
              const Icon(Icons.system_update, size: 18),
              const SizedBox(width: 8),
              Expanded(child: Text('${strings.updateAvailable} ${release.version}')),
              Text(
                strings.updateOpen,
                style: TextStyle(color: scheme.primary, fontSize: 12),
              ),
            ],
          ),
        ),
      ),
    );
  }
}



/// What the last update check concluded; the text for it is built at draw time.
enum _UpdateState { idle, checking, latest, found, failed }

/// Device-local preferences: language, locking, updates.
///
/// Everything applies as it is changed — a phone settings screen with a save button is a
/// phone settings screen someone will leave without pressing it. Rust sanitizes on write, and
/// what comes back is what is shown, so an impossible value cannot sit here looking accepted.
class SettingsScreen extends StatefulWidget {
  const SettingsScreen({
    super.key,
    required this.strings,
    required this.settings,
    required this.sync,
    required this.vaultDir,
    required this.onLock,
    required this.onLanguageChanged,
  });

  final FfiStrings strings;
  final SettingsStore settings;

  /// Only for the Wi-Fi switch: flipping it has to reach the running daemon now.
  final SyncController sync;

  /// Passed through to the security screen, which asks `vault.json` itself whether a
  /// recovery code exists.
  final String vaultDir;

  final Future<void> Function() onLock;
  final Future<void> Function(String) onLanguageChanged;

  @override
  State<SettingsScreen> createState() => _SettingsScreenState();
}

class _SettingsScreenState extends State<SettingsScreen> {
  /// Offered stay-unlocked periods. A free-text number field would be worse in every way:
  /// harder to tap and able to produce values Rust would only clamp away again.
  static const _dayChoices = [0, 1, 7, 30, 90, 365];

  /// The advanced timings, same reasoning. Each list starts at what the core clamps to and
  /// ends where going further stops being useful.
  static const _mergeChoices = [3, 5, 10, 15, 30, 60, 300];
  static const _watchChoices = [1, 2, 5, 10, 20, 60];
  static const _rescanChoices = [60, 300, 900, 3600];

  /// Retention is in days rather than seconds, and 0 is a real answer: keep nothing.
  static const _keepChoices = [0, 1, 7, 30, 90, 365];

  /// What the last check concluded. Kept as state rather than as a finished sentence: a
  /// rendered string would still be in the old language after the language is changed.
  _UpdateState _updateState = _UpdateState.idle;
  String? _updateError;
  FfiRelease? _update;
  bool _checking = false;

  FfiSettings get _s => widget.settings.value;

  /// Writes one changed field and redraws with whatever Rust kept.
  Future<void> _save({
    String? lang,
    int? unlockDays,
    bool? lockOnBackground,
    bool? biometricUnlock,
    bool? updateCheck,
    int? mergeSeconds,
    int? watchDelaySeconds,
    int? rescanSeconds,
    int? keepVersionsDays,
    bool? wifiOnlySync,
  }) async {
    await widget.settings.save(FfiSettings(
      lang: lang ?? _s.lang,
      unlockDays: unlockDays ?? _s.unlockDays,
      lockOnBackground: lockOnBackground ?? _s.lockOnBackground,
      biometricUnlock: biometricUnlock ?? _s.biometricUnlock,
      updateCheck: updateCheck ?? _s.updateCheck,
      mergeSeconds: mergeSeconds ?? _s.mergeSeconds,
      watchDelaySeconds: watchDelaySeconds ?? _s.watchDelaySeconds,
      rescanSeconds: rescanSeconds ?? _s.rescanSeconds,
      keepVersionsDays: keepVersionsDays ?? _s.keepVersionsDays,
      wifiOnlySync: wifiOnlySync ?? _s.wifiOnlySync,
      lastUpdateCheck: _s.lastUpdateCheck,
    ));
    // Flipping the switch has to take effect now, not at the next daemon start.
    if (wifiOnlySync != null) {
      await widget.sync.applyNetworkPolicy();
    }
    // The watch delay is Syncthing's, not ours, so saving has to push it across. It is a
    // no-op while the daemon is down; sync.dart applies it again when it comes up.
    if (watchDelaySeconds != null || rescanSeconds != null) {
      try {
        await syncSetTiming(
          watchDelaySeconds: _s.watchDelaySeconds,
          rescanSeconds: _s.rescanSeconds,
        );
      } catch (e) {
        debugPrint('could not apply the sync timing: $e');
      }
    }
    if (keepVersionsDays != null) {
      try {
        await syncSetVersioning(keepDays: _s.keepVersionsDays);
      } catch (e) {
        debugPrint('could not apply the version retention: $e');
      }
    }
    // One switch, two protections: closing the vault and keeping the memos out of the app
    // switcher. Someone who turned it off chose convenience, and hiding their thumbnail
    // anyway would be deciding for them.
    if (lockOnBackground != null) {
      await host.setScreenshotBlock(lockOnBackground);
    }
    if (!mounted) return;
    setState(() {});
    ScaffoldMessenger.of(context).showSnackBar(
      SnackBar(content: Text(widget.strings.saved), duration: const Duration(seconds: 1)),
    );
  }

  Future<void> _setLanguage(String lang) async {
    await _save(lang: lang);
    await widget.onLanguageChanged(lang);
  }

  /// Shortening the stay-unlocked window has to invalidate the key already stored under the
  /// old one, or the setting would be a suggestion rather than a rule.
  Future<void> _setUnlockDays(int days) async {
    await _save(unlockDays: days);
    await const SessionStore().clear();
  }

  /// Turns biometric unlock on or off.
  ///
  /// Turning it **on** is the moment the key is stored, and it can only happen from here:
  /// this screen is inside the unlocked app, so there is a key to store. The fingerprint is
  /// checked first — not for security, since the vault is already open, but so that a switch
  /// that cannot work never ends up looking on.
  ///
  /// Turning it **off** deletes the key, which is the entire promise of the switch.
  Future<void> _setBiometric(bool on) async {
    const store = BiometricStore();
    if (!on) {
      await store.disable();
      await _save(biometricUnlock: false);
      return;
    }
    if (!await store.available) {
      if (mounted) _say(widget.strings.biometricUnavailable);
      return;
    }
    if (!await store.confirm(
      widget.strings.biometricUnlock,
      title: widget.strings.biometricPrompt,
      cancel: widget.strings.cancel,
    )) {
      if (mounted) _say(widget.strings.biometricFailed);
      return;
    }
    try {
      await store.enable(await vaultKey());
    } catch (e) {
      debugPrint('could not store the biometric key: $e');
      if (mounted) _say('$e');
      return;
    }
    await _save(biometricUnlock: true);
  }

  /// One "N seconds" dropdown. A value outside the offered list — a hand-edited
  /// settings.json, or a list that shrank between versions — still shows, rather than
  /// snapping to something the user never chose.
  Widget _seconds(String label, String hint, List<int> choices, int value,
      void Function(int) onPick) {
    // A growable copy, always: `..sort()` binds to the whole conditional, so returning the
    // const list on the common path and sorting it threw "cannot modify an unmodifiable list".
    final items = [...choices];
    if (!items.contains(value)) {
      items.add(value);
      items.sort();
    }
    return ListTile(
      title: Text(label),
      subtitle: Text(hint),
      trailing: DropdownButton<int>(
        value: value,
        onChanged: (v) => v == null ? null : onPick(v),
        items: [
          for (final n in items)
            DropdownMenuItem(value: n, child: Text('$n ${widget.strings.secondsUnit}')),
        ],
      ),
    );
  }

  /// Shows the tail of the problem log, with one button that puts it on the clipboard —
  /// which is the whole point: a bug report someone can paste rather than describe.
  Future<void> _showLog() async {
    final s = widget.strings;
    final text = await diagTail(maxBytes: 64 * 1024);
    if (!mounted) return;
    await showDialog<void>(
      context: context,
      builder: (context) => AlertDialog(
        title: Text(s.log),
        content: SizedBox(
          width: double.maxFinite,
          child: SingleChildScrollView(
            child: SelectableText(
              text.isEmpty ? s.logEmpty : text,
              style: const TextStyle(fontFamily: 'monospace', fontSize: 11),
            ),
          ),
        ),
        actions: [
          if (text.isNotEmpty)
            TextButton(
              onPressed: () async {
                await Clipboard.setData(ClipboardData(text: text));
                if (context.mounted) Navigator.pop(context);
                if (mounted) _say(s.copied);
              },
              child: Text(s.copy),
            ),
          TextButton(onPressed: () => Navigator.pop(context), child: Text(s.ok)),
        ],
      ),
    );
  }

  void _say(String message) => ScaffoldMessenger.of(context).showSnackBar(
        SnackBar(content: Text(message), duration: const Duration(seconds: 2)),
      );

  Future<void> _checkNow() async {
    setState(() {
      _checking = true;
      _updateState = _UpdateState.checking;
    });
    await widget.settings.markUpdateChecked();
    try {
      final release = await updateCheck();
      if (!mounted) return;
      setState(() {
        _update = release;
        _updateState = release == null ? _UpdateState.latest : _UpdateState.found;
      });
    } catch (e) {
      // The core's message, already in the language it was raised in.
      if (mounted) {
        setState(() {
          _updateError = '$e';
          _updateState = _UpdateState.failed;
        });
      }
    } finally {
      if (mounted) setState(() => _checking = false);
    }
  }

  /// The status line, built from the state so it follows the language.
  String? get _updateStatus => switch (_updateState) {
        _UpdateState.idle => null,
        _UpdateState.checking => widget.strings.updateChecking,
        _UpdateState.latest => widget.strings.updateLatest,
        _UpdateState.found => '${widget.strings.updateAvailable} ${_update?.version ?? ''}',
        _UpdateState.failed => _updateError,
      };

  @override
  Widget build(BuildContext context) {
    final s = widget.strings;
    return Scaffold(
      appBar: AppBar(title: Text(s.settings)),
      body: ListView(
        padding: EdgeInsets.fromLTRB(0, 8, 0, 8 + _bottomInset(context)),
        children: [
          _header(s.language),
          // Language names stay untranslated: written in their own language they are findable
          // even by someone stuck in one they cannot read.
          RadioGroup<String>(
            groupValue: _s.lang,
            onChanged: (v) => v == null ? null : _setLanguage(v),
            child: Column(children: [
              RadioListTile<String>(value: 'auto', title: Text(s.languageAuto)),
              const RadioListTile<String>(value: 'ko', title: Text('한국어')),
              const RadioListTile<String>(value: 'en', title: Text('English')),
            ]),
          ),

          const Divider(),
          _header(s.lockSection),
          SwitchListTile(
            value: _s.lockOnBackground,
            onChanged: (v) => _save(lockOnBackground: v),
            title: Text(s.lockOnBackground),
            subtitle: Text(s.lockOnBackgroundHint),
          ),
          SwitchListTile(
            value: _s.biometricUnlock,
            onChanged: _setBiometric,
            title: Text(s.biometricUnlock),
            subtitle: Text(s.biometricUnlockHint),
          ),
          ListTile(
            title: Text(s.unlockDays),
            subtitle: Text(s.unlockDaysHint),
            trailing: DropdownButton<int>(
              value: _dayChoices.contains(_s.unlockDays) ? _s.unlockDays : 0,
              onChanged: (v) => v == null ? null : _setUnlockDays(v),
              items: [
                for (final days in _dayChoices)
                  DropdownMenuItem(
                    value: days,
                    child: Text(days == 0 ? '0' : '$days ${s.daysUnit}'),
                  ),
              ],
            ),
          ),
          ListTile(
            leading: const Icon(Icons.lock_outline),
            title: Text(s.lockNow),
            // No pop here. Locking already pops back to the root and swaps in the lock
            // screen; popping again would take the root with it and leave a black screen.
            onTap: widget.onLock,
          ),

          const Divider(),
          _header(s.securitySection),
          ListTile(
            leading: const Icon(Icons.password),
            title: Text(s.changePassword),
            subtitle: Text(s.recoveryCode),
            trailing: const Icon(Icons.chevron_right),
            onTap: () => Navigator.of(context).push(MaterialPageRoute(
              builder: (_) => SecurityScreen(
                strings: widget.strings,
                vaultDir: widget.vaultDir,
              ),
            )),
          ),

          const Divider(),
          _header(s.advanced),
          Padding(
            padding: const EdgeInsets.fromLTRB(16, 0, 16, 8),
            child: Text(s.advancedHint, style: Theme.of(context).textTheme.bodySmall),
          ),
          // The three together are what decides how fast a change appears: the sending
          // device's watch delay plus the receiving one's pull interval. Splitting them
          // across the screen would hide that they add up.
          _seconds(s.watchDelay, s.watchDelayHint, _watchChoices, _s.watchDelaySeconds,
              (v) => _save(watchDelaySeconds: v)),
          _seconds(s.mergeSeconds, s.mergeSecondsHint, _mergeChoices, _s.mergeSeconds,
              (v) => _save(mergeSeconds: v)),
          _seconds(s.rescan, s.rescanHint, _rescanChoices, _s.rescanSeconds,
              (v) => _save(rescanSeconds: v)),
          SwitchListTile(
            value: _s.wifiOnlySync,
            onChanged: (v) => _save(wifiOnlySync: v),
            title: Text(s.wifiOnly),
            subtitle: Text(s.wifiOnlyHint),
          ),
          ListTile(
            title: Text(s.keepVersions),
            subtitle: Text(s.keepVersionsHint),
            trailing: DropdownButton<int>(
              value: _keepChoices.contains(_s.keepVersionsDays) ? _s.keepVersionsDays : 30,
              onChanged: (v) => v == null ? null : _save(keepVersionsDays: v),
              items: [
                for (final days in _keepChoices)
                  DropdownMenuItem(value: days, child: Text('$days ${s.daysUnit}')),
              ],
            ),
          ),
          // A phone has no file manager worth sending someone to, so the log is shown here
          // and offered for copying rather than pointed at.
          ListTile(
            title: Text(s.log),
            subtitle: Text(s.logHint),
            trailing: TextButton(onPressed: _showLog, child: Text(s.logView)),
          ),

          const Divider(),
          _header(s.updateSection),
          SwitchListTile(
            value: _s.updateCheck,
            onChanged: (v) => _save(updateCheck: v),
            title: Text(s.updateCheck),
            subtitle: Text(s.updateCheckHint),
          ),
          ListTile(
            title: Text(s.updateNow),
            subtitle: _updateStatus == null ? null : Text(_updateStatus!),
            trailing: _checking
                ? const SizedBox(
                    width: 18, height: 18, child: CircularProgressIndicator(strokeWidth: 2))
                : const Icon(Icons.refresh),
            onTap: _checking ? null : _checkNow,
          ),
          if (_update != null)
            ListTile(
              leading: const Icon(Icons.download),
              title: Text(s.updateOpen),
              // The apk for this phone's ABI, named. A release carries three of them and the
              // release page cannot tell you which is yours.
              subtitle: _update!.file.isEmpty
                  ? null
                  : Text(_update!.file,
                      style: const TextStyle(fontFamily: 'monospace', fontSize: 11)),
              onTap: () => host.openUrl(_update!.url),
            ),

          const Divider(),
          FutureBuilder<String>(
            future: appVersion(),
            builder: (context, snapshot) => ListTile(
              dense: true,
              title: Text('${s.version} ${snapshot.data ?? ''}'),
            ),
          ),
        ],
      ),
    );
  }

  Widget _header(String text) => Padding(
        padding: const EdgeInsets.fromLTRB(16, 12, 16, 4),
        child: Text(
          text,
          style: Theme.of(context)
              .textTheme
              .titleSmall
              ?.copyWith(color: Theme.of(context).colorScheme.primary),
        ),
      );
}
