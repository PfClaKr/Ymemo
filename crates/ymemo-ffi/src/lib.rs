//! Ymemo FFI (Flutter 모바일용).
//!
//! `flutter_rust_bridge_codegen` 이 [`api`] 모듈의 공개 함수를 스캔해 Dart 바인딩을
//! 생성한다 (설정: `apps/mobile/flutter_rust_bridge.yaml`).
//!
//! `frb_generated` 는 그 생성물이다 — **손으로 고치지 말 것.** `api.rs` 를 고친 뒤
//! `flutter_rust_bridge_codegen generate` 를 다시 돌리면 이 파일과 Dart 쪽
//! `apps/mobile/lib/src/rust/` 가 함께 갱신된다. 생성물이지만 커밋한다(그래야
//! Flutter 가 없는 CI 에서도 이 crate 가 컴파일된다).
#[rustfmt::skip]
mod frb_generated;

pub mod api;
