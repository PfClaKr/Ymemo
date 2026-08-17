//! 기기 연결 UI 쪽 처리: 페어링 코드 등록, QR 렌더링, 페어링 패널 크기 조절.
//!
//! 코드 형식과 LAN 탐색 자체는 코어(`ymemo_core::pairing` / `lan_pair`)에 있다.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use slint::{ComponentHandle, LogicalSize, ModelRc, SharedString, TimerMode, VecModel};
use ymemo_core::{lan_pair, pairing::PairingCode, sync::Syncthing};
use ymemo_i18n::t;

use crate::sync::{to_shared_row, SYNC_FOLDER_ID};
use crate::{ListWindow, LockWindow, SharedDeviceRow};

/// 페어링 패널이 스크롤 없이 다 들어가는 최소 창 크기 (논리 px).
pub(crate) const PAIRING_MIN_SIZE: (f32, f32) = (360.0, 520.0);

/// 페어링 패널을 열 때 창을 패널이 다 보이는 크기로 넓히고, 닫으면 원래 크기로 되돌린다.
/// `saved` 에 열기 직전 크기를 담아 두므로 사용자가 조절해 둔 크기도 보존된다.
pub(crate) fn resize_for_pairing(win: &slint::Window, open: bool, saved: &Cell<Option<(f32, f32)>>) {
    let scale = win.scale_factor();
    let size = win.size();
    let cur = (size.width as f32 / scale, size.height as f32 / scale);
    if open {
        saved.set(Some(cur));
        let want = (cur.0.max(PAIRING_MIN_SIZE.0), cur.1.max(PAIRING_MIN_SIZE.1));
        if want != cur {
            win.set_size(LogicalSize::new(want.0, want.1));
        }
    } else if let Some((w, h)) = saved.take() {
        win.set_size(LogicalSize::new(w, h));
    }
}

/// 상대 페어링 코드 등록 핸들러. 잠금/목록 창이 같은 로직을 쓰되
/// 결과 메시지만 자기 창에 표시하도록 `set_msg` 를 받는다.
pub(crate) fn pairing_handler(
    syncthing: Rc<RefCell<Option<Syncthing>>>,
    set_msg: impl Fn(SharedString) + 'static,
) -> impl Fn(SharedString) + 'static {
    move |code| {
        let guard = syncthing.borrow();
        let Some(st) = guard.as_ref() else { return };
        let msg = match PairingCode::decode(&code) {
            Ok(peer) => match st.share_folder_with(SYNC_FOLDER_ID, &peer.syncthing_device_id) {
                Ok(()) => t!("msg.register_done"),
                Err(e) => t!("msg.register_failed", error = e),
            },
            Err(e) => format!("{e}"),
        };
        set_msg(SharedString::from(msg));
    }
}

/// 페어링된 상대 device-id 를 공유 폴더에 등록한다. (LAN 페어링 결과 처리 공용)
pub(crate) fn register_peer(syncthing: &Rc<RefCell<Option<Syncthing>>>, peer_id: &str) -> String {
    let guard = syncthing.borrow();
    let Some(st) = guard.as_ref() else {
        return t!("msg.sync_off_cannot_register");
    };
    match st.share_folder_with(SYNC_FOLDER_ID, peer_id) {
        Ok(()) => t!("msg.lan_connected"),
        Err(e) => t!("msg.register_failed", error = e),
    }
}

/// 페어링 코드를 QR 이미지로 렌더링한다 (quiet zone 2모듈 포함, 확대는 UI 몫).
pub(crate) fn qr_image(text: &str) -> Option<slint::Image> {
    let code = qrcode::QrCode::new(text.as_bytes()).ok()?;
    let width = code.width();
    let colors = code.to_colors();
    let quiet = 2usize;
    let size = width + quiet * 2;

    let mut buf = slint::SharedPixelBuffer::<slint::Rgb8Pixel>::new(size as u32, size as u32);
    let pixels = buf.make_mut_slice();
    pixels.fill(slint::Rgb8Pixel { r: 255, g: 255, b: 255 });
    for y in 0..width {
        for x in 0..width {
            if colors[y * width + x] == qrcode::Color::Dark {
                pixels[(y + quiet) * size + (x + quiet)] = slint::Rgb8Pixel { r: 0, g: 0, b: 0 };
            }
        }
    }
    Some(slint::Image::from_rgb8(buf))
}

