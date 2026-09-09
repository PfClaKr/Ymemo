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

/// A block of prose, or `None` when Slint's markdown will not read it.
///
/// Parsing here rather than in the `.slint`: `@markdown()` reads the literal it is written
/// with, so a memo — which only exists at runtime — has to arrive as styled text. `text`
/// keeps the markdown it came from: nothing draws it, and it is what the tests read back.
fn parsed(markdown: &str, font_size: f32) -> Option<NoteBlock> {
    let markdown = &crate::hangul::for_slint(markdown);
    Some(NoteBlock {
        styled: slint::StyledText::from_markdown(markdown).ok()?,
        text: markdown.into(),
        code: false,
        lines: Default::default(),
        font_size,
    })
}

/// One paragraph of a markdown region, as the blocks it takes to draw it.
///
/// Slint reads a **subset** of commonmark, and refuses the whole string over one line it does
/// not know: headings, horizontal rules, block quotes, images, indented code and inline HTML
/// are each enough to do it. So a single `> quote` used to cost every other line around it
/// its bold and its bullets. When the paragraph will not parse it is rebuilt a line at a
/// time: each run of lines that still parses together stays one block, and only the line that
/// stopped it is drawn as it was typed.
///
/// None of this is worth a `diag!`. A memo holding a construct Slint cannot draw is ordinary
/// writing, not a failure, and the note is re-split on **every keystroke** — one line per
/// character typed would push the log's real contents out inside a paragraph.
fn prose_blocks(markdown: &str, font_size: f32, out: &mut Vec<NoteBlock>) {
    if let Some(block) = parsed(markdown, font_size) {
        out.push(block);
        return;
    }
    let mut run: Vec<&str> = Vec::new();
    for line in markdown.lines() {
        run.push(line);
        if parsed(&run.join("\n"), font_size).is_some() {
            continue;
        }
        // This line is what stopped it: hand back the run without it, then the line itself.
        run.pop();
        flush_run(&mut run, font_size, out);
        out.push(plain_block(line, font_size));
    }
    flush_run(&mut run, font_size, out);
}

/// Pushes a run of lines already known to parse, and empties it.
fn flush_run(run: &mut Vec<&str>, font_size: f32, out: &mut Vec<NoteBlock>) {
    if run.is_empty() {
        return;
    }
    let text = run.join("\n");
    run.clear();
    // The `unwrap_or_else` cannot fire — the run parsed on the way in — and a note is not
    // worth a panic if it ever did.
    out.push(parsed(&text, font_size).unwrap_or_else(|| plain_block(&text, font_size)));
}

/// A block of writing that is not in a markdown region: shown exactly as it was typed.
fn plain_block(text: &str, font_size: f32) -> NoteBlock {
    let text = &crate::hangul::for_slint(text);
    NoteBlock {
        styled: slint::StyledText::from_plain_text(text),
        text: text.into(),
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
        text: crate::hangul::for_slint(text).into(),
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
        out.push(plain_block(&held.join("\n"), BODY_FONT_PX));
    }
    held.clear();
}

/// The inside of a bare fence: the marks mean what they say, and a heading becomes a block of
/// its own so it can be drawn larger.
fn flush_markdown(held: &mut Vec<&str>, out: &mut Vec<NoteBlock>) {
    let mut para: Vec<&str> = Vec::new();
    fn flush(lines: &mut Vec<&str>, out: &mut Vec<NoteBlock>) {
        if lines.iter().any(|l| !l.trim().is_empty()) {
            prose_blocks(&lines.join("\n"), BODY_FONT_PX, out);
        }
        lines.clear();
    }
    for line in held.iter() {
        // A blank line ends a paragraph, which is what it means in markdown and what keeps a
        // line Slint refuses from dragging the paragraph on the other side of the gap down
        // with it.
        if line.trim().is_empty() {
            flush(&mut para, out);
            continue;
        }
        match heading_level(line) {
            0 => para.push(line),
            level => {
                flush(&mut para, out);
                let title = line[level + 1..].trim();
                // A heading with nothing after the hashes is nothing to draw — and wrapped
                // in the `**` below it would be `****`, which commonmark reads as a
                // horizontal rule and Slint then refuses outright.
                if title.is_empty() {
                    continue;
                }
                // Bold, because Slint's markdown has no headings of its own; the size is
                // what actually says "heading" and the weight keeps it from reading as a
                // paragraph that happens to be large.
                prose_blocks(
                    &format!("**{}**", escape(title)),
                    BODY_FONT_PX * HEADING_SCALE[level - 1],
                    out,
                );
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

    /// A line Slint's markdown cannot read costs only itself.
    ///
    /// It reads a subset, and refuses the whole string over one line outside it — so the
    /// paragraph is rebuilt around the offending line rather than given up on.
    #[test]
    fn one_line_slint_cannot_read_does_not_flatten_the_rest() {
        for bad in ["> quote", "![img](x.png)", "---", "<b>tag</b>", "#"] {
            let body = format!("```\nbefore **bold**\n\n{bad}\n\nafter **bold**\n```");
            let got = blocks(&body);
            // The bad line is there, exactly as typed, drawn plain.
            let plain = got
                .iter()
                .find(|b| b.text == bad)
                .unwrap_or_else(|| panic!("{bad} should survive as its own block"));
            assert_eq!(plain.styled, slint::StyledText::from_plain_text(bad), "{bad}");
            // And the writing around it is still markdown.
            for side in ["before **bold**", "after **bold**"] {
                let b = got
                    .iter()
                    .find(|b| b.text == side)
                    .unwrap_or_else(|| panic!("{side} should stay its own block next to {bad}"));
                assert_ne!(b.styled, slint::StyledText::from_plain_text(side), "{bad}");
            }
        }
    }

    /// Every character of a paragraph comes back out, whatever Slint made of it.
    #[test]
    fn nothing_is_dropped_when_a_paragraph_is_rebuilt() {
        let body = "```\n# Title\n> quote\nplain **bold**\n---\ntail\n```";
        let joined: Vec<String> = blocks(body).iter().map(|b| b.text.to_string()).collect();
        assert_eq!(joined.join("\n"), "**Title**\n> quote\nplain **bold**\n---\ntail");
    }

    /// `# ` with nothing after it is an empty heading, not a horizontal rule.
    ///
    /// Wrapped in the `**` that makes a heading bold it came out as `****`, which commonmark
    /// reads as a rule — so the block was refused and the note drew four asterisks.
    #[test]
    fn a_heading_with_no_words_draws_nothing() {
        assert!(blocks("```\n# \n```").is_empty());
        let got = blocks("```\n# \nbody\n```");
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].text.to_string(), "body");
    }

    /// Blank lines alone are not a paragraph; an empty memo draws nothing.
    #[test]
    fn nothing_to_show_is_no_blocks() {
        assert!(blocks("").is_empty());
        assert!(blocks("\n\n\n").is_empty());
        assert!(blocks("```\n\n```").is_empty());
    }
}

