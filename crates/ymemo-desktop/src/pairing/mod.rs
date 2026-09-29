//! UI side of device linking: registering pairing codes, answering the requests that come
//! back, rendering QRs and sizing the pairing panel. The code format and LAN discovery live
//! in the core (`ymemo_core::pairing`, `ymemo_core::lan_pair`).
//!
//! Linking is two halves and this file owns both of them:
//!
//! - **Asking.** Entering or scanning another device's code registers it and starts dialling.
//!   Nothing syncs yet, so the panel switches to a waiting state showing the verification
//!   code (`pair-waiting-code`) until that device answers.
//! - **Answering.** A device that scanned *our* code turns up in
//!   [`Syncthing::pending_devices`], and [`ApproveWindow`] asks whether to let it in. It is a
//!   window of its own because the app lives in the tray: a request that only appeared inside
//!   the pairing panel would go unseen by anyone who had closed it.

use slint::{ComponentHandle, LogicalSize, SharedString, TimerMode};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;
use ymemo_core::diag;
use ymemo_core::{lan_pair, pairing, pairing::PairingCode, sync::Syncthing};
use ymemo_i18n::t;

mod approve;
mod devices;
mod lan;

use crate::state::APP;
use crate::sync::SYNC_FOLDER_ID;
use crate::{ApproveWindow, ListWindow, LockWindow};

/// Where the installers are, which carry `ymemo-sync` with them.
const RELEASES_PAGE: &str = "https://github.com/PfClaKr/Ymemo/releases/latest";

/// How often incoming requests are polled for. Answering one is a person walking to another
/// device, so seconds are fine and a tighter loop would only spend REST calls.
const PENDING_POLL: Duration = Duration::from_secs(2);

/// Consecutive polls a peer must look connected before the waiting panel calls it linked.
///
/// One poll is not enough: while a request is unanswered the peer's TLS handshake completes
/// and is *then* refused, so `connected` flickers true for well under a second on every
/// retry. Two polls two seconds apart never straddle that.
const LINKED_POLLS: u8 = 2;

/// The peer this device asked to be let in by, while the answer has not arrived.
struct Waiting {
    peer_id: String,
    /// Consecutive polls it has looked connected; see [`LINKED_POLLS`].
    connected_polls: u8,
}

/// Smallest window (logical px) that fits the pairing panel without scrolling.
pub(crate) const PAIRING_MIN_SIZE: (f32, f32) = (360.0, 520.0);

/// Grows the window to at least `min` while a panel is open and shrinks it back on close,
/// restoring the size from `saved` so a user-chosen size survives.
///
/// Slint sizes a window once, when it is first shown, and a panel that appears later is
/// simply clipped by whatever height that was — which is how the recovery code ended up cut
/// off halfway through. Every panel taller than the window it opens in goes through here.
pub(crate) fn grow_for_panel(
    win: &slint::Window,
    open: bool,
    min: (f32, f32),
    saved: &Cell<Option<(f32, f32)>>,
) {
    let scale = win.scale_factor();
    let size = win.size();
    let cur = (size.width as f32 / scale, size.height as f32 / scale);
    if open {
        saved.set(Some(cur));
        let want = (cur.0.max(min.0), cur.1.max(min.1));
        if want != cur {
            win.set_size(LogicalSize::new(want.0, want.1));
        }
    } else if let Some((w, h)) = saved.take() {
        win.set_size(LogicalSize::new(w, h));
    }
}

/// Handler for registering a peer's pairing code.
///
/// Registering is only this device's half: it starts dialling a device that has never heard
/// of it, and nothing syncs until that device allows the request. On success the panel is put
/// into the waiting state, showing the verification code the other screen will display, and
/// `waiting` is what the poll below watches to notice the answer.
fn pairing_handler(
    syncthing: Rc<RefCell<Option<Syncthing>>>,
    waiting: Rc<RefCell<Option<Waiting>>>,
    set_state: impl Fn(SharedString, SharedString) + Clone + 'static,
) -> impl Fn(SharedString) + Clone + 'static {
    move |code| {
        let guard = syncthing.borrow();
        let Some(st) = guard.as_ref() else { return };
        let peer = match PairingCode::decode(&code) {
            Ok(p) => p.syncthing_device_id,
            Err(e) => return set_state(SharedString::from(format!("{e}")), SharedString::new()),
        };
        if let Err(e) = st.share_folder_with(SYNC_FOLDER_ID, &peer) {
            return set_state(
                SharedString::from(t!("msg.register_failed", error = e)),
                SharedString::new(),
            );
        }
        lift_revocation(&peer);
        // The code is only worth showing when our own id is readable; without it there is
        // nothing to derive it from, and the request still works, so this degrades quietly.
        let verify = st
            .device_id()
            .map(|mine| pairing::verification_code(&mine, &peer))
            .unwrap_or_default();
        *waiting.borrow_mut() = Some(Waiting { peer_id: peer, connected_polls: 0 });
        set_state(
            SharedString::from(t!("msg.pair_requested")),
            SharedString::from(verify),
        );
    }
}

