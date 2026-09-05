//! Syncthing transport: run the bundled binary as a child process and drive it over REST.
//!
//! Syncthing is bundled whole, not reimplemented or embedded. This module only moves
//! files: register the vault directory as a shared folder and Syncthing propagates it
//! between devices, while `Vault::rebuild` does the merging. Logs are per-device and
//! append-only so files never conflict, and their contents are already E2E encrypted, so
//! the transport does not have to be trusted.
//!
//! **The daemon never outlives the app.** `Drop` shuts it down on a clean exit, and the OS
//! takes care of the unclean ones: a job object on Windows, `PR_SET_PDEATHSIG` on Linux.
//! Without that an orphan keeps `ymemo-sync` locked (Windows) or keeps syncing a vault whose
//! app is gone (Linux), which is exactly what makes installing and uninstalling messy.

use anyhow::{anyhow, bail, Context, Result};
use ymemo_i18n::t;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// How long old file versions are kept: 30 days, after which staggered versioning drops
/// them. Long enough to notice a vault that lost records, short enough to bound the disk.
const MAX_VERSION_AGE_SECONDS: u64 = 30 * 24 * 60 * 60;

/// How long to wait for a first start, key generation included.
const STARTUP_TIMEOUT: Duration = Duration::from_secs(60);

/// Id of the vault folder inside Syncthing.
///
/// **Every device must use the same one.** Syncthing matches shared folders by id, so a
/// desktop and a phone that disagree here would pair happily and then sync nothing. That is
/// why it lives in the core rather than in one of the front ends.
pub const VAULT_FOLDER_ID: &str = "ymemo-vault";

/// A running Syncthing child process plus its REST client. Dropping it shuts the daemon
/// down (REST shutdown first, then kill).
pub struct Syncthing {
    child: Child,
    base_url: String,
    api_key: String,
    /// Windows: the job object that kills the daemon when this process goes away. Nothing
    /// reads it; it only has to stay open. See [`kill_with_parent`].
    #[cfg(windows)]
    _job: Option<JobHandle>,
}

/// A device that asked to connect and was turned away because this one has never heard of
/// it, as returned by [`Syncthing::pending_devices`].
///
/// This is the whole basis of the approval flow: the side that scanned a pairing code adds
/// the other and starts dialling, and the side that was scanned learns about it here rather
/// than having to scan something back. See [`crate::pairing`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingDevice {
    /// Syncthing device id — what [`Syncthing::share_folder_with`] takes to approve it.
    pub id: String,
    /// Name the device announced for itself; empty when it announced none. **Chosen by the
    /// device that is asking**, so it is a hint for the user and never an identity.
    pub name: String,
    /// Address it dialled from, which may be a relay rather than the device itself.
    pub address: String,
    /// When it last tried, as Syncthing's RFC 3339 string; empty when absent.
    pub time: String,
}

/// Another device sharing this vault, as returned by [`Syncthing::shared_devices`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SharedDevice {
    /// Syncthing device id, also the handle for unsharing.
    pub id: String,
    /// Human-readable name; empty when the config has none.
    pub name: String,
    pub connected: bool,
}

impl Syncthing {
    /// Locates the binary: `YMEMO_SYNCTHING_BIN`, then next to our executable, then PATH.
    ///
    /// The bundled copy ships as `ymemo-sync` (`.exe` on Windows) so the user never has to
    /// know about Syncthing — that is also the name in ps and Task Manager. The original
    /// name stays as a fallback for a dev machine's PATH install.
    pub fn find_binary() -> Option<PathBuf> {
        let bundled = if cfg!(windows) { "ymemo-sync.exe" } else { "ymemo-sync" };
        let plain = if cfg!(windows) { "syncthing.exe" } else { "syncthing" };

        if let Ok(p) = std::env::var("YMEMO_SYNCTHING_BIN") {
            let p = PathBuf::from(p);
            if p.is_file() {
                return Some(p);
            }
        }
        // Release packages put the renamed binary in the install directory.
        if let Some(dir) = std::env::current_exe().ok().and_then(|p| p.parent().map(Path::to_path_buf)) {
            for name in [bundled, plain] {
                let cand = dir.join(name);
                if cand.is_file() {
                    return Some(cand);
                }
            }
        }
        // Finally PATH, for dev machines.
        let paths = std::env::var_os("PATH")?;
        for name in [bundled, plain] {
            if let Some(hit) = std::env::split_paths(&paths).map(|d| d.join(name)).find(|p| p.is_file()) {
                return Some(hit);
            }
        }
        None
    }

