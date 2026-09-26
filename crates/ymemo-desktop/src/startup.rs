//! Getting the process going: which renderer, which data directory, the one-shot
//! commands that run instead of a session, and the settings a session starts from.

use anyhow::Result;
use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;
use ymemo_core::diag;
use ymemo_core::sync::Syncthing;

use crate::instance;
use crate::settings::Settings;
use crate::sync::apply_folder_settings;

/// Whether the kernel is offering a device that GL could be accelerated on.
///
/// Always true off Linux, where [`select_renderer`] decides on other grounds.
fn has_render_device() -> bool {
    if !cfg!(target_os = "linux") {
        return true;
    }
    // An empty `/dev/dri` counts as none: the directory outlives the driver that filled it.
    std::fs::read_dir("/dev/dri").is_ok_and(|mut entries| entries.any(|e| e.is_ok()))
}

/// Picks the Slint renderer; call once before creating any window.
///
/// The default renderer (femtovg) needs OpenGL 2.0+, but Windows machines often have no GPU
/// driver or only legacy GL 1.1 under a VM or RDP, where even `glCreateShader` is missing.
/// So Windows defaults to the CPU renderer, which is plenty for this UI. Override with
/// `YMEMO_RENDERER=femtovg|software|skia`. Linux and macOS keep the default.
pub(crate) fn select_renderer() {
    let name = match std::env::var("YMEMO_RENDERER") {
        Ok(n) if !n.is_empty() => n,
        _ if cfg!(windows) => "software".to_string(),
        // Linux with no rendering device: Mesa has nothing to fall back to but `swrast`,
        // and that is not a renderer this app survives. Measured on a machine with no
        // `/dev/dri` — a VM, a container, an X session forwarded from elsewhere: the fourth
        // sticky window aborts the process with `malloc(): unsorted double linked list
        // corrupted`, inside `swrast_dri.so`, under the `glTexSubImage2D` that femtovg uses
        // to add a glyph to its atlas. Nothing above it can catch that. Slint's own software
        // renderer draws the same UI without a GL context at all, which is the route Windows
        // already takes for the same reason.
        //
        // Only the plain absence of a device is taken as the signal. A box that has one and
        // still lands on llvmpipe keeps the default, and `YMEMO_RENDERER=femtovg` overrides
        // either way.
        _ if !has_render_device() => "software".to_string(),
        _ => return, // keep the default where GL works
    };
    match i_slint_backend_winit::Backend::builder()
        .with_renderer_name(name.as_str())
        .build()
    {
        Ok(backend) => {
            if let Err(e) = slint::platform::set_platform(Box::new(backend)) {
                diag!("could not select renderer '{name}', continuing with the default: {e:?}");
            }
        }
        Err(e) => diag!("could not build the '{name}' backend, continuing with the default: {e}"),
    }
}

/// Platform data directory, e.g. ~/.local/share/ymemo on Linux, %APPDATA%\ymemo\Ymemo\data
/// on Windows — or whatever `YMEMO_DATA_DIR` names.
///
/// Only the path; nothing is created. `--quit` and `--purge` run before the directory should
/// exist, and creating it on the way to deleting it is how `--purge` used to report success
/// on a machine that had nothing left to delete.
///
/// The override exists because on Windows the default is not a path but a known folder id,
/// so there is otherwise no way to run a build against anything but the one real vault on
/// the machine. It also gives a portable install somewhere to put its data.
pub(crate) fn data_dir() -> std::path::PathBuf {
    if let Some(dir) = std::env::var_os("YMEMO_DATA_DIR").filter(|v| !v.is_empty()) {
        return std::path::PathBuf::from(dir);
    }
    match directories::ProjectDirs::from("dev", "ymemo", "Ymemo") {
        Some(dirs) => dirs.data_dir().to_path_buf(),
        // Last resort for a machine with no home directory at all.
        None => std::path::PathBuf::from("."),
    }
}

/// Deletes this device's data directory: vault, cache, session, settings and the sync
/// daemon's own configuration.
///
/// The running instance is stopped first, and not only because it holds the cache open:
/// Syncthing propagates deletions, so removing the vault while the daemon still carries it
/// would empty the memos on every paired device too. If the app will not quit, nothing is
/// deleted. Copies on other devices are never touched.
fn purge(dir: &std::path::Path) -> Result<()> {
    // Never act on data_dir()'s fallback: deleting the working directory is not something to
    // do on one's own initiative.
    if dir == std::path::Path::new(".") {
        anyhow::bail!("no data directory to delete on this platform");
    }
    // Before asking anything to quit: the lock file the check needs lives in this very
    // directory, so an absent one can hold neither data nor a running instance.
    if !dir.exists() {
        println!("nothing to delete: {}", dir.display());
        return Ok(());
    }
    // Returns true when the lock is free, so a machine with nothing running passes too.
    if !instance::quit_running(dir) {
        anyhow::bail!("Ymemo is still running; close it and try again");
    }
    std::fs::remove_dir_all(dir)?;
    println!("deleted {}", dir.display());
    Ok(())
}

/// Magnetic snapping needs to read and set window coordinates, which native Wayland
/// forbids; with XWayland available, drop `WAYLAND_DISPLAY` so winit takes X11.
/// `YMEMO_FORCE_WAYLAND=1` keeps Wayland and loses snapping.
pub(crate) fn prefer_x11() {
    if std::env::var_os("YMEMO_FORCE_WAYLAND").is_none() && std::env::var_os("DISPLAY").is_some() {
        std::env::remove_var("WAYLAND_DISPLAY");
    }
}

/// The command-line switches that are a message rather than a session. `Some` is the result
/// the process should exit with; `None` means carry on and start the app.
///
/// `ymemo --quit`: tell a running instance to save and exit, wait for it, and return.
/// Installers and package scripts use it to get the app and its sync daemon out of the way
/// without killing them (see packaging/).
///
/// `ymemo --purge`: delete everything this device stores and exit. It is what the Windows
/// uninstaller offers on the way out, and the only way to do the same on Linux, where a
/// package may not touch a user's home directory.
pub(crate) fn run_command(dir: &Path) -> Option<Result<()>> {
    let has = |flag: &str| std::env::args().skip(1).any(|a| a == flag);
    if has("--quit") {
        return Some(if instance::quit_running(dir) {
            Ok(())
        } else {
            Err(anyhow::anyhow!("Ymemo did not quit within the timeout"))
        });
    }
    if has("--purge") {
        return Some(purge(dir));
    }
    None
}

/// The stored settings, sanitized, with the sync timings pushed to the daemon.
pub(crate) fn load_settings(dir: &Path, syncthing: &Rc<RefCell<Option<Syncthing>>>) -> Settings {
    let mut loaded = Settings::load(dir);
    loaded.sanitize();
    apply_folder_settings(syncthing, &loaded);
    loaded
}
