# Ymemo 모바일 (Flutter)

Rust 코어(`crates/ymemo-ffi`)를 `flutter_rust_bridge` 로 쓰는 Android/iOS 앱.

플랫폼 디렉터리(`android/`)와 바인딩 생성물(`lib/src/rust/`,
`crates/ymemo-ffi/src/frb_generated.rs`)은 아래 절차로 만든 뒤 **커밋한다** —
codegen 이 `ymemo-ffi` 의 `lib.rs`·`Cargo.toml` 까지 고치므로, 생성물을 빼 두면
Flutter 가 없는 CI 에서 `cargo test --workspace` 가 깨진다. `build/`·`.gradle/`·
`jniLibs/` 처럼 순수 빌드 산출물만 gitignore 한다.

## 최초 설정

한 번만 하면 되는 절차다. 순서를 지킬 것 — 특히 codegen CLI 버전.

```bash
# 1. 시스템 도구 (WSL/Ubuntu 기준)
sudo apt install -y openjdk-17-jdk unzip     # Gradle 8 은 JDK 17
#    Flutter SDK: https://docs.flutter.dev/get-started/install/linux
#      (snap 말고 tar 를 풀어 PATH 에. 설치 후 `flutter doctor --android-licenses`)
#    Android SDK/NDK: Android Studio 또는 cmdline-tools. NDK 경로를 ANDROID_NDK_HOME 에.

# 2. Rust 쪽 (CI 의 android-libs 잡과 같은 조합)
rustup target add aarch64-linux-android armv7-linux-androideabi x86_64-linux-android
cargo install cargo-ndk
# CLI 는 pubspec.yaml 의 flutter_rust_bridge 와 **정확히 같은 버전**이어야 한다.
cargo install flutter_rust_bridge_codegen --version 2.11.1

# 3. 플랫폼 디렉터리 생성 (apps/mobile 에서)
#    주의: flutter create 가 lib/main.dart·pubspec.yaml 을 덮어쓸 수 있다.
#    실행 뒤 반드시 `git status` 로 확인하고, 덮였으면 `git checkout --` 로 되돌릴 것.
flutter create --platforms=android --org dev.ymemo --project-name ymemo_mobile .
flutter pub get

# 4. Dart/Rust 바인딩 생성 (설정: flutter_rust_bridge.yaml)
flutter_rust_bridge_codegen generate

# 5. 생성 결과를 한 커밋으로 남긴다 (Cargo.toml/lib.rs 변경도 함께!)
cd ../.. && cargo test -p ymemo-ffi   # 바인딩이 붙은 채로 컴파일되는지 먼저 확인
```

## Android 실행/빌드

```bash
# Rust → .so (NDK 필요, ANDROID_NDK_HOME 설정).
# 개발 중엔 쓰는 기기의 ABI 하나만 굽는 게 훨씬 빠르다 (실기기 대부분 arm64-v8a,
# 에뮬레이터는 x86_64). --release 없이 디버그로 구우면 빌드가 더 빠르다.
cargo ndk -t arm64-v8a -o android/app/src/main/jniLibs build -p ymemo-ffi

flutter run            # 연결된 기기/에뮬레이터
```

릴리스 APK 는 세 ABI 를 다 굽고 만든다:

```bash
cargo ndk -t arm64-v8a -t armeabi-v7a -t x86_64 \
  -o android/app/src/main/jniLibs build --release -p ymemo-ffi
flutter build apk
```

> Dart 를 고칠 때마다 `.so` 를 다시 구울 필요는 없지만, **Rust 를 고치면 반드시 다시 구워야
> 한다** — gradle 은 아직 Rust 를 모른다(cargokit 미적용, 아래 "남은 일").

### 에뮬레이터 (헤드리스, WSL2 에서 검증됨)

창 없이 띄우고 `adb` 로 조작·캡처하는 방식이라 WSLg 없이도 되고 메모리도 덜 먹는다.

