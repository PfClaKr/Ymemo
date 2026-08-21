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
import 'package:image_picker/image_picker.dart';
import 'package:mobile_scanner/mobile_scanner.dart';
import 'package:path_provider/path_provider.dart';

import 'src/rust/api.dart';
import 'src/rust/frb_generated.dart';
import 'sync.dart';

Future<void> main() async {
  WidgetsFlutterBinding.ensureInitialized();
  await RustLib.init();
  // Keep core errors and UI text in one language; an unknown locale falls back to the system one.
  await setLanguage(code: Platform.localeName);

  // Every path the app uses is derived here, once. The vault directory in particular is
  // shared between the two: it is what the daemon syncs and what the vault is opened from,
  // and two spellings of it would mean syncing one directory while reading another.
  final docs = await getApplicationDocumentsDirectory();
  final sync = SyncController(SyncPaths(
    homeDir: '${docs.path}/syncthing',
    vaultDir: '${docs.path}/vault',
  ));

  runApp(YmemoApp(
    strings: await mobileStrings(),
    sync: sync,
    cacheDbPath: '${docs.path}/ymemo.db',
  ));

  // Not awaited: the daemon's first start generates a device key and takes seconds, and the
  // lock screen has nothing to wait for. It comes up **before unlocking** on purpose — a new
  // device pairs and receives vault.json first, otherwise unlocking would create a second
  // vault with a different salt that could never converge with the first.
  unawaited(sync.init());
}

class YmemoApp extends StatelessWidget {
  const YmemoApp({
    super.key,
    required this.strings,
    required this.sync,
    required this.cacheDbPath,
  });

  final FfiStrings strings;
  final SyncController sync;

  /// Device-local SQLite cache; rebuilt from the logs, never synced.
  final String cacheDbPath;

  @override
  Widget build(BuildContext context) {
    return MaterialApp(
      title: 'Ymemo',
      theme: ThemeData(
        colorScheme: ColorScheme.fromSeed(seedColor: const Color(0xFFE6D24A)),
        useMaterial3: true,
      ),
      home: LockScreen(strings: strings, sync: sync, cacheDbPath: cacheDbPath),
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
    required this.cacheDbPath,
  });

  final FfiStrings strings;
  final SyncController sync;
  final String cacheDbPath;

  @override
  State<LockScreen> createState() => _LockScreenState();
}

class _LockScreenState extends State<LockScreen> {
  final _password = TextEditingController();
  String? _error;
  bool _busy = false;

  Future<void> _unlock() async {
    if (_password.text.isEmpty || _busy) return;
    setState(() => _busy = true);
    try {
      // The same directory the daemon shares (see main), so what arrives is what is opened.
      await vaultOpen(
        vaultDir: widget.sync.paths.vaultDir,
        cacheDbPath: widget.cacheDbPath,
        password: _password.text,
      );
      if (!mounted) return;
      Navigator.of(context).pushReplacement(
        MaterialPageRoute(
          builder: (_) => MemoListScreen(strings: widget.strings, sync: widget.sync),
        ),
      );
    } catch (e) {
      // Core errors already arrive in the current language.
      setState(() => _error = '$e');
    } finally {
      if (mounted) setState(() => _busy = false);
    }
  }

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      appBar: AppBar(
        // No title: the lock screen says what it is. The action is here so a fresh install
        // can pair before it has a vault to unlock.
        backgroundColor: Colors.transparent,
        actions: [SyncButton(strings: widget.strings, sync: widget.sync)],
      ),
      body: Center(
        child: Padding(
          padding: const EdgeInsets.all(24),
          child: Column(
            mainAxisSize: MainAxisSize.min,
            children: [
              const Text('Ymemo 🔒', style: TextStyle(fontSize: 24)),
              const SizedBox(height: 16),
              TextField(
                controller: _password,
                obscureText: true,
                decoration: InputDecoration(labelText: widget.strings.masterPassword),
                onSubmitted: (_) => _unlock(),
              ),
              if (_error != null)
                Padding(
                  padding: const EdgeInsets.only(top: 8),
                  child: Text(_error!, style: const TextStyle(color: Colors.red)),
                ),
              const SizedBox(height: 16),
              FilledButton(
                onPressed: _busy ? null : _unlock,
                child: Text(_busy ? widget.strings.opening : widget.strings.unlock),
              ),
            ],
          ),
        ),
      ),
    );
  }
}

