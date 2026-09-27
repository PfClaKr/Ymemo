//! Photos on a sticky: laying them out, picking, dropping and saving them, and the
//! callbacks that move them about.

use slint::ComponentHandle;
use std::cell::RefCell;
use std::rc::Rc;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use ymemo_core::diag;
use ymemo_core::vault::{RemovedPhoto, Vault};
use ymemo_i18n::t;

use crate::state::{touch, Ctx, APP};
use crate::{PhotoRow, StickyWindow};

use super::{BODY_FONT_PX, save_memo, set_body_text, sticky_text};

/// The photos of one memo, split by how they sit: the ones lying on the writing first, then
/// the ones with a band of their own under it. Two models because the two are drawn in
/// different places, and Slint's `for` cannot skip a row — see `sticky.slint`.
///
/// Photos are ciphertext inside the vault, so they are decrypted and decoded **in memory** —
/// no plaintext ever reaches a temp file. A photo that has not synced yet, or that cannot be
/// decoded, is marked `missing` so the UI can say so; an empty gap would read as data loss.
pub(crate) fn split_photo_rows(v: &Vault, memo_id: &str) -> PhotoModels {
    let list = match v.store().attachments_of(memo_id) {
        Ok(l) => l,
        Err(e) => {
            diag!("could not read the attachments: {e}");
            return PhotoModels::default();
        }
    };
    note_shows(memo_id, list.iter().map(|a| a.hash.clone()).collect());
    // The writing, for the photos standing in it: each one is drawn at the height of the
    // lines above it, and only the toolkit can measure that, so the words go with the row.
    let body = v.store().get(memo_id).ok().flatten().map(|m| m.body).unwrap_or_default();

    let mut float = Vec::new();
    let mut flow = Vec::new();
    let mut in_writing = Vec::new();
    for a in list {
        match a.mode() {
            ymemo_core::PhotoMode::Flow => flow.push(a),
            ymemo_core::PhotoMode::Inline => in_writing.push(a),
            ymemo_core::PhotoMode::Float => float.push(a),
        }
    }
    PhotoModels {
        float: rows_of(v, float, ""),
        flow: rows_of(v, flow, ""),
        in_writing: in_writing
            .into_iter()
            .map(|a| {
                let prefix = lines_before(&body, a.anchor_line.max(0) as usize).to_string();
                rows_of(v, vec![a], &prefix).remove(0)
            })
            .collect(),
    }
}

/// The three places a photo can be, each ready to hand to the window.
#[derive(Default)]
pub(crate) struct PhotoModels {
    /// Lying on the writing.
    pub float: Vec<PhotoRow>,
    /// In a band under it.
    pub flow: Vec<PhotoRow>,
    /// Standing in the writing itself.
    pub in_writing: Vec<PhotoRow>,
}

/// The first `n` lines of `body`, with no trailing newline.
///
/// This is what gets measured to find where a photo in the writing sits, so it has to be
/// exactly the text above it and nothing more — a stray newline here is a line of daylight
/// between the picture and the words it was put after.
pub(crate) fn lines_before(body: &str, n: usize) -> &str {
    if n == 0 {
        return "";
    }
    let mut seen = 0;
    for (i, b) in body.bytes().enumerate() {
        if b == b'\n' {
            seen += 1;
            if seen == n {
                return &body[..i];
            }
        }
    }
    body
}

pub(super) fn rows_of(v: &Vault, list: Vec<ymemo_core::Attachment>, prefix: &str) -> Vec<PhotoRow> {
    list.into_iter()
        .map(|a| {
            let (w, h) = a.display_size(BODY_FONT_PX);
            let image = photo_image(v, &a.hash);
            PhotoRow {
                id: a.id.into(),
                missing: image.is_none(),
                image: image.unwrap_or_default(),
                width_px: w as f32,
                height_px: h as f32,
                // A fraction, not a pixel offset: the note is whatever size the window is
                // right now, and Slint reapplies these on every resize without asking again.
                x_frac: ymemo_core::clamp_permille(a.x_permille) as f32 / 1000.0,
                y_frac: ymemo_core::clamp_permille(a.y_permille) as f32 / 1000.0,
                prefix: prefix.into(),
            }
        })
        .collect()
}

