// Ymemo 모바일: 잠금 화면 → 메모 목록 → 메모 편집.
// `lib/src/rust/` 는 flutter_rust_bridge codegen 생성물이다 (커밋한다 — README 참조).
// api.rs 를 고쳤으면 `flutter_rust_bridge_codegen generate` 를 먼저 돌려야 한다.
//
// 문구는 여기 쓰지 않는다. 데스크탑과 **같은 카탈로그**(저장소 루트 i18n/*.json)에서
// `mobileStrings()` 로 한 벌 받아 쓴다 — 코어 에러 메시지와 언어가 갈라지지 않게 하기
// 위함이다. 문구를 늘리려면 i18n/ko.json·en.json 에 `mobile.*` 키를 넣고
// crates/ymemo-ffi 의 FfiStrings 에 필드를 추가한다 (ymemo-i18n 테스트가 키를 검사한다).

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
  // 코어 에러와 UI 문구를 같은 언어로 맞춘다. 모르는 로캘이면 코어가 시스템 로캘로 떨어진다.
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

/// 잠금 화면: 마스터 암호로 vault 를 연다 (없으면 생성).
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
      // vault/ 는 동기화 대상 디렉터리 (Syncthing 연동 전까지는 로컬 전용).
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
      // 코어 에러는 이미 카탈로그를 거쳐 현재 언어로 온다.
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

/// 메모 목록 + 추가/열기/삭제.
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

  /// 빈 메모를 만들고 바로 편집 화면으로 — 목록에서 제목만 묻는 것보다 손이 덜 간다.
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
              // 다른 기기의 로그가 vault 디렉터리에 도착해 있으면 여기서 반영된다.
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

/// 메모 편집: 제목 + 본문. 나갈 때 저장한다.
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

  /// 사진첩/카메라에서 한 장 골라 붙인다.
  ///
  /// 원본 바이트를 그대로 코어에 넘긴다(리사이즈하지 않는 것이 결정 사항). 다만
  /// **원본 픽셀 크기는 여기서 재서 넘긴다** — 코어에 이미지 디코더를 두지 않기 때문이고,
  /// 그 값으로 표시 높이 비율이 정해진다.
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

  /// 원본 픽셀 크기. 디코딩에 실패하면 null (코어는 0 을 받고 1:1 로 취급한다).
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

  /// 바뀐 게 없으면 쓰지 않는다 — 빈 change 를 로그에 남기면 동기화 트래픽만 는다.
  Future<void> _save() async {
    if (_title.text == widget.title && _body.text == widget.body) return;
    await memoUpsert(id: widget.id, title: _title.text, body: _body.text);
  }

  @override
  Widget build(BuildContext context) {
    return PopScope(
      // 뒤로 가기 = 저장. 별도 저장 버튼도 두되, 눌러야만 저장되는 방식은 쓰지 않는다.
      //
      // `canPop: false` 로 두고 **저장한 뒤 직접 pop** 한다. 기본값(true)이면 라우트가
      // 먼저 사라지고 컨트롤러가 dispose 된 다음에 이 콜백이 도는데, 그때 `_title.text`
      // 를 읽으면 "used after being disposed" 로 저장이 조용히 실패한다.
      // (에뮬레이터에서 실제로 겪은 버그 — 뒤로 나가면 편집분이 사라졌다.)
      canPop: false,
      onPopInvokedWithResult: (didPop, _) async {
        if (didPop) return;
        // await 를 건너면 context 를 다시 쓸 수 없으므로 navigator 를 미리 잡아 둔다.
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

/// 상대 기기의 페어링 QR 을 카메라로 읽는다.
///
/// 코드 형식 검증은 코어(`pairingDecode`)가 한다 — 형식이 바뀌어도 Dart 는 그대로다.
/// 다만 **읽은 기기를 실제로 등록하지는 못한다**: 모바일 Syncthing(gomobile) 이 아직
/// 없어서 공유 폴더에 상대를 추가할 수단이 없다. 그래서 지금은 확인만 하고 그 사실을
/// 화면에 밝힌다 (조용히 실패해 "연결됐나?" 하게 만들지 않는다).
class ScanScreen extends StatefulWidget {
  const ScanScreen({super.key, required this.strings});

  final FfiStrings strings;

  @override
  State<ScanScreen> createState() => _ScanScreenState();
}

class _ScanScreenState extends State<ScanScreen> {
  final _controller = MobileScannerController(
    // 페어링 QR 만 보면 되므로 다른 바코드 형식은 아예 무시한다(오탐·배터리 둘 다 이득).
    formats: const [BarcodeFormat.qrCode],
    detectionSpeed: DetectionSpeed.noDuplicates,
  );
  // 한 번 읽으면 더 처리하지 않는다 — 카메라는 같은 코드를 계속 흘려보낸다.
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
      // 코어가 돌려준 문구(현재 언어)를 그대로 보여주고 다시 읽을 수 있게 푼다.
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
            // 카메라를 못 여는 경우(권한 거부·카메라 없음)를 빈 화면으로 두지 않는다.
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

/// 첨부 사진 한 장: 표시 + 크기 조절 + 떼기.
///
/// 표시 크기는 **폰트 크기 배수(em)** 로 동기화된다. 여기서 슬라이더로 줄이면 데스크탑에서도
/// 같은 배수로 줄어들고, 실제 픽셀은 각 플랫폼이 자기 기본 폰트로 환산한다.
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
    // 아직 동기화 전이면 바이트가 없다 — 그 사실을 그대로 보여준다.
    if (!await attachmentHasBlob(hash: widget.attachment.hash)) {
      if (mounted) setState(() => _missing = true);
      return;
    }
    final bytes = await attachmentBytes(hash: widget.attachment.hash);
    if (mounted) setState(() => _bytes = bytes);
  }

  @override
  Widget build(BuildContext context) {
    // 이 플랫폼의 기본 본문 폰트 크기 = em 의 기준.
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
                  // 코어의 허용 범위(4~80em)와 같은 값. 벗어나면 코어가 어차피 자른다.
                  min: 4,
                  max: 80,
                  value: _widthEm.clamp(4, 80),
                  onChanged: (v) => setState(() => _widthEm = v),
                  // 드래그하는 내내 쓰지 않고 손을 뗄 때 한 번만 기록한다
                  // (슬라이더 한 번에 change 를 수십 개 남기면 로그가 부푼다).
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
