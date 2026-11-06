//! Where the notes are.
//!
//! A sticky is a scrap of paper on a desk, and a desk that tidies itself every time you close
//! a note is not one. So each window's place and size is written to `settings.json` — device
//! local, never synced: which corner of which screen a note lives in is a fact about that
//! screen, and two devices cannot share one.

use i_slint_backend_winit::WinitWindowAccessor;
use slint::ComponentHandle;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::time::{Duration, Instant};
use ymemo_core::diag;

use crate::StickyWindow;
use crate::settings::POS_UNKNOWN;
use crate::state::Ctx;

use super::DEFAULT_SIZE;

/// Reads one window's geometry in physical px, `POS_UNKNOWN` for a position we cannot have.
pub(super) fn window_geometry(window: &slint::Window) -> [i32; 4] {
    let size = window.size();
    let position = window
        .with_winit_window(|ww| ww.outer_position().ok().map(|p| (p.x, p.y)))
        .flatten();
    match position {
        Some((x, y)) => [x, y, size.width as i32, size.height as i32],
        None => [POS_UNKNOWN, POS_UNKNOWN, size.width as i32, size.height as i32],
    }
}

/// `physical` as `settings.json` keeps it: the position as it is, the size in logical px —
/// see `settings::sane_geometry` for why the two differ.
fn kept_geometry(physical: [i32; 4], scale: f32) -> [i32; 4] {
    if scale <= 0.0 {
        return physical;
    }
    let logical = |px: i32| (px as f32 / scale).round() as i32;
    [physical[0], physical[1], logical(physical[2]), logical(physical[3])]
}

/// The geometry worth remembering for one sticky, in the units `settings.json` keeps.
///
/// A folded note is one title bar tall, and that is a *state*, not a size: remembering it put
/// the note back as a 24px strip that nothing had marked folded, so the whole button row was
/// drawn squeezed against the title. So while a note is folded only where it moved to is new,
/// and the height it had before it was folded is kept.
///
/// The colour panel is the same kind of thing: opening it grows the window by the panel's
/// height, and a note closed or locked with it open came back that much taller — 62px more
/// every time, with the panel shut. So its height is taken off while it is open.
///
/// `physical` is [`window_geometry`] of `window`, read once by the caller, which also needs it
/// to find the window's screen.
pub(super) fn geometry_to_remember(
    ctx: &Ctx,
    id: &str,
    window: &slint::Window,
    physical: [i32; 4],
    collapsed: bool,
    palette_open: bool,
) -> [i32; 4] {
    let mut geometry = kept_geometry(physical, window.scale_factor());
    if palette_open && !collapsed {
        geometry[3] = (geometry[3] - super::PALETTE_HEIGHT.round() as i32).max(1);
    }
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
    let (collapsed, palette_open) = ctx
        .stickies
        .borrow()
        .get(id)
        .map_or((false, false), |e| (e.window.get_collapsed(), e.window.get_show_palette()));
    let physical = window_geometry(window);
    let geometry = geometry_to_remember(ctx, id, window, physical, collapsed, palette_open);
    if ctx.settings.borrow_mut().set_memo_window(id, geometry) {
        if let Some((screens, _)) = window.with_winit_window(crate::screens::current) {
            if let Some(s) = crate::screens::screen_of(physical, &screens) {
                ctx.settings.borrow_mut().set_memo_screen(id, s.clone());
            }
        }
        ctx.settings.borrow().save(&ctx.dir);
    }
}

/// The screens, asked through whichever of our windows has a winit window to ask with: the
/// list's when it has been shown, otherwise any note's — a quiet start shows only notes.
pub(super) fn screens_via(
    ctx: &Ctx,
    list: &crate::ListWindow,
) -> Option<(Vec<crate::screens::Screen>, Option<usize>)> {
    list.window().with_winit_window(crate::screens::current).or_else(|| {
        ctx.stickies
            .borrow()
            .values()
            .find_map(|e| e.window.window().with_winit_window(crate::screens::current))
    })
}

thread_local! {
    /// The screens as the last geometry tick saw them; see [`rescue_offscreen`].
    static LAST_SCREENS: RefCell<Option<Vec<crate::screens::Screen>>> = const { RefCell::new(None) };
    /// Until when [`resync_scales`] runs after the screens changed, and how often each window
    /// has been nudged since.
    static RESYNC_UNTIL: Cell<Option<Instant>> = const { Cell::new(None) };
    static NUDGED: RefCell<HashMap<i_slint_backend_winit::winit::window::WindowId, u8>> =
        RefCell::new(HashMap::new());
}

/// How long after a change of screens the scales are checked: long enough for Windows to have
/// finished putting the windows back, which it does over several seconds after a session
/// switch (the log shows the screens settling 17 s apart on a return from Remote Desktop).
const RESYNC_FOR: Duration = Duration::from_secs(30);
/// Nudges per window per change of screens; a window that still disagrees after these is
/// standing across two screens on purpose, and jiggling it for the rest of the window would
/// be the bug.
const MAX_NUDGES: u8 = 3;