thread_local! {
    static PHOTOS: RefCell<PhotoCache> = RefCell::new(PhotoCache::default());
}

/// Decoded photos, kept while an open note shows them. See [`photo_image`].
#[derive(Default)]
struct PhotoCache {
    /// Decoded image by content hash.
    images: HashMap<String, slint::Image>,
    /// The hashes each open note is showing, by memo id — what keeps an image alive.
    shown: HashMap<String, Vec<String>>,
}

impl PhotoCache {
    fn drop_unshown(&mut self) {
        let live: HashSet<&String> = self.shown.values().flatten().collect();
        self.images.retain(|hash, _| live.contains(hash));
    }
}

/// A photo ready to draw, decrypted and decoded at most once while any open note shows it.
///
/// A note's photo rows are rebuilt routinely: every merge that brings anything in, every save
/// of a note with a photo standing in its writing. Each rebuild decrypted and decoded every
/// picture again — 370-490 ms of the UI thread per merge with four notes holding a phone-sized
/// photo, measured, whatever the merge was about. A blob never changes under its hash, so the
/// decoded image can be kept, and `slint::Image` shares its pixels: one the window is drawing
/// costs nothing more for being cached.
fn photo_image(v: &Vault, hash: &str) -> Option<slint::Image> {
    if let Some(image) = PHOTOS.with(|c| c.borrow().images.get(hash).cloned()) {
        return Some(image);
    }
    let image = v
        .has_blob(hash)
        .then(|| v.attachment_bytes(hash).ok())
        .flatten()
        .and_then(|bytes| decode_image(&bytes))?;
    PHOTOS.with(|c| c.borrow_mut().images.insert(hash.to_string(), image.clone()));
    Some(image)
}

/// Records which photos a note is about to show, and lets go of any no open note shows.
fn note_shows(memo_id: &str, hashes: Vec<String>) {
    PHOTOS.with(|c| {
        let mut c = c.borrow_mut();
        c.shown.insert(memo_id.to_string(), hashes);
        c.drop_unshown();
    });
}

/// A note closed: its photos are let go unless another open note shows them too. A full-size
/// decode is tens of megabytes, so the cache must not outlive the notes that asked for it.
pub(crate) fn forget_photos_of(memo_id: &str) {
    PHOTOS.with(|c| {
        let mut c = c.borrow_mut();
        c.shown.remove(memo_id);
        c.drop_unshown();
    });
}

/// Locked: every decoded photo goes. They are the plaintext of the vault's pictures, and a
/// locked vault leaves none of its contents behind.
pub(crate) fn forget_all_photos() {
    PHOTOS.with(|c| *c.borrow_mut() = PhotoCache::default());
}

/// Photo bytes to an RGBA8 Slint image; `None` for an unsupported format.
pub(super) fn decode_image(bytes: &[u8]) -> Option<slint::Image> {
    let decoded = image::load_from_memory(bytes).ok()?.into_rgba8();
    let (w, h) = decoded.dimensions();
    let buffer =
        slint::SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(decoded.as_raw(), w, h);
    Some(slint::Image::from_rgba8(buffer))
}

/// Refills an open sticky's photo lists after an add, a resize, a mode change or a merge.
pub(crate) fn refresh_photos(ctx: &Ctx, memo_id: &str) {
    let rows = {
        let Some(guard) = ctx.vault_ref() else { return };
        let v = &*guard;
        split_photo_rows(v, memo_id)
    };
    if let Some(entry) = ctx.stickies.borrow().get(memo_id) {
        set_photo_models(&entry.window, rows);
    }
}

/// Hands both photo models to a sticky window; the two always change together.
pub(crate) fn set_photo_models(window: &StickyWindow, models: PhotoModels) {
    window.set_photos(slint::ModelRc::new(slint::VecModel::from(models.float)));
    window.set_flow_photos(slint::ModelRc::new(slint::VecModel::from(models.flow)));
    window.set_inline_photos(slint::ModelRc::new(slint::VecModel::from(models.in_writing)));
}

