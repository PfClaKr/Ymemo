//! Everything in the vault as plain files, for leaving with.
//!
//! The vault is encrypted and the cache is a disposable view, so without this there was no
//! way to get one's own writing out short of copying memos one at a time — not for a backup
//! that does not depend on this app, and not for moving to another one. The result is a zip
//! of one Markdown file per memo, laid out in the vault's folders, with each memo's photos
//! beside it and linked from the end of its text.
//!
//! **What comes out is plaintext.** It is written only where the user asks for it and never
//! synced; SECURITY.md says so.
//!
//! The zip is written here rather than through a crate: the files are stored, not
//! compressed (the photos are JPEG and PNG already, and the text is small), which leaves a
//! format simple enough that a dependency would be most of the code.

use std::collections::{HashMap, HashSet};

use anyhow::Result;
use ymemo_i18n::t;

use crate::vault::Vault;
use crate::{group_children, Memo};

/// The vault as a zip, ready to be written to a file of the user's choosing.
///
/// Everything sits under one top-level directory named after the vault, so unpacking it
/// does not scatter memos across whatever folder it lands in. A photo whose blob has not
/// arrived yet is left out rather than failing the whole export.
pub fn markdown_zip(vault: &Vault) -> Result<Vec<u8>> {
    let store = vault.store();
    let root = match safe_name(&vault.name()) {
        name if name.is_empty() => "Ymemo".to_string(),
        name => name,
    };

    // Each folder's path, walked down from the top so a cycle or a missing parent (which
    // `group_children` already lifts to the top level) cannot loop.
    let groups = store.list_groups()?;
    let children = group_children(&groups);
    let mut folder_path: HashMap<String, String> = HashMap::new();
    let mut taken: HashMap<String, HashSet<String>> = HashMap::new();
    let mut stack = vec![(String::new(), root.clone())];
    while let Some((id, path)) = stack.pop() {
        for g in children.get(&id).map(Vec::as_slice).unwrap_or_default() {
            let name = unique(&mut taken, &path, &safe_name(&g.name), "");
            let child = format!("{path}/{name}");
            folder_path.insert(g.id.clone(), child.clone());
            stack.push((g.id.clone(), child));
        }
        folder_path.entry(id).or_insert(path);
    }

    let mut zip = ZipWriter::default();
    let mut memos = store.list()?;
    memos.sort_by(|a, b| a.order_key.cmp(&b.order_key).then_with(|| a.id.cmp(&b.id)));
    for memo in &memos {
        let dir = folder_path.get(&memo.group_id).unwrap_or(&root).clone();
        let base = unique(&mut taken, &dir, &memo_name(memo), ".md");
        let mut text = memo.body.clone();
        let photos = store.attachments_of(&memo.id)?;
        let mut links = Vec::new();
        for (i, a) in photos.iter().enumerate() {
            let Ok(bytes) = vault.attachment_bytes(&a.hash) else { continue };
            let ext = extension(&a.name, &a.mime);
            let file = unique(&mut taken, &dir, &format!("{base} {}", i + 1), &format!(".{ext}"));
            zip.add(&format!("{dir}/{file}.{ext}"), &bytes, memo.updated_at);
            links.push(format!("![{}](<{file}.{ext}>)", a.name.replace(['[', ']'], "")));
        }
        if !links.is_empty() {
            if !text.is_empty() && !text.ends_with('\n') {
                text.push('\n');
            }
            text.push('\n');
            text.push_str(&links.join("\n"));
            text.push('\n');
        }
        zip.add(&format!("{dir}/{base}.md"), text.as_bytes(), memo.updated_at);
    }
    Ok(zip.finish())
}

/// A file name for a memo: its title, or its first line, or the word for "untitled".
fn memo_name(memo: &Memo) -> String {
    let title = if memo.title.trim().is_empty() {
        memo.body.lines().find(|l| !l.trim().is_empty()).unwrap_or("")
    } else {
        &memo.title
    };
    match safe_name(title) {
        name if name.is_empty() => t!("core.export_untitled"),
        name => name,
    }
}

/// `name` made safe as one path component on Windows, macOS and Linux alike: the characters
/// any of them refuses become `_`, surrounding dots and spaces go (Windows drops a trailing
/// one silently, and a leading dot hides the file), and it is cut to a length every file
/// system takes — by characters, so a Korean title is never cut inside a syllable.
fn safe_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| if c.is_control() || r#"/\:*?"<>|"#.contains(c) { '_' } else { c })
        .take(80)
        .collect();
    let trimmed = cleaned.trim_matches(|c: char| c == '.' || c.is_whitespace());
    // Names Windows reserves for devices, whatever the extension.
    let stem = trimmed.split('.').next().unwrap_or("").to_ascii_uppercase();
    let reserved = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (stem.len() == 4
            && (stem.starts_with("COM") || stem.starts_with("LPT"))
            && stem.as_bytes()[3].is_ascii_digit());
    if reserved {
        format!("_{trimmed}")
    } else {
        trimmed.to_string()
    }
}

