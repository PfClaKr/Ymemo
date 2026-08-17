//! 콜백들이 공유하는 앱 상태.
//!
//! Slint 콜백은 서로 독립적으로 등록되므로, 필요한 것들을 `Ctx` 하나로 묶어
//! 클론해 넘긴다(전부 `Rc` 라 클론은 싸다).

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Instant;

use slint::VecModel;
use ymemo_core::vault::Vault;

use crate::settings::Settings;
use crate::{ListRow, ListWindow, LockWindow, StickyWindow};

pub(crate) type SharedVault = Rc<RefCell<Option<Vault>>>;

/// 열려 있는 스티커 창 하나의 상태.
pub(crate) struct StickyEntry {
    pub(crate) window: StickyWindow,
    /// 편집 디바운스 저장 타이머 (창과 수명을 같이한다).
    pub(crate) save_timer: slint::Timer,
    /// 아직 저장 안 된 편집이 있는가. 있으면 병합 타이머가 본문을 덮어쓰지 않는다.
    pub(crate) dirty: Rc<Cell<bool>>,
    /// 지난 스냅 틱에서 관측한 창 위치 (물리 px). 이동 종료 감지용.
    pub(crate) last_pos: Cell<Option<(i32, i32)>>,
    /// 지난 틱 대비 위치가 바뀌었는가(=드래그 중). 멈춘 순간 한 번만 스냅한다.
    pub(crate) moving: Cell<bool>,
    /// 제목 바로 끌고 있는 동안 잡은 지점(창 좌상단 기준, 물리 px). None = 끌기 아님.
    pub(crate) drag_grab: Cell<Option<(i32, i32)>>,
}

pub(crate) type Stickies = Rc<RefCell<HashMap<String, StickyEntry>>>;

/// 콜백들이 공유하는 앱 상태 묶음.
#[derive(Clone)]
pub(crate) struct Ctx {
    pub(crate) vault: SharedVault,
    pub(crate) model: Rc<VecModel<ListRow>>,
    pub(crate) stickies: Stickies,
    /// 접어 둔 그룹 id. **기기 로컬 보기 상태**라 동기화하지 않는다
    /// (없으면 펼침 — 새 기기에서도 내용이 바로 보이도록).
    pub(crate) collapsed: Rc<RefCell<HashSet<String>>>,
    /// 앱 데이터 디렉터리. settings.json / session.json 이 여기 있다.
    pub(crate) dir: Rc<PathBuf>,
    /// 기기 로컬 환경설정 (언어, 잠금 정책, 새 메모 기본값 …).
    pub(crate) settings: Rc<RefCell<Settings>>,
    /// 사용자가 마지막으로 앱을 건드린 시각. 자리 비움 자동 잠금이 이걸 본다.
    pub(crate) last_activity: Rc<Cell<Instant>>,
}

/// "방금 사용자가 앱을 건드렸다"고 표시한다 (자리 비움 자동 잠금 타이머를 되돌린다).
///
/// OS 전역 입력 훅이 아니라 앱 콜백에서 찍는 방식이라, 정확히는 "앱과의 상호작용"이
/// 기준이다. 창을 띄워 두고 마우스만 지나가는 건 활동으로 치지 않는다.
pub(crate) fn touch(ctx: &Ctx) {
    ctx.last_activity.set(Instant::now());
}

// 트레이 콜백(별도 스레드)이 invoke_from_event_loop 로 넘어온 뒤 UI 에 닿기 위한 통로.
// slint 컴포넌트는 Send 가 아니라 클로저에 직접 캡처할 수 없다.
thread_local! {
    pub(crate) static APP: RefCell<Option<AppUi>> = const { RefCell::new(None) };
}

pub(crate) struct AppUi {
    pub(crate) lock: LockWindow,
    pub(crate) list: ListWindow,
    pub(crate) unlocked: Rc<Cell<bool>>,
    /// 트레이 "잠금" 이 잠금 절차를 그대로 돌릴 수 있도록.
    pub(crate) ctx: Ctx,
}
