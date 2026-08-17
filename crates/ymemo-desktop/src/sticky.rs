//! 스티커 창: 생성·편집 저장·닫기와 자석 스냅.
//!
//! 스티커는 메모 하나당 창 하나이고, 본문이 곧 편집칸이다(디바운스 자동 저장).
//! 스냅은 창 좌표를 읽고 쓸 수 있는 환경(X11·Windows)에서만 동작하며, 못 읽으면
//! 조용히 아무 일도 하지 않는다(네이티브 Wayland).

use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::Duration;

use anyhow::Result;
use i_slint_backend_winit::winit::dpi::PhysicalPosition;
use i_slint_backend_winit::WinitWindowAccessor;
use slint::{ComponentHandle, LogicalSize, SharedString, TimerMode};
use ymemo_core::{now_millis, Memo};
use ymemo_i18n::t;

use crate::icon::set_window_icon;
use crate::list::refresh_list;
use crate::state::{touch, Ctx, StickyEntry, Stickies};
use crate::{apply_strings, PhotoRow, StickyWindow, Strings};

/// 사진 표시 크기(em)의 기준이 되는 본문 폰트 크기(논리 px).
/// **`ui/sticky.slint` 의 본문 `font-size` 와 같아야 한다** — 이 값으로 em 을 픽셀로 바꾼다.
const BODY_FONT_PX: f64 = 13.0;

/// 본문 편집 후 자동 저장까지의 디바운스.
pub(crate) const SAVE_DEBOUNCE: Duration = Duration::from_millis(800);
/// 스티커 제목 바 높이 (접힌 상태의 창 높이, app.slint 와 일치해야 함).
pub(crate) const BAR_HEIGHT: f32 = 28.0;
/// 자석 스냅 거리 (논리 px). 이 거리 안이면 화면/다른 스티커 테두리에 달라붙는다.
pub(crate) const SNAP_DIST: f32 = 12.0;
/// 스티커 위치 감시(스냅 판정) 주기.
pub(crate) const SNAP_INTERVAL: Duration = Duration::from_millis(90);

/// 스티커 창에 띄울 본문. 구버전 메모(제목만 있고 본문이 빈)는 제목을 본문으로 승격.
pub(crate) fn sticky_text(memo: &Memo) -> String {
    if memo.body.is_empty() && !memo.title.is_empty() {
        memo.title.clone()
    } else {
        memo.body.clone()
    }
}

/// 본문 첫 비어있지 않은 줄 → 목록/제목 바에 쓸 제목.
pub(crate) fn derive_title(text: &str) -> String {
    text.lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim()
        .chars()
        .take(40)
        .collect()
}

/// 편집된 본문을 vault 에 저장한다 (제목은 첫 줄에서 유도).
pub(crate) fn save_memo(ctx: &Ctx, id: &str, text: &str) {
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
    refresh_list(v, &ctx.model, &ctx.collapsed.borrow());
    // 제목 바에도 새 제목 반영.
    if let Some(entry) = ctx.stickies.borrow().get(id) {
        entry.window.set_memo_title(SharedString::from(memo.title));
    }
}

/// 새 메모를 만들고 그 스티커 창을 연다. (목록 ＋ 버튼, 스티커 ＋ 버튼 공용)
pub(crate) fn new_memo(ctx: &Ctx) {
    touch(ctx);
    let mut memo = Memo::new("", "");
    {
        // 새 메모의 색/투명도 기본값은 환경설정에서 온다.
        let s = ctx.settings.borrow();
        memo.color = s.default_color.clone();
        memo.opacity = s.default_opacity as i64;
    }
    {
        let mut guard = ctx.vault.borrow_mut();
        let Some(v) = guard.as_mut() else { return };
        if let Err(e) = v.upsert(&memo) {
            eprintln!("메모 생성 실패: {e}");
            return;
        }
        refresh_list(v, &ctx.model, &ctx.collapsed.borrow());
    }
    if let Err(e) = open_sticky(ctx, &memo, true) {
        eprintln!("스티커 창 열기 실패: {e}");
    }
}

