//! Sticky windows: creating them, saving edits, closing them, and magnetic snapping.
//!
//! One window per memo, with the body doubling as the editor (debounced autosave). Snapping
//! only works where window coordinates can be read and written (X11, Windows); elsewhere
//! (native Wayland) it silently does nothing.

use ymemo_core::diag;
use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use i_slint_backend_winit::winit::dpi::PhysicalPosition;
use i_slint_backend_winit::WinitWindowAccessor;
use slint::{ComponentHandle, LogicalSize, SharedString, TimerMode};
use ymemo_core::vault::Vault;
use ymemo_core::{now_millis, Memo};
use ymemo_i18n::t;

use crate::list::refresh_list;
use crate::settings::POS_UNKNOWN;
use crate::state::{touch, Ctx, StickyEntry, Stickies, APP};
use crate::window::{present, raise, restore_geometry, skip_taskbar};
use crate::{apply_strings, PhotoRow, StickyWindow, Strings};

/// Body font size (logical px) that photo sizes in em are measured against.
/// **Must match the body `font-size` in `ui/sticky.slint`.**
const BODY_FONT_PX: f64 = 13.0;

/// Debounce between an edit and the autosave.
pub(crate) const SAVE_DEBOUNCE: Duration = Duration::from_millis(800);
/// Title bar height, i.e. the collapsed window height; must match app.slint.
pub(crate) const BAR_HEIGHT: f32 = 24.0;
/// Snap distance in logical px; within it, edges stick to the screen or another sticky.
pub(crate) const SNAP_DIST: f32 = 12.0;
/// How often sticky positions are polled for snapping.
pub(crate) const SNAP_INTERVAL: Duration = Duration::from_millis(90);
/// How often where the windows are is written to `settings.json`.
///
/// Deliberately far slower than the snap poll: this is a file write, and a drag would
/// otherwise produce one per frame. Two seconds is under the time it takes to move a note and
/// reach for something else, and a close records its own window immediately anyway.
pub(crate) const GEOMETRY_INTERVAL: Duration = Duration::from_secs(2);
/// The size a sticky opens at (logical px); **must match the preferred size in
/// `ui/sticky.slint`**, which is what the window is actually given.
pub(crate) const DEFAULT_SIZE: (f32, f32) = (200.0, 120.0);
/// Height of the colour and opacity panel; **must match the one in `ui/sticky.slint`**.
/// A sticky opens small enough that the panel would take most of the note, so the window
/// makes room for it and gives it back on close.
const PALETTE_HEIGHT: f32 = 62.0;

