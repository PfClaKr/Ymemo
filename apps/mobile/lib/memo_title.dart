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
/// and calling it "```rust" in the list says nothing at all. A heading's hashes go with them,
/// but only inside a bare fence — there `# 회의록` is *drawn* as a heading saying 회의록, so
/// that is the name; anywhere else a hash is a character and stays.
///
/// **Must match `derive_title` in the desktop's `sticky.rs`**, so the same memo reads the
/// same on both devices.
String firstLine(String text) {
  final line = titleLine(text);
  // Code points, not grapheme clusters: `chars().take(40)` on the other side counts the
  // same units, and a title that reads one way on the phone and another on the desktop is
  // exactly what this file exists to prevent.
  return String.fromCharCodes(line.runes.take(40));
}

/// The line a title is taken from, as it should read rather than as it was typed.
///
/// The same walk `markdown_style.dart` does over the fences, and it has to stay the same one.
String titleLine(String text) {
  bool? fence; // true inside a bare fence, false inside one that named a language
  for (final raw in text.split('\n')) {
    final left = raw.trimLeft();
    if (left.startsWith('```')) {
      fence = fence != null ? null : left.substring(3).trim().isEmpty;
      continue;
    }
    // Left-trimmed first and only then stripped: `# ` has to still have its space when the
    // hashes are counted, or it is not a heading and the memo is called "#".
    final named = (fence == true ? _stripHeading(left) : left).trim();
    // A heading with no words names nothing; the line below is asked instead.
    if (named.isNotEmpty) return named;
  }
  return '';
}

/// `# Title` -> `Title`. Anything that is not a heading comes back whole.
String _stripHeading(String line) {
  var hashes = 0;
  while (hashes < line.length && line.codeUnitAt(hashes) == 0x23) {
    hashes++;
  }
  if (hashes == 0 || hashes > 6) return line;
  if (hashes >= line.length || line.codeUnitAt(hashes) != 0x20) return line;
  return line.substring(hashes + 1).trimLeft();
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
  // The line as it stands in the body, not as the title reads it: a heading's own hashes are
  // dropped from the name, and taking the name back out would leave them behind.
  final line = _rawTitleLine(memo.body);
  if (line.isEmpty) return memo.body;
  return memo.body.replaceFirst(line, '').trim();
}

/// The whole line the title came from, hashes and all.
String _rawTitleLine(String text) {
  bool? fence;
  for (final raw in text.split('\n')) {
    final left = raw.trimLeft();
    if (left.startsWith('```')) {
      fence = fence != null ? null : left.substring(3).trim().isEmpty;
      continue;
    }
    if ((fence == true ? _stripHeading(left) : left).trim().isNotEmpty) return raw.trim();
  }
  return '';
}
