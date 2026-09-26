//! The app's own clocks: idle lock, snapping, remembering where windows are, and the
//! one-shot that gives the first windows their icon.

use slint::{ComponentHandle, TimerMode};
use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use crate::icon::set_window_icon;
use crate::lock::lock_now;
use crate::state::{Ctx, Ui};
use crate::sticky::{self, snap_tick, GEOMETRY_INTERVAL, SNAP_INTERVAL};

/// How often idleness is checked; fine-grained enough against a setting in minutes.
const IDLE_CHECK_INTERVAL: Duration = Duration::from_secs(20);

/// Everything here stops when its timer is dropped, so the caller keeps this for as long as
/// the event loop runs.
pub(crate) struct Timers {
    _idle: slint::Timer,
    _snap: slint::Timer,
    _geometry: slint::Timer,
    _icon: slint::Timer,
}

/// Starts every timer; see [`Timers`].
pub(crate) fn start(ctx: &Ctx, ui: &Ui, unlocked: &Rc<Cell<bool>>) -> Timers {
    Timers {
        _idle: idle_lock(ctx, ui, unlocked),
        _snap: snapping(ctx),
        _geometry: geometry(ctx, ui),
        _icon: first_icons(ui),
    }
}

/// Idle auto-lock.
fn idle_lock(ctx: &Ctx, ui: &Ui, unlocked: &Rc<Cell<bool>>) -> slint::Timer {
    let timer = slint::Timer::default();
    let ctx = ctx.clone();
    let lock_weak = ui.lock.as_weak();
    let list_weak = ui.list.as_weak();
    let unlocked = unlocked.clone();
    timer.start(TimerMode::Repeated, IDLE_CHECK_INTERVAL, move || {
        let minutes = ctx.settings.borrow().idle_lock_minutes;
        if minutes <= 0 || !unlocked.get() {
            return;
        }
        if ctx.last_activity.get().elapsed() < Duration::from_secs(minutes as u64 * 60) {
            return;
        }
        let (Some(lock), Some(list)) = (lock_weak.upgrade(), list_weak.upgrade()) else {
            return;
        };
        lock_now(&ctx, &lock, &list, &unlocked);
    });
    timer
}

/// Magnetic snapping: a sticky that stops moving clips to the screen or another sticky's
/// edge. Works where window positions are readable (X11), inert on Wayland.
fn snapping(ctx: &Ctx) -> slint::Timer {
    let timer = slint::Timer::default();
    let stickies = ctx.stickies.clone();
    timer.start(TimerMode::Repeated, SNAP_INTERVAL, move || snap_tick(&stickies));
    timer
}

/// Remembering where the windows are, so a desk stays arranged across a restart — and
/// bringing back any a change of monitors left off every screen.
///
/// Polled, because Slint has no "the window moved" callback — the same reason the snapping
/// is a poll. Far slower than that one: this one writes a file.
fn geometry(ctx: &Ctx, ui: &Ui) -> slint::Timer {
    let timer = slint::Timer::default();
    let ctx = ctx.clone();
    let list_weak = ui.list.as_weak();
    timer.start(TimerMode::Repeated, GEOMETRY_INTERVAL, move || {
        if let Some(list) = list_weak.upgrade() {
            sticky::rescue_offscreen(&ctx, &list);
            sticky::remember_geometry(&ctx, &list);
        }
    });
    timer
}

/// Window icons only apply once the event loop has created the winit windows: a one-shot
/// covers the lock and list windows, and later windows get theirs where they are shown
/// (`open_sticky`, the tray toggle).
fn first_icons(ui: &Ui) -> slint::Timer {
    let timer = slint::Timer::default();
    let lock_w = ui.lock.as_weak();
    let list_w = ui.list.as_weak();
    timer.start(TimerMode::SingleShot, Duration::from_millis(100), move || {
        if let Some(w) = lock_w.upgrade() {
            set_window_icon(w.window());
        }
        if let Some(w) = list_w.upgrade() {
            set_window_icon(w.window());
        }
    });
    timer
}
