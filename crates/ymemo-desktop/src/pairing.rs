//! UI side of device linking: registering pairing codes, rendering QRs and sizing the
//! pairing panel. The code format and LAN discovery live in the core
//! (`ymemo_core::pairing`, `ymemo_core::lan_pair`).

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use slint::{ComponentHandle, LogicalSize, ModelRc, SharedString, TimerMode, VecModel};
use ymemo_core::{lan_pair, pairing::PairingCode, sync::Syncthing};
use ymemo_i18n::t;

use crate::sync::{to_shared_row, SYNC_FOLDER_ID};
use crate::{ListWindow, LockWindow, SharedDeviceRow};

/// Smallest window (logical px) that fits the pairing panel without scrolling.
pub(crate) const PAIRING_MIN_SIZE: (f32, f32) = (360.0, 520.0);

/// Grows the window to fit the pairing panel and shrinks it back on close, restoring the
/// size from `saved` so a user-chosen size survives.
pub(crate) fn resize_for_pairing(win: &slint::Window, open: bool, saved: &Cell<Option<(f32, f32)>>) {
    let scale = win.scale_factor();
    let size = win.size();
    let cur = (size.width as f32 / scale, size.height as f32 / scale);
    if open {
        saved.set(Some(cur));
        let want = (cur.0.max(PAIRING_MIN_SIZE.0), cur.1.max(PAIRING_MIN_SIZE.1));
        if want != cur {
            win.set_size(LogicalSize::new(want.0, want.1));
        }
    } else if let Some((w, h)) = saved.take() {
        win.set_size(LogicalSize::new(w, h));
    }
}

/// Handler for registering a peer's pairing code. The lock and list windows share the logic
/// and pass their own `set_msg` for the result.
pub(crate) fn pairing_handler(
    syncthing: Rc<RefCell<Option<Syncthing>>>,
    set_msg: impl Fn(SharedString) + 'static,
) -> impl Fn(SharedString) + 'static {
    move |code| {
        let guard = syncthing.borrow();
        let Some(st) = guard.as_ref() else { return };
        let msg = match PairingCode::decode(&code) {
            Ok(peer) => match st.share_folder_with(SYNC_FOLDER_ID, &peer.syncthing_device_id) {
                Ok(()) => t!("msg.register_done"),
                Err(e) => t!("msg.register_failed", error = e),
            },
            Err(e) => format!("{e}"),
        };
        set_msg(SharedString::from(msg));
    }
}

/// Registers a paired device id with the shared folder; shared by both LAN paths.
pub(crate) fn register_peer(syncthing: &Rc<RefCell<Option<Syncthing>>>, peer_id: &str) -> String {
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
    syncthing: &Rc<RefCell<Option<Syncthing>>>,
    lan: Option<Rc<lan_pair::PairListener>>,
    my_device_id: Option<String>,
    vault_dir: &std::path::Path,
) -> PairingTimers {
    let pair_timer = slint::Timer::default();
    let vault_watch_timer = slint::Timer::default();
    let devices_timer = slint::Timer::default();
    // ---- Pairing by pasting a full device id (both windows, the fallback path). ----
    {
        let w = lock.as_weak();
        lock.on_add_peer(pairing_handler(syncthing.clone(), move |m| {
            w.unwrap().set_peer_message(m)
        }));
    }
    {
        let w = list.as_weak();
        list.on_add_peer(pairing_handler(syncthing.clone(), move |m| {
            w.unwrap().set_peer_message(m)
        }));
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
        move || {
            let guard = syncthing.borrow();
            let Some(st) = guard.as_ref() else { return };
            match st.shared_devices(SYNC_FOLDER_ID) {
                Ok(list) => devices_model.set_vec(
                    list.into_iter().map(to_shared_row).collect::<Vec<_>>(),
                ),
                Err(e) => eprintln!("could not list the shared devices: {e}"),
            }
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

    // ---- Resize the window as the pairing panel opens and closes. ----
    // The panel has to fit on one screen without scrolling, and neither window is that big
    // by default, so it grows while open and shrinks back afterwards.
    {
        let saved = Rc::new(Cell::new(None));
        let weak = lock.as_weak();
        lock.on_sync_toggled(move |open| {
            if let Some(w) = weak.upgrade() {
                resize_for_pairing(w.window(), open, &saved);
            }
        });
    }
    {
        let saved = Rc::new(Cell::new(None));
        let weak = list.as_weak();
        list.on_sync_toggled(move |open| {
            if let Some(w) = weak.upgrade() {
                resize_for_pairing(w.window(), open, &saved);
            }
        });
    }

    PairingTimers {
        _pair: pair_timer,
        _vault_watch: vault_watch_timer,
        _devices: devices_timer,
    }
}

/// Keeps the pairing timers alive; dropping it stops them.
pub(crate) struct PairingTimers {
    _pair: slint::Timer,
    _vault_watch: slint::Timer,
    _devices: slint::Timer,
}
