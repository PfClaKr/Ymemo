//! Where the notes are.
//!
//! A sticky is a scrap of paper on a desk, and a desk that tidies itself every time you close
//! a note is not one. So each window's place and size is written to `settings.json` — device
//! local, never synced: which corner of which screen a note lives in is a fact about that
//! screen, and two devices cannot share one.

use i_slint_backend_winit::WinitWindowAccessor;
use slint::ComponentHandle;
use std::cell::RefCell;
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

/// The geometry worth remembering for one sticky.
///
/// A folded note is one title bar tall, and that is a *state*, not a size: remembering it put
/// the note back as a 24px strip that nothing had marked folded, so the whole button row was
/// drawn squeezed against the title. So while a note is folded only where it moved to is new,
/// and the height it had before it was folded is kept.
pub(super) fn geometry_to_remember(
    ctx: &Ctx,
    id: &str,
    window: &slint::Window,
    collapsed: bool,
) -> [i32; 4] {
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
    if ctx.settings.borrow_mut().set_memo_window(id, geometry) {
        if let Some((screens, _)) = window.with_winit_window(crate::screens::current) {
            if let Some(s) = crate::screens::screen_of(geometry, &screens) {
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
}

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
        for (id, geometry) in &seen {
            let moved = settings.set_memo_window(id, *geometry);
            if moved || settings.memo_screen(id).is_none() {
                if let Some(s) = crate::screens::screen_of(*geometry, &screens_now()) {
                    changed |= settings.set_memo_screen(id, s.clone());
                }
            }
            changed |= moved;
        }
        if list.window().is_visible() {
            let geometry = window_geometry(list.window());
            let moved = settings.set_list_window(geometry);
            if moved || settings.list_screen.is_none() {
                if let Some(s) = crate::screens::screen_of(geometry, &screens_now()) {
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
