//! Telling the user a newer release exists — and nothing more than telling.
//!
//! The check itself is `ymemo_core::update`; this is the desktop's half: when to ask, where to
//! show the answer, and how to hand the link to the browser. Downloading and running an
//! installer is deliberately not here (see the core module for why) — but the link is the
//! **package for this machine**, not the release page listing all seven.
//!
//! It runs on a worker thread. The request can take seconds or hang on a captive portal, and
//! the UI thread has memos to draw; the answer comes back through `invoke_from_event_loop`.

use std::cell::RefCell;

use slint::SharedString;
use ymemo_core::update::Release;
use ymemo_i18n::t;

use crate::state::{Ctx, APP};
use crate::{ListWindow, SettingsWindow};

// The newer release, once one is known. A thread-local rather than something passed around:
// the answer arrives from a worker thread through `invoke_from_event_loop`, and whatever that
// closure captures has to be `Send` — which an `Rc` shared with the UI is not. This is the
// same reason `APP` next door is a thread-local.
thread_local! {
    static PENDING: RefCell<Option<Release>> = const { RefCell::new(None) };
}

/// Asks in the background whether there is a newer release.
///
/// `announce` is what the settings window shows while waiting; the automatic check at startup
/// passes `false` so a machine that is offline says nothing at all.
pub(crate) fn spawn_check(ctx: &Ctx, announce: bool) {
    if announce {
        with_settings(|w| w.set_update_status(SharedString::from(t!("msg.update_checking"))));
    }
    // Record the attempt, not the result: a failing check must not retry on every start.
    {
        let mut settings = ctx.settings.borrow_mut();
        settings.last_update_check = ymemo_core::now_millis();
        settings.save(&ctx.dir);
    }

    std::thread::spawn(move || {
        let found = ymemo_core::update::check(env!("CARGO_PKG_VERSION"));
        let _ = slint::invoke_from_event_loop(move || {
            match found {
                Ok(Some(release)) => {
                    eprintln!(
                        "update available: {} ({})",
                        release.version,
                        release.download_url()
                    );
                    let text = t!("msg.update_found", version = release.version);
                    let version = SharedString::from(release.version.clone());
                    // The file name, so settings can say what the button will fetch rather
                    // than leaving the user to recognise it on the page.
                    let file = SharedString::from(release.asset_name.clone());
                    PENDING.with(|p| *p.borrow_mut() = Some(release));
                    with_settings(|w| {
                        w.set_update_version(version.clone());
                        w.set_update_file(file.clone());
                        w.set_update_status(SharedString::from(text.clone()));
                    });
                    with_list(|w| w.set_update_version(version.clone()));
                }
                Ok(None) => {
                    if announce {
                        with_settings(|w| {
                            w.set_update_status(SharedString::from(t!("msg.update_latest")))
                        });
                    }
                }
                // Offline, a captive portal, GitHub having a bad day: only worth a word when
                // the user asked for the check.
                Err(e) => {
                    eprintln!("update check failed: {e}");
                    if announce {
                        with_settings(|w| w.set_update_status(SharedString::from(format!("{e}"))));
                    }
                }
            }
        });
    });
}

/// Opens the download in the user's browser: the package for this machine when the release
/// carries one, the release page otherwise. Does nothing until a check has found something.
pub(crate) fn open_download() {
    let url = PENDING.with(|p| {
        p.borrow()
            .as_ref()
            .map(|r| r.download_url().to_string())
            .unwrap_or_default()
    });
    if url.is_empty() {
        return;
    }
    if let Err(e) = open_url(&url) {
        eprintln!("could not open the browser: {e}");
    }
}

/// Hands a URL to whatever the desktop uses for one.
///
/// Two lines of platform code instead of a crate: `xdg-open` on Linux, and on Windows the
/// shell's `start` through cmd — with `CREATE_NO_WINDOW`, since a GUI-subsystem parent makes
/// a console child flash one up.
fn open_url(url: &str) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        // The empty argument is `start`'s title parameter: without it a quoted URL is taken
        // as the window title and nothing opens.
        std::process::Command::new("cmd")
            .args(["/C", "start", "", url])
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()?;
    }
    #[cfg(not(windows))]
    {
        std::process::Command::new("xdg-open")
            .arg(url)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()?;
    }
    Ok(())
}

/// Runs `f` on the settings window. Nothing happens before the app is wired up or after it
/// has quit, which is exactly when a late answer can arrive.
fn with_settings(f: impl FnOnce(&SettingsWindow)) {
    APP.with(|a| {
        if let Some(app) = a.borrow().as_ref() {
            f(&app.settings);
        }
    });
}

fn with_list(f: impl FnOnce(&ListWindow)) {
    APP.with(|a| {
        if let Some(app) = a.borrow().as_ref() {
            f(&app.list);
        }
    });
}
