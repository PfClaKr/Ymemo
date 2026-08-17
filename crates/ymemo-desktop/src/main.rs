// Windows 릴리스 빌드는 콘솔 없이 GUI 서브시스템으로 뜬다. 이게 없으면 앱과 함께
// cmd 창이 뜨고, 그 콘솔 창을 닫으면 앱이 통째로 종료된다. (디버그 빌드는 println
// 확인을 위해 콘솔을 유지한다. Linux/macOS 에선 아무 영향 없다.)
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]
//! Ymemo 데스크탑 앱 (Slint) — 트레이 상주 + 스티커 메모 창.
//!
//! 창 구조:
//!  - `LockWindow`: 시작 시 마스터 암호 입력 → vault 열기/생성
//!  - `ListWindow`: 트레이 아이콘 클릭으로 토글되는 메모 목록 (열기/추가/삭제/페어링)
//!  - `StickyWindow`: 메모 하나당 하나씩 뜨는 무프레임 스티커 창.
//!    본문이 곧 편집칸이고(디바운스 자동 저장), 제목 바 드래그로 배치,
//!    더블클릭으로 얇은 바 접기/펴기, ＋ 새 메모, ✕ 창 닫기(삭제 아님).
//!
//! 모든 창을 닫아도 앱은 트레이에 남는다 (`run_event_loop_until_quit`).
//! 종료는 트레이 메뉴의 "종료" 로만 한다.


use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::time::{Duration, Instant};

use anyhow::Result;
use slint::{ComponentHandle, ModelRc, SharedString, TimerMode, VecModel};
use ymemo_core::{
    crypto::MasterKey, lan_pair, now_millis, pairing::PairingCode, sync::Syncthing, vault::Vault,
    Store,
};

use settings::Settings;
use ymemo_i18n::t;

slint::include_modules!();

// build.rs 가 i18n 카탈로그에서 만든 `apply_strings` (Slint 전역 Strings 채우기).
include!(concat!(env!("OUT_DIR"), "/i18n_apply.rs"));

mod icon;
mod list;
mod lock;
mod pairing;
mod settings;
mod state;
mod sticky;
mod sync;
mod tray;

use icon::set_window_icon;
use list::{move_row, refresh_list};
use lock::{apply_lang, apply_opened_vault, fill_settings_window, lock_now, start_unlock_session};
use pairing::qr_image;
use state::{touch, AppUi, Ctx, APP};
use sticky::{close_sticky, new_memo, open_sticky, snap_tick, SNAP_INTERVAL};
use sync::{start_merge_timer, start_syncthing};

/// 자리 비움(무동작) 확인 주기. 설정된 분 단위에 비하면 충분히 촘촘하다.
const IDLE_CHECK_INTERVAL: Duration = Duration::from_secs(20);


