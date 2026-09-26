//! The devices the folder is shared with: adding one, making every peer an introducer,
//! removing one for good, and the requests from devices this one has not met.

use anyhow::{anyhow, bail, Result};
use ymemo_i18n::t;

use super::{PendingDevice, SharedDevice, Syncthing};

impl Syncthing {
    /// Adds a peer and shares the folder with it — half of pairing; the peer must do the same.
    pub fn share_folder_with(&self, folder_id: &str, peer_device_id: &str) -> Result<()> {
        // 1. Register the device, as an introducer.
        self.upsert_peer(peer_device_id)?;

        // 2. Add it to the folder config.
        let url = format!("{}/rest/config/folders/{folder_id}", self.base_url);
        let mut res = self.agent.get(&url).header("X-API-Key", &self.api_key).call()?;
        let mut folder: serde_json::Value = res.body_mut().read_json()?;
        let devices = folder["devices"]
            .as_array_mut()
            .ok_or_else(|| anyhow!(t!("core.syncthing_no_devices")))?;
        if !devices.iter().any(|d| d["deviceID"] == peer_device_id) {
            devices.push(serde_json::json!({ "deviceID": peer_device_id }));
            self.agent.put(&url).header("X-API-Key", &self.api_key).send_json(&folder)?;
        }
        Ok(())
    }

    /// Registers a peer, or updates the one already there, **as an introducer**.
    ///
    /// The flag is what makes a vault with three devices in it a mesh rather than a star.
    /// Pairing is always between two devices — one shows a code, the other scans it — so
    /// without introduction a third device is known only to whichever device it was paired
    /// with. The other two hold the same vault and have never heard of each other, and a memo
    /// written on one waits for the middle device to be switched on before it reaches the
    /// last. With it, each device passes on the peers it shares the vault with and the
    /// missing links fill themselves in.
    ///
    /// It is not a new grant of trust: a device that another device let in already holds the
    /// vault and its data key, so it can already read everything. What changes is only
    /// whether the two talk directly or through the device that introduced them.
    ///
    /// Read-modify-write, because a PUT replaces the whole device entry: building one from
    /// `{deviceID}` alone would throw away the name Syncthing learned from the peer, its
    /// addresses, and whether it is paused.
    pub(super) fn upsert_peer(&self, peer_device_id: &str) -> Result<()> {
        let url = format!("{}/rest/config/devices/{peer_device_id}", self.base_url);
        let mut device: serde_json::Value =
            match self.agent.get(&url).header("X-API-Key", &self.api_key).call() {
                Ok(mut res) => res.body_mut().read_json()?,
                // Not registered yet, which is the usual case here.
                Err(_) => serde_json::json!({ "deviceID": peer_device_id }),
            };
        let awake = device["introducer"] == serde_json::json!(true)
            && device["paused"] != serde_json::json!(true);
        if awake {
            // Already as we want it. Saying so again would restart the connection to a peer
            // that may be in the middle of a transfer, and this runs on every start.
            return Ok(());
        }
        device["introducer"] = serde_json::json!(true);
        // Wakes a peer that was parked by a removal. A removed device is kept paused rather
        // than deleted (see `apply_revocations`), so without this, pairing with it again put
        // it back in the folder and then never dialled it: the reconnection silently did
        // nothing, which is the one thing worse than refusing.
        device["paused"] = serde_json::json!(false);
        self.agent.put(&url).header("X-API-Key", &self.api_key).send_json(&device)?;
        Ok(())
    }

