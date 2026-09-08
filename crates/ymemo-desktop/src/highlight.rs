//! Colouring the inside of a fenced code block.
//!
//! Deliberately not a parser. One scanner, told per language what a comment looks like, what
//! quotes a string and which words are keywords — that is enough to make code read as code,
//! and it cannot be wrong in a way that loses text: every character of the block comes out in
//! order, in some colour.
//!
//! A fence with no language after it (` ``` `) is left alone, the way it was before this
//! existed. Naming one (` ```rust `) is what asks for the colours, as it does in a chat box.
//!
//! **Keep in step with the phone's `code_highlight.dart`**, which has to colour the same
//! block the same way.

use crate::{CodeLine, CodeToken};
use slint::{Color, ModelRc, SharedString, VecModel};

/// What a run of code is. The colours are one set for both platforms, chosen against the pale
/// paper a note is written on rather than against an editor's dark background.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Kind {
    Plain,
    Keyword,
    String,
    Number,
    Comment,
}

impl Kind {
    fn color(self) -> Color {
        match self {
            Kind::Plain => Color::from_rgb_u8(0x24, 0x29, 0x2f),
            Kind::Keyword => Color::from_rgb_u8(0xcf, 0x22, 0x2e),
            Kind::String => Color::from_rgb_u8(0x0a, 0x30, 0x69),
            Kind::Number => Color::from_rgb_u8(0x05, 0x50, 0xae),
            Kind::Comment => Color::from_rgb_u8(0x6e, 0x77, 0x81),
        }
    }
}

/// How one language is written: enough of it to colour, not to understand.
struct Syntax {
    /// Everything after this on a line is a comment.
    line_comment: &'static [&'static str],
    /// Opening and closing of a comment that can span lines; empty when the language has none.
    block_comment: Option<(&'static str, &'static str)>,
    /// Characters that open and close a string.
    quotes: &'static [char],
    keywords: &'static [&'static str],
    /// Markup: the word after a `<` or `</` is the name of a tag, and reads as one. A tag
    /// name is not a word from a fixed list, so this is a rule rather than a keyword set.
    tags: bool,
}

const RUSTISH: &[&str] = &[
    "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum",
    "extern", "false", "fn", "for", "if", "impl", "in", "let", "loop", "match", "mod", "move",
    "mut", "pub", "ref", "return", "self", "static", "struct", "super", "trait", "true",
    "type", "unsafe", "use", "where", "while",
];
const CISH: &[&str] = &[
    "auto", "bool", "break", "case", "catch", "char", "class", "const", "continue", "default",
    "delete", "do", "double", "else", "enum", "extends", "extern", "false", "final", "float",
    "for", "if", "import", "int", "long", "namespace", "new", "null", "package", "private",
    "protected", "public", "return", "static", "struct", "switch", "template", "this", "throw",
    "true", "try", "typedef", "union", "unsigned", "void", "while",
];
const JSISH: &[&str] = &[
    "async", "await", "break", "case", "catch", "class", "const", "continue", "default",
    "delete", "do", "else", "export", "extends", "false", "finally", "for", "from", "function",
    "if", "import", "in", "instanceof", "let", "new", "null", "of", "return", "static",
    "super", "switch", "this", "throw", "true", "try", "typeof", "undefined", "var", "void",
    "while", "yield",
];
const DARTISH: &[&str] = &[
    "abstract", "as", "async", "await", "break", "case", "catch", "class", "const", "continue",
    "default", "do", "else", "enum", "export", "extends", "extension", "external", "factory",
    "false", "final", "finally", "for", "get", "if", "implements", "import", "in", "is",
    "late", "library", "mixin", "new", "null", "on", "part", "required", "return", "set",
    "static", "super", "switch", "this", "throw", "true", "try", "typedef", "var", "void",
    "while", "with", "yield",
];
const PYISH: &[&str] = &[
    "and", "as", "assert", "async", "await", "break", "class", "continue", "def", "del",
    "elif", "else", "except", "False", "finally", "for", "from", "global", "if", "import",
    "in", "is", "lambda", "None", "nonlocal", "not", "or", "pass", "raise", "return", "True",
    "try", "while", "with", "yield",
];
const SHISH: &[&str] = &[
    "case", "do", "done", "elif", "else", "esac", "export", "fi", "for", "function", "if",
    "in", "local", "return", "then", "until", "while",
];
const SQLISH: &[&str] = &[
    "and", "as", "asc", "by", "create", "delete", "desc", "drop", "from", "group", "having",
    "insert", "into", "join", "left", "limit", "not", "null", "on", "or", "order", "select",
    "set", "table", "update", "values", "where",
];

