//! Starting Syncthing and merging on a timer.
//!
//! The transport itself lives in `ymemo_core::sync`; this is only the desktop lifecycle —
//! spawn the daemon at startup, then periodically merge other devices' logs into the UI.

use std::time::Duration;

use slint::{ComponentHandle, TimerMode};
use ymemo_core::sync::{SharedDevice, Syncthing};

use crate::list::refresh_list;
use crate::state::Ctx;
use crate::sticky::sticky_text;
use crate::{ListWindow, SharedDeviceRow};

/// Fixed id of the vault folder in Syncthing; the same on every device, so it is defined
/// once in the core and mobile uses that same constant.
pub(crate) use ymemo_core::sync::VAULT_FOLDER_ID as SYNC_FOLDER_ID;

/// Turns a core `SharedDevice` into a Slint row, falling back to the first 7 characters of
/// the device id when there is no name (Slint cannot slice strings).
pub(crate) fn to_shared_row(d: SharedDevice) -> SharedDeviceRow {
    let name = if d.name.is_empty() {
        format!("{}…", d.id.chars().take(7).collect::<String>())
    } else {
        d.name.clone()
    };
    SharedDeviceRow {
        id: d.id.into(),
        name: name.into(),
        connected: d.connected,
    }
}

/// (Re-)arms the periodic merge timer.
///
/// The interval is a setting, so saving settings re-arms it; `Timer::start` replaces the
/// existing schedule, so the same timer can be reused.
pub(crate) fn start_merge_timer(timer: &slint::Timer, ctx: &Ctx, list_weak: slint::Weak<ListWindow>) {
    let interval = Duration::from_secs(ctx.settings.borrow().merge_seconds.max(1) as u64);
    let ctx = ctx.clone();
    timer.start(TimerMode::Repeated, interval, move || {
        let mut guard = ctx.vault.borrow_mut();
        let Some(v) = guard.as_mut() else { return };
        match v.rebuild() {
            Ok(()) => {
                refresh_list(v, &ctx.model, &ctx.collapsed.borrow());
                // Force a repaint: the Windows software renderer does not repaint on a model
                // change alone, which makes a successful merge look like a failed sync.
                if let Some(w) = list_weak.upgrade() {
                    w.window().request_redraw();
                }
                // Push remote changes into open stickies, except ones being edited.
                for (id, entry) in ctx.stickies.borrow().iter() {
                    if entry.dirty.get() {
                        continue;
                    }
                    match v.store().get(id) {
                        Ok(Some(m)) => {
                            let text = sticky_text(&m);
                            if entry.window.get_memo_text() != text.as_str() {
                                entry.window.set_memo_text(text.into());
                            }
                            entry.window.set_memo_title(m.title.into());
                            entry.window.set_sticky_color(m.color.into());
                            entry.window.set_sticky_opacity(m.opacity as f32);
                            entry.window.set_created_at(
                                crate::sticky::format_created_at(m.created_at).into(),
                            );
                            // Another device may have added, moved or resized a photo. The
                            // vault is passed straight in: it is already borrowed here, and
                            // going back through `ctx` for a second borrow is what used to
                            // kill the app on the first merge after a sticky was opened.
                            //
                            // Not while a photo is under the pointer, though: replacing the
                            // model rebuilds the items and the drag in progress would be
                            // dropped halfway — the same reason `dirty` protects the text.
                            if !entry.window.get_photo_busy() {
                                entry.window.set_photos(slint::ModelRc::new(
                                    slint::VecModel::from(crate::sticky::photo_rows(v, id)),
                                ));
                            }
                            entry.window.window().request_redraw();
                        }
                        // Deleted elsewhere: just hide it; removal happens on close.
                        Ok(None) => {
                            let _ = entry.window.hide();
                        }
                        Err(e) => eprintln!("could not read the memo: {e}"),
                    }
                }
            }
            Err(e) => eprintln!("merge failed: {e}"),
        }
    });
}

/// Finds and starts syncthing, registering the vault directory as a shared folder. `None`
/// without a binary, in which case the app runs local-only.
pub(crate) fn start_syncthing(data_dir: &std::path::Path, vault_dir: &std::path::Path) -> Option<Syncthing> {
    let bin = Syncthing::find_binary()?;
    match Syncthing::spawn(&bin, &data_dir.join("syncthing")) {
        Ok(st) => {
            if let Err(e) = st.ensure_folder(SYNC_FOLDER_ID, "Ymemo Vault", vault_dir) {
                eprintln!("could not register the shared folder: {e}");
            }
            // A safety net under the logs, not the memo history — see the core's docs.
            if let Err(e) = st.ensure_versioning(SYNC_FOLDER_ID) {
                eprintln!("could not enable vault file versioning: {e}");
            }
            Some(st)
        }
        Err(e) => {
            eprintln!("syncthing did not start, continuing without sync: {e}");
            None
        }
    }
}
