//! Ymemo shared core: a pure Rust library used by the Slint desktop app and, through
//! `ymemo-ffi`, by the Flutter mobile app.
//!
//! This file holds the data model ([`Memo`], [`Group`]) and the local SQLite cache
//! ([`Store`]). The layers above live in sibling modules: [`crypto`] (key derivation and
//! AEAD), [`changelog`] (encrypted append-only log), [`vault`] (automerge merge and cache
//! rebuild), [`sync`] (Syncthing control), [`pairing`] and [`lan_pair`] (device linking).

pub mod blob;
pub mod changelog;
pub mod crypto;
pub mod lan_pair;
pub mod pairing;
pub mod recovery;
pub mod sync;
pub mod update;
pub mod vault;

use anyhow::Result;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

/// Default palette key. The core only stores the string; the UI maps it to a real color.
pub const DEFAULT_COLOR: &str = "yellow";

/// Default sticky opacity in percent; 100 is fully opaque.
pub const DEFAULT_OPACITY: i64 = 100;
/// Lower bound, so a window can never become too transparent to find.
pub const MIN_OPACITY: i64 = 20;

/// Default display width of a photo, in 1/1000 em (20em = 20 characters wide).
pub const DEFAULT_WIDTH_EM_MILLI: i64 = 20_000;
/// Display-width bounds in 1/1000 em: too small is invisible, too large overflows.
pub const MIN_WIDTH_EM_MILLI: i64 = 4_000;
pub const MAX_WIDTH_EM_MILLI: i64 = 80_000;

/// A single memo. Photos hang off it as separate [`Attachment`]s.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Memo {
    pub id: String,
    pub title: String,
    pub body: String,
    /// Palette key ("yellow"/"pink"/"green"/"blue"/"purple"), opaque to the core.
    pub color: String,
    /// Window opacity in percent, [`MIN_OPACITY`]..=100.
    pub opacity: i64,
    /// Owning group (folder) id; empty means top level.
    pub group_id: String,
    /// Unix epoch millis.
    pub created_at: i64,
    /// Unix epoch millis.
    pub updated_at: i64,
}

impl Memo {
    /// New memo with a UUID v4 id, current timestamps and default color/opacity.
    pub fn new(title: impl Into<String>, body: impl Into<String>) -> Self {
        let now = now_millis();
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            title: title.into(),
            body: body.into(),
            color: DEFAULT_COLOR.to_string(),
            opacity: DEFAULT_OPACITY,
            group_id: String::new(),
            created_at: now,
            updated_at: now,
        }
    }
}

/// A photo attached to a memo. The bytes live content-addressed in [`blob`]; this record
/// only carries the hash and **how to display it**.
///
/// Display size is stored in **em** (multiples of the platform's body font), not pixels:
/// 300px sized on a phone would be a postage stamp on the desktop, and vice versa.
/// "20 characters wide" reads the same everywhere. Each UI converts with
/// `width_em * its own base font px`, and derives the height from the original aspect
/// ratio (`height_px / width_px`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Attachment {
    pub id: String,
    pub memo_id: String,
    /// Content hash (hex) of the blob, which is also its file name.
    pub hash: String,
    /// Original file name, for display and export.
    pub name: String,
    /// `image/jpeg` and friends; empty when unknown.
    pub mime: String,
    /// Original pixel size, used only for the aspect ratio; 0 when unknown.
    pub width_px: i64,
    pub height_px: i64,
    /// Display width in 1/1000 em; keep it inside [`clamp_width_em_milli`].
    pub width_em_milli: i64,
    pub created_at: i64,
}

