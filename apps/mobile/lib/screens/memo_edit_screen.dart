/// Editing one memo: its title, its body and the photos on it.
library;

import 'dart:async';
import 'dart:math' show max;

import 'package:flutter/material.dart';
import 'package:image_picker/image_picker.dart';

import '../host.dart' as host;
import '../markdown_style.dart';
import '../memo_title.dart';
import '../palette.dart';
import '../pending_edits.dart';
import '../src/rust/api.dart';
import '../ui_util.dart';
import '../widgets/note_photo.dart';
import 'history_screen.dart';

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
    this.isNew = false,
  });

  final FfiStrings strings;
  final String id;
  final String title;
  final String body;

  /// Opens the photo picker as soon as the editor is up, for the two ways of starting a
  /// memo that are about a photo rather than about text.
  final bool pickPhotoOnOpen;

  /// The list created this memo for the editor. Left blank, it is discarded on the way out —
  /// also when the way out is the app being left, which never comes back through the list.
  final bool isNew;

  /// Palette key the memo arrived with; the editor wears it the way the desktop's sticky
  /// does, so the same memo looks like the same memo on either device.
  final String color;

  /// What the editor pops with when the memo is to be deleted; the list does the deleting,
  /// because the list is where the undo can be offered.
  static const deleteResult = 'delete';

  @override
  State<MemoEditScreen> createState() => _MemoEditScreenState();
}

class _MemoEditScreenState extends State<MemoEditScreen> {
  late final TextEditingController _title = TextEditingController(text: widget.title);
  /// The body draws its own markdown as it is typed; the text it holds is the plain string
  /// with every marker still in it, which is what gets saved. See `markdown_style.dart`.
  late final MarkdownEditingController _body = MarkdownEditingController(
    text: widget.body,
    marker: paletteInk(_color).withValues(alpha: 0.45),
    codeBackground: paletteInk(_color).withValues(alpha: 0.10),
  );
  late String _color = widget.color;
  List<FfiAttachment> _photos = [];

  /// What is known to be in the vault. Not `widget.title`/`widget.body`: those are what the
  /// screen opened with, and opening room for a photo changes the stored note underneath it,
  /// so comparing against them would call a real edit "nothing to save".
  late String _savedTitle = widget.title;
  late String _savedBody = widget.body;

  /// The writing's own scroll, so a photo standing in it travels with the words when the note
  /// is longer than the screen.
  final ScrollController _bodyScrollCtl = ScrollController();

  /// Writes the note back shortly after typing stops, but **only** while a photo is standing
  /// in it. A photo in the writing is anchored to a line number, and `Vault::upsert` is what
  /// moves that anchor when lines are added above it — so without a save the picture sits at
  /// the line it was put on while the words slide past it, until the screen is left. The
  /// desktop gets this from the debounce it already saves on; the phone otherwise only writes
  /// on the way out, which is why this exists at all rather than being the same timer.
  Timer? _followTimer;
  double get _bodyScroll => _bodyScrollCtl.hasClients ? _bodyScrollCtl.offset : 0;

  /// The style the body is actually set in, filled in on every build. Everything that has to
  /// reason in lines measures with this rather than with a guess at the font.
  TextStyle _bodyStyle = const TextStyle(fontSize: 14);

  /// The first [n] lines of [text], with no trailing newline: exactly the words above a photo.
  String _linesBefore(String text, int n) {
    if (n <= 0) return '';
    var seen = 0;
    for (var i = 0; i < text.length; i++) {
      if (text.codeUnitAt(i) == 0x0a) {
        seen++;
        if (seen == n) return text.substring(0, i);
      }
    }
    return text;
  }

  /// How tall [prefix] is, laid out the way the field lays it out. This is the measurement
  /// the whole arrangement rests on — a wrapped line is two lines on screen and one in the
  /// text, so nothing here counts.
  double _writingHeight(String prefix, double width) {
    if (prefix.isEmpty) return 0;
    final painter = TextPainter(
      text: TextSpan(text: prefix, style: _bodyStyle),
      textDirection: TextDirection.ltr,
    )..layout(maxWidth: width);
    return painter.height;
  }

