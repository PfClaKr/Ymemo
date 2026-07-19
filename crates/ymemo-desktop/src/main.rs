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
use ymemo_core::{sync::Syncthing, vault::Vault, Memo, Store};

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
                    *syncthing.borrow_mut() = start_syncthing(&dir, v.dir());
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
            match st.device_id() {
                // 페어링 UI 전까지는 로그로 안내 (상대 기기에서 이 ID 를 등록해야 한다).
                Ok(id) => println!("Syncthing 기기 ID: {id}"),
                Err(e) => eprintln!("기기 ID 조회 실패: {e}"),
            }
            Some(st)
        }
        Err(e) => {
            eprintln!("syncthing 시작 실패 (동기화 없이 계속): {e}");
            None
        }
    }
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
