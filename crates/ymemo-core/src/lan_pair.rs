//! LAN pairing: exchange device ids over a **6-digit code** on the same network.
//!
//! Instead of typing a long Syncthing device id:
//!  1. In pairing mode every device shows a 6-digit code (rotating every minute) and
//!     listens for UDP requests.
//!  2. Entering the other side's code broadcasts for whoever knows it and the two swap
//!     device ids.
//!  3. The result is handed to [`crate::sync::Syncthing::share_folder_with`].
//!
//! Six digits is only a million possibilities, so the code itself derives an Argon2 key
//! ([`crate::crypto::MasterKey`]) that encrypts the exchange: a successful decrypt proves
//! the peer knows the code. Brute force is held off by Argon2's cost, the one-minute
//! expiry, a rate limit, and listening only while pairing mode is on. Even a wrong pairing
//! reads nothing, since the vault stays E2E encrypted.

use anyhow::{anyhow, Result};
use ymemo_i18n::t;
use rand::Rng;
use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::crypto::{generate_salt, MasterKey, Salt, SALT_LEN};

/// Pairing UDP port, chosen to avoid Syncthing's 21027/22000.
pub const PAIR_PORT: u16 = 21029;
/// Code lifetime; a new code is rotated in afterwards.
const CODE_TTL: Duration = Duration::from_secs(60);
/// Minimum gap between attempts: caps Argon2 abuse and brute-force speed alike.
const MIN_ATTEMPT_GAP: Duration = Duration::from_millis(200);

const MAGIC: &[u8; 6] = b"YMLAN1";
const MSG_HELLO: u8 = 1; // joiner -> host: "anyone know this code? here is my id"
const MSG_WELCOME: u8 = 2; // host -> joiner: "yes, here is mine"
const HEADER_LEN: usize = MAGIC.len() + 1 + SALT_LEN; // magic + type + salt

/// Generates a 6-digit code, leading zeros kept (e.g. "042913").
pub fn gen_code() -> String {
    format!("{:06}", rand::rngs::OsRng.gen_range(0..1_000_000))
}

/// `magic || type || salt || encrypt_code(device_id)`. Decrypting requires the same code,
/// so success is the authentication.
fn encode(msg_type: u8, code: &str, device_id: &str) -> Result<Vec<u8>> {
    let salt = generate_salt();
    let key = MasterKey::derive(code.as_bytes(), &salt)?;
    let ct = key.encrypt(device_id.as_bytes())?;
    let mut out = Vec::with_capacity(HEADER_LEN + ct.len());
    out.extend_from_slice(MAGIC);
    out.push(msg_type);
    out.extend_from_slice(&salt);
    out.extend_from_slice(&ct);
    Ok(out)
}

/// Decrypts a peer's device id with the code; `None` on bad framing, type or code.
/// Runs Argon2, so callers must rate-limit.
fn decode(expected_type: u8, code: &str, buf: &[u8]) -> Option<String> {
    if buf.len() <= HEADER_LEN || &buf[..MAGIC.len()] != MAGIC || buf[MAGIC.len()] != expected_type {
        return None;
    }
    let salt: Salt = buf[MAGIC.len() + 1..HEADER_LEN].try_into().ok()?;
    let ct = &buf[HEADER_LEN..];
    let key = MasterKey::derive(code.as_bytes(), &salt).ok()?;
    let pt = key.decrypt(ct).ok()?;
    let id = String::from_utf8(pt).ok()?;
    (!id.is_empty()).then_some(id)
}

/// Cheap framing check, to drop obvious garbage before paying for Argon2.
fn framed_as(buf: &[u8], msg_type: u8) -> bool {
    buf.len() > HEADER_LEN && &buf[..MAGIC.len()] == MAGIC && buf[MAGIC.len()] == msg_type
}

/// Rotating code state; the previous code stays valid briefly to cover the rotation race.
struct Codes {
    current: String,
    previous: Option<String>,
    rotated_at: Instant,
}

impl Codes {
    fn new() -> Self {
        Self { current: gen_code(), previous: None, rotated_at: Instant::now() }
    }
    fn maybe_rotate(&mut self) {
        if self.rotated_at.elapsed() >= CODE_TTL {
            self.rotate();
        }
    }
    fn rotate(&mut self) {
        self.previous = Some(std::mem::replace(&mut self.current, gen_code()));
        self.rotated_at = Instant::now();
    }
}

