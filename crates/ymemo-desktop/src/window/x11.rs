//! The X11 half of [`super::skip_taskbar`].
//!
//! The connection is opened once and kept for the life of the process: raising a deskful of
//! notes re-applies the hint to each of them, and a fresh connect and three `InternAtom`
//! round trips per note is a visible pause for something the user asked to be instant.

use std::cell::RefCell;

use x11rb::connection::Connection;
use x11rb::protocol::xproto::{
    AtomEnum, ClientMessageEvent, ConfigureWindowAux, ConnectionExt, EventMask, PropMode,
    StackMode, Window,
};
use x11rb::rust_connection::RustConnection;
// `change_property32` is on this second extension trait, not on the xproto one.
use x11rb::wrapper::ConnectionExt as _;

/// `_NET_WM_STATE_ADD`, and "the request comes from a normal application", per EWMH.
const STATE_ADD: u32 = 1;
const SOURCE_APPLICATION: u32 = 1;

struct Wm {
    conn: RustConnection,
    root: Window,
    state: u32,
    skip_taskbar: u32,
    skip_pager: u32,
    above: u32,
}

thread_local! {
    // Outer Option: not tried yet. Inner: tried and failed, so it is not tried again on
    // every window — a machine with no X server will not grow one mid-run.
    static WM: RefCell<Option<Option<Wm>>> = const { RefCell::new(None) };
}

fn open() -> Option<Wm> {
    let run = || -> anyhow::Result<Wm> {
        let (conn, screen) = x11rb::connect(None)?;
        let root = conn.setup().roots[screen].root;
        let atom = |name: &str| -> anyhow::Result<u32> {
            Ok(conn.intern_atom(false, name.as_bytes())?.reply()?.atom)
        };
        let state = atom("_NET_WM_STATE")?;
        let skip_taskbar = atom("_NET_WM_STATE_SKIP_TASKBAR")?;
        let skip_pager = atom("_NET_WM_STATE_SKIP_PAGER")?;
        let above = atom("_NET_WM_STATE_ABOVE")?;
        Ok(Wm { conn, root, state, skip_taskbar, skip_pager, above })
    };
    match run() {
        Ok(wm) => Some(wm),
        Err(e) => {
            ymemo_core::diag!("no X11 connection for the taskbar hint: {e}");
            None
        }
    }
}

pub(super) fn skip_taskbar(xid: u32) {
    with_wm(|wm| {
        // Checked rather than merely flushed: the round trips are a local socket, and
        // without them a rejected hint is a note that silently keeps its taskbar button.
        if let Err(e) = add_states(wm, xid, [wm.skip_taskbar, wm.skip_pager]) {
            ymemo_core::diag!("could not hide the window from the taskbar: {e}");
        }
    });
}

pub(super) fn keep_above(xid: u32) {
    with_wm(|wm| {
        // 0 in the second slot is "no second state", per EWMH.
        if let Err(e) = add_states(wm, xid, [wm.above, 0]) {
            ymemo_core::diag!("could not keep the window on top: {e}");
        }
    });
}

/// The monitors as RandR reports them **now**: one per lit CRTC, named after its first
/// output, with the primary marked. Asked fresh each time — see `crate::screens::current`.
pub(super) fn screens() -> Option<(Vec<crate::screens::Screen>, Option<usize>)> {
    use x11rb::protocol::randr::ConnectionExt as _;
    let mut out = None;
    with_wm(|wm| {
        let run = || -> anyhow::Result<(Vec<crate::screens::Screen>, Option<usize>)> {
            let res = wm.conn.randr_get_screen_resources_current(wm.root)?.reply()?;
            let primary = wm.conn.randr_get_output_primary(wm.root)?.reply()?.output;
            let mut screens = Vec::new();
            let mut at = None;
            for crtc in res.crtcs {
                let info = wm.conn.randr_get_crtc_info(crtc, res.config_timestamp)?.reply()?;
                let Some(&output) = info.outputs.first() else { continue };
                if info.mode == 0 || info.width == 0 {
                    continue; // not lit
                }
                let name = wm
                    .conn
                    .randr_get_output_info(output, res.config_timestamp)?
                    .reply()
                    .map(|o| String::from_utf8_lossy(&o.name).into_owned())
                    .unwrap_or_default();
                if info.outputs.contains(&primary) {
                    at = Some(screens.len());
                }
                screens.push(crate::screens::Screen {
                    name,
                    rect: [info.x.into(), info.y.into(), info.width.into(), info.height.into()],
                });
            }
            Ok((screens, at))
        };
        match run() {
            Ok(s) => out = Some(s),
            Err(e) => ymemo_core::diag!("could not list the screens: {e}"),
        }
    });
    out
}

pub(super) fn restack_top(xid: u32) {
    with_wm(|wm| {
        let run = || -> anyhow::Result<()> {
            let aux = ConfigureWindowAux::new().stack_mode(StackMode::ABOVE);
            wm.conn.configure_window(xid, &aux)?.check()?;
            Ok(())
        };
        if let Err(e) = run() {
            ymemo_core::diag!("could not restack a window: {e}");
        }
    });
}

fn with_wm(f: impl FnOnce(&Wm)) {
    WM.with(|cell| {
        let mut slot = cell.borrow_mut();
        if let Some(wm) = slot.get_or_insert_with(open).as_ref() {
            f(wm);
        }
    });
}

/// Asks for up to two states **both** ways EWMH defines, because which one works depends on
/// something we cannot see here.
///
/// The spec is explicit: a *mapped* window's state is changed by a client message to the
/// root window, and an *unmapped* one by writing `_NET_WM_STATE` directly — the window
/// manager owns the property from the map onwards. Slint's `show()` only queues the map
/// request, so by the time this runs the window is one or the other and there is no way
/// to ask which. Doing both costs one round trip and is right either way.
fn add_states(wm: &Wm, xid: u32, wanted: [u32; 2]) -> anyhow::Result<()> {
    // Read-modify-write, never a plain overwrite: the taskbar states and `ABOVE` share
    // this property, and replacing the list to add one would drop the other.
    let current = wm
        .conn
        .get_property(false, xid, wm.state, AtomEnum::ATOM, 0, 64)?
        .reply()?;
    let mut states: Vec<u32> = current.value32().map(|v| v.collect()).unwrap_or_default();
    for atom in wanted.into_iter().filter(|&a| a != 0) {
        if !states.contains(&atom) {
            states.push(atom);
        }
    }
    wm.conn
        .change_property32(PropMode::REPLACE, xid, wm.state, AtomEnum::ATOM, &states)?
        .check()?;

    // One message carries two states; _NET_WM_STATE takes exactly that many.
    let ev = ClientMessageEvent::new(
        32,
        xid,
        wm.state,
        [STATE_ADD, wanted[0], wanted[1], SOURCE_APPLICATION, 0],
    );
    wm.conn
        .send_event(
            false,
            wm.root,
            EventMask::SUBSTRUCTURE_NOTIFY | EventMask::SUBSTRUCTURE_REDIRECT,
            ev,
        )?
        .check()?;
    Ok(())
}