    /// Marks every peer already sharing the folder as an introducer.
    ///
    /// For the devices that were paired before the app asked for introductions; a pairing made
    /// since gets the flag from [`Syncthing::share_folder_with`]. Idempotent and silent when
    /// nothing has to change, so it belongs on every start rather than behind a migration
    /// flag — there is no record of which version paired a given device.
    ///
    /// Does nothing when the folder is not registered, which is a device that has not
    /// unlocked a vault yet.
    pub fn ensure_introducers(&self, folder_id: &str) -> Result<()> {
        let url = format!("{}/rest/config/folders/{folder_id}", self.base_url);
        let Ok(mut res) = self.agent.get(&url).header("X-API-Key", &self.api_key).call() else {
            return Ok(());
        };
        let folder: serde_json::Value = res.body_mut().read_json()?;
        let my_id = self.device_id()?;
        let peers: Vec<String> = folder["devices"]
            .as_array()
            .map(|a| a.iter().filter_map(|d| d["deviceID"].as_str().map(String::from)).collect())
            .unwrap_or_default();
        for peer in peers.iter().filter(|id| **id != my_id) {
            self.upsert_peer(peer)?;
        }
        Ok(())
    }

    /// Names this device, which is what the other devices show in their list.
    ///
    /// Syncthing names a device after its hostname and announces that to its peers. On a
    /// desktop that is the machine's name and exactly right; on Android the hostname is
    /// **`localhost`**, so every phone anyone paired arrived under the same label. This lets
    /// the platform say something better.
    ///
    /// Only ever raises the name over the daemon's own default, and only for peers that learn
    /// it **after** it is set: Syncthing keeps the name it first learned for a device
    /// (`overwriteRemoteDeviceNamesOnConnect` is off by default), so a pairing made before
    /// this keeps whatever it recorded then. Which is why it runs at startup rather than at
    /// pairing time — the name is in place before any peer asks.
    pub fn set_my_name(&self, name: &str) -> Result<()> {
        let name = name.trim();
        if name.is_empty() {
            return Ok(());
        }
        let my_id = self.device_id()?;
        let url = format!("{}/rest/config/devices/{my_id}", self.base_url);
        let mut res = self.agent.get(&url).header("X-API-Key", &self.api_key).call()?;
        let mut device: serde_json::Value = res.body_mut().read_json()?;
        if device["name"] == serde_json::json!(name) {
            return Ok(()); // already so; a PUT would restart the connections
        }
        device["name"] = serde_json::json!(name);
        self.agent.put(&url).header("X-API-Key", &self.api_key).send_json(&device)?;
        Ok(())
    }

    /// Stops sharing the folder with every device the vault says has been removed.
    ///
    /// The other half of making a removal stick. Every peer is an introducer, so a device
    /// dropped on one machine alone is handed back by the machines that still have it; what
    /// stops that is every machine applying the same list, which is why the list travels in
    /// the vault (see [`crate::RevokedDevice`]). Once they all have, nobody offers the device
    /// and there is nothing left to introduce.
    ///
    /// Returns how many it had to act on, so a caller can log a removal it did not make.
    ///
    /// **Paused, not deleted.** Dropping the device outright does not settle: the peers apply
    /// the list at slightly different moments, and whichever has not yet dropped it hands it
    /// back to the one that has, which hands it back in turn. Measured against v2.1.2, that
    /// ping-pong runs for as long as you care to watch — it never converges. A paused entry
    /// is the tombstone that does settle: introduction may put the device back in the folder,
    /// but a paused device is never dialled and never answers, so nothing flows either way.
    /// [`Syncthing::shared_devices`] hides them, so the list matches what is really shared.
    ///
    /// Idempotent and cheap when there is nothing to do, which is the normal case: it runs
    /// after every merge.
    pub fn apply_revocations(&self, folder_id: &str, revoked: &[String]) -> Result<usize> {
        if revoked.is_empty() {
            return Ok(0);
        }
        let my_id = self.device_id()?;
        let mut acted = 0;
        for id in revoked.iter().filter(|id| **id != my_id) {
            if self.set_device_paused(id, true)? {
                acted += 1;
            }
            acted += usize::from(self.drop_from_folder(folder_id, id)?);
        }
        Ok(acted)
    }

