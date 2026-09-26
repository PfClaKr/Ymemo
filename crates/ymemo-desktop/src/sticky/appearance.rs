//! How a note looks and sits: pinned, coloured, see-through, folded.

use slint::{ComponentHandle, LogicalSize};
use std::cell::Cell;
use std::rc::Rc;
use ymemo_core::diag;
use ymemo_core::now_millis;

use crate::StickyWindow;
use crate::list::refresh_list;
use crate::state::{touch, Ctx};

use super::{BAR_HEIGHT, DEFAULT_SIZE, PALETTE_HEIGHT};

/// Wires pinning, colour, opacity, the colour panel and folding.
pub(super) fn wire(ctx: &Ctx, window: &StickyWindow, id: &str, expanded_height: &Rc<Cell<f32>>) {
    wire_pin(ctx, window, id);
    wire_color(ctx, window, id);
    wire_opacity(ctx, window, id);
    wire_palette(window);
    wire_fold(ctx, window, id, expanded_height);
}

/// Pin: stay above other windows, or drop back among them. Stored in settings.json, so
/// it is remembered the next time this memo is opened and never reaches another device.
/// The window property alone is enough to apply it — Slint pushes the new level to the
/// window when `always-on-top` changes.
fn wire_pin(ctx: &Ctx, window: &StickyWindow, id: &str) {
    let ctx = ctx.clone();
    let id = id.to_string();
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

/// Color change: only the color is stored, and it syncs across devices.
fn wire_color(ctx: &Ctx, window: &StickyWindow, id: &str) {
    let ctx = ctx.clone();
    let id = id.to_string();
    let weak = window.as_weak();
    window.on_set_color(move |key| {
        touch(&ctx);
        {
            let Some(mut guard) = ctx.vault_mut() else { return };
            let v = &mut *guard;
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

/// Opacity is stored once on release; the UI previews it while dragging.
fn wire_opacity(ctx: &Ctx, window: &StickyWindow, id: &str) {
    let ctx = ctx.clone();
    let id = id.to_string();
    window.on_set_opacity(move |pct| {
        touch(&ctx);
        let pct = ymemo_core::clamp_opacity(pct.round() as i64);
        let Some(mut guard) = ctx.vault_mut() else { return };
        let v = &mut *guard;
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

/// The colour panel is taller than the note it would otherwise squeeze; grow by exactly
/// its height and take the same amount back, so a window the user has resized keeps the
/// size they chose.
fn wire_palette(window: &StickyWindow) {
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

/// Double-clicking the title bar folds the window to a thin bar and back.
fn wire_fold(ctx: &Ctx, window: &StickyWindow, id: &str, expanded_height: &Rc<Cell<f32>>) {
    let weak = window.as_weak();
    let expanded_height = expanded_height.clone();
    let ctx = ctx.clone();
    let id = id.to_string();
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
