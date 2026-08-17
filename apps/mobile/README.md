# Ymemo 모바일 (Flutter)

Rust 코어(`crates/ymemo-ffi`)를 `flutter_rust_bridge` 로 쓰는 Android/iOS 앱.

이 디렉터리는 **소스만** 커밋한다. 플랫폼 디렉터리(`android/`, `ios/`)와
바인딩 생성물(`lib/src/rust/`, `crates/ymemo-ffi/src/frb_generated.rs`)은
아래 절차로 로컬/CI 에서 생성한다.

## 최초 설정

```bash
# 1. 도구 설치
#    Flutter SDK: https://docs.flutter.dev/get-started/install
cargo install flutter_rust_bridge_codegen cargo-ndk

# 2. 플랫폼 디렉터리 생성 (apps/mobile 에서)
flutter create --platforms=android,ios --org dev.ymemo --project-name ymemo_mobile .
flutter pub get

# 3. Dart/Rust 바인딩 생성 (설정: flutter_rust_bridge.yaml)
flutter_rust_bridge_codegen generate
```

## Android 실행/빌드

```bash
# Rust → .so (NDK 필요, ANDROID_NDK_HOME 설정)
cargo ndk -t arm64-v8a -t armeabi-v7a -t x86_64 \
  -o android/app/src/main/jniLibs build --release -p ymemo-ffi

flutter run            # 연결된 기기/에뮬레이터
flutter build apk      # 릴리스 APK
```

## iOS 실행/빌드 (macOS)

```bash
rustup target add aarch64-apple-ios aarch64-apple-ios-sim
cargo build --release --target aarch64-apple-ios -p ymemo-ffi
# 생성된 target/aarch64-apple-ios/release/libymemo_ffi.a 를 Xcode 프로젝트에 링크
# (Runner > Build Phases > Link Binary With Libraries, 또는 cargokit 통합 권장)
flutter build ios --no-codesign
```

## 화면

- **잠금** — 마스터 암호로 vault 열기(없으면 생성).
- **목록** — 메모 목록, 밀어서 삭제, ＋ 로 새 메모, 🔄 로 도착한 로그 병합(`sync_rebuild`).
- **편집** — 제목 + 본문. 뒤로 가기/✓ 로 저장하며, 바뀐 게 없으면 쓰지 않는다.

문구는 Dart 에 두지 않는다. 저장소 루트 `i18n/*.json` 을 `mobile_strings()` 로 한 벌 받아
쓰므로 코어 에러와 화면 문구의 언어가 항상 같다. 문구를 늘리려면 두 JSON 에 `mobile.*`
키를 넣고 `crates/ymemo-ffi` 의 `FfiStrings` 에 필드를 추가한다.

## 남은 일

- [ ] Syncthing 모바일 연동 (gomobile `.aar` 번들 — 결정 사항). **이게 붙기 전까지 모바일은
      로컬 전용**이다 — vault 디렉터리에 파일이 도착할 방법이 없어 🔄 도 할 일이 없다.
- [ ] 그룹(폴더) 화면 — FFI(`group_*`)는 이미 있고 Dart UI 만 없다.
- [ ] QR 스캔으로 페어링 코드 읽기 (`mobile_scanner` 등, `pairing_decode` 와 연결)
- [ ] cargokit 통합으로 Rust 빌드를 gradle/Xcode 에 자동 연결
- [ ] 자동 잠금/세션 정책 (데스크탑의 `settings.rs` 에 해당하는 것)
