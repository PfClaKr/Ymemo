//! Magnetic snapping: a note dragged near a screen edge or another note clips to it.
//!
//! Only where window coordinates can be read and written (X11, Windows); on native Wayland
//! it silently does nothing.

use i_slint_backend_winit::WinitWindowAccessor;
use i_slint_backend_winit::winit::dpi::PhysicalPosition;
use slint::ComponentHandle;
use std::collections::HashMap;
use std::time::Instant;

use crate::StickyWindow;
use crate::state::{touch, Ctx, StickyEntry, Stickies};

use super::SNAP_DIST;

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
        // First sighting, or the app still placing it. Snapping here treated a note merely
        // *appearing* as the end of a drag: every restart pulled overlapping notes 10px
        // towards their neighbours, so a desk drifted a little further each time the app
        // started. Measured under openbox with twelve cascaded notes.
        if e.last_pos.get().is_none() || Instant::now() < e.settle_until.get() {
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

/// Wires dragging a note by its title bar, snapping as it goes.
pub(super) fn wire_drag(ctx: &Ctx, window: &StickyWindow, id: &str) {
    wire_drag_start(ctx, window, id);
    wire_drag_move(ctx, window, id);
    wire_drag_end(ctx, window, id);
}

/// Drag start. Where window coordinates are readable (X11) we move the window ourselves
/// and snap live; otherwise (native Wayland) the OS moves it and this returns false.
fn wire_drag_start(ctx: &Ctx, window: &StickyWindow, id: &str) {
    let weak = window.as_weak();
    let ctx = ctx.clone();
    let id = id.to_string();
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

/// Every pointer move while dragging: compute where the pointer wants the window, snap
/// that, and move there. It is recomputed from the absolute pointer position every time,
/// so pulling past the threshold releases the snap on its own.
fn wire_drag_move(ctx: &Ctx, window: &StickyWindow, id: &str) {
    let weak = window.as_weak();
    let ctx = ctx.clone();
    let id = id.to_string();
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

/// Release: clear the drag state and record the current position, so the snap timer does
/// not mistake this for a window that just stopped and snap it again.
fn wire_drag_end(ctx: &Ctx, window: &StickyWindow, id: &str) {
    let weak = window.as_weak();
    let ctx = ctx.clone();
    let id = id.to_string();
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
