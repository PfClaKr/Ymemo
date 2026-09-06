//! Where a memo sits in its folder: a **fractional index**, not a position number.
//!
//! The order has to survive two devices rearranging the same folder while neither can see
//! the other, which is the same problem the rest of the vault solves with automerge. A
//! position number cannot: moving one memo renumbers every memo after it, so two devices
//! that each move something write conflicting numbers for memos neither of them touched, and
//! one device's whole arrangement is silently thrown away.
//!
//! A fractional index writes **one memo**. A key is a string, ordered lexicographically, and
//! there is always room for another key between any two — [`between`] finds one, growing the
//! string by a character when it runs out of digits. Two devices that concurrently drop
//! something in the same gap produce two different keys, both survive, and every device
//! agrees on the resulting order.
//!
//! Keys are opaque. Nothing outside this module may parse one, compare one to a number, or
//! assume anything about its length; the only guarantee is that `<` on the string is `<` in
//! the folder. **Ties are possible** — two devices can land on the same key — so a caller
//! sorting by this must break ties on the memo id, or the two memos swap places depending on
//! which device is drawing.

/// The alphabet, in ascending byte order so that `<` on the string agrees with `<` on the
/// digits. Sixty-two digits keeps a key short enough to read in a log line.
const DIGITS: &[u8] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";

/// Position of a byte in [`DIGITS`], or `None` for anything not from the alphabet.
fn digit(byte: u8) -> Option<usize> {
    DIGITS.iter().position(|d| *d == byte)
}

/// The digit at `i`, treating a key that has run out as its smallest possible continuation.
fn digit_at(key: &str, i: usize) -> usize {
    key.as_bytes().get(i).copied().and_then(digit).unwrap_or(0)
}

/// A key that sorts strictly between `prev` and `next`.
///
/// `None` means "no neighbour on that side": `between(None, None)` is the key for the first
/// memo in an empty folder, `between(None, Some(first))` puts one above everything, and
/// `between(Some(last), None)` below everything.
///
/// **`prev` must sort before `next`.** Given a pair that does not, this returns a key after
/// `prev` rather than an impossible one — a folder whose keys got tangled ends up with the
/// memo somewhere sensible instead of the caller getting a key that breaks the ordering.
pub fn between(prev: Option<&str>, next: Option<&str>) -> String {
    let prev = prev.unwrap_or("");
    // A `next` that does not sort after `prev` is not a gap; treat it as the end of the list.
    let mut next = next.filter(|n| prev < *n);

    let mut key = String::new();
    let mut i = 0;
    loop {
        let low = digit_at(prev, i);
        // With no `next` the ceiling is one past the last digit: the gap runs to the end of
        // the folder. A `next` that has run out cannot constrain anything either — that only
        // happens for a pair that was not a gap to begin with.
        let high = match next {
            Some(n) if i < n.len() => digit_at(n, i),
            Some(_) => {
                next = None;
                DIGITS.len()
            }
            None => DIGITS.len(),
        };

        if high - low > 1 {
            key.push(DIGITS[(low + high) / 2] as char);
            return key;
        }
        // The two keys agree here, or sit next to each other with nothing between: take this
        // digit and look for room one place further down. The string grows by one character,
        // which is the whole trick — there is always another place further down.
        key.push(DIGITS[low] as char);
        // Taking a digit **below** next's makes the key strictly smaller than `next` whatever
        // follows, so from here the gap runs to the end and only `prev` still matters. Not
        // noticing this is an endless loop: `next` keeps saying "no room" at every depth while
        // the key it is being compared against has already left it behind.
        if high == low + 1 {
            next = None;
        }
        i += 1;
    }
}

