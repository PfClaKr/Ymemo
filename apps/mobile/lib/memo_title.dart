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

/// The line under the title: the writing after it, on one line — the same stretch the
/// desktop's list shows beside a title (`preview_of` in its list module).
///
/// Fence lines are left out, being markup rather than writing, and so is the line the title
/// was taken from. It used to be the body as stored, minus the title line: a memo that opened
/// with a fence showed a row of nothing but ``` under its name, because the row draws one line.
String rowPreview(FfiMemo memo) {
  final out = StringBuffer();
  var first = true;
  for (final raw in memo.body.split('\n')) {
    final line = raw.trim();
    if (line.isEmpty || line.startsWith('```')) continue;
    if (first) {
      first = false;
      final named = line.replaceFirst(RegExp(r'^#+'), '').trim();
      if (memo.title.isEmpty || named.startsWith(memo.title.trim())) continue;
    }
    if (out.isNotEmpty) out.write(' ');
    out.write(_plainLine(line));
    if (out.length >= 80) break;
  }
  return String.fromCharCodes(out.toString().runes.take(80));
}

/// One line as a preview reads it: a heading's hashes, a list's bullet and the emphasis marks
/// are markup, and in a line of grey text under a title they are noise. `plain_line` on the
/// desktop.
String _plainLine(String line) {
  var plain = line.replaceFirst(RegExp(r'^#+'), '').trimLeft();
  for (final mark in const ['- ', '* ', '+ ', '> ']) {
    if (plain.startsWith(mark)) {
      plain = plain.substring(mark.length);
      break;
    }
  }
  return plain.replaceAll('**', '').replaceAll('__', '').replaceAll('`', '');
}

/// When a memo was last written, the way a person would say it: "방금", "5분 전", "3시간 전",
/// "어제", then the date, with the year only once it is not this one. The phrasing is the
/// catalog's, shared with the desktop's list (`relative_time` in its list module).
String relativeTime(int millis, FfiStrings s, {DateTime? now}) {
  final t = DateTime.fromMillisecondsSinceEpoch(millis);
  final at = now ?? DateTime.now();
  final secs = at.difference(t).inSeconds;
  final days = DateTime(at.year, at.month, at.day).difference(DateTime(t.year, t.month, t.day)).inDays;
  String fill(String template, Map<String, int> values) =>
      values.entries.fold(template, (out, e) => out.replaceAll('{${e.key}}', '${e.value}'));
  if (secs < 60) return s.timeNow;
  if (secs < 3600) return fill(s.timeMinutes, {'n': secs ~/ 60});
  if (days == 0) return fill(s.timeHours, {'n': secs ~/ 3600});
  if (days == 1) return s.timeYesterday;
  if (t.year == at.year) return fill(s.timeDate, {'month': t.month, 'day': t.day});
  return fill(s.timeDateYear, {'year': t.year, 'month': t.month, 'day': t.day});
}

/// When a revision was made: "오늘 13:04", "어제 18:20", "9월 27일 13:04", with the year only
/// once it is not this one — the desktop history window's phrasing (`revision_time`).
String revisionTime(int millis, FfiStrings s, {DateTime? now}) {
  final t = DateTime.fromMillisecondsSinceEpoch(millis);
  final at = now ?? DateTime.now();
  String two(int n) => n.toString().padLeft(2, '0');
  final time = '${two(t.hour)}:${two(t.minute)}';
  final days = DateTime(at.year, at.month, at.day).difference(DateTime(t.year, t.month, t.day)).inDays;
  String fill(String template, Map<String, Object> values) =>
      values.entries.fold(template, (out, e) => out.replaceAll('{${e.key}}', '${e.value}'));
  if (days == 0) return fill(s.whenToday, {'time': time});
  if (days == 1) return fill(s.whenYesterday, {'time': time});
  if (t.year == at.year) return fill(s.whenDate, {'month': t.month, 'day': t.day, 'time': time});
  return fill(s.whenDateYear, {'year': t.year, 'month': t.month, 'day': t.day, 'time': time});
}

/// Where in `text` the search `query` was found: a few words before the first match, more
/// after, on one line, with "…" where it was cut — as three parts, so the caller can set the
/// match apart. Null when there is no match. The desktop builds the same stretch
/// (`search_snippet` in its list module).
({String before, String match, String after})? searchSnippet(String text, String query) {
  const before = 12, after = 40;
  final q = query.trim().toLowerCase();
  if (q.isEmpty) return null;
  // Fence lines left out (markup, not writing) and every run of whitespace one space, the
  // same as the desktop's `searchable`: an indented code line opened a gap before the match.
  final flat = text
      .split('\n')
      .where((l) => !l.trimLeft().startsWith('```'))
      .join(' ')
      .split(RegExp(r'\s+'))
      .where((w) => w.isNotEmpty)
      .join(' ');
  // Compared in runes, so a match cannot start or end halfway through a character.
  final chars = flat.runes.toList();
  final lower = flat.toLowerCase().runes.toList();
  final want = q.runes.toList();
  if (lower.length != chars.length) return null; // lower-casing changed the length; rare
  var at = -1;
  for (var i = 0; i + want.length <= lower.length; i++) {
    var hit = true;
    for (var j = 0; j < want.length; j++) {
      if (lower[i + j] != want[j]) {
        hit = false;
        break;
      }
    }
    if (hit) {
      at = i;
      break;
    }
  }
  if (at < 0) return null;
  final end = at + want.length;
  // Start at a word, not in one: cut mid-word the snippet read "…ons ship on Friday".
  var from = at - before < 0 ? 0 : at - before;
  if (from > 0) {
    for (var i = from; i < at; i++) {
      if (String.fromCharCode(chars[i]).trim().isEmpty) {
        from = i + 1;
        break;
      }
    }
  }
  final to = end + after > chars.length ? chars.length : end + after;
  String piece(int a, int b) => String.fromCharCodes(chars.sublist(a, b));
  return (
    before: '${from > 0 ? '…' : ''}${piece(from, at).trimLeft()}',
    match: piece(at, end),
    after: '${piece(end, to)}${to < chars.length ? '…' : ''}',
  );
}