/// The five things one arm of [`syntax`] answers, in the order [`Syntax`] takes them.
type Rules = (
    &'static [&'static str],
    Option<(&'static str, &'static str)>,
    &'static [char],
    &'static [&'static str],
    bool,
);

/// The language a fence named, or `None` when it named nothing this build knows.
///
/// Unknown is not an error: the block is drawn plainly, which is what a fence did before any
/// of this and what a name nobody recognises should do.
fn syntax(lang: &str) -> Option<Syntax> {
    let (line_comment, block_comment, quotes, keywords, tags): Rules =
        match lang.trim().to_ascii_lowercase().as_str() {
        "rust" | "rs" => (&["//"], Some(("/*", "*/")), &['"'], RUSTISH, false),
        "c" | "cpp" | "c++" | "java" | "kotlin" | "kt" | "cs" | "go" | "swift" => {
            (&["//"], Some(("/*", "*/")), &['"', '\''], CISH, false)
        }
        "js" | "javascript" | "ts" | "typescript" | "jsx" | "tsx" => {
            (&["//"], Some(("/*", "*/")), &['"', '\'', '`'], JSISH, false)
        }
        "dart" => (&["//"], Some(("/*", "*/")), &['"', '\''], DARTISH, false),
        "py" | "python" => (&["#"], None, &['"', '\''], PYISH, false),
        "sh" | "bash" | "zsh" | "shell" => (&["#"], None, &['"', '\''], SHISH, false),
        "sql" => (&["--"], Some(("/*", "*/")), &['\''], SQLISH, false),
        "json" => (&[], None, &['"'], &["false", "null", "true"], false),
        "yaml" | "yml" | "toml" | "ini" => (&["#"], None, &['"', '\''], &["false", "true"], false),
        // Markup: the tag names carry the meaning, and there is no list of them to check
        // against — anything after a `<` is one.
        "html" | "xml" | "svg" | "xhtml" => {
            (&[], Some(("<!--", "-->")), &['"', '\''], &[], true)
        }
        "css" | "scss" => (&["//"], Some(("/*", "*/")), &['"', '\''], &[], false),
        _ => return None,
    };
    Some(Syntax { line_comment, block_comment, quotes, keywords, tags })
}

/// Splits code into coloured lines. `lang` is whatever followed the opening fence.
///
/// Every character comes back exactly once and in order, whatever the language does — the
/// worst an unknown construct can cost is a run in the wrong colour.
pub(crate) fn lines(code: &str, lang: &str) -> Vec<CodeLine> {
    let Some(syntax) = syntax(lang) else {
        return Vec::new();
    };
    let mut in_block = false;
    code.lines()
        .map(|line| {
            let runs = scan(line, &syntax, &mut in_block);
            CodeLine {
                tokens: ModelRc::new(VecModel::from(
                    runs.into_iter()
                        .map(|(text, kind)| CodeToken {
                            text: SharedString::from(text),
                            color: kind.color(),
                        })
                        .collect::<Vec<_>>(),
                )),
            }
        })
        .collect()
}

