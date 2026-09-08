/// Markdown, styled where it is typed.
///
/// The memo keeps its markers: `**like this**` is stored exactly as written, and what
/// changes is only how it is drawn. That is the whole reason this is a `TextEditingController`
/// and not a rich-text editor — every character the field shows is a character in the memo,
/// so the caret, the selection, undo and the Rust side all keep working on plain text, and a
/// memo written on the phone is the same string the desktop sticky opens.
///
/// The markers stay visible but faded, the way a chat box does it: seeing `**` tells you why
/// the words went bold and how to take it back, and hiding them would move the caret out of
/// step with the text underneath.
///
/// **Keep in step with the desktop's `markdown.rs`**, which recognises the same subset for
/// the sticky's read view.
library;

import 'package:flutter/material.dart';

import 'code_highlight.dart';

/// One run of the text and how it is drawn.
class _Run {
  _Run(this.start, this.end, this.style);
  final int start;
  final int end;
  final TextStyle style;
}

/// How much bigger a heading is than the body, by level (1..6).
const List<double> _headingScale = [1.45, 1.30, 1.18, 1.10, 1.05, 1.0];

/// Splits `text` into styled runs. Every character of `text` appears exactly once, in order,
/// so the result can back an editable field.
List<TextSpan> markdownSpans(
  String text, {
  required TextStyle base,
  required Color marker,
  required Color codeBackground,
}) {
  final runs = <_Run>[];
  final mono = base.copyWith(fontFamily: 'monospace', backgroundColor: codeBackground);
  final markerStyle = base.copyWith(color: marker);

  var offset = 0;
  var inFence = false;
  // The scanner for the block being typed, made when a fence names a language and thrown
  // away when it closes. Null means the block is drawn plainly, which is what an unnamed
  // fence has always done.
  CodeScanner? scanner;
  final lines = _lines(text);
  for (var i = 0; i < lines.length; i++) {
    final line = lines[i];
    final end = offset + line.length;
    final trimmed = line.trimLeft();
    // The newline between two lines is a character of the memo like any other, and the runs
    // have to account for it or every multi-line note falls back to plain text.
    void endLine(TextStyle style) {
      if (i < lines.length - 1) runs.add(_Run(end, end + 1, style));
      offset = end + 1;
    }

    // A fence line toggles the block. The line itself is drawn as code, so an unclosed
    // fence looks unfinished rather than looking like ordinary writing.
    if (trimmed.startsWith('```')) {
      // Only the opening fence names a language; the closing one is bare.
      scanner = inFence ? null : CodeScanner(trimmed.substring(3));
      inFence = !inFence;
      runs.add(_Run(offset, end, mono.copyWith(color: marker)));
      endLine(mono);
      continue;
    }
    if (inFence) {
      final colouring = scanner;
      if (colouring != null && colouring.colours) {
        var at = offset;
        for (final run in colouring.scan(line)) {
          runs.add(_Run(at, at + run.length, mono.copyWith(color: codeColors[run.kind])));
          at += run.length;
        }
      } else {
        runs.add(_Run(offset, end, mono));
      }
      endLine(mono);
      continue;
    }

    final heading = _headingLevel(line);
    if (heading > 0) {
      final hashes = line.indexOf(' ') + 1;
      runs.add(_Run(offset, offset + hashes, markerStyle));
      runs.addAll(_inline(
        line.substring(hashes),
        offset + hashes,
        base.copyWith(
          fontWeight: FontWeight.w700,
          fontSize: (base.fontSize ?? 14) * _headingScale[heading - 1],
        ),
        markerStyle,
        mono,
      ));
      endLine(base);
      continue;
    }

    runs.addAll(_inline(line, offset, base, markerStyle, mono));
    endLine(base);
  }

  final spans = [
    for (final run in runs)
      TextSpan(text: text.substring(run.start, run.end), style: run.style),
  ];
  // The runs must put the string back together exactly, in order and once each: they back an
  // editable field, so a gap or an overlap would move the caret away from the letter it is
  // standing on. Rather than trust the arithmetic, check it — and if it ever fails, draw the
  // memo as plain text. Losing the styling is a blemish; losing the caret is not usable.
  if (spans.map((s) => s.text!).join() != text) {
    assert(false, 'markdown spans did not reconstruct the text');
    return [TextSpan(text: text, style: base)];
  }
  return spans;
}

