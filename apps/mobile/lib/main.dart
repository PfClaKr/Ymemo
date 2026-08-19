// Ymemo mobile: lock screen, memo list, memo editor.
// `lib/src/rust/` is flutter_rust_bridge codegen output and is committed; see the README.
// After changing api.rs, run `flutter_rust_bridge_codegen generate` first.
//
// No strings are written here. They come from the **same catalog** as the desktop
// (i18n/*.json at the repo root) through `mobileStrings()`, so the UI never drifts from the
// language of the core's error messages. To add one, put a `mobile.*` key in ko.json and
// en.json and a field in FfiStrings in crates/ymemo-ffi; the ymemo-i18n tests check it.

import 'dart:io' show Platform;
import 'dart:typed_data';
import 'dart:ui' as ui;

import 'package:flutter/material.dart';
import 'package:image_picker/image_picker.dart';
import 'package:mobile_scanner/mobile_scanner.dart';
import 'package:path_provider/path_provider.dart';

import 'src/rust/api.dart';
import 'src/rust/frb_generated.dart';

Future<void> main() async {
  WidgetsFlutterBinding.ensureInitialized();
  await RustLib.init();
  // Keep core errors and UI text in one language; an unknown locale falls back to the system one.
  await setLanguage(code: Platform.localeName);
  runApp(YmemoApp(strings: await mobileStrings()));
}

class YmemoApp extends StatelessWidget {
  const YmemoApp({super.key, required this.strings});

  final FfiStrings strings;

  @override
  Widget build(BuildContext context) {
    return MaterialApp(
      title: 'Ymemo',
      theme: ThemeData(
        colorScheme: ColorScheme.fromSeed(seedColor: const Color(0xFFE6D24A)),
        useMaterial3: true,
      ),
      home: LockScreen(strings: strings),
    );
  }
}

/// Lock screen: opens the vault with the master password, creating it if needed.
class LockScreen extends StatefulWidget {
  const LockScreen({super.key, required this.strings});

  final FfiStrings strings;

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
      final docs = await getApplicationDocumentsDirectory();
      // vault/ is the directory that will be synced; local-only until Syncthing lands.
      await vaultOpen(
        vaultDir: '${docs.path}/vault',
        cacheDbPath: '${docs.path}/ymemo.db',
        password: _password.text,
      );
      if (!mounted) return;
      Navigator.of(context).pushReplacement(
        MaterialPageRoute(builder: (_) => MemoListScreen(strings: widget.strings)),
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
  const MemoListScreen({super.key, required this.strings});

  final FfiStrings strings;

  @override
  State<MemoListScreen> createState() => _MemoListScreenState();
}

class _MemoListScreenState extends State<MemoListScreen> {
  List<FfiMemo> _memos = [];

  @override
  void initState() {
    super.initState();
    _reload();
  }

  Future<void> _reload() async {
    final memos = await memoList();
    if (mounted) setState(() => _memos = memos);
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
          IconButton(
            icon: const Icon(Icons.qr_code_scanner),
            tooltip: widget.strings.scanQr,
            onPressed: () => Navigator.of(context).push(
              MaterialPageRoute(builder: (_) => ScanScreen(strings: widget.strings)),
            ),
          ),
          IconButton(
            icon: const Icon(Icons.sync),
            tooltip: widget.strings.syncNow,
            onPressed: () async {
              // Picks up any logs other devices have delivered to the vault directory.
              await syncRebuild();
              await _reload();
            },
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

/// Scans another device's pairing QR.
///
/// The core validates the format (`pairingDecode`), so a format change leaves Dart alone.
/// The scanned device **cannot actually be registered** yet: without mobile Syncthing
/// (gomobile) there is no way to add a peer to the shared folder. So this only validates and
/// says so on screen, rather than failing quietly and leaving the user guessing.
class ScanScreen extends StatefulWidget {
  const ScanScreen({super.key, required this.strings});

  final FfiStrings strings;

  @override
  State<ScanScreen> createState() => _ScanScreenState();
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
              Text(
                widget.strings.scanPairingUnavailable,
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
