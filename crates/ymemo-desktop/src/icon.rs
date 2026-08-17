//! 앱/트레이 아이콘. 외부 에셋 없이 픽셀을 직접 그린다 — 디코더 의존성도,
//! 실행 파일 옆에 놓아야 하는 파일도 없다.

use i_slint_backend_winit::WinitWindowAccessor;

/// 22x22 스티커 모양 트레이 아이콘의 RGBA 픽셀 (외부 에셋 없이 직접 그린다).
/// 반환: (rgba flat bytes, width, height). 백엔드가 요구 포맷으로 변환해 쓴다.
pub(crate) fn tray_icon_rgba() -> (Vec<u8>, u32, u32) {
    note_icon_rgba(22)
}

/// 접힌 귀퉁이 스티커 아이콘을 `size`×`size` RGBA 로 직접 그린다(에셋·디코더 불필요).
/// 트레이(22px)와 창 아이콘(64px)이 같은 그림을 쓰도록 크기만 매개변수화했다.
/// 여백·테두리·접힘은 크기에 비례하므로 22px 는 기존 트레이 아이콘과 동일하게 나온다.
pub(crate) fn note_icon_rgba(size: usize) -> (Vec<u8>, u32, u32) {
    let s = size;
    let m = (size / 10).max(2); // 여백 ~10%
    let f = (size * 7 / 22).max(7); // 오른쪽 위 접힌 귀퉁이 (22px 기준 7)
    let border = (size / 22).max(1); // 테두리/빗변 두께
    let mut data = vec![0u8; s * s * 4]; // RGBA, 기본 투명
    for y in 0..s {
        for x in 0..s {
            let in_note = (m..s - m).contains(&x) && (m..s - m).contains(&y);
            if !in_note {
                continue;
            }
            let from_right = s - m - 1 - x;
            let from_top = y - m;
            let (r, g, b) = if from_right + from_top < f {
                if from_right + from_top >= f - border {
                    (0xb8u8, 0xa6u8, 0x3au8) // 접힌 귀퉁이 빗변
                } else {
                    continue; // 삼각형 바깥 → 투명
                }
            } else if x < m + border || x >= s - m - border || y < m + border || y >= s - m - border
            {
                (0xb8, 0xa6, 0x3a) // 테두리
            } else {
                (0xf7, 0xe9, 0x8c) // 스티커 본체
            };
            let i = (y * s + x) * 4;
            data[i] = r;
            data[i + 1] = g;
            data[i + 2] = b;
            data[i + 3] = 0xff; // A
        }
    }
    (data, s as u32, s as u32)
}

/// winit 창에 앱 아이콘을 심는다 (X11 `_NET_WM_ICON` / Windows 창 아이콘 → 작업표시줄·
/// alt-tab 에 반영). 이벤트 루프가 도는 중 창이 떠 있을 때만 적용되고, 그 외엔 조용히
/// 무시된다(네이티브 Wayland 는 창 아이콘 프로토콜이 없어 no-op — 스냅과 같은 제약).
pub(crate) fn set_window_icon(win: &slint::Window) {
    let (rgba, w, h) = note_icon_rgba(64);
    let Ok(icon) = i_slint_backend_winit::winit::window::Icon::from_rgba(rgba, w, h) else {
        return;
    };
    win.with_winit_window(|ww| ww.set_window_icon(Some(icon)));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn note_icon_buffer_is_well_formed() {
        // winit::Icon::from_rgba 는 len == 4*w*h 를 요구한다 → 각 크기마다 지켜져야 한다.
        for size in [16usize, 22, 32, 64] {
            let (rgba, w, h) = note_icon_rgba(size);
            assert_eq!(w, size as u32);
            assert_eq!(h, size as u32);
            assert_eq!(rgba.len(), size * size * 4);
            // 최소한 스티커 본체 색 픽셀이 하나는 있어야 한다(빈 아이콘이 아님).
            assert!(rgba.chunks(4).any(|p| p == [0xf7, 0xe9, 0x8c, 0xff]));
        }
    }
}
