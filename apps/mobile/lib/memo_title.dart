/// How a memo reads when it has no title of its own.
///
/// The phone has a title field and leaving it empty is the ordinary way to use it; a sticky
/// on the desktop has no such field and reads by the first line of what is written on it.
/// Without this, a phoneful of memos arrives as a column of "New memo" — in the list and on
/// the home screen alike. Nothing here changes a memo: the stored title stays empty.
library;

import 'package:flutter/widgets.dart';

import 'src/rust/api.dart';

/// The first non-empty line of `text`, capped the way a sticky's derived title is.
///
/// **Must match `derive_title` in the desktop's `sticky.rs`**, so the same memo reads the
/// same on both devices.
String firstLine(String text) {
  final line = text
      .split('\n')
      .firstWhere((l) => l.trim().isNotEmpty, orElse: () => '')
      .trim();
  return line.characters.take(40).toString();
}

/// What a memo's row is headed by. `fallback` is used when there is no writing at all.
String rowTitle(FfiMemo memo, String fallback) {
  if (memo.title.isNotEmpty) return memo.title;
  final line = firstLine(memo.body);
  return line.isEmpty ? fallback : line;
}

/// The line under the title: the body, minus the part of it the title is already showing.
String rowPreview(FfiMemo memo) {
  if (memo.title.isNotEmpty) return memo.body;
  final line = firstLine(memo.body);
  if (line.isEmpty) return memo.body;
  return memo.body.replaceFirst(line, '').trim();
}
