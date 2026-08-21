//! Sticky windows: creating them, saving edits, closing them, and magnetic snapping.
//!
//! One window per memo, with the body doubling as the editor (debounced autosave). Snapping
//! only works where window coordinates can be read and written (X11, Windows); elsewhere
//! (native Wayland) it silently does nothing.

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
use crate::state::{touch, Ctx, StickyEntry, Stickies, APP};
use crate::window::present;
use crate::{apply_strings, PhotoRow, StickyWindow, Strings};

/// Body font size (logical px) that photo sizes in em are measured against.
/// **Must match the body `font-size` in `ui/sticky.slint`.**
const BODY_FONT_PX: f64 = 13.0;

/// Debounce between an edit and the autosave.
pub(crate) const SAVE_DEBOUNCE: Duration = Duration::from_millis(800);
/// Title bar height, i.e. the collapsed window height; must match app.slint.
pub(crate) const BAR_HEIGHT: f32 = 28.0;
/// Snap distance in logical px; within it, edges stick to the screen or another sticky.
pub(crate) const SNAP_DIST: f32 = 12.0;
/// How often sticky positions are polled for snapping.
pub(crate) const SNAP_INTERVAL: Duration = Duration::from_millis(90);

/// Body text for the window; an older memo with only a title promotes it to the body.
pub(crate) fn sticky_text(memo: &Memo) -> String {
    if memo.body.is_empty() && !memo.title.is_empty() {
        memo.title.clone()
    } else {
        memo.body.clone()
    }
}

/// First non-empty line of the body, used as the title in the list and title bar.
pub(crate) fn derive_title(text: &str) -> String {
    text.lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim()
        .chars()
        .take(40)
        .collect()
}

/// Saves an edited body to the vault, deriving the title from its first line.
pub(crate) fn save_memo(ctx: &Ctx, id: &str, text: &str) {
    let mut guard = ctx.vault.borrow_mut();
    let Some(v) = guard.as_mut() else { return };
    let mut memo = match v.store().get(id) {
        Ok(Some(m)) => m,
        _ => return, // drop leftover edits of a deleted memo
    };
    let title = derive_title(text);
    if memo.body == text && memo.title == title {
        return;
    }
    memo.title = title;
    memo.body = text.to_string();
    memo.updated_at = now_millis();
    if let Err(e) = v.upsert(&memo) {
        eprintln!("could not save the memo: {e}");
        return;
    }
    refresh_list(v, &ctx.model, &ctx.collapsed.borrow());
    // Reflect the new title in the title bar.
    if let Some(entry) = ctx.stickies.borrow().get(id) {
        entry.window.set_memo_title(SharedString::from(memo.title));
    }
}

/// Writes out every sticky's pending edit and stops its debounce timer. Returns the ids of
/// the open stickies, in no particular order.
///
/// Called wherever the windows are about to stop existing — locking and quitting — because
/// the autosave is debounced and the last keystrokes are otherwise still only in the widget.
/// Two passes, since `save_memo` borrows the sticky map itself.
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
    ids
}

/// Creates a memo and opens its sticky; shared by the + button in both windows.
pub(crate) fn new_memo(ctx: &Ctx) {
    touch(ctx);
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
            eprintln!("could not create the memo: {e}");
            return;
        }
        refresh_list(v, &ctx.model, &ctx.collapsed.borrow());
    }
    if let Err(e) = open_sticky(ctx, &memo, true) {
        eprintln!("could not open the sticky window: {e}");
    }
}

/// Hides a sticky and schedules its removal from the registry; dropping the window inside
/// its own callback is unsafe, so it waits for the next event-loop turn.
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

/// Builds the Slint model of a memo's photos.
///
/// Photos are ciphertext inside the vault, so they are decrypted and decoded **in memory** —
/// no plaintext ever reaches a temp file. A photo that has not synced yet, or that cannot be
/// decoded, is marked `missing` so the UI can say so; an empty gap would read as data loss.
pub(crate) fn photo_rows(v: &Vault, memo_id: &str) -> Vec<PhotoRow> {
    let list = match v.store().attachments_of(memo_id) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("could not read the attachments: {e}");
            return Vec::new();
        }
    };

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
            }
        })
        .collect()
}

/// Photo bytes to an RGBA8 Slint image; `None` for an unsupported format.
fn decode_image(bytes: &[u8]) -> Option<slint::Image> {
    let decoded = image::load_from_memory(bytes).ok()?.into_rgba8();
    let (w, h) = decoded.dimensions();
    let buffer =
        slint::SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(decoded.as_raw(), w, h);
    Some(slint::Image::from_rgba8(buffer))
}

/// Refills an open sticky's photo list after an add, a resize or a remote merge.
pub(crate) fn refresh_photos(ctx: &Ctx, memo_id: &str) {
    let rows = {
        let guard = ctx.vault.borrow();
        let Some(v) = guard.as_ref() else { return };
        photo_rows(v, memo_id)
    };
    if let Some(entry) = ctx.stickies.borrow().get(memo_id) {
        entry
            .window
            .set_photos(slint::ModelRc::new(slint::VecModel::from(rows)));
    }
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
            eprintln!("could not read the photo: {e}");
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
            eprintln!("could not attach the photo: {e}");
            return;
        }
    }
    refresh_photos(&ctx, memo_id);
}

