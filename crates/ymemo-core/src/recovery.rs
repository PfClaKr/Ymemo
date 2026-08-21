//! Recovery code: the second way into a vault when the master password is gone.
//!
//! The vault's data key is stored twice in `vault.json` — wrapped under the password key
//! and, once a code is issued, under a key derived from this code (see [`crate::vault`]).
//! So a recovery code does not *reveal* the password; it unwraps the same data key and
//! lets the user set a new one.
//!
//! Format: 8 groups of 4 characters, `A1B2-C3D4-...`, drawn from a 32-character alphabet
//! without the pairs people confuse when copying by hand (I/1, L/1, O/0, U). 32 characters
//! at 5 bits each is 160 bits of entropy, so the code is a secret in its own right —
//! **anyone holding it can open the vault.**
//!
//! [`normalize`] is what the encoding hangs on: it is the only thing the derived key sees,
//! so dashes, spacing and case never matter, and swapping this module's alphabet for a
//! word list later would leave the rest of the vault untouched.

use anyhow::{bail, Result};
use rand::{rngs::OsRng, RngCore};
use ymemo_i18n::t;

/// Characters a code is built from. 32 of them, so a random byte maps without bias.
const ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
/// Characters per dash-separated group.
const GROUP: usize = 4;
/// Number of groups; `GROUPS * GROUP * 5` bits of entropy.
const GROUPS: usize = 8;
/// Total characters once the dashes are gone.
const LEN: usize = GROUP * GROUPS;

/// A fresh random code, formatted for display: `A1B2-C3D4-...`.
pub fn generate() -> String {
    let mut raw = [0u8; LEN];
    OsRng.fill_bytes(&mut raw);
    let chars: Vec<u8> = raw.iter().map(|b| ALPHABET[(b & 31) as usize]).collect();
    chars
        .chunks(GROUP)
        .map(|c| String::from_utf8_lossy(c).into_owned())
        .collect::<Vec<_>>()
        .join("-")
}

/// Canonical form of a typed code: uppercase, no separators, confusables folded in.
///
/// Everything that derives a key from a code goes through here, so `a1b2-c3d4`,
/// `A1B2 C3D4` and `A1B2C3D4` are one and the same secret.
pub fn normalize(input: &str) -> Result<String> {
    let mut out = String::with_capacity(LEN);
    for ch in input.chars() {
        if ch.is_whitespace() || ch == '-' || ch == '_' {
            continue;
        }
        // Fold what a person is likely to write instead of the real character.
        let c = match ch.to_ascii_uppercase() {
            'I' | 'L' => '1',
            'O' => '0',
            'U' => 'V',
            other => other,
        };
        if !ALPHABET.contains(&(c as u8)) {
            bail!(t!("core.bad_recovery_code"));
        }
        out.push(c);
    }
    if out.len() != LEN {
        bail!(t!("core.bad_recovery_code"));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_codes_are_well_formed_and_distinct() {
        let a = generate();
        let b = generate();
        assert_ne!(a, b, "two codes in a row must not repeat");
        assert_eq!(a.len(), LEN + GROUPS - 1, "8 groups of 4 plus 7 dashes");
        assert_eq!(a.matches('-').count(), GROUPS - 1);
        assert_eq!(normalize(&a).unwrap().len(), LEN);
    }

    #[test]
    fn normalize_ignores_formatting_and_folds_confusables() {
        let code = generate();
        let bare = normalize(&code).unwrap();
        assert_eq!(normalize(&code.to_lowercase()).unwrap(), bare);
        assert_eq!(normalize(&code.replace('-', " ")).unwrap(), bare);
        assert_eq!(normalize(&code.replace('-', "")).unwrap(), bare);
        // The characters the alphabet leaves out map onto the ones it keeps.
        assert_eq!(normalize("iiii-llll-oooo-uuuu-0000-1111-2222-3333").unwrap(),
                   "111111110000VVVV0000111122223333");
    }

    #[test]
    fn normalize_rejects_wrong_length_or_characters() {
        assert!(normalize("").is_err());
        assert!(normalize("A1B2-C3D4").is_err(), "too short");
        assert!(normalize(&format!("{}-EXTR", generate())).is_err(), "too long");
        assert!(normalize("A1B2-C3D4-E5F6-G7H8-J9K0-MNPQ-RSTV-WXY!").is_err());
    }
}
