//! Photos on a memo: attaching them, laying them out, and the room one takes in the writing.

use anyhow::{bail, Result};
use automerge::{transaction::Transactable, ObjType, ReadDoc, Value};
use ymemo_i18n::t;

use crate::{clamp_permille, clamp_width_em_milli, Attachment};

use super::Vault;
use super::doc::{put_i64_if_changed, put_str_if_changed};

impl Vault {
    /// Attaches a photo: bytes go to the blob store, only the record is synced.
    ///
    /// The UI passes `width_px`/`height_px` so the core needs no image decoder — both UIs
    /// already have one. Pass 0 when unknown; the aspect ratio then falls back to 1:1.
    pub fn attach(
        &mut self,
        memo_id: &str,
        data: &[u8],
        name: &str,
        mime: &str,
        width_px: i64,
        height_px: i64,
    ) -> Result<Attachment> {
        let hash = self.blobs.put(data)?;
        let mut a = Attachment::new(memo_id, hash);
        a.name = name.to_string();
        a.mime = mime.to_string();
        a.width_px = width_px;
        a.height_px = height_px;
        // Offset from the photos already on this memo, so the new one is not dropped exactly
        // on top of the last and left unreachable.
        let placed = self.store.attachments_of(memo_id).map(|l| l.len()).unwrap_or(0);
        let (x, y) = crate::cascade_permille(placed);
        a.x_permille = x;
        a.y_permille = y;
        self.upsert_attachment(&a)?;
        Ok(a)
    }

    /// Inserts or updates an attachment; display-size changes go through here too.
    pub fn upsert_attachment(&mut self, a: &Attachment) -> Result<()> {
        let attachments = self.attachments_obj()?;
        let obj = match self.doc.get(&attachments, &a.id)? {
            Some((Value::Object(ObjType::Map), id)) => id,
            _ => self.doc.put_object(&attachments, &a.id, ObjType::Map)?,
        };
        put_str_if_changed(&mut self.doc, &obj, "memo_id", &a.memo_id)?;
        put_str_if_changed(&mut self.doc, &obj, "hash", &a.hash)?;
        put_str_if_changed(&mut self.doc, &obj, "name", &a.name)?;
        put_str_if_changed(&mut self.doc, &obj, "mime", &a.mime)?;
        put_i64_if_changed(&mut self.doc, &obj, "width_px", a.width_px)?;
        put_i64_if_changed(&mut self.doc, &obj, "height_px", a.height_px)?;
        put_i64_if_changed(
            &mut self.doc,
            &obj,
            "width_em_milli",
            clamp_width_em_milli(a.width_em_milli),
        )?;
        put_i64_if_changed(&mut self.doc, &obj, "x_permille", clamp_permille(a.x_permille))?;
        put_i64_if_changed(&mut self.doc, &obj, "y_permille", clamp_permille(a.y_permille))?;
        put_str_if_changed(&mut self.doc, &obj, "mode", a.mode().as_stored())?;
        put_i64_if_changed(&mut self.doc, &obj, "anchor_line", a.anchor_line.max(0))?;
        put_i64_if_changed(&mut self.doc, &obj, "created_at", a.created_at)?;

        self.append_local_change()?;
        self.store.upsert_attachment(a)
    }

    /// Sets position and size together — one write, because dragging a photo's corner moves
    /// and resizes it at once and two writes would leave two revisions in the history.
    pub fn set_attachment_layout(
        &mut self,
        id: &str,
        x_permille: i64,
        y_permille: i64,
        width_em_milli: i64,
    ) -> Result<()> {
        let Some(mut a) = self.store.get_attachment(id)? else {
            bail!(t!("core.attachment_not_found", id = id));
        };
        a.x_permille = clamp_permille(x_permille);
        a.y_permille = clamp_permille(y_permille);
        a.width_em_milli = clamp_width_em_milli(width_em_milli);
        self.upsert_attachment(&a)
    }

    /// Puts a photo **into** the writing after `after_line` lines of it, opening `rows` blank
    /// lines to stand in.
    ///
    /// The room really is blank lines in the memo. A note is one text box and a text box
    /// cannot have a hole in it — but it can have empty lines, and they behave the way the
    /// user already expects everything in a note to behave: write a paragraph above and the
    /// gap moves down with the rest, select across it, delete it if the picture is not wanted
    /// there any more.
    ///
    /// Both halves happen together, here rather than in a UI, so the phone and the desktop
    /// cannot disagree about what "in the writing" means — and so a photo can never end up
    /// pointing at a line of a body that was never given room for it.
    pub fn place_attachment_in_writing(
        &mut self,
        id: &str,
        after_line: i64,
        rows: usize,
    ) -> Result<()> {
        let Some(mut a) = self.store.get_attachment(id)? else {
            bail!(t!("core.attachment_not_found", id = id));
        };
        let Some(mut memo) = self.store.get(&a.memo_id)? else {
            bail!(t!("core.memo_not_found", id = a.memo_id));
        };
        // Moving one that is already in the writing: take its old room back first, or every
        // move leaves a hole behind it.
        if a.mode() == crate::PhotoMode::Inline {
            memo.body = close_gap(&memo.body, a.anchor_line.max(0) as usize);
        }
        let after_line = after_line.clamp(0, line_count(&memo.body) as i64);
        memo.body = open_gap(&memo.body, after_line as usize, rows);
        a.mode = crate::PHOTO_MODE_INLINE.to_string();
        a.anchor_line = after_line;
        self.upsert_attachment(&a)?;
        // After the attachment, so the re-anchoring that `upsert` does walks over a photo
        // that already knows where it is going.
        self.upsert(&memo)
    }

