//! Splitting a memo into the blocks the sticky's read view draws.
//!
//! The memo itself is never touched: what is stored is the markdown the user typed, and this
//! only decides how it is *shown* while the caret is somewhere else. Click into the note and
//! the plain text comes back, markers and all — see `sticky.slint`.
//!
//! A memo is plain writing with marked-off regions in it, the way a chat message is:
//!
//! - ordinary lines are **plain text**. `**stars**` typed in a shopping list are stars.
//! - a bare ` ``` ` fence opens a **markdown region**: inside it the marks mean what they say.
//! - ` ```rust ` and friends open a **code block**, coloured by `highlight.rs`.
//!
//! Marking the formatting off is the point: nobody has to escape anything, and a memo that
//! was never meant to be markdown cannot be reformatted behind the user's back.
//!
//! Slint's own `StyledText` reads a subset of commonmark and covers the inline marks — bold,
//! italic, strikethrough, inline code, links, lists. It does **not** do headings or fenced
//! code blocks, which are why this splits headings out itself (a block with a bigger font)
//! and hands code over separately (monospace on a tinted card).
//!
//! **Keep in step with the phone's `markdown_style.dart`**, which recognises the same subset
//! while typing.

use crate::NoteBlock;
use ymemo_core::diag;

/// A block of prose, with its markdown already parsed.
///
/// Parsing here rather than in the `.slint`: `@markdown()` reads the literal it is written
/// with, so a memo — which only exists at runtime — has to arrive as styled text. `text`
/// keeps the markdown it came from: nothing draws it, and it is what the tests read back.
fn prose_block(markdown: &str, font_size: f32) -> NoteBlock {
    let styled = slint::StyledText::from_markdown(markdown).unwrap_or_else(|e| {
        // Markdown that will not parse is still writing, and dropping it would look like the
        // memo had lost a paragraph. Shown as it was typed instead.
        diag!("could not read the memo's markdown, showing it as written: {e}");
        slint::StyledText::from_plain_text(markdown)
    });
    NoteBlock {
        styled,
        text: markdown.into(),
        code: false,
        lines: Default::default(),
        font_size,
    }
}

/// A block of writing that is not in a markdown region: shown exactly as it was typed.
fn plain_block(text: &str) -> NoteBlock {
    NoteBlock {
        styled: slint::StyledText::from_plain_text(text),
        text: text.into(),
        code: false,
        lines: Default::default(),
        font_size: BODY_FONT_PX,
    }
}

/// A block of code, drawn as it was typed.
///
/// `lang` is whatever followed the opening fence. Naming a language colours the block, the
/// way it does in a chat box; naming nothing — or something this build does not know — leaves
/// it plain, which is what a fence has always done. `lines` empty means "draw `text` as it
/// is", so the two never disagree about what the block says.
fn code_block(text: &str, lang: &str) -> NoteBlock {
    NoteBlock {
        styled: Default::default(),
        text: text.into(),
        code: true,
        lines: slint::ModelRc::new(slint::VecModel::from(crate::highlight::lines(text, lang))),
        font_size: BODY_FONT_PX,
    }
}

/// Body font size (logical px) the block sizes are measured against.
/// **Must match the body `font-size` in `ui/sticky.slint`.**
const BODY_FONT_PX: f32 = 13.0;

/// How much bigger a heading is than the body, by level (1..6).
/// **Keep in step with `_headingScale` in `markdown_style.dart`.**
const HEADING_SCALE: [f32; 6] = [1.45, 1.30, 1.18, 1.10, 1.05, 1.0];

/// Splits a memo body into blocks to draw, in order.
///
/// Consecutive lines stay in one block so the text wraps as one paragraph rather than one
/// stiff line per newline. What each run of lines becomes is decided by the fence it is in —
/// see the note at the top of this file.
pub(crate) fn blocks(body: &str) -> Vec<NoteBlock> {
    let mut out = Vec::new();
    let mut held: Vec<&str> = Vec::new();
    // The language of the fence we are inside, empty for a bare one; `None` outside any.
    let mut fence: Option<String> = None;

    for line in body.lines() {
        match line.trim_start().strip_prefix("```") {
            Some(tag) => {
                match fence.take() {
                    Some(lang) => close(&mut held, &lang, &mut out),
                    None => {
                        flush_plain(&mut held, &mut out);
                        // Only the opening fence names a language; the closing one is bare.
                        fence = Some(tag.trim().to_string());
                    }
                }
            }
            None => held.push(line),
        }
    }
    // A fence left open holds what it has rather than throwing it away — it is being typed.
    match fence {
        Some(lang) => close(&mut held, &lang, &mut out),
        None => flush_plain(&mut held, &mut out),
    }
    out
}

/// Turns what a fence held into blocks, by what the fence said it was.
fn close(held: &mut Vec<&str>, lang: &str, out: &mut Vec<NoteBlock>) {
    if lang.is_empty() {
        flush_markdown(held, out);
    } else {
        // Even an empty block someone deliberately opened is worth showing as empty.
        out.push(code_block(&held.join("\n"), lang));
        held.clear();
    }
}

/// Writing outside any fence: one block, exactly as typed.
fn flush_plain(held: &mut Vec<&str>, out: &mut Vec<NoteBlock>) {
    if held.iter().any(|l| !l.trim().is_empty()) {
        out.push(plain_block(&held.join("\n")));
    }
    held.clear();
}