/// 스티커 창을 숨기고 레지스트리에서 제거를 예약한다.
/// (창 자신의 콜백 안에서 즉시 drop 하면 위험하므로 이벤트 루프 다음 턴으로 미룬다)
pub(crate) fn close_sticky(stickies: &Stickies, id: &str) {
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

/// 이 메모의 첨부 사진을 읽어 Slint 모델로 만든다.
///
/// 사진은 vault 안에서 암호문이라 **메모리에서 복호화·디코딩해** 넘긴다(평문을 임시 파일로
/// 흘리지 않는다). 아직 동기화되지 않았거나 디코딩할 수 없는 형식이면 `missing` 으로 두고
/// UI 가 그 사실을 알린다 — 빈 자리로 두면 사용자는 사진이 사라진 줄 안다.
pub(crate) fn photo_rows(ctx: &Ctx, memo_id: &str) -> Vec<PhotoRow> {
    let guard = ctx.vault.borrow();
    let Some(v) = guard.as_ref() else { return Vec::new() };
    let list = match v.store().attachments_of(memo_id) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("첨부 조회 실패: {e}");
            return Vec::new();
        }
    };

    list.into_iter()
        .map(|a| {
            let (w, h) = a.display_size(BODY_FONT_PX);
            let image = v
                .has_blob(&a.hash)
                .then(|| v.attachment_bytes(&a.hash).ok())
                .flatten()
                .and_then(|bytes| decode_image(&bytes));
            PhotoRow {
                id: a.id.into(),
                missing: image.is_none(),
                image: image.unwrap_or_default(),
                width_px: w as f32,
                height_px: h as f32,
            }
        })
        .collect()
}

/// 사진 바이트 → Slint 이미지 (RGBA8). 지원하지 않는 포맷이면 None.
fn decode_image(bytes: &[u8]) -> Option<slint::Image> {
    let decoded = image::load_from_memory(bytes).ok()?.into_rgba8();
    let (w, h) = decoded.dimensions();
    let buffer =
        slint::SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(decoded.as_raw(), w, h);
    Some(slint::Image::from_rgba8(buffer))
}

/// 열려 있는 스티커의 사진 목록을 다시 채운다 (첨부 추가·크기 변경·원격 병합 후).
pub(crate) fn refresh_photos(ctx: &Ctx, memo_id: &str) {
    let rows = photo_rows(ctx, memo_id);
    if let Some(entry) = ctx.stickies.borrow().get(memo_id) {
        entry
            .window
            .set_photos(slint::ModelRc::new(slint::VecModel::from(rows)));
    }
}

