/// Colouring the inside of a fenced code block.
///
/// Deliberately not a parser. One scanner, told per language what a comment looks like, what
/// quotes a string and which words are keywords — enough to make code read as code, and it
/// cannot be wrong in a way that loses text: every character comes out in order, in some
/// colour.
///
/// A fence with no language after it (` ``` `) is left alone. Naming one (` ```rust `) is
/// what asks for the colours, as it does in a chat box.
///
/// **Keep in step with the desktop's `highlight.rs`**, which has to colour the same block the
/// same way — the same languages, the same keywords, the same five colours.
library;

import 'package:flutter/material.dart';

/// What a run of code is.
enum CodeKind { plain, keyword, string, number, comment }

/// The colours, chosen against the pale paper a note is written on rather than against an
/// editor's dark background. **Same values as `Kind::color` in `highlight.rs`.**
const Map<CodeKind, Color> codeColors = {
  CodeKind.plain: Color(0xFF24292F),
  CodeKind.keyword: Color(0xFFCF222E),
  CodeKind.string: Color(0xFF0A3069),
  CodeKind.number: Color(0xFF0550AE),
  CodeKind.comment: Color(0xFF6E7781),
};

/// One run of a line and what it is.
class CodeRun {
  const CodeRun(this.length, this.kind);
  final int length;
  final CodeKind kind;
}

/// How one language is written: enough of it to colour, not to understand.
class _Syntax {
  const _Syntax({
    required this.lineComment,
    required this.blockComment,
    required this.quotes,
    required this.keywords,
  });
  final List<String> lineComment;
  final List<String>? blockComment; // [open, close]
  final List<String> quotes;
  final Set<String> keywords;
}

const _rustish = {
  'as', 'async', 'await', 'break', 'const', 'continue', 'crate', 'dyn', 'else', 'enum',
  'extern', 'false', 'fn', 'for', 'if', 'impl', 'in', 'let', 'loop', 'match', 'mod', 'move',
  'mut', 'pub', 'ref', 'return', 'self', 'static', 'struct', 'super', 'trait', 'true',
  'type', 'unsafe', 'use', 'where', 'while',
};
const _cish = {
  'auto', 'bool', 'break', 'case', 'catch', 'char', 'class', 'const', 'continue', 'default',
  'delete', 'do', 'double', 'else', 'enum', 'extends', 'extern', 'false', 'final', 'float',
  'for', 'if', 'import', 'int', 'long', 'namespace', 'new', 'null', 'package', 'private',
  'protected', 'public', 'return', 'static', 'struct', 'switch', 'template', 'this', 'throw',
  'true', 'try', 'typedef', 'union', 'unsigned', 'void', 'while',
};
const _jsish = {
  'async', 'await', 'break', 'case', 'catch', 'class', 'const', 'continue', 'default',
  'delete', 'do', 'else', 'export', 'extends', 'false', 'finally', 'for', 'from', 'function',
  'if', 'import', 'in', 'instanceof', 'let', 'new', 'null', 'of', 'return', 'static',
  'super', 'switch', 'this', 'throw', 'true', 'try', 'typeof', 'undefined', 'var', 'void',
  'while', 'yield',
};
const _dartish = {
  'abstract', 'as', 'async', 'await', 'break', 'case', 'catch', 'class', 'const', 'continue',
  'default', 'do', 'else', 'enum', 'export', 'extends', 'extension', 'external', 'factory',
  'false', 'final', 'finally', 'for', 'get', 'if', 'implements', 'import', 'in', 'is',
  'late', 'library', 'mixin', 'new', 'null', 'on', 'part', 'required', 'return', 'set',
  'static', 'super', 'switch', 'this', 'throw', 'true', 'try', 'typedef', 'var', 'void',
  'while', 'with', 'yield',
};
const _pyish = {
  'and', 'as', 'assert', 'async', 'await', 'break', 'class', 'continue', 'def', 'del',
  'elif', 'else', 'except', 'False', 'finally', 'for', 'from', 'global', 'if', 'import',
  'in', 'is', 'lambda', 'None', 'nonlocal', 'not', 'or', 'pass', 'raise', 'return', 'True',
  'try', 'while', 'with', 'yield',
};
const _shish = {
  'case', 'do', 'done', 'elif', 'else', 'esac', 'export', 'fi', 'for', 'function', 'if',
  'in', 'local', 'return', 'then', 'until', 'while',
};
const _sqlish = {
  'and', 'as', 'asc', 'by', 'create', 'delete', 'desc', 'drop', 'from', 'group', 'having',
  'insert', 'into', 'join', 'left', 'limit', 'not', 'null', 'on', 'or', 'order', 'select',
  'set', 'table', 'update', 'values', 'where',
};