/// `name`, or `name (2)`, `name (3)` … — the first not yet used in `dir` with `ext`.
/// Compared without case, since two names differing only in case are one file on Windows
/// and macOS.
fn unique(taken: &mut HashMap<String, HashSet<String>>, dir: &str, name: &str, ext: &str) -> String {
    let used = taken.entry(dir.to_string()).or_default();
    let mut candidate = name.to_string();
    let mut n = 2;
    while !used.insert(format!("{candidate}{ext}").to_lowercase()) {
        candidate = format!("{name} ({n})");
        n += 1;
    }
    candidate
}

/// A photo's file extension, from its original name, else its type, else a generic one.
fn extension(name: &str, mime: &str) -> String {
    if let Some((_, ext)) = name.rsplit_once('.') {
        if (1..=5).contains(&ext.len()) && ext.chars().all(|c| c.is_ascii_alphanumeric()) {
            return ext.to_ascii_lowercase();
        }
    }
    match mime {
        "image/jpeg" => "jpg",
        "image/png" => "png",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "image/heic" => "heic",
        _ => "bin",
    }
    .to_string()
}

/// A zip of stored (uncompressed) files. Names are UTF-8 and flagged as such, so Korean
/// file names come out as written.
#[derive(Default)]
struct ZipWriter {
    out: Vec<u8>,
    central: Vec<u8>,
    count: u16,
}

impl ZipWriter {
    fn add(&mut self, path: &str, data: &[u8], modified_ms: i64) {
        let name = path.as_bytes();
        let crc = crc32(data);
        let (time, date) = dos_time(modified_ms);
        let offset = self.out.len() as u32;
        let size = data.len() as u32;
        // Version 2.0, bit 11 = UTF-8 names, method 0 = stored.
        let common = |buf: &mut Vec<u8>| {
            buf.extend_from_slice(&20u16.to_le_bytes());
            buf.extend_from_slice(&0x0800u16.to_le_bytes());
            buf.extend_from_slice(&0u16.to_le_bytes());
            buf.extend_from_slice(&time.to_le_bytes());
            buf.extend_from_slice(&date.to_le_bytes());
            buf.extend_from_slice(&crc.to_le_bytes());
            buf.extend_from_slice(&size.to_le_bytes());
            buf.extend_from_slice(&size.to_le_bytes());
            buf.extend_from_slice(&(name.len() as u16).to_le_bytes());
            buf.extend_from_slice(&0u16.to_le_bytes()); // extra field length
        };

        self.out.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
        common(&mut self.out);
        self.out.extend_from_slice(name);
        self.out.extend_from_slice(data);

        self.central.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
        // Made by: Unix (3), spec 2.0. With MS-DOS here, the unzip most Linux systems ship
        // ignores the UTF-8 flag and runs the names through an OEM code page, so every Korean
        // file name came out garbled (measured with Ubuntu's unzip 6.0; bsdtar and Python
        // were fine). Unix also makes the external attributes a file mode.
        self.central.extend_from_slice(&((3u16 << 8) | 20).to_le_bytes());
        common(&mut self.central);
        self.central.extend_from_slice(&0u16.to_le_bytes()); // comment length
        self.central.extend_from_slice(&0u16.to_le_bytes()); // disk number
        self.central.extend_from_slice(&0u16.to_le_bytes()); // internal attributes
        self.central.extend_from_slice(&(0o100644u32 << 16).to_le_bytes()); // rw-r--r--
        self.central.extend_from_slice(&offset.to_le_bytes());
        self.central.extend_from_slice(name);
        self.count += 1;
    }

    fn finish(mut self) -> Vec<u8> {
        let central_offset = self.out.len() as u32;
        let central_size = self.central.len() as u32;
        self.out.append(&mut self.central);
        self.out.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
        self.out.extend_from_slice(&0u16.to_le_bytes()); // this disk
        self.out.extend_from_slice(&0u16.to_le_bytes()); // disk with the directory
        self.out.extend_from_slice(&self.count.to_le_bytes());
        self.out.extend_from_slice(&self.count.to_le_bytes());
        self.out.extend_from_slice(&central_size.to_le_bytes());
        self.out.extend_from_slice(&central_offset.to_le_bytes());
        self.out.extend_from_slice(&0u16.to_le_bytes()); // comment length
        self.out
    }
}

