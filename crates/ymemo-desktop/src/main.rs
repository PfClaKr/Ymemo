//! Ymemo 데스크탑 앱 (Slint).
//!
//! 잠금 화면에서 마스터 암호를 받아 `Vault` 를 열고(없으면 생성),
//! 이후 모든 메모 변경은 vault 를 통해 암호화 change 로그 + SQLite 캐시에 기록된다.
//! syncthing 바이너리가 있으면 자식 프로세스로 띄워 vault 디렉터리를 공유 폴더로
//! 등록하고, 타이머로 주기적으로 병합(rebuild)해 다른 기기의 변경을 반영한다.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use anyhow::Result;
use slint::{ModelRc, SharedString, TimerMode, VecModel};
use ymemo_core::{pairing::PairingCode, sync::Syncthing, vault::Vault, Memo, Store};

slint::include_modules!();

/// Syncthing 쪽에서 vault 공유 폴더를 식별하는 고정 id (모든 기기가 같은 값 사용).
const SYNC_FOLDER_ID: &str = "ymemo-vault";
/// 다른 기기의 로그 반영 주기.
const MERGE_INTERVAL: Duration = Duration::from_secs(15);

/// 플랫폼별 데이터 디렉터리. (예: Linux ~/.local/share/Ymemo)
fn data_dir() -> std::path::PathBuf {
    if let Some(dirs) = directories::ProjectDirs::from("dev", "ymemo", "Ymemo") {
        let dir = dirs.data_dir().to_path_buf();
        let _ = std::fs::create_dir_all(&dir);
        dir
    } else {
        std::path::PathBuf::from(".")
    }
}

fn main() -> Result<()> {
    let vault: Rc<RefCell<Option<Vault>>> = Rc::new(RefCell::new(None));
    // 앱 종료 시 Drop 으로 데몬도 함께 종료된다.
    let syncthing: Rc<RefCell<Option<Syncthing>>> = Rc::new(RefCell::new(None));

    let ui = AppWindow::new()?;

    // 메모 목록 모델 (vault 가 열리면 채워진다)
    let model: Rc<VecModel<SharedString>> = Rc::new(VecModel::from(Vec::<SharedString>::new()));
    ui.set_memos(ModelRc::from(model.clone()));

    // "열기" 콜백: 마스터 암호로 vault 를 열거나 생성
    {
        let vault = vault.clone();
        let syncthing = syncthing.clone();
        let model = model.clone();
        let ui_weak = ui.as_weak();
        ui.on_unlock(move |password| {
            let ui = ui_weak.unwrap();
            if password.is_empty() {
                ui.set_lock_message("암호를 입력하세요".into());
                return;
            }
            let dir = data_dir();
            // 캐시 DB 는 로컬 전용, vault/ 는 Syncthing 공유 폴더가 된다.
            let store = match Store::open(dir.join("ymemo.db")) {
                Ok(s) => s,
                Err(e) => {
                    ui.set_lock_message(SharedString::from(format!("캐시 열기 실패: {e}")));
                    return;
                }
            };
            // Argon2id 유도가 잠깐(수백 ms) UI 를 막지만 잠금 화면에서만 일어난다.
            match Vault::open_or_create(dir.join("vault"), password.as_bytes(), store) {
                Ok(v) => {
                    refresh(&v, &model);
                    let st = start_syncthing(&dir, v.dir());
                    if let Some(st) = &st {
                        // 페어링 정보: 자기 코드 + QR 을 UI 에 노출.
                        match st.device_id() {
                            Ok(id) => {
                                let code = PairingCode::new(id).encode();
                                if let Some(img) = qr_image(&code) {
                                    ui.set_qr_image(img);
                                }
                                ui.set_my_pairing_code(SharedString::from(code));
                                ui.set_sync_available(true);
                            }
                            Err(e) => eprintln!("기기 ID 조회 실패: {e}"),
                        }
                    }
                    *syncthing.borrow_mut() = st;
                    *vault.borrow_mut() = Some(v);
                    ui.set_locked(false);
                }
                Err(e) => {
                    ui.set_lock_message(SharedString::from(format!("{e}")));
                }
            }
        });
    }

    // "추가" 콜백: vault 에 기록하고 모델 갱신
    {
        let vault = vault.clone();
        let model = model.clone();
        ui.on_add_memo(move |text| {
            let text = text.trim();
            if text.is_empty() {
                return;
            }
            let mut guard = vault.borrow_mut();
            let Some(v) = guard.as_mut() else { return };
            let memo = Memo::new(text.to_string(), String::new());
            if let Err(e) = v.upsert(&memo) {
                eprintln!("메모 저장 실패: {e}");
                return;
            }
            refresh(v, &model);
        });
    }

    // "등록" 콜백: 상대 페어링 코드로 peer 를 등록하고 vault 폴더를 공유
    {
        let syncthing = syncthing.clone();
        let ui_weak = ui.as_weak();
        ui.on_add_peer(move |code| {
            let ui = ui_weak.unwrap();
            let guard = syncthing.borrow();
            let Some(st) = guard.as_ref() else { return };
            let msg = match PairingCode::decode(&code) {
                Ok(peer) => match st.share_folder_with(SYNC_FOLDER_ID, &peer.syncthing_device_id) {
                    Ok(()) => "등록 완료. 상대 기기에서도 이 코드를 등록하세요.".to_string(),
                    Err(e) => format!("등록 실패: {e}"),
                },
                Err(e) => format!("{e}"),
            };
            ui.set_peer_message(SharedString::from(msg));
        });
    }

    // 주기적 병합: Syncthing 이 가져다 놓은 다른 기기의 로그를 UI 에 반영
    let merge_timer = slint::Timer::default();
    {
        let vault = vault.clone();
        let model = model.clone();
        merge_timer.start(TimerMode::Repeated, MERGE_INTERVAL, move || {
            let mut guard = vault.borrow_mut();
            let Some(v) = guard.as_mut() else { return };
            match v.rebuild() {
                Ok(()) => refresh(v, &model),
                Err(e) => eprintln!("병합 실패: {e}"),
            }
        });
    }

    ui.run()?;
    Ok(())
}

/// syncthing 을 찾아 띄우고 vault 디렉터리를 공유 폴더로 등록한다.
/// 바이너리가 없으면 None — 동기화 없이 로컬 전용으로 동작한다.
fn start_syncthing(data_dir: &std::path::Path, vault_dir: &std::path::Path) -> Option<Syncthing> {
    let bin = Syncthing::find_binary()?;
    match Syncthing::spawn(&bin, &data_dir.join("syncthing")) {
        Ok(st) => {
            if let Err(e) = st.ensure_folder(SYNC_FOLDER_ID, "Ymemo Vault", vault_dir) {
                eprintln!("공유 폴더 등록 실패: {e}");
            }
            Some(st)
        }
        Err(e) => {
            eprintln!("syncthing 시작 실패 (동기화 없이 계속): {e}");
            None
        }
    }
}

/// 페어링 코드를 QR 이미지로 렌더링한다 (quiet zone 2모듈 포함, 확대는 UI 몫).
fn qr_image(text: &str) -> Option<slint::Image> {
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

/// vault 캐시의 메모 제목들을 Slint 모델에 반영한다.
fn refresh(vault: &Vault, model: &VecModel<SharedString>) {
    match vault.store().list() {
        Ok(memos) => {
            let titles: Vec<SharedString> =
                memos.into_iter().map(|m| SharedString::from(m.title)).collect();
            model.set_vec(titles);
        }
        Err(e) => eprintln!("메모 목록 조회 실패: {e}"),
    }
}
