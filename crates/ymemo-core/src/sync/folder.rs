//! The shared folder itself: creating it, its timings, pausing it, keeping old copies,
//! and removing it.

use anyhow::Result;
use std::path::Path;

use super::Syncthing;

impl Syncthing {
    /// Registers the vault directory as a shared folder. An existing folder is left alone,
    /// so its peer list is not overwritten.
    pub fn ensure_folder(&self, folder_id: &str, label: &str, path: &Path) -> Result<()> {
        let url = format!("{}/rest/config/folders/{folder_id}", self.base_url);
        if self.agent.get(&url).header("X-API-Key", &self.api_key).call().is_ok() {
            return Ok(()); // already registered
        }
        self.agent.put(&url)
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
        let mut res = self.agent.get(&url).header("X-API-Key", &self.api_key).call()?;
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
        self.agent.put(&url).header("X-API-Key", &self.api_key).send_json(&folder)?;
        Ok(())
    }

    /// Pauses or resumes the vault folder.
    ///
    /// Pausing stops file transfer while leaving the daemon and its device connections up, so
    /// pairing still works and syncing resumes the instant it is lifted. That is the right
    /// granularity for "only on Wi-Fi": stopping the daemon instead would drop the
    /// connections and make coming back slow, for a saving of a few keepalive bytes.
    pub fn set_folder_paused(&self, folder_id: &str, paused: bool) -> Result<()> {
        let url = format!("{}/rest/config/folders/{folder_id}", self.base_url);
        let mut res = self.agent.get(&url).header("X-API-Key", &self.api_key).call()?;
        let mut folder: serde_json::Value = res.body_mut().read_json()?;
        if folder["paused"] == serde_json::json!(paused) {
            return Ok(()); // a PUT restarts the folder; this is called on every network change
        }
        folder["paused"] = serde_json::json!(paused);
        self.agent.put(&url).header("X-API-Key", &self.api_key).send_json(&folder)?;
        Ok(())
    }

    /// Sets how long Syncthing keeps its own copies of replaced files, in days; 0 turns the
    /// copies off entirely.
    ///
    /// **This is a backup, not the history.** A memo's past lives in the change logs and is
    /// read from there ([`crate::history`]). What this protects against is the other kind of
    /// loss: a log truncated by a full disk or a crash syncs that truncation to every device,
    /// and the records past the cut are gone everywhere at once. A kept copy is the only way
    /// back from that.
    ///
    /// Staggered rather than the simpler schemes, because logs are appended to constantly:
    /// every sync of a changed log would otherwise archive the version before it, and a scheme
    /// that keeps them all would outgrow the vault. Staggered thins as versions age — hourly
    /// for a day, daily for a month — so the cost stays bounded but is **not small**: the
    /// archive is a series of snapshots of a file that only ever grows, which is why how long
    /// to keep them is the user's call and not a constant.
    ///
    /// Versions live in `.stversions` inside the folder, which Syncthing does not sync, so
    /// each device keeps its own and nothing new travels between them.
    pub fn set_folder_versioning(&self, folder_id: &str, keep_days: i32) -> Result<()> {
        let url = format!("{}/rest/config/folders/{folder_id}", self.base_url);
        let mut res = self.agent.get(&url).header("X-API-Key", &self.api_key).call()?;
        let mut folder: serde_json::Value = res.body_mut().read_json()?;

        let want = if keep_days <= 0 {
            serde_json::json!({ "type": "", "params": {}, "cleanupIntervalS": 3600 })
        } else {
            let max_age = keep_days as u64 * 24 * 60 * 60;
            serde_json::json!({
                "type": "staggered",
                "params": { "maxAge": max_age.to_string() },
                "cleanupIntervalS": 3600,
            })
        };
        // Skip the PUT when it already matches: this runs on every settings save, and a PUT
        // restarts the folder.
        if folder["versioning"]["type"] == want["type"]
            && folder["versioning"]["params"]["maxAge"] == want["params"]["maxAge"]
        {
            return Ok(());
        }
        folder["versioning"] = want;
        self.agent.put(&url).header("X-API-Key", &self.api_key).send_json(&folder)?;
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
        let peers: Vec<String> = match self.agent.get(&url).header("X-API-Key", &self.api_key).call() {
            Ok(mut res) => {
                let folder: serde_json::Value = res.body_mut().read_json()?;
                folder["devices"]
                    .as_array()
                    .map(|a| a.iter().filter_map(|d| d["deviceID"].as_str().map(String::from)).collect())
                    .unwrap_or_default()
            }
            Err(_) => return Ok(()), // never registered, nothing to remove
        };

        self.agent.delete(&url).header("X-API-Key", &self.api_key).call()?;

        // Best effort: an undeletable peer must not stop the folder from going away.
        let my_id = self.device_id().unwrap_or_default();
        for id in peers.iter().filter(|id| **id != my_id) {
            let _ = self.agent.delete(format!("{}/rest/config/devices/{id}", self.base_url))
                .header("X-API-Key", &self.api_key)
                .call();
        }
        Ok(())
    }
}