/// One line, as runs of text and what each run is. `in_block` carries a block comment across
/// lines and is updated.
fn scan<'a>(line: &'a str, syntax: &Syntax, in_block: &mut bool) -> Vec<(&'a str, Kind)> {
    let mut out: Vec<(&str, Kind)> = Vec::new();
    let mut i = 0;
    let mut plain_from = 0;

    // Closes the run of ordinary characters gathered so far.
    fn flush<'a>(out: &mut Vec<(&'a str, Kind)>, line: &'a str, from: usize, to: usize) {
        if to > from {
            out.push((&line[from..to], Kind::Plain));
        }
    }

    while i < line.len() {
        if !line.is_char_boundary(i) {
            i += 1;
            continue;
        }
        let rest = &line[i..];

        if *in_block {
            let (_, close) = syntax.block_comment.expect("only set inside a block comment");
            let end = rest.find(close).map(|p| i + p + close.len()).unwrap_or(line.len());
            out.push((&line[i..end], Kind::Comment));
            *in_block = end == line.len() && !rest.contains(close);
            i = end;
            plain_from = i;
            continue;
        }

        if let Some((open, close)) = syntax.block_comment {
            if rest.starts_with(open) {
                flush(&mut out, line, plain_from, i);
                let after_open = rest.strip_prefix(open).unwrap_or_default();
                let end = after_open
                    .find(close)
                    .map(|p| i + open.len() + p + close.len())
                    .unwrap_or(line.len());
                out.push((&line[i..end], Kind::Comment));
                *in_block = end == line.len() && !after_open.contains(close);
                i = end;
                plain_from = i;
                continue;
            }
        }

        if syntax.line_comment.iter().any(|p| rest.starts_with(p)) {
            flush(&mut out, line, plain_from, i);
            out.push((rest, Kind::Comment));
            return out;
        }

        let ch = rest.chars().next().expect("i is on a boundary inside the line");
        if syntax.quotes.contains(&ch) {
            flush(&mut out, line, plain_from, i);
            let end = string_end(line, i, ch);
            out.push((&line[i..end], Kind::String));
            i = end;
            plain_from = i;
            continue;
        }

        if ch.is_ascii_digit() && !starts_inside_word(line, i) {
            flush(&mut out, line, plain_from, i);
            let end = word_end(line, i, |c| c.is_ascii_alphanumeric() || c == '.' || c == '_');
            out.push((&line[i..end], Kind::Number));
            i = end;
            plain_from = i;
            continue;
        }

        // Markup: `<tag`, `</tag`. The name is whatever follows, not a word from a list.
        if syntax.tags && ch == '<' {
            let after = i + 1 + usize::from(rest[1..].starts_with('/'));
            let end = word_end(line, after, |c| c.is_alphanumeric() || c == '-' || c == ':');
            if end > after {
                flush(&mut out, line, plain_from, i);
                out.push((&line[i..end], Kind::Keyword));
                i = end;
                plain_from = i;
                continue;
            }
        }

        if ch.is_alphabetic() || ch == '_' {
            let end = word_end(line, i, |c| c.is_alphanumeric() || c == '_');
            let word = &line[i..end];
            if syntax.keywords.contains(&word) {
                flush(&mut out, line, plain_from, i);
                out.push((word, Kind::Keyword));
                plain_from = end;
            }
            i = end;
            continue;
        }

        i += ch.len_utf8();
    }
    flush(&mut out, line, plain_from, line.len());
    out
}

/// Where the string opened at `start` ends, past its closing quote; the end of the line when
/// it never closes, which is what an unfinished string looks like while it is being typed.
fn string_end(line: &str, start: usize, quote: char) -> usize {
    let mut chars = line[start + quote.len_utf8()..].char_indices();
    while let Some((offset, c)) = chars.next() {
        if c == '\\' {
            chars.next();
            continue;
        }
        if c == quote {
            return start + quote.len_utf8() + offset + c.len_utf8();
        }
    }
    line.len()
}

/// Where the run of characters matching `keep` starting at `start` ends.
fn word_end(line: &str, start: usize, keep: impl Fn(char) -> bool) -> usize {
    line[start..]
        .char_indices()
        .find(|(_, c)| !keep(*c))
        .map(|(offset, _)| start + offset)
        .unwrap_or(line.len())
}