/// 기기 연결과 관련된 UI 배선을 한자리에 모은다: 페어링 코드 직접 등록, LAN 6자리 코드,
/// vault.json 도착 감시, 공유 기기 목록 갱신/해제, 페어링 패널 크기 조절.
///
/// 반환한 [`PairingTimers`] 를 살려 두는 동안만 주기 작업이 돈다 — `main` 이 끝까지 들고 있는다.
pub(crate) fn wire(
    lock: &LockWindow,
    list: &ListWindow,
    syncthing: &Rc<RefCell<Option<Syncthing>>>,
    lan: Option<Rc<lan_pair::PairListener>>,
    my_device_id: Option<String>,
    vault_dir: &std::path::Path,
) -> PairingTimers {
    let pair_timer = slint::Timer::default();
    let vault_watch_timer = slint::Timer::default();
    let devices_timer = slint::Timer::default();
    // ---- 페어링: 긴 device-id 직접 등록 (잠금/목록 창 공용, 폴백 경로) ----
    {
        let w = lock.as_weak();
        lock.on_add_peer(pairing_handler(syncthing.clone(), move |m| {
            w.unwrap().set_peer_message(m)
        }));
    }
    {
        let w = list.as_weak();
        list.on_add_peer(pairing_handler(syncthing.clone(), move |m| {
            w.unwrap().set_peer_message(m)
        }));
    }

    // ---- LAN 페어링: 6자리 코드로 연결 ----
    // join 은 몇 초 블로킹하므로 스레드에서 돌리고, 결과는 채널로 UI 스레드에 넘겨
    // 아래 pair_timer 가 받아 등록한다.
    let (join_tx, join_rx) = std::sync::mpsc::channel::<Result<Option<String>, String>>();
    {
        let lan_join = {
            let id = my_device_id.clone();
            let lock_w = lock.as_weak();
            let list_w = list.as_weak();
            let join_tx = join_tx.clone();
            move |code: SharedString| {
                let Some(my_id) = id.clone() else { return };
                let set_msg = |m: &str| {
                    if let Some(w) = lock_w.upgrade() {
                        w.set_lan_message(SharedString::from(m));
                    }
                    if let Some(w) = list_w.upgrade() {
                        w.set_lan_message(SharedString::from(m));
                    }
                };
                let code = code.trim().to_string();
                if code.len() != 6 || !code.bytes().all(|b| b.is_ascii_digit()) {
                    set_msg(&t!("msg.enter_six_digits"));
                    return;
                }
                set_msg(&t!("msg.connecting"));
                let join_tx = join_tx.clone();
                std::thread::spawn(move || {
                    let res = lan_pair::join(&code, &my_id, Duration::from_secs(6))
                        .map_err(|e| e.to_string());
                    let _ = join_tx.send(res);
                });
            }
        };
        lock.on_lan_join(lan_join.clone());
        list.on_lan_join(lan_join);
    }

    // 코드 표시 갱신 + 페어링된 상대 등록을 주기적으로 처리.
    if let Some(lan) = lan.clone() {
        let lock_w = lock.as_weak();
        let list_w = list.as_weak();
        let syncthing = syncthing.clone();
        pair_timer.start(TimerMode::Repeated, Duration::from_millis(800), move || {
            // 이 기기의 현재 6자리 코드를 양쪽 창에 반영.
            let code = SharedString::from(lan.code());
            if let Some(w) = lock_w.upgrade() {
                w.set_lan_pair_code(code.clone());
            }
            if let Some(w) = list_w.upgrade() {
                w.set_lan_pair_code(code.clone());
            }
            let set_msg = |m: String| {
                let m = SharedString::from(m);
                if let Some(w) = lock_w.upgrade() {
                    w.set_lan_message(m.clone());
                }
                if let Some(w) = list_w.upgrade() {
                    w.set_lan_message(m);
                }
            };
            // 우리 코드로 붙어 온 상대(호스트 역할)를 등록.
            while let Some(peer) = lan.next_paired_peer() {
                set_msg(register_peer(&syncthing, &peer));
            }
            // 우리가 상대 코드로 붙은 결과(조인 역할)를 등록.
            while let Ok(res) = join_rx.try_recv() {
                match res {
                    Ok(Some(peer)) => set_msg(register_peer(&syncthing, &peer)),
                    Ok(None) => set_msg(t!("msg.lan_peer_not_found")),
                    Err(e) => set_msg(e),
                }
            }
        });
    }

    // ---- vault.json 도착 감시: 새 기기가 "기존 기기 연결"을 고른 뒤, 페어링으로
    // vault.json 이 동기화돼 오면 잠금 화면을 암호 입력 모드로 전환한다. 이 감시가
    // 없으면 도착 전에 암호를 입력해 제 salt 로 vault 를 만들어 키가 갈라진다.
    if !vault_dir.join("vault.json").exists() {
        let lock_weak = lock.as_weak();
        let vault_json = vault_dir.join("vault.json");
        vault_watch_timer.start(TimerMode::Repeated, Duration::from_millis(800), move || {
            if !vault_json.exists() {
                return;
            }
            if let Some(lock) = lock_weak.upgrade() {
                lock.set_vault_exists(true);
                lock.set_show_sync(false);
                lock.set_lock_message(t!("msg.paired_enter_password").into());
            }
        });
    }

    // ---- 공유 중인 기기 목록: 주기적 갱신 + 해제 ----
    // 두 창이 같은 모델을 공유하므로 모델만 갱신하면 양쪽에 반영된다.
    let devices_model: Rc<VecModel<SharedDeviceRow>> = Rc::new(VecModel::from(Vec::new()));
    lock.set_shared_devices(ModelRc::from(devices_model.clone()));
    list.set_shared_devices(ModelRc::from(devices_model.clone()));

    let refresh_devices = {
        let syncthing = syncthing.clone();
        let devices_model = devices_model.clone();
        move || {
            let guard = syncthing.borrow();
            let Some(st) = guard.as_ref() else { return };
            match st.shared_devices(SYNC_FOLDER_ID) {
                Ok(list) => devices_model.set_vec(
                    list.into_iter().map(to_shared_row).collect::<Vec<_>>(),
                ),
                Err(e) => eprintln!("공유 기기 목록 조회 실패: {e}"),
            }
        }
    };

    {
        let refresh = refresh_devices.clone();
        devices_timer.start(TimerMode::Repeated, Duration::from_secs(4), refresh);
    }
    refresh_devices(); // 시작 시 한 번

    {
        let syncthing = syncthing.clone();
        let refresh = refresh_devices.clone();
        let lock_w = lock.as_weak();
        let list_w = list.as_weak();
        let unshare = move |id: SharedString| {
            let msg = match syncthing.borrow().as_ref() {
                Some(st) => match st.unshare_folder_with(SYNC_FOLDER_ID, id.as_str()) {
                    Ok(()) => t!("msg.unshared"),
                    Err(e) => t!("msg.unshare_failed", error = e),
                },
                None => t!("msg.sync_off"),
            };
            let msg = SharedString::from(msg);
            if let Some(w) = lock_w.upgrade() {
                w.set_lan_message(msg.clone());
            }
            if let Some(w) = list_w.upgrade() {
                w.set_lan_message(msg);
            }
            refresh(); // 목록 즉시 갱신
        };
        lock.on_unshare(unshare.clone());
        list.on_unshare(unshare);
    }

    // ---- 페어링 패널 열림/닫힘 → 창 크기 조절 ----
    // 패널은 스크롤 없이 한 화면에 다 보여야 하는데 두 창 모두 그만큼 크지 않다.
    // 열 때만 잠시 넓히고, 닫으면 열기 직전 크기로 되돌린다.
    {
        let saved = Rc::new(Cell::new(None));
        let weak = lock.as_weak();
        lock.on_sync_toggled(move |open| {
            if let Some(w) = weak.upgrade() {
                resize_for_pairing(w.window(), open, &saved);
            }
        });
    }
    {
        let saved = Rc::new(Cell::new(None));
        let weak = list.as_weak();
        list.on_sync_toggled(move |open| {
            if let Some(w) = weak.upgrade() {
                resize_for_pairing(w.window(), open, &saved);
            }
        });
    }

    PairingTimers {
        _pair: pair_timer,
        _vault_watch: vault_watch_timer,
        _devices: devices_timer,
    }
}

/// 페어링 관련 주기 작업을 살려 두는 핸들. drop 되면 타이머가 멈춘다.
pub(crate) struct PairingTimers {
    _pair: slint::Timer,
    _vault_watch: slint::Timer,
    _devices: slint::Timer,
}
