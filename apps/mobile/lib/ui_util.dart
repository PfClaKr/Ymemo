/// Small layout helpers shared by the screens.
library;

import 'dart:ui' as ui;

import 'package:flutter/foundation.dart';
import 'package:flutter/material.dart';

/// Height of the system navigation bar (or gesture pill) at the bottom of the screen.
///
/// Scrollables add it to their padding rather than being wrapped in a `SafeArea`: the
/// content still scrolls *under* the translucent bar, which is the point of edge to edge,
/// but the last row can be scrolled clear of it instead of ending up underneath.
double bottomInset(BuildContext context) => MediaQuery.paddingOf(context).bottom;

/// Asks before something a stray tap should not be enough for. True only when [confirm] was
/// pressed; backing out or tapping outside is a no.
Future<bool> confirmAction(
  BuildContext context, {
  required String message,
  required String confirm,
  required String cancel,
}) async {
  final answer = await showDialog<bool>(
    context: context,
    builder: (context) => AlertDialog(
      content: Text(message),
      actions: [
        TextButton(onPressed: () => Navigator.pop(context, false), child: Text(cancel)),
        FilledButton(onPressed: () => Navigator.pop(context, true), child: Text(confirm)),
      ],
    ),
  );
  return answer ?? false;
}

/// A picture's pixel size, or null when it will not decode; the core then assumes 1:1.
Future<ui.Size?> decodeImageSize(Uint8List bytes) async {
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
