// Sync lifecycle for the mobile app.
//
// The daemon is the same bundled syncthing the desktop runs, driven by the same core code
// (crates/ymemo-ffi exposes it as sync_*). Two things are mobile-specific:
//
//  - **Where the binary is.** Android only executes binaries from the native library
//    directory, so it ships as `libsyncthing.so` and MainActivity hands the path over the
//    `dev.ymemo/native` channel. No path, no sync: the app then runs local-only rather than
//    refusing to start, which is what a debug build without the bundled daemon does.
//  - **When it runs.** Only while the app is in the foreground. Android freezes background
//    processes anyway, so keeping it alive would mean a foreground service and a permanent
//    notification for a memo app that syncs perfectly well whenever it is opened.
//
// It starts **before the vault is unlocked**, exactly as on the desktop: a brand-new device
// has to pair and receive vault.json first, or unlocking would create a second vault with a
// different salt and the two would never converge.

import 'dart:async';
import 'dart:io' show Platform;

import 'package:flutter/widgets.dart';

import 'host.dart' as host;
import 'src/rust/api.dart' as ffi;

/// Where the app keeps the daemon's own state and the synced vault.
class SyncPaths {
  const SyncPaths({required this.homeDir, required this.vaultDir});

  /// syncthing's config and database — device-local, never synced.
  final String homeDir;

  /// The shared folder: vault.json and the per-device logs.
  final String vaultDir;
}

/// Runs the daemon while the app is in the foreground and exposes what the UI shows.
///
/// A [ChangeNotifier] rather than anything larger: three screens read it and nothing else in
/// the app has state worth a framework.
class SyncController extends ChangeNotifier with WidgetsBindingObserver {
  SyncController(this.paths);

  /// How long a join broadcasts before giving up. Each attempt costs both sides an Argon2
  /// derivation, so this is a handful of retries, not a busy loop.
  static const int _joinTimeoutSecs = 8;

  /// How often incoming requests are polled for while the daemon is up. Answering one means
  /// a person walking to another device, so seconds are plenty and a tighter loop would only
  /// spend REST calls and battery.
  static const _pendingPoll = Duration(seconds: 2);

  final SyncPaths paths;

  String? _binaryPath;
  String? _code;
  String? _error;
  bool _starting = false;

  List<ffi.FfiPendingDevice> _pending = const [];
  Timer? _pendingTimer;

  /// Devices asking to be let in, oldest first. Polled here rather than by the screen so the
  /// app-bar button can badge itself without the pairing screen being open.
  List<ffi.FfiPendingDevice> get pending => _pending;

  /// This device's pairing code, once the daemon is up.
  String? get pairingCode => _code;

  /// The last failure, in the catalog's language; cleared by a successful start.
  String? get error => _error;

  bool get starting => _starting;
  bool get running => _code != null;

  /// False when this build ships no daemon — the UI says so instead of offering pairing.
  bool get available => _binaryPath != null;

  /// Begins observing the lifecycle and starts the daemon. Call once, at app start.
  Future<void> init() async {
    WidgetsBinding.instance.addObserver(this);
    _binaryPath = await _findBinary();
    if (_binaryPath == null) {
      notifyListeners(); // available == false; the UI explains it
      return;
    }
    await start();
  }

  @override
  void dispose() {
    _pendingTimer?.cancel();
    WidgetsBinding.instance.removeObserver(this);
    super.dispose();
  }

  @override
  void didChangeAppLifecycleState(AppLifecycleState state) {
    switch (state) {
      case AppLifecycleState.resumed:
        start();
      case AppLifecycleState.paused:
      case AppLifecycleState.detached:
      case AppLifecycleState.hidden:
        stop();
      case AppLifecycleState.inactive:
        break; // a passing overlay is not a reason to tear the daemon down
    }
  }

  /// Starts the daemon if it is not already running. Safe to call repeatedly — the Rust side
  /// is idempotent — which is what makes it usable straight from the lifecycle callback.
  Future<void> start() async {
    final binary = _binaryPath;
    if (binary == null || _starting) return;
    _starting = true;
    notifyListeners();
    try {
      // The first start generates the device key and takes a few seconds; this runs on a
      // worker thread, so the UI keeps drawing.
      _code = await ffi.syncStart(
        binaryPath: binary,
        homeDir: paths.homeDir,
        vaultDir: paths.vaultDir,
      );
      _error = null;
      _startPendingPoll();
    } catch (e) {
      _code = null;
      _error = '$e';
    } finally {
      _starting = false;
      notifyListeners();
    }
  }

  void _startPendingPoll() {
    _pendingTimer?.cancel();
    _pendingTimer = Timer.periodic(_pendingPoll, (_) => refreshPending());
    unawaited(refreshPending());
  }

