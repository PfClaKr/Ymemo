//! Pairing over the 6-digit code two devices on one network can read to each other.

use slint::{ComponentHandle, SharedString, TimerMode};
use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;
use ymemo_core::{lan_pair, sync::Syncthing};
use ymemo_i18n::t;

use crate::{ListWindow, LockWindow};

use super::register_peer;

/// Wires the 6-digit LAN code on both windows. The returned timer keeps the code on screen
/// fresh and registers whoever paired; it runs only while held.
pub(super) fn wire(
    lock: &LockWindow,
    list: &ListWindow,
    syncthing: &Rc<RefCell<Option<Syncthing>>>,
    lan: Option<Rc<lan_pair::PairListener>>,
    my_device_id: Option<String>,
) -> slint::Timer {
    let pair_timer = slint::Timer::default();
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
    //
    // **The timer runs whether or not this device could open the listener.** Only the top
    // half needs one: showing our own six digits, and taking the peers that joined with them.
    // Draining `join_rx` is the other direction — someone typed *their* code here — and that
    // needs no listener at all, which is exactly the case the panel promises still works when
    // something else already holds the port. With the whole timer behind `lan` the join
    // thread's answer sat in the channel forever: the peer was never registered, the folder
    // never shared back, and the panel stayed on "connecting" until it was closed. The other
    // device then dialled in as a stranger and asked to be approved — a screen that tells the
    // user to check eight characters against a device which, having paired over the six
    // digits already, has no reason to be showing them.
    {
        let lan = lan.clone();
        let lock_w = lock.as_weak();
        let list_w = list.as_weak();
        let syncthing = syncthing.clone();
        pair_timer.start(TimerMode::Repeated, Duration::from_millis(800), move || {
            let set_msg = |m: String| {
                let m = SharedString::from(m);
                if let Some(w) = lock_w.upgrade() {
                    w.set_lan_message(m.clone());
                }
                if let Some(w) = list_w.upgrade() {
                    w.set_lan_message(m);
                }
            };
            if let Some(lan) = lan.as_ref() {
                // Show this device's current code in both windows.
                let code = SharedString::from(lan.code());
                if let Some(w) = lock_w.upgrade() {
                    w.set_lan_pair_code(code.clone());
                }
                if let Some(w) = list_w.upgrade() {
                    w.set_lan_pair_code(code.clone());
                }
                // Peers that joined with our code (host side).
                while let Some(peer) = lan.next_paired_peer() {
                    set_msg(register_peer(&syncthing, &peer));
                }
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
    pair_timer
}
