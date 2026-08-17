//! 잠금/해제 흐름과 창 전반에 걸친 반영(언어·환경설정 값).

use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use slint::{ComponentHandle, SharedString};
use ymemo_core::vault::Vault;

use crate::icon::set_window_icon;
use crate::list::refresh_list;
use crate::settings;
use crate::state::Ctx;
use crate::sticky::save_memo;
use crate::{apply_strings, ListWindow, LockWindow, SettingsWindow, Strings};

/// 현재 설정값을 환경설정 창의 입력들에 채워 넣는다 (열 때 / 저장 후 되돌려 보여줄 때).
pub(crate) fn fill_settings_window(ctx: &Ctx, win: &SettingsWindow) {
    // 워크스페이스 버전 = 릴리스 태그 버전 (release.yml 이 둘의 일치를 검사한다).
    win.set_app_version(SharedString::from(env!("CARGO_PKG_VERSION")));
    let s = ctx.settings.borrow();
    win.set_lang_sel(SharedString::from(s.lang.clone()));
    win.set_unlock_days(s.unlock_days);
    win.set_idle_minutes(s.idle_lock_minutes);
    win.set_default_color(SharedString::from(s.default_color.clone()));
    win.set_default_opacity(s.default_opacity);
    win.set_merge_seconds(s.merge_seconds);
}

/// 언어를 **모든 창**에 반영한다.
///
/// Slint 전역(`Strings`)은 컴포넌트 인스턴스마다 하나씩 생기므로 한 창만 바꾸면 나머지는
/// 옛 언어로 남는다. 스티커는 열려 있는 것 전부를 돌고, 이후 새로 열리는 스티커는
/// `open_sticky` 가 생성 직후 직접 넣는다.
pub(crate) fn apply_lang(ctx: &Ctx, lock: &LockWindow, list: &ListWindow, settings_win: &SettingsWindow) {
    // 카탈로그의 현재 언어를 먼저 바꾼다 — 이후 t!/apply_strings 가 모두 이걸 본다.
    // (코어와 트레이도 같은 전역을 보므로 별도 통지가 필요 없다.)
    ymemo_i18n::set_lang(ctx.settings.borrow().effective_lang());
    apply_strings(&lock.global::<Strings>());
    apply_strings(&list.global::<Strings>());
    apply_strings(&settings_win.global::<Strings>());
    for entry in ctx.stickies.borrow().values() {
        apply_strings(&entry.window.global::<Strings>());
    }
}

/// 암호로 잠금을 푼 직후: 설정된 기간만큼 자동 해제 세션을 남긴다.
///
/// 기간은 **암호를 넣은 시점부터** 고정이다(쓸 때마다 연장되는 방식이 아니다) — 그래야
/// "N일마다 한 번은 암호를 확인한다"가 실제로 지켜진다. 0일이면 아무것도 남기지 않는다.
pub(crate) fn start_unlock_session(ctx: &Ctx, vault: &Vault) {
    let days = ctx.settings.borrow().unlock_days;
    settings::save_session(&ctx.dir, &vault.key_bytes(), days);
}

/// 지금 잠근다: 미저장 편집을 흘려보내고, 스티커를 모두 닫고, 메모리에서 vault 를 내리고,
/// 자동 해제 세션까지 지운 뒤 잠금 창을 띄운다.
///
/// 세션을 지우는 게 핵심이다 — 남겨 두면 앱을 다시 켤 때 그대로 열려서 "잠금" 버튼이
/// 아무 의미가 없어진다.
pub(crate) fn lock_now(ctx: &Ctx, lock: &LockWindow, list: &ListWindow, unlocked: &Rc<Cell<bool>>) {
    if !unlocked.get() {
        return;
    }

    // 열려 있는 스티커의 미저장 편집을 먼저 저장한다 (borrow 를 겹치지 않도록 두 단계로).
    let ids: Vec<String> = ctx.stickies.borrow().keys().cloned().collect();
    for id in &ids {
        let pending = {
            let map = ctx.stickies.borrow();
            match map.get(id) {
                Some(e) if e.dirty.get() => {
                    e.save_timer.stop();
                    Some(e.window.get_memo_text().to_string())
                }
                Some(e) => {
                    e.save_timer.stop();
                    None
                }
                None => None,
            }
        };
        if let Some(text) = pending {
            save_memo(ctx, id, &text);
        }
    }
    for id in &ids {
        if let Some(e) = ctx.stickies.borrow().get(id) {
            let _ = e.window.hide();
        }
    }
    // 창 handle 의 drop 은 이벤트 루프 다음 턴으로 미룬다 (close_sticky 와 같은 이유).
    {
        let stickies = ctx.stickies.clone();
        slint::Timer::single_shot(Duration::ZERO, move || stickies.borrow_mut().clear());
    }

    *ctx.vault.borrow_mut() = None;
    ctx.model.set_vec(Vec::new());
    unlocked.set(false);
    settings::clear_session(&ctx.dir);

    let _ = list.hide();
    lock.invoke_clear_password();
    lock.set_lock_message(SharedString::new());
    lock.set_show_sync(false);
    let _ = lock.show();
    set_window_icon(lock.window());
    lock.window().request_redraw();
}

/// vault 를 연 뒤 공통 마무리: 목록 채우기 → ctx 에 보관 → 잠금 창 숨기고 목록 창 표시.
/// (unlock/create-vault 두 경로가 공유한다.)
pub(crate) fn apply_opened_vault(
    v: Vault,
    ctx: &Ctx,
    lock: &LockWindow,
    list_weak: &slint::Weak<ListWindow>,
    unlocked: &Rc<Cell<bool>>,
) {
    refresh_list(&v, &ctx.model, &ctx.collapsed.borrow());
    *ctx.vault.borrow_mut() = Some(v);
    unlocked.set(true);
    let _ = lock.hide();
    if let Some(list) = list_weak.upgrade() {
        let _ = list.show();
        set_window_icon(list.window());
    }
}
