//! Keeping a remembered window on a screen that is actually there.
//!
//! A position is only a pair of numbers in a coordinate space the **primary** monitor
//! anchors, and a laptop changes that space every time it moves: undocked, the external
//! screen a note lived on is gone and the note comes back in empty space; docked somewhere
//! whose primary is another monitor, every coordinate shifts by that monitor's offset and a
//! note lands on the wrong screen or off all of them. With the stickies out of the taskbar,
//! a note off every screen cannot be reached at all. Measured on a three-monitor Windows
//! desk before this existed: notes saved at x=2500 and at (-3000,-2000) came back there.
//!
//! So each window's screen is remembered beside its geometry ([`Screen`]), and a window put
//! back is placed by [`place`]:
//!
//! 1. its screen is still attached (same name, same size) → the same spot on that screen,
//!    wherever that screen now sits in the coordinate space;
//! 2. otherwise, if the title bar is on some screen as it is → left alone;
//! 3. otherwise → onto the primary screen, at the same relative spot it had on its old one,
//!    and wholly inside it.
//!
//! Everything is physical pixels, like the geometry it sits beside.

use serde::{Deserialize, Serialize};

/// One monitor as the window system describes it: a name and where it sits, in physical px.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Screen {
    /// `eDP-1` or `HDMI-1` on X11, `\\.\DISPLAY2` on Windows; empty when the platform has none.
    pub name: String,
    /// `[x, y, width, height]`.
    pub rect: [i32; 4],
}

/// How much of a window's top edge has to be on a screen for it to count as reachable: room
/// to grab the title bar and drag it.
const GRAB_W: i32 = 40;
const GRAB_H: i32 = 16;

/// The screen a window is on: the one holding the most of it, or none when it is on none.
pub fn screen_of(g: [i32; 4], screens: &[Screen]) -> Option<&Screen> {
    screens
        .iter()
        .map(|s| (overlap(g, s.rect), s))
        .filter(|(area, _)| *area > 0)
        .max_by_key(|(area, _)| *area)
        .map(|(_, s)| s)
}

/// Whether enough of the window's top edge is on some screen to take hold of it.
pub fn reachable(g: [i32; 4], screens: &[Screen]) -> bool {
    let bar = [g[0], g[1], g[2], GRAB_H.min(g[3])];
    screens.iter().any(|s| {
        let (w, h) = intersection(bar, s.rect);
        w >= GRAB_W.min(g[2]) && h >= GRAB_H.min(g[3])
    })
}

/// Where a window remembered at `g`, last seen on `was`, goes on the screens there are now.
///
/// `primary` indexes `screens`; `None` falls back to the first. Returns `g`'s position when
/// there are no screens to go by, which is what a platform that will not list them gets.
pub fn place(g: [i32; 4], was: Option<&Screen>, screens: &[Screen], primary: Option<usize>) -> (i32, i32) {
    if screens.is_empty() {
        return (g[0], g[1]);
    }
    // 1. Its own screen, wherever the coordinate space has moved it to.
    if let Some(was) = was {
        let same = screens
            .iter()
            .find(|s| !s.name.is_empty() && s.name == was.name && s.rect[2..] == was.rect[2..]);
        if let Some(now) = same {
            let moved = [g[0] - was.rect[0] + now.rect[0], g[1] - was.rect[1] + now.rect[1], g[2], g[3]];
            return inside(moved, now.rect);
        }
    }
    // 2. Somewhere reachable as it is: an arrangement nothing says is wrong stays.
    if reachable(g, screens) {
        return (g[0], g[1]);
    }
    // 3. The primary screen, keeping the note's place on the old screen as a proportion.
    let target = &screens[primary.filter(|&i| i < screens.len()).unwrap_or(0)].rect;
    let (x, y) = match was {
        Some(was) if was.rect[2] > 0 && was.rect[3] > 0 => (
            target[0] + scale(g[0] - was.rect[0], was.rect[2], target[2]),
            target[1] + scale(g[1] - was.rect[1], was.rect[3], target[3]),
        ),
        _ => (g[0], g[1]),
    };
    inside([x, y, g[2], g[3]], *target)
}

/// Where a window `size` big goes to stand over `anchor` — centred on it, and wholly on the
/// screen the anchor is on. With no anchor (it is hidden) it goes to the middle of the primary
/// screen. Without this every dialog opened wherever the window system put a new window,
/// which on X11 and Windows alike is the top left corner, on top of the list.
pub fn centered_on(
    anchor: Option<[i32; 4]>,
    size: (i32, i32),
    screens: &[Screen],
    primary: Option<usize>,
) -> Option<(i32, i32)> {
    let primary_rect = screens.get(primary.unwrap_or(0)).or(screens.first()).map(|s| s.rect);
    let (area, screen) = match anchor {
        Some(a) => (a, screen_of(a, screens).map(|s| s.rect).or(primary_rect)),
        None => (primary_rect?, primary_rect),
    };
    let x = area[0] + (area[2] - size.0) / 2;
    let y = area[1] + (area[3] - size.1) / 2;
    Some(match screen {
        Some(s) => inside([x, y, size.0, size.1], s),
        None => (x, y),
    })
}

