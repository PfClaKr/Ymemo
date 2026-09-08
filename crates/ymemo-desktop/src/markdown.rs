//! Splitting a memo into the blocks the sticky's read view draws.
//!
//! The memo itself is never touched: what is stored is the markdown the user typed, and this
//! only decides how it is *shown* while the caret is somewhere else. Click into the note and
//! the plain text comes back, markers and all — see `sticky.slint`.
//!
//! Slint's own `StyledText` reads a subset of commonmark and covers the inline marks — bold,
//! italic, strikethrough, inline code, links, lists. It does **not** do headings or fenced
//! code blocks, which are exactly the two this splits out and hands to the UI separately: a
//! heading as a block with a bigger font, a fence as a block drawn in monospace on a tinted
//! card.
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
/// Consecutive ordinary lines stay in one block so the text wraps as one paragraph rather
/// than one stiff line per newline.
pub(crate) fn blocks(body: &str) -> Vec<NoteBlock> {
    let mut out = Vec::new();
    let mut para: Vec<&str> = Vec::new();
    let mut fence: Vec<&str> = Vec::new();
    let mut in_fence = false;
    let mut lang = String::new();

    // Flushes whatever prose has been gathered, so a heading or a fence cannot swallow it.
    fn flush(lines: &mut Vec<&str>, out: &mut Vec<NoteBlock>) {
        if lines.iter().any(|l| !l.trim().is_empty()) {
            out.push(prose_block(&lines.join("\n"), BODY_FONT_PX));
        }
        lines.clear();
    }

    for line in body.lines() {
        if let Some(tag) = line.trim_start().strip_prefix("```") {
            if in_fence {
                // The fence closes: everything gathered is one card, even if it is empty —
                // an empty block someone deliberately opened is worth showing as empty.
                out.push(code_block(&fence.join("\n"), &lang));
                fence.clear();
            } else {
                flush(&mut para, &mut out);
                // Only the opening fence names the language; the closing one is bare.
                lang = tag.trim().to_string();
            }
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            fence.push(line);
            continue;
        }
        match heading_level(line) {
            0 => para.push(line),
            level => {
                flush(&mut para, &mut out);
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
    // A fence left open runs to the end of the memo rather than being thrown away.
    if in_fence {
        out.push(code_block(&fence.join("\n"), &lang));
    }
    flush(&mut para, &mut out);
    out
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

    fn parts(body: &str) -> Vec<(String, bool)> {
        blocks(body)
            .into_iter()
            .map(|b| (b.text.to_string(), b.code))
            .collect()
    }

    #[test]
    fn prose_stays_one_block_so_it_wraps_as_a_paragraph() {
        assert_eq!(
            parts("one\ntwo\nthree"),
            [("one\ntwo\nthree".to_string(), false)]
        );
    }

    #[test]
    fn a_fence_becomes_a_block_of_its_own() {
        assert_eq!(
            parts("before\n```\nfn main() {}\n```\nafter"),
            [
                ("before".to_string(), false),
                ("fn main() {}".to_string(), true),
                ("after".to_string(), false),
            ]
        );
    }

    #[test]
    fn an_unclosed_fence_keeps_the_rest_rather_than_dropping_it() {
        assert_eq!(
            parts("prose\n```\nstill code\nand this"),
            [
                ("prose".to_string(), false),
                ("still code\nand this".to_string(), true),
            ]
        );
    }

    #[test]
    fn a_heading_is_its_own_block_and_bigger() {
        let got = blocks("# Title\nbody");
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].text.to_string(), "**Title**");
        assert!(got[0].font_size > got[1].font_size);
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
        let got = blocks("# a ** b");
        assert_eq!(got[0].text.to_string(), "**a \\*\\* b**");
    }

    /// Blank lines alone are not a paragraph; an empty memo draws nothing.
    #[test]
    fn nothing_to_show_is_no_blocks() {
        assert!(blocks("").is_empty());
        assert!(blocks("\n\n\n").is_empty());
    }
}
