//! The local SQLite cache ([`Store`]): a disposable, plaintext view of the vault that
//! `Vault::rebuild` rewrites from the logs. Never a source of truth.

use anyhow::Result;
use rusqlite::{params, Connection};

use crate::{
    clamp_opacity, clamp_permille, clamp_width_em_milli, Attachment, Group, Memo, RevokedDevice,
};

/// Local SQLite memo store.
///
/// `rusqlite::Connection` is single-threaded; the desktop uses this as
/// `Rc<RefCell<Store>>` on the UI thread.
pub struct Store {
    conn: Connection,
}

impl Store {
    /// Opens (and creates if needed) a store at `path`.
    pub fn open(path: impl AsRef<std::path::Path>) -> Result<Self> {
        let store = Self {
            conn: Connection::open(path)?,
        };
        // SQLite's defaults are chosen for a database somebody would miss. This one is a
        // **disposable view** of the vault — `Vault::rebuild` throws it away and writes it
        // again from the logs — so the durability the defaults buy is paid for and never
        // used. `DELETE` + `FULL` means an fsync per statement, and the rebuild that runs on
        // the merge timer writes one statement per memo, folder, photo and removal: on an
        // ordinary vault that was a quarter of a second of fsyncs, on the UI thread, every
        // fifteen seconds. Measured.
        store.conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;",
        )?;
        store.init()?;
        Ok(store)
    }

    /// Deletes a cache file and the two sidecars WAL mode keeps beside it.
    ///
    /// `ymemo.db-wal` holds writes that have not been folded back into the database yet.
    /// Deleting the database and leaving that behind is how a reset gives back the memos it
    /// was asked to destroy, so the three always go together.
    pub fn delete_file(path: impl AsRef<std::path::Path>) -> Result<()> {
        let path = path.as_ref();
        for p in [
            path.to_path_buf(),
            path.with_extension(format!(
                "{}-wal",
                path.extension().and_then(|e| e.to_str()).unwrap_or_default()
            )),
            path.with_extension(format!(
                "{}-shm",
                path.extension().and_then(|e| e.to_str()).unwrap_or_default()
            )),
        ] {
            if p.exists() {
                std::fs::remove_file(&p)?;
            }
        }
        Ok(())
    }

    /// In-memory store, for tests.
    pub fn open_in_memory() -> Result<Self> {
        let store = Self {
            conn: Connection::open_in_memory()?,
        };
        store.init()?;
        Ok(store)
    }

    fn init(&self) -> Result<()> {
        self.conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS memos (
                id         TEXT PRIMARY KEY,
                title      TEXT NOT NULL,
                body       TEXT NOT NULL,
                color      TEXT NOT NULL DEFAULT 'yellow',
                opacity    INTEGER NOT NULL DEFAULT 100,
                group_id   TEXT NOT NULL DEFAULT '',
                order_key  TEXT NOT NULL DEFAULT '',
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL
            );
            -- Folders; parent_id nests them (empty = top level).
            CREATE TABLE IF NOT EXISTS groups (
                id         TEXT PRIMARY KEY,
                name       TEXT NOT NULL,
                parent_id  TEXT NOT NULL DEFAULT '',
                color      TEXT NOT NULL DEFAULT 'yellow',
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL
            );
            -- Photos on a memo. The bytes live in vault blobs/; this is just a reference.
            CREATE TABLE IF NOT EXISTS attachments (
                id             TEXT PRIMARY KEY,
                memo_id        TEXT NOT NULL,
                hash           TEXT NOT NULL,
                name           TEXT NOT NULL DEFAULT '',
                mime           TEXT NOT NULL DEFAULT '',
                width_px       INTEGER NOT NULL DEFAULT 0,
                height_px      INTEGER NOT NULL DEFAULT 0,
                width_em_milli INTEGER NOT NULL DEFAULT 20000,
                x_permille     INTEGER NOT NULL DEFAULT 40,
                y_permille     INTEGER NOT NULL DEFAULT 40,
                mode           TEXT NOT NULL DEFAULT '',
                anchor_line    INTEGER NOT NULL DEFAULT 0,
                created_at     INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS attachments_memo ON attachments(memo_id);
            -- Devices the user has removed from the vault, as the **synced document** says.
            -- Unlike `meta` below this is not device-local: it is materialized from the logs
            -- like memos are, which is the whole point — a removal made on one device has to
            -- reach the others, or they introduce the device straight back.
            CREATE TABLE IF NOT EXISTS revoked (
                device_id TEXT PRIMARY KEY,
                at        INTEGER NOT NULL,
                by        TEXT NOT NULL DEFAULT ''
            );
            -- Device-local metadata (device_id, ...). Never synced.
            CREATE TABLE IF NOT EXISTS meta (
                key   TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );",
        )?;
        // Migration: add columns introduced after the initial schema, so an old cache opens
        // without a full rebuild. A new column goes both in the CREATE TABLE above and here.
        for (table, name, ddl) in [
            ("memos", "color", "ALTER TABLE memos ADD COLUMN color TEXT NOT NULL DEFAULT 'yellow'"),
            ("memos", "opacity", "ALTER TABLE memos ADD COLUMN opacity INTEGER NOT NULL DEFAULT 100"),
            ("memos", "group_id", "ALTER TABLE memos ADD COLUMN group_id TEXT NOT NULL DEFAULT ''"),
            ("memos", "order_key", "ALTER TABLE memos ADD COLUMN order_key TEXT NOT NULL DEFAULT ''"),
            ("groups", "color", "ALTER TABLE groups ADD COLUMN color TEXT NOT NULL DEFAULT 'yellow'"),
            (
                "attachments",
                "x_permille",
                "ALTER TABLE attachments ADD COLUMN x_permille INTEGER NOT NULL DEFAULT 40",
            ),
            (
                "attachments",
                "y_permille",
                "ALTER TABLE attachments ADD COLUMN y_permille INTEGER NOT NULL DEFAULT 40",
            ),
            (
                "attachments",
                "mode",
                "ALTER TABLE attachments ADD COLUMN mode TEXT NOT NULL DEFAULT ''",
            ),
            (
                "attachments",
                "anchor_line",
                "ALTER TABLE attachments ADD COLUMN anchor_line INTEGER NOT NULL DEFAULT 0",
            ),
        ] {
            let exists = self
                .conn
                .prepare("SELECT 1 FROM pragma_table_info(?1) WHERE name = ?2")?
                .exists(params![table, name])?;
            if !exists {
                self.conn.execute(ddl, [])?;
            }
        }
        Ok(())
    }

    /// Unique id of this device, generated and persisted on first use.
    ///
    /// It lives in the cache, which is device-local, so it is never synced. Deleting the
    /// cache yields a new id; old logs stay and new appends just go to a new log file.
    pub fn device_id(&self) -> Result<String> {
        if let Some(id) = self.meta_get("device_id")? {
            return Ok(id);
        }
        let id = uuid::Uuid::new_v4().to_string();
        self.meta_set("device_id", &id)?;
        Ok(id)
    }

    fn meta_get(&self, key: &str) -> Result<Option<String>> {
        let mut stmt = self.conn.prepare("SELECT value FROM meta WHERE key = ?1")?;
        let mut rows = stmt.query_map([key], |row| row.get(0))?;
        Ok(rows.next().transpose()?)
    }

    fn meta_set(&self, key: &str, value: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO meta (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = ?2",
            params![key, value],
        )?;
        Ok(())
    }

    /// Empties the memo/group/attachment tables before replaying the log; keeps `meta`.
    /// Opens a transaction over the cache.
    ///
    /// Every write here is its own transaction otherwise, which is both a needless fsync each
    /// and a cache that is visibly half-written while a rebuild is running. Paired with
    /// [`Store::commit`] or [`Store::rollback`]; see `Vault::materialize`, which is the reason
    /// this exists.
    pub fn begin(&self) -> Result<()> {
        self.conn.execute_batch("BEGIN")?;
        Ok(())
    }

    /// Makes everything since [`Store::begin`] visible.
    pub fn commit(&self) -> Result<()> {
        self.conn.execute_batch("COMMIT")?;
        Ok(())
    }

    /// Throws away everything since [`Store::begin`], leaving the cache as it was.
    pub fn rollback(&self) -> Result<()> {
        self.conn.execute_batch("ROLLBACK")?;
        Ok(())
    }

    pub fn clear_memos(&self) -> Result<()> {
        self.conn.execute("DELETE FROM memos", [])?;
        self.conn.execute("DELETE FROM groups", [])?;
        self.conn.execute("DELETE FROM attachments", [])?;
        self.conn.execute("DELETE FROM revoked", [])?;
        Ok(())
    }

    /// Inserts or updates an attachment by id.
    pub fn upsert_attachment(&self, a: &Attachment) -> Result<()> {
        self.conn.execute(
            "INSERT INTO attachments
                 (id, memo_id, hash, name, mime, width_px, height_px, width_em_milli,
                  x_permille, y_permille, mode, anchor_line, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
             ON CONFLICT(id) DO UPDATE SET
                 memo_id = ?2, hash = ?3, name = ?4, mime = ?5,
                 width_px = ?6, height_px = ?7, width_em_milli = ?8,
                 x_permille = ?9, y_permille = ?10, mode = ?11, anchor_line = ?12",
            params![
                a.id,
                a.memo_id,
                a.hash,
                a.name,
                a.mime,
                a.width_px,
                a.height_px,
                clamp_width_em_milli(a.width_em_milli),
                clamp_permille(a.x_permille),
                clamp_permille(a.y_permille),
                // Normalised, so an unknown mode from a newer version is stored back as the
                // float it is being drawn as, rather than kept alive in this device's cache.
                a.mode().as_stored(),
                a.anchor_line.max(0),
                a.created_at
            ],
        )?;
        Ok(())
    }

    /// The ids of every memo that has at least one photo on it.
    ///
    /// One query for the whole list rather than one per row: a list is drawn on every merge,
    /// and a memo with nothing written on it but a picture has to say so somehow — otherwise
    /// it is a row called "(untitled)" next to another row called "(untitled)".
    pub fn memos_with_attachments(&self) -> Result<std::collections::HashSet<String>> {
        let mut stmt = self.conn.prepare("SELECT DISTINCT memo_id FROM attachments")?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        Ok(rows.collect::<rusqlite::Result<std::collections::HashSet<_>>>()?)
    }

    /// Attachments of one memo, in the order they were added.
    pub fn attachments_of(&self, memo_id: &str) -> Result<Vec<Attachment>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, memo_id, hash, name, mime, width_px, height_px, width_em_milli,
                    x_permille, y_permille, mode, anchor_line, created_at
             FROM attachments WHERE memo_id = ?1 ORDER BY created_at",
        )?;
        let rows = stmt.query_map([memo_id], row_to_attachment)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Looks up a single attachment.
    pub fn get_attachment(&self, id: &str) -> Result<Option<Attachment>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, memo_id, hash, name, mime, width_px, height_px, width_em_milli,
                    x_permille, y_permille, mode, anchor_line, created_at
             FROM attachments WHERE id = ?1",
        )?;
        let mut rows = stmt.query_map([id], row_to_attachment)?;
        Ok(rows.next().transpose()?)
    }

    /// Deletes the attachment record. **The blob file stays** (no GC — see [`crate::blob`]).
    pub fn delete_attachment(&self, id: &str) -> Result<()> {
        self.conn.execute("DELETE FROM attachments WHERE id = ?1", [id])?;
        Ok(())
    }

    /// Inserts or updates a memo by id.
    pub fn upsert(&self, memo: &Memo) -> Result<()> {
        self.conn.execute(
            "INSERT INTO memos (id, title, body, color, opacity, group_id, order_key, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
             ON CONFLICT(id) DO UPDATE SET
                 title = ?2, body = ?3, color = ?4, opacity = ?5, group_id = ?6,
                 order_key = ?7, updated_at = ?9",
            params![
                memo.id,
                memo.title,
                memo.body,
                memo.color,
                clamp_opacity(memo.opacity),
                memo.group_id,
                memo.order_key,
                memo.created_at,
                memo.updated_at
            ],
        )?;
        Ok(())
    }

    /// All memos, most recently updated first.
    pub fn list(&self) -> Result<Vec<Memo>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, title, body, color, opacity, group_id, order_key, created_at, updated_at
             -- The arrangement, with the id breaking a tie so two devices that landed on
             -- the same key still draw the folder the same way round. `updated_at` is what
             -- orders memos nobody has arranged yet: every key is empty then, and the list
             -- looks exactly as it did before folders could be arranged at all.
             FROM memos ORDER BY order_key ASC, updated_at DESC, id ASC",
        )?;
        let rows = stmt.query_map([], row_to_memo)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Looks up one memo by id.
    pub fn get(&self, id: &str) -> Result<Option<Memo>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, title, body, color, opacity, group_id, order_key, created_at, updated_at
             FROM memos WHERE id = ?1",
        )?;
        let mut rows = stmt.query_map([id], row_to_memo)?;
        Ok(rows.next().transpose()?)
    }

    // ---- groups ----

    /// Inserts or updates a group by id.
    pub fn upsert_group(&self, group: &Group) -> Result<()> {
        self.conn.execute(
            "INSERT INTO groups (id, name, parent_id, color, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(id) DO UPDATE SET name = ?2, parent_id = ?3, color = ?4, updated_at = ?6",
            params![
                group.id,
                group.name,
                group.parent_id,
                group.color,
                group.created_at,
                group.updated_at
            ],
        )?;
        Ok(())
    }

    /// All groups, sorted by name.
    pub fn list_groups(&self) -> Result<Vec<Group>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, parent_id, color, created_at, updated_at FROM groups ORDER BY name",
        )?;
        let rows = stmt.query_map([], row_to_group)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Empties the removed-device table, so materializing it is a rebuild rather than a merge.
    pub fn clear_revoked(&self) -> Result<()> {
        self.conn.execute("DELETE FROM revoked", [])?;
        Ok(())
    }

    /// Records a removed device in the cache. Called only while materializing the document.
    pub fn upsert_revoked(&self, device: &RevokedDevice) -> Result<()> {
        self.conn.execute(
            "INSERT INTO revoked (device_id, at, by) VALUES (?1, ?2, ?3)
             ON CONFLICT(device_id) DO UPDATE SET at = ?2, by = ?3",
            rusqlite::params![device.device_id, device.at, device.by],
        )?;
        Ok(())
    }

    /// Every device the vault says has been removed, oldest removal first.
    pub fn list_revoked(&self) -> Result<Vec<RevokedDevice>> {
        let mut stmt =
            self.conn.prepare("SELECT device_id, at, by FROM revoked ORDER BY at, device_id")?;
        let rows = stmt.query_map([], |r| {
            Ok(RevokedDevice { device_id: r.get(0)?, at: r.get(1)?, by: r.get(2)? })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Looks up one group by id.
    pub fn get_group(&self, id: &str) -> Result<Option<Group>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, parent_id, color, created_at, updated_at FROM groups WHERE id = ?1",
        )?;
        let mut rows = stmt.query_map([id], row_to_group)?;
        Ok(rows.next().transpose()?)
    }

    /// Deletes a group; re-parenting its children is `Vault::delete_group`'s job.
    pub fn delete_group(&self, id: &str) -> Result<()> {
        self.conn.execute("DELETE FROM groups WHERE id = ?1", [id])?;
        Ok(())
    }

    /// The smallest arrangement key in one folder, or `None` if nothing there has one.
    ///
    /// `None` means the folder has never been arranged: every memo in it still sorts by
    /// `updated_at`. Empty keys are skipped rather than returned as the smallest, because an
    /// empty key is the absence of an answer, not an answer of "first".
    pub fn first_order_key(&self, group_id: &str) -> Result<Option<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT MIN(order_key) FROM memos WHERE group_id = ?1 AND order_key <> ''",
        )?;
        let key: Option<String> = stmt.query_row([group_id], |row| row.get(0))?;
        Ok(key)
    }

    /// Every memo in one folder, in the order it is drawn in.
    pub fn in_group(&self, group_id: &str) -> Result<Vec<Memo>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, title, body, color, opacity, group_id, order_key, created_at, updated_at
             FROM memos WHERE group_id = ?1
             ORDER BY order_key ASC, updated_at DESC, id ASC",
        )?;
        let rows = stmt.query_map([group_id], row_to_memo)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Writes one memo's arrangement key, leaving `updated_at` alone.
    ///
    /// Rearranging is not editing: bumping the timestamp would reorder the very list being
    /// arranged (an unarranged folder sorts by it) and would show up as an edit everywhere
    /// else that reads it.
    pub fn set_order_key(&self, id: &str, key: &str) -> Result<()> {
        self.conn
            .execute("UPDATE memos SET order_key = ?2 WHERE id = ?1", params![id, key])?;
        Ok(())
    }

    /// Deletes a memo by id.
    pub fn delete(&self, id: &str) -> Result<()> {
        self.conn.execute("DELETE FROM memos WHERE id = ?1", [id])?;
        Ok(())
    }
}

