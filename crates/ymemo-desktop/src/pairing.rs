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

use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::rc::Rc;
use std::time::Duration;

use slint::{ComponentHandle, LogicalSize, ModelRc, SharedString, TimerMode, VecModel};
use ymemo_core::{lan_pair, pairing, pairing::PairingCode, sync::Syncthing};
use ymemo_i18n::t;

use crate::sync::{to_shared_row, SYNC_FOLDER_ID};
use crate::window::present;
use crate::{ApproveWindow, ListWindow, LockWindow, SharedDeviceRow};

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
fn register_peer(syncthing: &Rc<RefCell<Option<Syncthing>>>, peer_id: &str) -> String {
    let guard = syncthing.borrow();
    let Some(st) = guard.as_ref() else {
        return t!("msg.sync_off_cannot_register");
    };
    match st.share_folder_with(SYNC_FOLDER_ID, peer_id) {
        Ok(()) => t!("msg.lan_connected"),
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
/// The periodic work runs only while the returned [`PairingTimers`] is alive; `main` holds
/// it to the end.
pub(crate) fn wire(
    lock: &LockWindow,
    list: &ListWindow,
    approve: &ApproveWindow,
    syncthing: &Rc<RefCell<Option<Syncthing>>>,
    lan: Option<Rc<lan_pair::PairListener>>,
    my_device_id: Option<String>,
    vault_dir: &std::path::Path,
) -> PairingTimers {
    let pair_timer = slint::Timer::default();
    let vault_watch_timer = slint::Timer::default();
    let devices_timer = slint::Timer::default();
    let pending_timer = slint::Timer::default();

    // Whose answer we are waiting on, shared by the two panels: registering from either
    // window is the same act, so both show the same waiting state.
    let waiting: Rc<RefCell<Option<Waiting>>> = Rc::new(RefCell::new(None));

    // ---- Pairing by pasting a full device id (both windows, the fallback path). ----
    {
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
    }

    // ---- LAN pairing over a 6-digit code. ----
    // join blocks for seconds, so it runs on a thread and sends its result back through a
    // channel, which pair_timer below drains and registers.
    let (join_tx, join_rx) = std::sync::mpsc::channel::<Result<Option<String>, String>>();
    {
        let lan_join = {
            let id = my_device_id.clone();
            let lock_w = lock.as_weak();
            let list_w = list.as_weak();
            let join_tx = join_tx.clone();
            move |code: SharedString| {
                let Some(my_id) = id.clone() else { return };
                let set_msg = |m: &str| {
                    if let Some(w) = lock_w.upgrade() {
                        w.set_lan_message(SharedString::from(m));
                    }
                    if let Some(w) = list_w.upgrade() {
                        w.set_lan_message(SharedString::from(m));
                    }
                };
                let code = code.trim().to_string();
                if code.len() != 6 || !code.bytes().all(|b| b.is_ascii_digit()) {
                    set_msg(&t!("msg.enter_six_digits"));
                    return;
                }
                set_msg(&t!("msg.connecting"));
                let join_tx = join_tx.clone();
                std::thread::spawn(move || {
                    let res = lan_pair::join(&code, &my_id, Duration::from_secs(6))
                        .map_err(|e| e.to_string());
                    let _ = join_tx.send(res);
                });
            }
        };
        lock.on_lan_join(lan_join.clone());
        list.on_lan_join(lan_join);
    }

    // Refresh the displayed code and register whoever paired, on a timer.
    if let Some(lan) = lan.clone() {
        let lock_w = lock.as_weak();
        let list_w = list.as_weak();
        let syncthing = syncthing.clone();
        pair_timer.start(TimerMode::Repeated, Duration::from_millis(800), move || {
            // Show this device's current code in both windows.
            let code = SharedString::from(lan.code());
            if let Some(w) = lock_w.upgrade() {
                w.set_lan_pair_code(code.clone());
            }
            if let Some(w) = list_w.upgrade() {
                w.set_lan_pair_code(code.clone());
            }
            let set_msg = |m: String| {
                let m = SharedString::from(m);
                if let Some(w) = lock_w.upgrade() {
                    w.set_lan_message(m.clone());
                }
                if let Some(w) = list_w.upgrade() {
                    w.set_lan_message(m);
                }
            };
            // Peers that joined with our code (host side).
            while let Some(peer) = lan.next_paired_peer() {
                set_msg(register_peer(&syncthing, &peer));
            }
            // Results of us joining with their code (joiner side).
            while let Ok(res) = join_rx.try_recv() {
                match res {
                    Ok(Some(peer)) => set_msg(register_peer(&syncthing, &peer)),
                    Ok(None) => set_msg(t!("msg.lan_peer_not_found")),
                    Err(e) => set_msg(e),
                }
            }
        });
    }

    // ---- Watch for vault.json: once pairing has synced it to a device that chose "link to
    // an existing device", switch the lock screen to password entry. Without this the user
    // could enter a password first and create a vault with its own salt, diverging keys.
    if !vault_dir.join("vault.json").exists() {
        let lock_weak = lock.as_weak();
        let vault_json = vault_dir.join("vault.json");
        vault_watch_timer.start(TimerMode::Repeated, Duration::from_millis(800), move || {
            if !vault_json.exists() {
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

    // ---- Shared devices: periodic refresh plus revoke. ----
    // Both windows share one model, so updating it updates both.
    let devices_model: Rc<VecModel<SharedDeviceRow>> = Rc::new(VecModel::from(Vec::new()));
    lock.set_shared_devices(ModelRc::from(devices_model.clone()));
    list.set_shared_devices(ModelRc::from(devices_model.clone()));

    let refresh_devices = {
        let syncthing = syncthing.clone();
        let devices_model = devices_model.clone();
        let waiting = waiting.clone();
        let lock_w = lock.as_weak();
        let list_w = list.as_weak();
        move || {
            let guard = syncthing.borrow();
            let Some(st) = guard.as_ref() else { return };
            let devices = match st.shared_devices(SYNC_FOLDER_ID) {
                Ok(list) => list,
                Err(e) => return eprintln!("could not list the shared devices: {e}"),
            };

            // Has the device we asked let us in yet? It has to look connected on
            // LINKED_POLLS polls running, because a refused request flickers connected on
            // every retry (see the constant).
            let mut linked = false;
            if let Some(w) = waiting.borrow_mut().as_mut() {
                let up = devices.iter().any(|d| d.id == w.peer_id && d.connected);
                w.connected_polls = if up { w.connected_polls + 1 } else { 0 };
                linked = w.connected_polls >= LINKED_POLLS;
            }
            if linked {
                *waiting.borrow_mut() = None;
                let msg = SharedString::from(t!("msg.pair_connected"));
                for w in [lock_w.upgrade().map(Panel::Lock), list_w.upgrade().map(Panel::List)]
                    .into_iter()
                    .flatten()
                {
                    w.set_pair_state(msg.clone(), SharedString::new());
                }
            }

            devices_model.set_vec(devices.into_iter().map(to_shared_row).collect::<Vec<_>>());
        }
    };

    {
        let refresh = refresh_devices.clone();
        devices_timer.start(TimerMode::Repeated, Duration::from_secs(4), refresh);
    }
    refresh_devices(); // once at startup

    {
        let syncthing = syncthing.clone();
        let refresh = refresh_devices.clone();
        let lock_w = lock.as_weak();
        let list_w = list.as_weak();
        let unshare = move |id: SharedString| {
            let msg = match syncthing.borrow().as_ref() {
                Some(st) => match st.unshare_folder_with(SYNC_FOLDER_ID, id.as_str()) {
                    Ok(()) => t!("msg.unshared"),
                    Err(e) => t!("msg.unshare_failed", error = e),
                },
                None => t!("msg.sync_off"),
            };
            let msg = SharedString::from(msg);
            if let Some(w) = lock_w.upgrade() {
                w.set_lan_message(msg.clone());
            }
            if let Some(w) = list_w.upgrade() {
                w.set_lan_message(msg);
            }
            refresh(); // update the list right away
        };
        lock.on_unshare(unshare.clone());
        list.on_unshare(unshare);
    }

    // ---- Incoming requests: the other half of linking. ----
    // Refusals are remembered here rather than in Syncthing, which files a device again on
    // its next retry. In memory on purpose: restarting the app gives a mis-clicked "reject"
    // another chance, and there is no list of blocked devices to maintain or explain.
    let rejected: Rc<RefCell<HashSet<String>>> = Rc::new(RefCell::new(HashSet::new()));
    // Which request the window is currently showing, so it is only raised when that changes
    // — presenting it on every poll would take the focus away every two seconds.
    let shown: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));

    {
        let syncthing = syncthing.clone();
        let rejected = rejected.clone();
        let shown = shown.clone();
        let approve_w = approve.as_weak();
        pending_timer.start(TimerMode::Repeated, PENDING_POLL, move || {
            let Some(win) = approve_w.upgrade() else { return };
            let guard = syncthing.borrow();
            let Some(st) = guard.as_ref() else { return };

            let mut pending = match st.pending_devices() {
                Ok(p) => p,
                // Offline or shutting down: not worth a message on a window nobody asked for.
                Err(e) => return eprintln!("could not read the pending devices: {e}"),
            };
            pending.retain(|d| !rejected.borrow().contains(&d.id));

            let Some(next) = pending.first() else {
                // Nothing left to answer — including the case where the peer gave up, so the
                // window must not sit there offering a stale request.
                if shown.borrow_mut().take().is_some() {
                    let _ = win.hide();
                }
                return;
            };

            win.set_more_message(SharedString::from(if pending.len() > 1 {
                t!("msg.more_requests_waiting", count = pending.len() - 1)
            } else {
                String::new()
            }));

            if shown.borrow().as_deref() == Some(next.id.as_str()) {
                return; // already on screen; leave the window where the user put it
            }
            let verify = st
                .device_id()
                .map(|mine| pairing::verification_code(&mine, &next.id))
                .unwrap_or_default();
            win.set_device_id(SharedString::from(next.id.clone()));
            win.set_device_name(SharedString::from(next.name.clone()));
            win.set_verification_code(SharedString::from(verify));
            win.set_status(SharedString::new());
            win.set_status_is_error(false);
            *shown.borrow_mut() = Some(next.id.clone());
            present(&win);
        });
    }

    {
        let syncthing = syncthing.clone();
        let shown = shown.clone();
        let refresh = refresh_devices.clone();
        let approve_w = approve.as_weak();
        approve.on_allow(move || {
            let Some(win) = approve_w.upgrade() else { return };
            let id = win.get_device_id().to_string();
            let guard = syncthing.borrow();
            let Some(st) = guard.as_ref() else { return };
            // Sharing the folder back is the whole of the approval; Syncthing drops the
            // pending entry itself once the device is in the config.
            match st.share_folder_with(SYNC_FOLDER_ID, &id) {
                Ok(()) => {
                    *shown.borrow_mut() = None;
                    let _ = win.hide();
                    drop(guard);
                    refresh(); // show it in the device list straight away
                }
                Err(e) => {
                    win.set_status_is_error(true);
                    win.set_status(SharedString::from(t!("msg.approve_failed", error = e)));
                }
            }
        });
    }

    {
        let syncthing = syncthing.clone();
        let rejected = rejected.clone();
        let shown = shown.clone();
        let approve_w = approve.as_weak();
        approve.on_reject(move || {
            let Some(win) = approve_w.upgrade() else { return };
            let id = win.get_device_id().to_string();
            rejected.borrow_mut().insert(id.clone());
            // Best effort: our own answer is what silences the prompt, and clearing
            // Syncthing's copy only keeps its list tidy.
            if let Some(st) = syncthing.borrow().as_ref() {
                let _ = st.dismiss_pending_device(&id);
            }
            *shown.borrow_mut() = None;
            let _ = win.hide();
        });
    }

    {
        // Closing the window is not an answer: the request stays pending and comes back on
        // the next poll, which is what someone who wants to go and check the other device's
        // screen first would expect.
        let shown = shown.clone();
        let approve_w = approve.as_weak();
        approve.on_close_requested(move || {
            let Some(win) = approve_w.upgrade() else { return };
            *shown.borrow_mut() = None;
            let _ = win.hide();
        });
    }

    // ---- Resize the window as the pairing panel opens and closes. ----
    // The panel has to fit on one screen without scrolling, and neither window is that big
    // by default, so it grows while open and shrinks back afterwards.
    {
        let saved = Rc::new(Cell::new(None));
        let weak = lock.as_weak();
        lock.on_sync_toggled(move |open| {
            if let Some(w) = weak.upgrade() {
                grow_for_panel(w.window(), open, PAIRING_MIN_SIZE, &saved);
            }
        });
    }
    {
        let saved = Rc::new(Cell::new(None));
        let weak = list.as_weak();
        list.on_sync_toggled(move |open| {
            if let Some(w) = weak.upgrade() {
                grow_for_panel(w.window(), open, PAIRING_MIN_SIZE, &saved);
            }
        });
    }

    PairingTimers {
        _pair: pair_timer,
        _vault_watch: vault_watch_timer,
        _devices: devices_timer,
        _pending: pending_timer,
    }
}

/// Keeps the pairing timers alive; dropping it stops them.
pub(crate) struct PairingTimers {
    _pair: slint::Timer,
    _vault_watch: slint::Timer,
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
