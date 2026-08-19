//! Syncthing transport: run the bundled binary as a child process and drive it over REST.
//!
//! Syncthing is bundled whole, not reimplemented or embedded. This module only moves
//! files: register the vault directory as a shared folder and Syncthing propagates it
//! between devices, while `Vault::rebuild` does the merging. Logs are per-device and
//! append-only so files never conflict, and their contents are already E2E encrypted, so
//! the transport does not have to be trusted.

use anyhow::{anyhow, bail, Context, Result};
use ymemo_i18n::t;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// How long to wait for a first start, key generation included.
const STARTUP_TIMEOUT: Duration = Duration::from_secs(60);

/// A running Syncthing child process plus its REST client. Dropping it shuts the daemon
/// down (REST shutdown first, then kill).
pub struct Syncthing {
    child: Child,
    base_url: String,
    api_key: String,
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
        let child = cmd
            .spawn()
            .with_context(|| t!("core.syncthing_spawn_failed", path = binary.display()))?;

        let mut st = Self {
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