/// A photo chosen by the worker thread. Only `Send` values, since it crosses to the event loop.
pub(super) struct PhotoPick {
    bytes: Vec<u8>,
    name: String,
    mime: &'static str,
    width: i64,
    height: i64,
}

/// Opens the file dialog on a worker thread and sends only the chosen photo back.
///
/// `rfd`'s synchronous API blocks waiting for the portal's answer on Linux (xdg-portal), so
/// calling it from the UI thread would freeze the event loop for as long as the dialog is
/// open — every other sticky, the merge timer and the idle timer with it. Picking and
/// decoding therefore happen on the worker and only the result comes back. `Ctx` is an `Rc`
/// and cannot cross threads, so the other side recovers it through `APP`.
///
/// `picking` keeps a second dialog from opening: the UI used to be frozen, so the attach
/// button could not be pressed twice; now it can.
pub(super) fn spawn_photo_picker(memo_id: String, title: String, picking: Arc<AtomicBool>) {
    if picking.swap(true, Ordering::SeqCst) {
        return; // a dialog is already open
    }
    std::thread::spawn(move || {
        let pick = pick_photo(&title);
        let _ = slint::invoke_from_event_loop(move || {
            picking.store(false, Ordering::SeqCst);
            if let Some(pick) = pick {
                attach_photo(&memo_id, pick);
            }
        });
    });
}

/// (Worker thread) Picks a photo and reads it; `None` on cancel or a read error.
pub(super) fn pick_photo(title: &str) -> Option<PhotoPick> {
    let path = rfd::FileDialog::new()
        .add_filter("image", &["png", "jpg", "jpeg"])
        .set_title(title)
        .pick_file()?; // cancelled
    read_photo(&path)
}

/// The picture types a note takes, whether it arrives through the dialog or off a drag.
///
/// The dialog filters by these and a drop is checked against them, so what a note can hold
/// is decided in one place rather than two that can drift.
pub(super) const PHOTO_EXTENSIONS: [&str; 3] = ["png", "jpg", "jpeg"];

/// Whether a dropped file looks like a picture this app can read.
///
/// By name, not by content: the check happens the moment the file is dragged over the note,
/// to say whether it will be taken, and opening every file the pointer passes over to find
/// out would be both slow and a surprise.
pub(super) fn looks_like_a_photo(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .is_some_and(|e| PHOTO_EXTENSIONS.contains(&e.as_str()))
}

/// (Worker thread) Reads a picture off disk and measures it; `None` if it cannot be read.
pub(super) fn read_photo(path: &Path) -> Option<PhotoPick> {
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) => {
            diag!("could not read the photo: {e}");
            return None;
        }
    };
    // Measure the original size here; the core has no decoder.
    let (width, height) = image::load_from_memory(&bytes)
        .map(|img| (img.width() as i64, img.height() as i64))
        .unwrap_or((0, 0));
    let name = path.file_name().unwrap_or_default().to_string_lossy().to_string();
    let mime = match path.extension().and_then(|e| e.to_str()) {
        Some("png") => "image/png",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        _ => "",
    };
    Some(PhotoPick { bytes, name, mime, width, height })
}

/// Takes a file dragged onto a note, off the event loop.
///
/// Reading and decoding happen on a worker for the same reason the dialog does: a photo off a
/// phone is tens of megabytes, and `image::load_from_memory` on the UI thread is a freeze of
/// every window at once. Only the decoded result comes back.
pub(super) fn accept_dropped_photo(memo_id: String, path: PathBuf) {
    if !looks_like_a_photo(&path) {
        return; // a note holds pictures; anything else is quietly not for us
    }
    std::thread::spawn(move || {
        let Some(pick) = read_photo(&path) else { return };
        let _ = slint::invoke_from_event_loop(move || attach_photo(&memo_id, pick));
    });
}