/// Memo list, with add, open and delete.
class MemoListScreen extends StatefulWidget {
  const MemoListScreen({super.key, required this.strings, required this.sync});

  final FfiStrings strings;
  final SyncController sync;

  @override
  State<MemoListScreen> createState() => _MemoListScreenState();
}

class _MemoListScreenState extends State<MemoListScreen> {
  /// How often logs that have arrived are merged in, matching the desktop's timer. The
  /// daemon delivers files whenever it likes; this is what turns them into memos on screen.
  static const _mergeInterval = Duration(seconds: 15);

  List<FfiMemo> _memos = [];
  Timer? _merge;

  @override
  void initState() {
    super.initState();
    _reload();
    _merge = Timer.periodic(_mergeInterval, (_) => _mergeNow());
  }

  @override
  void dispose() {
    _merge?.cancel();
    super.dispose();
  }

  Future<void> _reload() async {
    final memos = await memoList();
    if (mounted) setState(() => _memos = memos);
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
  Future<void> _add() async {
    final id = await memoUpsert(title: '', body: '');
    if (!mounted) return;
    await Navigator.of(context).push(
      MaterialPageRoute(
        builder: (_) => MemoEditScreen(strings: widget.strings, id: id, title: '', body: ''),
      ),
    );
    await _reload();
  }

  Future<void> _open(FfiMemo memo) async {
    await Navigator.of(context).push(
      MaterialPageRoute(
        builder: (_) =>
            MemoEditScreen(strings: widget.strings, id: memo.id, title: memo.title, body: memo.body),
      ),
    );
    await _reload();
  }

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      appBar: AppBar(
        title: Text(widget.strings.listTitle),
        actions: [
          SyncButton(strings: widget.strings, sync: widget.sync),
          IconButton(
            icon: const Icon(Icons.sync),
            tooltip: widget.strings.syncNow,
            // The timer does this every 15s; the button is for when you are waiting on a
            // memo you just wrote on the other device.
            onPressed: _mergeNow,
          ),
        ],
      ),
      body: ListView.builder(
        itemCount: _memos.length,
        itemBuilder: (context, i) {
          final memo = _memos[i];
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
            child: ListTile(
              title: Text(memo.title.isEmpty ? widget.strings.newMemo : memo.title),
              subtitle: memo.body.isEmpty
                  ? null
                  : Text(memo.body, maxLines: 1, overflow: TextOverflow.ellipsis),
              onTap: () => _open(memo),
            ),
          );
        },
      ),
      floatingActionButton: FloatingActionButton(
        onPressed: _add,
        child: const Icon(Icons.add),
      ),
    );
  }
}

/// Memo editor: title and body, saved on the way out.
class MemoEditScreen extends StatefulWidget {
  const MemoEditScreen({
    super.key,
    required this.strings,
    required this.id,
    required this.title,
    required this.body,
  });

  final FfiStrings strings;
  final String id;
  final String title;
  final String body;

  @override
  State<MemoEditScreen> createState() => _MemoEditScreenState();
}

class _MemoEditScreenState extends State<MemoEditScreen> {
  late final TextEditingController _title = TextEditingController(text: widget.title);
  late final TextEditingController _body = TextEditingController(text: widget.body);
  List<FfiAttachment> _photos = [];