/// `count` keys in ascending order, evenly spaced.
///
/// For giving a folder that has never been arranged its first set of keys, in whatever order
/// it is being shown in at that moment. Evenly spaced rather than one after another, so that
/// the first drag into any gap gets a short key instead of a long one.
pub fn spread(count: usize) -> Vec<String> {
    if count == 0 {
        return Vec::new();
    }
    // Enough digits that every key is distinct with gaps left over: 62^width > count + 1.
    let mut width = 1usize;
    let mut capacity = DIGITS.len() as u128;
    while capacity <= count as u128 + 1 {
        width += 1;
        capacity *= DIGITS.len() as u128;
    }
    (0..count)
        .map(|i| encode(capacity * (i as u128 + 1) / (count as u128 + 1), width))
        .collect()
}

/// `value` as exactly `width` digits, most significant first.
fn encode(value: u128, width: usize) -> String {
    let base = DIGITS.len() as u128;
    let mut out = vec![DIGITS[0]; width];
    let mut rest = value;
    for slot in out.iter_mut().rev() {
        *slot = DIGITS[(rest % base) as usize];
        rest /= base;
    }
    String::from_utf8(out).expect("the alphabet is ascii")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_first_key_leaves_room_on_both_sides() {
        let first = between(None, None);
        assert!(!first.is_empty());
        assert!(between(None, Some(&first)) < first);
        assert!(between(Some(&first), None) > first);
    }

    #[test]
    fn lands_strictly_between_its_neighbours() {
        let a = between(None, None);
        let b = between(Some(&a), None);
        let mid = between(Some(&a), Some(&b));
        assert!(a < mid && mid < b, "{a} < {mid} < {b}");
    }

    // The case a position number cannot survive: dropping into the same gap over and over.
    // Every key has to stay strictly between the pair, however tight the gap has become.
    #[test]
    fn the_same_gap_can_be_filled_forever() {
        let low = between(None, None);
        let mut high = between(Some(&low), None);
        for step in 0..200 {
            let mid = between(Some(&low), Some(&high));
            assert!(low < mid && mid < high, "step {step}: {low} < {mid} < {high}");
            high = mid;
        }
    }

    #[test]
    fn walking_off_either_end_keeps_going() {
        let mut top = between(None, None);
        let mut bottom = top.clone();
        for step in 0..200 {
            let above = between(None, Some(&top));
            let below = between(Some(&bottom), None);
            assert!(above < top, "step {step}: {above} < {top}");
            assert!(below > bottom, "step {step}: {below} > {bottom}");
            top = above;
            bottom = below;
        }
    }

    #[test]
    fn a_spread_is_ascending_and_leaves_gaps() {
        for count in [1usize, 2, 5, 61, 62, 63, 100, 4000] {
            let keys = spread(count);
            assert_eq!(keys.len(), count);
            for pair in keys.windows(2) {
                assert!(pair[0] < pair[1], "count {count}: {} < {}", pair[0], pair[1]);
                // Every gap still takes another memo.
                let mid = between(Some(&pair[0]), Some(&pair[1]));
                assert!(pair[0] < mid && mid < pair[1], "count {count}: no room in a gap");
            }
            // And so do the two ends.
            assert!(between(None, Some(&keys[0])) < keys[0]);
            assert!(between(Some(keys.last().unwrap()), None) > *keys.last().unwrap());
        }
    }

    // Keys from an older version, a hand-edited file, or another implementation must not
    // produce a key that breaks the ordering — the folder just gets a sensible answer.
    #[test]
    fn nonsense_neighbours_do_not_produce_a_broken_key() {
        // Out of order: `next` is ignored and the key lands after `prev`.
        let key = between(Some("m"), Some("a"));
        assert!(key.as_str() > "m");
        // Not from the alphabet at all: still ordered against what it was given.
        let key = between(Some("!!"), None);
        assert!(!key.is_empty());
        // An empty key is the smallest thing there is, so anything lands after it.
        assert!(between(Some(""), None) > String::new());
    }

    #[test]
    fn keys_stay_short_enough_to_read() {
        let keys = spread(1000);
        assert!(keys.iter().all(|k| k.len() <= 2), "{}", keys[0]);
    }
}
