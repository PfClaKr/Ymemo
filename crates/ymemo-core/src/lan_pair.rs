//! LAN 페어링: 같은 네트워크에서 **6자리 코드**로 기기끼리 device-id 를 교환한다.
//!
//! 긴 Syncthing device-id 를 직접 입력하는 대신:
//!  1. 모든 기기는 페어링 모드일 때 1분마다 도는 6자리 코드를 띄우고 UDP 로 요청을 듣는다.
//!  2. 상대 코드를 입력하면 브로드캐스트로 그 코드를 아는 기기를 찾아 device-id 를 주고받는다.
//!  3. 얻은 device-id 로 기존 [`crate::sync::Syncthing::share_folder_with`] 를 호출해 등록한다.
//!
//! 6자리는 엔트로피가 낮으므로(100만) 코드로 Argon2 키를 유도해 교환 자체를 암호화한다
//! ([`crate::crypto::MasterKey`] 재사용). 복호화 성공 = 상대가 코드를 안다는 증명.
//! 무차별 대입 방어: Argon2 가 느림 + 1분 만료 + 처리율 제한 + "페어링 모드일 때만 수신".
//! (설령 잘못 붙어도 vault 는 E2E 암호문이라 마스터 암호 없이는 아무것도 못 읽는다.)

use anyhow::{anyhow, Result};
use rand::Rng;
use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::crypto::{generate_salt, MasterKey, Salt, SALT_LEN};

/// 페어링 UDP 포트 (Syncthing 의 21027/22000 과 겹치지 않게 고른 값).
pub const PAIR_PORT: u16 = 21029;
/// 코드 유효 시간. 지나면 새 코드로 회전한다.
const CODE_TTL: Duration = Duration::from_secs(60);
/// Argon2(느림) 남용을 막는 최소 처리 간격 — 무차별 대입 속도도 함께 제한한다.
const MIN_ATTEMPT_GAP: Duration = Duration::from_millis(200);

const MAGIC: &[u8; 6] = b"YMLAN1";
const MSG_HELLO: u8 = 1; // 조인 → 호스트: "이 코드 아는 기기 있나요? 내 id 는 이거"
const MSG_WELCOME: u8 = 2; // 호스트 → 조인: "네, 내 id 는 이거"
const HEADER_LEN: usize = MAGIC.len() + 1 + SALT_LEN; // magic + type + salt

/// 6자리 코드 문자열 생성 (앞자리 0 유지, 예: "042913").
pub fn gen_code() -> String {
    format!("{:06}", rand::rngs::OsRng.gen_range(0..1_000_000))
}

/// `magic || type || salt || encrypt_code(device_id)`.
/// 복호화하려면 같은 코드를 알아야 하므로, 성공 자체가 코드 인증이 된다.
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

/// 코드로 복호화해 상대 device-id 를 꺼낸다. 프레이밍/타입/코드가 안 맞으면 None.
/// (Argon2 를 돌리므로 호출 측에서 반드시 처리율을 제한할 것)
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

/// 프레이밍(magic+type)만 싸게 검사 — Argon2 전에 명백한 쓰레기를 버린다.
fn framed_as(buf: &[u8], msg_type: u8) -> bool {
    buf.len() > HEADER_LEN && &buf[..MAGIC.len()] == MAGIC && buf[MAGIC.len()] == msg_type
}

/// 회전하는 코드 상태. 회전 경계 경쟁을 위해 직전 코드도 잠깐 받아준다.
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

/// 백그라운드로 페어링 요청을 듣는 호스트. 살아 있는 동안만 코드를 노출하고 수신한다
/// (페어링 모드를 켤 때 start, 끌 때 drop — 노출 창을 최소화).
pub struct PairListener {
    codes: Arc<Mutex<Codes>>,
    peers_rx: Receiver<String>,
    running: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl PairListener {
    /// 고정 포트로 리스너를 띄운다. 같은 기기의 내 device-id 를 응답에 쓴다.
    pub fn start(device_id: impl Into<String>) -> Result<Self> {
        Self::start_on(device_id, (Ipv4Addr::UNSPECIFIED, PAIR_PORT).into())
    }

    fn start_on(device_id: impl Into<String>, bind: SocketAddr) -> Result<Self> {
        let device_id = device_id.into();
        let socket = UdpSocket::bind(bind)?;
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

        Ok(Self { codes, peers_rx, running, handle: Some(handle) })
    }

    /// 화면에 띄울 현재 6자리 코드.
    pub fn code(&self) -> String {
        let mut c = self.codes.lock().unwrap();
        c.maybe_rotate();
        c.current.clone()
    }

    /// 이번에 새로 페어링된 상대 device-id 를 하나 꺼낸다 (없으면 None). 호출 측이
    /// 폴링해 `share_folder_with` 로 등록한다. 여러 건이면 반복 호출한다.
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

/// 호스트 수신 루프: HELLO 를 받으면 코드로 복호화해 상대 id 를 얻고 WELCOME 으로 답한다.
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
            Err(_) => continue, // read timeout → 회전/종료 확인 후 다시 듣는다
        };
        let packet = &buf[..n];
        // Argon2 전에 싸게 거른다 + 처리율 제한 (CPU DoS·무차별 대입 방어).
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
        // 성공: 상대에게 내 id 를 (같은 코드로) 답하고, 앱에 등록을 넘긴다.
        if let Ok(welcome) = encode(MSG_WELCOME, &code, &device_id) {
            let _ = socket.send_to(&welcome, src);
        }
        let _ = peers_tx.send(peer_id);
        // 코드를 바로 회전시켜 재사용/재생을 막는다.
        codes.lock().unwrap().rotate();
    }
}