/// Whether the character at `at` continues a word, so `utf8` is not read as the number 8.
fn starts_inside_word(line: &str, at: usize) -> bool {
    line[..at]
        .chars()
        .next_back()
        .is_some_and(|c| c.is_alphanumeric() || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The runs of one line, as (text, kind).
    fn runs<'a>(line: &'a str, lang: &str) -> Vec<(&'a str, Kind)> {
        let syntax = syntax(lang).expect("a language this build knows");
        let mut in_block = false;
        scan(line, &syntax, &mut in_block)
    }

    /// Whatever the language, the block comes back whole: this is code someone wrote, and a
    /// scanner that drops a character of it is worse than no colour at all.
    #[test]
    fn every_character_survives() {
        for (line, lang) in [
            ("fn main() { let x = 1; }", "rust"),
            ("// just a comment", "rust"),
            ("let s = \"a \\\" b\"; // after", "rust"),
            ("/* open", "rust"),
            ("x = 'unterminated", "python"),
            ("가나다 = \"한글\"  # 주석", "python"),
            ("SELECT * FROM t WHERE a = 'b'", "sql"),
            ("", "rust"),
            ("utf8 and 3.14 and x2", "rust"),
        ] {
            let joined: String = runs(line, lang).iter().map(|(t, _)| *t).collect();
            assert_eq!(joined, line, "for {line:?}");
        }
    }

    #[test]
    fn keywords_strings_numbers_and_comments_are_told_apart() {
        let got = runs("let x = 42; // note", "rust");
        assert!(got.contains(&("let", Kind::Keyword)));
        assert!(got.contains(&("42", Kind::Number)));
        assert!(got.contains(&("// note", Kind::Comment)));
        assert!(runs("s = \"hi\"", "python").contains(&("\"hi\"", Kind::String)));
    }

    /// A word that merely contains a keyword is not one, and a digit inside a name is not a
    /// number.
    #[test]
    fn only_whole_words_count() {
        assert!(!runs("letter = 1", "rust").contains(&("let", Kind::Keyword)));
        assert!(!runs("utf8 = 1", "rust").contains(&("8", Kind::Number)));
    }

    /// A block comment that opens on one line goes on until it closes.
    #[test]
    fn a_block_comment_runs_past_the_end_of_a_line() {
        let syntax = syntax("rust").unwrap();
        let mut in_block = false;
        let first = scan("code /* start", &syntax, &mut in_block);
        assert!(in_block, "the comment is still open");
        assert!(first.contains(&("/* start", Kind::Comment)));
        let second = scan("still comment */ code", &syntax, &mut in_block);
        assert!(!in_block, "and closes on the next line");
        assert!(second.contains(&("still comment */", Kind::Comment)));
    }

    /// Markup has no keyword list: what reads as a tag is whatever follows a `<`.
    #[test]
    fn markup_colours_its_tags_and_attributes() {
        let got = runs("<b class=\"x\">hi</b>", "html");
        assert!(got.contains(&("<b", Kind::Keyword)));
        assert!(got.contains(&("</b", Kind::Keyword)));
        assert!(got.contains(&("\"x\"", Kind::String)));
        // And nothing is lost on the way.
        let joined: String = got.iter().map(|(t, _)| *t).collect();
        assert_eq!(joined, "<b class=\"x\">hi</b>");
    }

    /// A fence with no language, or one nobody here knows, is left plain.
    #[test]
    fn an_unknown_language_is_not_coloured() {
        assert!(syntax("").is_none());
        assert!(syntax("brainfuck").is_none());
        assert!(lines("anything", "").is_empty());
        // The tag is taken as written apart from case and space around it.
        assert!(syntax("rust").is_some());
        assert!(syntax("Python").is_some());
        assert!(syntax("  sh  ").is_some());
    }
}
