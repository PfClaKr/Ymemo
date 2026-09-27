//! The short form of an explanation: its first sentence.
//!
//! Every setting carries a hint, and several ran four lines; a window of them read as a wall
//! of text. The settings window shows this under each one and the whole hint behind an ⓘ.
//! `build.rs` generates a `*_hint_short` string beside every `*_hint` key from it, so the
//! catalog stays one sentence-per-thought text and nothing has to be written twice. The phone
//! does the same (`_firstSentence` in its settings screen).

/// Up to and including the first full stop that ends a sentence (". ", or the same with "?"
/// or "!"), or all of it when there is only one.
pub(crate) fn first_sentence(text: &str) -> String {
    let text = text.trim();
    let bytes = text.as_bytes();
    for (i, b) in bytes.iter().enumerate() {
        if matches!(b, b'.' | b'?' | b'!') && bytes.get(i + 1).is_some_and(|n| n.is_ascii_whitespace()) {
            return text[..=i].to_string();
        }
    }
    text.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_sentence_stands_alone() {
        assert_eq!(first_sentence("한 문장입니다. 둘째 문장입니다."), "한 문장입니다.");
        assert_eq!(first_sentence("One. Two."), "One.");
        assert_eq!(first_sentence("Only one."), "Only one.");
        // A dot inside a word or a number is not the end of a sentence.
        assert_eq!(first_sentence("v1.1.0 is out. More."), "v1.1.0 is out.");
        assert_eq!(first_sentence("  no stop at all  "), "no stop at all");
    }
}
