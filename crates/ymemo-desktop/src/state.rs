//! App state shared by the callbacks.
//!
//! Slint callbacks are registered independently, so everything they need is bundled into
//! one `Ctx` and cloned into each of them; it is all `Rc`, so cloning is cheap.

use slint::VecModel;
use std::cell::{Cell, Ref, RefCell, RefMut};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Instant;
use ymemo_core::diag;
use ymemo_core::sync::Syncthing;
use ymemo_core::vault::Vault;

use crate::settings::Settings;
use crate::{
    ApproveWindow, HistoryWindow, ListRow, ListWindow, LockWindow, SecurityWindow, SettingsWindow,
    StickyWindow,
};

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
    /// Until then, a change of position is the app placing the note, not a hand moving it,
    /// and is not snapped. See `snap_tick`.
    pub(crate) settle_until: Cell<Instant>,
    /// When this note was last opened or activated, as a tick of `sticky::next_stamp`.
    /// Raising the desk goes through the notes in this order, so it keeps the stacking the
    /// user left rather than the order of a hash map.
    pub(crate) last_active: Rc<Cell<u64>>,
}

pub(crate) type Stickies = Rc<RefCell<HashMap<String, StickyEntry>>>;

impl Ctx {
    /// The state a session starts with: no vault open yet, an empty list, nothing on the
    /// desk. `has_tray` is answered later, once the tray has had its go.
    pub(crate) fn new(
        dir: PathBuf,
        settings: Settings,
        syncthing: Rc<RefCell<Option<Syncthing>>>,
    ) -> Self {
        Ctx {
            vault: Rc::new(RefCell::new(None)),
            model: Rc::new(VecModel::from(Vec::<ListRow>::new())),
            stickies: Rc::new(RefCell::new(HashMap::new())),
            collapsed: Rc::new(RefCell::new(HashSet::new())),
            query: Rc::new(RefCell::new(String::new())),
            undo: Rc::new(RefCell::new(None)),
            undo_timer: Rc::new(slint::Timer::default()),
            syncthing,
            dir: Rc::new(dir),
            settings: Rc::new(RefCell::new(settings)),
            last_activity: Rc::new(Cell::new(Instant::now())),
            has_tray: Rc::new(Cell::new(false)),
            quiet_start: Rc::new(Cell::new(crate::autostart::launched_hidden())),
        }
    }

    /// The open vault, or `None` when it is locked — **or already borrowed further up the
    /// stack**. Slint can run a callback from inside another one, and a second
    /// `borrow_mut` there is a panic: that is what used to kill the app on the first merge
    /// after a sticky was opened (see the merge timer). Asking through here turns the same
    /// mistake into a skipped action and a line in the log.
    #[track_caller]
    pub(crate) fn vault_mut(&self) -> Option<RefMut<'_, Vault>> {
        let Ok(guard) = self.vault.try_borrow_mut() else {
            let at = std::panic::Location::caller();
            diag!("the vault was already in use; skipped a write at {at}");
            return None;
        };
        RefMut::filter_map(guard, Option::as_mut).ok()
    }

    /// [`Ctx::vault_mut`] for reading.
    #[track_caller]
    pub(crate) fn vault_ref(&self) -> Option<Ref<'_, Vault>> {
        let Ok(guard) = self.vault.try_borrow() else {
            let at = std::panic::Location::caller();
            diag!("the vault was already in use; skipped a read at {at}");
            return None;
        };
        Ref::filter_map(guard, Option::as_ref).ok()
    }
}

/// The windows that exist for the whole session, built once in `main` and handed to the
/// code that wires them. Slint handles are cheap to hold; the windows are only shown and
/// hidden, never rebuilt, so their positions survive.
pub(crate) struct Ui {
    pub(crate) lock: LockWindow,
    pub(crate) list: ListWindow,
    pub(crate) settings: SettingsWindow,
    pub(crate) security: SecurityWindow,
    pub(crate) history: HistoryWindow,
    pub(crate) approve: ApproveWindow,
}

/// The bundle of shared app state.
#[derive(Clone)]
pub(crate) struct Ctx {
    pub(crate) vault: SharedVault,
    pub(crate) model: Rc<VecModel<ListRow>>,
    pub(crate) stickies: Stickies,
    /// Collapsed group ids. Device-local view state, never synced; absent means expanded,
    /// so a new device shows its contents right away.
    pub(crate) collapsed: Rc<RefCell<HashSet<String>>>,
    /// What is typed in the list's find box; empty shows the whole tree.
    ///
    /// It lives here rather than being read off the window because every refresh of the model
    /// has to honour it — a memo saved, or a merge arriving from another device, would
    /// otherwise quietly put the unfiltered list back while the user was still reading the
    /// matches.
    pub(crate) query: Rc<RefCell<String>>,
    /// The last delete, while the bar in the list is still offering to put it back, and the
    /// timer that withdraws the offer.
    ///
    /// Shared rather than private to `main` because locking has to empty it: the slot holds
    /// the removed memo's title and body, and a locked vault must leave no memo text behind.
    pub(crate) undo: Rc<RefCell<Option<ymemo_core::vault::Deleted>>>,
    pub(crate) undo_timer: Rc<slint::Timer>,
    /// App data directory, holding settings.json and session.json.
    pub(crate) dir: Rc<PathBuf>,
    /// Device-local preferences (language, lock policy, new-memo defaults, ...).
    pub(crate) settings: Rc<RefCell<Settings>>,
    /// When the user last interacted; the idle auto-lock watches this.
    pub(crate) last_activity: Rc<Cell<Instant>>,
    /// The sync daemon, when there is one. `None` on a build or a machine without it, where
    /// the app runs local-only.
    ///
    /// Here rather than only in `main` because the merge timer needs it: a merge can bring in
    /// a device removal made on another device, and applying that means telling the daemon.
    pub(crate) syncthing: Rc<RefCell<Option<ymemo_core::sync::Syncthing>>>,
    /// Whether a tray icon actually registered.
    ///
    /// Not a detail: taking the stickies out of the taskbar is only defensible *because* the
    /// tray can bring a buried note back. Plenty of Linux desktops have no StatusNotifier
    /// host at all — vanilla GNOME is one, and this project ships a Fedora package — and
    /// there the two together would leave a note with no way back to it. So the notes keep
    /// their taskbar buttons when this is false. Set once, after `tray::start`.
    pub(crate) has_tray: Rc<Cell<bool>>,
    /// Set while this launch was started by the session (`--hidden`) and has not yet been
    /// asked for. It is what keeps a machine that has just booted from handing its owner the
    /// whole desk; `lock::show_desk` clears it. See `autostart.rs`.
    pub(crate) quiet_start: Rc<Cell<bool>>,
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
