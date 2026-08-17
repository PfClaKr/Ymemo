//! Syncthing 기동과 주기적 병합.
//!
//! 전송은 코어(`ymemo_core::sync`)가 하고, 여기서는 데스크탑 쪽 수명 주기만 맡는다 —
//! 앱 시작 시 데몬을 띄우고, 타이머로 다른 기기의 로그를 가져와 화면에 반영한다.

use std::time::Duration;

use slint::{ComponentHandle, TimerMode};
use ymemo_core::sync::{SharedDevice, Syncthing};

use crate::list::refresh_list;
use crate::state::Ctx;
use crate::sticky::sticky_text;
use crate::{ListWindow, SharedDeviceRow};

/// Syncthing 쪽에서 vault 공유 폴더를 식별하는 고정 id (모든 기기가 같은 값 사용).
pub(crate) const SYNC_FOLDER_ID: &str = "ymemo-vault";

/// core 의 공유 기기 정보를 Slint 행으로 변환.
/// 표시 이름이 없으면 device-id 앞 7자로 대체한다(Slint 엔 문자열 슬라이스가 없음).
pub(crate) fn to_shared_row(d: SharedDevice) -> SharedDeviceRow {
    let name = if d.name.is_empty() {
        format!("{}…", d.id.chars().take(7).collect::<String>())
    } else {
        d.name.clone()
    };
    SharedDeviceRow {
        id: d.id.into(),
        name: name.into(),
        connected: d.connected,
    }
}

/// 다른 기기의 로그를 가져오는 주기 타이머를 (다시) 건다.
///
/// 주기가 환경설정 값이라 저장할 때마다 새 간격으로 다시 걸어야 한다. `Timer::start` 는
/// 기존 예약을 대체하므로 같은 타이머에 그대로 다시 걸면 된다.
pub(crate) fn start_merge_timer(timer: &slint::Timer, ctx: &Ctx, list_weak: slint::Weak<ListWindow>) {
    let interval = Duration::from_secs(ctx.settings.borrow().merge_seconds.max(1) as u64);
    let ctx = ctx.clone();
    timer.start(TimerMode::Repeated, interval, move || {
        let mut guard = ctx.vault.borrow_mut();
        let Some(v) = guard.as_mut() else { return };
        match v.rebuild() {
            Ok(()) => {
                refresh_list(v, &ctx.model, &ctx.collapsed.borrow());
                // 목록 창을 강제로 다시 그린다 (Windows 소프트웨어 렌더러가 모델 변경만으론
                // 리페인트를 안 해, 병합돼도 화면이 그대로여서 "동기화 안 됨"처럼 보인다).
                if let Some(w) = list_weak.upgrade() {
                    w.window().request_redraw();
                }
                // 열린 스티커에 원격 변경 반영. 편집 중(dirty)이면 덮어쓰지 않는다.
                for (id, entry) in ctx.stickies.borrow().iter() {
                    if entry.dirty.get() {
                        continue;
                    }
                    match v.store().get(id) {
                        Ok(Some(m)) => {
                            let text = sticky_text(&m);
                            if entry.window.get_memo_text() != text.as_str() {
                                entry.window.set_memo_text(text.into());
                            }
                            entry.window.set_memo_title(m.title.into());
                            entry.window.set_sticky_color(m.color.into());
                            entry.window.set_sticky_opacity(m.opacity as f32);
                            entry.window.window().request_redraw();
                        }
                        // 다른 기기에서 삭제됨 → 창만 숨긴다 (제거는 다음 닫기에서).
                        Ok(None) => {
                            let _ = entry.window.hide();
                        }
                        Err(e) => eprintln!("메모 조회 실패: {e}"),
                    }
                }
            }
            Err(e) => eprintln!("병합 실패: {e}"),
        }
    });
}

/// syncthing 을 찾아 띄우고 vault 디렉터리를 공유 폴더로 등록한다.
/// 바이너리가 없으면 None — 동기화 없이 로컬 전용으로 동작한다.
pub(crate) fn start_syncthing(data_dir: &std::path::Path, vault_dir: &std::path::Path) -> Option<Syncthing> {
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
