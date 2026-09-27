/// A photo on a memo, drawn where it was placed and movable in place.
library;

import 'dart:async';
import 'dart:math' show max, min;

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

import '../host.dart' as host;
import '../src/rust/api.dart';

/// One photo on the note: drag it anywhere, pull its corner to resize, ✕ to detach.
///
/// Nothing is written until the finger lifts — a drag would otherwise leave one entry in the
/// change log per frame. Both numbers that get written are platform-independent: the width
/// in **em**, multiples of the body font, and the position as a **fraction of the note**. A
/// photo half way down a phone screen is half way down the desktop sticky as well.
///
/// In [`flow`] the photo is placed by the column under the writing instead of by its stored
/// corner, so there is nothing to drag — only the resize stays. The corner it *would* go back
/// to is left untouched, so taking it out of the flow puts it where it was.
class NotePhoto extends StatefulWidget {
  const NotePhoto({
    super.key,
    required this.strings,
    required this.attachment,
    required this.canvas,
    required this.baseFont,
    required this.ink,
    required this.selected,
    required this.onSelect,
    required this.onChanged,
    required this.onRemove,
    this.flow = false,
    this.inWriting = false,
    this.placeAt,
    this.onMove,
  });

  final FfiStrings strings;
  final FfiAttachment attachment;

  /// Whether this one sits in the band under the writing rather than on top of it.
  final bool flow;

  /// Whether this one stands **in** the writing, in room the memo makes for it, rather than
  /// lying on it. Placed by [placeAt] — the measured height of the words above it — so it is
  /// no more draggable than one in the flow: the writing is what puts it there.
  final bool inWriting;
  final double? placeAt;

  /// Asked to move into the writing (true) or back out of it. The screen above does this, not
  /// the photo: the room is opened at the caret and in the text the editor is holding, and
  /// neither of those is anything a photo knows about.
  final Future<void> Function(bool intoWriting)? onMove;

  /// Size of the note the photo lies on; positions are a fraction of it.
  final Size canvas;

  /// This platform's body font size, which the stored width in em is measured against.
  final double baseFont;
  final Color ink;
  final bool selected;
  final VoidCallback onSelect;
  final Future<void> Function() onChanged;

  /// Removing the photo. The editor does it, not this widget: a photo in the writing takes
  /// its room with it, which changes the body the editor is holding, and the removal is
  /// offered back from the editor's own snackbar.
  final Future<void> Function() onRemove;

  @override
  State<NotePhoto> createState() => _NotePhotoState();
}

class _NotePhotoState extends State<NotePhoto> {
  /// Smallest a photo may be pulled; below this the handles cover the picture.
  static const double _minW = 44;
  static const double _handle = 30;

  Uint8List? _bytes;
  bool _missing = false;

  /// Live drag offsets, folded into the stored geometry when the finger lifts.
  double _dx = 0;
  double _dy = 0;
  double _dw = 0;

  /// While the photo has not synced, how often to look for it again.
  static const Duration _recheck = Duration(seconds: 3);
  Timer? _waiting;

  @override
  void initState() {
    super.initState();
    _load();
  }

  @override
  void dispose() {
    _waiting?.cancel();
    super.dispose();
  }

  Future<void> _load() async {
    // Before it syncs there are no bytes; say so rather than showing nothing — and keep
    // looking. It used to say so until the memo was closed and opened again, long after the
    // photo had arrived.
    if (!await attachmentHasBlob(hash: widget.attachment.hash)) {
      if (!mounted) return;
      setState(() => _missing = true);
      _waiting?.cancel();
      _waiting = Timer(_recheck, _load);
      return;
    }
    final bytes = await attachmentBytes(hash: widget.attachment.hash);
    if (mounted) {
      setState(() {
        _missing = false;
        _bytes = bytes;
      });
    }
  }

  /// Whether the note decides where this photo goes, rather than the finger.
  bool get _placed => widget.flow || widget.inWriting;

  /// Proportions of the picture, for turning a width into a height.
  double get _ratio {
    final a = widget.attachment;
    return (a.widthPx > 0 && a.heightPx > 0) ? a.heightPx / a.widthPx : 1.0;
  }