impl Attachment {
    /// New attachment at the default display size.
    pub fn new(memo_id: impl Into<String>, hash: impl Into<String>) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            memo_id: memo_id.into(),
            hash: hash.into(),
            name: String::new(),
            mime: String::new(),
            width_px: 0,
            height_px: 0,
            width_em_milli: DEFAULT_WIDTH_EM_MILLI,
            created_at: now_millis(),
        }
    }

    /// Display size in logical px for this platform, where `base_font_px` is the UI's body
    /// font size. Without an aspect ratio the result is square — a placeholder.
    pub fn display_size(&self, base_font_px: f64) -> (f64, f64) {
        let w = clamp_width_em_milli(self.width_em_milli) as f64 / 1000.0 * base_font_px;
        let ratio = if self.width_px > 0 && self.height_px > 0 {
            self.height_px as f64 / self.width_px as f64
        } else {
            1.0
        };
        (w, w * ratio)
    }
}

/// Clamps a display width, so a bad value from another device or version cannot break the UI.
pub fn clamp_width_em_milli(v: i64) -> i64 {
    v.clamp(MIN_WIDTH_EM_MILLI, MAX_WIDTH_EM_MILLI)
}

/// A folder of memos; `parent_id` nests them.
///
/// Concurrent edits can make parenthood cyclic (A -> B, B -> A), so whoever builds the
/// tree has to break cycles — see [`group_children`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Group {
    pub id: String,
    pub name: String,
    /// Parent group id; empty means top level.
    pub parent_id: String,
    pub created_at: i64,
    pub updated_at: i64,
}

impl Group {
    /// New top-level group with the given name.
    pub fn new(name: impl Into<String>) -> Self {
        let now = now_millis();
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            name: name.into(),
            parent_id: String::new(),
            created_at: now,
            updated_at: now,
        }
    }
}