/// Where a new note goes: a step down and to the right of `from`, the note or list it was
/// made from, so it is seen to come out of it and covers none of its title — and wholly on
/// that screen.
pub fn cascade_from(from: [i32; 4], size: (i32, i32), step: i32, screens: &[Screen]) -> (i32, i32) {
    let (x, y) = (from[0] + step, from[1] + step);
    match screen_of(from, screens) {
        Some(s) => inside([x, y, size.0, size.1], s.rect),
        None => (x, y),
    }
}

/// Where a note made from the list goes: just right of it, level with its top — or left of
/// it when the right side has no room — stepping down past any note already standing on
/// exactly that spot, so a second new note is not hidden behind the first.
pub fn beside(from: [i32; 4], size: (i32, i32), gap: i32, taken: &[(i32, i32)], screens: &[Screen]) -> (i32, i32) {
    let screen = screen_of(from, screens).map(|s| s.rect);
    let right = from[0] + from[2] + gap;
    let x = match screen {
        Some(s) if right + size.0 > s[0] + s[2] && from[0] - gap - size.0 >= s[0] => from[0] - gap - size.0,
        _ => right,
    };
    let mut y = from[1];
    while taken.contains(&(x, y)) {
        y += gap * 2;
    }
    match screen {
        Some(s) => inside([x, y, size.0, size.1], s),
        None => (x, y),
    }
}

/// `g` moved the least distance that puts it wholly on `screen`; its top-left corner wins
/// when the window is larger than the screen, since that is where the title bar is.
fn inside(g: [i32; 4], screen: [i32; 4]) -> (i32, i32) {
    let x = g[0].min(screen[0] + screen[2] - g[2]).max(screen[0]);
    let y = g[1].min(screen[1] + screen[3] - g[3]).max(screen[1]);
    (x, y)
}

fn scale(offset: i32, from: i32, to: i32) -> i32 {
    (i64::from(offset) * i64::from(to) / i64::from(from)) as i32
}

fn intersection(a: [i32; 4], b: [i32; 4]) -> (i32, i32) {
    let w = (a[0] + a[2]).min(b[0] + b[2]) - a[0].max(b[0]);
    let h = (a[1] + a[3]).min(b[1] + b[3]) - a[1].max(b[1]);
    (w.max(0), h.max(0))
}

fn overlap(a: [i32; 4], b: [i32; 4]) -> i64 {
    let (w, h) = intersection(a, b);
    i64::from(w) * i64::from(h)
}