/// Opens a memo's sticky, or raises it when already open.
pub(crate) fn open_sticky(ctx: &Ctx, memo: &Memo, focus: bool) -> Result<()> {
    if let Some(entry) = ctx.stickies.borrow().get(&memo.id) {
        present(&entry.window);
        return Ok(());
    }

    let window = StickyWindow::new()?;
    // The globals are per instance, so fill this one with the current strings.
    apply_strings(&window.global::<Strings>());
    window.set_memo_title(SharedString::from(memo.title.clone()));
    window.set_memo_text(SharedString::from(sticky_text(memo)));
    window.set_sticky_color(SharedString::from(memo.color.clone()));
    window.set_sticky_opacity(memo.opacity as f32);
    {
        let guard = ctx.vault.borrow();
        if let Some(v) = guard.as_ref() {
            window.set_photos(slint::ModelRc::new(slint::VecModel::from(photo_rows(v, &memo.id))));
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

    // Resize a photo. The value is in em, so mobile sees the same proportion.
    {
        let ctx = ctx.clone();
        let id = memo.id.clone();
        window.on_resize_photo(move |photo_id, delta_em| {
            touch(&ctx);
            {
                let mut guard = ctx.vault.borrow_mut();
                let Some(v) = guard.as_mut() else { return };
                let Ok(Some(a)) = v.store().get_attachment(photo_id.as_str()) else { return };
                let next = a.width_em_milli + (delta_em * 1000.0) as i64;
                if let Err(e) = v.set_attachment_width(photo_id.as_str(), next) {
                    eprintln!("could not resize the photo: {e}");
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
        window.on_edited(move |_| {
            touch(&ctx);
            dirty.set(true);
            let ctx2 = ctx.clone();
            let id2 = id.clone();
            let dirty2 = dirty.clone();
            let weak2 = weak.clone();
            if let Some(entry) = ctx.stickies.borrow().get(&id) {
                entry.save_timer.start(TimerMode::SingleShot, SAVE_DEBOUNCE, move || {
                    if let Some(w) = weak2.upgrade() {
                        save_memo(&ctx2, &id2, w.get_memo_text().as_str());
                        dirty2.set(false);
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
            if dirty.get() {
                if let Some(w) = weak.upgrade() {
                    save_memo(&ctx, &id, w.get_memo_text().as_str());
                }
                dirty.set(false);
            }
            close_sticky(&ctx.stickies, &id);
        });
    }

    // New memo.
    {
        let ctx = ctx.clone();
        window.on_new_memo(move || new_memo(&ctx));
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
                    eprintln!("could not change the color: {e}");
                    return;
                }
                refresh_list(v, &ctx.model, &ctx.collapsed.borrow());
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
                eprintln!("could not change the opacity: {e}");
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

    // Double-clicking the title bar folds the window to a thin bar and back.
    {
        let weak = window.as_weak();
        let expanded_height = expanded_height.clone();
        window.on_toggle_collapse(move || {
            let w = weak.unwrap();
            let sw = w.window();
            let scale = sw.scale_factor();
            let size = sw.size();
            let logical_w = size.width as f32 / scale;
            if w.get_collapsed() {
                w.set_collapsed(false);
                let h = expanded_height.get().max(120.0);
                sw.set_size(LogicalSize::new(logical_w, h));
            } else {
                expanded_height.set(size.height as f32 / scale);
                w.set_collapsed(true);
                sw.set_size(LogicalSize::new(logical_w, BAR_HEIGHT));
            }
        });
    }

    present(&window);
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

/// A rectangle in physical px: (x, y, w, h).
pub(crate) type Rect = (i32, i32, i32, i32);

/// One snap tick: read every open sticky's position and snap the ones that just stopped.
pub(crate) fn snap_tick(stickies: &Stickies) {
    let map = stickies.borrow();
    if map.is_empty() {
        return;
    }
    // 1) Read rect, scale and monitor of the visible windows (only works on X11).
    let mut rects: Vec<(String, Rect, f32, Option<Rect>)> = Vec::new();
    for (id, e) in map.iter() {
        if !e.window.window().is_visible() {
            continue;
        }
        let got = e.window.window().with_winit_window(|ww| {
            let p = ww.outer_position().ok()?;
            let s = ww.inner_size();
            let mon = ww.current_monitor().map(|m| {
                let mp = m.position();
                let ms = m.size();
                (mp.x, mp.y, ms.width as i32, ms.height as i32)
            });
            Some(((p.x, p.y, s.width as i32, s.height as i32), ww.scale_factor() as f32, mon))
        });
        if let Some(Some((rect, scale, mon))) = got {
            rects.push((id.clone(), rect, scale, mon));
        }
    }

    // 2) Compare with the last tick to detect the end of a move, then snap once.
    for (idx, (id, rect, scale, mon)) in rects.iter().enumerate() {
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
        // Just stopped: snap to the other windows and the screen edges.
        let others: Vec<Rect> = rects
            .iter()
            .enumerate()
            .filter(|(j, _)| *j != idx)
            .map(|(_, r)| r.1)
            .collect();
        let threshold = (SNAP_DIST * *scale) as i32;
        let (nx, ny) = snap_position(*rect, &others, *mon, threshold);
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
