//! Showing a window so that it is actually painted.
//!
//! Hiding a Slint window destroys the native one, so every `show()` builds a fresh window
//! that has never been drawn into. Windows fills it with the class background — white —
//! until the first frame arrives, and requesting the redraw in the same breath as `show()`
//! is too early: the native window is not up yet, so the request lands on nothing and the
//! white stays until some unrelated event (a mouse move, a merge tick) forces a repaint.
//!
//! [`present`] therefore asks twice: once now, for the backends where that is enough, and
//! once on the next turn of the event loop, by which time the window exists. The second
//! request costs one frame and only on the turn a window is shown.

use std::time::Duration;

use slint::ComponentHandle;

use crate::icon::set_window_icon;

/// Shows `component`, gives it the app icon and makes sure it gets painted.
pub(crate) fn present<T: ComponentHandle + 'static>(component: &T) {
    let _ = component.show();
    set_window_icon(component.window());
    component.window().request_redraw();

    let weak = component.as_weak();
    slint::Timer::single_shot(Duration::ZERO, move || {
        if let Some(c) = weak.upgrade() {
            c.window().request_redraw();
        }
    });
}
