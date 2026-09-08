// The markdown the body field draws as it is typed.
//
// These runs back an *editable* field: they have to put the memo back together character for
// character, or the caret ends up standing somewhere other than where the letter is. That is
// the property worth a test — the styling itself is visible the moment anyone types.
//
// Run with `flutter test` from `apps/mobile`.

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:ymemo_mobile/code_highlight.dart';
import 'package:ymemo_mobile/markdown_style.dart';

const _base = TextStyle(fontSize: 14);

List<TextSpan> spans(String text) => markdownSpans(
      text,
      base: _base,
      marker: const Color(0xFF888888),
      codeBackground: const Color(0x11000000),
    );

String rebuilt(String text) => spans(text).map((s) => s.text!).join();

/// The style of the run holding the first occurrence of `needle`.
TextStyle styleOf(String text, String needle) =>
    spans(text).firstWhere((s) => s.text == needle).style!;

void main() {
  group('every character survives', () {
    for (final sample in <String>[
      '',
      'plain',
      '**bold**',
      'a **bold** b *italic* c `code` d ~~gone~~ e',
      '# heading\nbody',
      '```\nfn main() {}\n```\nafter',
      'unclosed **bold',
      '****',
      '`',
      '*a* *b* *c*',
      '가족 **회의** 메모\n- 준비물\n- 안건',
      '\n\n\n',
      'trailing newline\n',
    ]) {
      test(sample.isEmpty ? '(empty)' : sample.replaceAll('\n', r'\n'), () {
        expect(rebuilt(sample), sample);
      });
    }
  });

  test('bold, italic, strike and code each get their own style', () {
    expect(styleOf('a **b** c', 'b').fontWeight, FontWeight.w700);
    expect(styleOf('a *b* c', 'b').fontStyle, FontStyle.italic);
    expect(styleOf('a _b_ c', 'b').fontStyle, FontStyle.italic);
    expect(styleOf('a ~~b~~ c', 'b').decoration, TextDecoration.lineThrough);
    expect(styleOf('a `b` c', 'b').fontFamily, 'monospace');
  });

  test('the markers stay in the text, drawn faded', () {
    expect(rebuilt('**b**'), '**b**');
    expect(styleOf('**b**', '**').color, const Color(0xFF888888));
  });

  test('a heading is bigger, and only with a space after the hashes', () {
    expect(styleOf('# Title', 'Title').fontSize, greaterThan(14));
    expect(styleOf('## Title', 'Title').fontSize, greaterThan(14));
    // Seven is too many to be a heading, and `#tag` is a word.
    expect(styleOf('####### too deep', '####### too deep').fontSize, 14);
    expect(styleOf('#tag here', '#tag here').fontSize, 14);
  });

  test('a fence turns the lines between it into code', () {
    const text = '```\ncode line\n```\nprose';
    expect(styleOf(text, 'code line').fontFamily, 'monospace');
    expect(styleOf(text, 'prose').fontFamily, isNull);
  });

  test('an unclosed fence takes the rest of the memo, rather than half a line', () {
    const text = 'prose\n```\nstill code\nand this too';
    expect(styleOf(text, 'still code').fontFamily, 'monospace');
    expect(styleOf(text, 'and this too').fontFamily, 'monospace');
  });

  group('a fence that names a language is coloured', () {
    /// The runs of one line, as (text, kind).
    List<(String, CodeKind)> runs(String line, String lang) {
      final scanner = CodeScanner(lang);
      var at = 0;
      return [
        for (final run in scanner.scan(line))
          (line.substring(at, at += run.length), run.kind),
      ];
    }

    test('every character survives, whatever the language', () {
      for (final (line, lang) in <(String, String)>[
        ('fn main() { let x = 1; }', 'rust'),
        ('// just a comment', 'rust'),
        (r'let s = "a " b"; // after', 'rust'),
        ('/* open', 'rust'),
        ("x = 'unterminated", 'python'),
        ('가나다 = "한글"  # 주석', 'python'),
        ("SELECT * FROM t WHERE a = 'b'", 'sql'),
        ('', 'rust'),
        ('utf8 and 3.14 and x2', 'rust'),
      ]) {
        expect(runs(line, lang).map((r) => r.$1).join(), line, reason: line);
      }
    });

    test('keywords, strings, numbers and comments are told apart', () {
      final got = runs('let x = 42; // note', 'rust');
      expect(got, contains(('let', CodeKind.keyword)));
      expect(got, contains(('42', CodeKind.number)));
      expect(got, contains(('// note', CodeKind.comment)));
      expect(runs('s = "hi"', 'python'), contains(('"hi"', CodeKind.string)));
    });

    test('only whole words count', () {
      expect(runs('letter = 1', 'rust'), isNot(contains(('let', CodeKind.keyword))));
      expect(runs('utf8 = 1', 'rust'), isNot(contains(('8', CodeKind.number))));
    });

    test('a block comment runs past the end of a line', () {
      final scanner = CodeScanner('rust');
      scanner.scan('code /* start');
      // The second line is still inside the comment it opened.
      final second = scanner.scan('still comment */ code');
      expect(second.first.kind, CodeKind.comment);
    });

    test('an unknown language, or none, is left plain', () {
      expect(knownLanguage(''), isFalse);
      expect(knownLanguage('brainfuck'), isFalse);
      expect(CodeScanner('').colours, isFalse);
      expect(knownLanguage('rust') && knownLanguage('Python'), isTrue);
    });

    test('the whole memo still comes back with a coloured fence in it', () {
      const text = '```rust\nlet x = 1; // hi\n```\nafter';
      expect(rebuilt(text), text);
    });
  });

  test('emphasis does not span a blank pair or reach across lines', () {
    // Nothing between the markers is not emphasis, so the text is left alone.
    expect(rebuilt('** **'), '** **');
    // A marker opened on one line and closed on the next is two loose markers.
    expect(styleOf('*a\nb*', '*a').fontStyle, isNull);
  });
}