    /// Takes a photo back out of the writing and closes the room it was standing in.
    pub fn take_attachment_out_of_writing(
        &mut self,
        id: &str,
        mode: crate::PhotoMode,
    ) -> Result<()> {
        let Some(mut a) = self.store.get_attachment(id)? else {
            bail!(t!("core.attachment_not_found", id = id));
        };
        if a.mode() == crate::PhotoMode::Inline {
            if let Some(mut memo) = self.store.get(&a.memo_id)? {
                memo.body = close_gap(&memo.body, a.anchor_line.max(0) as usize);
                self.upsert(&memo)?;
            }
        }
        a.mode = mode.as_stored().to_string();
        self.upsert_attachment(&a)
    }

    /// Keeps every photo in a memo's writing pointing at the same words after an edit.
    ///
    /// A photo in the writing is drawn at the height of the lines above it, so if those lines
    /// change in number and the anchor does not, the picture and the room left for it drift
    /// apart — type a line at the top and the gap moves down while the photo stays. The edit
    /// is compared end to end, the way [`splice_changed_span`] compares one: what is identical
    /// at the front is untouched, so only a change that starts **above** a photo can move it,
    /// and then only by however many lines it added or took away.
    pub(super) fn reanchor_attachments(&mut self, memo_id: &str, old: &str, new: &str) -> Result<()> {
        if old == new {
            return Ok(());
        }
        let head = old
            .as_bytes()
            .iter()
            .zip(new.as_bytes())
            .take_while(|(a, b)| a == b)
            .count();
        // The line the change begins **on**, counted in line breaks rather than in lines:
        // "one\ntw" is two lines but the change is on line 1, and treating it as line 2 left
        // a photo anchored there standing still while a line was opened above it.
        //
        // Counted over the bytes, never over a slice: `head` is a byte count off a plain
        // comparison and lands in the middle of a Korean syllable as easily as between two,
        // and `&old[..head]` would panic there.
        let first_touched =
            old.as_bytes()[..head].iter().filter(|b| **b == b'\n').count() as i64;
        let moved = line_count(new) as i64 - line_count(old) as i64;
        if moved == 0 {
            return Ok(());
        }
        for mut a in self.store.attachments_of(memo_id)? {
            if a.mode() != crate::PhotoMode::Inline || a.anchor_line <= first_touched {
                continue;
            }
            a.anchor_line = (a.anchor_line + moved).max(first_touched);
            self.upsert_attachment(&a)?;
        }
        Ok(())
    }

    /// Detaches a photo. **The blob file stays** — no GC, other devices may still show it.
    pub fn detach(&mut self, id: &str) -> Result<()> {
        let attachments = self.attachments_obj()?;
        if self.doc.get(&attachments, id)?.is_some() {
            self.doc.delete(&attachments, id)?;
            self.append_local_change()?;
        }
        self.store.delete_attachment(id)
    }

    /// Photo bytes. Errors while the blob has not synced yet; the UI shows a placeholder.
    pub fn attachment_bytes(&self, hash: &str) -> Result<Vec<u8>> {
        self.blobs.get(hash)
    }

    /// Whether the photo has arrived on this device.
    pub fn has_blob(&self, hash: &str) -> bool {
        self.blobs.has(hash)
    }
}

/// How many lines `body` holds. An empty body is no lines; a trailing newline does not open
/// one, so this counts the same way a person does.
pub(super) fn line_count(body: &str) -> usize {
    if body.is_empty() {
        return 0;
    }
    body.lines().count()
}

/// The byte offset where line `n` begins, or the end of `body` when there are fewer.
pub(super) fn line_offset(body: &str, n: usize) -> usize {
    if n == 0 {
        return 0;
    }
    let mut seen = 0;
    for (i, b) in body.bytes().enumerate() {
        if b == b'\n' {
            seen += 1;
            if seen == n {
                return i + 1;
            }
        }
    }
    body.len()
}

/// Opens `rows` blank lines after `after_line` lines of `body`.
pub(super) fn open_gap(body: &str, after_line: usize, rows: usize) -> String {
    let at = line_offset(body, after_line);
    let mut out = String::with_capacity(body.len() + rows + 2);
    out.push_str(&body[..at]);
    // A gap at the very end of a note that does not end in one needs a newline of its own to
    // sit after, or the first blank line is really the end of the last line of writing.
    if at == body.len() && !body.is_empty() && !body.ends_with('\n') {
        out.push('\n');
    }
    for _ in 0..rows {
        out.push('\n');
    }
    out.push_str(&body[at..]);
    out
}

/// Closes the run of blank lines that begins after `after_line` lines of `body`.
///
/// Only the blank ones, and only the run that starts right there: if the user has written in
/// the room since, what they wrote stays and the picture simply has less of it.
pub(super) fn close_gap(body: &str, after_line: usize) -> String {
    let at = line_offset(body, after_line);
    let rest = &body[at..];
    let kept: Vec<&str> = rest.split('\n').collect();
    let blanks = kept.iter().take_while(|l| l.trim().is_empty()).count();
    // `split` on a string that ends in a newline leaves a trailing empty piece which is not a
    // line of its own; never eat that, or closing a gap at the end swallows the line break.
    let blanks = blanks.min(kept.len().saturating_sub(1));
    let mut out = String::with_capacity(body.len());
    out.push_str(&body[..at]);
    out.push_str(&kept[blanks..].join("\n"));
    out
}