/// Brings back any window a change of monitors has left off every screen, while the app is
/// running — a laptop unplugged from its dock, a screen switched off.
///
/// Windows moves top-level windows off a screen that goes away; an X11 window manager may
/// not, and a sticky left in the space where a monitor used to be has no taskbar button to
/// reach it by. So on every geometry tick the screens are compared with the last ones, and
/// only when they differ is each open window checked — and only a window nothing of whose
/// title bar is on a screen is moved, with the screen it was remembered on deciding where.
/// **Called before** [`remember_geometry`], which would otherwise record the stranded place
/// and the screen it no longer has.
pub(crate) fn rescue_offscreen(ctx: &Ctx, list: &crate::ListWindow) {
    use crate::screens::{place, reachable};

    let Some((screens, primary)) = screens_via(ctx, list) else { return };
    let changed = LAST_SCREENS.with(|last| {
        let mut last = last.borrow_mut();
        let changed = last.as_ref().is_some_and(|l| *l != screens);
        *last = Some(screens.clone());
        changed
    });
    if changed {
        RESYNC_UNTIL.with(|u| u.set(Some(Instant::now() + RESYNC_FOR)));
        NUDGED.with(|n| n.borrow_mut().clear());
    }
    if RESYNC_UNTIL.with(|u| u.get()).is_some_and(|until| Instant::now() < until) {
        resync_scales(ctx, list);
    }
    if !changed || screens.is_empty() {
        return;
    }
    diag!("the screens changed: {} attached", screens.len());
    let mut windows: Vec<(Option<String>, slint::Weak<StickyWindow>)> = Vec::new();
    for (id, e) in ctx.stickies.borrow().iter() {
        if e.window.window().is_visible() {
            windows.push((Some(id.clone()), e.window.as_weak()));
        }
    }
    let move_if_stranded = |window: &slint::Window, was: Option<crate::screens::Screen>| {
        let g = window_geometry(window);
        if g[0] == POS_UNKNOWN || reachable(g, &screens) {
            return;
        }
        let (x, y) = place(g, was.as_ref(), &screens, primary);
        diag!("a window was left off every screen at ({},{}); moved to ({x},{y})", g[0], g[1]);
        window.set_position(slint::PhysicalPosition::new(x, y));
    };
    for (id, weak) in windows {
        let Some(w) = weak.upgrade() else { continue };
        let was = id.and_then(|id| ctx.settings.borrow().memo_screen(&id));
        move_if_stranded(w.window(), was);
    }
    if list.window().is_visible() {
        let was = ctx.settings.borrow().list_screen.clone();
        move_if_stranded(list.window(), was);
    }
}

/// Puts right a window drawing at a scale its screen does not have — for a while after the
/// screens change, since that is when it happens.
///
/// Coming back to the desk from a Remote Desktop session on a phone, Windows put each note
/// back where it was and never sent `WM_DPICHANGED`: the window was 100% again while winit, and
/// so Slint, still held the phone's scale, and every note drew its writing enormous and cut off
/// until it was dragged — moving a window is what makes Windows check its scale again. So the
/// scale each window was last told is compared with its screen's, and a window that disagrees
/// is moved a pixel and back ([`crate::window::nudge`]), which is the drag without anybody's
/// hand. Windows only: an X11 window manager re-sends the scale with every configure, and on
/// Wayland a client cannot move itself.
///
/// The other half is Slint missing a change winit did get, which leaves the same picture with
/// winit right: that is handed to Slint directly. Each case writes one line to the log — the
/// two scales, nothing else — so a report says which of them it was.
fn resync_scales(ctx: &Ctx, list: &crate::ListWindow) {
    let notes: Vec<StickyWindow> = ctx
        .stickies
        .borrow()
        .values()
        .filter(|e| e.window.window().is_visible())
        .map(|e| e.window.clone_strong())
        .collect();
    for note in &notes {
        resync_one(note);
    }
    if list.window().is_visible() {
        resync_one(list);
    }
}