  double get _w {
    final stored = widget.attachment.widthEmMilli / 1000.0 * widget.baseFont;
    // Held to the note's width, and — for one lying **on** the writing — to three quarters of
    // its height, the same cap the desktop sticky uses. A floating photo takes the touch, so
    // a tall picture that covered the note end to end left nowhere to put the caret at all:
    // with the keyboard up, the writing is a third of a phone screen and a photo from a phone
    // camera is taller than it is wide. In the flow, or standing in the writing, there is
    // nothing to cap — the picture has room of its own there and hides no line.
    final widest = max(widget.canvas.width, _minW);
    final tallest = widget.flow || widget.inWriting
        ? widest
        : max(widget.canvas.height * 0.75 / max(_ratio, 0.01), _minW);
    return (stored + _dw).clamp(_minW, max(min(widest, tallest), _minW));
  }

  double get _h => _w * _ratio;

  // Never fully off the note: a photo whose corner cannot be reached cannot be brought back.
  double get _x => (widget.attachment.xPermille / 1000.0 * widget.canvas.width + _dx)
      .clamp(0.0, max(widget.canvas.width - _w, 0.0));
  double get _y => (widget.attachment.yPermille / 1000.0 * widget.canvas.height + _dy)
      .clamp(0.0, max(widget.canvas.height - _h, 0.0));

  /// Stores where the photo ended up. The clamped geometry is what is read back, so what is
  /// saved is where the photo actually is and not where the finger went.
  Future<void> _commit() async {
    await attachmentSetLayout(
      id: widget.attachment.id,
      // A photo in the flow is placed by the column, so the corner it would return to is
      // written back unchanged: only its width is the user's to set here.
      xPermille: _placed
          ? widget.attachment.xPermille
          : (_x / max(widget.canvas.width, 1) * 1000).round(),
      yPermille: _placed
          ? widget.attachment.yPermille
          : (_y / max(widget.canvas.height, 1) * 1000).round(),
      widthEmMilli: (_w / widget.baseFont * 1000).round(),
    );
    // Cleared without a setState of their own: reloading rebuilds this widget with the
    // stored values the offsets have just been folded into, and clearing them separately
    // would show the old position for one frame.
    _dx = 0;
    _dy = 0;
    _dw = 0;
    await widget.onChanged();
  }

  @override
  Widget build(BuildContext context) {
    final selected = widget.selected;
    final frame = Stack(
      clipBehavior: Clip.none,
      children: [
        GestureDetector(
          onTap: widget.onSelect,
          onPanStart: (_) => widget.onSelect(),
          // Nowhere to drag one the writing places; move the caret and place it again.
          onPanUpdate: _placed
              ? null
              : (d) => setState(() {
                    _dx += d.delta.dx;
                    _dy += d.delta.dy;
                  }),
          onPanEnd: _placed ? null : (_) => _commit(),
          child: Container(
            width: _w,
            height: _h,
            decoration: BoxDecoration(
              // White behind the picture, nothing behind the placeholder. A picture with
              // transparency in it was showing the note's own colour through its clear parts,
              // which reads as the picture being stained rather than as paper showing through;
              // white is what "nothing here" means everywhere else a picture is shown. The
              // placeholder is not a picture and stays part of the note.
              color: _missing ? null : Colors.white,
              borderRadius: BorderRadius.circular(6),
              border: Border.all(
                color: selected ? widget.ink : widget.ink.withValues(alpha: 0.35),
                width: selected ? 2 : 1,
              ),
            ),
            clipBehavior: Clip.antiAlias,
            child: _picture(),
          ),
        ),
        if (selected) ..._furniture(),
      ],
    );
    // In the flow the column places it; standing in the writing, the writing does; on top of
    // it, it places itself. The padding is for the controls, which hang outside the frame.
    if (widget.flow) {
      return Padding(
        padding: const EdgeInsets.fromLTRB(0, 12, 12, 4),
        child: SizedBox(width: _w, height: _h, child: frame),
      );
    }
    if (widget.inWriting) {
      return Positioned(
        left: 0,
        top: widget.placeAt ?? 0,
        width: _w,
        height: _h,
        child: frame,
      );
    }
    return Positioned(left: _x, top: _y, width: _w, height: _h, child: frame);
  }