/// The lines of `text`, without their newlines. Unlike `split('\n')` this keeps a trailing
/// empty line, which matters because the offsets have to add up to the whole string.
List<String> _lines(String text) => text.split('\n');

/// 1..6 for a heading line, 0 for anything else. `#` with no space after it is not one —
/// `#tag` is a word, not a title.
int _headingLevel(String line) {
  var level = 0;
  while (level < line.length && line.codeUnitAt(level) == 0x23) {
    level++;
  }
  if (level == 0 || level > 6) return 0;
  if (level >= line.length || line.codeUnitAt(level) != 0x20) return 0;
  return level;
}

/// Emphasis inside one line. Longest markers first, so `**bold**` is not read as two
/// italics; a marker with nothing between it is left as plain text.
final RegExp _inlinePattern = RegExp(
  r'(\*\*)(?=\S)(.+?)(?<=\S)(\*\*)'
  r'|(~~)(?=\S)(.+?)(?<=\S)(~~)'
  r'|(`)([^`]+)(`)'
  r'|(\*)(?=\S)([^*]+?)(?<=\S)(\*)'
  r'|(_)(?=\S)([^_]+?)(?<=\S)(_)',
);

List<_Run> _inline(
  String line,
  int lineStart,
  TextStyle base,
  TextStyle marker,
  TextStyle mono,
) {
  final runs = <_Run>[];
  var at = 0;
  for (final m in _inlinePattern.allMatches(line)) {
    if (m.start > at) runs.add(_Run(lineStart + at, lineStart + m.start, base));
    // Which alternative matched decides the style; the groups come in threes.
    final TextStyle inner;
    if (m.group(1) != null) {
      inner = base.copyWith(fontWeight: FontWeight.w700);
    } else if (m.group(4) != null) {
      inner = base.copyWith(decoration: TextDecoration.lineThrough);
    } else if (m.group(7) != null) {
      inner = mono.merge(TextStyle(fontSize: base.fontSize));
    } else {
      inner = base.copyWith(fontStyle: FontStyle.italic);
    }
    final open = (m.group(1) ?? m.group(4) ?? m.group(7) ?? m.group(10) ?? m.group(13))!;
    final close = (m.group(3) ?? m.group(6) ?? m.group(9) ?? m.group(12) ?? m.group(15))!;
    final openEnd = m.start + open.length;
    final closeStart = m.end - close.length;
    runs.add(_Run(lineStart + m.start, lineStart + openEnd, marker.merge(inner).copyWith(color: marker.color)));
    runs.add(_Run(lineStart + openEnd, lineStart + closeStart, inner));
    runs.add(_Run(lineStart + closeStart, lineStart + m.end, marker.merge(inner).copyWith(color: marker.color)));
    at = m.end;
  }
  if (at < line.length) runs.add(_Run(lineStart + at, lineStart + line.length, base));
  return runs;
}

/// A body field that draws its own markdown as it is typed.
///
/// Only the drawing changes: [text] is the plain string with every marker in it, which is
/// what gets saved and what the other device opens.
class MarkdownEditingController extends TextEditingController {
  MarkdownEditingController({super.text, required this.marker, required this.codeBackground});

  /// Colour for the markers themselves — faded against the writing they wrap. Not final:
  /// the note's paper colour can be changed while it is open, and the ink goes with it.
  Color marker;
  Color codeBackground;

  @override
  TextSpan buildTextSpan({
    required BuildContext context,
    TextStyle? style,
    required bool withComposing,
  }) {
    final base = style ?? const TextStyle();
    // While an IME is composing, the field must draw exactly what the framework asked for,
    // underline and all — restyling mid-composition drops the Korean syllable being built.
    if (withComposing && !value.composing.isCollapsed && value.isComposingRangeValid) {
      return super.buildTextSpan(
        context: context,
        style: style,
        withComposing: withComposing,
      );
    }
    return TextSpan(
      style: base,
      children: markdownSpans(
        text,
        base: base,
        marker: marker,
        codeBackground: codeBackground,
      ),
    );
  }
}