/// Clamps opacity to the valid range; always run values through this before storing.
pub fn clamp_opacity(v: i64) -> i64 {
    v.clamp(MIN_OPACITY, 100)
}

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
        store.init()?;
        Ok(store)
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
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL
            );
            -- Folders; parent_id nests them (empty = top level).
            CREATE TABLE IF NOT EXISTS groups (
                id         TEXT PRIMARY KEY,
                name       TEXT NOT NULL,
                parent_id  TEXT NOT NULL DEFAULT '',
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
                created_at     INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS attachments_memo ON attachments(memo_id);
            -- Device-local metadata (device_id, ...). Never synced.
            CREATE TABLE IF NOT EXISTS meta (
                key   TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );",
        )?;
        // Migration: add columns introduced after the initial schema, so an old cache opens
        // without a full rebuild. A new column goes both in the CREATE TABLE above and here.
        for (name, ddl) in [
            ("color", "ALTER TABLE memos ADD COLUMN color TEXT NOT NULL DEFAULT 'yellow'"),
            ("opacity", "ALTER TABLE memos ADD COLUMN opacity INTEGER NOT NULL DEFAULT 100"),
            ("group_id", "ALTER TABLE memos ADD COLUMN group_id TEXT NOT NULL DEFAULT ''"),
        ] {
            let exists = self
                .conn
                .prepare("SELECT 1 FROM pragma_table_info('memos') WHERE name = ?1")?
                .exists([name])?;
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
    pub fn clear_memos(&self) -> Result<()> {
        self.conn.execute("DELETE FROM memos", [])?;
        self.conn.execute("DELETE FROM groups", [])?;
        self.conn.execute("DELETE FROM attachments", [])?;
        Ok(())
    }

    /// Inserts or updates an attachment by id.
    pub fn upsert_attachment(&self, a: &Attachment) -> Result<()> {
        self.conn.execute(
            "INSERT INTO attachments
                 (id, memo_id, hash, name, mime, width_px, height_px, width_em_milli, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
             ON CONFLICT(id) DO UPDATE SET
                 memo_id = ?2, hash = ?3, name = ?4, mime = ?5,
                 width_px = ?6, height_px = ?7, width_em_milli = ?8",
            params![
                a.id,
                a.memo_id,
                a.hash,
                a.name,
                a.mime,
                a.width_px,
                a.height_px,
                clamp_width_em_milli(a.width_em_milli),
                a.created_at
            ],
        )?;
        Ok(())
    }

    /// Attachments of one memo, in the order they were added.
    pub fn attachments_of(&self, memo_id: &str) -> Result<Vec<Attachment>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, memo_id, hash, name, mime, width_px, height_px, width_em_milli, created_at
             FROM attachments WHERE memo_id = ?1 ORDER BY created_at",
        )?;
        let rows = stmt.query_map([memo_id], row_to_attachment)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Looks up a single attachment.
    pub fn get_attachment(&self, id: &str) -> Result<Option<Attachment>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, memo_id, hash, name, mime, width_px, height_px, width_em_milli, created_at
             FROM attachments WHERE id = ?1",
        )?;
        let mut rows = stmt.query_map([id], row_to_attachment)?;
        Ok(rows.next().transpose()?)
    }

    /// Deletes the attachment record. **The blob file stays** (no GC — see [`blob`]).
    pub fn delete_attachment(&self, id: &str) -> Result<()> {
        self.conn.execute("DELETE FROM attachments WHERE id = ?1", [id])?;
        Ok(())
    }

    /// Inserts or updates a memo by id.
    pub fn upsert(&self, memo: &Memo) -> Result<()> {
        self.conn.execute(
            "INSERT INTO memos (id, title, body, color, opacity, group_id, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(id) DO UPDATE SET
                 title = ?2, body = ?3, color = ?4, opacity = ?5, group_id = ?6, updated_at = ?8",
            params![
                memo.id,
                memo.title,
                memo.body,
                memo.color,
                clamp_opacity(memo.opacity),
                memo.group_id,
                memo.created_at,
                memo.updated_at
            ],
        )?;
        Ok(())
    }

    /// All memos, most recently updated first.
    pub fn list(&self) -> Result<Vec<Memo>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, title, body, color, opacity, group_id, created_at, updated_at
             FROM memos ORDER BY updated_at DESC",
        )?;
        let rows = stmt.query_map([], row_to_memo)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Looks up one memo by id.
    pub fn get(&self, id: &str) -> Result<Option<Memo>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, title, body, color, opacity, group_id, created_at, updated_at
             FROM memos WHERE id = ?1",
        )?;
        let mut rows = stmt.query_map([id], row_to_memo)?;
        Ok(rows.next().transpose()?)
    }

    // ---- groups ----

    /// Inserts or updates a group by id.
    pub fn upsert_group(&self, group: &Group) -> Result<()> {
        self.conn.execute(
            "INSERT INTO groups (id, name, parent_id, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(id) DO UPDATE SET name = ?2, parent_id = ?3, updated_at = ?5",
            params![group.id, group.name, group.parent_id, group.created_at, group.updated_at],
        )?;
        Ok(())
    }

    /// All groups, sorted by name.
    pub fn list_groups(&self) -> Result<Vec<Group>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, parent_id, created_at, updated_at FROM groups ORDER BY name",
        )?;
        let rows = stmt.query_map([], row_to_group)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Looks up one group by id.
    pub fn get_group(&self, id: &str) -> Result<Option<Group>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, parent_id, created_at, updated_at FROM groups WHERE id = ?1",
        )?;
        let mut rows = stmt.query_map([id], row_to_group)?;
        Ok(rows.next().transpose()?)
    }

    /// Deletes a group; re-parenting its children is `Vault::delete_group`'s job.
    pub fn delete_group(&self, id: &str) -> Result<()> {
        self.conn.execute("DELETE FROM groups WHERE id = ?1", [id])?;
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
        created_at: row.get(6)?,
        updated_at: row.get(7)?,
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
        created_at: row.get(8)?,
    })
}

fn row_to_group(row: &rusqlite::Row) -> rusqlite::Result<Group> {
    Ok(Group {
        id: row.get(0)?,
        name: row.get(1)?,
        parent_id: row.get(2)?,
        created_at: row.get(3)?,
        updated_at: row.get(4)?,
    })
}

