// Ymemo 모바일: 잠금 화면 → 메모 목록 → 메모 편집.
// `lib/src/rust/` 는 flutter_rust_bridge codegen 생성물이다 (커밋 안 함).
// 생성 전에는 이 파일이 컴파일되지 않는다 — apps/mobile/README.md 의 절차 참조.
//
// 문구는 여기 쓰지 않는다. 데스크탑과 **같은 카탈로그**(저장소 루트 i18n/*.json)에서
// `mobileStrings()` 로 한 벌 받아 쓴다 — 코어 에러 메시지와 언어가 갈라지지 않게 하기
// 위함이다. 문구를 늘리려면 i18n/ko.json·en.json 에 `mobile.*` 키를 넣고
// crates/ymemo-ffi 의 FfiStrings 에 필드를 추가한다 (ymemo-i18n 테스트가 키를 검사한다).

import 'dart:io' show Platform;

import 'package:flutter/material.dart';
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
      onPopInvokedWithResult: (didPop, _) async {
        if (didPop) await _save();
      },
      child: Scaffold(
        appBar: AppBar(
          title: Text(widget.strings.newMemo),
          actions: [
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
                child: TextField(
                  controller: _body,
                  decoration: InputDecoration(hintText: widget.strings.bodyHint),
                  maxLines: null,
                  expands: true,
                  textAlignVertical: TextAlignVertical.top,
                ),
              ),
            ],
          ),
        ),
      ),
    );
  }
}
