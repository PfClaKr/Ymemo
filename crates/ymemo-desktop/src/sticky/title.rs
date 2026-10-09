//! A sticky's title: derived from the first line of what is written, until somebody names it.

use ymemo_core::Memo;

use super::sticky_text;

/// First line of the body worth naming a memo by, used as the title in the list and title
/// bar.
///
/// Fence lines are skipped: a memo that opens with ` ```rust ` is about what is inside it,
/// and calling it "```rust" in the list says nothing at all.
///
/// **Keep in step with `firstLine` in the phone's `memo_title.dart`.**
pub(crate) fn derive_title(text: &str) -> String {
    title_line(text).chars().take(40).collect()
}

/// The line a title is taken from, as it should read rather than as it was typed.
///
/// The only difference between the two is a heading's hashes, and only **inside a markdown
/// region**: there `# 회의록` is drawn as a heading saying 회의록, so that is what the memo is
/// called. Outside one — and inside a fence that named a language — a hash is a character
/// like any other and stays, because that is what the read view draws.
pub(super) fn title_line(text: &str) -> &str {
    // `Some(true)` inside a bare fence, `Some(false)` inside one that named a language.
    // The same walk `markdown::blocks` does, and it has to stay the same walk.
    let mut fence: Option<bool> = None;
    for line in text.lines() {
        if let Some(tag) = line.trim_start().strip_prefix("```") {
            fence = if fence.is_some() { None } else { Some(tag.trim().is_empty()) };
            continue;
        }
        let line = line.trim_start();
        // Left-trimmed first and only then stripped: `# ` has to still have its space when
        // the hashes are counted, or it is not a heading and the memo is called "#".
        let named = if fence == Some(true) { strip_heading(line) } else { line }.trim();
        // A line with nothing left to it — a heading with no words — names nothing, so the
        // search goes on to the line below rather than leaving the memo blank.
        if !named.is_empty() {
            return named;
        }
    }
    ""
}

/// `# Title` -> `Title`. Anything that is not a heading comes back whole.
///
/// The same rule `markdown::heading_level` reads one by, so a line drawn as a heading is
/// exactly a line named after its words.
pub(super) fn strip_heading(line: &str) -> &str {
    let hashes = line.bytes().take_while(|b| *b == b'#').count();
    if hashes == 0 || hashes > 6 || line.as_bytes().get(hashes) != Some(&b' ') {
        return line;
    }
    line[hashes + 1..].trim_start()
}

/// What a memo is called on screen: its title, or the first line of its writing when it has
/// none — a memo written on the phone with the title field left blank. The stored title is
/// left alone; the list, a note's title bar and its history all read the memo this way.
pub(crate) fn display_title(memo: &Memo) -> String {
    if memo.title.is_empty() {
        derive_title(&memo.body)
    } else {
        memo.title.clone()
    }
}

/// The title a memo should carry once its body becomes `text`.
///
/// A sticky has no title field — the title *is* the first line of what is written on it — but
/// the phone has one, and a memo written there carries a title its body never mentions.
/// Re-deriving unconditionally meant that opening such a memo here and touching a single
/// character silently renamed it to its first line: the kind of loss that is only noticed
/// much later, on the other device.
///
/// So the test is the memo as it stands. A title that still matches what its own body would
/// produce is this app's own doing and follows the text; anything else was typed by hand
/// somewhere and is left alone.
pub(crate) fn title_for(memo: &Memo, text: &str) -> String {
    if memo.title.is_empty() || memo.title == derive_title(&sticky_text(memo)) {
        derive_title(text)
    } else {
        memo.title.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    /// A memo written here has a title that follows its first line, as it always did.
    #[test]
    fn a_derived_title_follows_the_text() {
        let mut memo = Memo::new("old first line", "old first line\nrest");
        assert_eq!(title_for(&memo, "new first line\nrest"), "new first line");
        // And a memo that never had one gets one.
        memo.title = String::new();
        assert_eq!(title_for(&memo, "first\nsecond"), "first");
    }

    /// A memo that opens with a fence is named by what is inside it, not by the fence.
    #[test]
    fn a_fence_is_not_a_title() {
        assert_eq!(derive_title("```rust\nfn hello() {}\n```"), "fn hello() {}");
        assert_eq!(derive_title("```\n**bold**\n```"), "**bold**");
        // A memo that is nothing but an empty block has no name to give.
        assert_eq!(derive_title("```\n```"), "");
    }

    /// A heading names the memo by its words, and only where a heading is a heading.
    #[test]
    fn a_headings_hashes_are_not_part_of_the_name() {
        assert_eq!(derive_title("```\n# 회의록\n본문\n```"), "회의록");
        assert_eq!(derive_title("```\n### Deep\n```"), "Deep");
        // Outside a markdown region a hash is a character, which is how it is drawn.
        assert_eq!(derive_title("# tag\nbody"), "# tag");
        // Inside a fence that named a language it is code, and code is shown as written.
        assert_eq!(derive_title("```py\n# comment\n```"), "# comment");
        // Past a closed code block the writing is plain again.
        assert_eq!(derive_title("```c\n```\n# still plain"), "# still plain");
        // Not a heading: no space, and too many hashes.
        assert_eq!(derive_title("```\n#tag\n```"), "#tag");
        assert_eq!(derive_title("```\n####### deep\n```"), "####### deep");
        // A heading with no words names nothing, so the line below is asked instead.
        assert_eq!(derive_title("```\n# \n본문\n```"), "본문");
        assert_eq!(derive_title("```\n# \n```"), "");
        // A bare `#` is not a heading, here or in the read view.
        assert_eq!(derive_title("```\n#\n```"), "#");
    }

    /// A title typed on the phone survives an edit made here.
    #[test]
    fn a_hand_written_title_is_left_alone() {
        let memo = Memo::new("Groceries", "milk");
        assert_eq!(
            title_for(&memo, "milk\neggs"),
            "Groceries",
            "editing the body on the desktop must not rename a memo titled elsewhere"
        );
    }

    /// An old memo that only ever had a title still gets one derived: `sticky_text` promotes
    /// that title into the body, so the two do match and the memo is this app's own.
    #[test]
    fn an_old_title_only_memo_still_derives() {
        let memo = Memo::new("just a title", "");
        assert_eq!(title_for(&memo, "just a title\nand now a body"), "just a title");
    }
}
