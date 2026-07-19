//! Ymemo 데스크탑 앱 (Slint).
//!
//! 잠금 화면에서 마스터 암호를 받아 `Vault` 를 열고(없으면 생성),
//! 이후 모든 메모 변경은 vault 를 통해 암호화 change 로그 + SQLite 캐시에 기록된다.

use std::cell::RefCell;
use std::rc::Rc;

use anyhow::Result;
use slint::{ModelRc, SharedString, VecModel};
use ymemo_core::{vault::Vault, Memo, Store};

slint::include_modules!();

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

    let ui = AppWindow::new()?;

    // 메모 목록 모델 (vault 가 열리면 채워진다)
    let model: Rc<VecModel<SharedString>> = Rc::new(VecModel::from(Vec::<SharedString>::new()));
    ui.set_memos(ModelRc::from(model.clone()));

    // "열기" 콜백: 마스터 암호로 vault 를 열거나 생성
    {
        let vault = vault.clone();
        let model = model.clone();
        let ui_weak = ui.as_weak();
        ui.on_unlock(move |password| {
            let ui = ui_weak.unwrap();
            if password.is_empty() {
                ui.set_lock_message("암호를 입력하세요".into());
                return;
            }
            let dir = data_dir();
            // 캐시 DB 는 로컬 전용, vault/ 는 이후 Syncthing 공유 폴더가 된다.
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

    ui.run()?;
    Ok(())
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