/// MS-DOS time and date, in local time, as zip stores a file's modification time.
fn dos_time(ms: i64) -> (u16, u16) {
    use chrono::{Datelike, Local, TimeZone, Timelike};
    let Some(t) = Local.timestamp_millis_opt(ms).single() else { return (0, 0x21) };
    if t.year() < 1980 {
        return (0, 0x21); // 1980-01-01, the format's epoch
    }
    let time = ((t.hour() << 11) | (t.minute() << 5) | (t.second() / 2)) as u16;
    let date = ((((t.year() - 1980) as u32) << 9) | (t.month() << 5) | t.day()) as u16;
    (time, date)
}

/// CRC-32 (IEEE), the checksum zip keeps for every file.
fn crc32(data: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &byte in data {
        crc ^= byte as u32;
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xEDB8_8320 & (crc & 1).wrapping_neg());
        }
    }
    !crc
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Group, Store};

    #[test]
    fn crc32_matches_the_standard_check_value() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }

    #[test]
    fn names_are_made_safe_everywhere() {
        assert_eq!(safe_name("a/b:c?"), "a_b_c_");
        assert_eq!(safe_name("  .hidden. "), "hidden");
        assert_eq!(safe_name("con"), "_con");
        assert_eq!(safe_name("com1.txt"), "_com1.txt");
        assert_eq!(safe_name("회의록"), "회의록");
        assert_eq!(safe_name(&"가".repeat(200)).chars().count(), 80);
    }

    #[test]
    fn repeated_names_are_numbered() {
        let mut taken = HashMap::new();
        assert_eq!(unique(&mut taken, "d", "메모", ".md"), "메모");
        assert_eq!(unique(&mut taken, "d", "메모", ".md"), "메모 (2)");
        // Case differs only: the same file on Windows.
        assert_eq!(unique(&mut taken, "d", "Note", ".md"), "Note");
        assert_eq!(unique(&mut taken, "d", "note", ".md"), "note (2)");
        // Another folder is another namespace.
        assert_eq!(unique(&mut taken, "e", "메모", ".md"), "메모");
    }

    /// Every file in the zip, by name, read back from its central directory.
    fn entries(zip: &[u8]) -> HashMap<String, Vec<u8>> {
        let end = zip.len() - 22;
        assert_eq!(&zip[end..end + 4], &0x0605_4b50u32.to_le_bytes());
        let count = u16::from_le_bytes([zip[end + 10], zip[end + 11]]) as usize;
        let mut at = u32::from_le_bytes(zip[end + 16..end + 20].try_into().unwrap()) as usize;
        let mut out = HashMap::new();
        for _ in 0..count {
            let field = |o: usize| u32::from_le_bytes(zip[at + o..at + o + 4].try_into().unwrap());
            let crc = field(16);
            let size = field(20) as usize;
            let name_len = u16::from_le_bytes([zip[at + 28], zip[at + 29]]) as usize;
            let local = field(42) as usize;
            let name = String::from_utf8(zip[at + 46..at + 46 + name_len].to_vec()).unwrap();
            let data_at = local + 30 + name_len;
            let data = zip[data_at..data_at + size].to_vec();
            assert_eq!(crc32(&data), crc, "{name}");
            out.insert(name, data);
            at += 46 + name_len;
        }
        out
    }

    #[test]
    fn the_vault_comes_out_as_folders_of_markdown() {
        let dir = std::env::temp_dir().join(format!("ymemo-export-{}", uuid::Uuid::new_v4()));
        let mut v = Vault::create(dir.join("vault"), b"pw", Store::open_in_memory().unwrap()).unwrap();
        v.set_name("집").unwrap();
        let work = Group::new("업무");
        v.upsert_group(&work).unwrap();
        let mut inner = Group::new("회의");
        inner.parent_id = work.id.clone();
        v.upsert_group(&inner).unwrap();

        let mut a = Memo::new("장보기", "장보기\n- 우유");
        a.group_id = work.id.clone();
        v.upsert(&a).unwrap();
        let mut b = Memo::new("", "");
        b.group_id = inner.id.clone();
        v.upsert(&b).unwrap();
        // Same title as another memo in the same place.
        v.upsert(&Memo::new("할 일", "할 일\n1")).unwrap();
        v.upsert(&Memo::new("할 일", "할 일\n2")).unwrap();
        let photo = b"\x89PNG not really";
        v.attach(&a.id, photo, "사진.png", "image/png", 10, 10).unwrap();

        let files = entries(&markdown_zip(&v).unwrap());
        let text = |k: &str| String::from_utf8(files[k].clone()).unwrap();
        assert!(text("집/업무/장보기.md").starts_with("장보기\n- 우유"));
        assert!(text("집/업무/장보기.md").contains("![사진.png](<장보기 1.png>)"));
        assert_eq!(files["집/업무/장보기 1.png"], photo);
        assert_eq!(text(&format!("집/업무/회의/{}.md", t!("core.export_untitled"))), "");
        assert!(files.contains_key("집/할 일.md"));
        assert!(files.contains_key("집/할 일 (2).md"));
        assert_eq!(files.len(), 5);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
