//! Ymemo 데스크탑 앱 (Slint).
//!
//! Phase 0: ymemo-core 의 SQLite 저장소와 스티커 메모 UI 를 연결한다.
//! 동기화/암호화는 아직 없다.

use std::cell::RefCell;
use std::rc::Rc;

use anyhow::Result;
use slint::{ModelRc, SharedString, VecModel};
use ymemo_core::{Memo, Store};

slint::include_modules!();

/// 플랫폼별 데이터 디렉터리 아래 SQLite 파일 경로. (예: Linux ~/.local/share/Ymemo/ymemo.db)
fn data_db_path() -> std::path::PathBuf {
    if let Some(dirs) = directories::ProjectDirs::from("dev", "ymemo", "Ymemo") {
        let dir = dirs.data_dir().to_path_buf();
        let _ = std::fs::create_dir_all(&dir);
        dir.join("ymemo.db")
    } else {
        std::path::PathBuf::from("ymemo.db")
    }
}

fn main() -> Result<()> {
    let store = Rc::new(RefCell::new(Store::open(data_db_path())?));

    let ui = AppWindow::new()?;

    // 메모 목록 모델 준비 및 초기 로드
    let model: Rc<VecModel<SharedString>> = Rc::new(VecModel::from(Vec::<SharedString>::new()));
    refresh(&store.borrow(), &model);
    ui.set_memos(ModelRc::from(model.clone()));

    // "추가" 콜백: 저장소에 넣고 모델 갱신
    {
        let store = store.clone();
        let model = model.clone();
        ui.on_add_memo(move |text| {
            let text = text.trim();
            if text.is_empty() {
                return;
            }
            let memo = Memo::new(text.to_string(), String::new());
            if let Err(e) = store.borrow().upsert(&memo) {
                eprintln!("메모 저장 실패: {e}");
                return;
            }
            refresh(&store.borrow(), &model);
        });
    }

    ui.run()?;
    Ok(())
}

/// 저장소의 메모 제목들을 Slint 모델에 반영한다.
fn refresh(store: &Store, model: &VecModel<SharedString>) {
    match store.list() {
        Ok(memos) => {
            let titles: Vec<SharedString> =
                memos.into_iter().map(|m| SharedString::from(m.title)).collect();
            model.set_vec(titles);
        }
        Err(e) => eprintln!("메모 목록 조회 실패: {e}"),
    }
}
