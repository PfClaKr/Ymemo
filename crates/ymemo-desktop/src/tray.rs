//! 트레이 아이콘. 플랫폼별 백엔드를 cfg 로 나눈다.
//!
//!  - **Linux**: `ksni` (StatusNotifierItem, D-Bus 기반 순수 Rust — gtk/libdbus 불필요)
//!  - **Windows**: `tray-icon` (Shell_NotifyIcon; gtk 를 끌어오지 않도록 default-features off)
//!  - **그 외(macOS 등)**: 미구현 → 트레이 없이 동작 (핸들만 반환)
//!
//! 공통 진입점 [`start`] 는 살아 있는 동안 트레이를 유지하는 핸들을 돌려준다
//! (핸들이 drop 되면 트레이도 사라진다). 클릭/메뉴는 백엔드가
//! [`crate::request_toggle`] / [`crate::request_quit`] 로 이벤트 루프에 위임한다.

// TrayHandle 은 start() 의 반환 타입이라 공개하되, main 은 이름 없이 추론해 쓴다.
#[allow(unused_imports)]
pub use imp::{start, TrayHandle};

// ===========================================================================
// Linux: ksni (StatusNotifierItem)
// ===========================================================================
#[cfg(target_os = "linux")]
mod imp {
    use crate::{request_quit, request_toggle, tray_icon_rgba};

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
                    label: "메모 목록".into(),
                    activate: Box::new(|_| request_toggle()),
                    ..Default::default()
                }
                .into(),
                MenuItem::Separator,
                StandardItem {
                    label: "종료".into(),
                    activate: Box::new(|_| request_quit()),
                    ..Default::default()
                }
                .into(),
            ]
        }
    }

    /// 살아 있는 동안 트레이를 유지하는 핸들. (spawn 실패 시 None)
    pub struct TrayHandle(#[allow(dead_code)] Option<ksni::blocking::Handle<YmemoTray>>);

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

    use crate::{request_quit, request_toggle, tray_icon_rgba};

    /// 트레이 아이콘 + 메뉴/클릭 이벤트를 폴링하는 타이머를 함께 들고 있는 핸들.
    /// (둘 다 살아 있어야 트레이가 유지되고 이벤트가 처리된다)
    pub struct TrayHandle {
        _tray: Option<TrayIcon>,
        _poll: slint::Timer,
    }

    pub fn start() -> TrayHandle {
        // 메뉴: "메모 목록" / --- / "종료". 각 항목 id 로 이벤트를 분기한다.
        let list_item = MenuItem::new("메모 목록", true, None);
        let quit_item = MenuItem::new("종료", true, None);
        let list_id: MenuId = list_item.id().clone();
        let quit_id: MenuId = quit_item.id().clone();

        let menu = Menu::new();
        // append 는 muda(=tray_icon::menu) 의 Result 를 돌려준다 (tray_icon::Result 아님).
        let build_menu = || -> tray_icon::menu::Result<()> {
            menu.append(&list_item)?;
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
        }
    }
}

// ===========================================================================
// 그 외 플랫폼 (macOS 등): 트레이 미구현
// ===========================================================================
#[cfg(not(any(target_os = "linux", windows)))]
mod imp {
    pub struct TrayHandle;

    pub fn start() -> TrayHandle {
        eprintln!("이 플랫폼에는 트레이 백엔드가 없어 트레이 없이 동작합니다.");
        TrayHandle
    }
}