/// One `memos` row to a [`Memo`].
fn row_to_memo(row: &rusqlite::Row) -> rusqlite::Result<Memo> {
    Ok(Memo {
        id: row.get(0)?,
        title: row.get(1)?,
        body: row.get(2)?,
        color: row.get(3)?,
        opacity: row.get(4)?,
        group_id: row.get(5)?,
        order_key: row.get(6)?,
        created_at: row.get(7)?,
        updated_at: row.get(8)?,
    })
}

/// One `groups` row to a [`Group`].
fn row_to_attachment(row: &rusqlite::Row) -> rusqlite::Result<Attachment> {
    Ok(Attachment {
        id: row.get(0)?,
        memo_id: row.get(1)?,
        hash: row.get(2)?,
        name: row.get(3)?,
        mime: row.get(4)?,
        width_px: row.get(5)?,
        height_px: row.get(6)?,
        width_em_milli: row.get(7)?,
        x_permille: row.get(8)?,
        y_permille: row.get(9)?,
        mode: row.get(10)?,
        anchor_line: row.get(11)?,
        created_at: row.get(12)?,
    })
}

fn row_to_group(row: &rusqlite::Row) -> rusqlite::Result<Group> {
    Ok(Group {
        id: row.get(0)?,
        name: row.get(1)?,
        parent_id: row.get(2)?,
        color: row.get(3)?,
        created_at: row.get(4)?,
        updated_at: row.get(5)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DEFAULT_COLOR, DEFAULT_OPACITY, MIN_OPACITY};

    #[test]
    fn crud_roundtrip() {
        let store = Store::open_in_memory().unwrap();
        assert_eq!(store.list().unwrap().len(), 0);

        let memo = Memo::new("title", "body");
        store.upsert(&memo).unwrap();

        let all = store.list().unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].title, "title");
        assert_eq!(store.get(&memo.id).unwrap().unwrap(), memo);

        store.delete(&memo.id).unwrap();
        assert_eq!(store.list().unwrap().len(), 0);
    }

    #[test]
    fn upsert_updates_existing() {
        let store = Store::open_in_memory().unwrap();
        let mut memo = Memo::new("v1", "");
        store.upsert(&memo).unwrap();
        memo.title = "v2".into();
        memo.updated_at += 1000;
        store.upsert(&memo).unwrap();

        let all = store.list().unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].title, "v2");
    }

    /// Opacity is always clamped on store, so a bad value from another device is harmless.
    #[test]
    fn opacity_is_clamped_on_store() {
        assert_eq!(clamp_opacity(0), MIN_OPACITY);
        assert_eq!(clamp_opacity(1000), 100);
        assert_eq!(clamp_opacity(55), 55);

        let store = Store::open_in_memory().unwrap();
        let mut memo = Memo::new("t", "");
        assert_eq!(memo.opacity, DEFAULT_OPACITY);
        memo.opacity = 5; // below the floor
        store.upsert(&memo).unwrap();
        assert_eq!(store.get(&memo.id).unwrap().unwrap().opacity, MIN_OPACITY);
    }

    #[test]
    fn group_crud_roundtrip() {
        let store = Store::open_in_memory().unwrap();
        let g = Group::new("Work");
        store.upsert_group(&g).unwrap();
        assert_eq!(store.get_group(&g.id).unwrap().unwrap(), g);
        assert_eq!(store.list_groups().unwrap().len(), 1);
        store.delete_group(&g.id).unwrap();
        assert!(store.list_groups().unwrap().is_empty());
    }

    /// Deleting the cache takes the WAL with it.
    ///
    /// The database alone is not the cache: `-wal` holds writes not yet folded into it, and a
    /// reset that leaves one behind is a reset that gives the memos back.
    #[test]
    fn deleting_the_cache_takes_its_sidecars() {
        let path = std::env::temp_dir().join(format!("ymemo-wal-{}.db", uuid::Uuid::new_v4()));
        {
            let store = Store::open(&path).unwrap();
            store.upsert(&Memo::new("secret", "secret")).unwrap();
            // A write held in the WAL rather than folded back, which is the case that matters.
            assert!(path.with_extension("db-wal").exists(), "WAL mode is on");
        }
        Store::delete_file(&path).unwrap();
        assert!(!path.exists());
        assert!(!path.with_extension("db-wal").exists());
        assert!(!path.with_extension("db-shm").exists());
        // And it is not an error to delete a cache that was never there.
        Store::delete_file(&path).unwrap();
    }

    /// A rebuild's writes land together or not at all.
    #[test]
    fn a_rolled_back_cache_write_leaves_the_rows_alone() {
        let store = Store::open_in_memory().unwrap();
        store.upsert(&Memo::new("kept", "kept")).unwrap();

        store.begin().unwrap();
        store.clear_memos().unwrap();
        store.upsert(&Memo::new("half written", "")).unwrap();
        store.rollback().unwrap();
        assert_eq!(store.list().unwrap().len(), 1);
        assert_eq!(store.list().unwrap()[0].title, "kept");

        store.begin().unwrap();
        store.upsert(&Memo::new("second", "")).unwrap();
        store.commit().unwrap();
        assert_eq!(store.list().unwrap().len(), 2);
    }

    /// Opening a pre-color cache adds the columns with their defaults.
    #[test]
    fn migrates_pre_color_cache() {
        let path = std::env::temp_dir().join(format!("ymemo-mig-{}.db", uuid::Uuid::new_v4()));
        // Build the old schema (no color) by hand and insert a row.
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE memos (
                    id TEXT PRIMARY KEY, title TEXT NOT NULL, body TEXT NOT NULL,
                    created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL
                );
                INSERT INTO memos VALUES ('old1', 'old memo', 'body', 1, 2);",
            )
            .unwrap();
        }
        // Opening with the current Store runs init()'s migration.
        let store = Store::open(&path).unwrap();
        let m = store.get("old1").unwrap().unwrap();
        assert_eq!(m.title, "old memo");
        assert_eq!(m.color, DEFAULT_COLOR);
        assert_eq!(m.opacity, DEFAULT_OPACITY);

        // Updating the color still persists.
        let mut m2 = m.clone();
        m2.color = "blue".into();
        store.upsert(&m2).unwrap();
        assert_eq!(store.get("old1").unwrap().unwrap().color, "blue");

        std::fs::remove_file(&path).ok();
    }
}