/// Body text for the window; an older memo with only a title promotes it to the body.
pub(crate) fn sticky_text(memo: &Memo) -> String {
    if memo.body.is_empty() && !memo.title.is_empty() {
        memo.title.clone()
    } else {
        memo.body.clone()
    }
}

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
fn title_line(text: &str) -> &str {
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
fn strip_heading(line: &str) -> &str {
    let hashes = line.bytes().take_while(|b| *b == b'#').count();
    if hashes == 0 || hashes > 6 || line.as_bytes().get(hashes) != Some(&b' ') {
        return line;
    }
    line[hashes + 1..].trim_start()
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

/// Saves an edited body to the vault, deriving the title from its first line.
///
/// Returns whether the vault now holds `text`. **False means the writing is only in the
/// window**, so the caller must leave the sticky marked dirty: the merge timer skips a dirty
/// note, and that is what stops the next tick from painting the last stored version over
/// what is still being typed.
pub(crate) fn save_memo(ctx: &Ctx, id: &str, text: &str) -> bool {
    let mut guard = ctx.vault.borrow_mut();
    let Some(v) = guard.as_mut() else { return false };
    let mut memo = match v.store().get(id) {
        Ok(Some(m)) => m,
        // Deleted: the edits have nowhere to go and nothing is waiting to be written.
        _ => return true,
    };
    let title = title_for(&memo, text);
    if memo.body == text && memo.title == title {
        return true;
    }
    memo.title = title;
    memo.body = text.to_string();
    memo.updated_at = now_millis();
    if let Err(e) = v.upsert(&memo) {
        diag!("could not save the memo: {e}");
        crate::list::report_write_failure(&e);
        // On the note as well: this is the one place where what is lost is still on screen,
        // and the list saying so is no use behind a closed window.
        if let Some(entry) = ctx.stickies.borrow().get(id) {
            entry
                .window
                .set_notice(SharedString::from(t!("msg.write_failed", error = e)));
        }
        return false;
    }
    refresh_list(v, &ctx.model, &ctx.collapsed.borrow(), &ctx.query.borrow());
    // Reflect the new title in the title bar.
    if let Some(entry) = ctx.stickies.borrow().get(id) {
        set_title(&entry.window, &memo.title);
        // The save went through, so whatever the last one said no longer holds.
        entry.window.set_notice(SharedString::new());
    }
    true
}

/// Writes out every sticky's pending edit, stops its debounce timer and drops the notes that
/// were never written on. Returns the ids of the open stickies, in no particular order.
///
/// Called wherever the windows are about to stop existing — locking and quitting — because
/// the autosave is debounced and the last keystrokes are otherwise still only in the widget.
/// Two passes, since `save_memo` borrows the sticky map itself.
///
/// The blank ones go the same way they go when a note is closed by hand: a sticky opened and
/// left empty is not a note, and locking used to leave "(empty memo)" in the list for good —
/// the very row [`discard_if_blank`] exists to prevent.
pub(crate) fn flush_dirty(ctx: &Ctx) -> Vec<String> {
    let ids: Vec<String> = ctx.stickies.borrow().keys().cloned().collect();
    for id in &ids {
        let pending = {
            let map = ctx.stickies.borrow();
            match map.get(id) {
                Some(e) if e.dirty.get() => {
                    e.save_timer.stop();
                    Some(e.window.get_memo_text().to_string())
                }
                Some(e) => {
                    e.save_timer.stop();
                    None
                }
                None => None,
            }
        };
        if let Some(text) = pending {
            save_memo(ctx, id, &text);
        }
    }
    // After the saves, never before: a note whose only writing is still in the widget would
    // otherwise look blank and be thrown away with it.
    for id in &ids {
        discard_if_blank(ctx, id);
    }
    ids
}

/// Creates a memo and opens its sticky; shared by the + button in both windows.
pub(crate) fn new_memo(ctx: &Ctx) {
    touch(ctx);
    crate::list::clear_search(ctx);
    let mut memo = Memo::new("", "");
    {
        // Color and opacity defaults come from the settings.
        let s = ctx.settings.borrow();
        memo.color = s.default_color.clone();
        memo.opacity = s.default_opacity as i64;
    }
    {
        let mut guard = ctx.vault.borrow_mut();
        let Some(v) = guard.as_mut() else { return };
        if let Err(e) = v.upsert(&memo) {
            diag!("could not create the memo: {e}");
            crate::list::report_write_failure(&e);
            return;
        }
        refresh_list(v, &ctx.model, &ctx.collapsed.borrow(), &ctx.query.borrow());
    }
    if let Err(e) = open_sticky(ctx, &memo, true) {
        diag!("could not open the sticky window: {e}");
    }
}

/// Hides a sticky and schedules its removal from the registry; dropping the window inside
/// its own callback is unsafe, so it waits for the next event-loop turn.
/// `present`, plus the taskbar hint a sticky wants — when there is a tray to reach it from.
///
/// The two go together every time, not once when the window is built: on Windows the shell
/// hands a window a fresh taskbar button whenever it is shown, so hiding it is undone by the
/// very next `show`. See [`crate::window::skip_taskbar`].
///
/// Without a tray the button stays. It is the only other way back to a note that has slipped
/// behind something, and a desktop with no StatusNotifier host is not unusual — see
/// [`Ctx::has_tray`].
pub(crate) fn present_sticky(ctx: &Ctx, window: &StickyWindow) {
    present(window);
    if ctx.has_tray.get() {
        skip_taskbar(window);
    }
}

/// Brings one sticky to the front, showing it first if it is not on screen.
pub(crate) fn raise_sticky(ctx: &Ctx, window: &StickyWindow) {
    if window.window().is_visible() {
        // Deliberately not `present`: that resizes the window a pixel and back to force a
        // repaint, and a note already on screen has nothing to repaint — the jolt would be
        // the only thing the user saw, times every note on the desk. The taskbar hint is
        // still re-applied, since it is one call and the shell is generous with buttons.
        if ctx.has_tray.get() {
            skip_taskbar(window);
        }
    } else {
        present_sticky(ctx, window);
    }
    raise(window);
}

/// Brings every open sticky to the front. Returns how many there were.
///
/// This is the whole reason the tray has something to activate: a note that is not pinned
/// can end up behind another window, and with the stickies out of the taskbar there is no
/// button to click to get it back. Notes are only raised, never opened — the tray must not
/// decide which memos the user wanted on the desk.
///
/// **Only the ones on screen**, which is not the same as "everything in the map". A memo
/// deleted on another device leaves its window hidden but still in there (see the merge
/// timer in `sync.rs`), and so does a close, until the timer that removes it runs. Raising
/// those would put a note back on the desk that the user deleted, or one they just closed —
/// and would count towards the return value, so the tray would not fall back to opening the
/// list when the desk is in fact empty. `snap_tick` skips them for the same reason.
pub(crate) fn raise_open(ctx: &Ctx) -> usize {
    let windows: Vec<StickyWindow> = ctx
        .stickies
        .borrow()
        .values()
        .filter(|e| e.window.window().is_visible())
        .map(|e| e.window.clone_strong())
        .collect();
    for window in &windows {
        raise_sticky(ctx, window);
    }
    windows.len()
}

pub(crate) fn close_sticky(stickies: &Stickies, id: &str) {
    if let Some(entry) = stickies.borrow().get(id) {
        entry.save_timer.stop();
        let _ = entry.window.hide();
    }
    let stickies = stickies.clone();
    let id = id.to_string();
    slint::Timer::single_shot(Duration::ZERO, move || {
        stickies.borrow_mut().remove(&id);
    });
}

/// The photos of one memo, split by how they sit: the ones lying on the writing first, then
/// the ones with a band of their own under it. Two models because the two are drawn in
/// different places, and Slint's `for` cannot skip a row — see `sticky.slint`.
///
/// Photos are ciphertext inside the vault, so they are decrypted and decoded **in memory** —
/// no plaintext ever reaches a temp file. A photo that has not synced yet, or that cannot be
/// decoded, is marked `missing` so the UI can say so; an empty gap would read as data loss.
pub(crate) fn split_photo_rows(v: &Vault, memo_id: &str) -> (Vec<PhotoRow>, Vec<PhotoRow>) {
    let list = match v.store().attachments_of(memo_id) {
        Ok(l) => l,
        Err(e) => {
            diag!("could not read the attachments: {e}");
            return (Vec::new(), Vec::new());
        }
    };

    let (flow, float): (Vec<_>, Vec<_>) = list
        .into_iter()
        .partition(|a| a.mode() == ymemo_core::PhotoMode::Flow);
    (rows_of(v, float), rows_of(v, flow))
}

fn rows_of(v: &Vault, list: Vec<ymemo_core::Attachment>) -> Vec<PhotoRow> {
    list.into_iter()
        .map(|a| {
            let (w, h) = a.display_size(BODY_FONT_PX);
            let image = v
                .has_blob(&a.hash)
                .then(|| v.attachment_bytes(&a.hash).ok())
                .flatten()
                .and_then(|bytes| decode_image(&bytes));
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
            }
        })
        .collect()
}

/// Epoch millis to a local `YYYY-MM-DD HH:MM` stamp; empty when the value is unusable.
///
/// Local time, not UTC: the stamp exists to answer "when did I write this", and an offset
/// answer is worse than none. The layout is the same in both languages, so it needs no
/// catalog entry.
pub(crate) fn format_created_at(millis: i64) -> String {
    use chrono::{Local, TimeZone};
    match Local.timestamp_millis_opt(millis) {
        chrono::offset::LocalResult::Single(t) => t.format("%Y-%m-%d %H:%M").to_string(),
        _ => String::new(),
    }
}

/// Photo bytes to an RGBA8 Slint image; `None` for an unsupported format.
fn decode_image(bytes: &[u8]) -> Option<slint::Image> {
    let decoded = image::load_from_memory(bytes).ok()?.into_rgba8();
    let (w, h) = decoded.dimensions();
    let buffer =
        slint::SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(decoded.as_raw(), w, h);
    Some(slint::Image::from_rgba8(buffer))
}

/// Refills an open sticky's photo lists after an add, a resize, a mode change or a merge.
pub(crate) fn refresh_photos(ctx: &Ctx, memo_id: &str) {
    let rows = {
        let guard = ctx.vault.borrow();
        let Some(v) = guard.as_ref() else { return };
        split_photo_rows(v, memo_id)
    };
    if let Some(entry) = ctx.stickies.borrow().get(memo_id) {
        set_photo_models(&entry.window, rows);
    }
}

/// Sets a memo's title on its sticky, in both the places it appears.
///
/// Two properties for one string: `memo-title` is drawn by Slint and goes through
/// [`crate::hangul::for_slint`], `window-title` is the desktop's own title bar and must carry
/// the name as it is really spelled. Behind one function because setting one and not the
/// other is exactly the bug this replaces — two of the four callers were passing the title
/// straight through, so the same memo's title bar read one way after a save and another after
/// a merge.
pub(crate) fn set_title(window: &StickyWindow, title: &str) {
    window.set_memo_title(SharedString::from(crate::hangul::for_slint(title)));
    window.set_window_title(SharedString::from(title));
}

/// Sets a sticky's body and the blocks its read view draws, which must never disagree: the
/// read view is the same words, and showing yesterday's next to today's would be worse than
/// showing none.
pub(crate) fn set_body_text(window: &StickyWindow, text: &str) {
    window.set_memo_text(SharedString::from(text));
    window.set_blocks(slint::ModelRc::new(slint::VecModel::from(
        crate::markdown::blocks(text),
    )));
}

/// Hands both photo models to a sticky window; the two always change together.
pub(crate) fn set_photo_models(window: &StickyWindow, (float, flow): (Vec<PhotoRow>, Vec<PhotoRow>)) {
    window.set_photos(slint::ModelRc::new(slint::VecModel::from(float)));
    window.set_flow_photos(slint::ModelRc::new(slint::VecModel::from(flow)));
}

/// A photo chosen by the worker thread. Only `Send` values, since it crosses to the event loop.
struct PhotoPick {
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
fn spawn_photo_picker(memo_id: String, title: String, picking: Arc<AtomicBool>) {
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
fn pick_photo(title: &str) -> Option<PhotoPick> {
    let path = rfd::FileDialog::new()
        .add_filter("image", &["png", "jpg", "jpeg"])
        .set_title(title)
        .pick_file()?; // cancelled
    let bytes = match std::fs::read(&path) {
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

/// Writes a photo out to a file the user picks, on a worker thread.
///
/// The bytes are read here, on the event loop, and only they cross to the worker: the vault
/// is an `Rc` and cannot. The dialog itself blocks, for the same reason `spawn_photo_picker`
/// exists — see its comment.
///
/// What is written is the file as it was attached, not the size it happens to be drawn at.
fn save_photo(ctx: &Ctx, photo_id: &str, title: String, saving: Arc<AtomicBool>) {
    if saving.swap(true, Ordering::SeqCst) {
        return; // a dialog is already open
    }
    let picked = {
        let guard = ctx.vault.borrow();
        guard.as_ref().and_then(|v| match v.store().get_attachment(photo_id) {
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
fn attach_photo(memo_id: &str, pick: PhotoPick) {
    let ctx = APP.with(|a| a.borrow().as_ref().map(|app| app.ctx.clone()));
    let Some(ctx) = ctx else { return };
    {
        let mut guard = ctx.vault.borrow_mut();
        let Some(v) = guard.as_mut() else { return };
        if let Err(e) = v.attach(memo_id, &pick.bytes, &pick.name, pick.mime, pick.width, pick.height)
        {
            diag!("could not attach the photo: {e}");
            return;
        }
    }
    refresh_photos(&ctx, memo_id);
}

/// Opens a memo's sticky, or raises it when already open.
pub(crate) fn open_sticky(ctx: &Ctx, memo: &Memo, focus: bool) -> Result<()> {
    if let Some(entry) = ctx.stickies.borrow().get(&memo.id) {
        raise_sticky(ctx, &entry.window);
        return Ok(());
    }

    let window = StickyWindow::new()?;
    // The globals are per instance, so fill this one with the current strings.
    apply_strings(&window.global::<Strings>());
    set_title(&window, &memo.title);
    set_body_text(&window, &sticky_text(memo));
    window.set_sticky_color(SharedString::from(memo.color.clone()));
    window.set_sticky_opacity(memo.opacity as f32);
    window.set_pinned(ctx.settings.borrow().memo_pinned(&memo.id));
    window.set_created_at(SharedString::from(format_created_at(memo.created_at)));
    {
        let guard = ctx.vault.borrow();
        if let Some(v) = guard.as_ref() {
            set_photo_models(&window, split_photo_rows(v, &memo.id));
        }
    }

    // Attach: pick a photo from the file dialog, which runs on a worker thread.
    {
        let ctx = ctx.clone();
        let id = memo.id.clone();
        let picking = Arc::new(AtomicBool::new(false));
        window.on_add_photo(move || {
            touch(&ctx);
            spawn_photo_picker(id.clone(), t!("ui.sticky_pick_photo"), picking.clone());
        });
    }

    // Past versions of this memo. The window belongs to main, so it is reached through the
    // same thread_local the tray uses.
    {
        let id = memo.id.clone();
        window.on_show_history(move || {
            APP.with(|a| {
                let borrow = a.borrow();
                let Some(app) = borrow.as_ref() else { return };
                touch(&app.ctx);
                crate::history::show(
                    &app.ctx,
                    &app.history,
                    &app.history_subject,
                    ymemo_core::history::Entity::Memo,
                    &id,
                );
            });
        });
    }

    // Move and resize in one write, once the pointer is released. The width crosses as
    // pixels and is stored in em, so mobile shows the same proportion of a line of text; the
    // position crosses as a fraction and is stored as one, so it lands on the same part of a
    // phone screen as of this sticky.
    {
        let ctx = ctx.clone();
        let id = memo.id.clone();
        window.on_place_photo(move |photo_id, x_frac, y_frac, width_px| {
            touch(&ctx);
            {
                let mut guard = ctx.vault.borrow_mut();
                let Some(v) = guard.as_mut() else { return };
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

    // Detach a photo. The blob file stays — another device may still be showing it.
    {
        let ctx = ctx.clone();
        let id = memo.id.clone();
        window.on_remove_photo(move |photo_id| {
            touch(&ctx);
            {
                let mut guard = ctx.vault.borrow_mut();
                let Some(v) = guard.as_mut() else { return };
                if let Err(e) = v.detach(photo_id.as_str()) {
                    diag!("could not remove the photo: {e}");
                }
            }
            refresh_photos(&ctx, &id);
        });
    }

    // Write a photo out to a file the user picks.
    {
        let ctx = ctx.clone();
        let saving = Arc::new(AtomicBool::new(false));
        window.on_save_photo(move |photo_id| {
            touch(&ctx);
            save_photo(&ctx, photo_id.as_str(), t!("ui.sticky_photo_save"), saving.clone());
        });
    }

    // Move a photo between lying on the writing and having a band of its own under it.
    {
        let ctx = ctx.clone();
        let id = memo.id.clone();
        window.on_set_photo_flow(move |photo_id, flow| {
            touch(&ctx);
            {
                let mut guard = ctx.vault.borrow_mut();
                let Some(v) = guard.as_mut() else { return };
                let mode = if flow {
                    ymemo_core::PhotoMode::Flow
                } else {
                    ymemo_core::PhotoMode::Float
                };
                if let Err(e) = v.set_attachment_mode(photo_id.as_str(), mode) {
                    diag!("could not change how the photo sits: {e}");
                    crate::list::report_write_failure(&e);
                }
            }
            refresh_photos(&ctx, &id);
        });
    }

    let dirty = Rc::new(Cell::new(false));
    let expanded_height = Rc::new(Cell::new(0.0f32));

    // An edit marks the sticky dirty and arms the debounced save.
    {
        let ctx = ctx.clone();
        let id = memo.id.clone();
        let dirty = dirty.clone();
        let weak = window.as_weak();
        window.on_edited(move |text| {
            touch(&ctx);
            // The read view is rebuilt as it is typed rather than when the caret leaves: it
            // is cheap, and doing it on the way out would show the old words for a frame.
            if let Some(w) = weak.upgrade() {
                w.set_blocks(slint::ModelRc::new(slint::VecModel::from(
                    crate::markdown::blocks(text.as_str()),
                )));
            }
            dirty.set(true);
            let ctx2 = ctx.clone();
            let id2 = id.clone();
            let dirty2 = dirty.clone();
            let weak2 = weak.clone();
            if let Some(entry) = ctx.stickies.borrow().get(&id) {
                entry.save_timer.start(TimerMode::SingleShot, SAVE_DEBOUNCE, move || {
                    if let Some(w) = weak2.upgrade() {
                        // Stays dirty when the write failed, so the note keeps what was
                        // typed instead of being merged back to the last stored version.
                        if save_memo(&ctx2, &id2, w.get_memo_text().as_str()) {
                            dirty2.set(false);
                        }
                    }
                });
            }
        });
    }

    // Close: save first, then hide the window; the memo stays.
    {
        let ctx = ctx.clone();
        let id = memo.id.clone();
        let dirty = dirty.clone();
        let weak = window.as_weak();
        window.on_close_requested(move || {
            touch(&ctx);
            if let Some(w) = weak.upgrade() {
                // Where it was when it was closed, not where the last poll saw it.
                remember_one(&ctx, &id, w.window());
                if dirty.get() && save_memo(&ctx, &id, w.get_memo_text().as_str()) {
                    dirty.set(false);
                }
            }
            // A note that was opened and left blank is not a note. Closing one used to leave
            // an "(empty memo)" row in the list for good, which the user then had to find and
            // delete — and the button that does that is one row away from the memos that are
            // not blank.
            discard_if_blank(&ctx, &id);
            // Off the desk on purpose, so the next launch does not put it back.
            {
                let mut settings = ctx.settings.borrow_mut();
                if settings.set_memo_open(&id, false) {
                    settings.save(&ctx.dir);
                }
            }
            close_sticky(&ctx.stickies, &id);
            APP.with(|a| {
                let borrow = a.borrow();
                if let Some(app) = borrow.as_ref() {
                    quit_if_last_window(&app.ctx, &app.list);
                }
            });
        });
    }

    // New memo.
    {
        let ctx = ctx.clone();
        window.on_new_memo(move || new_memo(&ctx));
    }

    // Pin: stay above other windows, or drop back among them. Stored in settings.json, so
    // it is remembered the next time this memo is opened and never reaches another device.
    // The window property alone is enough to apply it — Slint pushes the new level to the
    // window when `always-on-top` changes.
    {
        let ctx = ctx.clone();
        let id = memo.id.clone();
        let weak = window.as_weak();
        window.on_toggle_pin(move || {
            touch(&ctx);
            let Some(w) = weak.upgrade() else { return };
            let pinned = !w.get_pinned();
            {
                let mut settings = ctx.settings.borrow_mut();
                if !settings.set_memo_pinned(&id, pinned) {
                    return;
                }
                settings.save(&ctx.dir);
            }
            w.set_pinned(pinned);
            // Changing the level makes winit rebuild the ex-style from its own flags, which
            // undoes the taskbar hint; see `window::reassert_taskbar`. Nothing to undo when
            // the notes were never taken out of the taskbar in the first place.
            if ctx.has_tray.get() {
                crate::window::reassert_taskbar(&w);
            }
        });
    }

    // Color change: only the color is stored, and it syncs across devices.
    {
        let ctx = ctx.clone();
        let id = memo.id.clone();
        let weak = window.as_weak();
        window.on_set_color(move |key| {
            touch(&ctx);
            {
                let mut guard = ctx.vault.borrow_mut();
                let Some(v) = guard.as_mut() else { return };
                let Ok(Some(mut m)) = v.store().get(&id) else { return };
                if m.color == key.as_str() {
                    return;
                }
                m.color = key.to_string();
                m.updated_at = now_millis();
                if let Err(e) = v.upsert(&m) {
                    diag!("could not change the color: {e}");
                    return;
                }
                refresh_list(v, &ctx.model, &ctx.collapsed.borrow(), &ctx.query.borrow());
            }
            if let Some(w) = weak.upgrade() {
                w.set_sticky_color(key);
            }
        });
    }

    // Opacity is stored once on release; the UI previews it while dragging.
    {
        let ctx = ctx.clone();
        let id = memo.id.clone();
        window.on_set_opacity(move |pct| {
            touch(&ctx);
            let pct = ymemo_core::clamp_opacity(pct.round() as i64);
            let mut guard = ctx.vault.borrow_mut();
            let Some(v) = guard.as_mut() else { return };
            let Ok(Some(mut m)) = v.store().get(&id) else { return };
            if m.opacity == pct {
                return;
            }
            m.opacity = pct;
            m.updated_at = now_millis();
            if let Err(e) = v.upsert(&m) {
                diag!("could not change the opacity: {e}");
            }
        });
    }

    // Drag start. Where window coordinates are readable (X11) we move the window ourselves
    // and snap live; otherwise (native Wayland) the OS moves it and this returns false.
    {
        let weak = window.as_weak();
        let ctx = ctx.clone();
        let id = memo.id.clone();
        window.on_begin_drag(move |px, py| {
            touch(&ctx);
            let Some(w) = weak.upgrade() else { return false };
            let sw = w.window();
            let scale = sw.scale_factor();
            let can_move = sw
                .with_winit_window(|ww| ww.outer_position().is_ok())
                .unwrap_or(false);
            if !can_move {
                sw.with_winit_window(|ww| {
                    let _ = ww.drag_window();
                });
                return false;
            }
            if let Some(e) = ctx.stickies.borrow().get(&id) {
                e.drag_grab.set(Some(((px * scale) as i32, (py * scale) as i32)));
            }
            true
        });
    }

    // Every pointer move while dragging: compute where the pointer wants the window, snap
    // that, and move there. It is recomputed from the absolute pointer position every time,
    // so pulling past the threshold releases the snap on its own.
    {
        let weak = window.as_weak();
        let ctx = ctx.clone();
        let id = memo.id.clone();
        window.on_drag_move(move |mx, my| {
            let Some(w) = weak.upgrade() else { return };
            let map = ctx.stickies.borrow();
            let Some(me) = map.get(&id) else { return };
            let Some(grab) = me.drag_grab.get() else { return };
            let sw = w.window();
            let scale = sw.scale_factor();
            let Some(Some((pos, size, mon))) = sw.with_winit_window(|ww| {
                let p = ww.outer_position().ok()?;
                let s = ww.inner_size();
                let mon = ww.current_monitor().map(|m| {
                    let mp = m.position();
                    let ms = m.size();
                    (mp.x, mp.y, ms.width as i32, ms.height as i32)
                });
                Some(((p.x, p.y), (s.width as i32, s.height as i32), mon))
            }) else {
                return;
            };
            // Window position plus in-window pointer position is the pointer on screen;
            // minus the grab point gives where the window would be without snapping.
            let want = (
                pos.0 + (mx * scale) as i32 - grab.0,
                pos.1 + (my * scale) as i32 - grab.1,
            );
            let others = other_rects(&map, &id);
            let threshold = (SNAP_DIST * scale) as i32;
            let (nx, ny) = snap_position((want.0, want.1, size.0, size.1), &others, mon, threshold);
            if (nx, ny) != pos {
                sw.with_winit_window(|ww| {
                    ww.set_outer_position(PhysicalPosition::new(nx, ny));
                });
                me.last_pos.set(Some((nx, ny)));
            }
        });
    }

    // Release: clear the drag state and record the current position, so the snap timer does
    // not mistake this for a window that just stopped and snap it again.
    {
        let weak = window.as_weak();
        let ctx = ctx.clone();
        let id = memo.id.clone();
        window.on_drag_end(move || {
            let map = ctx.stickies.borrow();
            let Some(e) = map.get(&id) else { return };
            e.drag_grab.set(None);
            if let Some(w) = weak.upgrade() {
                if let Some(Some(p)) = w
                    .window()
                    .with_winit_window(|ww| ww.outer_position().ok().map(|p| (p.x, p.y)))
                {
                    e.last_pos.set(Some(p));
                }
            }
            e.moving.set(false);
        });
    }

    // The colour panel is taller than the note it would otherwise squeeze; grow by exactly
    // its height and take the same amount back, so a window the user has resized keeps the
    // size they chose.
    {
        let weak = window.as_weak();
        window.on_palette_toggled(move |open| {
            let Some(w) = weak.upgrade() else { return };
            let sw = w.window();
            let scale = sw.scale_factor();
            let size = sw.size();
            let (lw, lh) = (size.width as f32 / scale, size.height as f32 / scale);
            let want = if open { lh + PALETTE_HEIGHT } else { (lh - PALETTE_HEIGHT).max(BAR_HEIGHT) };
            sw.set_size(LogicalSize::new(lw, want));
        });
    }

    // Double-clicking the title bar folds the window to a thin bar and back.
    {
        let weak = window.as_weak();
        let expanded_height = expanded_height.clone();
        let ctx = ctx.clone();
        let id = memo.id.clone();
        window.on_toggle_collapse(move || {
            let w = weak.unwrap();
            let sw = w.window();
            let scale = sw.scale_factor();
            let size = sw.size();
            let logical_w = size.width as f32 / scale;
            if w.get_collapsed() {
                w.set_collapsed(false);
                let h = expanded_height.get().max(DEFAULT_SIZE.1);
                sw.set_size(LogicalSize::new(logical_w, h));
            } else {
                expanded_height.set(size.height as f32 / scale);
                w.set_collapsed(true);
                sw.set_size(LogicalSize::new(logical_w, BAR_HEIGHT));
            }
            // Written down, so a note folded on purpose is still folded when it comes back.
            let folded = w.get_collapsed();
            let mut settings = ctx.settings.borrow_mut();
            if settings.set_memo_folded(&id, folded) {
                settings.save(&ctx.dir);
            }
        });
    }

    // Back where it was left, if this note has been on the desk before. Applied after the
    // window is on screen: a size set on a window that has not been shown is what the
    // backends disagree about, and the position needs a real window underneath it.
    let (saved_geometry, folded) = {
        let settings = ctx.settings.borrow();
        (settings.memo_window(&memo.id), settings.memo_folded(&memo.id))
    };
    present_sticky(ctx, &window);
    if let Some(geometry) = saved_geometry {
        restore_geometry(&window, geometry);
        // What it should go back to when it is unfolded; the geometry above is always the
        // note's expanded size, folded or not.
        expanded_height.set(geometry[3] as f32 / window.window().scale_factor());
    }
    // Folded is a state of its own, applied after the size above rather than instead of it.
    if folded {
        window.set_collapsed(true);
        let logical_w = window.window().size().width as f32 / window.window().scale_factor();
        window.window().set_size(LogicalSize::new(logical_w, BAR_HEIGHT));
    }
    {
        let mut settings = ctx.settings.borrow_mut();
        if settings.set_memo_open(&memo.id, true) {
            settings.save(&ctx.dir);
        }
    }
    // A note opens at its first line, whatever the widget's scroll offset happened to be.
    window.invoke_body_to_top();
    if focus {
        window.invoke_focus_body();
    }
    ctx.stickies.borrow_mut().insert(
        memo.id.clone(),
        StickyEntry {
            window,
            save_timer: slint::Timer::default(),
            dirty,
            last_pos: Cell::new(None),
            moving: Cell::new(false),
            drag_grab: Cell::new(None),
        },
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Magnetic snapping
// ---------------------------------------------------------------------------

/// Quits when the window that just closed was the last one, on a desktop with no tray.
///
/// The app is tray-resident, and closing every window is meant to leave it waiting in the
/// tray. Where no tray registered there is nothing to wait in: the app went on running with
/// no window and no icon, and clicking the launcher again was the only way back — which is
/// indistinguishable from a crash, and left a vault open in a process the user believed they
/// had closed, since the idle auto-lock is off by default.
///
/// With a tray this does nothing at all; the notes are one click away there.
pub(crate) fn quit_if_last_window(ctx: &Ctx, _list: &crate::ListWindow) {
    if ctx.has_tray.get() {
        return;
    }
    // The window that triggered this is still on screen while its close is being handled — a
    // `close_requested` handler runs *before* the hide it asks for — so "the last one" can
    // only be counted on the next turn of the event loop, once it is really gone.
    let _ = slint::invoke_from_event_loop(|| {
        APP.with(|a| {
            let borrow = a.borrow();
            let Some(app) = borrow.as_ref() else { return };
            if app.list.window().is_visible() {
                return;
            }
            if app.ctx.stickies.borrow().values().any(|e| e.window.window().is_visible()) {
                return;
            }
            crate::tray::request_quit();
        });
    });
}

/// Deletes a memo that was never written in, so closing an empty note leaves nothing behind.
///
/// Only a memo with **nothing at all** on it: no title, no body, and no photo. A blank note
/// with a picture on it is a note. Nothing is offered to undo here on purpose — there is
/// nothing in it to lose, and an undo bar after every stray `+` would be its own kind of
/// clutter.
pub(crate) fn discard_if_blank(ctx: &Ctx, id: &str) {
    {
        let mut guard = ctx.vault.borrow_mut();
        let Some(v) = guard.as_mut() else { return };
        match v.store().get(id) {
            Ok(Some(memo)) if memo.title.is_empty() && memo.body.is_empty() => {
                // A photo makes it a note, and a cache that cannot be read is not grounds for
                // deleting anything.
                match v.store().attachments_of(id) {
                    Ok(photos) if photos.is_empty() => {}
                    _ => return,
                }
            }
            _ => return,
        }
        if let Err(e) = v.delete(id) {
            diag!("could not discard the blank memo: {e}");
            return;
        }
        refresh_list(v, &ctx.model, &ctx.collapsed.borrow(), &ctx.query.borrow());
    }
    let mut settings = ctx.settings.borrow_mut();
    let changed = settings.forget_memo(id);
    if changed {
        settings.save(&ctx.dir);
    }
}

// ---------------------------------------------------------------------------
// Where the notes are
//
// A sticky is a scrap of paper on a desk, and a desk that tidies itself every time you close
// a note is not one. So each window's place and size is written to `settings.json` — device
// local, never synced: which corner of which screen a note lives in is a fact about that
// screen, and two devices cannot share one.
// ---------------------------------------------------------------------------

/// Reads one window's geometry in physical px, `POS_UNKNOWN` for a position we cannot have.
fn window_geometry(window: &slint::Window) -> [i32; 4] {
    let size = window.size();
    let position = window
        .with_winit_window(|ww| ww.outer_position().ok().map(|p| (p.x, p.y)))
        .flatten();
    match position {
        Some((x, y)) => [x, y, size.width as i32, size.height as i32],
        None => [POS_UNKNOWN, POS_UNKNOWN, size.width as i32, size.height as i32],
    }
}

/// The geometry worth remembering for one sticky.
///
/// A folded note is one title bar tall, and that is a *state*, not a size: remembering it put
/// the note back as a 24px strip that nothing had marked folded, so the whole button row was
/// drawn squeezed against the title. So while a note is folded only where it moved to is new,
/// and the height it had before it was folded is kept.
fn geometry_to_remember(ctx: &Ctx, id: &str, window: &slint::Window, collapsed: bool) -> [i32; 4] {
    let mut geometry = window_geometry(window);
    if collapsed {
        geometry[3] = ctx
            .settings
            .borrow()
            .memo_window(id)
            .map_or(DEFAULT_SIZE.1 as i32, |old| old[3]);
    }
    geometry
}

/// Records this memo's window, and saves if that changed anything.
pub(crate) fn remember_one(ctx: &Ctx, id: &str, window: &slint::Window) {
    if !window.is_visible() {
        return;
    }
    let collapsed = ctx
        .stickies
        .borrow()
        .get(id)
        .is_some_and(|e| e.window.get_collapsed());
    let geometry = geometry_to_remember(ctx, id, window, collapsed);
    let changed = ctx.settings.borrow_mut().set_memo_window(id, geometry);
    if changed {
        ctx.settings.borrow().save(&ctx.dir);
    }
}

/// Records every open sticky's window, plus the list's, and saves once if anything moved.
///
/// Polled rather than hooked to a move: Slint has no "window moved" callback, which is why
/// the snapping is a poll too.
pub(crate) fn remember_geometry(ctx: &Ctx, list: &crate::ListWindow) {
    let mut changed = false;
    // Read outside the `settings` borrow: `geometry_to_remember` reads the settings itself.
    let seen: Vec<(String, [i32; 4])> = {
        let map = ctx.stickies.borrow();
        map.iter()
            .filter(|(_, e)| e.window.window().is_visible())
            .map(|(id, e)| {
                let g = geometry_to_remember(ctx, id, e.window.window(), e.window.get_collapsed());
                (id.clone(), g)
            })
            .collect()
    };
    {
        let mut settings = ctx.settings.borrow_mut();
        for (id, geometry) in &seen {
            changed |= settings.set_memo_window(id, *geometry);
        }
        if list.window().is_visible() {
            changed |= settings.set_list_window(window_geometry(list.window()));
        }
    }
    if changed {
        ctx.settings.borrow().save(&ctx.dir);
    }
}

/// A rectangle in physical px: (x, y, w, h).
pub(crate) type Rect = (i32, i32, i32, i32);

/// One snap tick: read every open sticky's position and snap the ones that just stopped.
pub(crate) fn snap_tick(stickies: &Stickies) {
    let map = stickies.borrow();
    if map.is_empty() {
        return;
    }
    // 1) Read rect and scale of the visible windows (only works on X11).
    //
    // **Not the monitor.** `current_monitor()` is a question for the windowing system, and
    // asking it for every note eleven times a second — to answer something only the one note
    // that just stopped being dragged ever asks — is most of what a desk full of notes costs
    // while nobody is touching it: measured at 4.3% of a core with two notes open and 14.4%
    // with eight, doing nothing at all. It is asked for below instead, once, of the one note
    // that needs it.
    let mut rects: Vec<(String, Rect, f32)> = Vec::new();
    for (id, e) in map.iter() {
        if !e.window.window().is_visible() {
            continue;
        }
        let got = e.window.window().with_winit_window(|ww| {
            let p = ww.outer_position().ok()?;
            let s = ww.inner_size();
            Some(((p.x, p.y, s.width as i32, s.height as i32), ww.scale_factor() as f32))
        });
        if let Some(Some((rect, scale))) = got {
            rects.push((id.clone(), rect, scale));
        }
    }

    // 2) Compare with the last tick to detect the end of a move, then snap once.
    for (idx, (id, rect, scale)) in rects.iter().enumerate() {
        let Some(e) = map.get(id) else { continue };
        let cur = (rect.0, rect.1);
        // A window being dragged is already snapped live by drag_move.
        if e.drag_grab.get().is_some() {
            e.last_pos.set(Some(cur));
            e.moving.set(false);
            continue;
        }
        if e.last_pos.get() != Some(cur) {
            // Still moving.
            e.moving.set(true);
            e.last_pos.set(Some(cur));
            continue;
        }
        if !e.moving.get() {
            continue; // still at rest, leave it alone
        }
        // Just stopped: snap to the other windows and the screen edges. The monitor is asked
        // for here and nowhere else — one note, once, at the end of one drag.
        let others: Vec<Rect> = rects
            .iter()
            .enumerate()
            .filter(|(j, _)| *j != idx)
            .map(|(_, r)| r.1)
            .collect();
        let mon = e
            .window
            .window()
            .with_winit_window(|ww| {
                ww.current_monitor().map(|m| {
                    let mp = m.position();
                    let ms = m.size();
                    (mp.x, mp.y, ms.width as i32, ms.height as i32)
                })
            })
            .flatten();
        let threshold = (SNAP_DIST * *scale) as i32;
        let (nx, ny) = snap_position(*rect, &others, mon, threshold);
        if (nx, ny) != cur {
            e.window.window().with_winit_window(|ww| {
                ww.set_outer_position(PhysicalPosition::new(nx, ny));
            });
            e.last_pos.set(Some((nx, ny)));
        }
        e.moving.set(false);
    }
}

/// Physical-px rects of the other visible stickies, i.e. the snap targets.
pub(crate) fn other_rects(map: &HashMap<String, StickyEntry>, me: &str) -> Vec<Rect> {
    let mut out = Vec::new();
    for (id, e) in map.iter() {
        if id == me || !e.window.window().is_visible() {
            continue;
        }
        let got = e.window.window().with_winit_window(|ww| {
            let p = ww.outer_position().ok()?;
            let s = ww.inner_size();
            Some((p.x, p.y, s.width as i32, s.height as i32))
        });
        if let Some(Some(r)) = got {
            out.push(r);
        }
    }
    out
}

/// Pure function computing the snapped position of `rect` against the screen and the other
/// windows: each axis is pulled independently to its nearest candidate within `threshold`.
pub(crate) fn snap_position(rect: Rect, others: &[Rect], monitor: Option<Rect>, threshold: i32) -> (i32, i32) {
    let (x, y, w, h) = rect;
    let mut xs: Vec<i32> = Vec::new();
    let mut ys: Vec<i32> = Vec::new();

    if let Some((mx, my, mw, mh)) = monitor {
        xs.push(mx); // left screen edge
        xs.push(mx + mw - w); // right screen edge
        ys.push(my); // top screen edge
        ys.push(my + mh - h); // bottom screen edge
    }
    for &(ox, oy, ow, oh) in others {
        xs.push(ox + ow); // sit to its right
        xs.push(ox - w); // sit to its left
        xs.push(ox); // align left edges
        xs.push(ox + ow - w); // align right edges
        ys.push(oy + oh); // sit below
        ys.push(oy - h); // sit above
        ys.push(oy); // align top edges
        ys.push(oy + oh - h); // align bottom edges
    }

    (nearest(&xs, x, threshold), nearest(&ys, y, threshold))
}

/// Nearest candidate to `v` within `threshold`, or `v` itself.
pub(crate) fn nearest(cands: &[i32], v: i32, threshold: i32) -> i32 {
    let mut best = v;
    let mut best_dist = threshold + 1;
    for &c in cands {
        let d = (c - v).abs();
        if d <= threshold && d < best_dist {
            best_dist = d;
            best = c;
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;

    const T: i32 = 12; // threshold

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

    #[test]
    fn snaps_to_screen_left_edge_when_near() {
        // 5px from the left edge snaps to 0.
        let mon = Some((0, 0, 1920, 1080));
        let (nx, ny) = snap_position((5, 300, 260, 240), &[], mon, T);
        assert_eq!(nx, 0);
        assert_eq!(ny, 300); // no vertical candidate
    }

    #[test]
    fn snaps_right_edge_to_neighbor_left() {
        // Our right edge (260) is 8px from their left (268), so x shifts by 8.
        let other = (268, 300, 200, 240);
        let (nx, _) = snap_position((0, 300, 260, 240), &[other], None, T);
        assert_eq!(nx, 268 - 260); // flush against the neighbor
    }

    #[test]
    fn no_snap_when_far() {
        let mon = Some((0, 0, 1920, 1080));
        let other = (900, 900, 200, 200);
        let start = (500, 500, 260, 240);
        assert_eq!(snap_position(start, &[other], mon, T), (500, 500));
    }

    #[test]
    fn aligns_tops_of_adjacent_stickies() {
        // 3px of vertical offset snaps the tops together.
        let other = (300, 100, 200, 240);
        let (_, ny) = snap_position((0, 103, 260, 240), &[other], None, T);
        assert_eq!(ny, 100);
    }
}