/// 메모의 스티커 창을 연다 (이미 열려 있으면 앞으로 가져오기만).
pub(crate) fn open_sticky(ctx: &Ctx, memo: &Memo, focus: bool) -> Result<()> {
    if let Some(entry) = ctx.stickies.borrow().get(&memo.id) {
        entry.window.show()?;
        set_window_icon(entry.window.window());
        return Ok(());
    }

    let window = StickyWindow::new()?;
    // 전역은 인스턴스마다 새로 생기므로, 새 스티커에도 현재 문구를 넣어 준다.
    apply_strings(&window.global::<Strings>());
    window.set_memo_title(SharedString::from(memo.title.clone()));
    window.set_memo_text(SharedString::from(sticky_text(memo)));
    window.set_sticky_color(SharedString::from(memo.color.clone()));
    window.set_sticky_opacity(memo.opacity as f32);
    window.set_photos(slint::ModelRc::new(slint::VecModel::from(photo_rows(ctx, &memo.id))));

    // 📎 → 파일 대화상자에서 사진을 골라 붙인다.
    {
        let ctx = ctx.clone();
        let id = memo.id.clone();
        window.on_add_photo(move || {
            touch(&ctx);
            let Some(path) = rfd::FileDialog::new()
                .add_filter("image", &["png", "jpg", "jpeg"])
                .set_title(t!("ui.sticky_pick_photo"))
                .pick_file()
            else {
                return; // 사용자가 취소
            };
            let bytes = match std::fs::read(&path) {
                Ok(b) => b,
                Err(e) => {
                    eprintln!("사진 읽기 실패: {e}");
                    return;
                }
            };
            // 원본 픽셀 크기는 여기서 재서 코어에 넘긴다(코어엔 디코더가 없다).
            let (w, h) = image::load_from_memory(&bytes)
                .map(|img| (img.width() as i64, img.height() as i64))
                .unwrap_or((0, 0));
            let name = path.file_name().unwrap_or_default().to_string_lossy().to_string();
            let mime = match path.extension().and_then(|e| e.to_str()) {
                Some("png") => "image/png",
                Some("jpg") | Some("jpeg") => "image/jpeg",
                _ => "",
            };
            {
                let mut guard = ctx.vault.borrow_mut();
                let Some(v) = guard.as_mut() else { return };
                if let Err(e) = v.attach(&id, &bytes, &name, mime, w, h) {
                    eprintln!("사진 붙이기 실패: {e}");
                    return;
                }
            }
            refresh_photos(&ctx, &id);
        });
    }

    // ＋/− 로 사진 표시 크기 변경. 값은 em 이라 모바일에도 같은 비율로 반영된다.
    {
        let ctx = ctx.clone();
        let id = memo.id.clone();
        window.on_resize_photo(move |photo_id, delta_em| {
            touch(&ctx);
            {
                let mut guard = ctx.vault.borrow_mut();
                let Some(v) = guard.as_mut() else { return };
                let Ok(Some(a)) = v.store().get_attachment(photo_id.as_str()) else { return };
                let next = a.width_em_milli + (delta_em * 1000.0) as i64;
                if let Err(e) = v.set_attachment_width(photo_id.as_str(), next) {
                    eprintln!("사진 크기 변경 실패: {e}");
                }
            }
            refresh_photos(&ctx, &id);
        });
    }

    let dirty = Rc::new(Cell::new(false));
    let expanded_height = Rc::new(Cell::new(0.0f32));

    // 본문 편집 → dirty 표시 + 디바운스 저장 예약.
    {
        let ctx = ctx.clone();
        let id = memo.id.clone();
        let dirty = dirty.clone();
        let weak = window.as_weak();
        window.on_edited(move |_| {
            touch(&ctx);
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
            touch(&ctx);
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
            touch(&ctx);
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
                refresh_list(v, &ctx.model, &ctx.collapsed.borrow());
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
            touch(&ctx);
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

    // 제목 바 끌기 시작. 창 좌표를 읽을 수 있으면(X11) 우리가 직접 창을 옮기며
    // 실시간 스냅을 걸고, 아니면(네이티브 Wayland) OS 이동에 맡기고 false 를 돌려준다.
    {
        let weak = window.as_weak();
        let ctx = ctx.clone();
        let id = memo.id.clone();
        window.on_begin_drag(move |px, py| {
            touch(&ctx);
            let Some(w) = weak.upgrade() else { return false };
            let sw = w.window();
            let scale = sw.scale_factor();
            let can_move = sw
                .with_winit_window(|ww| ww.outer_position().is_ok())
                .unwrap_or(false);
            if !can_move {
                sw.with_winit_window(|ww| {
                    let _ = ww.drag_window();
                });
                return false;
            }
            if let Some(e) = ctx.stickies.borrow().get(&id) {
                e.drag_grab.set(Some(((px * scale) as i32, (py * scale) as i32)));
            }
            true
        });
    }

    // 끄는 중 매 포인터 이동: 포인터를 따라갈 위치를 구한 뒤 스냅해서 창을 옮긴다.
    // 스냅된 뒤에도 포인터의 절대 위치로 다시 계산하므로, 임계값을 벗어나면 자연히 떨어진다.
    {
        let weak = window.as_weak();
        let ctx = ctx.clone();
        let id = memo.id.clone();
        window.on_drag_move(move |mx, my| {
            let Some(w) = weak.upgrade() else { return };
            let map = ctx.stickies.borrow();
            let Some(me) = map.get(&id) else { return };
            let Some(grab) = me.drag_grab.get() else { return };
            let sw = w.window();
            let scale = sw.scale_factor();
            let Some(Some((pos, size, mon))) = sw.with_winit_window(|ww| {
                let p = ww.outer_position().ok()?;
                let s = ww.inner_size();
                let mon = ww.current_monitor().map(|m| {
                    let mp = m.position();
                    let ms = m.size();
                    (mp.x, mp.y, ms.width as i32, ms.height as i32)
                });
                Some(((p.x, p.y), (s.width as i32, s.height as i32), mon))
            }) else {
                return;
            };
            // 창 위치 + 창 안 포인터 좌표 = 화면상의 포인터 위치. 여기서 잡은 지점을
            // 빼면 "스냅이 없었다면 있었을" 위치가 나온다.
            let want = (
                pos.0 + (mx * scale) as i32 - grab.0,
                pos.1 + (my * scale) as i32 - grab.1,
            );
            let others = other_rects(&map, &id);
            let threshold = (SNAP_DIST * scale) as i32;
            let (nx, ny) = snap_position((want.0, want.1, size.0, size.1), &others, mon, threshold);
            if (nx, ny) != pos {
                sw.with_winit_window(|ww| {
                    ww.set_outer_position(PhysicalPosition::new(nx, ny));
                });
                me.last_pos.set(Some((nx, ny)));
            }
        });
    }

    // 손을 뗌: 끌기 상태를 지우고, 스냅 타이머가 이걸 "방금 멈춘 창"으로 오인해
    // 한 번 더 스냅하지 않도록 관측 위치를 현재 값으로 맞춰 둔다.
    {
        let weak = window.as_weak();
        let ctx = ctx.clone();
        let id = memo.id.clone();
        window.on_drag_end(move || {
            let map = ctx.stickies.borrow();
            let Some(e) = map.get(&id) else { return };
            e.drag_grab.set(None);
            if let Some(w) = weak.upgrade() {
                if let Some(Some(p)) = w
                    .window()
                    .with_winit_window(|ww| ww.outer_position().ok().map(|p| (p.x, p.y)))
                {
                    e.last_pos.set(Some(p));
                }
            }
            e.moving.set(false);
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
    set_window_icon(window.window());
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
            drag_grab: Cell::new(None),
        },
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// 자석 스냅
// ---------------------------------------------------------------------------

/// 물리 px 사각형. (x, y, w, h)
pub(crate) type Rect = (i32, i32, i32, i32);

/// 스냅 타이머 한 틱: 열린 스티커 창들의 위치를 읽어 "방금 멈춘" 창을 스냅한다.
pub(crate) fn snap_tick(stickies: &Stickies) {
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
        // 제목 바로 끄는 중인 창은 drag-move 가 이미 실시간으로 스냅한다.
        if e.drag_grab.get().is_some() {
            e.last_pos.set(Some(cur));
            e.moving.set(false);
            continue;
        }
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

/// 나를 뺀, 화면에 보이는 다른 스티커들의 물리 px rect (끌기 중 스냅 대상).
pub(crate) fn other_rects(map: &HashMap<String, StickyEntry>, me: &str) -> Vec<Rect> {
    let mut out = Vec::new();
    for (id, e) in map.iter() {
        if id == me || !e.window.window().is_visible() {
            continue;
        }
        let got = e.window.window().with_winit_window(|ww| {
            let p = ww.outer_position().ok()?;
            let s = ww.inner_size();
            Some((p.x, p.y, s.width as i32, s.height as i32))
        });
        if let Some(Some(r)) = got {
            out.push(r);
        }
    }
    out
}

/// 활성 창 rect 를 화면 테두리·다른 창 테두리에 스냅한 새 좌표를 계산한다(순수 함수).
/// threshold 이내의 가장 가까운 후보로 각 축을 독립적으로 끌어당긴다.
pub(crate) fn snap_position(rect: Rect, others: &[Rect], monitor: Option<Rect>, threshold: i32) -> (i32, i32) {
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
pub(crate) fn nearest(cands: &[i32], v: i32, threshold: i32) -> i32 {
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