```bash
# 1. 준비 (한 번만). KVM 가속에 /dev/kvm 접근 권한이 필요하다:
#    sudo usermod -aG kvm $USER   (적용은 WSL 재시작 후. 즉시 쓰려면 sudo chmod 666 /dev/kvm)
sdkmanager "emulator" "system-images;android-36;google_apis;x86_64"
avdmanager create avd -n ymemo -k "system-images;android-36;google_apis;x86_64" -d pixel_6

# 2. 기동 → 부팅 대기
emulator -avd ymemo -no-window -no-audio -no-boot-anim -gpu swiftshader_indirect -memory 4096 &
adb wait-for-device && until [ "$(adb shell getprop sys.boot_completed | tr -d '\r')" = 1 ]; do sleep 5; done

# 3. 설치 → 실행 → 화면 확인
cargo ndk -t x86_64 -o android/app/src/main/jniLibs build -p ymemo-ffi
flutter build apk --debug --target-platform android-x64
adb install -r build/app/outputs/flutter-apk/app-debug.apk
adb shell am start -n dev.ymemo.ymemo_mobile/.MainActivity
adb exec-out screencap -p > /tmp/shot.png      # 화면 캡처
adb shell input tap 540 1212                   # 좌표는 1080x2400 기준
adb shell input text "hunter2"                 # 공백은 %s 로 넣는다
```

vault 가 실제로 기기에 생겼는지는 앱 전용 디렉터리에서 확인한다(디버그 빌드만 가능):

```bash
adb shell run-as dev.ymemo.ymemo_mobile ls -l app_flutter/vault app_flutter/vault/logs
# vault.json (salt + key_check) + logs/<uuid>.ymlog (암호화된 change 로그)
```

에뮬레이터 종료: `adb emu kill`

### WSL2 에서 실기기 붙이기

- **실기기(권장)** — Android 11+ 의 무선 디버깅으로 `adb pair` → `adb connect`.
  USB 로 하려면 Windows 쪽 `usbipd-win` 으로 장치를 WSL 에 넘겨야 한다.
- **에뮬레이터** — 이 머신은 `/dev/kvm` 이 있어 WSL 안에서도 뜨지만 메모리를 많이 먹는다.
  WSL 에 할당된 RAM 이 8GB 미만이면 `.wslconfig` 에서 늘리거나 실기기를 쓸 것.

## iOS 실행/빌드 (macOS)

iOS 는 아직 착수하지 않았다. 시작할 때 플랫폼 디렉터리부터 만든다
(`flutter create --platforms=ios .`) — 그때 `android/` 와 같은 이유로 커밋 대상이 된다.

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

## 릴리스 (CI)

`v*` 태그를 밀면 `.github/workflows/release.yml` 의 `android-app` 잡이 APK 를 만들어
GitHub Release 에 붙인다. **ABI 별로 나눠서** 올린다:

| 파일 | 대상 | 크기 |
|---|---|---|
| `ymemo-<버전>-android-arm64-v8a.apk` | **요즘 폰 대부분** | ~29 MB |
| `ymemo-<버전>-android-armeabi-v7a.apk` | 구형 32비트 폰 | ~24 MB |
| `ymemo-<버전>-android-x86_64.apk` | 에뮬레이터 | ~32 MB |

셋을 한 APK 에 담으면 76 MB 라 나눴다. 크기의 상당 부분은 Flutter 엔진(~12 MB)과
QR 스캔용 ML Kit(`libbarhopper_v3.so`, ~5 MB)이고, 우리 Rust 라이브러리는 ABI 당 3~5 MB 다.

> **서명 주의:** 지금은 `flutter create` 기본값대로 릴리스 APK 가 **디버그 키로 서명**된다.
> 설치는 되지만(기기에서 "출처를 알 수 없는 앱" 허용 필요) Play 스토어에는 올릴 수 없고,
> 나중에 정식 키로 바꾸면 **기존 설치본을 덮어 업데이트할 수 없다**(서명이 달라 재설치해야 함).
> 정식 서명은 keystore 를 secrets 로 받아 `signingConfigs` 를 채우면 된다
> (windows-desktop 잡의 인증서 처리와 같은 방식).

CI 가 쓰는 Flutter·NDK 버전은 `release.yml` 상단 `env` 에 못 박혀 있다. NDK 는
`android/app/build.gradle.kts` 의 `ndkVersion` 과 **같은 값이어야** 한다.

## 남은 일

- [ ] Syncthing 모바일 연동 (gomobile `.aar` 번들 — 결정 사항). **이게 붙기 전까지 모바일은
      로컬 전용**이다 — vault 디렉터리에 파일이 도착할 방법이 없어 🔄 도 할 일이 없다.
- [ ] 그룹(폴더) 화면 — FFI(`group_*`)는 이미 있고 Dart UI 만 없다.
- [ ] QR 스캔으로 페어링 코드 읽기 (`mobile_scanner` 등, `pairing_decode` 와 연결)
- [ ] cargokit 통합으로 Rust 빌드를 gradle/Xcode 에 자동 연결
- [ ] 자동 잠금/세션 정책 (데스크탑의 `settings.rs` 에 해당하는 것)