/// Slint 렌더러 선택. 창을 만들기 전에 한 번 호출한다.
///
/// 기본 렌더러(femtovg)는 OpenGL 2.0+ 를 요구하는데, Windows 는 GPU 드라이버가 없거나
/// VM/RDP 라 legacy GL 1.1 만 있는 경우가 흔해 `glCreateShader` 조차 못 찾고 죽는다.
/// 그래서 Windows 는 CPU 소프트웨어 렌더러를 기본으로 쓴다(이 앱은 그래픽이 가벼워 충분).
/// `YMEMO_RENDERER=femtovg|software|skia` 로 강제할 수 있다. Linux/macOS 는 기본 유지.
fn select_renderer() {
    let name = match std::env::var("YMEMO_RENDERER") {
        Ok(n) if !n.is_empty() => n,
        _ if cfg!(windows) => "software".to_string(),
        _ => return, // 기본 렌더러 유지 (GL 이 정상인 환경)
    };
    match i_slint_backend_winit::Backend::builder()
        .with_renderer_name(name.as_str())
        .build()
    {
        Ok(backend) => {
            if let Err(e) = slint::platform::set_platform(Box::new(backend)) {
                eprintln!("렌더러 '{name}' 설정 실패, 기본값으로 계속: {e:?}");
            }
        }
        Err(e) => eprintln!("렌더러 '{name}' 백엔드 생성 실패, 기본값으로 계속: {e}"),
    }
}

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
    // 자석 스냅은 창 좌표를 읽고/설정할 수 있어야 한다. 네이티브 Wayland 는 이를
    // 막으므로, DISPLAY(XWayland)가 있으면 WAYLAND_DISPLAY 를 지워 winit 이 X11 로
    // 뜨게 한다. YMEMO_FORCE_WAYLAND=1 이면 이 우회를 끄고 Wayland 로 뜬다(스냅 불가).
    if std::env::var_os("YMEMO_FORCE_WAYLAND").is_none() && std::env::var_os("DISPLAY").is_some() {
        std::env::remove_var("WAYLAND_DISPLAY");
    }

    select_renderer();

    let lock = LockWindow::new()?;
    let list = ListWindow::new()?;

    // syncthing 은 unlock 전에 띄운다 (키가 필요 없음). 이래야 새 기기가
    // "먼저 페어링 → vault.json/로그 동기화 → 그 다음 암호 입력" 순서로
    // 기존 vault 에 합류할 수 있다.
    let dir = data_dir();
    let vault_dir = dir.join("vault");
    let _ = std::fs::create_dir_all(&vault_dir);
    let st = start_syncthing(&dir, &vault_dir);
    let mut my_device_id: Option<String> = None;
    if let Some(st) = &st {
        match st.device_id() {
            Ok(id) => {
                let code = PairingCode::new(&id).encode();
                if let Some(img) = qr_image(&code) {
                    lock.set_qr_image(img);
                }
                if let Some(img) = qr_image(&code) {
                    list.set_qr_image(img);
                }
                lock.set_my_pairing_code(SharedString::from(code.clone()));
                list.set_my_pairing_code(SharedString::from(code));
                lock.set_sync_available(true);
                list.set_sync_available(true);
                my_device_id = Some(id);
            }
            Err(e) => eprintln!("기기 ID 조회 실패: {e}"),
        }
    }
    // 앱 종료 시 Drop 으로 데몬도 함께 종료된다.
    let syncthing: Rc<RefCell<Option<Syncthing>>> = Rc::new(RefCell::new(st));

    // LAN 페어링 리스너: 같은 네트워크에서 6자리 코드로 device-id 를 주고받는다.
    // (device-id 를 알 때만 — 그래야 상대에게 응답할 수 있다)
    let lan = my_device_id
        .as_ref()
        .and_then(|id| match lan_pair::PairListener::start(id.clone()) {
            Ok(l) => Some(Rc::new(l)),
            Err(e) => {
                eprintln!("LAN 페어링 시작 실패 (LAN 연결 없이 계속): {e}");
                None
            }
        });

    let mut loaded = Settings::load(&dir);
    loaded.sanitize();
    let ctx = Ctx {
        vault: Rc::new(RefCell::new(None)),
        model: Rc::new(VecModel::from(Vec::<ListRow>::new())),
        stickies: Rc::new(RefCell::new(HashMap::new())),
        collapsed: Rc::new(RefCell::new(HashSet::new())),
        dir: Rc::new(dir.clone()),
        settings: Rc::new(RefCell::new(loaded)),
        last_activity: Rc::new(Cell::new(Instant::now())),
    };
    list.set_rows(ModelRc::from(ctx.model.clone()));
    let unlocked = Rc::new(Cell::new(false));

    // 환경설정 창은 한 번만 만들어 두고 보이기/숨기기만 한다 (매번 새로 만들면 열 때마다
    // 창 위치가 초기화된다).
    let settings_win = SettingsWindow::new()?;
    apply_lang(&ctx, &lock, &list, &settings_win);

    // 첫 실행 판별: vault.json 이 있으면 기존 vault(→ 잠금 해제), 없으면 새 기기
    // (→ "새로 만들기" / "기존 기기 연결" 선택). 이 분기가 없으면 새 기기가 페어링으로
    // vault.json 을 받기 전에 암호를 입력해 제 salt 로 vault 를 만들어버려 키가 갈라진다.
    let vault_exists = vault_dir.join("vault.json").exists();
    lock.set_vault_exists(vault_exists);

    // ---- 자동 잠금 해제: 유효한 세션 키가 남아 있으면 암호를 묻지 않는다 ----
    // 실패하면(키 불일치, 캐시 손상, 갈라진 키 등) 조용히 세션을 버리고 잠금 화면으로 간다.
    let auto_unlocked = vault_exists
        && settings::load_session(&dir).is_some_and(|key_bytes| {
            let opened = MasterKey::from_bytes(&key_bytes)
                .and_then(|key| {
                    let store = Store::open(dir.join("ymemo.db"))?;
                    Vault::open_with_key(vault_dir.clone(), key, store)
                });
            match opened {
                Ok(v) => {
                    apply_opened_vault(v, &ctx, &lock, &list.as_weak(), &unlocked);
                    true
                }
                Err(e) => {
                    eprintln!("자동 잠금 해제 실패, 암호를 다시 묻습니다: {e}");
                    settings::clear_session(&dir);
                    false
                }
            }
        });

    // ---- 잠금 창: 기존 vault 열기 / 새 vault 생성 ----
    {
        let ctx = ctx.clone();
        let lock_weak = lock.as_weak();
        let list_weak = list.as_weak();
        let unlocked = unlocked.clone();
        let dir = dir.clone();
        // 기존 vault 열기: vault.json 의 salt 로 키를 유도한다. 갈라진 키는 open 안에서
        // 자가 치유된다. **create 로 폴백하지 않는다** — 그게 키 분기의 원인이었다.
        lock.on_unlock(move |password| {
            let lock = lock_weak.unwrap();
            if password.is_empty() {
                lock.set_lock_message(t!("msg.enter_password").into());
                return;
            }
            let store = match Store::open(dir.join("ymemo.db")) {
                Ok(s) => s,
                Err(e) => {
                    lock.set_lock_message(SharedString::from(t!("msg.cache_open_failed", error = e)));
                    return;
                }
            };
            // Argon2id 유도가 잠깐(수백 ms) UI 를 막지만 잠금 화면에서만 일어난다.
            match Vault::open(dir.join("vault"), password.as_bytes(), store) {
                Ok(v) => {
                    start_unlock_session(&ctx, &v);
                    apply_opened_vault(v, &ctx, &lock, &list_weak, &unlocked);
                }
                Err(e) => lock.set_lock_message(SharedString::from(format!("{e}"))),
            }
        });
    }
    {
        let ctx = ctx.clone();
        let lock_weak = lock.as_weak();
        let list_weak = list.as_weak();
        let unlocked = unlocked.clone();
        let dir = dir.clone();
        // 새 vault 생성: 이 기기에서 처음 시작할 때만. salt 는 여기서 단 한 번 생긴다.
        lock.on_create_vault(move |password| {
            let lock = lock_weak.unwrap();
            if password.is_empty() {
                lock.set_lock_message(t!("msg.enter_new_password").into());
                return;
            }
            let store = match Store::open(dir.join("ymemo.db")) {
                Ok(s) => s,
                Err(e) => {
                    lock.set_lock_message(SharedString::from(t!("msg.cache_open_failed", error = e)));
                    return;
                }
            };
            match Vault::open_or_create(dir.join("vault"), password.as_bytes(), store) {
                Ok(v) => {
                    start_unlock_session(&ctx, &v);
                    apply_opened_vault(v, &ctx, &lock, &list_weak, &unlocked);
                }
                Err(e) => lock.set_lock_message(SharedString::from(format!("{e}"))),
            }
        });
    }

    // ---- 목록 창: 메모 열기 / 새 메모 / 삭제 ----
    {
        let ctx = ctx.clone();
        list.on_open_memo(move |id| {
            touch(&ctx);
            let memo = {
                let guard = ctx.vault.borrow();
                let Some(v) = guard.as_ref() else { return };
                match v.store().get(&id) {
                    Ok(Some(m)) => m,
                    _ => return,
                }
            };
            if let Err(e) = open_sticky(&ctx, &memo, false) {
                eprintln!("스티커 창 열기 실패: {e}");
            }
        });
    }
    {
        let ctx = ctx.clone();
        list.on_new_memo(move || new_memo(&ctx));
    }
    {
        let ctx = ctx.clone();
        list.on_delete_row(move |id, is_group| {
            touch(&ctx);
            {
                let mut guard = ctx.vault.borrow_mut();
                let Some(v) = guard.as_mut() else { return };
                // 그룹 삭제는 안의 내용을 지우지 않고 상위로 올린다 (코어가 처리).
                let res = if is_group { v.delete_group(&id) } else { v.delete(&id) };
                if let Err(e) = res {
                    eprintln!("삭제 실패: {e}");
                    return;
                }
                refresh_list(v, &ctx.model, &ctx.collapsed.borrow());
            }
            if !is_group {
                close_sticky(&ctx.stickies, id.as_str()); // 열려 있던 스티커 창 정리
            }
        });
    }

    // ---- 그룹: 생성 / 펼침 토글 / 이름 변경 / 드래그 이동 ----
    {
        let ctx = ctx.clone();
        let list_weak = list.as_weak();
        list.on_new_group(move || {
            touch(&ctx);
            let group = ymemo_core::Group::new(t!("msg.new_group_name"));
            {
                let mut guard = ctx.vault.borrow_mut();
                let Some(v) = guard.as_mut() else { return };
                if let Err(e) = v.upsert_group(&group) {
                    eprintln!("그룹 생성 실패: {e}");
                    return;
                }
                refresh_list(v, &ctx.model, &ctx.collapsed.borrow());
            }
            // 만들자마자 이름 편집 상태로 — 바로 타이핑할 수 있게.
            if let Some(w) = list_weak.upgrade() {
                w.set_editing_text(SharedString::from(group.name.clone()));
                w.set_editing_id(SharedString::from(group.id));
            }
        });
    }
    {
        let ctx = ctx.clone();
        list.on_toggle_group(move |id| {
            touch(&ctx);
            {
                let mut collapsed = ctx.collapsed.borrow_mut();
                if !collapsed.remove(id.as_str()) {
                    collapsed.insert(id.to_string());
                }
            }
            let guard = ctx.vault.borrow();
            let Some(v) = guard.as_ref() else { return };
            refresh_list(v, &ctx.model, &ctx.collapsed.borrow());
        });
    }
    {
        let ctx = ctx.clone();
        list.on_rename_group(move |id, name| {
            touch(&ctx);
            let mut guard = ctx.vault.borrow_mut();
            let Some(v) = guard.as_mut() else { return };
            let Ok(Some(mut g)) = v.store().get_group(&id) else { return };
            if g.name == name.as_str() {
                return;
            }
            g.name = name.to_string();
            g.updated_at = now_millis();
            if let Err(e) = v.upsert_group(&g) {
                eprintln!("그룹 이름 변경 실패: {e}");
                return;
            }
            refresh_list(v, &ctx.model, &ctx.collapsed.borrow());
        });
    }
    {
        let ctx = ctx.clone();
        list.on_move_row(move |src, dst| {
            touch(&ctx);
            move_row(&ctx, src, dst);
        });
    }

    // ---- 기기 연결(페어링·공유 기기 목록)은 pairing 모듈이 통째로 배선한다 ----
    let _pairing = pairing::wire(&lock, &list, &syncthing, lan.clone(), my_device_id.clone(), &vault_dir);

    // ---- 주기적 병합: 다른 기기의 로그를 목록/스티커에 반영 ----
    // 주기가 설정값이라 환경설정 저장 시 다시 걸어야 한다 → Rc 로 들고 있는다.
    let merge_timer = Rc::new(slint::Timer::default());
    start_merge_timer(&merge_timer, &ctx, list.as_weak());

    // 트레이는 아래에서 만들지만, 언어를 바꾸면 메뉴 문구도 갈아야 해서 설정 콜백이
    // 핸들에 닿아야 한다. 먼저 빈 칸을 만들어 두고 생성 후 채운다.
    let tray_handle: Rc<RefCell<Option<tray::TrayHandle>>> = Rc::new(RefCell::new(None));

    // ---- 환경설정 창 ----
    {
        let ctx = ctx.clone();
        let win = settings_win.as_weak();
        let unlocked = unlocked.clone();
        list.on_open_settings(move || {
            touch(&ctx);
            let Some(w) = win.upgrade() else { return };
            fill_settings_window(&ctx, &w);
            w.set_unlocked(unlocked.get());
            w.set_status(SharedString::new());
            let _ = w.show();
            set_window_icon(w.window());
        });
    }
    {
        let win = settings_win.as_weak();
        settings_win.on_close_requested(move || {
            if let Some(w) = win.upgrade() {
                let _ = w.hide();
            }
        });
    }
    {
        let ctx = ctx.clone();
        let win = settings_win.as_weak();
        let lock_weak = lock.as_weak();
        let list_weak = list.as_weak();
        let merge_timer = merge_timer.clone();
        let tray_handle = tray_handle.clone();
        settings_win.on_apply(move || {
            let Some(w) = win.upgrade() else { return };
            let (Some(lock), Some(list)) = (lock_weak.upgrade(), list_weak.upgrade()) else {
                return;
            };
            touch(&ctx);

            let mut next = Settings {
                lang: w.get_lang_sel().to_string(),
                unlock_days: w.get_unlock_days(),
                idle_lock_minutes: w.get_idle_minutes(),
                default_color: w.get_default_color().to_string(),
                default_opacity: w.get_default_opacity(),
                merge_seconds: w.get_merge_seconds(),
            };
            next.sanitize();
            let prev_unlock_days = ctx.settings.borrow().unlock_days;
            next.save(&ctx.dir);
            *ctx.settings.borrow_mut() = next.clone();

            // 다듬어진 값을 창에 되돌려 보여 준다 (범위를 벗어난 입력이 조용히 바뀌지 않도록).
            fill_settings_window(&ctx, &w);
            apply_lang(&ctx, &lock, &list, &w);
            if let Some(t) = tray_handle.borrow().as_ref() {
                t.refresh();
            }
            start_merge_timer(&merge_timer, &ctx, list.as_weak());

            // 자동 해제 기간을 줄였거나 껐다면 이미 남아 있는 세션이 그 정책을 어긴다.
            // 가장 안전한 쪽으로: 세션을 버리고 다음 실행 때 암호를 다시 받는다.
            let status = if next.unlock_days != prev_unlock_days {
                settings::clear_session(&ctx.dir);
                t!("msg.settings_saved_relock")
            } else {
                t!("msg.settings_saved")
            };
            w.set_status(SharedString::from(status));
        });
    }
    {
        let ctx = ctx.clone();
        let win = settings_win.as_weak();
        let lock_weak = lock.as_weak();
        let list_weak = list.as_weak();
        let unlocked = unlocked.clone();
        settings_win.on_lock_now(move || {
            let (Some(lock), Some(list)) = (lock_weak.upgrade(), list_weak.upgrade()) else {
                return;
            };
            lock_now(&ctx, &lock, &list, &unlocked);
            if let Some(w) = win.upgrade() {
                w.set_unlocked(false);
                let _ = w.hide();
            }
        });
    }
    {
        let ctx = ctx.clone();
        let lock_weak = lock.as_weak();
        let list_weak = list.as_weak();
        let unlocked = unlocked.clone();
        list.on_lock_now(move || {
            let (Some(lock), Some(list)) = (lock_weak.upgrade(), list_weak.upgrade()) else {
                return;
            };
            lock_now(&ctx, &lock, &list, &unlocked);
        });
    }

    // ---- 자리 비움 자동 잠금 ----
    let idle_timer = slint::Timer::default();
    {
        let ctx = ctx.clone();
        let lock_weak = lock.as_weak();
        let list_weak = list.as_weak();
        let unlocked = unlocked.clone();
        idle_timer.start(TimerMode::Repeated, IDLE_CHECK_INTERVAL, move || {
            let minutes = ctx.settings.borrow().idle_lock_minutes;
            if minutes <= 0 || !unlocked.get() {
                return;
            }
            if ctx.last_activity.get().elapsed() < Duration::from_secs(minutes as u64 * 60) {
                return;
            }
            let (Some(lock), Some(list)) = (lock_weak.upgrade(), list_weak.upgrade()) else {
                return;
            };
            lock_now(&ctx, &lock, &list, &unlocked);
        });
    }

    // ---- 자석 스냅: 스티커가 멈추면 화면/다른 스티커 테두리에 달라붙는다 ----
    // (창 위치 제어가 되는 X11 에서만 동작; Wayland 면 좌표 읽기가 실패해 자동 무효)
    let snap_timer = slint::Timer::default();
    {
        let stickies = ctx.stickies.clone();
        snap_timer.start(TimerMode::Repeated, SNAP_INTERVAL, move || snap_tick(&stickies));
    }

    // ---- 트레이 아이콘 ----
    APP.with(|a| {
        *a.borrow_mut() = Some(AppUi {
            lock: lock.clone_strong(),
            list: list.clone_strong(),
            unlocked: unlocked.clone(),
            ctx: ctx.clone(),
        })
    });
    // 핸들이 살아 있는 동안만 트레이가 유지된다 (tray_handle 이 main 끝까지 산다).
    *tray_handle.borrow_mut() = Some(tray::start());

    // 창 아이콘은 이벤트 루프가 돌기 시작해 winit 창이 생긴 뒤에야 적용된다.
    // 시작 직후 한 번(단발 타이머) 잠금·목록 창에 심고, 이후 새로 뜨는 창은 각 show
    // 지점에서 직접 심는다(open_sticky / 트레이 토글).
    let icon_timer = slint::Timer::default();
    {
        let lock_w = lock.as_weak();
        let list_w = list.as_weak();
        icon_timer.start(TimerMode::SingleShot, Duration::from_millis(100), move || {
            if let Some(w) = lock_w.upgrade() {
                set_window_icon(w.window());
            }
            if let Some(w) = list_w.upgrade() {
                set_window_icon(w.window());
            }
        });
    }

    // 자동 해제로 이미 목록이 떠 있으면 잠금 창을 다시 띄우지 않는다.
    if !auto_unlocked {
        lock.show()?;
    }
    slint::run_event_loop_until_quit()?;
    Ok(())
}