/// Writes a photo out to a file the user picks, on a worker thread.
///
/// The bytes are read here, on the event loop, and only they cross to the worker: the vault
/// is an `Rc` and cannot. The dialog itself blocks, for the same reason `spawn_photo_picker`
/// exists — see its comment.
///
/// What is written is the file as it was attached, not the size it happens to be drawn at.
pub(super) fn save_photo(ctx: &Ctx, photo_id: &str, title: String, saving: Arc<AtomicBool>) {
    if saving.swap(true, Ordering::SeqCst) {
        return; // a dialog is already open
    }
    let picked = {
        ctx.vault_ref().and_then(|v| match v.store().get_attachment(photo_id) {
            Ok(Some(a)) => match v.attachment_bytes(&a.hash) {
                Ok(bytes) => Some((a.name, bytes)),
                // Not on this device yet: there is nothing to write.
                Err(e) => {
                    diag!("could not read the photo to save it: {e}");
                    None
                }
            },
            _ => None,
        })
    };
    let Some((name, bytes)) = picked else {
        saving.store(false, Ordering::SeqCst);
        return;
    };
    std::thread::spawn(move || {
        let chosen = rfd::FileDialog::new()
            .set_title(&title)
            .set_file_name(if name.is_empty() { "photo.png" } else { &name })
            .save_file();
        if let Some(path) = chosen {
            if let Err(e) = std::fs::write(&path, &bytes) {
                diag!("could not save the photo: {e}");
            }
        }
        let _ = slint::invoke_from_event_loop(move || saving.store(false, Ordering::SeqCst));
    });
}

/// (Event loop) Attaches the chosen photo to the vault and redraws the sticky.
///
/// The idle auto-lock may have fired while the dialog was open; without a vault the photo is
/// dropped silently, rather than written after locking.
pub(super) fn attach_photo(memo_id: &str, pick: PhotoPick) {
    let ctx = APP.with(|a| a.borrow().as_ref().map(|app| app.ctx.clone()));
    let Some(ctx) = ctx else { return };
    {
        let Some(mut guard) = ctx.vault_mut() else { return };
        let v = &mut *guard;
        if let Err(e) = v.attach(memo_id, &pick.bytes, &pick.name, pick.mime, pick.width, pick.height)
        {
            diag!("could not attach the photo: {e}");
            return;
        }
    }
    refresh_photos(&ctx, memo_id);
}

/// Which line a picture goes on when the caret is at byte offset `at`.
///
/// Where the caret is, meaning: **on its own line, under whatever the caret sits after.** With
/// the caret at the end of a line that is the line below — somebody who has just written a
/// line and reached for a picture wants it under what they wrote, not above it. With the
/// caret at the start of one — which is where pressing return leaves it — that is the line
/// itself, so the empty line just made is the room rather than a blank line above the room.
/// Both are how people arrive here and the two rules disagree, which is why this is not
/// simply a count of line breaks.
pub(super) fn line_for_caret(body: &str, at: usize) -> i64 {
    let at = at.min(body.len());
    let bytes = body.as_bytes();
    let breaks = bytes[..at].iter().filter(|b| **b == b'\n').count() as i64;
    let at_line_start = at == 0 || bytes[at - 1] == b'\n';
    if at_line_start {
        breaks
    } else {
        breaks + 1
    }
}

/// How many blank lines a photo needs to stand in, at this note's line height.
///
/// Rounded **up**, so the room is never shorter than the picture: a line of writing peeping
/// out from under a photo reads as a bug, where a sliver of blank paper reads as spacing.
pub(super) fn rows_for_photo(v: &Vault, photo_id: &str, line_height: f32) -> usize {
    let Ok(Some(a)) = v.store().get_attachment(photo_id) else { return 1 };
    let (_, h) = a.display_size(BODY_FONT_PX);
    if line_height <= 0.0 {
        return 1;
    }
    ((h as f32 / line_height).ceil() as usize).max(1)
}

/// Wires the photo callbacks of one sticky.
pub(super) fn wire(ctx: &Ctx, window: &StickyWindow, id: &str) {
    wire_attach(ctx, window, id);
    wire_place(ctx, window, id);
    wire_remove(ctx, window, id);
    wire_save(ctx, window);
    wire_flow(ctx, window, id);
}