  Widget _picture() {
    if (_missing) {
      return Center(
        child: Padding(
          padding: const EdgeInsets.all(4),
          child: Text(
            widget.strings.photoMissing,
            textAlign: TextAlign.center,
            style: Theme.of(context).textTheme.bodySmall,
          ),
        ),
      );
    }
    if (_bytes == null) {
      return const Center(child: CircularProgressIndicator());
    }
    // Decoded at the size it is drawn, not the size it was taken: a phone camera's picture is
    // twelve megapixels, about 48 MB once decoded, for a photo a note shows a few hundred
    // pixels wide. Rounded up to a step so dragging the resize handle does not decode it
    // again on every frame, and never past the original, which would only upscale it.
    return LayoutBuilder(builder: (context, box) {
      final original = widget.attachment.widthPx.toInt();
      int? decodeWidth;
      if (box.maxWidth.isFinite) {
        final wanted = box.maxWidth * MediaQuery.devicePixelRatioOf(context);
        decodeWidth = ((wanted / 256).ceil() * 256);
        if (original > 0 && decodeWidth > original) decodeWidth = null;
      }
      return Image.memory(
        _bytes!,
        fit: BoxFit.cover,
        cacheWidth: decodeWidth,
        // The old decode stays up while a new size is made, instead of a blank frame.
        gaplessPlayback: true,
      );
    });
  }

  /// Detach and resize, shown only on the selected photo so the picture is not permanently
  /// covered by its own controls. They hang half outside the frame, where a fingertip
  /// reaches them without hiding what it is about to change.
  List<Widget> _furniture() => [
        Positioned(
          right: -_handle / 3,
          top: -_handle / 3,
          child: Semantics(
            label: widget.strings.photoRemove,
            button: true,
            child: GestureDetector(
              onTap: widget.onRemove,
              child: _chip(const Color(0xFFD64541), Icons.close),
            ),
          ),
        ),
        // Keep a copy of the picture outside the vault. The system's own picker asks where;
        // nothing is written until it is answered.
        Positioned(
          right: _handle * 5 / 3,
          top: -_handle / 3,
          child: Semantics(
            label: widget.strings.photoSave,
            button: true,
            child: GestureDetector(onTap: _save, child: _chip(widget.ink, Icons.save_alt)),
          ),
        ),
        // Move it between the two ways of sitting. Next to the detach, and labelled with
        // what it will do rather than with what the photo is now.
        Positioned(
          right: _handle * 2 / 3,
          top: -_handle / 3,
          child: Semantics(
            label: _placed
                ? widget.strings.photoOverText
                : widget.strings.photoUnderText,
            button: true,
            child: GestureDetector(
              onTap: () async => widget.onMove?.call(!_placed),
              child: _chip(
                widget.ink,
                _placed ? Icons.flip_to_front : Icons.vertical_align_bottom,
              ),
            ),
          ),
        ),
        Positioned(
          right: -_handle / 3,
          bottom: -_handle / 3,
          child: Semantics(
            label: widget.strings.photoSize,
            child: GestureDetector(
              // Width only; the height follows the original aspect ratio.
              onPanUpdate: (d) => setState(() => _dw += d.delta.dx),
              onPanEnd: (_) => _commit(),
              child: _chip(widget.ink, Icons.open_in_full),
            ),
          ),
        ),
      ];

  /// Writes the photo wherever the user says, under the name it was attached with.
  ///
  /// The bytes are read from the vault rather than from what is on screen: what gets saved is
  /// the original file, not the size it happens to be drawn at.
  Future<void> _save() async {
    final messenger = ScaffoldMessenger.of(context);
    try {
      final bytes = await attachmentBytes(hash: widget.attachment.hash);
      final ok = await host.saveAs(
        name: widget.attachment.name.isEmpty ? 'photo.jpg' : widget.attachment.name,
        mime: widget.attachment.mime,
        bytes: bytes,
      );
      messenger.showSnackBar(SnackBar(
        content: Text(ok ? widget.strings.photoSaved : widget.strings.photoSaveFailed),
      ));
    } catch (e) {
      // A photo that has not synced to this device yet has no bytes to save.
      messenger.showSnackBar(SnackBar(content: Text('$e')));
    }
  }

  Widget _chip(Color background, IconData icon) => Container(
        width: _handle,
        height: _handle,
        decoration: BoxDecoration(color: background, shape: BoxShape.circle),
        child: Icon(icon, size: 16, color: Colors.white),
      );
}
