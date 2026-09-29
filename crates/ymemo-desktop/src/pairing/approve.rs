//! Answering a device that asked to be let in: the approval window and its poll.

use slint::{ComponentHandle, SharedString, TimerMode};
use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;
use ymemo_core::diag;
use ymemo_core::{pairing, sync::Syncthing};
use ymemo_i18n::t;

use crate::ApproveWindow;
use crate::sync::SYNC_FOLDER_ID;
use crate::window::present_dialog;

use super::{lift_revocation, PENDING_POLL};

/// Watches for devices asking to connect and wires the approval window's answers. The
/// returned timer is the poll; it runs only while held.
pub(super) fn wire(
    approve: &ApproveWindow,
    syncthing: &Rc<RefCell<Option<Syncthing>>>,
    refresh_devices: &Rc<dyn Fn()>,
) -> slint::Timer {
    let pending_timer = slint::Timer::default();
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
                Err(e) => return diag!("could not read the pending devices: {e}"),
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
            present_dialog(&win, (400.0, 420.0), None);
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
                    lift_revocation(&id);
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
    pending_timer
}
