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
use std::collections::HashMap;
use std::rc::Rc;
use std::time::Duration;

use anyhow::Result;
use i_slint_backend_winit::winit::dpi::PhysicalPosition;
use i_slint_backend_winit::WinitWindowAccessor;
use slint::{ComponentHandle, LogicalSize, ModelRc, SharedString, TimerMode, VecModel};
use ymemo_core::{now_millis, pairing::PairingCode, sync::Syncthing, vault::Vault, Memo, Store};

slint::include_modules!();

mod tray;

/// Syncthing 쪽에서 vault 공유 폴더를 식별하는 고정 id (모든 기기가 같은 값 사용).
const SYNC_FOLDER_ID: &str = "ymemo-vault";
/// 다른 기기의 로그 반영 주기.
const MERGE_INTERVAL: Duration = Duration::from_secs(15);
/// 본문 편집 후 자동 저장까지의 디바운스.
const SAVE_DEBOUNCE: Duration = Duration::from_millis(800);
/// 스티커 제목 바 높이 (접힌 상태의 창 높이, app.slint 와 일치해야 함).
const BAR_HEIGHT: f32 = 36.0;
/// 자석 스냅 거리 (논리 px). 이 거리 안이면 화면/다른 스티커 테두리에 달라붙는다.
const SNAP_DIST: f32 = 12.0;
/// 스티커 위치 감시(스냅 판정) 주기.
const SNAP_INTERVAL: Duration = Duration::from_millis(90);

type SharedVault = Rc<RefCell<Option<Vault>>>;

/// 열려 있는 스티커 창 하나의 상태.
struct StickyEntry {
    window: StickyWindow,
    /// 편집 디바운스 저장 타이머 (창과 수명을 같이한다).
    save_timer: slint::Timer,
    /// 아직 저장 안 된 편집이 있는가. 있으면 병합 타이머가 본문을 덮어쓰지 않는다.
    dirty: Rc<Cell<bool>>,
    /// 지난 스냅 틱에서 관측한 창 위치 (물리 px). 이동 종료 감지용.
    last_pos: Cell<Option<(i32, i32)>>,
    /// 지난 틱 대비 위치가 바뀌었는가(=드래그 중). 멈춘 순간 한 번만 스냅한다.
    moving: Cell<bool>,
}

type Stickies = Rc<RefCell<HashMap<String, StickyEntry>>>;

/// 콜백들이 공유하는 앱 상태 묶음.
#[derive(Clone)]
struct Ctx {
    vault: SharedVault,
    model: Rc<VecModel<MemoItem>>,
    stickies: Stickies,
}

// 트레이 콜백(별도 스레드)이 invoke_from_event_loop 로 넘어온 뒤 UI 에 닿기 위한 통로.
// slint 컴포넌트는 Send 가 아니라 클로저에 직접 캡처할 수 없다.
thread_local! {
    static APP: RefCell<Option<AppUi>> = const { RefCell::new(None) };
}

struct AppUi {
    lock: LockWindow,
    list: ListWindow,
    unlocked: Rc<Cell<bool>>,
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

    let lock = LockWindow::new()?;
    let list = ListWindow::new()?;

    // syncthing 은 unlock 전에 띄운다 (키가 필요 없음). 이래야 새 기기가
    // "먼저 페어링 → vault.json/로그 동기화 → 그 다음 암호 입력" 순서로
    // 기존 vault 에 합류할 수 있다.
    let dir = data_dir();
    let vault_dir = dir.join("vault");
    let _ = std::fs::create_dir_all(&vault_dir);
    let st = start_syncthing(&dir, &vault_dir);
    if let Some(st) = &st {
        match st.device_id() {
            Ok(id) => {
                let code = PairingCode::new(id).encode();
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
            }
            Err(e) => eprintln!("기기 ID 조회 실패: {e}"),
        }
    }
    // 앱 종료 시 Drop 으로 데몬도 함께 종료된다.
    let syncthing: Rc<RefCell<Option<Syncthing>>> = Rc::new(RefCell::new(st));

    let ctx = Ctx {
        vault: Rc::new(RefCell::new(None)),
        model: Rc::new(VecModel::from(Vec::<MemoItem>::new())),
        stickies: Rc::new(RefCell::new(HashMap::new())),
    };
    list.set_memos(ModelRc::from(ctx.model.clone()));
    let unlocked = Rc::new(Cell::new(false));

