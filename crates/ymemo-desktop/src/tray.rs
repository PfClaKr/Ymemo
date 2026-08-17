//! 트레이 아이콘. 플랫폼별 백엔드를 cfg 로 나눈다.
//!
//!  - **Linux**: `ksni` (StatusNotifierItem, D-Bus 기반 순수 Rust — gtk/libdbus 불필요)
//!  - **Windows**: `tray-icon` (Shell_NotifyIcon; gtk 를 끌어오지 않도록 default-features off)
//!  - **그 외(macOS 등)**: 미구현 → 트레이 없이 동작 (핸들만 반환)
//!
//! 공통 진입점 [`start`] 는 살아 있는 동안 트레이를 유지하는 핸들을 돌려준다
//! (핸들이 drop 되면 트레이도 사라진다). 클릭/메뉴는 백엔드가 아래 `request_*` 로
//! 이벤트 루프에 위임한다 — 트레이 콜백은 UI 스레드가 아니고, slint 컴포넌트는
//! `Send` 가 아니라 클로저에 직접 캡처할 수 없기 때문이다.

// TrayHandle 은 start() 의 반환 타입이라 공개하되, main 은 이름 없이 추론해 쓴다.
#[allow(unused_imports)]
pub use imp::{start, TrayHandle};

use slint::ComponentHandle;

use crate::icon::set_window_icon;
use crate::lock::lock_now;
use crate::state::{touch, APP};

/// 트레이 클릭/메뉴: 잠겨 있으면 잠금 창, 아니면 목록 창 토글.
/// (트레이 콜백이 어느 스레드에서 오든 이벤트 루프로 넘긴 뒤 thread_local 로 UI 접근)
pub(crate) fn request_toggle() {
    let _ = slint::invoke_from_event_loop(|| {
        APP.with(|a| {
            let borrow = a.borrow();
            let Some(app) = borrow.as_ref() else { return };
            touch(&app.ctx);
            if !app.unlocked.get() {
                let _ = app.lock.show();
                set_window_icon(app.lock.window());
                app.lock.window().request_redraw();
            } else if app.list.window().is_visible() {
                let _ = app.list.hide();
            } else {
                let _ = app.list.show();
                set_window_icon(app.list.window());
                // Windows 소프트웨어 렌더러는 다시 띄운 창을 바로 안 그려서 리프레시 전까지
                // 하얗게 남는다 → 표시 직후 강제로 다시 그린다.
                app.list.window().request_redraw();
            }
        });
    });
}

/// 트레이 "잠금": 지금 잠근다 (목록 창의 🔒 와 같은 동작).
pub(crate) fn request_lock() {
    let _ = slint::invoke_from_event_loop(|| {
        APP.with(|a| {
            let borrow = a.borrow();
            let Some(app) = borrow.as_ref() else { return };
            lock_now(&app.ctx, &app.lock, &app.list, &app.unlocked);
        });
    });
}

/// 트레이 "종료": 이벤트 루프를 끝내 앱을 종료한다.
pub(crate) fn request_quit() {
    let _ = slint::invoke_from_event_loop(|| {
        let _ = slint::quit_event_loop();
    });
}

// ===========================================================================
// Linux: ksni (StatusNotifierItem)
// ===========================================================================
#[cfg(target_os = "linux")]
mod imp {
    use super::{request_lock, request_quit, request_toggle};
    use crate::icon::tray_icon_rgba;
    use ymemo_i18n::t;

    struct YmemoTray;

    impl ksni::Tray for YmemoTray {
        fn id(&self) -> String {
            "dev.ymemo.Ymemo".into()
        }

        fn title(&self) -> String {
            "Ymemo".into()
        }

        fn icon_pixmap(&self) -> Vec<ksni::Icon> {
            // ksni 는 ARGB32(네트워크 바이트 순서) 를 요구 → RGBA 를 ARGB 로 변환.
            let (rgba, w, h) = tray_icon_rgba();
            let mut data = vec![0u8; rgba.len()];
            for (px, out) in rgba.chunks_exact(4).zip(data.chunks_exact_mut(4)) {
                out[0] = px[3]; // A
                out[1] = px[0]; // R
                out[2] = px[1]; // G
                out[3] = px[2]; // B
            }
            vec![ksni::Icon {
                width: w as i32,
                height: h as i32,
                data,
            }]
        }

        fn activate(&mut self, _x: i32, _y: i32) {
            request_toggle();
        }

        fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
            use ksni::menu::*;
            vec![
                StandardItem {
                    label: t!("tray.memo_list"),
                    activate: Box::new(|_| request_toggle()),
                    ..Default::default()
                }
                .into(),
                StandardItem {
                    label: t!("tray.lock"),
                    activate: Box::new(|_| request_lock()),
                    ..Default::default()
                }
                .into(),
                MenuItem::Separator,
                StandardItem {
                    label: t!("tray.quit"),
                    activate: Box::new(|_| request_quit()),
                    ..Default::default()
                }
                .into(),
            ]
        }
    }

    /// 살아 있는 동안 트레이를 유지하는 핸들. (spawn 실패 시 None)
    pub struct TrayHandle(Option<ksni::blocking::Handle<YmemoTray>>);

    impl TrayHandle {
        /// 메뉴를 다시 그리게 한다 (언어가 바뀌었을 때).
        pub fn refresh(&self) {
            if let Some(h) = &self.0 {
                h.update(|_| {});
            }
        }
    }

    pub fn start() -> TrayHandle {
        use ksni::blocking::TrayMethods;
        match YmemoTray.spawn() {
            Ok(handle) => TrayHandle(Some(handle)),
            Err(e) => {
                eprintln!("트레이 아이콘 등록 실패 (트레이 없이 계속): {e}");
                TrayHandle(None)
            }
        }
    }
}