/// Attach: pick a photo from the file dialog, which runs on a worker thread.
fn wire_attach(ctx: &Ctx, window: &StickyWindow, id: &str) {
    let ctx = ctx.clone();
    let id = id.to_string();
    let picking = Arc::new(AtomicBool::new(false));
    window.on_add_photo(move || {
        touch(&ctx);
        spawn_photo_picker(id.clone(), t!("ui.sticky_pick_photo"), picking.clone());
    });
}

/// Move and resize in one write, once the pointer is released. The width crosses as
/// pixels and is stored in em, so mobile shows the same proportion of a line of text; the
/// position crosses as a fraction and is stored as one, so it lands on the same part of a
/// phone screen as of this sticky.
fn wire_place(ctx: &Ctx, window: &StickyWindow, id: &str) {
    let ctx = ctx.clone();
    let id = id.to_string();
    window.on_place_photo(move |photo_id, x_frac, y_frac, width_px| {
        touch(&ctx);
        {
            let Some(mut guard) = ctx.vault_mut() else { return };
            let v = &mut *guard;
            let em_milli = (width_px as f64 / BODY_FONT_PX * 1000.0).round() as i64;
            if let Err(e) = v.set_attachment_layout(
                photo_id.as_str(),
                (x_frac as f64 * 1000.0).round() as i64,
                (y_frac as f64 * 1000.0).round() as i64,
                em_milli,
            ) {
                diag!("could not move the photo: {e}");
            }
        }
        refresh_photos(&ctx, &id);
    });
}

/// Detach a photo. The blob file stays — another device may still be showing it.
fn wire_remove(ctx: &Ctx, window: &StickyWindow, id: &str) {
    // The one removal this note can still take back, and the timer that withdraws the offer.
    // Per note, like the bar that shows it; a second removal replaces the first.
    let last: Rc<RefCell<Option<RemovedPhoto>>> = Rc::new(RefCell::new(None));
    let expiry = Rc::new(slint::Timer::default());
    {
        let ctx = ctx.clone();
        let id = id.to_string();
        let weak = window.as_weak();
        let last = last.clone();
        let expiry = expiry.clone();
        window.on_remove_photo(move |photo_id| {
            touch(&ctx);
            let Some(w) = weak.upgrade() else { return };
            // A photo in the writing takes its room with it, and the room is closed in what
            // is **stored**: save what is on screen first, as moving one in or out does.
            save_memo(&ctx, &id, w.get_memo_text().as_str());
            let removed = {
                let Some(mut guard) = ctx.vault_mut() else { return };
                match guard.remove_attachment(photo_id.as_str()) {
                    Ok(removed) => removed,
                    Err(e) => {
                        diag!("could not remove the photo: {e}");
                        crate::list::report_write_failure(&e);
                        None
                    }
                }
            };
            reload_body(&ctx, &w, &id);
            refresh_photos(&ctx, &id);
            let Some(removed) = removed else { return };
            *last.borrow_mut() = Some(removed);
            w.set_photo_undo(true);
            let weak = weak.clone();
            let last = last.clone();
            expiry.start(slint::TimerMode::SingleShot, PHOTO_UNDO_FOR, move || {
                last.borrow_mut().take();
                if let Some(w) = weak.upgrade() {
                    w.set_photo_undo(false);
                }
            });
        });
    }
    let ctx = ctx.clone();
    let id = id.to_string();
    let weak = window.as_weak();
    window.on_undo_photo(move || {
        touch(&ctx);
        let Some(w) = weak.upgrade() else { return };
        w.set_photo_undo(false);
        expiry.stop();
        let Some(removed) = last.borrow_mut().take() else { return };
        save_memo(&ctx, &id, w.get_memo_text().as_str());
        {
            let Some(mut guard) = ctx.vault_mut() else { return };
            if let Err(e) = guard.restore_attachment(&removed) {
                diag!("could not put the photo back: {e}");
                crate::list::report_write_failure(&e);
            }
        }
        reload_body(&ctx, &w, &id);
        refresh_photos(&ctx, &id);
    });
}

/// How long a removed photo is offered back. Shorter than the list's undo: this bar sits over
/// the bottom of the note, where the writing is.
const PHOTO_UNDO_FOR: std::time::Duration = std::time::Duration::from_secs(10);

