/// Markdown, styled where it is typed.
///
/// A memo is plain writing with marked-off regions in it, the way a chat message is:
///
/// - ordinary lines are **plain text**. `**stars**` typed in a shopping list are stars.
/// - a bare ` ``` ` fence opens a **markdown region**: inside it the marks mean what they say.
/// - ` ```rust ` and friends open a **code block**, coloured by `code_highlight.dart`.
///
/// Marking the formatting off is the point: nobody has to escape anything, and a memo that
/// was never meant to be markdown cannot be reformatted behind the user's back.
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

import 'dart:math' as math;

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
///
/// With [blockBackground] off, a fenced block's lines are drawn without a background of their
/// own and the caller paints one behind the whole block ([CodeBlockPainter]): a background on
/// the text stops where each line's letters stop, so a block came out as a ragged stack of
/// bars one line long each. Inline `code` keeps its own either way.
List<TextSpan> markdownSpans(
  String text, {
  required TextStyle base,
  required Color marker,
  required Color codeBackground,
  bool blockBackground = true,
}) {
  final runs = <_Run>[];
  final inline = base.copyWith(fontFamily: 'monospace', backgroundColor: codeBackground);
  final mono = blockBackground ? inline : base.copyWith(fontFamily: 'monospace');
  final markerStyle = base.copyWith(color: marker);

  var offset = 0;
  var inFence = false;
  // The scanner for the block being typed, made when a fence names a language and thrown
  // away when it closes. Null means the block is drawn plainly, which is what an unnamed
  // fence has always done.
  CodeScanner? scanner;
  /// Whether the fence we are inside is a bare one, where the marks are read.
  var markdownRegion = false;
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
      if (inFence) {
        inFence = false;
        scanner = null;
        markdownRegion = false;
      } else {
        inFence = true;
        // Only the opening fence names a language; the closing one is bare. A bare fence is
        // not code at all — it is where the markdown marks start meaning something.
        final tag = trimmed.substring(3).trim();
        markdownRegion = tag.isEmpty;
        scanner = markdownRegion ? null : CodeScanner(tag);
      }
      runs.add(_Run(offset, end, mono.copyWith(color: marker)));
      endLine(mono);
      continue;
    }
    if (inFence && !markdownRegion) {
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
    if (!inFence) {
      // Outside a region the writing is writing: no marks are read, nothing is restyled.
      runs.add(_Run(offset, end, base));
      endLine(base);
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
        inline,
      ));
      endLine(base);
      continue;
    }

    runs.addAll(_inline(line, offset, base, markerStyle, inline));
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

/// The fenced code blocks in [text] — from a fence that names a language to the fence that
/// closes it, both included, or to the end of an unclosed one — as character ranges. A bare
/// fence is a markdown region, not code, and has no block drawn behind it.
List<TextRange> codeBlockRanges(String text) {
  final ranges = <TextRange>[];
  var offset = 0;
  var inFence = false;
  int? start;
  for (final line in _lines(text)) {
    final end = offset + line.length;
    final trimmed = line.trimLeft();
    if (trimmed.startsWith('```')) {
      if (inFence) {
        if (start != null) ranges.add(TextRange(start: start, end: end));
        inFence = false;
        start = null;
      } else {
        inFence = true;
        if (trimmed.substring(3).trim().isNotEmpty) start = offset;
      }
    }
    offset = end + 1;
  }
  if (start != null) ranges.add(TextRange(start: start, end: text.length));
  return ranges;
}

/// One rounded background behind each code block of [text], the whole width of the field —
/// what [markdownSpans] leaves out with `blockBackground: false`.
///
/// It lays the text out again to find where each block's lines landed, which is the only way
/// to know with lines that wrapped, so [text] must be the very span the field draws (its
/// `style` the field's base style) and the painter as wide as the field. Nothing is laid out for a memo without a code block. With
/// [scroll] it follows the field's scroll offset and repaints as it moves.
class CodeBlockPainter extends CustomPainter {
  CodeBlockPainter({
    required this.source,
    required this.text,
    required this.textScaler,
    required this.color,
    this.scroll,
    this.inset = 0,
  })  : ranges = codeBlockRanges(source),
        super(repaint: scroll);

  /// The plain memo, which decides where the blocks are; [text] is how it is drawn.
  final String source;
  final InlineSpan text;
  final TextScaler textScaler;
  final Color color;
  final ScrollController? scroll;

  /// How far the background reaches past the text on either side, where there is room.
  final double inset;
  final List<TextRange> ranges;

  @override
  void paint(Canvas canvas, Size size) {
    if (ranges.isEmpty) return;
    final base = text.style;
    final painter = TextPainter(
      text: text,
      textDirection: TextDirection.ltr,
      textScaler: textScaler,
      // What `EditableText` lays its text out with, so a `TextField` and a `SelectableText`
      // alike: every line forced to the body's height, a heading's included. Left out, a
      // heading line came out taller here than on screen and every block under it was
      // painted half a line low.
      strutStyle: base == null ? null : StrutStyle.fromTextStyle(base, forceStrutHeight: true),
    )..layout(maxWidth: size.width);
    final controller = scroll;
    final dy = controller != null && controller.hasClients ? controller.offset : 0.0;
    final paint = Paint()..color = color;
    for (final range in ranges) {
      final boxes = painter.getBoxesForSelection(
        TextSelection(baseOffset: range.start, extentOffset: range.end),
      );
      if (boxes.isEmpty) continue;
      final top = boxes.map((b) => b.top).reduce(math.min);
      final bottom = boxes.map((b) => b.bottom).reduce(math.max);
      canvas.drawRRect(
        RRect.fromLTRBR(-inset, top - dy - 2, size.width + inset, bottom - dy + 2,
            const Radius.circular(6)),
        paint,
      );
    }
    painter.dispose();
  }

  @override
  bool shouldRepaint(CodeBlockPainter old) =>
      old.source != source ||
      old.text != text ||
      old.color != color ||
      old.textScaler != textScaler ||
      old.scroll != scroll;
}

/// How tall [text] stands in a body field [width] wide: the measurement a photo standing in
/// the writing is placed by.
///
/// Laid out the way `EditableText` lays out the field, or the photo lands beside the wrong
/// words: with the system's text size (Android's font size setting scales the field and did
/// not scale this, so on a phone set larger than default a photo in the writing sat over the
/// lines above its room), with the field's forced line height, and with the markdown drawn as
/// the field draws it — a heading or a code line wraps at a different point than plain text.
/// A line is styled by what comes before it only, so a prefix's spans are exactly the start
/// of the whole memo's.
double heightAsDrawn(
  String text, {
  required double width,
  required TextStyle base,
  required Color marker,
  required Color codeBackground,
  required TextScaler textScaler,
}) {
  if (text.isEmpty) return 0;
  final painter = TextPainter(
    text: TextSpan(
      style: base,
      children: markdownSpans(
        text,
        base: base,
        marker: marker,
        codeBackground: codeBackground,
        blockBackground: false,
      ),
    ),
    textDirection: TextDirection.ltr,
    textScaler: textScaler,
    strutStyle: StrutStyle.fromTextStyle(base, forceStrutHeight: true),
  )..layout(maxWidth: width);
  final height = painter.height;
  painter.dispose();
  return height;
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
  MarkdownEditingController({
    super.text,
    required this.marker,
    required this.codeBackground,
    this.blockBackground = true,
  });

  /// Off where the field paints whole blocks behind itself; see [markdownSpans].
  final bool blockBackground;

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
        blockBackground: blockBackground,
      ),
    );
  }
}
