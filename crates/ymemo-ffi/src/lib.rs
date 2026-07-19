//! Ymemo FFI (Flutter 모바일용).
//!
//! `flutter_rust_bridge_codegen` 이 [`api`] 모듈의 공개 함수를 스캔해 Dart 바인딩을
//! 생성한다 (설정: `apps/mobile/flutter_rust_bridge.yaml`). codegen 을 돌리면
//! `src/frb_generated.rs` 가 이 crate 에 추가되는데, 그 파일은 생성물이므로
//! 커밋하지 않는다 (모바일 빌드 파이프라인에서 생성).

pub mod api;
