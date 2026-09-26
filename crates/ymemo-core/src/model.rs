//! The data model: what a memo, a folder and a photo on a memo are, and the limits every
//! value is clamped to before it is stored.

use serde::{Deserialize, Serialize};

use crate::now_millis;

/// Default palette key. The core only stores the string; the UI maps it to a real color.
pub const DEFAULT_COLOR: &str = "yellow";

/// Default sticky opacity in percent; 100 is fully opaque.
pub const DEFAULT_OPACITY: i64 = 100;
/// Lower bound, so a window can never become too transparent to find.
pub const MIN_OPACITY: i64 = 20;

/// Default display width of a photo, in 1/1000 em (20em = 20 characters wide).
pub const DEFAULT_WIDTH_EM_MILLI: i64 = 20_000;

/// Longest a vault name may be. It is a heading, not a document: past this it stops being
/// readable in the one line the window has for it, and a value arriving from another device
/// is not this device's to trust.
pub const VAULT_NAME_MAX: usize = 40;

/// Trims a vault name and cuts it to [`VAULT_NAME_MAX`] **characters**, not bytes, so a
/// Korean name is not cut through the middle of a syllable.
pub fn clamp_vault_name(name: &str) -> String {
    name.trim().chars().take(VAULT_NAME_MAX).collect()
}
/// Display-width bounds in 1/1000 em: too small is invisible, too large overflows.
pub const MIN_WIDTH_EM_MILLI: i64 = 4_000;
pub const MAX_WIDTH_EM_MILLI: i64 = 80_000;

/// How far each further photo on the same memo is offset from the previous one, in
/// per-mille of the note area. Without it every photo would land on the same spot and the
/// one underneath would be unreachable.
pub const PLACE_CASCADE_PERMILLE: i64 = 60;
/// Where the first photo lands, in per-mille of the note area.
pub const PLACE_ORIGIN_PERMILLE: i64 = 40;
/// After this many steps the cascade starts over, so it cannot walk off the note.
const PLACE_CASCADE_STEPS: i64 = 6;

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
    /// Where this memo sits **within its folder** — a fractional index, see [`crate::order`].
    ///
    /// Sorted ascending, and **always with the id as the tie-break**: two devices can land on
    /// the same key, and without the tie-break those two memos would swap places depending on
    /// which device is drawing them. Empty on a memo from before folders could be arranged,
    /// and on one that arrived from a device that has not been updated; those sort to the top
    /// (an empty string is the smallest key there is) until something gives them one.
    pub order_key: String,
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
            // Empty until the folder it lands in decides where it goes; `Vault::upsert` is
            // what gives a memo with no key one, at the top of wherever it is being put.
            order_key: String::new(),
            created_at: now,
            updated_at: now,
        }
    }
}

/// A photo attached to a memo. The bytes live content-addressed in [`crate::blob`]; this record
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
    /// Where the photo's top-left corner sits **inside the note**, in per-mille of the note
    /// area (0..=1000 across and down).
    ///
    /// A fraction, not em: the note is a phone screen on one device and a 260px sticky on
    /// the next, so a photo pinned two thirds of the way down stays two thirds of the way
    /// down instead of falling off the short one. The width stays in em, because a photo's
    /// size is about how much of the *text* it is worth, not how much of the window.
    pub x_permille: i64,
    pub y_permille: i64,
    /// How the photo sits against the writing. See [`PhotoMode`].
    ///
    /// Stored as a string rather than an enum so a value written by a future version — a
    /// mode this build has never heard of — reads back as [`PhotoMode::Float`] instead of
    /// failing the whole memo. The same reason the colour is a palette *key*.
    pub mode: String,
    /// For [`PhotoMode::Inline`]: how many lines of the body the photo sits **after**.
    ///
    /// A line count rather than a position, because that is what "in the middle of the
    /// writing" actually means — write another paragraph above it and the picture is still
    /// after the same words, wherever that has moved to. Everything else about a photo is a
    /// fraction of the note, which is right for something lying *on* the writing and wrong
    /// for something *in* it. Ignored in the other two modes, where it stays whatever it was
    /// so that a photo taken out of the writing and put back lands where it was before.
    pub anchor_line: i64,
    pub created_at: i64,
}

/// How a photo sits against the writing on the note.
///
/// A memo written before this existed has an empty string here, which is [`Self::Float`] —
/// the way every photo behaved when the only choice was where to drop it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PhotoMode {
    /// Lying on top of the note at the position it was dropped, text running underneath it.
    Float,
    /// In the flow: the photo takes a band of its own under the writing, and no line of text
    /// is ever hidden behind it.
    Flow,
    /// In the writing itself, after [`Attachment::anchor_line`] lines of it: the note is
    /// written above the picture and carries on below it.
    ///
    /// The room it sits in is **blank lines in the body**, put there when the photo was
    /// placed. That is what makes this work at all: a note is one text box and a text box
    /// cannot have a hole in it, but it can have empty lines, and those move with the
    /// writing the way anything typed does. It also means the gap is the user's to keep or
    /// close — delete the blank lines and the picture is simply over the words again.
    Inline,
}