/// Background host for pairing requests. It only shows a code and listens while alive —
/// started when pairing mode turns on, dropped when it turns off.
pub struct PairListener {
    /// The address actually bound, which matters when started on port 0.
    local_addr: SocketAddr,
    codes: Arc<Mutex<Codes>>,
    peers_rx: Receiver<String>,
    running: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl PairListener {
    /// Starts a listener on the fixed port, answering with this device's id.
    pub fn start(device_id: impl Into<String>) -> Result<Self> {
        Self::start_on(device_id, (Ipv4Addr::UNSPECIFIED, PAIR_PORT).into())
    }

    fn start_on(device_id: impl Into<String>, bind: SocketAddr) -> Result<Self> {
        let device_id = device_id.into();
        let socket = UdpSocket::bind(bind)?;
        let local_addr = socket.local_addr()?;
        socket.set_broadcast(true).ok();
        socket.set_read_timeout(Some(Duration::from_millis(500)))?;

        let codes = Arc::new(Mutex::new(Codes::new()));
        let running = Arc::new(AtomicBool::new(true));
        let (peers_tx, peers_rx): (Sender<String>, Receiver<String>) = mpsc::channel();

        let handle = {
            let codes = codes.clone();
            let running = running.clone();
            std::thread::spawn(move || recv_loop(socket, device_id, codes, running, peers_tx))
        };

        Ok(Self { local_addr, codes, peers_rx, running, handle: Some(handle) })
    }

    /// Address this listener is bound to.
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// Current 6-digit code to display.
    pub fn code(&self) -> String {
        let mut c = self.codes.lock().unwrap();
        c.maybe_rotate();
        c.current.clone()
    }

    /// Pops one newly paired peer id. Callers poll this and register each with
    /// `share_folder_with`.
    pub fn next_paired_peer(&self) -> Option<String> {
        self.peers_rx.try_recv().ok()
    }
}

impl Drop for PairListener {
    fn drop(&mut self) {
        self.running.store(false, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

/// Host loop: decrypt an incoming HELLO with the code, then answer with WELCOME.
fn recv_loop(
    socket: UdpSocket,
    device_id: String,
    codes: Arc<Mutex<Codes>>,
    running: Arc<AtomicBool>,
    peers_tx: Sender<String>,
) {
    let mut buf = [0u8; 2048];
    let mut last_attempt = Instant::now() - MIN_ATTEMPT_GAP;
    while running.load(Ordering::Relaxed) {
        {
            codes.lock().unwrap().maybe_rotate();
        }
        let (n, src) = match socket.recv_from(&mut buf) {
            Ok(v) => v,
            Err(_) => continue, // read timeout: re-check rotation and shutdown
        };
        let packet = &buf[..n];
        // Filter cheaply and rate-limit before Argon2 (CPU DoS and brute force).
        if !framed_as(packet, MSG_HELLO) || last_attempt.elapsed() < MIN_ATTEMPT_GAP {
            continue;
        }
        last_attempt = Instant::now();

        let (current, previous) = {
            let c = codes.lock().unwrap();
            (c.current.clone(), c.previous.clone())
        };
        let matched = decode(MSG_HELLO, &current, packet)
            .map(|id| (id, current))
            .or_else(|| previous.and_then(|p| decode(MSG_HELLO, &p, packet).map(|id| (id, p))));

        let Some((peer_id, code)) = matched else { continue };
        // Matched: answer with our id under the same code and hand the peer to the app.
        if let Ok(welcome) = encode(MSG_WELCOME, &code, &device_id) {
            let _ = socket.send_to(&welcome, src);
        }
        let _ = peers_tx.send(peer_id);
        // Rotate immediately, so the code cannot be reused or replayed.
        codes.lock().unwrap().rotate();
    }
}

/// Joiner side: broadcast for the host that knows `code`. Returns its device id, or
/// `Ok(None)` on timeout.
pub fn join(code: &str, my_device_id: &str, timeout: Duration) -> Result<Option<String>> {
    let targets: [SocketAddr; 1] = [(Ipv4Addr::BROADCAST, PAIR_PORT).into()];
    join_to(code, my_device_id, timeout, &targets)
}

/// `join` with injectable targets, so tests can send straight to 127.0.0.1.
fn join_to(
    code: &str,
    my_device_id: &str,
    timeout: Duration,
    targets: &[SocketAddr],
) -> Result<Option<String>> {
    if code.len() != 6 || !code.bytes().all(|b| b.is_ascii_digit()) {
        return Err(anyhow!(t!("core.bad_lan_code")));
    }
    let socket = UdpSocket::bind(("0.0.0.0", 0))?;
    socket.set_broadcast(true)?;
    // The read timeout is also the retry period. One HELLO costs the host two Argon2
    // derivations (decrypt, then encrypt the answer), so retrying fast only piles work on a
    // slow device.
    socket.set_read_timeout(Some(Duration::from_millis(1500)))?;

    let deadline = Instant::now() + timeout;
    let mut buf = [0u8; 2048];
    while Instant::now() < deadline {
        let hello = encode(MSG_HELLO, code, my_device_id)?;
        for t in targets {
            let _ = socket.send_to(&hello, t);
        }
        // Wait for WELCOME; resend on every read timeout.
        match socket.recv_from(&mut buf) {
            Ok((n, _src)) => {
                if let Some(host_id) = decode(MSG_WELCOME, code, &buf[..n]) {
                    return Ok(Some(host_id));
                }
            }
            Err(_) => continue,
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gen_code_is_six_digits() {
        for _ in 0..100 {
            let c = gen_code();
            assert_eq!(c.len(), 6);
            assert!(c.bytes().all(|b| b.is_ascii_digit()));
        }
    }

    #[test]
    fn encode_decode_roundtrip() {
        let msg = encode(MSG_HELLO, "123456", "DEVICE-ID-ABC").unwrap();
        assert_eq!(decode(MSG_HELLO, "123456", &msg).as_deref(), Some("DEVICE-ID-ABC"));
    }

    #[test]
    fn wrong_code_fails_to_decode() {
        let msg = encode(MSG_HELLO, "123456", "DEVICE-ID-ABC").unwrap();
        assert_eq!(decode(MSG_HELLO, "000000", &msg), None); // wrong code
        assert_eq!(decode(MSG_WELCOME, "123456", &msg), None); // wrong type
        assert_eq!(decode(MSG_HELLO, "123456", b"garbage"), None); // bad framing
    }

    #[test]
    fn join_rejects_bad_code() {
        assert!(join("12345", "id", Duration::from_millis(1)).is_err()); // too short
        assert!(join("abcdef", "id", Duration::from_millis(1)).is_err()); // not digits
    }

    /// Real socket round-trip over 127.0.0.1: with a matching code both sides must end up
    /// with the other's device id.
    #[test]
    fn udp_exchange_with_matching_code() {
        // Bind port 0 and ask for the real address; the fixed port breaks the test when the
        // app is running on this machine.
        let host =
            PairListener::start_on("HOST-DEVICE-ID", (Ipv4Addr::LOCALHOST, 0).into()).unwrap();
        let code = host.code();
        let target = host.local_addr();

        // Generous budget: the round-trip runs four Argon2 derivations (joiner encode, host
        // decode, host encode, joiner decode) at hundreds of ms each, and a slow two-core CI
        // runner sharing time with the other Argon2 tests stretches that to seconds. Success
        // returns immediately, so the usual run time is unaffected.
        let host_id = join_to(&code, "JOIN-DEVICE-ID", Duration::from_secs(60), &[target])
            .unwrap()
            .expect("host must answer");
        assert_eq!(host_id, "HOST-DEVICE-ID");

        // The host must have queued the joiner's id too.
        let mut got = None;
        for _ in 0..100 {
            if let Some(p) = host.next_paired_peer() {
                got = Some(p);
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        assert_eq!(got.as_deref(), Some("JOIN-DEVICE-ID"));
    }

    /// A wrong code gets no answer, so the join must time out.
    #[test]
    fn udp_exchange_wrong_code_times_out() {
        let host = PairListener::start_on("HOST", (Ipv4Addr::LOCALHOST, 0).into()).unwrap();
        let real = host.code();
        // Any code other than the real one.
        let wrong = if real == "000000" { "111111" } else { "000000" };
        let target = host.local_addr();
        // Short budget here: we are only confirming that nothing answers.
        let res = join_to(wrong, "JOIN", Duration::from_millis(1600), &[target]).unwrap();
        assert_eq!(res, None);
    }
}