    /// Starts the daemon with its own home directory on a free local port, no browser and
    /// no default folder. Waits out first-run key generation and reads the API key from
    /// `config.xml`.
    pub fn spawn(binary: &Path, home_dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(home_dir)?;
        let port = free_port()?;
        let gui = format!("127.0.0.1:{port}");

        let mut cmd = Command::new(binary);
        cmd.arg("serve")
            .arg("--home")
            .arg(home_dir)
            .args(["--gui-address", &gui, "--no-browser", "--no-restart"])
            .env("STNOUPGRADE", "1") // pin the version we bundled
            // v1 had --no-default-folder, dropped in v2; the env var works on both
            .env("STNODEFAULTFOLDER", "1")
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        // Windows: keep the console-subsystem child from opening (or flashing) a console.
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }
        // Linux: ask the kernel to signal the daemon when we die (see kill_with_parent).
        #[cfg(target_os = "linux")]
        unsafe {
            use std::os::unix::process::CommandExt;
            cmd.pre_exec(|| {
                libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
                // The parent may already have died between fork and here, in which case the
                // signal was missed; getppid() == 1 catches that window.
                if libc::getppid() == 1 {
                    libc::raise(libc::SIGTERM);
                }
                Ok(())
            });
        }
        let child = cmd
            .spawn()
            .with_context(|| t!("core.syncthing_spawn_failed", path = binary.display()))?;

        let mut st = Self {
            #[cfg(windows)]
            _job: kill_with_parent(&child),
            child,
            base_url: format!("http://{gui}"),
            api_key: String::new(),
        };

        // Wait for config.xml to appear with an <apikey>.
        let config_path = home_dir.join("config.xml");
        let deadline = Instant::now() + STARTUP_TIMEOUT;
        st.api_key = loop {
            if let Ok(xml) = std::fs::read_to_string(&config_path) {
                if let Some(key) = parse_api_key(&xml) {
                    break key;
                }
            }
            if Instant::now() > deadline {
                bail!(t!("core.syncthing_config_timeout", path = config_path.display()));
            }
            if let Some(status) = st.child.try_wait()? {
                bail!(t!("core.syncthing_exited_early", status = status));
            }
            std::thread::sleep(Duration::from_millis(200));
        };

