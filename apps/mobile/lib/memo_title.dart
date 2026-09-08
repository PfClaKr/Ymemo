/// How a memo reads when it has no title of its own.
///
/// The phone has a title field and leaving it empty is the ordinary way to use it; a sticky
/// on the desktop has no such field and reads by the first line of what is written on it.
/// Without this, a phoneful of memos arrives as a column of "New memo" — in the list and on
/// the home screen alike. Nothing here changes a memo: the stored title stays empty.
library;

import 'src/rust/api.dart';

/// The first line of `text` worth naming a memo by, capped the way a sticky's derived title
/// is.
///
/// Fence lines are skipped: a memo that opens with ` ```rust ` is about what is inside it,
/// and calling it "```rust" in the list says nothing at all.
///
/// **Must match `derive_title` in the desktop's `sticky.rs`**, so the same memo reads the
/// same on both devices.
String firstLine(String text) {
  final line = text
      .split('\n')
      .firstWhere((l) => l.trim().isNotEmpty && !l.trimLeft().startsWith('```'),
          orElse: () => '')
      .trim();
  // Code points, not grapheme clusters: `chars().take(40)` on the other side counts the
  // same units, and a title that reads one way on the phone and another on the desktop is
  // exactly what this file exists to prevent.
  return String.fromCharCodes(line.runes.take(40));
}

/// What a memo's row is headed by. `fallback` is used when there is no writing at all.
String rowTitle(FfiMemo memo, String fallback) =>
    headingFor(memo.title, memo.body, fallback);

/// [`rowTitle`] for a memo held as two loose fields, which is how the editor has it.
String headingFor(String title, String body, String fallback) {
  if (title.isNotEmpty) return title;
  final line = firstLine(body);
  return line.isEmpty ? fallback : line;
}

/// The line under the title: the body, minus the part of it the title is already showing.
String rowPreview(FfiMemo memo) {
  if (memo.title.isNotEmpty) return memo.body;
  final line = firstLine(memo.body);
  if (line.isEmpty) return memo.body;
  return memo.body.replaceFirst(line, '').trim();
}