fn resync_one<T: ComponentHandle + 'static>(component: &T) {
    let window = component.window();
    let Some(Some((id, told, screen, inner))) = window.with_winit_window(|w| {
        let screen = w.current_monitor()?.scale_factor() as f32;
        Some((w.id(), w.scale_factor() as f32, screen, w.inner_size()))
    }) else {
        return;
    };
    let differs = |a: f32, b: f32| (a - b).abs() > 0.01;
    if cfg!(windows) && differs(told, screen) {
        let attempts = NUDGED.with(|n| {
            let mut n = n.borrow_mut();
            let count = n.entry(id).or_insert(0);
            *count += 1;
            *count
        });
        if attempts <= MAX_NUDGES {
            diag!("a window drew at scale {told:.2} on a screen at {screen:.2}; nudged it");
            crate::window::nudge(component);
        }
        return;
    }
    let slint_scale = window.scale_factor();
    // `SLINT_SCALE_FACTOR` pins Slint's scale away from the system's on purpose.
    if differs(slint_scale, told) && std::env::var_os("SLINT_SCALE_FACTOR").is_none() {
        diag!("the toolkit drew at scale {slint_scale:.2} where the window is at {told:.2}; corrected");
        use slint::platform::WindowEvent;
        window.dispatch_event(WindowEvent::ScaleFactorChanged { scale_factor: told });
        // Slint does not re-derive the logical size from a new scale on its own; it waits for
        // a resize that, here, is not coming.
        let size = slint::PhysicalSize::new(inner.width, inner.height).to_logical(told);
        window.dispatch_event(WindowEvent::Resized { size });
        window.request_redraw();
    }
}

/// Records every open sticky's window, plus the list's, and saves once if anything moved.
///
/// Polled rather than hooked to a move: Slint has no "window moved" callback, which is why
/// the snapping is a poll too.
pub(crate) fn remember_geometry(ctx: &Ctx, list: &crate::ListWindow) {
    let mut changed = false;
    // Read outside the `settings` borrow: `geometry_to_remember` reads the settings itself.
    // Only the notes winit said moved or resized (`Motion`), and any with no screen on record
    // yet — asking every note where it is, every two seconds, cost a round trip each.
    let seen: Vec<(String, [i32; 4], [i32; 4])> = {
        let map = ctx.stickies.borrow();
        let settings = ctx.settings.borrow();
        let wanted: Vec<(&String, &crate::state::StickyEntry)> = map
            .iter()
            .filter(|(id, e)| {
                e.window.window().is_visible()
                    && (e.motion.geometry_dirty.get() || settings.memo_screen(id).is_none())
            })
            .collect();
        drop(settings);
        wanted
            .into_iter()
            .map(|(id, e)| {
                e.motion.geometry_dirty.set(false);
                let physical = window_geometry(e.window.window());
                let g = geometry_to_remember(
                    ctx,
                    id,
                    e.window.window(),
                    physical,
                    e.window.get_collapsed(),
                    e.window.get_show_palette(),
                );
                (id.clone(), g, physical)
            })
            .collect()
    };
    {
        // Asked for only when something moved: the screens do not change on their own, and
        // this runs every two seconds for as long as the app is up.
        let mut screens: Option<Vec<crate::screens::Screen>> = None;
        let mut screens_now = || {
            screens
                .get_or_insert_with(|| screens_via(ctx, list).map(|(s, _)| s).unwrap_or_default())
                .clone()
        };
        let mut settings = ctx.settings.borrow_mut();
        // A window with no screen on record yet gets one even when it has not moved: a
        // desk arranged before screens were remembered would otherwise wait for its first
        // drag, and a new primary monitor before then would still scatter it.
        for (id, geometry, physical) in &seen {
            let moved = settings.set_memo_window(id, *geometry);
            if moved || settings.memo_screen(id).is_none() {
                if let Some(s) = crate::screens::screen_of(*physical, &screens_now()) {
                    changed |= settings.set_memo_screen(id, s.clone());
                }
            }
            changed |= moved;
        }
        if list.window().is_visible() {
            let physical = window_geometry(list.window());
            let moved = settings.set_list_window(kept_geometry(physical, list.window().scale_factor()));
            if moved || settings.list_screen.is_none() {
                if let Some(s) = crate::screens::screen_of(physical, &screens_now()) {
                    changed |= settings.set_list_screen(s.clone());
                }
            }
            changed |= moved;
        }
    }
    if changed {
        ctx.settings.borrow().save(&ctx.dir);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A note 300x220 on a screen at 150% is 450x330 physical px, and has to be kept as
    /// 300x220: that is what it is given back as, before its window knows the scale.
    #[test]
    fn the_size_is_kept_logical_and_the_position_physical() {
        assert_eq!(kept_geometry([2600, -400, 450, 330], 1.5), [2600, -400, 300, 220]);
        assert_eq!(kept_geometry([60, 80, 300, 220], 1.0), [60, 80, 300, 220]);
        let unknown = [POS_UNKNOWN, POS_UNKNOWN, 500, 300];
        assert_eq!(kept_geometry(unknown, 2.0), [POS_UNKNOWN, POS_UNKNOWN, 250, 150]);
    }

    /// Kept, put back at the same scale and read again, a size does not creep.
    #[test]
    fn a_kept_size_survives_a_round_trip() {
        for scale in [1.0f32, 1.25, 1.5, 1.75, 2.0] {
            for w in [140, 199, 200, 201, 333, 1000] {
                let physical = (w as f32 * scale).round() as i32;
                assert_eq!(kept_geometry([0, 0, physical, physical], scale)[2], w, "{w} at {scale}");
            }
        }
    }
}