/// Safe parent -> children map, keyed by parent id (`""` = top level) with children sorted
/// by name.
///
/// Concurrent re-parenting on two devices can produce a cycle (A -> B, B -> A); the CRDT
/// keeps both. Rendering that as-is would recurse forever, so any group whose ancestor
/// chain never reaches the root is lifted to the top level.
pub fn group_children(groups: &[Group]) -> HashMap<String, Vec<Group>> {
    let by_id: HashMap<&str, &Group> = groups.iter().map(|g| (g.id.as_str(), g)).collect();
    let mut out: HashMap<String, Vec<Group>> = HashMap::new();
    for g in groups {
        let parent = if reaches_root(&by_id, g) {
            g.parent_id.clone()
        } else {
            String::new()
        };
        out.entry(parent).or_default().push(g.clone());
    }
    for children in out.values_mut() {
        children.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.id.cmp(&b.id)));
    }
    out
}

/// True when the ancestor chain reaches the top level; false on a cycle or missing parent.
fn reaches_root(by_id: &HashMap<&str, &Group>, g: &Group) -> bool {
    let mut seen: HashSet<&str> = HashSet::from([g.id.as_str()]);
    let mut cur = g.parent_id.as_str();
    while !cur.is_empty() {
        if !seen.insert(cur) {
            return false; // cycle
        }
        match by_id.get(cur) {
            Some(parent) => cur = parent.parent_id.as_str(),
            None => return false, // parent is gone
        }
    }
    true
}

/// Whether `id` is `ancestor` itself or below it — used to reject a drop that would put a
/// group inside its own subtree.
pub fn is_descendant(groups: &[Group], id: &str, ancestor: &str) -> bool {
    if id == ancestor {
        return true;
    }
    let by_id: HashMap<&str, &Group> = groups.iter().map(|g| (g.id.as_str(), g)).collect();
    let mut seen: HashSet<&str> = HashSet::new();
    let mut cur = id;
    while let Some(g) = by_id.get(cur) {
        if !seen.insert(cur) {
            return false; // cycle
        }
        if g.parent_id == ancestor {
            return true;
        }
        if g.parent_id.is_empty() {
            return false;
        }
        cur = g.parent_id.as_str();
    }
    false
}

/// Current time in Unix epoch millis; public so FFI callers share the same clock.
pub fn now_millis() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

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

    fn group_at(id: &str, name: &str, parent: &str) -> Group {
        Group {
            id: id.into(),
            name: name.into(),
            parent_id: parent.into(),
            created_at: 0,
            updated_at: 0,
        }
    }

    #[test]
    fn group_children_nests_and_sorts() {
        let groups = vec![
            group_at("b", "Work", ""),
            group_at("a", "Home", ""),
            group_at("c", "Inner", "a"),
        ];
        let tree = group_children(&groups);
        let roots: Vec<&str> = tree[""].iter().map(|g| g.id.as_str()).collect();
        assert_eq!(roots, vec!["a", "b"]); // by name: Home < Work
        assert_eq!(tree["a"].len(), 1);
        assert_eq!(tree["a"][0].id, "c");
    }

    /// Two devices making each other the parent forms a cycle; both must be rescued to the
    /// top level so the tree stays renderable.
    #[test]
    fn group_children_breaks_parent_cycles() {
        let groups = vec![group_at("a", "A", "b"), group_at("b", "B", "a")];
        let tree = group_children(&groups);
        let mut roots: Vec<&str> = tree[""].iter().map(|g| g.id.as_str()).collect();
        roots.sort();
        assert_eq!(roots, vec!["a", "b"]); // both rescued
    }

    /// A group whose parent was deleted elsewhere surfaces at the top level, not nowhere.
    #[test]
    fn group_children_rescues_orphans() {
        let groups = vec![group_at("a", "A", "missing-parent")];
        let tree = group_children(&groups);
        assert_eq!(tree[""].len(), 1);
        assert_eq!(tree[""][0].id, "a");
    }

    #[test]
    fn is_descendant_detects_self_and_nested() {
        let groups = vec![group_at("a", "A", ""), group_at("b", "B", "a"), group_at("c", "C", "b")];
        assert!(is_descendant(&groups, "a", "a")); // itself
        assert!(is_descendant(&groups, "c", "a")); // grandchild
        assert!(!is_descendant(&groups, "a", "c")); // not the other way
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