/// Registers a paired device id with the shared folder; shared by both LAN paths.
/// Takes a device off the vault's removed list, which is what deliberately pairing with it
/// again means. Without this the other devices would go on dropping it, and no amount of
/// re-pairing would take.
fn lift_revocation(peer_id: &str) {
    APP.with(|a| {
        let borrow = a.borrow();
        let Some(app) = borrow.as_ref() else { return };
        let Some(mut guard) = app.ctx.vault_mut() else { return };
        let v = &mut *guard;
        if let Err(e) = v.unrevoke_device(peer_id) {
            diag!("could not clear the removed device in the vault: {e}");
        }
    });
}

fn register_peer(syncthing: &Rc<RefCell<Option<Syncthing>>>, peer_id: &str) -> String {
    let guard = syncthing.borrow();
    let Some(st) = guard.as_ref() else {
        return t!("msg.sync_off_cannot_register");
    };
    match st.share_folder_with(SYNC_FOLDER_ID, peer_id) {
        Ok(()) => {
            lift_revocation(peer_id);
            t!("msg.lan_connected")
        }
        Err(e) => t!("msg.register_failed", error = e),
    }
}

/// Renders a pairing code as a QR image with a 2-module quiet zone; the UI scales it.
pub(crate) fn qr_image(text: &str) -> Option<slint::Image> {
    let code = qrcode::QrCode::new(text.as_bytes()).ok()?;
    let width = code.width();
    let colors = code.to_colors();
    let quiet = 2usize;
    let size = width + quiet * 2;

    let mut buf = slint::SharedPixelBuffer::<slint::Rgb8Pixel>::new(size as u32, size as u32);
    let pixels = buf.make_mut_slice();
    pixels.fill(slint::Rgb8Pixel { r: 255, g: 255, b: 255 });
    for y in 0..width {
        for x in 0..width {
            if colors[y * width + x] == qrcode::Color::Dark {
                pixels[(y + quiet) * size + (x + quiet)] = slint::Rgb8Pixel { r: 0, g: 0, b: 0 };
            }
        }
    }
    Some(slint::Image::from_rgb8(buf))
}

/// Wires up everything about device linking: entering a pairing code, the 6-digit LAN code,
/// watching for vault.json, refreshing and revoking shared devices, and resizing the panel.
///
/// What `wire` needs to know about the vault on disk: where it is, and whether this device
/// made it rather than receiving it over sync. The two travel together because the watcher
/// below is the only thing that reads either.
pub(crate) struct VaultOrigin<'a> {
    pub(crate) dir: &'a std::path::Path,
    pub(crate) created_here: Rc<Cell<bool>>,
}

/// The periodic work runs only while the returned [`PairingTimers`] is alive; `main` holds
/// it to the end.
pub(crate) fn wire(
    lock: &LockWindow,
    list: &ListWindow,
    approve: &ApproveWindow,
    syncthing: &Rc<RefCell<Option<Syncthing>>>,
    lan: Option<Rc<lan_pair::PairListener>>,
    my_device_id: Option<String>,
    vault: VaultOrigin<'_>,
) -> PairingTimers {
    let VaultOrigin { dir: vault_dir, created_here } = vault;
    // Whose answer we are waiting on, shared by the two panels: registering from either
    // window is the same act, so both show the same waiting state.
    let waiting: Rc<RefCell<Option<Waiting>>> = Rc::new(RefCell::new(None));

    wire_add_peer(lock, list, syncthing, &waiting);
    let pair = lan::wire(lock, list, syncthing, lan, my_device_id);
    let vault_watch = watch_for_vault(lock, vault_dir, created_here);
    let (devices, refresh_devices) = devices::wire(lock, list, syncthing, &waiting);
    let pending = approve::wire(approve, syncthing, &refresh_devices);
    wire_panel_resize(list);

    PairingTimers { _pair: pair, _vault_watch: vault_watch, _devices: devices, _pending: pending }
}

/// Pairing by pasting a full device id (both windows, the fallback path).
fn wire_add_peer(
    lock: &LockWindow,
    list: &ListWindow,
    syncthing: &Rc<RefCell<Option<Syncthing>>>,
    waiting: &Rc<RefCell<Option<Waiting>>>,
) {
    let lock_w = lock.as_weak();
    let list_w = list.as_weak();
    let set_state = move |msg: SharedString, code: SharedString| {
        if let Some(w) = lock_w.upgrade() {
            w.set_peer_message(msg.clone());
            w.set_pair_waiting_code(code.clone());
        }
        if let Some(w) = list_w.upgrade() {
            w.set_peer_message(msg);
            w.set_pair_waiting_code(code);
        }
    };
    let handler = pairing_handler(syncthing.clone(), waiting.clone(), set_state);
    lock.on_add_peer(handler.clone());
    list.on_add_peer(handler);
    // No sync program here: the release page is where the installer that carries it is.
    let get_sync = || {
        if let Err(e) = crate::update::open_url(RELEASES_PAGE) {
            diag!("could not open the browser: {e}");
        }
    };
    lock.on_get_sync(get_sync);
    list.on_get_sync(get_sync);
}