  @override
  void initState() {
    super.initState();
    _reloadPhotos();
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
    await _reloadPhotos();
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
    return PopScope(
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
        appBar: AppBar(
          title: Text(widget.strings.newMemo),
          actions: [
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
          padding: const EdgeInsets.all(16),
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.stretch,
            children: [
              TextField(
                controller: _title,
                decoration: InputDecoration(labelText: widget.strings.titleHint),
                textInputAction: TextInputAction.next,
              ),
              const SizedBox(height: 12),
              Expanded(
                child: ListView(
                  children: [
                    TextField(
                      controller: _body,
                      decoration: InputDecoration(hintText: widget.strings.bodyHint),
                      maxLines: null,
                      minLines: 4,
                    ),
                    for (final photo in _photos)
                      AttachmentView(
                        key: ValueKey(photo.id),
                        strings: widget.strings,
                        attachment: photo,
                        onChanged: _reloadPhotos,
                      ),
                  ],
                ),
              ),
            ],
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
        final Widget icon;
        if (sync.starting) {
          icon = const SizedBox(
            width: 18,
            height: 18,
            child: CircularProgressIndicator(strokeWidth: 2),
          );
        } else if (sync.running) {
          icon = const Icon(Icons.devices);
        } else {
          icon = const Icon(Icons.cloud_off);
        }
        return IconButton(
          icon: icon,
          tooltip: strings.syncDevices,
          onPressed: () => Navigator.of(context).push(
            MaterialPageRoute(builder: (_) => SyncScreen(strings: strings, sync: sync)),
          ),
        );
      },
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

  List<FfiSharedDevice> _devices = const [];

  final _lanInput = TextEditingController();
  String? _lanCode;
  String? _lanMessage;
  bool _joining = false;
  Timer? _lanPoll;

  @override
  void initState() {
    super.initState();
    _reloadDevices();
    _startLan();
  }

  @override
  void dispose() {
    _lanPoll?.cancel();
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
    await Navigator.of(context).push(
      MaterialPageRoute(
        builder: (_) => ScanScreen(strings: widget.strings, sync: widget.sync),
      ),
    );
    await _reloadDevices();
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
          padding: const EdgeInsets.all(16),
          children: [
            _status(context),
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
      final deviceId = await pairingDecode(code: raw);
      await widget.sync.pairWith(raw);
      await _controller.stop();
      if (!mounted) return;
      await showDialog<void>(
        context: context,
        builder: (context) => AlertDialog(
          title: Text(widget.strings.scanResult),
          content: Column(
            mainAxisSize: MainAxisSize.min,
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              SelectableText(deviceId),
              const SizedBox(height: 12),
              // Pairing is mutual; without this line the user would wait forever for a
              // connection that needs a step on the other device.
              Text(
                widget.strings.pairAdded,
                style: Theme.of(context).textTheme.bodySmall,
              ),
            ],
          ),
          actions: [
            TextButton(
              onPressed: () => Navigator.of(context).pop(),
              child: Text(widget.strings.close),
            ),
          ],
        ),
      );
      navigator.pop();
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

/// One attached photo: display, resize, detach.
///
/// The display size syncs in **em**, multiples of the font size. Shrinking it here shrinks it
/// by the same factor on the desktop, where the pixels are computed from that platform's own
/// base font.
class AttachmentView extends StatefulWidget {
  const AttachmentView({
    super.key,
    required this.strings,
    required this.attachment,
    required this.onChanged,
  });

  final FfiStrings strings;
  final FfiAttachment attachment;
  final Future<void> Function() onChanged;

  @override
  State<AttachmentView> createState() => _AttachmentViewState();
}

class _AttachmentViewState extends State<AttachmentView> {
  Uint8List? _bytes;
  bool _missing = false;
  late double _widthEm = widget.attachment.widthEmMilli / 1000.0;

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

  @override
  Widget build(BuildContext context) {
    // This platform's body font size is what em is measured against.
    final baseFont = DefaultTextStyle.of(context).style.fontSize ?? 14.0;
    final width = _widthEm * baseFont;
    final a = widget.attachment;
    final ratio = (a.widthPx > 0 && a.heightPx > 0) ? a.heightPx / a.widthPx : 1.0;

    return Padding(
      padding: const EdgeInsets.symmetric(vertical: 12),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          if (_missing)
            Text(widget.strings.photoMissing, style: Theme.of(context).textTheme.bodySmall)
          else if (_bytes == null)
            const SizedBox(height: 48, child: Center(child: CircularProgressIndicator()))
          else
            ClipRRect(
              borderRadius: BorderRadius.circular(8),
              child: Image.memory(
                _bytes!,
                width: width,
                height: width * ratio,
                fit: BoxFit.cover,
              ),
            ),
          Row(
            children: [
              Text(widget.strings.photoSize, style: Theme.of(context).textTheme.bodySmall),
              Expanded(
                child: Slider(
                  // The core's own range (4-80em); it clamps anything outside anyway.
                  min: 4,
                  max: 80,
                  value: _widthEm.clamp(4, 80),
                  onChanged: (v) => setState(() => _widthEm = v),
                  // Written once on release, not throughout the drag; one slide would
                  // otherwise leave dozens of changes in the log.
                  onChangeEnd: (v) async {
                    await attachmentSetWidth(id: a.id, widthEmMilli: (v * 1000).round());
                    await widget.onChanged();
                  },
                ),
              ),
              IconButton(
                icon: const Icon(Icons.delete_outline),
                tooltip: widget.strings.photoRemove,
                onPressed: () async {
                  await attachmentRemove(id: a.id);
                  await widget.onChanged();
                },
              ),
            ],
          ),
        ],
      ),
    );
  }
}
