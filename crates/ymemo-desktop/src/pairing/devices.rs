//! The devices this vault is shared with: the list both windows show, and removing one.

use slint::{ComponentHandle, ModelRc, SharedString, TimerMode, VecModel};
use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;
use ymemo_core::diag;
use ymemo_core::sync::Syncthing;
use ymemo_i18n::t;

use crate::state::APP;
use crate::sync::{to_shared_row, SYNC_FOLDER_ID};
use crate::{ListWindow, LockWindow, SharedDeviceRow};

use super::{Panel, Waiting, LINKED_POLLS};

/// Keeps the shared-device list fresh and wires the remove button. Returns the timer that
/// refreshes it and the refresh itself, which the approval flow calls straight after letting
/// a device in.
pub(super) fn wire(
    lock: &LockWindow,
    list: &ListWindow,
    syncthing: &Rc<RefCell<Option<Syncthing>>>,
    waiting: &Rc<RefCell<Option<Waiting>>>,
) -> (slint::Timer, Rc<dyn Fn()>) {
    let devices_timer = slint::Timer::default();
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
                Err(e) => return diag!("could not list the shared devices: {e}"),
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

            let online = devices.iter().filter(|d| d.connected).count() as i32;
            if let Some(w) = list_w.upgrade() {
                w.set_linked_devices(devices.len() as i32);
                w.set_online_devices(online);
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
            // Record the decision in the vault first, so it travels to the other devices.
            // Dropping the peer here alone does not hold: every device is an introducer, and
            // the ones that still have it hand it straight back.
            //
            // The pairing panel is reachable from the lock screen, where there is no open
            // vault to write to. The local drop still happens; it is the one case where a
            // removal can come back, and pressing it again once unlocked makes it stick.
            APP.with(|a| {
                let borrow = a.borrow();
                let Some(app) = borrow.as_ref() else { return };
                let Some(mut guard) = app.ctx.vault_mut() else { return };
                let v = &mut *guard;
                if let Err(e) = v.revoke_device(id.as_str()) {
                    diag!("could not record the removed device in the vault: {e}");
                }
                // Straight away, rather than on the next merge: the peer is dropped and its
                // entry parked, so a re-introduction in the meantime lands on nothing live.
                crate::sync::apply_revocations(&app.ctx, v);
            });
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
    (devices_timer, Rc::new(refresh_devices))
}