/// The value stored for [`PhotoMode::Flow`]; `Float` stores the empty string, so a memo
/// from before this existed needs no migration to keep looking the way it did.
pub const PHOTO_MODE_FLOW: &str = "flow";
/// The value stored for [`PhotoMode::Inline`].
pub const PHOTO_MODE_INLINE: &str = "inline";

impl PhotoMode {
    /// Reads a stored value. Anything unrecognised is [`Self::Float`].
    pub fn parse(stored: &str) -> Self {
        match stored {
            PHOTO_MODE_FLOW => Self::Flow,
            PHOTO_MODE_INLINE => Self::Inline,
            _ => Self::Float,
        }
    }

    /// What to store for this mode.
    pub fn as_stored(self) -> &'static str {
        match self {
            Self::Float => "",
            Self::Flow => PHOTO_MODE_FLOW,
            Self::Inline => PHOTO_MODE_INLINE,
        }
    }
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
            x_permille: PLACE_ORIGIN_PERMILLE,
            y_permille: PLACE_ORIGIN_PERMILLE,
            mode: String::new(),
            anchor_line: 0,
            created_at: now_millis(),
        }
    }

    /// Top-left corner in logical px for a note area of `canvas_w` x `canvas_h` px.
    ///
    /// Clamped so the photo always ends up at least partly on the note, however small the
    /// window is or whatever another device stored.
    pub fn display_pos(&self, canvas_w: f64, canvas_h: f64, base_font_px: f64) -> (f64, f64) {
        let (w, h) = self.display_size(base_font_px);
        let x = clamp_permille(self.x_permille) as f64 / 1000.0 * canvas_w;
        let y = clamp_permille(self.y_permille) as f64 / 1000.0 * canvas_h;
        (
            x.min((canvas_w - w).max(0.0)),
            y.min((canvas_h - h).max(0.0)),
        )
    }

    /// Display size in logical px for this platform, where `base_font_px` is the UI's body
    /// font size. Without an aspect ratio the result is square — a placeholder.
    /// How this photo sits against the writing.
    pub fn mode(&self) -> PhotoMode {
        PhotoMode::parse(&self.mode)
    }

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

/// Clamps a position fraction to 0..=1000, for the same reason.
pub fn clamp_permille(v: i64) -> i64 {
    v.clamp(0, 1000)
}

/// Where the `n`-th photo of a memo should land, so photos do not pile up on one spot.
pub fn cascade_permille(n: usize) -> (i64, i64) {
    let step = (n as i64) % PLACE_CASCADE_STEPS;
    let off = PLACE_ORIGIN_PERMILLE + step * PLACE_CASCADE_PERMILLE;
    (clamp_permille(off), clamp_permille(off))
}

/// A device the user has removed from the vault.
///
/// Lives in the **synced document**, not in this device's settings, because a removal that
/// only one device knows about does not hold: the others go on sharing the vault with the
/// device and introduce it straight back (see `Syncthing::upsert_peer`).
///
/// **Not a lock.** A removed device still holds the data key and every memo it already
/// received, and nothing stops it writing to its own log — including to take itself off this
/// list. What this carries is the user's decision, to every device that is willing to honour
/// it. Shutting a hostile device out would mean a new data key and re-wrapping every log and
/// blob, which this design deliberately does not do.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RevokedDevice {
    /// Syncthing device id.
    pub device_id: String,
    /// When it was removed, unix epoch millis.
    pub at: i64,
    /// The device that removed it, so a screen can say where the decision came from.
    pub by: String,
}

/// A folder of memos; `parent_id` nests them.
///
/// Concurrent edits can make parenthood cyclic (A -> B, B -> A), so whoever builds the
/// tree has to break cycles — see [`crate::group_children`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Group {
    pub id: String,
    pub name: String,
    /// Parent group id; empty means top level.
    pub parent_id: String,
    /// Palette key, the same set memos use and equally opaque to the core.
    pub color: String,
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
            color: DEFAULT_COLOR.to_string(),
            created_at: now,
            updated_at: now,
        }
    }
}

/// Clamps opacity to the valid range; always run values through this before storing.
pub fn clamp_opacity(v: i64) -> i64 {
    v.clamp(MIN_OPACITY, 100)
}