/// The language a fence named, or null when it named nothing this build knows.
_Syntax? _syntaxFor(String lang) {
  switch (lang.trim().toLowerCase()) {
    case 'rust':
    case 'rs':
      return const _Syntax(
          lineComment: ['//'], blockComment: ['/*', '*/'], quotes: ['"'], keywords: _rustish);
    case 'c':
    case 'cpp':
    case 'c++':
    case 'java':
    case 'kotlin':
    case 'kt':
    case 'cs':
    case 'go':
    case 'swift':
      return const _Syntax(
          lineComment: ['//'],
          blockComment: ['/*', '*/'],
          quotes: ['"', "'"],
          keywords: _cish);
    case 'js':
    case 'javascript':
    case 'ts':
    case 'typescript':
    case 'jsx':
    case 'tsx':
      return const _Syntax(
          lineComment: ['//'],
          blockComment: ['/*', '*/'],
          quotes: ['"', "'", '`'],
          keywords: _jsish);
    case 'dart':
      return const _Syntax(
          lineComment: ['//'],
          blockComment: ['/*', '*/'],
          quotes: ['"', "'"],
          keywords: _dartish);
    case 'py':
    case 'python':
      return const _Syntax(
          lineComment: ['#'], blockComment: null, quotes: ['"', "'"], keywords: _pyish);
    case 'sh':
    case 'bash':
    case 'zsh':
    case 'shell':
      return const _Syntax(
          lineComment: ['#'], blockComment: null, quotes: ['"', "'"], keywords: _shish);
    case 'sql':
      return const _Syntax(
          lineComment: ['--'], blockComment: ['/*', '*/'], quotes: ["'"], keywords: _sqlish);
    case 'json':
      return const _Syntax(
          lineComment: [],
          blockComment: null,
          quotes: ['"'],
          keywords: {'false', 'null', 'true'});
    case 'yaml':
    case 'yml':
    case 'toml':
    case 'ini':
      return const _Syntax(
          lineComment: ['#'],
          blockComment: null,
          quotes: ['"', "'"],
          keywords: {'false', 'true'});
    default:
      return null;
  }
}

/// Whether a fence tag names a language this can colour.
bool knownLanguage(String lang) => _syntaxFor(lang) != null;

/// Carries a block comment from one line to the next.
class CodeScanner {
  CodeScanner(String lang) : _syntax = _syntaxFor(lang);
  final _Syntax? _syntax;
  bool _inBlock = false;

  /// Whether this scanner colours anything at all.
  bool get colours => _syntax != null;

  /// The runs of one line, left to right. Their lengths add up to `line.length` exactly.
  List<CodeRun> scan(String line) {
    final syntax = _syntax;
    if (syntax == null) return [CodeRun(line.length, CodeKind.plain)];

    final runs = <CodeRun>[];
    var i = 0;
    var plainFrom = 0;

    void flush(int to) {
      if (to > plainFrom) runs.add(CodeRun(to - plainFrom, CodeKind.plain));
      plainFrom = to;
    }

    while (i < line.length) {
      final rest = line.substring(i);

      if (_inBlock) {
        final close = syntax.blockComment![1];
        final at = rest.indexOf(close);
        final end = at < 0 ? line.length : i + at + close.length;
        runs.add(CodeRun(end - i, CodeKind.comment));
        _inBlock = at < 0;
        i = end;
        plainFrom = i;
        continue;
      }

      final block = syntax.blockComment;
      if (block != null && rest.startsWith(block[0])) {
        flush(i);
        final after = rest.substring(block[0].length);
        final at = after.indexOf(block[1]);
        final end = at < 0 ? line.length : i + block[0].length + at + block[1].length;
        runs.add(CodeRun(end - i, CodeKind.comment));
        _inBlock = at < 0;
        i = end;
        plainFrom = i;
        continue;
      }

      if (syntax.lineComment.any(rest.startsWith)) {
        flush(i);
        runs.add(CodeRun(line.length - i, CodeKind.comment));
        return runs;
      }

      final ch = line[i];
      if (syntax.quotes.contains(ch)) {
        flush(i);
        final end = _stringEnd(line, i, ch);
        runs.add(CodeRun(end - i, CodeKind.string));
        i = end;
        plainFrom = i;
        continue;
      }

      if (_isDigit(ch) && !_insideWord(line, i)) {
        flush(i);
        final end = _wordEnd(line, i, (c) => _isWordChar(c) || c == '.');
        runs.add(CodeRun(end - i, CodeKind.number));
        i = end;
        plainFrom = i;
        continue;
      }

      if (_isLetter(ch) || ch == '_') {
        final end = _wordEnd(line, i, _isWordChar);
        if (syntax.keywords.contains(line.substring(i, end))) {
          flush(i);
          runs.add(CodeRun(end - i, CodeKind.keyword));
          plainFrom = end;
        }
        i = end;
        continue;
      }

      i += 1;
    }
    flush(line.length);
    return runs;
  }
}

/// Where the string opened at `start` ends, past its closing quote; the end of the line when
/// it never closes, which is what an unfinished string looks like while it is being typed.
int _stringEnd(String line, int start, String quote) {
  var i = start + 1;
  while (i < line.length) {
    if (line[i] == r'\') {
      i += 2;
      continue;
    }
    if (line[i] == quote) return i + 1;
    i += 1;
  }
  return line.length;
}

int _wordEnd(String line, int start, bool Function(String) keep) {
  var i = start;
  while (i < line.length && keep(line[i])) {
    i += 1;
  }
  return i;
}

bool _isDigit(String c) => c.codeUnitAt(0) >= 0x30 && c.codeUnitAt(0) <= 0x39;

bool _isLetter(String c) {
  final u = c.codeUnitAt(0);
  return (u >= 0x41 && u <= 0x5A) || (u >= 0x61 && u <= 0x7A) || u > 0x7F;
}

bool _isWordChar(String c) => _isLetter(c) || _isDigit(c) || c == '_';

/// Whether the character at `at` continues a word, so `utf8` is not read as the number 8.
bool _insideWord(String line, int at) => at > 0 && _isWordChar(line[at - 1]);
