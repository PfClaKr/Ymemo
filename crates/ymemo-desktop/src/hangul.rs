//! A workaround for one Slint text bug, kept in one place so it can be deleted in one.
//!
//! **Slint 1.17.1 draws 27 Korean syllables as two loose letters.** Exactly `U+AC01`..`U+AC1B`
//! — 각 갂 갃 간 … 갛, every syllable that is ㄱ + ㅏ + a final — come out as `가` followed by
//! a stranded jamo: "감사합니다" reads "가ㅁ사합니다". Measured on both renderers, with the
//! font forced and with it left to fall back, and the same in `Text`, `TextInput` and
//! `StyledText`. The text itself is fine (the store holds `U+AC11`), the font is fine (the
//! same glyphs draw correctly through any other rasteriser), and every neighbouring block —
//! 개, 나, 바, 하 — is fine. It is the first block of the syllable range, the one whose
//! lead-and-vowel index is zero.
//!
//! Slint composes *conjoining* jamo correctly, though, so handing it the same syllables
//! decomposed draws them properly. That is the whole trick here: take those 27 apart on the
//! way to the screen, leave every other character exactly as it is, and never touch what is
//! stored or synced.
//!
//! It is applied where text is only ever **read**: the catalog's own strings, list rows, a
//! sticky's title, the read view. Not to the body being typed in — the field's text is the
//! memo, and a note held decomposed would delete a jamo at a time under backspace and reach
//! the vault in a form the phone never writes.
//!
//! **Delete this file when Slint draws 각 correctly**, along with the calls to [`for_slint`].

/// The first syllable of the broken block, which is also the base they decompose against.
const GA: u32 = 0xAC00;
/// One past the last broken syllable: 개, which draws correctly.
const GAE: u32 = 0xAC1C;
/// Conjoining jamo: the lead ㄱ and the vowel ㅏ that every syllable in the block starts with.
const LEAD_G: char = '\u{1100}';
const VOWEL_A: char = '\u{1161}';
/// The first conjoining final; the block's finals follow it in order.
const FIRST_FINAL: u32 = 0x11A7;

/// Rewrites the syllables Slint cannot draw, and nothing else.
///
/// Returns the input untouched when it holds none of them, which is every string in the app
/// that is not Korean and most of the ones that are.
pub(crate) fn for_slint(text: &str) -> String {
    if !text.chars().any(is_broken) {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len() + 8);
    for c in text.chars() {
        if is_broken(c) {
            out.push(LEAD_G);
            out.push(VOWEL_A);
            // The final is the offset from 가, counted from the first conjoining final.
            out.push(
                char::from_u32(FIRST_FINAL + (c as u32 - GA))
                    .expect("a conjoining final for every offset in the block"),
            );
        } else {
            out.push(c);
        }
    }
    out
}

/// Puts back together what [`for_slint`] took apart.
///
/// The rename boxes are seeded from what is on screen, so text that went out decomposed comes
/// back the same way and would be **stored** like that — a form the phone never writes and
/// nothing else in the vault uses. This is the exact inverse of [`for_slint`] and nothing
/// more: it recomposes ㄱ + ㅏ + a final, and leaves every other jamo sequence alone.
pub(crate) fn from_slint(text: &str) -> String {
    if !text.contains(LEAD_G) {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c != LEAD_G || chars.peek() != Some(&VOWEL_A) {
            out.push(c);
            continue;
        }
        let mut rest = chars.clone();
        rest.next(); // the vowel
        match rest.next().map(|f| f as u32) {
            Some(final_) if (FIRST_FINAL + 1..FIRST_FINAL + (GAE - GA)).contains(&final_) => {
                out.push(
                    char::from_u32(GA + (final_ - FIRST_FINAL))
                        .expect("a syllable for every final in the block"),
                );
                chars = rest;
            }
            // ㄱ + ㅏ with no final of ours after it: 가, which never needed taking apart.
            _ => out.push(c),
        }
    }
    out
}

/// Whether Slint draws this character as two letters instead of one.
fn is_broken(c: char) -> bool {
    (GA + 1..GAE).contains(&(c as u32))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The 27 that break are taken apart; 가 itself, and every other block, are not.
    #[test]
    fn only_the_broken_block_is_touched() {
        assert_eq!(for_slint("가"), "가");
        assert_eq!(for_slint("개객"), "개객");
        assert_eq!(for_slint("나낙"), "나낙");
        assert_eq!(for_slint("hello"), "hello");
        assert_eq!(for_slint(""), "");

        // 감 becomes ㄱ + ㅏ + ㅁ, which Slint composes back into one syllable on screen.
        assert_eq!(for_slint("감"), "\u{1100}\u{1161}\u{11B7}");
        assert_eq!(for_slint("각"), "\u{1100}\u{1161}\u{11A8}");
        assert_eq!(for_slint("갛"), "\u{1100}\u{1161}\u{11C2}");
    }

    /// What comes out means the same as what went in: it is the same text, differently
    /// encoded, and normalising it back gives the original.
    #[test]
    fn the_text_is_unchanged_apart_from_its_encoding() {
        for sample in ["감사합니다", "각각 강남 값", "메모를 끌어 폴더 위에 놓으면 그 안으로 들어갑니다."] {
            let shaped = for_slint(sample);
            assert_eq!(
                shaped.chars().filter(|c| !is_jamo(*c)).count()
                    + shaped.chars().filter(|c| *c == LEAD_G).count(),
                sample.chars().count(),
                "one syllable in, one syllable out for {sample}"
            );
        }
    }

    fn is_jamo(c: char) -> bool {
        (0x1100..0x1200).contains(&(c as u32))
    }

    /// What goes to the screen comes back from a rename box unchanged.
    #[test]
    fn the_round_trip_is_lossless() {
        for sample in [
            "감사합니다",
            "각각 강남 값",
            "가",
            "개객 나낙",
            "plain english",
            "",
            "간단한 메모 — 값 각",
        ] {
            assert_eq!(from_slint(&for_slint(sample)), sample, "round trip of {sample}");
        }
    }

    /// Text that was never taken apart is left exactly as it is, jamo and all.
    #[test]
    fn composing_leaves_other_jamo_alone() {
        // ㄱ + ㅏ with nothing after it is 가, which is not one of the broken ones.
        assert_eq!(from_slint("\u{1100}\u{1161}"), "\u{1100}\u{1161}");
        // A lead that is not ours.
        assert_eq!(from_slint("\u{1102}\u{1161}\u{11A8}"), "\u{1102}\u{1161}\u{11A8}");
    }

    /// Every syllable in the block round-trips: 27 of them, none missed at either end.
    #[test]
    fn the_whole_block_decomposes() {
        for cp in GA + 1..GAE {
            let c = char::from_u32(cp).unwrap();
            let shaped = for_slint(&c.to_string());
            assert_eq!(shaped.chars().count(), 3, "{c} should become three jamo");
            assert!(shaped.starts_with(LEAD_G));
        }
        // And the two syllables either side of the block are left alone.
        assert_eq!(for_slint("가"), "가");
        assert_eq!(for_slint("개"), "개");
    }
}
