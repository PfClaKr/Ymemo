//! Showing a window so that it is actually painted.
//!
//! Hiding a Slint window on Windows does not destroy it — the winit backend only calls
//! `set_visible(false)` — and that is where the trouble starts. Showing it again presents
//! the surface as it stands, and the **software renderer**, which is what Windows gets by
//! default (see `select_renderer`), has no damage recorded against that surface. So it
//! presents an empty buffer: the window comes back **white and stays white**, until
//! something unrelated finally marks it dirty.
//!
//! `request_redraw()` does not rescue it, on either turn of the event loop; the redraw
//! happens and draws nothing. A **resize** is what the renderer treats as damage to all of
//! it, so [`present`] grows the window a pixel and puts it back. That has to span two turns
//! of the loop: both sizes applied in one turn cancel out and no resize is ever delivered.
//!
//! Verified on Windows against both renderers — white with `software`, correct with
//! `femtovg`, and correct with `software` once the resize is in.

use std::time::Duration;

use slint::{ComponentHandle, LogicalSize};

use crate::icon::set_window_icon;

/// How long to leave the window a pixel taller. One turn of the loop is enough for the
/// resize to reach the renderer; this is simply a short wait that is certain to be one.
const NUDGE: Duration = Duration::from_millis(32);

/// Shows `component`, gives it the app icon and makes sure it gets painted.
pub(crate) fn present<T: ComponentHandle + 'static>(component: &T) {
    let _ = component.show();
    set_window_icon(component.window());
    component.window().request_redraw();

    let weak = component.as_weak();
    slint::Timer::single_shot(Duration::ZERO, move || {
        let Some(c) = weak.upgrade() else { return };
        let window = c.window();
        let scale = window.scale_factor();
        let original = window.size().to_logical(scale);
        window.set_size(LogicalSize::new(original.width, original.height + 1.0));

        let weak = c.as_weak();
        slint::Timer::single_shot(NUDGE, move || {
            let Some(c) = weak.upgrade() else { return };
            let window = c.window();
            // Only undo our own pixel. Folding a sticky resizes the window from another
            // callback, and restoring a stale height would undo that instead.
            let now = window.size().to_logical(window.scale_factor());
            if (now.height - (original.height + 1.0)).abs() < 0.5 {
                window.set_size(LogicalSize::new(original.width, original.height));
            }
            window.request_redraw();
        });
    });
}
