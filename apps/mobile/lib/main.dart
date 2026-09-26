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
import 'dart:ui' as ui;

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:path_provider/path_provider.dart';

import 'home_widgets.dart' as widgets;
import 'host.dart' as host;
import 'screens/lock_screen.dart';
import 'screens/memo_list_screen.dart';
import 'settings.dart';
import 'src/rust/api.dart';
import 'src/rust/frb_generated.dart';
import 'sync.dart';

Future<void> main() async {
  WidgetsFlutterBinding.ensureInitialized();
  await RustLib.init();

  // Android 15 draws every app edge to edge whether it asks to or not, so the system bars
  // sit *on top of* the UI. Asking for it explicitly makes older versions behave the same
  // way instead of leaving two layouts to reason about; `bottomInset` below is what keeps
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
  // An error nobody awaited — a timer's callback, a fire-and-forget future — never reaches
  // `presentError`; it goes here, and with no handler the engine logs it to stderr, which on
  // Android is nowhere. Returning true says it has been dealt with: the app carries on.
  ui.PlatformDispatcher.instance.onError = (error, stack) {
    debugPrint('uncaught: $error\n$stack');
    return true;
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
    // The status bar is the system's, drawn over our own background because the app is edge
    // to edge. Flutter's default leaves its clock and icons **light**, which on this cream
    // paper is white on off-white — unreadable on every screen that has no AppBar to set the
    // style for it, which is every screen before the vault is open. Setting it on the theme
    // covers those too, and follows the platform brightness so a dark phone still gets light
    // icons. `systemNavigationBar` is left alone: the gesture bar draws its own contrast.
    final dark = MediaQuery.platformBrightnessOf(context) == Brightness.dark;
    final overlay = SystemUiOverlayStyle(
      statusBarColor: Colors.transparent,
      statusBarIconBrightness: dark ? Brightness.light : Brightness.dark,
      statusBarBrightness: dark ? Brightness.dark : Brightness.light,
    );
    SystemChrome.setSystemUIOverlayStyle(overlay);
    return MaterialApp(
      title: 'Ymemo',
      navigatorKey: _navigator,
      theme: ThemeData(
        colorScheme: ColorScheme.fromSeed(seedColor: const Color(0xFFE6D24A)),
        useMaterial3: true,
        appBarTheme: AppBarTheme(systemOverlayStyle: overlay),
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