/// The screens attached now, and which of them is primary.
///
/// On X11 asked of RandR directly: winit keeps the list in a cache it clears only when it
/// handles a RandR event, and under Xvfb a mode switch left it describing the old screen
/// for as long as anyone watched — so a window stranded by the change was never noticed.
/// Windows and Wayland go through winit, which asks the system each time there.
pub fn current(window: &i_slint_backend_winit::winit::window::Window) -> (Vec<Screen>, Option<usize>) {
    if let Some(found) = crate::window::x11_screens(window) {
        return found;
    }
    let primary = window.primary_monitor();
    let mut at = None;
    let screens = window
        .available_monitors()
        .enumerate()
        .map(|(i, m)| {
            if primary.as_ref() == Some(&m) {
                at = Some(i);
            }
            let (p, s) = (m.position(), m.size());
            Screen {
                name: m.name().unwrap_or_default(),
                rect: [p.x, p.y, s.width as i32, s.height as i32],
            }
        })
        .collect();
    (screens, at)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screen(name: &str, rect: [i32; 4]) -> Screen {
        Screen { name: name.into(), rect }
    }

    /// The desk this was measured on: a primary laptop-sized screen, one above it, and a
    /// portrait one to its left.
    fn desk() -> Vec<Screen> {
        vec![
            screen("DISPLAY1", [0, -1200, 1920, 1200]),
            screen("DISPLAY2", [0, 0, 1920, 1080]),
            screen("DISPLAY3", [-1080, -543, 1080, 1920]),
        ]
    }

    #[test]
    fn a_note_on_a_screen_that_is_still_there_stays_put() {
        let s = desk();
        for (g, on) in [([300, 300, 220, 150], 1), ([300, -900, 220, 150], 0), ([-800, 200, 220, 150], 2)] {
            assert_eq!(place(g, Some(&s[on]), &s, Some(1)), (g[0], g[1]));
        }
    }

    /// The office made the top screen primary; at home the laptop's is. Same screens, every
    /// coordinate shifted by 1200 — and each note must follow its own screen.
    #[test]
    fn a_new_primary_moves_each_note_with_its_own_screen() {
        let office_top = screen("DISPLAY1", [0, 0, 1920, 1200]);
        let office_laptop = screen("DISPLAY2", [0, 1200, 1920, 1080]);
        let s = desk();
        // Was on the top screen at (300,300): without this it landed on the laptop's.
        assert_eq!(place([300, 300, 220, 150], Some(&office_top), &s, Some(1)), (300, -900));
        // Was on the laptop's at (300,300) of it: without this it was off every screen.
        assert_eq!(place([300, 1500, 220, 150], Some(&office_laptop), &s, Some(1)), (300, 300));
    }

    /// Undocked: the external screen is gone. The note comes to the primary one, at the
    /// same relative place, and wholly on it.
    #[test]
    fn a_note_whose_screen_is_gone_comes_to_the_primary_one() {
        let external = screen("HDMI-1", [1920, 0, 2560, 1440]);
        let s = desk();
        let (x, y) = place([1920 + 1280, 720, 220, 150], Some(&external), &s, Some(1));
        assert_eq!((x, y), (960, 540), "the middle of one screen is the middle of the other");
        // A note in the far corner of a larger screen still ends up wholly visible.
        let (x, y) = place([1920 + 2500, 1400, 220, 150], Some(&external), &s, Some(1));
        assert!(reachable([x, y, 220, 150], &s));
        assert!(x + 220 <= 1920 && y + 150 <= 1080, "({x},{y}) spills off the screen");
    }

    /// Settings written before screens were remembered: nothing to translate by, so a note
    /// that is reachable is trusted and one that is not is pulled onto the primary screen.
    #[test]
    fn without_a_remembered_screen_only_the_unreachable_move() {
        let s = desk();
        assert_eq!(place([300, -900, 220, 150], None, &s, Some(1)), (300, -900));
        let (x, y) = place([2500, 300, 220, 150], None, &s, Some(1));
        assert_eq!((x, y), (1700, 300));
        let (x, y) = place([-3000, -2000, 220, 150], None, &s, Some(1));
        assert_eq!((x, y), (0, 0));
    }

    /// Same name, different size: another monitor that happens to be called the same thing
    /// (Windows numbers them), so it is not taken for the old one.
    #[test]
    fn a_same_named_screen_of_another_size_is_not_the_same_screen() {
        let office = screen("DISPLAY1", [0, 0, 2560, 1440]);
        let s = desk();
        // (300,300) is on the laptop's screen as it stands, so it is left there.
        assert_eq!(place([300, 300, 220, 150], Some(&office), &s, Some(1)), (300, 300));
    }

    #[test]
    fn reachable_needs_the_title_bar_not_just_a_corner() {
        let s = desk();
        assert!(reachable([100, 100, 220, 150], &s));
        // Only the bottom edge pokes onto the screen: the bar is above the top screen.
        assert!(!reachable([100, -1200 - 140, 220, 150], &s));
        // Only 10px of the bar's width is on a screen.
        assert!(!reachable([1910, 100, 220, 150], &[screen("A", [0, 0, 1920, 1080])]));
    }

    #[test]
    fn a_dialog_stands_over_what_opened_it_and_stays_on_its_screen() {
        let s = desk();
        // Centred over a list in the middle of the laptop's screen.
        assert_eq!(centered_on(Some([700, 300, 400, 500]), (560, 480), &s, Some(1)), Some((620, 310)));
        // Over a list in the screen's top left corner, the dialog is pushed back onto it.
        assert_eq!(centered_on(Some([0, 0, 200, 120]), (560, 480), &s, Some(1)), Some((0, 0)));
        // Over the screen above, it stays on that one.
        let (_, y) = centered_on(Some([100, -1200, 300, 200]), (400, 520), &s, Some(1)).unwrap();
        assert_eq!(y, -1200);
        // Nothing to stand over: the middle of the primary screen.
        assert_eq!(centered_on(None, (400, 480), &s, Some(1)), Some((760, 300)));
        assert_eq!(centered_on(None, (400, 480), &[], None), None);
    }

    #[test]
    fn a_new_note_steps_out_of_the_one_it_came_from() {
        let s = desk();
        assert_eq!(cascade_from([300, 300, 220, 200], (200, 120), 28, &s), (328, 328));
        // At the bottom right corner it stays on the screen.
        assert_eq!(cascade_from([1700, 960, 220, 120], (200, 120), 28, &s), (1720, 960));
    }

    #[test]
    fn a_note_from_the_list_opens_beside_it_and_not_on_another() {
        let s = desk();
        assert_eq!(beside([20, 40, 380, 520], (200, 120), 14, &[], &s), (414, 40));
        // The spot is taken by the last new note: the next one goes below it.
        assert_eq!(beside([20, 40, 380, 520], (200, 120), 14, &[(414, 40)], &s), (414, 68));
        // A list against the right edge puts it on the left.
        assert_eq!(beside([1500, 40, 400, 520], (200, 120), 14, &[], &s), (1286, 40));
    }

    #[test]
    fn no_screens_listed_leaves_the_position_alone() {
        assert_eq!(place([2500, 300, 220, 150], None, &[], None), (2500, 300));
    }
}