/// Puts the stored body back into the note after the core changed it underneath — a room
/// opened or closed. `refresh_photos` only carries the pictures.
fn reload_body(ctx: &Ctx, w: &StickyWindow, id: &str) {
    let memo = ctx.vault_ref().and_then(|v| v.store().get(id).ok().flatten());
    if let Some(memo) = memo {
        set_body_text(w, &sticky_text(&memo));
    }
}

/// Write a photo out to a file the user picks.
fn wire_save(ctx: &Ctx, window: &StickyWindow) {
    let ctx = ctx.clone();
    let saving = Arc::new(AtomicBool::new(false));
    window.on_save_photo(move |photo_id| {
        touch(&ctx);
        save_photo(&ctx, photo_id.as_str(), t!("ui.sticky_photo_save"), saving.clone());
    });
}

/// Move a photo between lying on the writing and standing **in** it.
///
/// In goes to the caret — the one place the user has actually pointed at — and opens as
/// many blank lines as the picture is tall. Out closes them again. Both halves are one
/// call into the vault so the memo and the photo can never disagree about where the room
/// is; see `place_attachment_in_writing`.
fn wire_flow(ctx: &Ctx, window: &StickyWindow, id: &str) {
    let ctx = ctx.clone();
    let id = id.to_string();
    let weak = window.as_weak();
    window.on_set_photo_flow(move |photo_id, into_writing| {
        touch(&ctx);
        let Some(w) = weak.upgrade() else { return };
        // What is on the note is newer than what is stored until the debounce fires, and
        // the room is opened in what is **stored** — so without this the gap is cut into
        // an older version of the writing and the pending save then closes it again,
        // leaving the photo anchored to a line nobody made room for. A save with nothing
        // to save costs nothing.
        save_memo(&ctx, &id, w.get_memo_text().as_str());
        {
            let Some(mut guard) = ctx.vault_mut() else { return };
            let v = &mut *guard;
            let res = if into_writing {
                let body = v.store().get(&id).ok().flatten().map(|m| m.body).unwrap_or_default();
                let after_line = line_for_caret(&body, w.get_caret_byte().max(0) as usize);
                let rows = rows_for_photo(v, photo_id.as_str(), w.get_body_line_height());
                v.place_attachment_in_writing(photo_id.as_str(), after_line, rows)
            } else {
                v.take_attachment_out_of_writing(
                    photo_id.as_str(),
                    ymemo_core::PhotoMode::Float,
                )
            };
            if let Err(e) = res {
                diag!("could not change how the photo sits: {e}");
                crate::list::report_write_failure(&e);
            }
        }
        // The body changed under the note, so the field has to be told; `refresh_photos`
        // only carries the pictures.
        let memo = ctx.vault_ref().and_then(|v| v.store().get(&id).ok().flatten());
        if let Some(memo) = memo {
            set_body_text(&w, &sticky_text(&memo));
        }
        refresh_photos(&ctx, &id);
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    /// Where a picture lands for a given caret: under what the caret sits after.
    #[test]
    fn a_picture_goes_on_the_line_under_what_the_caret_sits_after() {
        let body = "one\ntwo\nthree";
        // Caret at the end of a line: the picture goes below that line.
        assert_eq!(line_for_caret(body, 3), 1, "end of \"one\"");
        assert_eq!(line_for_caret(body, 7), 2, "end of \"two\"");
        // Mid-line counts as being on that line, so still below it.
        assert_eq!(line_for_caret(body, 5), 2, "inside \"two\"");
        // Caret at the start of a line — where return leaves it — is that line itself.
        assert_eq!(line_for_caret(body, 0), 0, "very start");
        assert_eq!(line_for_caret(body, 4), 1, "start of \"two\"");
        assert_eq!(line_for_caret(body, 8), 2, "start of \"three\"");
        // A caret past the end, or on a body that has never been in, lands at the end.
        assert_eq!(line_for_caret(body, 999), 3);
        assert_eq!(line_for_caret("", 0), 0);
        // After a return at the end of the note: the empty line made is the room.
        assert_eq!(line_for_caret("one\n", 4), 1);
    }
}
