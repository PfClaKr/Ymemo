//! Reading and writing automerge fields: text that merges, strings that do not, and the
//! fallbacks for fields older changes never wrote.

use anyhow::{bail, Result};
use automerge::{transaction::Transactable, AutoCommit, ObjId, ObjType, ReadDoc, ScalarValue, Value};
use ymemo_i18n::t;

/// Writes a field the user **types** — a memo's title or body, a folder's name.
///
/// These are automerge `Text`, not the plain strings the rest of the document uses, and that
/// is the whole difference between two devices merging and one of them losing what somebody
/// wrote. A `put` of a string is last-write-wins: edit the same memo on two devices inside
/// the time it takes to sync and one version silently becomes the memo, with the other left
/// only in the change history. A `Text` merges the two edits instead.
///
/// It is not magic. The UI hands over the **whole** body on every save, so the change has to
/// be recovered by diffing (`update_text`) rather than captured as it was typed; two people
/// typing at the same spot get their letters interleaved. Nothing is lost, which is the part
/// that matters.
///
/// A field that is a plain string here was written by a build from before this, and is
/// replaced by a text object the first time it changes — which is also the one moment two
/// devices converting the same memo at once can still lose an edit, since replacing the key
/// is itself a `put`.
///
/// **Only the body.** The rule is not "anything the user types" — it is *prose that is added
/// to*. A title and a folder's name are short labels, replaced whole rather than extended, and
/// merging two of them is worse than losing one: renaming a folder to "Work" on one device and
/// "Home" on the other gives **"WHorkme"**, which is not a name anybody chose and which the
/// user now has to notice and repair. Measured. Last-write-wins leaves a name somebody meant,
/// and the one that lost is still in the change history. Colours and ids stay strings for the
/// same reason, only more obviously.
pub(super) fn put_text_if_changed(doc: &mut AutoCommit, obj: &ObjId, key: &str, val: &str) -> Result<()> {
    if let Some((Value::Object(ObjType::Text), text)) = doc.get(obj, key)? {
        let old = doc.text(&text)?;
        if old != val {
            splice_changed_span(doc, &text, &old, val)?;
        }
        return Ok(());
    }
    let text = doc.put_object(obj, key, ObjType::Text)?;
    doc.splice_text(&text, 0, 0, val)?;
    Ok(())
}

/// Writes the one stretch of the body that moved, found by what is identical at each end.
///
/// The obvious way to do this is automerge's `update_text`, and it is a trap on a long memo:
/// it grapheme-segments **both** copies into vectors and runs a Myers diff over the whole
/// thing, every save, however small the edit. Measured, 50 keystrokes at the end of a 200 KB
/// memo cost 1190 ms — 24 ms of the UI thread per keystroke, growing with the memo, which is
/// the stutter this app has spent a whole release chasing out. Comparing the ends instead is
/// a byte scan: the same 50 keystrokes cost under a millisecond and stop caring how long the
/// memo is.
///
/// It writes one replaced span where Myers might write two, which costs a little precision if
/// somebody edits two far-apart places between saves and a second device edits the text
/// between them at that exact moment. A save is one debounce of typing, so that stretch is
/// one place; paying 24 ms per keystroke against it is not a trade worth making.
///
/// Positions here are **characters**, not bytes — automerge indexes text by unicode code
/// point — so both ends are pulled back to a character boundary before anything is spliced.
/// A boundary in one string is a boundary in the other: the bytes at that offset are the same
/// byte, since that is what made it common.
pub(super) fn splice_changed_span(doc: &mut AutoCommit, text: &ObjId, old: &str, new: &str) -> Result<()> {
    let mut head = old
        .as_bytes()
        .iter()
        .zip(new.as_bytes())
        .take_while(|(a, b)| a == b)
        .count();
    while !old.is_char_boundary(head) {
        head -= 1;
    }

    let room = (old.len() - head).min(new.len() - head);
    let mut tail = old
        .bytes()
        .rev()
        .zip(new.bytes().rev())
        .take_while(|(a, b)| a == b)
        .count()
        .min(room);
    while !old.is_char_boundary(old.len() - tail) {
        tail -= 1;
    }

    let pos = old[..head].chars().count();
    let del = old[head..old.len() - tail].chars().count();
    doc.splice_text(text, pos, del as isize, &new[head..new.len() - tail])?;
    Ok(())
}

/// Reads a field written by [`put_text_if_changed`], or the plain string an older build left.
pub(super) fn get_text(doc: &AutoCommit, obj: &ObjId, key: &str) -> Result<String> {
    match doc.get(obj, key)? {
        Some((Value::Object(ObjType::Text), text)) => Ok(doc.text(&text)?),
        // Written before the change above; still perfectly readable.
        Some((Value::Scalar(s), _)) => match s.as_ref() {
            ScalarValue::Str(v) => Ok(v.to_string()),
            other => bail!(t!("core.field_not_string", key = key, found = format!("{other:?}"))),
        },
        _ => bail!(t!("core.field_missing", key = key)),
    }
}

/// [`get_text`] with a fallback, for a field an old change may not carry at all.
pub(super) fn get_text_or(doc: &AutoCommit, obj: &ObjId, key: &str, default: &str) -> String {
    get_text(doc, obj, key).unwrap_or_else(|_| default.to_string())
}

pub(super) fn put_str_if_changed(doc: &mut AutoCommit, obj: &ObjId, key: &str, val: &str) -> Result<()> {
    let same = matches!(
        doc.get(obj, key)?,
        Some((Value::Scalar(ref s), _)) if matches!(s.as_ref(), ScalarValue::Str(cur) if cur.as_str() == val)
    );
    if !same {
        doc.put(obj, key, val)?;
    }
    Ok(())
}

pub(super) fn put_i64_if_changed(doc: &mut AutoCommit, obj: &ObjId, key: &str, val: i64) -> Result<()> {
    let same = matches!(
        doc.get(obj, key)?,
        Some((Value::Scalar(ref s), _)) if matches!(s.as_ref(), ScalarValue::Int(cur) if *cur == val)
    );
    if !same {
        doc.put(obj, key, val)?;
    }
    Ok(())
}

/// Reads a string field, falling back to `default` when missing or of another type.
pub(super) fn get_str_or(doc: &AutoCommit, obj: &ObjId, key: &str, default: &str) -> String {
    match doc.get(obj, key) {
        Ok(Some((Value::Scalar(s), _))) => match s.as_ref() {
            ScalarValue::Str(v) => v.to_string(),
            _ => default.to_string(),
        },
        _ => default.to_string(),
    }
}

/// Reads an integer field, falling back to `default` when missing or of another type.
pub(super) fn get_i64_or(doc: &AutoCommit, obj: &ObjId, key: &str, default: i64) -> i64 {
    match doc.get(obj, key) {
        Ok(Some((Value::Scalar(s), _))) => match s.as_ref() {
            ScalarValue::Int(v) => *v,
            _ => default,
        },
        _ => default,
    }
}

pub(super) fn get_i64(doc: &AutoCommit, obj: &ObjId, key: &str) -> Result<i64> {
    match doc.get(obj, key)? {
        Some((Value::Scalar(s), _)) => match s.as_ref() {
            ScalarValue::Int(v) => Ok(*v),
            other => bail!(t!("core.field_not_int", key = key, found = format!("{other:?}"))),
        },
        _ => bail!(t!("core.field_missing", key = key)),
    }
}

/// `value`, or `fallback` when it is empty — a revision from before a field existed.
pub(super) fn non_empty(value: &str, fallback: &str) -> String {
    if value.is_empty() { fallback.to_string() } else { value.to_string() }
}