  /// Which photo shows its move/resize/detach handles. There is no hovering on a phone, so
  /// a photo has to be tapped before its controls appear; tapping the text puts them away.
  String? _selectedPhoto;

  /// Writes the new palette key straight through and repaints.
  ///
  /// Not batched into `_save` with the text: the color *is* what the screen looks like, and a
  /// swatch that did nothing until you left would read as a broken button.
  Future<void> _setColor(String color) async {
    setState(() {
      _color = color;
      // The markdown is drawn in the note's own ink, so it follows the paper.
      _body.marker = paletteInk(color).withValues(alpha: 0.45);
      _body.codeBackground = paletteInk(color).withValues(alpha: 0.10);
    });
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

  /// Saves a moment after typing stops, the way the desktop's sticky does: the editor used to
  /// write only on the way back, so anything that took the app away first took the text too.
  Timer? _autosave;

  /// What `YmemoApp` calls before it takes the vault away; see [PendingEdits].
  late final Future<void> Function() _flushHook = _flushBeforeLeaving;

  Future<void> _flushBeforeLeaving() async {
    _autosave?.cancel();
    await _save();
    // The same as backing out of a new memo left blank (the list's `_add`), for the way out
    // that never returns to the list.
    if (widget.isNew && _title.text.trim().isEmpty && _body.text.trim().isEmpty) {
      await memoDiscardIfBlank(id: widget.id);
    }
  }

  /// Saves, shows the memo's past, and — if a version was put back — takes the restored text
  /// into the fields, which would otherwise write the pre-restore words straight back.
  Future<void> _openHistory() async {
    if (!await _save() || !mounted) return;
    final restored = await Navigator.of(context).push<bool>(MaterialPageRoute(
      builder: (_) => HistoryScreen(
        strings: widget.strings,
        memoId: widget.id,
        title: _title.text,
        color: _color,
      ),
    ));
    if (restored != true || !mounted) return;
    final memo = (await memoList()).where((m) => m.id == widget.id).firstOrNull;
    if (memo == null || !mounted) return;
    _savedTitle = memo.title;
    _savedBody = memo.body;
    _title.text = memo.title;
    _body.text = memo.body;
    setState(() => _color = memo.color);
    await _reloadPhotos();
  }

  void _scheduleAutosave() {
    _autosave?.cancel();
    _autosave = Timer(const Duration(milliseconds: 800), () {
      if (mounted) _save();
    });
  }

  @override
  void initState() {
    super.initState();
    PendingEdits.attach(_flushHook);
    _title.addListener(_scheduleAutosave);
    _body.addListener(_scheduleAutosave);
    // A photo standing in the writing is drawn at a height measured from the top of the text,
    // so it has to be redrawn when the text scrolls under it — otherwise it stays where it is
    // while its words move away.
    _bodyScrollCtl.addListener(() {
      if (mounted && _photos.any((p) => p.mode == FfiPhotoMode.inWriting)) {
        setState(() {});
      }
    });
    _body.addListener(() {
      if (!_photos.any((p) => p.mode == FfiPhotoMode.inWriting)) return;
      _followTimer?.cancel();
      _followTimer = Timer(const Duration(milliseconds: 700), () async {
        if (!mounted) return;
        await _save();
        await _reloadPhotos();
      });
    });
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

  /// Removes a photo and offers it back for a moment, the way a deleted memo is: the history
  /// does not cover photos, so without the offer a stray tap on the ✕ lost the picture.
  ///
  /// Saved first, for the reason [_movePhoto] is: a photo in the writing takes its room with
  /// it, and the room is closed in what the core has stored.
  Future<void> _removePhoto(FfiAttachment photo) async {
    final messenger = ScaffoldMessenger.of(context);
    try {
      await memoUpsert(id: widget.id, title: _title.text, body: _body.text);
      _showBody(await attachmentRemove(id: photo.id));
    } catch (e) {
      messenger.showSnackBar(SnackBar(content: Text('$e')));
      return;
    }
    if (_selectedPhoto == photo.id) _selectedPhoto = null;
    await _reloadPhotos();
    messenger.hideCurrentSnackBar();
    messenger.showSnackBar(SnackBar(
      content: Text(widget.strings.photoRemoved),
      action: SnackBarAction(label: widget.strings.undo, onPressed: _restorePhoto),
    ));
  }

  Future<void> _restorePhoto() async {
    try {
      await memoUpsert(id: widget.id, title: _title.text, body: _body.text);
      final body = await attachmentRestore();
      if (body != null) _showBody(body);
    } catch (e) {
      if (mounted) {
        ScaffoldMessenger.of(context).showSnackBar(SnackBar(content: Text('$e')));
      }
      return;
    }
    await _reloadPhotos();
  }

  /// The body the core now holds, after it opened or closed a photo's room. Left alone when
  /// nothing moved, so the caret stays where it was.
  void _showBody(String body) {
    if (mounted && _body.text != body) _body.text = body;
    _savedBody = body;
  }

  /// Moves a photo into the writing at the caret, or takes it back out.
  ///
  /// The memo is written **first**. The room is opened in whatever the core has stored, and
  /// what is on screen is newer than that until the editor is left — so without this the gap
  /// would be cut into an older version of the note and then overwritten by the newer one on
  /// the way out, leaving a photo anchored to a line nobody made room for.
  Future<void> _movePhoto(FfiAttachment photo, {required bool intoWriting}) async {
    final messenger = ScaffoldMessenger.of(context);
    try {
      await memoUpsert(id: widget.id, title: _title.text, body: _body.text);
      final body = intoWriting
          ? await attachmentPlaceInWriting(
              id: photo.id,
              afterLine: _caretLine(),
              rows: _rowsFor(photo),
            )
          : await attachmentTakeOutOfWriting(id: photo.id);
      // The body under the editor just changed — blank lines were opened or closed in it.
      // It is also exactly what the vault now holds, so nothing is pending on it.
      if (mounted) _body.text = body;
      _savedBody = body;
    } catch (e) {
      messenger.showSnackBar(SnackBar(content: Text('$e')));
      return;
    }
    await _reloadPhotos();
  }

  /// Which line a picture goes on: **its own line, under whatever the caret sits after.**
  ///
  /// With the caret at the end of a line that is the line below it — somebody who has just
  /// written a line and reached for a picture wants it under what they wrote. With the caret
  /// at the start of one, which is where return leaves it, that is the line itself, so the
  /// empty line just made becomes the room instead of a blank line above the room. The end of
  /// the note when the caret has never been in it, which is where someone who has pointed at
  /// nothing would expect a picture to land. Matches `line_for_caret` on the desktop.
  int _caretLine() {
    final text = _body.text;
    final caret = _body.selection.baseOffset;
    final at = (caret < 0 || caret > text.length) ? text.length : caret;
    final breaks = '\n'.allMatches(text.substring(0, at)).length;
    final atLineStart = at == 0 || text.codeUnitAt(at - 1) == 0x0a;
    return atLineStart ? breaks : breaks + 1;
  }

  /// How many blank lines a photo needs to stand in, at this screen's line height. Rounded up:
  /// a line of writing peeping out from under a picture reads as a bug where a sliver of blank
  /// paper reads as spacing.
  int _rowsFor(FfiAttachment photo) {
    final line = _writingHeight('X', double.infinity);
    final width = photo.widthEmMilli / 1000.0 * (_bodyStyle.fontSize ?? 14.0);
    final ratio = (photo.widthPx > 0 && photo.heightPx > 0)
        ? photo.heightPx / photo.widthPx
        : 1.0;
    if (line <= 0) return 1;
    return max(1, (width * ratio / line).ceil());
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
    final size = await decodeImageSize(bytes);
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
    PendingEdits.detach(_flushHook);
    _autosave?.cancel();
    _followTimer?.cancel();
    _title.dispose();
    _body.dispose();
    _bodyScrollCtl.dispose();
    super.dispose();
  }

  /// Skips the write when nothing changed; an empty change is pure sync traffic.
  ///
  /// Returns whether the memo is safely in the vault. A write can fail — a full disk, or
  /// storage the app cannot reach — and leaving the screen on a false would throw away what
  /// is typed in it with nothing said, so the caller stays put and shows why. The core's
  /// message says what went wrong; it is shown as it is.
  Future<bool> _save() async {
    if (_title.text == _savedTitle && _body.text == _savedBody) return true;
    final messenger = ScaffoldMessenger.of(context);
    try {
      await memoUpsert(id: widget.id, title: _title.text, body: _body.text);
      _savedTitle = _title.text;
      _savedBody = _body.text;
      return true;
    } catch (e) {
      messenger.showSnackBar(SnackBar(content: Text('$e')));
      return false;
    }
  }

  @override
  Widget build(BuildContext context) {
    final base = Theme.of(context);
    final ink = paletteInk(_color);
    // What the body is really set in, for the measuring above.
    _bodyStyle = base.textTheme.bodyLarge ?? const TextStyle(fontSize: 16);
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
        if (await _save()) navigator.pop();
      },
      child: Scaffold(
        backgroundColor: paletteBg(_color),
        appBar: AppBar(
          // The memo's own title, as the list shows it — the bar said "New memo" over every
          // memo ever opened, including ones written months ago. Fixed at the title it
          // arrived with rather than following the field below it, which is right there.
          // A memo with no title of its own reads by its first line, the same fallback the
          // list uses, or the two would name the same memo differently.
          title: Text(headingFor(widget.title, widget.body, widget.strings.newMemo)),
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
                final saved = await _save();
                if (saved && context.mounted) Navigator.of(context).pop();
              },
            ),
            // The memo's past and its deletion, which were only reachable from the list's
            // long press — somewhere nobody looks from inside a memo.
            PopupMenuButton<String>(
              onSelected: (action) async {
                if (action == 'history') {
                  await _openHistory();
                } else if (action == 'share') {
                  // What is on screen, not what was last saved: the words being looked at.
                  await host.shareText(title: _title.text.trim(), text: _body.text);
                } else if (action == MemoEditScreen.deleteResult) {
                  _autosave?.cancel();
                  Navigator.of(context).pop(MemoEditScreen.deleteResult);
                }
              },
              itemBuilder: (context) => [
                PopupMenuItem(value: 'share', child: Text(widget.strings.share)),
                PopupMenuItem(value: 'history', child: Text(widget.strings.history)),
                PopupMenuItem(
                  value: MemoEditScreen.deleteResult,
                  child: Text(widget.strings.delete),
                ),
              ],
            ),
          ],
        ),
        body: Padding(
          padding: EdgeInsets.fromLTRB(16, 16, 16, 16 + bottomInset(context)),
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.stretch,
            children: [
              TextField(
                controller: _title,
                decoration: InputDecoration(labelText: widget.strings.titleHint),
                textInputAction: TextInputAction.next,
                // A memo that has not been written yet opens ready to be written in — the
                // keyboard used to need a tap of its own before a new note could be started.
                // Only a new one: on an existing memo the keyboard would cover half of what
                // the user came back to read, and not when the photo picker is about to open
                // over the top of it.
                autofocus: widget.title.isEmpty && widget.body.isEmpty && !widget.pickPhotoOnOpen,
              ),
              const SizedBox(height: 12),
              // The note itself: text underneath, photos lying on top of it wherever they
              // were dropped. One surface rather than a column of text followed by a column
              // of pictures — the same arrangement as the desktop sticky, and the position
              // each photo is given here travels to it.
              Expanded(
                child: LayoutBuilder(
                  builder: (context, box) {
                    final baseFont = DefaultTextStyle.of(context).style.fontSize ?? 14.0;
                    final floating = _photos
                        .where((p) => p.mode == FfiPhotoMode.float)
                        .toList();
                    final flowing =
                        _photos.where((p) => p.mode == FfiPhotoMode.flow).toList();
                    final inWriting = _photos
                        .where((p) => p.mode == FfiPhotoMode.inWriting)
                        .toList();
                    // The writing, and under it the photos that asked not to be written
                    // over. A column rather than one surface, because that *is* the
                    // difference between the two modes: what is in this column cannot have
                    // text behind it. Same arrangement as the desktop sticky.
                    return Column(
                      crossAxisAlignment: CrossAxisAlignment.stretch,
                      children: [
                        Expanded(
                          child: LayoutBuilder(
                            builder: (context, area) {
                              final canvas = Size(area.maxWidth, area.maxHeight);
                              return Stack(
                                children: [
                                  Positioned.fill(
                                    child: TextField(
                                      controller: _body,
                                      scrollController: _bodyScrollCtl,
                                      style: _bodyStyle,
                                      decoration: InputDecoration(
                                        // No padding of its own: a photo standing in the
                                        // writing is placed at a height measured from the
                                        // top of the text, so the text has to start there.
                                        contentPadding: EdgeInsets.zero,
                                        hintText: widget.strings.bodyHint,
                                        // The hint is also the only place the app says what
                                        // ``` does, so it has room to say it.
                                        hintMaxLines: 3,
                                        border: InputBorder.none,
                                      ),
                                      maxLines: null,
                                      expands: true,
                                      textAlignVertical: TextAlignVertical.top,
                                      // Writing puts the photo handles away; they would
                                      // otherwise sit over the line being typed.
                                      onTap: () =>
                                          setState(() => _selectedPhoto = null),
                                    ),
                                  ),
                                  // Standing **in** the writing: drawn at the height of the
                                  // words above it, measured with the very style the field
                                  // sets them in. Counting lines would be wrong — a line that
                                  // wrapped is one line in the text and two on the screen —
                                  // and the scroll offset keeps the picture with its words
                                  // once the note is longer than the screen.
                                  for (final photo in inWriting)
                                    NotePhoto(
                                      key: ValueKey(photo.id),
                                      strings: widget.strings,
                                      attachment: photo,
                                      inWriting: true,
                                      placeAt: _writingHeight(
                                            _linesBefore(
                                                _body.text, photo.anchorLine),
                                            canvas.width,
                                          ) -
                                          _bodyScroll,
                                      canvas: canvas,
                                      baseFont: baseFont,
                                      ink: ink,
                                      selected: _selectedPhoto == photo.id,
                                      onSelect: () =>
                                          setState(() => _selectedPhoto = photo.id),
                                      onChanged: _reloadPhotos,
                                      onRemove: () => _removePhoto(photo),
                                      onMove: (into) =>
                                          _movePhoto(photo, intoWriting: into),
                                    ),
                                  for (final photo in floating)
                                    NotePhoto(
                                      key: ValueKey(photo.id),
                                      strings: widget.strings,
                                      attachment: photo,
                                      canvas: canvas,
                                      baseFont: baseFont,
                                      ink: ink,
                                      selected: _selectedPhoto == photo.id,
                                      onSelect: () =>
                                          setState(() => _selectedPhoto = photo.id),
                                      onChanged: _reloadPhotos,
                                      onRemove: () => _removePhoto(photo),
                                      onMove: (into) =>
                                          _movePhoto(photo, intoWriting: into),
                                    ),
                                ],
                              );
                            },
                          ),
                        ),
                        // The band never takes more than half the note, and scrolls inside
                        // what it is given. Without the cap it took its full height first and
                        // the writing lived on the remainder — so with the keyboard up, which
                        // is most of the time on a phone, a photo from a phone camera left no
                        // room to type in at all. Reported, and the reason for the cap on a
                        // floating photo in `NotePhoto` too.
                        if (flowing.isNotEmpty)
                          ConstrainedBox(
                            constraints:
                                BoxConstraints(maxHeight: box.maxHeight * 0.5),
                            child: SingleChildScrollView(
                            child: Column(
                              crossAxisAlignment: CrossAxisAlignment.start,
                              children: [
                                for (final photo in flowing)
                                  NotePhoto(
                                    key: ValueKey(photo.id),
                                    strings: widget.strings,
                                    attachment: photo,
                                    flow: true,
                                    // The band is as wide as the note; the width the core
                                    // stores is what caps the picture inside it.
                                    canvas: Size(box.maxWidth, box.maxHeight),
                                    baseFont: baseFont,
                                    ink: ink,
                                    selected: _selectedPhoto == photo.id,
                                    onSelect: () =>
                                        setState(() => _selectedPhoto = photo.id),
                                    onChanged: _reloadPhotos,
                                    onRemove: () => _removePhoto(photo),
                                    onMove: (into) =>
                                        _movePhoto(photo, intoWriting: into),
                                  ),
                              ],
                            ),
                          ),
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
