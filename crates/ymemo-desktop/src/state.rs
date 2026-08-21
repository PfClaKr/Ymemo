//! App state shared by the callbacks.
//!
//! Slint callbacks are registered independently, so everything they need is bundled into
//! one `Ctx` and cloned into each of them; it is all `Rc`, so cloning is cheap.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Instant;

use slint::VecModel;
use ymemo_core::vault::Vault;

use crate::settings::Settings;
use crate::{HistoryWindow, ListRow, ListWindow, LockWindow, SettingsWindow, StickyWindow};

pub(crate) type SharedVault = Rc<RefCell<Option<Vault>>>;

/// State of one open sticky window.
pub(crate) struct StickyEntry {
    pub(crate) window: StickyWindow,
    /// Debounced save timer, living as long as the window.
    pub(crate) save_timer: slint::Timer,
    /// Unsaved edits pending; while set, the merge timer must not overwrite the body.
    pub(crate) dirty: Rc<Cell<bool>>,
    /// Window position at the last snap tick (physical px), to detect the end of a move.
    pub(crate) last_pos: Cell<Option<(i32, i32)>>,
    /// Moved since the last tick, i.e. dragging; snapping happens once it stops.
    pub(crate) moving: Cell<bool>,
    /// Grab point while dragging the title bar, relative to the window (physical px).
    pub(crate) drag_grab: Cell<Option<(i32, i32)>>,
}

pub(crate) type Stickies = Rc<RefCell<HashMap<String, StickyEntry>>>;

/// The bundle of shared app state.
#[derive(Clone)]
pub(crate) struct Ctx {
    pub(crate) vault: SharedVault,
    pub(crate) model: Rc<VecModel<ListRow>>,
    pub(crate) stickies: Stickies,
    /// Collapsed group ids. Device-local view state, never synced; absent means expanded,
    /// so a new device shows its contents right away.
    pub(crate) collapsed: Rc<RefCell<HashSet<String>>>,
    /// App data directory, holding settings.json and session.json.
    pub(crate) dir: Rc<PathBuf>,
    /// Device-local preferences (language, lock policy, new-memo defaults, ...).
    pub(crate) settings: Rc<RefCell<Settings>>,
    /// When the user last interacted; the idle auto-lock watches this.
    pub(crate) last_activity: Rc<Cell<Instant>>,
}

/// Marks user activity, resetting the idle auto-lock.
///
/// This is stamped from app callbacks rather than a global input hook, so it measures
/// interaction with the app: a mouse passing over an open window does not count.
pub(crate) fn touch(ctx: &Ctx) {
    ctx.last_activity.set(Instant::now());
}

// How tray callbacks reach the UI after invoke_from_event_loop hands them to this thread:
// slint components are not Send, so they cannot be captured by those closures directly.
thread_local! {
    pub(crate) static APP: RefCell<Option<AppUi>> = const { RefCell::new(None) };
}

pub(crate) struct AppUi {
    pub(crate) lock: LockWindow,
    pub(crate) list: ListWindow,
    /// So a sticky can open its own past without owning the window.
    pub(crate) history: HistoryWindow,
    pub(crate) history_subject: crate::history::Subject,
    /// Built once at startup and only shown or hidden, so a strong handle is fine.
    pub(crate) settings: SettingsWindow,
    pub(crate) unlocked: Rc<Cell<bool>>,
    /// So the tray's "lock" runs the same lock path.
    pub(crate) ctx: Ctx,
}