/// The inside of a bare fence: the marks mean what they say, and a heading becomes a block of
/// its own so it can be drawn larger.
fn flush_markdown(held: &mut Vec<&str>, out: &mut Vec<NoteBlock>) {
    let mut para: Vec<&str> = Vec::new();
    fn flush(lines: &mut Vec<&str>, out: &mut Vec<NoteBlock>) {
        if lines.iter().any(|l| !l.trim().is_empty()) {
            out.push(prose_block(&lines.join("\n"), BODY_FONT_PX));
        }
        lines.clear();
    }
    for line in held.iter() {
        match heading_level(line) {
            0 => para.push(line),
            level => {
                flush(&mut para, out);
                // Bold, because Slint's markdown has no headings of its own; the size is
                // what actually says "heading" and the weight keeps it from reading as a
                // paragraph that happens to be large.
                out.push(prose_block(
                    &format!("**{}**", escape(line[level + 1..].trim())),
                    BODY_FONT_PX * HEADING_SCALE[level - 1],
                ));
            }
        }
    }
    flush(&mut para, out);
    held.clear();
}

/// 1..6 for a heading line, 0 for anything else.
///
/// `#` with no space after it is not a heading — `#tag` is a word, not a title — and seven
/// hashes is not one either.
fn heading_level(line: &str) -> usize {
    let hashes = line.bytes().take_while(|b| *b == b'#').count();
    if hashes == 0 || hashes > 6 {
        return 0;
    }
    if line.as_bytes().get(hashes) == Some(&b' ') {
        hashes
    } else {
        0
    }
}

/// Keeps a heading's own text from being read as more markdown once it is wrapped in `**`.
///
/// Only the two characters that would close the wrapper early; everything else in the line is
/// left as the user wrote it, so `# **already bold**` still reads as a heading.
fn escape(text: &str) -> String {
    text.replace('\\', "\\\\").replace("**", "\\*\\*")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every block as (text, is-code), so a test can say what the memo turned into.
    fn parts(body: &str) -> Vec<(String, bool)> {
        blocks(body)
            .into_iter()
            .map(|b| (b.text.to_string(), b.code))
            .collect()
    }

    /// Writing outside a fence is writing: the marks are characters, not instructions.
    #[test]
    fn ordinary_lines_are_left_exactly_as_typed() {
        let got = blocks("buy **milk**\nand *bread*");
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].text.to_string(), "buy **milk**\nand *bread*");
        assert_eq!(got[0].styled, slint::StyledText::from_plain_text("buy **milk**\nand *bread*"));
    }

    /// A bare fence is where the marks start meaning something.
    #[test]
    fn a_bare_fence_is_a_markdown_region() {
        let got = blocks("plain\n```\nbuy **milk**\n```\nplain again");
        assert_eq!(got.len(), 3);
        assert_eq!(got[0].text.to_string(), "plain");
        assert_eq!(got[0].styled, slint::StyledText::from_plain_text("plain"));
        // The middle one was parsed rather than taken as written.
        assert_eq!(got[1].text.to_string(), "buy **milk**");
        assert_ne!(got[1].styled, slint::StyledText::from_plain_text("buy **milk**"));
        assert_eq!(got[2].text.to_string(), "plain again");
    }

    /// A fence that names a language is code, not markdown.
    #[test]
    fn a_named_fence_is_a_code_block() {
        assert_eq!(
            parts("before\n```rust\nfn main() {}\n```\nafter"),
            [
                ("before".to_string(), false),
                ("fn main() {}".to_string(), true),
                ("after".to_string(), false),
            ]
        );
    }

    /// Two code blocks in a row, each in its own language, with writing between them.
    #[test]
    fn regions_follow_one_another() {
        let got = parts("```\nmd here\n```\ntext\n```c\nint x;\n```\n```html\n<b>hi</b>\n```\ntail");
        assert_eq!(
            got,
            [
                ("md here".to_string(), false),
                ("text".to_string(), false),
                ("int x;".to_string(), true),
                ("<b>hi</b>".to_string(), true),
                ("tail".to_string(), false),
            ]
        );
    }

    #[test]
    fn an_unclosed_fence_keeps_the_rest_rather_than_dropping_it() {
        assert_eq!(
            parts("prose\n```rust\nstill code\nand this"),
            [
                ("prose".to_string(), false),
                ("still code\nand this".to_string(), true),
            ]
        );
        // And a bare one keeps it as markdown.
        assert_eq!(parts("```\n# Title"), [("**Title**".to_string(), false)]);
    }

    /// A heading is only a heading inside a markdown region, and is drawn larger there.
    #[test]
    fn a_heading_is_its_own_block_and_bigger() {
        let got = blocks("```\n# Title\nbody\n```");
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].text.to_string(), "**Title**");
        assert!(got[0].font_size > got[1].font_size);
        // Outside a region it is just a line that starts with a hash.
        let outside = blocks("# Title");
        assert_eq!(outside.len(), 1);
        assert_eq!(outside[0].text.to_string(), "# Title");
        assert_eq!(outside[0].font_size, BODY_FONT_PX);
    }

    /// `#tag` is a word and `#######` is too deep; neither is a title.
    #[test]
    fn only_a_real_heading_is_a_heading() {
        assert_eq!(heading_level("#tag"), 0);
        assert_eq!(heading_level("####### deep"), 0);
        assert_eq!(heading_level("###### six"), 6);
    }

    /// A heading is wrapped in `**` to make it bold, so its own asterisks must not close it.
    #[test]
    fn a_heading_cannot_break_out_of_its_own_wrapper() {
        let got = blocks("```\n# a ** b\n```");
        assert_eq!(got[0].text.to_string(), "**a \\*\\* b**");
    }

    /// Blank lines alone are not a paragraph; an empty memo draws nothing.
    #[test]
    fn nothing_to_show_is_no_blocks() {
        assert!(blocks("").is_empty());
        assert!(blocks("\n\n\n").is_empty());
        assert!(blocks("```\n\n```").is_empty());
    }
}