/// 조인 측: 코드를 알고 브로드캐스트로 호스트를 찾는다. 성공 시 호스트 device-id 반환,
/// 시간 안에 못 찾으면 `Ok(None)`.
pub fn join(code: &str, my_device_id: &str, timeout: Duration) -> Result<Option<String>> {
    let targets: [SocketAddr; 1] = [(Ipv4Addr::BROADCAST, PAIR_PORT).into()];
    join_to(code, my_device_id, timeout, &targets)
}

/// 브로드캐스트 주소를 주입할 수 있는 join (테스트는 127.0.0.1 로 직접 보낸다).
fn join_to(
    code: &str,
    my_device_id: &str,
    timeout: Duration,
    targets: &[SocketAddr],
) -> Result<Option<String>> {
    if code.len() != 6 || !code.bytes().all(|b| b.is_ascii_digit()) {
        return Err(anyhow!("6자리 숫자 코드가 아닙니다"));
    }
    let socket = UdpSocket::bind(("0.0.0.0", 0))?;
    socket.set_broadcast(true)?;
    socket.set_read_timeout(Some(Duration::from_millis(700)))?;

    let deadline = Instant::now() + timeout;
    let mut buf = [0u8; 2048];
    while Instant::now() < deadline {
        let hello = encode(MSG_HELLO, code, my_device_id)?;
        for t in targets {
            let _ = socket.send_to(&hello, t);
        }
        // WELCOME 을 기다린다 (read timeout 마다 다시 보낸다).
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
        assert_eq!(decode(MSG_HELLO, "000000", &msg), None); // 코드 불일치
        assert_eq!(decode(MSG_WELCOME, "123456", &msg), None); // 타입 불일치
        assert_eq!(decode(MSG_HELLO, "123456", b"garbage"), None); // 프레이밍 깨짐
    }

    #[test]
    fn join_rejects_bad_code() {
        assert!(join("12345", "id", Duration::from_millis(1)).is_err()); // 5자리
        assert!(join("abcdef", "id", Duration::from_millis(1)).is_err()); // 숫자 아님
    }

    /// 실제 소켓 왕복 (같은 호스트, 127.0.0.1 로 직접). 코드가 맞으면 양쪽이 서로의
    /// device-id 를 얻어야 한다.
    #[test]
    fn udp_exchange_with_matching_code() {
        // 호스트를 임의 포트로 띄우고, 그 코드를 알아낸 뒤 그 포트로 조인한다.
        let host = PairListener::start_on("HOST-DEVICE-ID", (Ipv4Addr::LOCALHOST, 0).into()).unwrap();
        // 리스너의 실제 포트를 알아야 조인 대상이 된다 → codes 는 있지만 포트는 소켓이 가짐.
        // start_on 은 포트를 노출하지 않으므로, 테스트용으로 고정 포트를 쓴다.
        drop(host);

        let port = 21999u16;
        let host = PairListener::start_on("HOST-DEVICE-ID", (Ipv4Addr::LOCALHOST, port).into()).unwrap();
        let code = host.code();

        let target: SocketAddr = (Ipv4Addr::LOCALHOST, port).into();
        let host_id = join_to(&code, "JOIN-DEVICE-ID", Duration::from_secs(3), &[target])
            .unwrap()
            .expect("호스트가 응답해야 한다");
        assert_eq!(host_id, "HOST-DEVICE-ID");

        // 호스트도 조인의 id 를 받아 등록 큐에 넣었어야 한다.
        let mut got = None;
        for _ in 0..20 {
            if let Some(p) = host.next_paired_peer() {
                got = Some(p);
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        assert_eq!(got.as_deref(), Some("JOIN-DEVICE-ID"));
    }

    /// 틀린 코드로 조인하면 호스트가 응답하지 않아 시간 초과(None)여야 한다.
    #[test]
    fn udp_exchange_wrong_code_times_out() {
        let port = 21998u16;
        let host = PairListener::start_on("HOST", (Ipv4Addr::LOCALHOST, port).into()).unwrap();
        let real = host.code();
        // 실제 코드와 다른 코드를 만든다.
        let wrong = if real == "000000" { "111111" } else { "000000" };
        let target: SocketAddr = (Ipv4Addr::LOCALHOST, port).into();
        let res = join_to(wrong, "JOIN", Duration::from_millis(800), &[target]).unwrap();
        assert_eq!(res, None);
    }
}