    /// Pauses or resumes a peer, creating the entry if it is not there. Returns whether
    /// anything changed, so the caller can stay quiet when it did not.
    ///
    /// A paused device is kept in the configuration on purpose: it is what a re-introduction
    /// lands on instead of creating a live peer. See [`Syncthing::apply_revocations`].
    pub(super) fn set_device_paused(&self, device_id: &str, paused: bool) -> Result<bool> {
        let url = format!("{}/rest/config/devices/{device_id}", self.base_url);
        let mut device: serde_json::Value =
            match self.agent.get(&url).header("X-API-Key", &self.api_key).call() {
                Ok(mut res) => res.body_mut().read_json()?,
                Err(_) => serde_json::json!({ "deviceID": device_id }),
            };
        if device["paused"] == serde_json::json!(paused) {
            return Ok(false);
        }
        device["paused"] = serde_json::json!(paused);
        // A device that is not to be talked to is not one to take introductions from either.
        if paused {
            device["introducer"] = serde_json::json!(false);
        }
        self.agent.put(&url).header("X-API-Key", &self.api_key).send_json(&device)?;
        Ok(true)
    }

    /// Takes a device out of the folder's peer list, leaving its device entry alone.
    /// Returns whether it was there.
    pub(super) fn drop_from_folder(&self, folder_id: &str, device_id: &str) -> Result<bool> {
        let url = format!("{}/rest/config/folders/{folder_id}", self.base_url);
        let Ok(mut res) = self.agent.get(&url).header("X-API-Key", &self.api_key).call() else {
            return Ok(false); // no folder: nothing shared with anyone
        };
        let mut folder: serde_json::Value = res.body_mut().read_json()?;
        let Some(devices) = folder["devices"].as_array_mut() else { return Ok(false) };
        let before = devices.len();
        devices.retain(|d| d["deviceID"] != device_id);
        if devices.len() == before {
            return Ok(false);
        }
        self.agent.put(&url).header("X-API-Key", &self.api_key).send_json(&folder)?;
        Ok(true)
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
        let mut res = self.agent.get(format!("{}/rest/cluster/pending/devices", self.base_url))
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
        self.agent.delete(format!(
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
        let mut res = self.agent.get(format!("{}/rest/config/folders/{folder_id}", self.base_url))
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
        let mut res = self.agent.get(format!("{}/rest/config/devices", self.base_url))
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
        // A removed device is kept in the configuration, paused, so that a re-introduction
        // lands on a peer that is never dialled rather than creating a live one — see
        // `apply_revocations`. It is not shared with anybody, so it does not belong in a list
        // of the devices this vault is shared with.
        let paused = |id: &str| -> bool {
            devices
                .as_array()
                .and_then(|a| a.iter().find(|d| d["deviceID"] == id))
                .and_then(|d| d["paused"].as_bool())
                .unwrap_or(false)
        };

        // Current connection state.
        let mut res = self.agent.get(format!("{}/rest/system/connections", self.base_url))
            .header("X-API-Key", &self.api_key)
            .call()?;
        let conns: serde_json::Value = res.body_mut().read_json()?;

        let mut out = Vec::new();
        for id in ids {
            if id == my_id || paused(&id) {
                continue; // never list ourselves, nor a device that has been removed
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
        let mut res = self.agent.get(&url).header("X-API-Key", &self.api_key).call()?;
        let mut folder: serde_json::Value = res.body_mut().read_json()?;
        if let Some(devices) = folder["devices"].as_array_mut() {
            devices.retain(|d| d["deviceID"] != peer_device_id);
        }
        self.agent.put(&url).header("X-API-Key", &self.api_key).send_json(&folder)?;

        // 2. Remove the device config too (harmless if absent).
        let _ = self.agent.delete(format!("{}/rest/config/devices/{peer_device_id}", self.base_url))
            .header("X-API-Key", &self.api_key)
            .call();
        Ok(())
    }
}
