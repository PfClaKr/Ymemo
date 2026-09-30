/// The memo list: folders, search, the long-press actions and the desk-wide merge loop.
library;

import 'dart:async';
import 'dart:io' show Directory, File, FileSystemEvent;

import 'package:flutter/foundation.dart' show Uint8List, listEquals, mapEquals;
import 'package:flutter/material.dart';

import '../home_widgets.dart' as widgets;
import '../memo_title.dart';
import '../palette.dart';
import '../settings.dart';
import '../src/rust/api.dart';
import '../sync.dart';
import '../ui_util.dart';
import '../widgets/update_banner.dart';
import 'history_screen.dart';
import 'memo_edit_screen.dart';
import 'settings_screen.dart';
import 'sync_screen.dart';

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

class _MemoListScreenState extends State<MemoListScreen> with WidgetsBindingObserver {
  /// How often logs that have arrived are merged in — the settings screen's "pull
  /// interval", which was fixed at 15 seconds before it became one. The daemon delivers
  /// files whenever it likes; this is what turns them into memos on screen.
  ///
  /// Only half of how long a change takes to appear: the *other* device's watch delay comes
  /// first, and the two add up. Both are in Settings > Advanced.
  Duration get _mergeInterval =>
      Duration(seconds: widget.settings.value.mergeSeconds);

  /// How long after coming back to the front the catch-up merges run.
  ///
  /// Android will not let this app sync while it is away — measured: with a network
  /// constraint a periodic job never runs in deep Doze at all, and an allow-while-idle alarm
  /// is deferred past half an hour even from the most privileged standby bucket. So what
  /// arrives, arrives while the app is open, and the seconds right after it opens are the
  /// ones that decide whether a memo written on the laptop is already here or is fifteen
  /// seconds late. The daemon needs a moment to start and connect, which is why this is a
  /// short burst and not a single try.
  ///
  /// Cheap to be wrong about: a merge with nothing new to read costs nothing now — the core
  /// fingerprints the logs and returns without re-reading them.
  static const _catchUpAfterResume = [
    Duration.zero,
    Duration(seconds: 3),
    Duration(seconds: 8),
  ];

  List<FfiMemo> _memos = [];
  List<FfiGroup> _folders = [];

  /// Whether the lists above have been read at least once. Until then they are empty
  /// because nothing has been asked yet, not because the vault is — and the "no memos yet"
  /// screen shown for that second on every start read as the memos having been lost.
  bool _loaded = false;

  /// How many things each folder on screen holds directly — its subfolders and its memos —
  /// the number the desktop's tree shows beside a folder. Without it a folder row said nothing
  /// about whether opening it was worth it.
  Map<String, int> _counts = const {};
  Timer? _merge;
  final List<Timer> _catchUp = [];
  /// Watches `vault/logs` so an arriving change is merged as it lands.
  StreamSubscription<FileSystemEvent>? _logWatch;
  Timer? _logSettle;
  FfiRelease? _update;

  /// What the vault is called, empty until it is named. It comes out of the synced document,
  /// so renaming it here renames it on every paired device.
  String _vaultName = '';

  /// The find box, and what is in it. While it has something in it `_reload` fills the two
  /// lists above with matches from the whole vault instead of this folder's contents.
  final TextEditingController _search = TextEditingController();
  String _query = '';

  /// Whether the memos show their drag handles; the menu's "Rearrange" turns it on.
  bool _reordering = false;

  bool get _atRoot => widget.groupId.isEmpty;

