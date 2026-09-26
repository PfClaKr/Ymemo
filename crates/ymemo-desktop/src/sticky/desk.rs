//! The desk as a whole: showing notes, raising them in the order they were left, and
//! keeping them out of the taskbar where there is a tray to reach them from instead.

use i_slint_backend_winit::WinitWindowAccessor;
use slint::ComponentHandle;
use std::cell::Cell;
use std::rc::Rc;
use std::time::Instant;
use ymemo_core::diag;

use crate::StickyWindow;
use crate::state::{Ctx, Motion};
use crate::window::{present, raise, skip_taskbar};

use super::SETTLE;
use super::photos::{accept_dropped_photo, looks_like_a_photo};

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
    if window.get_pinned() {
        crate::window::keep_above(window);
    }
}

/// Re-applies the taskbar hint to every note already on screen.
///
/// The desk is put back while the vault is opened, and on the ordinary way in — a session
/// that is still valid, so the app comes up already unlocked — that happens **before** the
/// tray has said whether it exists. `present_sticky` asks `has_tray` and it was still false,
/// so every restored note kept the taskbar button a sticky is never supposed to have. Called
/// once, the moment the answer is known.
pub(crate) fn hide_open_notes_from_taskbar(ctx: &Ctx) {
    if !ctx.has_tray.get() {
        return; // nowhere else to reach them from; they keep their buttons on purpose
    }
    for entry in ctx.stickies.borrow().values() {
        if entry.window.window().is_visible() {
            skip_taskbar(&entry.window);
        }
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
///
/// **In the order they were last activated**, oldest first, so the note on top of the desk is
/// still on top of it afterwards. Going through the map instead shuffled the notes every time
/// the tray was clicked — measured under openbox, twelve cascaded notes came back in hash order.
pub(crate) fn raise_open(ctx: &Ctx) -> usize {
    let mut windows: Vec<(u64, StickyWindow)> = ctx
        .stickies
        .borrow()
        .values()
        .filter(|e| e.window.window().is_visible())
        .map(|e| (e.last_active.get(), e.window.clone_strong()))
        .collect();
    windows.sort_by_key(|(stamp, _)| *stamp);
    let windows: Vec<StickyWindow> = windows.into_iter().map(|(_, w)| w).collect();
    for window in &windows {
        raise_sticky(ctx, window);
    }
    windows.len()
}

thread_local! {
    static STAMP: Cell<u64> = const { Cell::new(0) };
}

/// A number larger than every one handed out before; see `StickyEntry::last_active`.
pub(super) fn next_stamp() -> u64 {
    STAMP.with(|s| {
        s.set(s.get() + 1);
        s.get()
    })
}

/// Stacks the desk as it was left: `below` (the list) at the bottom, then the notes in
/// `ids` order, oldest first — without focusing anything.
///
/// Needed because showing them in that order is not enough. Slint creates the windows shown
/// in one turn of the event loop **last first** (`create_inactive_windows` pops a `Vec`), so
/// the desk came back upside down, and the list — shown first so the notes would land in
/// front of it — ended up over all of them. Measured under openbox.
///
/// Every window is awaited into existence first, and the restack waits a moment after that:
/// the window manager maps new windows on top as it gets round to them, and a restack that
/// arrives before the map is undone by it.
pub(crate) fn stack_desk(ctx: &Ctx, below: Option<slint::Weak<crate::ListWindow>>, ids: &[String]) {
    use i_slint_backend_winit::WinitWindowAccessor;

    let notes: Vec<slint::Weak<StickyWindow>> = {
        let map = ctx.stickies.borrow();
        ids.iter().filter_map(|id| map.get(id).map(|e| e.window.as_weak())).collect()
    };
    let spawned = slint::spawn_local(async move {
        let mut order = Vec::new();
        if let Some(list) = below.and_then(|w| w.upgrade()) {
            if let Ok(w) = list.window().winit_window().await {
                order.push(w);
            }
        }
        for note in notes {
            let Some(note) = note.upgrade() else { continue };
            if let Ok(w) = note.window().winit_window().await {
                order.push(w);
            }
        }
        slint::Timer::single_shot(SETTLE / 5, move || {
            for w in &order {
                crate::window::restack_top(w);
            }
        });
    });
    if let Err(e) = spawned {
        diag!("could not reach the event loop to stack the desk: {e}");
    }
}

/// Lets a picture dragged from a file manager onto this note become an attachment.
///
/// Slint has no drag-and-drop of its own and its winit backend throws these events away, so
/// the note asks winit directly through `on_winit_window_event`. Three events matter:
/// `HoveredFile` and `HoveredFileCancelled` light the note up and put it out again, so the
/// pointer has somewhere to aim, and `DroppedFile` is the one that actually attaches.
///
/// Dragging several files in delivers one `DroppedFile` each, which is why this takes them one
/// at a time rather than collecting a list. The events **propagate** rather than being
/// swallowed: nothing in Slint reads them, and stopping them here would only mean this had to
/// be the place that noticed if that ever changed.
///
/// There is no pointer position on a `DroppedFile` — winit does not carry one — so the picture
/// lands where the button would have put it rather than under the cursor.
///
/// The same hook — a window has only one — also stamps the note when it is **activated**, by
/// a click or by the window manager, which is what [`raise_open`] and the next restart put the
/// notes back in order by.
pub(super) fn wire_window_events(
    ctx: &Ctx,
    window: &StickyWindow,
    memo_id: &str,
    last_active: Rc<Cell<u64>>,
    motion: Rc<Motion>,
) {
    use i_slint_backend_winit::winit::event::WindowEvent;
    use i_slint_backend_winit::EventResult;

    let id = memo_id.to_string();
    let weak = window.as_weak();
    let ctx = ctx.clone();
    let born = Instant::now();
    window.window().on_winit_window_event(move |_win, event| {
        match event {
            // Not while the note is still arriving: a window manager that focuses new windows
            // hands the focus to each note as it maps, in the order Slint creates them —
            // which is backwards (see `stack_desk`). Counted, that reversed the saved order on
            // every restart.
            WindowEvent::Focused(true) if born.elapsed() >= SETTLE => {
                last_active.set(next_stamp());
                // The open list is the order the desk comes back in, bottom first; the note
                // just activated is now the top of it.
                let mut settings = ctx.settings.borrow_mut();
                if settings.move_open_memo_to_top(&id) {
                    settings.save(&ctx.dir);
                }
            }
            WindowEvent::Moved(_) => {
                motion.moved_at.set(Some(Instant::now()));
                motion.geometry_dirty.set(true);
            }
            WindowEvent::Resized(_) => motion.geometry_dirty.set(true),
            WindowEvent::HoveredFile(path) => {
                if looks_like_a_photo(path) {
                    if let Some(w) = weak.upgrade() {
                        w.set_photo_drop_target(true);
                    }
                }
            }
            WindowEvent::HoveredFileCancelled => {
                if let Some(w) = weak.upgrade() {
                    w.set_photo_drop_target(false);
                }
            }
            WindowEvent::DroppedFile(path) => {
                if let Some(w) = weak.upgrade() {
                    w.set_photo_drop_target(false);
                }
                accept_dropped_photo(id.clone(), path.clone());
            }
            _ => {}
        }
        EventResult::Propagate
    });
}