    // ---- 잠금 창: 마스터 암호로 vault 열기 ----
    {
        let ctx = ctx.clone();
        let lock_weak = lock.as_weak();
        let list_weak = list.as_weak();
        let unlocked = unlocked.clone();
        let dir = dir.clone();
        lock.on_unlock(move |password| {
            let lock = lock_weak.unwrap();
            if password.is_empty() {
                lock.set_lock_message("암호를 입력하세요".into());
                return;
            }
            // 캐시 DB 는 로컬 전용, vault/ 는 Syncthing 공유 폴더가 된다.
            let store = match Store::open(dir.join("ymemo.db")) {
                Ok(s) => s,
                Err(e) => {
                    lock.set_lock_message(SharedString::from(format!("캐시 열기 실패: {e}")));
                    return;
                }
            };
            // Argon2id 유도가 잠깐(수백 ms) UI 를 막지만 잠금 화면에서만 일어난다.
            match Vault::open_or_create(dir.join("vault"), password.as_bytes(), store) {
                Ok(v) => {
                    refresh_list(&v, &ctx.model);
                    *ctx.vault.borrow_mut() = Some(v);
                    unlocked.set(true);
                    let _ = lock.hide();
                    let _ = list_weak.unwrap().show();
                }
                Err(e) => {
                    lock.set_lock_message(SharedString::from(format!("{e}")));
                }
            }
        });
    }

