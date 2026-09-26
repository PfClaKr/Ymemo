//! Removing a device from the vault, as a fact in the synced document.

use anyhow::{bail, Result};
use automerge::{transaction::Transactable, ObjType, ReadDoc};
use ymemo_i18n::t;

use super::Vault;

impl Vault {
    /// Records that a device is no longer part of this vault, on every device that shares it.
    ///
    /// Idempotent, and never a no-op on the timestamp alone: re-revoking an already revoked
    /// device would otherwise write a change per call, and the callers poll.
    pub fn revoke_device(&mut self, device_id: &str) -> Result<()> {
        if device_id == self.device_id {
            bail!(t!("core.cannot_unshare_self"));
        }
        let revoked = self.revoked_obj()?;
        if self.doc.get(&revoked, device_id)?.is_some() {
            return Ok(());
        }
        let obj = self.doc.put_object(&revoked, device_id, ObjType::Map)?;
        self.doc.put(&obj, "at", crate::now_millis())?;
        self.doc.put(&obj, "by", self.device_id.clone())?;
        self.append_local_change()?;
        self.materialize_revoked()
    }

    /// Takes a device off the removed list, which is what pairing with it again means.
    ///
    /// Without this a mis-click would be permanent: the peers would go on refusing a device
    /// the user has since decided to keep, and no amount of re-pairing would stick.
    pub fn unrevoke_device(&mut self, device_id: &str) -> Result<()> {
        let revoked = self.revoked_obj()?;
        if self.doc.get(&revoked, device_id)?.is_none() {
            return Ok(());
        }
        self.doc.delete(&revoked, device_id)?;
        self.append_local_change()?;
        self.materialize_revoked()
    }

    /// Every device the vault says has been removed, oldest first.
    pub fn revoked_devices(&self) -> Result<Vec<crate::RevokedDevice>> {
        self.store.list_revoked()
    }

    /// Whether the vault says **this** device has been removed.
    ///
    /// A device that finds itself here stops syncing rather than fighting the others for the
    /// folder. It keeps what it already has: taking a user's memos off their own machine
    /// because another machine said so is a different act, and not one this promises.
    pub fn is_revoked_here(&self) -> Result<bool> {
        Ok(self.revoked_devices()?.iter().any(|d| d.device_id == self.device_id))
    }
}
