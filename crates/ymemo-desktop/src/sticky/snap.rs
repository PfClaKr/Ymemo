//! Magnetic snapping: a note dragged near a screen edge or another note clips to it.
//!
//! Only where window coordinates can be read and written (X11, Windows); on native Wayland
//! it silently does nothing.

use i_slint_backend_winit::WinitWindowAccessor;
use i_slint_backend_winit::winit::dpi::PhysicalPosition;
use slint::ComponentHandle;
use std::collections::HashMap;
use std::time::{Duration, Instant};

use crate::StickyWindow;
use crate::state::{touch, Ctx, StickyEntry, Stickies};

use super::SNAP_DIST;

/// A rectangle in physical px: (x, y, w, h).
pub(crate) type Rect = (i32, i32, i32, i32);

/// How long a note has to have been still before it counts as put down.
const STILL: Duration = Duration::from_millis(150);

/// One snap tick: snap the notes that moved and have since stopped.
///
/// Which ones moved is what winit said (`Motion::moved_at`), so a desk nobody is touching
/// costs nothing here — no note is asked where it is. Only a note that has just been put down
/// is, and the others once, as snap targets. The monitor too, of that one note only: asking
/// every note for it every tick was once most of what an idle desk cost.
pub(crate) fn snap_tick(stickies: &Stickies) {
    let map = stickies.borrow();
    let now = Instant::now();
    let stopped: Vec<&String> = map
        .iter()
        .filter_map(|(id, e)| {
            let moved = e.motion.moved_at.get()?;
            // Dragged by its own title bar: `drag_move` snaps live. Still arriving: the window
            // manager's placement and `restore_geometry` are not a hand putting it down, and
            // snapping them pulled overlapping notes a little closer on every start.
            if e.drag_grab.get().is_some() || now < e.settle_until.get() {
                e.motion.moved_at.set(None);
                return None;
            }
            (now.duration_since(moved) >= STILL).then_some(id)
        })
        .collect();
    for id in stopped {
        let e = &map[id];
        e.motion.moved_at.set(None);
        if !e.window.window().is_visible() {
            continue;
        }
        let got = e.window.window().with_winit_window(|ww| {
            let p = ww.outer_position().ok()?;
            let s = ww.inner_size();
            let mon = ww.current_monitor().map(|m| {
                let (mp, ms) = (m.position(), m.size());
                (mp.x, mp.y, ms.width as i32, ms.height as i32)
            });
            Some(((p.x, p.y, s.width as i32, s.height as i32), ww.scale_factor() as f32, mon))
        });
        let Some(Some((rect, scale, mon))) = got else { continue };
        let threshold = (SNAP_DIST * scale) as i32;
        let (nx, ny) = snap_position(rect, &other_rects(&map, id), mon, threshold);
        if (nx, ny) != (rect.0, rect.1) {
            e.window.window().with_winit_window(|ww| {
                ww.set_outer_position(PhysicalPosition::new(nx, ny));
            });
        }
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
        }
    });
}

/// Release: clear the drag state, so the snap timer does not take the end of a drag it
/// already snapped live for a note just put down.
fn wire_drag_end(ctx: &Ctx, window: &StickyWindow, id: &str) {
    let ctx = ctx.clone();
    let id = id.to_string();
    window.on_drag_end(move || {
        let map = ctx.stickies.borrow();
        let Some(e) = map.get(&id) else { return };
        e.drag_grab.set(None);
        // Already snapped live while it moved; a `Moved` still on its way is not a new drop.
        e.motion.moved_at.set(None);
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