    // ---- 목록 창: 메모 열기 / 새 메모 / 삭제 ----
    {
        let ctx = ctx.clone();
        list.on_open_memo(move |id| {
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
        list.on_delete_memo(move |id| {
            {
                let mut guard = ctx.vault.borrow_mut();
                let Some(v) = guard.as_mut() else { return };
                if let Err(e) = v.delete(&id) {
                    eprintln!("메모 삭제 실패: {e}");
                    return;
                }
                refresh_list(v, &ctx.model);
            }
            // 열려 있던 스티커 창도 정리한다.
            close_sticky(&ctx.stickies, id.as_str());
        });
    }

    // ---- 페어링 (잠금/목록 창 공용) ----
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

    // ---- 주기적 병합: 다른 기기의 로그를 목록/스티커에 반영 ----
    let merge_timer = slint::Timer::default();
    {
        let ctx = ctx.clone();
        merge_timer.start(TimerMode::Repeated, MERGE_INTERVAL, move || {
            let mut guard = ctx.vault.borrow_mut();
            let Some(v) = guard.as_mut() else { return };
            match v.rebuild() {
                Ok(()) => {
                    refresh_list(v, &ctx.model);
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
            unlocked,
        })
    });
    let _tray = tray::start(); // 핸들이 살아 있는 동안만 트레이가 유지된다

    lock.show()?;
    slint::run_event_loop_until_quit()?;
    Ok(())
}

// ---------------------------------------------------------------------------
// 스티커 창
// ---------------------------------------------------------------------------

/// 스티커 창에 띄울 본문. 구버전 메모(제목만 있고 본문이 빈)는 제목을 본문으로 승격.
fn sticky_text(memo: &Memo) -> String {
    if memo.body.is_empty() && !memo.title.is_empty() {
        memo.title.clone()
    } else {
        memo.body.clone()
    }
}

/// 본문 첫 비어있지 않은 줄 → 목록/제목 바에 쓸 제목.
fn derive_title(text: &str) -> String {
    text.lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim()
        .chars()
        .take(40)
        .collect()
}

/// 편집된 본문을 vault 에 저장한다 (제목은 첫 줄에서 유도).
fn save_memo(ctx: &Ctx, id: &str, text: &str) {
    let mut guard = ctx.vault.borrow_mut();
    let Some(v) = guard.as_mut() else { return };
    let mut memo = match v.store().get(id) {
        Ok(Some(m)) => m,
        _ => return, // 삭제된 메모의 잔여 편집은 버린다
    };
    let title = derive_title(text);
    if memo.body == text && memo.title == title {
        return;
    }
    memo.title = title;
    memo.body = text.to_string();
    memo.updated_at = now_millis();
    if let Err(e) = v.upsert(&memo) {
        eprintln!("메모 저장 실패: {e}");
        return;
    }
    refresh_list(v, &ctx.model);
    // 제목 바에도 새 제목 반영.
    if let Some(entry) = ctx.stickies.borrow().get(id) {
        entry.window.set_memo_title(SharedString::from(memo.title));
    }
}

/// 새 메모를 만들고 그 스티커 창을 연다. (목록 ＋ 버튼, 스티커 ＋ 버튼 공용)
fn new_memo(ctx: &Ctx) {
    let memo = Memo::new("", "");
    {
        let mut guard = ctx.vault.borrow_mut();
        let Some(v) = guard.as_mut() else { return };
        if let Err(e) = v.upsert(&memo) {
            eprintln!("메모 생성 실패: {e}");
            return;
        }
        refresh_list(v, &ctx.model);
    }
    if let Err(e) = open_sticky(ctx, &memo, true) {
        eprintln!("스티커 창 열기 실패: {e}");
    }
}

/// 스티커 창을 숨기고 레지스트리에서 제거를 예약한다.
/// (창 자신의 콜백 안에서 즉시 drop 하면 위험하므로 이벤트 루프 다음 턴으로 미룬다)
fn close_sticky(stickies: &Stickies, id: &str) {
    if let Some(entry) = stickies.borrow().get(id) {
        entry.save_timer.stop();
        let _ = entry.window.hide();
    }
    let stickies = stickies.clone();
    let id = id.to_string();
    slint::Timer::single_shot(Duration::ZERO, move || {
        stickies.borrow_mut().remove(&id);
    });
}

/// 메모의 스티커 창을 연다 (이미 열려 있으면 앞으로 가져오기만).
fn open_sticky(ctx: &Ctx, memo: &Memo, focus: bool) -> Result<()> {
    if let Some(entry) = ctx.stickies.borrow().get(&memo.id) {
        entry.window.show()?;
        return Ok(());
    }

    let window = StickyWindow::new()?;
    window.set_memo_title(SharedString::from(memo.title.clone()));
    window.set_memo_text(SharedString::from(sticky_text(memo)));
    window.set_sticky_color(SharedString::from(memo.color.clone()));
    window.set_sticky_opacity(memo.opacity as f32);

    let dirty = Rc::new(Cell::new(false));
    let expanded_height = Rc::new(Cell::new(0.0f32));

    // 본문 편집 → dirty 표시 + 디바운스 저장 예약.
    {
        let ctx = ctx.clone();
        let id = memo.id.clone();
        let dirty = dirty.clone();
        let weak = window.as_weak();
        window.on_edited(move |_| {
            dirty.set(true);
            let ctx2 = ctx.clone();
            let id2 = id.clone();
            let dirty2 = dirty.clone();
            let weak2 = weak.clone();
            if let Some(entry) = ctx.stickies.borrow().get(&id) {
                entry.save_timer.start(TimerMode::SingleShot, SAVE_DEBOUNCE, move || {
                    if let Some(w) = weak2.upgrade() {
                        save_memo(&ctx2, &id2, w.get_memo_text().as_str());
                        dirty2.set(false);
                    }
                });
            }
        });
    }

    // ✕ → 저장 후 창 닫기 (메모 삭제 아님).
    {
        let ctx = ctx.clone();
        let id = memo.id.clone();
        let dirty = dirty.clone();
        let weak = window.as_weak();
        window.on_close_requested(move || {
            if dirty.get() {
                if let Some(w) = weak.upgrade() {
                    save_memo(&ctx, &id, w.get_memo_text().as_str());
                }
                dirty.set(false);
            }
            close_sticky(&ctx.stickies, &id);
        });
    }

    // ＋ → 새 메모.
    {
        let ctx = ctx.clone();
        window.on_new_memo(move || new_memo(&ctx));
    }

    // 🎨 → 스티커 색 변경 (본문/제목은 유지, 색만 저장 → 기기 간 동기화).
    {
        let ctx = ctx.clone();
        let id = memo.id.clone();
        let weak = window.as_weak();
        window.on_set_color(move |key| {
            {
                let mut guard = ctx.vault.borrow_mut();
                let Some(v) = guard.as_mut() else { return };
                let Ok(Some(mut m)) = v.store().get(&id) else { return };
                if m.color == key.as_str() {
                    return;
                }
                m.color = key.to_string();
                m.updated_at = now_millis();
                if let Err(e) = v.upsert(&m) {
                    eprintln!("색 변경 실패: {e}");
                    return;
                }
                refresh_list(v, &ctx.model);
            }
            if let Some(w) = weak.upgrade() {
                w.set_sticky_color(key);
            }
        });
    }

    // 투명도 슬라이더 → 손을 뗄 때 한 번 저장 (드래그 중 미리보기는 UI 가 처리).
    {
        let ctx = ctx.clone();
        let id = memo.id.clone();
        window.on_set_opacity(move |pct| {
            let pct = ymemo_core::clamp_opacity(pct.round() as i64);
            let mut guard = ctx.vault.borrow_mut();
            let Some(v) = guard.as_mut() else { return };
            let Ok(Some(mut m)) = v.store().get(&id) else { return };
            if m.opacity == pct {
                return;
            }
            m.opacity = pct;
            m.updated_at = now_millis();
            if let Err(e) = v.upsert(&m) {
                eprintln!("투명도 변경 실패: {e}");
            }
        });
    }

    // 제목 바 드래그 → OS 네이티브 창 이동 (Wayland 에선 이 방법뿐이다).
    {
        let weak = window.as_weak();
        window.on_start_drag(move || {
            let w = weak.unwrap();
            w.window().with_winit_window(|ww| {
                let _ = ww.drag_window();
            });
        });
    }

    // 제목 바 더블클릭 → 얇은 바 접기/펴기 (창 크기를 직접 바꾼다).
    {
        let weak = window.as_weak();
        let expanded_height = expanded_height.clone();
        window.on_toggle_collapse(move || {
            let w = weak.unwrap();
            let sw = w.window();
            let scale = sw.scale_factor();
            let size = sw.size();
            let logical_w = size.width as f32 / scale;
            if w.get_collapsed() {
                w.set_collapsed(false);
                let h = expanded_height.get().max(120.0);
                sw.set_size(LogicalSize::new(logical_w, h));
            } else {
                expanded_height.set(size.height as f32 / scale);
                w.set_collapsed(true);
                sw.set_size(LogicalSize::new(logical_w, BAR_HEIGHT));
            }
        });
    }

    window.show()?;
    if focus {
        window.invoke_focus_body();
    }
    ctx.stickies.borrow_mut().insert(
        memo.id.clone(),
        StickyEntry {
            window,
            save_timer: slint::Timer::default(),
            dirty,
            last_pos: Cell::new(None),
            moving: Cell::new(false),
        },
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// 자석 스냅
// ---------------------------------------------------------------------------

/// 물리 px 사각형. (x, y, w, h)
type Rect = (i32, i32, i32, i32);

/// 스냅 타이머 한 틱: 열린 스티커 창들의 위치를 읽어 "방금 멈춘" 창을 스냅한다.
fn snap_tick(stickies: &Stickies) {
    let map = stickies.borrow();
    if map.is_empty() {
        return;
    }
    // 1) 보이는 창들의 현재 rect/scale/모니터를 읽는다 (X11 에서만 성공).
    let mut rects: Vec<(String, Rect, f32, Option<Rect>)> = Vec::new();
    for (id, e) in map.iter() {
        if !e.window.window().is_visible() {
            continue;
        }
        let got = e.window.window().with_winit_window(|ww| {
            let p = ww.outer_position().ok()?;
            let s = ww.inner_size();
            let mon = ww.current_monitor().map(|m| {
                let mp = m.position();
                let ms = m.size();
                (mp.x, mp.y, ms.width as i32, ms.height as i32)
            });
            Some(((p.x, p.y, s.width as i32, s.height as i32), ww.scale_factor() as f32, mon))
        });
        if let Some(Some((rect, scale, mon))) = got {
            rects.push((id.clone(), rect, scale, mon));
        }
    }

    // 2) 각 창: 지난 틱 대비 정지 여부로 이동 종료를 감지하고, 멈춘 순간 한 번 스냅.
    for (idx, (id, rect, scale, mon)) in rects.iter().enumerate() {
        let Some(e) = map.get(id) else { continue };
        let cur = (rect.0, rect.1);
        if e.last_pos.get() != Some(cur) {
            // 아직 움직이는 중.
            e.moving.set(true);
            e.last_pos.set(Some(cur));
            continue;
        }
        if !e.moving.get() {
            continue; // 계속 정지 상태 — 손대지 않는다.
        }
        // 방금 멈췄다 → 다른 창들 + 화면 테두리에 스냅.
        let others: Vec<Rect> = rects
            .iter()
            .enumerate()
            .filter(|(j, _)| *j != idx)
            .map(|(_, r)| r.1)
            .collect();
        let threshold = (SNAP_DIST * *scale) as i32;
        let (nx, ny) = snap_position(*rect, &others, *mon, threshold);
        if (nx, ny) != cur {
            e.window.window().with_winit_window(|ww| {
                ww.set_outer_position(PhysicalPosition::new(nx, ny));
            });
            e.last_pos.set(Some((nx, ny)));
        }
        e.moving.set(false);
    }
}

/// 활성 창 rect 를 화면 테두리·다른 창 테두리에 스냅한 새 좌표를 계산한다(순수 함수).
/// threshold 이내의 가장 가까운 후보로 각 축을 독립적으로 끌어당긴다.
fn snap_position(rect: Rect, others: &[Rect], monitor: Option<Rect>, threshold: i32) -> (i32, i32) {
    let (x, y, w, h) = rect;
    let mut xs: Vec<i32> = Vec::new();
    let mut ys: Vec<i32> = Vec::new();

    if let Some((mx, my, mw, mh)) = monitor {
        xs.push(mx); // 왼쪽 화면 테두리
        xs.push(mx + mw - w); // 오른쪽 화면 테두리
        ys.push(my); // 위 화면 테두리
        ys.push(my + mh - h); // 아래 화면 테두리
    }
    for &(ox, oy, ow, oh) in others {
        xs.push(ox + ow); // 내 왼쪽 ↔ 상대 오른쪽 (오른쪽에 인접)
        xs.push(ox - w); // 내 오른쪽 ↔ 상대 왼쪽 (왼쪽에 인접)
        xs.push(ox); // 왼쪽 정렬
        xs.push(ox + ow - w); // 오른쪽 정렬
        ys.push(oy + oh); // 아래에 인접
        ys.push(oy - h); // 위에 인접
        ys.push(oy); // 위 정렬
        ys.push(oy + oh - h); // 아래 정렬
    }

    (nearest(&xs, x, threshold), nearest(&ys, y, threshold))
}

/// 후보들 중 v 에 threshold 이내로 가장 가까운 값. 없으면 v 그대로.
fn nearest(cands: &[i32], v: i32, threshold: i32) -> i32 {
    let mut best = v;
    let mut best_dist = threshold + 1;
    for &c in cands {
        let d = (c - v).abs();
        if d <= threshold && d < best_dist {
            best_dist = d;
            best = c;
        }
    }
    best
}

// ---------------------------------------------------------------------------
// 목록 / 페어링
// ---------------------------------------------------------------------------

/// vault 캐시의 메모들을 목록 모델에 반영한다.
fn refresh_list(vault: &Vault, model: &VecModel<MemoItem>) {
    match vault.store().list() {
        Ok(memos) => {
            let items: Vec<MemoItem> = memos
                .into_iter()
                .map(|m| MemoItem {
                    id: SharedString::from(m.id),
                    title: SharedString::from(m.title),
                    color: SharedString::from(m.color),
                })
                .collect();
            model.set_vec(items);
        }
        Err(e) => eprintln!("메모 목록 조회 실패: {e}"),
    }
}

/// 상대 페어링 코드 등록 핸들러. 잠금/목록 창이 같은 로직을 쓰되
/// 결과 메시지만 자기 창에 표시하도록 `set_msg` 를 받는다.
fn pairing_handler(
    syncthing: Rc<RefCell<Option<Syncthing>>>,
    set_msg: impl Fn(SharedString) + 'static,
) -> impl Fn(SharedString) + 'static {
    move |code| {
        let guard = syncthing.borrow();
        let Some(st) = guard.as_ref() else { return };
        let msg = match PairingCode::decode(&code) {
            Ok(peer) => match st.share_folder_with(SYNC_FOLDER_ID, &peer.syncthing_device_id) {
                Ok(()) => "등록 완료. 상대 기기에서도 이 코드를 등록하세요.".to_string(),
                Err(e) => format!("등록 실패: {e}"),
            },
            Err(e) => format!("{e}"),
        };
        set_msg(SharedString::from(msg));
    }
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

// ---------------------------------------------------------------------------
// 트레이 콜백 (백엔드는 tray.rs 에서 플랫폼별 cfg 로 나뉘어 여기를 호출한다)
// ---------------------------------------------------------------------------

/// 트레이 클릭/메뉴: 잠겨 있으면 잠금 창, 아니면 목록 창 토글.
/// (트레이 콜백이 어느 스레드에서 오든 이벤트 루프로 넘긴 뒤 thread_local 로 UI 접근)
pub(crate) fn request_toggle() {
    let _ = slint::invoke_from_event_loop(|| {
        APP.with(|a| {
            let borrow = a.borrow();
            let Some(app) = borrow.as_ref() else { return };
            if !app.unlocked.get() {
                let _ = app.lock.show();
            } else if app.list.window().is_visible() {
                let _ = app.list.hide();
            } else {
                let _ = app.list.show();
            }
        });
    });
}

/// 트레이 "종료": 이벤트 루프를 끝내 앱을 종료한다.
pub(crate) fn request_quit() {
    let _ = slint::invoke_from_event_loop(|| {
        let _ = slint::quit_event_loop();
    });
}

/// 22x22 스티커 모양 트레이 아이콘의 RGBA 픽셀 (외부 에셋 없이 직접 그린다).
/// 반환: (rgba flat bytes, width, height). 백엔드가 요구 포맷으로 변환해 쓴다.
pub(crate) fn tray_icon_rgba() -> (Vec<u8>, u32, u32) {
    const S: usize = 22; // 아이콘 한 변
    const M: usize = 2; // 여백
    const F: usize = 7; // 오른쪽 위 접힌 귀퉁이 크기
    let mut data = vec![0u8; S * S * 4]; // RGBA, 기본 투명
    for y in 0..S {
        for x in 0..S {
            let in_note = (M..S - M).contains(&x) && (M..S - M).contains(&y);
            if !in_note {
                continue;
            }
            let from_right = S - M - 1 - x;
            let from_top = y - M;
            let (r, g, b) = if from_right + from_top < F {
                if from_right + from_top == F - 1 {
                    (0xb8u8, 0xa6u8, 0x3au8) // 접힌 귀퉁이 빗변
                } else {
                    continue; // 삼각형 바깥 → 투명
                }
            } else if x == M || x == S - M - 1 || y == M || y == S - M - 1 {
                (0xb8, 0xa6, 0x3a) // 테두리
            } else {
                (0xf7, 0xe9, 0x8c) // 스티커 본체
            };
            let i = (y * S + x) * 4;
            data[i] = r;
            data[i + 1] = g;
            data[i + 2] = b;
            data[i + 3] = 0xff; // A
        }
    }
    (data, S as u32, S as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    const T: i32 = 12; // threshold

    #[test]
    fn snaps_to_screen_left_edge_when_near() {
        // 창이 화면 왼쪽에서 5px 안쪽 → 왼쪽 테두리(0)로 붙는다.
        let mon = Some((0, 0, 1920, 1080));
        let (nx, ny) = snap_position((5, 300, 260, 240), &[], mon, T);
        assert_eq!(nx, 0);
        assert_eq!(ny, 300); // 세로는 후보 없음 → 유지
    }

    #[test]
    fn snaps_right_edge_to_neighbor_left() {
        // 내 오른쪽(x+w=260)이 상대 왼쪽(268)과 8px 차 → 인접하도록 x 를 8 당긴다.
        let other = (268, 300, 200, 240);
        let (nx, _) = snap_position((0, 300, 260, 240), &[other], None, T);
        assert_eq!(nx, 268 - 260); // 내 오른쪽이 상대 왼쪽에 딱 붙음
    }

    #[test]
    fn no_snap_when_far() {
        let mon = Some((0, 0, 1920, 1080));
        let other = (900, 900, 200, 200);
        let start = (500, 500, 260, 240);
        assert_eq!(snap_position(start, &[other], mon, T), (500, 500));
    }

    #[test]
    fn aligns_tops_of_adjacent_stickies() {
        // 세로로 3px 어긋난 두 창 → 위 정렬로 붙는다.
        let other = (300, 100, 200, 240);
        let (_, ny) = snap_position((0, 103, 260, 240), &[other], None, T);
        assert_eq!(ny, 100);
    }
}