  /// Re-reads the pending requests and notifies if the set changed.
  ///
  /// Compared by id rather than replaced outright: a rebuild every two seconds would rebuild
  /// the pairing screen under the user's finger for nothing.
  Future<void> refreshPending() async {
    List<ffi.FfiPendingDevice> next;
    try {
      next = await pendingDevices();
    } catch (e) {
      // Offline or shutting down; the next tick tries again.
      debugPrint('could not read the pending devices: $e');
      return;
    }
    final same = next.length == _pending.length &&
        List.generate(next.length, (i) => next[i].id == _pending[i].id).every((v) => v);
    _pending = next;
    if (!same) notifyListeners();
  }

  /// Re-registers the vault directory with the daemon.
  ///
  /// [start] does it too, but it is idempotent by short-circuiting on an already running
  /// daemon, so the folder a reset removed would stay removed for the rest of the session.
  /// Called after a vault is created, which is the one moment that can follow a reset.
  Future<void> ensureFolder() => ffi.syncEnsureFolder(vaultDir: paths.vaultDir);

  /// Stops the daemon. Errors are swallowed: this runs while the app is going away.
  Future<void> stop() async {
    if (_code == null && !_starting) return;
    // Pairing mode goes with it. The socket is moot once Android freezes us, but the wifi
    // multicast lock would keep the radio awake in the background for nothing.
    await lanStop();
    try {
      await ffi.syncStop();
    } catch (_) {
      // Nothing useful to do — the process is being backgrounded either way.
    }
    _code = null;
    _pendingTimer?.cancel();
    _pendingTimer = null;
    _pending = const [];
    notifyListeners();
  }

  /// Registers a scanned or typed code. Only half of pairing: the other device has to add
  /// this one's code too.
  /// Registers a scanned or typed code and returns the peer's device id.
  ///
  /// Only this device's half of the link. It now starts dialling a device that has never
  /// heard of it, and nothing syncs until **that** device allows the request — which is what
  /// the returned id is for: the screen waits on it and shows [verificationCode] meanwhile.
  Future<String> pairWith(String code) => ffi.syncPairWith(code: code);

  // ---- Incoming requests ---------------------------------------------------------------
  //
  // The mirror image of pairWith: a device that scanned *our* code is dialling us, the
  // daemon refuses the caller it does not know and files it, and these three answer it.

  /// Devices asking to be let in, oldest first, minus the ones already rejected.
  Future<List<ffi.FfiPendingDevice>> pendingDevices() async {
    if (!running) return const [];
    return ffi.syncPendingDevices();
  }

  /// Lets a device in, completing the link.
  Future<void> approveDevice(String deviceId) async {
    await ffi.syncApproveDevice(deviceId: deviceId);
    await refreshPending(); // drop it from the screen now, not on the next tick
  }

  /// Turns a device away and stops asking about it until the app is restarted.
  Future<void> rejectDevice(String deviceId) async {
    await ffi.syncRejectDevice(deviceId: deviceId);
    await refreshPending();
  }

  /// The eight characters [peerDeviceId] is showing on its own approval screen.
  Future<String> verificationCode(String peerDeviceId) =>
      ffi.syncVerificationCode(peerDeviceId: peerDeviceId);

  // ---- LAN pairing (the 6-digit code) -----------------------------------------------
  //
  // Both halves at once: whichever side answers registers the other, so unlike the QR path
  // there is nothing left to do on the other device. Pairing mode is on only while the
  // screen is open — it holds a UDP socket and a wifi multicast lock.

  /// Enters pairing mode; returns the code to show, or null when there is no daemon to pair.
  Future<String?> lanStart() async {
    if (!running) return null;
    // Without the multicast lock the wifi stack quietly drops the other device's broadcast.
    // Failing to take it is not fatal: on many devices the packet arrives anyway.
    await host.acquireMulticastLock();
    return ffi.lanStart();
  }

  /// Leaves pairing mode and drops the lock. Safe to call when it was never entered.
  Future<void> lanStop() async {
    await host.releaseMulticastLock();
    try {
      await ffi.lanStop();
    } catch (_) {
      // The screen is closing; there is nothing to report it to.
    }
  }

  /// The code currently on offer — it rotates every minute, so the screen re-reads it.
  Future<String?> lanCode() => ffi.lanCode();

  /// Devices that used *our* code, already registered by the Rust side.
  Future<List<String>> lanPollPaired() => ffi.lanPollPaired();

  /// Pairs with the device showing [code]. Null means nobody answered in time.
  Future<String?> lanJoin(String code) =>
      ffi.lanJoin(code: code, timeoutSecs: BigInt.from(_joinTimeoutSecs));

  /// Peers this vault is shared with. Empty while the daemon is down.
  Future<List<ffi.FfiSharedDevice>> devices() async {
    if (!running) return const [];
    return ffi.syncDevices();
  }

  Future<void> unpair(String deviceId) => ffi.syncUnpair(deviceId: deviceId);

  /// The daemon's path, or null where there is none to run.
  Future<String?> _findBinary() async {
    if (!Platform.isAndroid) return null; // iOS cannot exec a bundled binary at all
    return host.syncBinaryPath();
  }
}
