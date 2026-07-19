// Ymemo 모바일: 잠금 화면 → 메모 목록/추가.
// `lib/src/rust/` 는 flutter_rust_bridge codegen 생성물이다 (커밋 안 함).
// 생성 전에는 이 파일이 컴파일되지 않는다 — apps/mobile/README.md 의 절차 참조.

import 'package:flutter/material.dart';
import 'package:path_provider/path_provider.dart';

import 'src/rust/api.dart';
import 'src/rust/frb_generated.dart';

Future<void> main() async {
  WidgetsFlutterBinding.ensureInitialized();
  await RustLib.init();
  runApp(const YmemoApp());
}

class YmemoApp extends StatelessWidget {
  const YmemoApp({super.key});

  @override
  Widget build(BuildContext context) {
    return MaterialApp(
      title: 'Ymemo',
      theme: ThemeData(
        colorScheme: ColorScheme.fromSeed(seedColor: const Color(0xFFE6D24A)),
        useMaterial3: true,
      ),
      home: const LockScreen(),
    );
  }
}

/// 잠금 화면: 마스터 암호로 vault 를 연다 (없으면 생성).
class LockScreen extends StatefulWidget {
  const LockScreen({super.key});

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
        MaterialPageRoute(builder: (_) => const MemoListScreen()),
      );
    } catch (e) {
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
                decoration: const InputDecoration(labelText: '마스터 암호'),
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
                child: Text(_busy ? '여는 중...' : '열기'),
              ),
            ],
          ),
        ),
      ),
    );
  }
}

/// 메모 목록 + 추가/삭제.
class MemoListScreen extends StatefulWidget {
  const MemoListScreen({super.key});

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

  Future<void> _add() async {
    final controller = TextEditingController();
    final title = await showDialog<String>(
      context: context,
      builder: (context) => AlertDialog(
        title: const Text('새 메모'),
        content: TextField(controller: controller, autofocus: true),
        actions: [
          TextButton(
            onPressed: () => Navigator.pop(context),
            child: const Text('취소'),
          ),
          FilledButton(
            onPressed: () => Navigator.pop(context, controller.text.trim()),
            child: const Text('추가'),
          ),
        ],
      ),
    );
    if (title == null || title.isEmpty) return;
    await memoUpsert(title: title, body: '');
    await _reload();
  }

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      appBar: AppBar(
        title: const Text('Ymemo 📝'),
        actions: [
          IconButton(
            icon: const Icon(Icons.sync),
            tooltip: '동기화 반영',
            onPressed: () async {
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
            child: ListTile(title: Text(memo.title)),
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
