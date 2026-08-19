//! Ymemo FFI for the Flutter mobile app.
//!
//! `flutter_rust_bridge_codegen` scans the public functions of [`api`] and generates the
//! Dart bindings (configured in `apps/mobile/flutter_rust_bridge.yaml`).
//!
//! `frb_generated` is its output — **do not edit by hand.** After changing `api.rs`, rerun
//! `flutter_rust_bridge_codegen generate` to refresh both this file and
//! `apps/mobile/lib/src/rust/`. Generated or not, it is committed, so the crate still
//! compiles on CI machines without Flutter.
#[rustfmt::skip]
mod frb_generated;

pub mod api;