/// Watch for vault.json: once pairing has synced it to a device that chose "link to an
/// existing device", switch the lock screen to password entry. Without this the user could
/// enter a password first and create a vault with its own salt, diverging keys.
fn watch_for_vault(
    lock: &LockWindow,
    vault_dir: &std::path::Path,
    created_here: Rc<Cell<bool>>,
) -> Rc<slint::Timer> {
    // Held by an Rc so the watch can stop itself; see the comment where it fires.
    let vault_watch_timer = Rc::new(slint::Timer::default());
    if !vault_dir.join("vault.json").exists() {
        let lock_weak = lock.as_weak();
        let vault_json = vault_dir.join("vault.json");
        // Weak, so the timer holding the closure that holds the timer is not a cycle.
        let self_ref = Rc::downgrade(&vault_watch_timer);
        vault_watch_timer.start(TimerMode::Repeated, Duration::from_millis(800), move || {
            if !vault_json.exists() {
                return;
            }
            // **It stops the moment it fires.** This ran on for the life of the process
            // before, and once the user created a vault here it kept re-announcing a pairing
            // that never happened — and, because it wrote `lock_message` every 800ms, it
            // also wiped "wrong password" about a tenth of a second after it appeared, so a
            // mistyped password looked like a dead button.
            if let Some(timer) = self_ref.upgrade() {
                timer.stop();
            }
            // A vault this device made itself is not news arriving from anywhere; the create
            // path has already put the right screen up.
            if created_here.get() {
                return;
            }
            if let Some(lock) = lock_weak.upgrade() {
                lock.set_vault_exists(true);
                // The header just arrived from the other device, so whether this vault has
                // a recovery code is only knowable now.
                lock.set_has_recovery(ymemo_core::vault::recovery_code_exists(
                    vault_json.parent().unwrap_or(&vault_json),
                ));
                lock.set_show_sync(false);
                lock.set_lock_message(t!("msg.paired_enter_password").into());
            }
        });
    }
    vault_watch_timer
}

/// Resize the window as the pairing panel opens and closes. The panel has to fit on one
/// screen without scrolling, and neither window is that big by default, so it grows while
/// open and shrinks back afterwards.
fn wire_panel_resize(list: &ListWindow) {
    let saved = Rc::new(Cell::new(None));
    let weak = list.as_weak();
    list.on_sync_toggled(move |open| {
        if let Some(w) = weak.upgrade() {
            grow_for_panel(w.window(), open, PAIRING_MIN_SIZE, &saved);
        }
    });
}

/// Keeps the pairing timers alive; dropping it stops them.
pub(crate) struct PairingTimers {
    _pair: slint::Timer,
    _vault_watch: Rc<slint::Timer>,
    _devices: slint::Timer,
    _pending: slint::Timer,
}

/// The two windows that carry a pairing panel, so the same update can be written to either.
///
/// They are separate Slint components with separate generated setters, and this is the only
/// place that has to care which one it is holding.
enum Panel {
    Lock(LockWindow),
    List(ListWindow),
}

impl Panel {
    fn set_pair_state(&self, message: SharedString, waiting_code: SharedString) {
        match self {
            Panel::Lock(w) => {
                w.set_peer_message(message);
                w.set_pair_waiting_code(waiting_code);
            }
            Panel::List(w) => {
                w.set_peer_message(message);
                w.set_pair_waiting_code(waiting_code);
            }
        }
    }
}

/// Puts this device's pairing code, and its QR, on the lock and list windows. Returns the
/// device id, or `None` when the daemon would not say.
pub(crate) fn show_own_code(
    st: &Syncthing,
    lock: &LockWindow,
    list: &ListWindow,
) -> Option<String> {
    let id = match st.device_id() {
        Ok(id) => id,
        Err(e) => {
            diag!("could not read the device id: {e}");
            return None;
        }
    };
    let code = PairingCode::new(&id).encode();
    if let Some(img) = qr_image(&code) {
        lock.set_qr_image(img);
    }
    if let Some(img) = qr_image(&code) {
        list.set_qr_image(img);
    }
    lock.set_my_pairing_code(SharedString::from(code.clone()));
    list.set_my_pairing_code(SharedString::from(code));
    lock.set_sync_available(true);
    list.set_sync_available(true);
    Some(id)
}

/// The LAN pairing listener: swaps device ids over a 6-digit code. Only started once our own
/// id is known, since that is what it answers with.
pub(crate) fn start_lan(my_device_id: Option<&String>) -> Option<Rc<lan_pair::PairListener>> {
    let id = my_device_id?;
    match lan_pair::PairListener::start(id.clone()) {
        Ok(l) => Some(Rc::new(l)),
        Err(e) => {
            diag!("LAN pairing unavailable, continuing without it: {e}");
            None
        }
    }
}