  @override
  void initState() {
    super.initState();
    WidgetsBinding.instance.addObserver(this);
    _reload();
    _merge = Timer.periodic(_mergeInterval, (_) => _mergeNow());
    // A cold start never delivers `resumed` — the app is already resumed by the time this
    // observer exists — and a cold start is exactly what tapping a widget usually is. So the
    // burst is armed from here as well as from the lifecycle callback.
    _catchUpNow();
    _watchLogs();
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
      case widgets.WidgetAction.share:
        await _addShared(request);
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

  /// Back in front: merge at once instead of waiting out the pull interval.
  ///
  /// See [_catchUpAfterResume] for why the moment the app opens is the only moment that can
  /// be made faster.
  @override
  void didChangeAppLifecycleState(AppLifecycleState state) {
    if (state == AppLifecycleState.resumed) _catchUpNow();
  }

  /// Merges the moment another device's log lands, instead of waiting for the next tick.
  ///
  /// The daemon delivers files into `vault/logs`, and one arriving is the only moment there
  /// is anything new to merge at all. Watching for it is what makes a memo written on the
  /// laptop appear as it arrives rather than up to `merge_seconds` afterwards — and it costs
  /// nothing at all while nothing is arriving, which a shorter timer would not.
  ///
  /// The timer stays as the net under it: a watch can be refused (no inotify left, a
  /// filesystem that has none) and is silently nothing when it is.
  void _watchLogs() {
    final logs = Directory('${widget.sync.paths.vaultDir}/logs');
    if (!logs.existsSync()) return;
    try {
      _logWatch = logs.watch().listen(
        (_) {
          // One arrival is a burst of writes — syncthing writes a temporary file and renames
          // it — so this settles before reading rather than merging once per event.
          _logSettle?.cancel();
          _logSettle = Timer(const Duration(milliseconds: 400), () {
            if (mounted) _mergeNow();
          });
        },
        onError: (Object e) => debugPrint('log watch stopped: $e'),
      );
    } catch (e) {
      debugPrint('cannot watch the log directory: $e');
    }
  }

  /// Arms the catch-up merges, replacing any burst still running.
  void _catchUpNow() {
    for (final t in _catchUp) {
      t.cancel();
    }
    _catchUp.clear();
    for (final after in _catchUpAfterResume) {
      _catchUp.add(Timer(after, () {
        if (mounted) _mergeNow();
      }));
    }
  }

  @override
  void dispose() {
    WidgetsBinding.instance.removeObserver(this);
    _merge?.cancel();
    _logWatch?.cancel();
    _logSettle?.cancel();
    for (final t in _catchUp) {
      t.cancel();
    }
    _search.dispose();
    if (_atRoot) widgets.pendingWidgetRequest.removeListener(_runWidgetRequest);
    super.dispose();
  }

  /// Applies a new search term. Every later reload honours it, so a merge arriving from
  /// another device cannot quietly put the unfiltered list back mid-read.
  Future<void> _runSearch(String query) async {
    // Redrawn now, not only when the matches change: the snippets bold the query itself, and
    // typing "bread" after "br" finds the same memos, so `_reload` left "**br**ead" on screen.
    setState(() => _query = query);
    await _reload();
  }

  Future<void> _reload() async {
    final needle = _query.trim().toLowerCase();
    // Searching looks through the whole vault, not this folder: a memo's folder is exactly
    // what someone searching has forgotten. Matches are on the body as well as the title,
    // since a title here is often just the first line of what was written.
    final folders = needle.isEmpty
        ? await groupChildren(parentId: widget.groupId)
        : (await groupList())
            .where((g) => g.name.toLowerCase().contains(needle))
            .toList();
    final memos = needle.isEmpty
        ? await memosInGroup(groupId: widget.groupId)
        : (await memoList())
            .where((m) =>
                m.title.toLowerCase().contains(needle) ||
                m.body.toLowerCase().contains(needle))
            .toList();
    // Re-read on every reload rather than once: a merge can bring a rename from another
    // device, and the heading is where that shows up.
    final name = _atRoot ? await vaultName() : '';
    var counts = const <String, int>{};
    if (folders.isNotEmpty) {
      final ids = {for (final f in folders) f.id};
      final tally = <String, int>{};
      for (final g in await groupList()) {
        if (ids.contains(g.parentId)) tally[g.parentId] = (tally[g.parentId] ?? 0) + 1;
      }
      for (final m in await memoList()) {
        if (ids.contains(m.groupId)) tally[m.groupId] = (tally[m.groupId] ?? 0) + 1;
      }
      counts = tally;
    }
    // Nothing moved: leave the screen alone. Reloads are cheap to *ask* for and most of them
    // find nothing — the merge timer's, and now every save of your own, which the log watch
    // notices as a change to this device's own file. Rebuilding the list for those would be
    // work with nothing to show for it.
    final same = listEquals(_folders, folders) &&
        listEquals(_memos, memos) &&
        mapEquals(_counts, counts) &&
        _vaultName == name;
    if (mounted && (!same || !_loaded)) {
      setState(() {
        _loaded = true;
        _folders = folders;
        _memos = memos;
        _counts = counts;
        _vaultName = name;
      });
    }
    // Every change to a memo or a folder comes back through here, so this is the one place
    // the home screen has to be told about. It skips the write when nothing moved, which is
    // what most of the merge timer's reloads are.
    unawaited(widgets.publishWidgets());
  }

  /// Places a dragged memo where it was let go, and syncs the arrangement.
  ///
  /// The list draws the folders first and then the memos, so the indices arrive counting both
  /// and are shifted back onto the memos here. A memo dragged up among the folders lands at
  /// the top of the memos rather than nowhere — the folders are not a place a memo can go.
  ///
  /// The core is told the two memos it ended up between rather than a position, so the same
  /// call works whatever another device did to the folder in the meantime.
  Future<void> _reorderMemo(int oldIndex, int newIndex) async {
    // `onReorderItem` rather than `onReorder`: it hands over an index already counted in the
    // list with the dragged row taken out, which is the off-by-one every use of the older
    // callback has to remember to undo.
    final from = oldIndex - _folders.length;
    if (from < 0 || from >= _memos.length) return; // a folder: not arranged this way

    final rest = [..._memos]..removeAt(from);
    final to = (newIndex - _folders.length).clamp(0, rest.length);
    if (to == from) return; // let go where it started

    final moved = _memos[from];
    // Move it in the list first so the row does not spring back while the write happens.
    setState(() {
      _memos = [...rest]..insert(to, moved);
    });
    await memoMove(
      id: moved.id,
      groupId: widget.groupId,
      after: to > 0 ? rest[to - 1].id : null,
      before: to < rest.length ? rest[to].id : null,
    );
    await _reload();
  }

  /// Renames the vault, on every device that shares it.
  Future<void> _renameVault() async {
    final s = widget.strings;
    final name =
        await _askForName(context, s, s.renameVault, _vaultName,
            label: s.vaultName, action: s.save);
    if (name == null) return;
    // The core trims it and cuts it to length, so show back what was actually stored.
    final stored = await vaultSetName(name: name);
    if (mounted) setState(() => _vaultName = stored);
  }

  /// Asks for a name and creates a folder inside the one on screen.
  Future<void> _openSettings() async {
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
  }

  Future<void> _newFolder() async {
    final name = await _askForName(context, widget.strings, widget.strings.newGroup, '',
        action: widget.strings.create);
    if (name == null || name.isEmpty) return;
    _clearSearch();
    if (!mounted) return;
    final messenger = ScaffoldMessenger.of(context);
    try {
      await groupCreate(name: name, parentId: widget.groupId);
    } catch (e) {
      messenger.showSnackBar(SnackBar(content: Text('$e')));
      return;
    }
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
            _sheetTitle(context, folder.name),
            _colorSection(context, s, folder.color),
            const Divider(height: 1),
            ListTile(
              leading: const Icon(Icons.drive_file_rename_outline),
              title: Text(s.rename),
              onTap: () => Navigator.of(context).pop('rename'),
            ),
            ListTile(
              leading: const Icon(Icons.delete_outline),
              iconColor: Theme.of(context).colorScheme.error,
              textColor: Theme.of(context).colorScheme.error,
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
      final name = await _askForName(context, s, s.rename, folder.name, action: s.save);
      if (name != null && name.isNotEmpty) await groupRename(id: folder.id, name: name);
    } else if (action == 'delete') {
      final messenger = ScaffoldMessenger.of(context);
      await groupDelete(id: folder.id);
      await _reload();
      if (!mounted) return;
      // The same offer the swipe gets. Undoing gathers the folder's contents back into it —
      // except anything moved elsewhere in the meantime, which stays where it was put.
      messenger.hideCurrentSnackBar();
      messenger.showSnackBar(SnackBar(
        content: Text(widget.strings.deleted),
        duration: const Duration(seconds: 6),
        action: SnackBarAction(
          label: widget.strings.undo,
          onPressed: () async {
            await memoUndelete();
            if (mounted) await _reload();
          },
        ),
      ));
      return;
    }
    await _reload();
  }

  /// One folder row. Identical whether the list is being arranged or searched.
  Widget _folderTile(FfiGroup folder) => Column(
        key: ValueKey('folder:${folder.id}'),
        mainAxisSize: MainAxisSize.min,
        children: [
          _tinted(
            context,
            folder.color,
            ListTile(
              // The folder's own colour, not the row's ink: the mark is what says both
              // "folder" and "which folder", the same job it does on the desktop's tree.
              // Drawn against the ink so a pale swatch still has an edge on the wash.
              leading: Icon(Icons.folder_rounded,
                  color: paletteSwatch(folder.color), shadows: [
                Shadow(color: paletteInk(folder.color).withValues(alpha: 0.55), blurRadius: 1.5),
              ]),
              title: Text(folder.name, style: const TextStyle(fontWeight: FontWeight.w600)),
              // A folder is the one row here that goes somewhere rather than opening an
              // editor, and nothing on it said so.
              trailing: Row(
                mainAxisSize: MainAxisSize.min,
                children: [
                  if ((_counts[folder.id] ?? 0) > 0)
                    Text('${_counts[folder.id]}',
                        style: Theme.of(context).textTheme.bodySmall?.copyWith(
                            color: Theme.of(context).colorScheme.onSurfaceVariant)),
                  Icon(Icons.chevron_right,
                      color: paletteMark(folder.color, Theme.of(context).brightness)),
                ],
              ),
              onTap: () => _openFolder(folder),
              onLongPress: () => _folderMenu(folder),
            ),
          ),
        ],
      );

  /// Under a memo's title: how it goes on — or, while searching, where in it the words were
  /// found, with them in bold. The start of the body said nothing about why a memo was on the
  /// list of results.
  Widget? _subtitle(FfiMemo memo) {
    final hit = _query.isEmpty ? null : searchSnippet(memo.body, _query);
    if (hit != null) {
      return Text.rich(
        TextSpan(children: [
          TextSpan(text: hit.before),
          TextSpan(
            text: hit.match,
            style: TextStyle(
              fontWeight: FontWeight.w700,
              color: Theme.of(context).colorScheme.onSurface,
            ),
          ),
          TextSpan(text: hit.after),
        ]),
        maxLines: 1,
        overflow: TextOverflow.ellipsis,
      );
    }
    final preview = rowPreview(memo);
    return preview.isEmpty
        ? null
        : Text(preview, maxLines: 1, overflow: TextOverflow.ellipsis);
  }

  /// One memo row. `dragIndex` is its position in the reorderable list, or null while the
  /// list is showing search results, where there is no arrangement to drag it into.
  Widget _memoTile(FfiMemo memo, int? dragIndex) => Column(
        key: ValueKey(memo.id),
        mainAxisSize: MainAxisSize.min,
        children: [
          Dismissible(
            key: ValueKey('dismiss:${memo.id}'),
            direction: DismissDirection.endToStart,
            onDismissed: (_) => _deleteWithUndo(memo),
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
                title: Row(
                  children: [
                    Flexible(child: Text(rowTitle(memo, widget.strings.newMemo))),
                    // A memo with a picture on it says so. It matters most for the memo with
                    // nothing written on it at all, which would otherwise be one "New memo"
                    // row beside another.
                    if (memo.hasPhoto) ...[
                      const SizedBox(width: 6),
                      Icon(Icons.image_outlined,
                          size: 15, color: paletteMark(memo.color, Theme.of(context).brightness)),
                    ],
                  ],
                ),
                subtitle: _subtitle(memo),
                onTap: () => _open(memo),
                onLongPress: () => _memoMenu(memo),
                // A handle of its own, rather than a long press: a long press already
                // opens this memo's menu, and a drag that starts anywhere on the row
                // would fight the swipe that deletes it.
                // When it was last written, the way a person would say it — or, while the
                // list is being rearranged, the handle to drag it by. The handle used to be
                // on every row all the time, three grey bars down the right of the list.
                trailing: dragIndex != null && _reordering
                    ? ReorderableDragStartListener(
                        index: dragIndex,
                        child: Icon(
                          Icons.drag_handle,
                          color: paletteMark(memo.color, Theme.of(context).brightness),
                        ),
                      )
                    : Text(
                        relativeTime(memo.updatedAt.toInt(), widget.strings),
                        style: Theme.of(context).textTheme.labelSmall?.copyWith(
                            color: Theme.of(context).colorScheme.onSurfaceVariant),
                      ),
              ),
            ),
          ),
        ],
      );

  /// Deletes a memo and offers, for as long as the bar is up, to put it back.
  ///
  /// Deleting here is a **swipe**, which is the easiest gesture on this screen to make by
  /// accident: a thumb that wanders sideways while scrolling used to take a memo with it, with
  /// no confirmation and nothing to press afterwards. A confirmation on every swipe would tax
  /// the many that were meant, so the delete stands and the undo is one tap.
  ///
  /// The memo is really gone from the document the whole time; undoing writes it back as a new
  /// edit (see `Vault::undelete`), so nothing here is a pending state that sync could catch
  /// half-done. The core keeps exactly one of these, and drops it when the vault closes.
  Future<void> _deleteWithUndo(FfiMemo memo) => _deleteIdWithUndo(memo.id);

  Future<void> _deleteIdWithUndo(String id) async {
    final messenger = ScaffoldMessenger.of(context);
    await memoDelete(id: id);
    await _reload();
    if (!mounted) return;
    messenger.hideCurrentSnackBar();
    messenger.showSnackBar(SnackBar(
      content: Text(widget.strings.deleted),
      duration: const Duration(seconds: 6),
      action: SnackBarAction(
        label: widget.strings.undo,
        onPressed: () async {
          await memoUndelete();
          if (mounted) await _reload();
        },
      ),
    ));
  }

  /// Recolor, move, look back or delete, on a long press. Deleting is also the swipe, but a
  /// swipe is a gesture nobody is told about: with the sheet offering everything else, a
  /// memo looked as if it could not be deleted at all.
  Future<void> _memoMenu(FfiMemo memo) async {
    final s = widget.strings;
    final action = await showModalBottomSheet<String>(
      context: context,
      builder: (context) => SafeArea(
        child: Column(
          mainAxisSize: MainAxisSize.min,
          children: [
            _sheetTitle(context, rowTitle(memo, s.newMemo)),
            _colorSection(context, s, memo.color),
            const Divider(height: 1),
            ListTile(
              leading: const Icon(Icons.drive_file_move_outline),
              title: Text(s.moveTo),
              onTap: () => Navigator.of(context).pop('move'),
            ),
            ListTile(
              leading: const Icon(Icons.history),
              title: Text(s.history),
              onTap: () => Navigator.of(context).pop('history'),
            ),
            ListTile(
              leading: const Icon(Icons.delete_outline),
              iconColor: Theme.of(context).colorScheme.error,
              textColor: Theme.of(context).colorScheme.error,
              title: Text(s.delete),
              onTap: () => Navigator.of(context).pop('delete'),
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
    } else if (action == 'delete') {
      await _deleteWithUndo(memo);
    } else if (action == 'history') {
      final restored = await Navigator.of(context).push<bool>(MaterialPageRoute(
        builder: (_) => HistoryScreen(
          strings: widget.strings,
          memoId: memo.id,
          // The name the row shows, first line and all: a memo with no title of its own was
          // headed "New memo" in its own history.
          title: rowTitle(memo, widget.strings.newMemo),
          color: memo.color,
        ),
      ));
      if (restored == true && mounted) await _reload();
    }
  }

  /// One row, wearing its palette key: a wash of the color behind it and a saturated stripe
  /// down the leading edge.
  ///
  /// The wash alone is too faint to separate at a glance once the list is long, and the
  /// stripe alone loses to the row's own text; together they are the signal the desktop's
  /// list gives, at a size a thumb scrolls past.
  ///
  /// A card, not a band: rounded, with room around it, the way a list of notes looks in a
  /// notes app rather than in a spreadsheet. The stripe stays, inside the rounding.
  Widget _tinted(BuildContext context, String color, Widget child) => Padding(
        padding: const EdgeInsets.symmetric(horizontal: 12, vertical: 4),
        child: ClipRRect(
          borderRadius: BorderRadius.circular(14),
          child: Material(
            color: paletteRow(color, Theme.of(context).colorScheme.surface),
            child: Container(
              decoration: BoxDecoration(
                border: Border(left: BorderSide(color: paletteSwatch(color), width: 5)),
              ),
              child: child,
            ),
          ),
        ),
      );

  /// What a long-press sheet is about, at its top. The sheet dims the list as it rises, and
  /// with nothing naming the row it came from, "delete" did not say what it would delete.
  Widget _sheetTitle(BuildContext sheetContext, String title) => Padding(
        padding: const EdgeInsets.fromLTRB(16, 16, 16, 0),
        child: Align(
          alignment: Alignment.centerLeft,
          child: Text(
            title,
            maxLines: 1,
            overflow: TextOverflow.ellipsis,
            style: Theme.of(sheetContext)
                .textTheme
                .titleMedium
                ?.copyWith(fontWeight: FontWeight.w700),
          ),
        ),
      );

  /// The palette, under the title of both long-press sheets.
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
  /// Drops the find box's filter, on both sides: the text in the field and the query the
  /// list is rebuilt from.
  ///
  /// Called before anything new appears. A memo or a folder made while a search is on does
  /// not match it, so it is written, saved — and nowhere to be seen. Wanting a new note is
  /// the end of the search that was running.
  void _clearSearch() {
    if (_query.isEmpty) return;
    _search.clear();
    _query = '';
  }

  /// What another app shared, as a new memo: the text (under its subject, when the text
  /// does not already say it) and the picture, if one came. The copy the host made of the
  /// picture is deleted once it is in the vault — or once it is known it will not be.
  Future<void> _addShared(widgets.WidgetRequest shared) async {
    final text = shared.text.trim();
    final subject = shared.subject.trim();
    final body = subject.isEmpty || text.contains(subject)
        ? text
        : text.isEmpty
            ? subject
            : '$subject\n$text';
    _SharedPhoto? photo;
    if (shared.file.isNotEmpty) {
      final file = File(shared.file);
      try {
        final bytes = await file.readAsBytes();
        final ext = switch (shared.mime) {
          'image/png' => 'png',
          'image/gif' => 'gif',
          'image/webp' => 'webp',
          _ => 'jpg',
        };
        photo = _SharedPhoto(bytes, 'shared.$ext', shared.mime);
      } catch (e) {
        debugPrint('could not read the shared picture: $e');
      } finally {
        try {
          await file.delete();
        } catch (_) {}
      }
    }
    if (body.isEmpty && photo == null) return;
    await _add(body: body, photo: photo);
  }

  Future<void> _add({bool withPhoto = false, String body = '', _SharedPhoto? photo}) async {
    _clearSearch();
    final messenger = ScaffoldMessenger.of(context);
    final color = widget.settings.value.defaultColor;
    final String id;
    try {
      id = await memoUpsert(title: '', body: body);
      if (color != defaultColor) await memoSetColor(id: id, color: color);
      if (!_atRoot) await memoSetGroup(id: id, groupId: widget.groupId);
      if (photo != null) {
        final size = await decodeImageSize(photo.bytes);
        await attachmentAdd(
          memoId: id,
          data: photo.bytes,
          name: photo.name,
          mime: photo.mime,
          widthPx: size?.width.toInt() ?? 0,
          heightPx: size?.height.toInt() ?? 0,
        );
      }
    } catch (e) {
      // Without this the button simply did nothing, which reads as a broken app rather
      // than as storage the vault cannot be written to.
      messenger.showSnackBar(SnackBar(content: Text('$e')));
      return;
    }
    if (!mounted) return;
    final result = await Navigator.of(context).push<String>(
      MaterialPageRoute(
        builder: (_) => MemoEditScreen(
          strings: widget.strings,
          id: id,
          title: '',
          body: body,
          color: color,
          pickPhotoOnOpen: withPhoto,
          isNew: true,
        ),
      ),
    );
    if (result == MemoEditScreen.deleteResult) return _deleteIdWithUndo(id);
    // Backing out without writing anything leaves the memo this created behind, and one
    // "New memo" row for every time anyone tapped the button and changed their mind. The
    // core decides — a photo counts as writing.
    try {
      await memoDiscardIfBlank(id: id);
    } catch (e) {
      debugPrint('could not discard the blank memo: $e');
    }
    await _reload();
  }

  Future<void> _open(FfiMemo memo) async {
    final result = await Navigator.of(context).push<String>(
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
    // Deleting from the editor comes back here, where the undo can be offered.
    if (result == MemoEditScreen.deleteResult) return _deleteIdWithUndo(memo.id);
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
        titleTextStyle: Theme.of(context)
            .textTheme
            .headlineSmall
            ?.copyWith(fontWeight: FontWeight.w700, color: Theme.of(context).colorScheme.onSurface),
        actions: [
          // Pairing, settings and the update banner belong to the screen you always start
          // from. The pairing button stays out here because it carries the sync state;
          // everything else is in the menu, so the bar is two things rather than four.
          if (_atRoot) SyncButton(strings: widget.strings, sync: widget.sync),
          PopupMenuButton<String>(
            onSelected: (action) async {
              switch (action) {
                case 'folder':
                  await _newFolder();
                case 'reorder':
                  setState(() => _reordering = !_reordering);
                case 'settings':
                  await _openSettings();
              }
            },
            itemBuilder: (context) => [
              PopupMenuItem(
                value: 'folder',
                child: ListTile(
                  leading: const Icon(Icons.create_new_folder_outlined),
                  title: Text(widget.strings.newGroup),
                  contentPadding: EdgeInsets.zero,
                ),
              ),
              PopupMenuItem(
                value: 'reorder',
                enabled: _query.isEmpty && _memos.length > 1,
                child: ListTile(
                  leading: Icon(_reordering ? Icons.check : Icons.swap_vert),
                  title: Text(_reordering ? widget.strings.reorderDone : widget.strings.reorder),
                  contentPadding: EdgeInsets.zero,
                ),
              ),
              if (_atRoot)
                PopupMenuItem(
                  value: 'settings',
                  child: ListTile(
                    leading: const Icon(Icons.settings_outlined),
                    title: Text(widget.strings.settings),
                    contentPadding: EdgeInsets.zero,
                  ),
                ),
            ],
          ),
        ],
      ),
      body: Column(children: [
        if (_update != null)
          UpdateBanner(strings: widget.strings, release: _update!),
        // Find. Always on screen rather than behind a toolbar icon, the same call the
        // desktop's list makes: a search that has to be discovered is a search the app does
        // not have, and this row costs one line. It looks through the **whole vault**, not
        // the folder being shown — the question is where a memo went, and the answer is
        // useless if it excludes the folders it might have gone into.
        Padding(
          padding: const EdgeInsets.fromLTRB(12, 8, 12, 4),
          child: TextField(
            controller: _search,
            onChanged: _runSearch,
            textInputAction: TextInputAction.search,
            decoration: InputDecoration(
              isDense: true,
              hintText: widget.strings.search,
              prefixIcon: const Icon(Icons.search, size: 20),
              // A pill, filled, no outline: the search bar every current Android app has.
              border: OutlineInputBorder(
                borderRadius: BorderRadius.circular(28),
                borderSide: BorderSide.none,
              ),
              suffixIcon: _query.isEmpty
                  ? null
                  : IconButton(
                      icon: const Icon(Icons.close, size: 20),
                      tooltip: widget.strings.clearSearch,
                      onPressed: () {
                        _search.clear();
                        _runSearch('');
                      },
                    ),
            ),
          ),
        ),
        if (!_loaded)
          const Spacer()
        else if (_folders.isEmpty && _memos.isEmpty)
          Expanded(
            child: Center(
              child: Padding(
                padding: const EdgeInsets.symmetric(horizontal: 32),
                child: Column(
                  mainAxisSize: MainAxisSize.min,
                  children: [
                    // The top level, empty: what someone opening the app for the first time
                    // sees, so it is the app's mark and the one thing to do next, not a line
                    // of text alone in the middle of the screen.
                    if (_query.isEmpty && _atRoot) ...[
                      Opacity(
                        opacity: 0.9,
                        child: Image.asset('assets/logo.png', width: 96, height: 96),
                      ),
                      const SizedBox(height: 16),
                    ] else ...[
                      // An empty folder, or a search that found nothing: a mark for which
                      // of the two it is, rather than a line of text alone on the screen.
                      Icon(
                        _query.isNotEmpty ? Icons.search_off_rounded : Icons.folder_open_rounded,
                        size: 56,
                        color: Theme.of(context).colorScheme.onSurfaceVariant.withValues(alpha: 0.5),
                      ),
                      const SizedBox(height: 12),
                    ],
                    Text(
                      _query.isNotEmpty
                          ? widget.strings.searchNone
                          : _atRoot
                              ? widget.strings.emptyHint
                              : widget.strings.emptyFolder,
                      textAlign: TextAlign.center,
                      style: Theme.of(context).textTheme.bodyLarge,
                    ),
                    if (_query.isEmpty) ...[
                      const SizedBox(height: 16),
                      // In a folder, the memo it makes goes into that folder.
                      FilledButton.icon(
                        onPressed: _add,
                        icon: const Icon(Icons.edit_outlined),
                        label: Text(_atRoot ? widget.strings.firstMemo : widget.strings.newMemo),
                      ),
                    ],
                  ],
                ),
              ),
            ),
          )
        // Searching, the rows are the matches, flat: they come from all over the vault, so
        // there is no arrangement here to drag them into. A plain list, and no handles.
        else if (_query.isNotEmpty)
          Expanded(
            child: ListView.builder(
              padding: EdgeInsets.only(bottom: bottomInset(context) + 88),
              itemCount: _folders.length + _memos.length,
              itemBuilder: (context, i) => i < _folders.length
                  ? _folderTile(_folders[i])
                  : _memoTile(_memos[i - _folders.length], null),
            ),
          )
        else
        Expanded(
          // Pull to merge: what the sync button in the bar used to do, where every list app
          // puts it. The timer and the log watch still do it on their own.
          child: RefreshIndicator(
            onRefresh: _mergeNow,
            child: ReorderableListView.builder(
        // Room for the gesture bar and for the button floating above it, or the last memo
        // in the list is unreachable behind one or the other.
        padding: EdgeInsets.only(bottom: bottomInset(context) + 88),
        // Folders first, then memos — the same order the desktop's tree draws them in.
        itemCount: _folders.length + _memos.length,
        // No drag handle on every row: folders are not arranged this way, and a memo row
        // already answers to a tap, a long press and a swipe. The handle is added by hand to
        // the memo rows below, so the three gestures it already has keep working.
        buildDefaultDragHandles: false,
        onReorderItem: _reorderMemo,
        itemBuilder: (context, i) => i < _folders.length
            ? _folderTile(_folders[i])
            : _memoTile(_memos[i - _folders.length], i),
            ),
          ),
        ),
      ]),
      floatingActionButton: Padding(
        // Scaffold lifts the button off the bottom of the *window*, which edge to edge puts
        // behind the gesture bar.
        padding: EdgeInsets.only(bottom: bottomInset(context)),
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

/// Asks for a folder name, pre-filled when renaming. Null when the user backs out.
Future<String?> _askForName(
  BuildContext context,
  FfiStrings strings,
  String title,
  String initial, {
  /// What the field is for. Folders are what this dialog was written for, so that stays the
  /// default; the vault's own name goes through it too and must not be labelled a folder.
  String? label,
  /// The button's word: what pressing it does ("Create", "Save"), not "OK".
  required String action,
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
          child: Text(action),
        ),
      ],
    ),
  );
}

/// A picture another app shared, read and ready to attach.
class _SharedPhoto {
  const _SharedPhoto(this.bytes, this.name, this.mime);
  final Uint8List bytes;
  final String name;
  final String mime;
}