// ===========================================================================
// Windows: tray-icon (Shell_NotifyIcon)
// ===========================================================================
#[cfg(windows)]
mod imp {
    use std::time::Duration;

    use tray_icon::menu::{Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem};
    use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

    use super::{request_lock, request_quit, request_toggle};
    use crate::icon::tray_icon_rgba;
    use ymemo_i18n::t;

    /// 트레이 아이콘 + 메뉴/클릭 이벤트를 폴링하는 타이머를 함께 들고 있는 핸들.
    /// (둘 다 살아 있어야 트레이가 유지되고 이벤트가 처리된다)
    pub struct TrayHandle {
        _tray: Option<TrayIcon>,
        _poll: slint::Timer,
        /// 언어가 바뀔 때 문구를 갈아 끼우기 위해 항목을 들고 있는다.
        items: Vec<MenuItem>,
    }

    impl TrayHandle {
        /// 메뉴 문구를 현재 언어로 다시 쓴다. 순서는 start() 의 append 순서와 같다.
        pub fn refresh(&self) {
            let labels = [t!("tray.memo_list"), t!("tray.lock"),
                          t!("tray.quit")];
            for (item, label) in self.items.iter().zip(labels) {
                item.set_text(label);
            }
        }
    }

    pub fn start() -> TrayHandle {
        // 메뉴: "메모 목록" / "잠금" / --- / "종료". 각 항목 id 로 이벤트를 분기한다.
        let list_item = MenuItem::new(t!("tray.memo_list"), true, None);
        let lock_item = MenuItem::new(t!("tray.lock"), true, None);
        let quit_item = MenuItem::new(t!("tray.quit"), true, None);
        let list_id: MenuId = list_item.id().clone();
        let lock_id: MenuId = lock_item.id().clone();
        let quit_id: MenuId = quit_item.id().clone();

        let menu = Menu::new();
        // append 는 muda(=tray_icon::menu) 의 Result 를 돌려준다 (tray_icon::Result 아님).
        let build_menu = || -> tray_icon::menu::Result<()> {
            menu.append(&list_item)?;
            menu.append(&lock_item)?;
            menu.append(&PredefinedMenuItem::separator())?;
            menu.append(&quit_item)?;
            Ok(())
        };

        let (rgba, w, h) = tray_icon_rgba();
        let tray = Icon::from_rgba(rgba, w, h)
            .map_err(|e| format!("아이콘 생성 실패: {e}"))
            .and_then(|icon| build_menu().map(|_| icon).map_err(|e| format!("메뉴 구성 실패: {e}")))
            .and_then(|icon| {
                TrayIconBuilder::new()
                    .with_menu(Box::new(menu))
                    // 좌클릭은 목록 토글, 우클릭은 메뉴. (메뉴가 좌클릭을 가로채지 않게)
                    .with_menu_on_left_click(false)
                    .with_tooltip("Ymemo")
                    .with_icon(icon)
                    .build()
                    .map_err(|e| format!("트레이 생성 실패: {e}"))
            });

        let tray = match tray {
            Ok(t) => Some(t),
            Err(e) => {
                eprintln!("트레이 아이콘 등록 실패 (트레이 없이 계속): {e}");
                None
            }
        };

        // 이벤트 루프 스레드에서 주기적으로 채널을 비우며 클릭/메뉴를 처리한다.
        // (Windows 는 트레이 메시지를 winit 메시지 펌프가 넘겨주고, 여기서 수신)
        let poll = slint::Timer::default();
        poll.start(slint::TimerMode::Repeated, Duration::from_millis(120), move || {
            while let Ok(ev) = MenuEvent::receiver().try_recv() {
                if ev.id == list_id {
                    request_toggle();
                } else if ev.id == lock_id {
                    request_lock();
                } else if ev.id == quit_id {
                    request_quit();
                }
            }
            while let Ok(ev) = TrayIconEvent::receiver().try_recv() {
                // 좌클릭 한 번(버튼을 뗀 순간) → 목록 토글.
                if let TrayIconEvent::Click {
                    button: MouseButton::Left,
                    button_state: MouseButtonState::Up,
                    ..
                } = ev
                {
                    request_toggle();
                }
            }
        });

        TrayHandle {
            _tray: tray,
            _poll: poll,
            items: vec![list_item, lock_item, quit_item],
        }
    }
}

// ===========================================================================
// 그 외 플랫폼 (macOS 등): 트레이 미구현
// ===========================================================================
#[cfg(not(any(target_os = "linux", windows)))]
mod imp {
    pub struct TrayHandle;

    impl TrayHandle {
        pub fn refresh(&self) {}
    }

    pub fn start() -> TrayHandle {
        eprintln!("이 플랫폼에는 트레이 백엔드가 없어 트레이 없이 동작합니다.");
        TrayHandle
    }
}