        // Wait for REST to answer.
        while st.ping().is_err() {
            if Instant::now() > deadline {
                bail!(t!("core.syncthing_rest_timeout", url = st.base_url));
            }
            if let Some(status) = st.child.try_wait()? {
                bail!(t!("core.syncthing_exited_early", status = status));
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        Ok(st)
    }

    fn ping(&self) -> Result<()> {
        ureq::get(format!("{}/rest/system/ping", self.base_url))
            .header("X-API-Key", &self.api_key)
            .call()?;
        Ok(())
    }

    /// This daemon's device id — the value a pairing QR carries.
    pub fn device_id(&self) -> Result<String> {
        let mut res = ureq::get(format!("{}/rest/system/status", self.base_url))
            .header("X-API-Key", &self.api_key)
            .call()?;
        let status: serde_json::Value = res.body_mut().read_json()?;
        status["myID"]
            .as_str()
            .map(String::from)
            .ok_or_else(|| anyhow!(t!("core.syncthing_no_my_id")))
    }

    /// Registers the vault directory as a shared folder. An existing folder is left alone,
    /// so its peer list is not overwritten.
    pub fn ensure_folder(&self, folder_id: &str, label: &str, path: &Path) -> Result<()> {
        let url = format!("{}/rest/config/folders/{folder_id}", self.base_url);
        if ureq::get(&url).header("X-API-Key", &self.api_key).call().is_ok() {
            return Ok(()); // already registered
        }
        ureq::put(&url)
            .header("X-API-Key", &self.api_key)
            .send_json(serde_json::json!({
                "id": folder_id,
                "label": label,
                "path": path.to_string_lossy(),
                "type": "sendreceive",
                "fsWatcherEnabled": true,
                "rescanIntervalS": 60,
            }))?;
        Ok(())
    }

    /// How fast a change on one device becomes a change on the others.
    ///
    /// Two numbers, and the delay a user actually notices is their **sum with the merge
    /// interval on the receiving side**: Syncthing waits `watch_delay_s` after a write
    /// before it acts on it, ships the file, and the other app then picks it up on its own
    /// timer. With the defaults (10 + 15) a memo takes up to twenty-odd seconds to appear.
    ///
    /// `rescan_s` is the fallback sweep for changes the filesystem watcher missed, which is
    /// rare on a directory this app writes itself; it exists because a watcher can drop
    /// events under load or on filesystems that do not support them.
    ///
    /// Separate from [`Syncthing::ensure_folder`] because that one returns early on a folder
    /// that already exists — every device but a brand-new one. This is what a settings
    /// change has to go through to reach a folder that is already registered.
    pub fn set_folder_timing(&self, folder_id: &str, watch_delay_s: i32, rescan_s: i32) -> Result<()> {
        let url = format!("{}/rest/config/folders/{folder_id}", self.base_url);
        let mut res = ureq::get(&url).header("X-API-Key", &self.api_key).call()?;
        let mut folder: serde_json::Value = res.body_mut().read_json()?;

        // Nothing to say if the daemon already agrees: a PUT restarts the folder, which
        // interrupts a transfer in progress, and this is called on every settings save.
        let same = folder["fsWatcherDelayS"].as_i64() == Some(watch_delay_s as i64)
            && folder["rescanIntervalS"].as_i64() == Some(rescan_s as i64);
        if same {
            return Ok(());
        }
        folder["fsWatcherEnabled"] = serde_json::json!(true);
        folder["fsWatcherDelayS"] = serde_json::json!(watch_delay_s);
        folder["rescanIntervalS"] = serde_json::json!(rescan_s);
        ureq::put(&url).header("X-API-Key", &self.api_key).send_json(&folder)?;
        Ok(())
    }

    /// Turns on Syncthing's file versioning for the vault folder, if it has none.
    ///
    /// **This is a backup, not the history.** A memo's past lives in the change logs and is
    /// read from there ([`crate::history`]). What this protects against is the other kind of
    /// loss: a log truncated by a full disk or a crash syncs that truncation to every device,
    /// and the records past the cut are gone everywhere at once. A kept copy is the only way
    /// back from that.
    ///
    /// Staggered rather than the simpler schemes, because logs are appended to constantly:
    /// every sync of a changed log would archive the version before it, and a scheme that
    /// keeps them all would outgrow the vault. Staggered thins as versions age — hourly for
    /// a day, daily for a month — so the cost stays bounded.
    ///
    /// Versions live in `.stversions` inside the folder, which Syncthing does not sync, so
    /// each device keeps its own and nothing new travels between them.
    pub fn ensure_versioning(&self, folder_id: &str) -> Result<()> {
        let url = format!("{}/rest/config/folders/{folder_id}", self.base_url);
        let mut res = ureq::get(&url).header("X-API-Key", &self.api_key).call()?;
        let mut folder: serde_json::Value = res.body_mut().read_json()?;

        // Leave a configuration the user chose alone; only an unset one is filled in.
        if folder["versioning"]["type"].as_str().is_some_and(|t| !t.is_empty()) {
            return Ok(());
        }
        folder["versioning"] = serde_json::json!({
            "type": "staggered",
            "params": { "maxAge": MAX_VERSION_AGE_SECONDS.to_string() },
            "cleanupIntervalS": 3600,
        });
        ureq::put(&url).header("X-API-Key", &self.api_key).send_json(&folder)?;
        Ok(())
    }

    /// Adds a peer and shares the folder with it — half of pairing; the peer must do the same.
    pub fn share_folder_with(&self, folder_id: &str, peer_device_id: &str) -> Result<()> {
        // 1. Register the device (harmless to overwrite).
        ureq::put(format!("{}/rest/config/devices/{peer_device_id}", self.base_url))
            .header("X-API-Key", &self.api_key)
            .send_json(serde_json::json!({ "deviceID": peer_device_id }))?;

        // 2. Add it to the folder config.
        let url = format!("{}/rest/config/folders/{folder_id}", self.base_url);
        let mut res = ureq::get(&url).header("X-API-Key", &self.api_key).call()?;
        let mut folder: serde_json::Value = res.body_mut().read_json()?;
        let devices = folder["devices"]
            .as_array_mut()
            .ok_or_else(|| anyhow!(t!("core.syncthing_no_devices")))?;
        if !devices.iter().any(|d| d["deviceID"] == peer_device_id) {
            devices.push(serde_json::json!({ "deviceID": peer_device_id }));
            ureq::put(&url).header("X-API-Key", &self.api_key).send_json(&folder)?;
        }
        Ok(())
    }

    /// Devices that tried to connect and are waiting to be allowed in, oldest request first.
    ///
    /// Syncthing keeps this list itself: an inbound connection from a device that is not in
    /// the config is refused and recorded here. Approving one is just
    /// [`Syncthing::share_folder_with`] — Syncthing drops the entry as soon as the device is
    /// configured, so nothing has to clear it afterwards.
    ///
    /// Empty is the normal state, and this is polled, so it stays a single cheap GET.
    pub fn pending_devices(&self) -> Result<Vec<PendingDevice>> {
        let mut res = ureq::get(format!("{}/rest/cluster/pending/devices", self.base_url))
            .header("X-API-Key", &self.api_key)
            .call()?;
        let body: serde_json::Value = res.body_mut().read_json()?;
        let Some(map) = body.as_object() else { return Ok(Vec::new()) };

        let mut out: Vec<PendingDevice> = map
            .iter()
            .map(|(id, v)| PendingDevice {
                id: id.clone(),
                name: v["name"].as_str().unwrap_or_default().to_string(),
                address: v["address"].as_str().unwrap_or_default().to_string(),
                time: v["time"].as_str().unwrap_or_default().to_string(),
            })
            .collect();
        // Oldest first, so a queue of requests keeps its order between polls. The timestamps
        // are RFC 3339 in UTC, which sorts correctly as text.
        out.sort_by(|a, b| a.time.cmp(&b.time).then_with(|| a.id.cmp(&b.id)));
        Ok(out)
    }

    /// Drops one pending request without allowing it.
    ///
    /// **Syncthing does not remember the refusal.** A device that keeps dialling is recorded
    /// again on its next attempt, so a front end that does not want to ask twice has to
    /// remember the answer itself.
    pub fn dismiss_pending_device(&self, device_id: &str) -> Result<()> {
        ureq::delete(format!(
            "{}/rest/cluster/pending/devices?device={device_id}",
            self.base_url
        ))
        .header("X-API-Key", &self.api_key)
        .call()?;
        Ok(())
    }

    /// Devices sharing this folder, minus ourselves, joined with their configured name and
    /// current connection state.
    pub fn shared_devices(&self, folder_id: &str) -> Result<Vec<SharedDevice>> {
        let my_id = self.device_id()?;

        // Device ids attached to the folder.
        let mut res = ureq::get(format!("{}/rest/config/folders/{folder_id}", self.base_url))
            .header("X-API-Key", &self.api_key)
            .call()?;
        let folder: serde_json::Value = res.body_mut().read_json()?;
        let ids: Vec<String> = folder["devices"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|d| d["deviceID"].as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();

        // User-assigned labels, if any.
        let mut res = ureq::get(format!("{}/rest/config/devices", self.base_url))
            .header("X-API-Key", &self.api_key)
            .call()?;
        let devices: serde_json::Value = res.body_mut().read_json()?;
        let name_of = |id: &str| -> String {
            devices
                .as_array()
                .and_then(|a| a.iter().find(|d| d["deviceID"] == id))
                .and_then(|d| d["name"].as_str())
                .unwrap_or("")
                .to_string()
        };

        // Current connection state.
        let mut res = ureq::get(format!("{}/rest/system/connections", self.base_url))
            .header("X-API-Key", &self.api_key)
            .call()?;
        let conns: serde_json::Value = res.body_mut().read_json()?;

        let mut out = Vec::new();
        for id in ids {
            if id == my_id {
                continue; // never list ourselves
            }
            let connected = conns["connections"][&id]["connected"].as_bool().unwrap_or(false);
            let name = name_of(&id);
            out.push(SharedDevice { id, name, connected });
        }
        Ok(out)
    }

    /// Drops a peer from the folder and deletes its device config, cutting the link.
    ///
    /// Ourselves cannot be removed. This side stops sending and receiving immediately; the
    /// peer must do the same for the link to be gone on both ends.
    pub fn unshare_folder_with(&self, folder_id: &str, peer_device_id: &str) -> Result<()> {
        if peer_device_id == self.device_id()? {
            bail!(t!("core.cannot_unshare_self"));
        }
        // 1. Remove it from the folder's device list.
        let url = format!("{}/rest/config/folders/{folder_id}", self.base_url);
        let mut res = ureq::get(&url).header("X-API-Key", &self.api_key).call()?;
        let mut folder: serde_json::Value = res.body_mut().read_json()?;
        if let Some(devices) = folder["devices"].as_array_mut() {
            devices.retain(|d| d["deviceID"] != peer_device_id);
        }
        ureq::put(&url).header("X-API-Key", &self.api_key).send_json(&folder)?;

        // 2. Remove the device config too (harmless if absent).
        let _ = ureq::delete(format!("{}/rest/config/devices/{peer_device_id}", self.base_url))
            .header("X-API-Key", &self.api_key)
            .call();
        Ok(())
    }

    /// Stops sharing the folder and forgets every peer attached to it.
    ///
    /// The step that has to come before a local wipe: Syncthing propagates deletions, so
    /// emptying a folder it still carries empties it on every paired device too. Removing
    /// the folder first turns the wipe into a purely local act.
    ///
    /// The peers are dropped as well, since a vault that is about to disappear should not
    /// leave this device configured to receive it back the moment a new one is created.
    pub fn remove_folder(&self, folder_id: &str) -> Result<()> {
        let url = format!("{}/rest/config/folders/{folder_id}", self.base_url);
        let peers: Vec<String> = match ureq::get(&url).header("X-API-Key", &self.api_key).call() {
            Ok(mut res) => {
                let folder: serde_json::Value = res.body_mut().read_json()?;
                folder["devices"]
                    .as_array()
                    .map(|a| a.iter().filter_map(|d| d["deviceID"].as_str().map(String::from)).collect())
                    .unwrap_or_default()
            }
            Err(_) => return Ok(()), // never registered, nothing to remove
        };

        ureq::delete(&url).header("X-API-Key", &self.api_key).call()?;

        // Best effort: an undeletable peer must not stop the folder from going away.
        let my_id = self.device_id().unwrap_or_default();
        for id in peers.iter().filter(|id| **id != my_id) {
            let _ = ureq::delete(format!("{}/rest/config/devices/{id}", self.base_url))
                .header("X-API-Key", &self.api_key)
                .call();
        }
        Ok(())
    }

    /// Shuts the daemon down. `Drop` does this too, but calling it surfaces errors.
    pub fn shutdown(mut self) -> Result<()> {
        self.shutdown_inner()
    }

    fn shutdown_inner(&mut self) -> Result<()> {
        let _ = ureq::post(format!("{}/rest/system/shutdown", self.base_url))
            .header("X-API-Key", &self.api_key)
            .send_empty();
        // Give it a moment, then kill.
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if self.child.try_wait()?.is_some() {
                return Ok(());
            }
            if Instant::now() > deadline {
                self.child.kill().ok();
                self.child.wait()?;
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

impl Drop for Syncthing {
    fn drop(&mut self) {
        let _ = self.shutdown_inner();
    }
}

/// Windows: put the daemon in a job object that kills its members once the last handle to it
/// closes — which the kernel does for us when this process ends, however it ends.
///
/// `Drop` only covers a clean exit. An installer, Task Manager or a crash terminates us
/// without running any of it, and the orphaned daemon then keeps `ymemo-sync.exe` locked, so
/// installing or uninstalling over it fails or demands a reboot. The job closes that hole;
/// the Linux counterpart is `PR_SET_PDEATHSIG` in [`Syncthing::spawn`].
///
/// `None` when the job cannot be created (an old Windows nested-job restriction, say): the
/// daemon then behaves as before rather than the app failing to start.
#[cfg(windows)]
fn kill_with_parent(child: &Child) -> Option<JobHandle> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, SetInformationJobObject,
        JobObjectExtendedLimitInformation, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };

    unsafe {
        let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
        if job.is_null() {
            return None;
        }
        let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let ok = SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            std::ptr::addr_of!(info).cast(),
            std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        ) != 0
            && AssignProcessToJobObject(job, child.as_raw_handle() as _) != 0;
        if !ok {
            CloseHandle(job);
            return None;
        }
        Some(JobHandle(job))
    }
}

/// Owns the job handle from [`kill_with_parent`]; closing it kills the daemon.
#[cfg(windows)]
pub struct JobHandle(windows_sys::Win32::Foundation::HANDLE);

// A job handle is just a kernel handle, safe to move between threads. The raw pointer inside
// is what makes the compiler doubt it.
#[cfg(windows)]
unsafe impl Send for JobHandle {}

#[cfg(windows)]
impl Drop for JobHandle {
    fn drop(&mut self) {
        unsafe { windows_sys::Win32::Foundation::CloseHandle(self.0) };
    }
}

/// Pulls `<apikey>` out of config.xml — enough without an XML parser dependency.
fn parse_api_key(xml: &str) -> Option<String> {
    let start = xml.find("<apikey>")? + "<apikey>".len();
    let end = xml[start..].find("</apikey>")? + start;
    let key = xml[start..end].trim();
    (!key.is_empty()).then(|| key.to_string())
}

/// Picks a free local port.
fn free_port() -> Result<u16> {
    Ok(std::net::TcpListener::bind("127.0.0.1:0")?.local_addr()?.port())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_api_key_from_config_xml() {
        let xml = r#"<configuration version="37">
            <gui enabled="true" tls="false">
                <address>127.0.0.1:8384</address>
                <apikey>abcDEF123456</apikey>
            </gui>
        </configuration>"#;
        assert_eq!(parse_api_key(xml).as_deref(), Some("abcDEF123456"));
        assert_eq!(parse_api_key("<gui></gui>"), None);
        assert_eq!(parse_api_key("<apikey></apikey>"), None);
    }

    /// Round-trip against a real daemon; skipped when no binary is available.
    #[test]
    fn spawn_configure_shutdown() {
        let Some(bin) = Syncthing::find_binary() else {
            eprintln!("skip: no syncthing binary");
            return;
        };
        let home = std::env::temp_dir().join(format!("ymemo-st-{}", uuid::Uuid::new_v4()));
        let folder = std::env::temp_dir().join(format!("ymemo-vault-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&folder).unwrap();

        let st = Syncthing::spawn(&bin, &home).unwrap();
        let id = st.device_id().unwrap();
        assert!(!id.is_empty(), "empty device id");

        st.ensure_folder("ymemo-vault", "Ymemo Vault", &folder).unwrap();
        st.ensure_folder("ymemo-vault", "Ymemo Vault", &folder).unwrap(); // idempotent

        st.shutdown().unwrap();
        std::fs::remove_dir_all(&home).ok();
        std::fs::remove_dir_all(&folder).ok();
    }
}
