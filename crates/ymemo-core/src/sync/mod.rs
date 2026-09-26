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
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};
use ymemo_i18n::t;

mod folder;
mod peers;

/// How long to wait for a first start, key generation included.
const STARTUP_TIMEOUT: Duration = Duration::from_secs(60);
/// Longest any one REST call may take. Generous for a daemon on localhost — a config PUT
/// can restart parts of it — and short enough that a stuck one is a hiccup, not a hang.
const REST_TIMEOUT: Duration = Duration::from_secs(10);

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
    /// Every REST call goes through this, for its timeout. Several of them run on the
    /// desktop's UI thread (the merge timer applies revocations), so a daemon that stopped
    /// answering froze the whole app for as long as it stayed stuck — ureq's own default
    /// has no timeout at all.
    agent: ureq::Agent,
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
            agent: ureq::Agent::new_with_config(
                ureq::Agent::config_builder().timeout_global(Some(REST_TIMEOUT)).build(),
            ),
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

        // Wait for REST to answer us specifically.
        while st.ping().is_err() {
            if Instant::now() > deadline {
                bail!(t!("core.syncthing_rest_timeout", url = st.base_url));
            }
            if let Some(status) = st.child.try_wait()? {
                bail!(t!("core.syncthing_exited_early", status = status));
            }
            std::thread::sleep(Duration::from_millis(200));
            // Re-read the key on every turn. The daemon writes config.xml as it starts, and a
            // key read a moment too early — or one left by a previous run that this start
            // replaced — is refused by everything afterwards. Picking the new one up here is
            // the difference between sync working and sync silently not.
            if let Some(key) = std::fs::read_to_string(&config_path).ok().and_then(|x| parse_api_key(&x)) {
                st.api_key = key;
            }
        }

        // Best effort: a daemon that will not take this still syncs, and refusing to start
        // over a privacy default we merely prefer would be the wrong trade.
        if let Err(e) = st.disable_crash_reporting() {
            crate::diag!("could not turn syncthing's crash reporting off: {e}");
        }
        Ok(st)
    }

    /// Turns off Syncthing's own crash reporting.
    ///
    /// **Not a setting, a correction.** Syncthing enables it by default and posts crashes to
    /// `crash.syncthing.net`, which is a third party this app never told anyone about — and
    /// an app whose whole claim is that nothing of yours leaves your devices cannot ship a
    /// component that quietly phones home. Nobody would turn this back on, so it is not
    /// offered as a choice.
    ///
    /// It runs at the end of [`Syncthing::spawn`], which leaves a window: a crash during
    /// those first few seconds is still reportable. Closing that would mean writing
    /// `config.xml` before the daemon's first start, and the daemon is what creates it.
    fn disable_crash_reporting(&self) -> Result<()> {
        let url = format!("{}/rest/config/options", self.base_url);
        let mut res = self.agent.get(&url).header("X-API-Key", &self.api_key).call()?;
        let mut options: serde_json::Value = res.body_mut().read_json()?;
        if options["crashReportingEnabled"] == serde_json::json!(false) {
            return Ok(()); // already off, and a PUT here restarts more than it needs to
        }
        options["crashReportingEnabled"] = serde_json::json!(false);
        self.agent.put(&url).header("X-API-Key", &self.api_key).send_json(&options)?;
        Ok(())
    }

    /// Whether the daemon is up **and** accepts our API key.
    ///
    /// It used to call `/rest/system/ping` and treat any answer as success — and a refusal is
    /// an answer. With a key the daemon did not accept, startup looked healthy and then every
    /// call failed with `json: expected value at line 1 column 1`: the plain-text "missing or
    /// invalid authentication code" body, being parsed as JSON. The app went on to report
    /// sync as unavailable, blaming a missing binary for an authentication problem, and left
    /// the daemon it had spawned running. Reading a field out of the response proves both
    /// halves at once.
    fn ping(&self) -> Result<()> {
        let mut res = self.agent.get(format!("{}/rest/system/status", self.base_url))
            .header("X-API-Key", &self.api_key)
            .call()?;
        let status: serde_json::Value = res.body_mut().read_json()?;
        if status["myID"].as_str().is_none_or(str::is_empty) {
            bail!(t!("core.syncthing_no_my_id"));
        }
        Ok(())
    }

    /// This daemon's device id — the value a pairing QR carries.
    pub fn device_id(&self) -> Result<String> {
        let mut res = self.agent.get(format!("{}/rest/system/status", self.base_url))
            .header("X-API-Key", &self.api_key)
            .call()?;
        let status: serde_json::Value = res.body_mut().read_json()?;
        status["myID"]
            .as_str()
            .map(String::from)
            .ok_or_else(|| anyhow!(t!("core.syncthing_no_my_id")))
    }

    /// Shuts the daemon down. `Drop` does this too, but calling it surfaces errors.
    pub fn shutdown(mut self) -> Result<()> {
        self.shutdown_inner()
    }

    fn shutdown_inner(&mut self) -> Result<()> {
        let _ = self.agent.post(format!("{}/rest/system/shutdown", self.base_url))
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
