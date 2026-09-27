/// The past versions of a memo or folder, and putting one back.
library;

import 'dart:async';

import 'package:flutter/material.dart';

import '../palette.dart';
import '../src/rust/api.dart';
import '../memo_title.dart';
import '../theme.dart';
import '../ui_util.dart';

/// Every past version of one memo, and the way back to any of them.
///
/// The desktop has had this since the history landed; the phone had the same versions sitting
/// in the same logs and no screen that could reach them — which mattered most in exactly the
/// case the undo bar does not cover, a memo edited into uselessness rather than deleted.
///
/// Restoring is not a rewrite: it appends the old values as a new edit, so the versions it
/// stepped over stay readable and two devices restoring at once merge instead of fighting.
/// See `Vault::restore`.
class HistoryScreen extends StatefulWidget {
  const HistoryScreen({
    super.key,
    required this.strings,
    required this.memoId,
    required this.title,
    required this.color,
  });

  final FfiStrings strings;
  final String memoId;
  final String title;
  final String color;

  @override
  State<HistoryScreen> createState() => _HistoryScreenState();
}

class _HistoryScreenState extends State<HistoryScreen> {
  List<FfiRevision> _revisions = [];
  bool _loading = true;
  int? _selected;

  /// Whether anything was restored, so the screen behind knows to reload.
  bool _changed = false;

  @override
  void initState() {
    super.initState();
    _load();
  }

  Future<void> _load() async {
    try {
      final revisions = await memoHistory(id: widget.memoId);
      // The newest open from the start, as the desktop window opens on it: the screen
      // otherwise began as a column of closed dates.
      if (mounted) {
        setState(() {
          _revisions = revisions;
          _loading = false;
          _selected ??= revisions.isEmpty ? null : 0;
        });
      }
    } catch (e) {
      if (mounted) setState(() => _loading = false);
      _say('$e');
    }
  }

  void _say(String message) => ScaffoldMessenger.of(context)
      .showSnackBar(SnackBar(content: Text(message), duration: const Duration(seconds: 2)));

  Future<void> _restore(FfiRevision revision) async {
    try {
      await memoRestore(id: widget.memoId, index: revision.index);
      _changed = true;
      // Nothing stays open afterwards: `_selected` is an index into a list that has just
      // grown a row at the top, so the expanded row would be the revision below the one it
      // was showing.
      if (mounted) setState(() => _selected = null);
      await _load();
      if (mounted) _say(widget.strings.historyRestored);
    } catch (e) {
      _say('$e');
    }
  }

  String _when(int millis) => revisionTime(millis, widget.strings);

  @override
  Widget build(BuildContext context) => PaperTheme(builder: _page);

  Widget _page(BuildContext context) {
    final ink = paletteInk(widget.color);
    return PopScope(
      canPop: false,
      onPopInvokedWithResult: (didPop, _) {
        if (!didPop) Navigator.of(context).pop(_changed);
      },
      child: Scaffold(
        backgroundColor: paletteBg(widget.color),
        appBar: AppBar(
          title: Text(widget.title.isEmpty ? widget.strings.newMemo : widget.title),
          backgroundColor: paletteBar(widget.color),
          foregroundColor: ink,
        ),
        body: _loading
            ? const Center(child: CircularProgressIndicator())
            : _revisions.isEmpty
                ? Center(child: Text(widget.strings.historyEmpty))
                : ListView.builder(
                    padding: EdgeInsets.fromLTRB(12, 12, 12, 12 + bottomInset(context)),
                    itemCount: _revisions.length,
                    itemBuilder: (context, i) {
                      final revision = _revisions[i];
                      final open = _selected == i;
                      // Newest first, so the top row is what the memo already holds: putting
                      // it back would change nothing and only add a row. Marked, not offered.
                      final current = i == 0;
                      return Card(
                        margin: const EdgeInsets.only(bottom: 8),
                        color: paletteBar(widget.color),
                        child: Column(
                          crossAxisAlignment: CrossAxisAlignment.stretch,
                          children: [
                            ListTile(
                              title: Text(_when(revision.at),
                                  style: TextStyle(fontWeight: FontWeight.bold, color: ink)),
                              subtitle: Text(
                                [
                                  revision.kind,
                                  if (current) widget.strings.historyCurrent,
                                  revision.device,
                                  revision.changed,
                                ]
                                    .where((part) => part.isNotEmpty)
                                    .join(' · '),
                                style: TextStyle(color: ink.withValues(alpha: 0.75)),
                              ),
                              // Tapping opens the version in place. A phone has no room for
                              // the desktop's second pane, and pushing a page to read two
                              // lines of a memo is a page too many.
                              onTap: () => setState(() => _selected = open ? null : i),
                              trailing: Icon(open ? Icons.expand_less : Icons.expand_more,
                                  color: ink),
                            ),
                            if (open) ...[
                              const Divider(height: 1),
                              Padding(
                                padding: const EdgeInsets.fromLTRB(16, 12, 16, 4),
                                child: SelectableText(
                                  revision.body.isEmpty ? revision.title : revision.body,
                                  style: TextStyle(color: ink),
                                ),
                              ),
                              Padding(
                                padding: const EdgeInsets.fromLTRB(16, 4, 16, 12),
                                child: Align(
                                  alignment: Alignment.centerLeft,
                                  // A deletion is a revision with nothing in it; the version
                                  // before it is the one to go back to.
                                  child: revision.restorable && !current
                                      ? FilledButton(
                                          onPressed: () => _restore(revision),
                                          child: Text(widget.strings.historyRestore),
                                        )
                                      : null,
                                ),
                              ),
                            ],
                          ],
                        ),
                      );
                    },
                  ),
      ),
    );
  }
}
