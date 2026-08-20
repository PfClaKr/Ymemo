# Ymemo mobile (Flutter)

The Android/iOS app, using the Rust core (`crates/ymemo-ffi`) through
`flutter_rust_bridge`.

The platform directory (`android/`) and the generated bindings (`lib/src/rust/`,
`crates/ymemo-ffi/src/frb_generated.rs`) are produced by the steps below and then
**committed**: codegen also edits `ymemo-ffi`'s `lib.rs` and `Cargo.toml`, so leaving the
output out breaks `cargo test --workspace` on CI machines without Flutter. Only pure build
output (`build/`, `.gradle/`, `jniLibs/`) is gitignored.

## First-time setup

Done once, in this order — the codegen CLI version especially.

```bash
# 1. System tools (WSL/Ubuntu)
sudo apt install -y openjdk-17-jdk unzip     # Gradle 8 needs JDK 17
#    Flutter SDK: https://docs.flutter.dev/get-started/install/linux
#      (untar it onto PATH rather than using snap, then `flutter doctor --android-licenses`)
#    Android SDK/NDK: Android Studio or cmdline-tools; point ANDROID_NDK_HOME at the NDK.

# 2. Rust side (same set as CI's android-libs job)
rustup target add aarch64-linux-android armv7-linux-androideabi x86_64-linux-android
cargo install cargo-ndk
# The CLI must match flutter_rust_bridge in pubspec.yaml **exactly**.
cargo install flutter_rust_bridge_codegen --version 2.11.1

# 3. Create the platform directory (from apps/mobile)
#    Careful: flutter create can overwrite lib/main.dart and pubspec.yaml. Check
#    `git status` afterwards and `git checkout --` anything it clobbered.
flutter create --platforms=android --org dev.ymemo --project-name ymemo_mobile .
flutter pub get

# 4. Generate the Dart/Rust bindings (config: flutter_rust_bridge.yaml)
flutter_rust_bridge_codegen generate

# 5. Commit the result in one commit, Cargo.toml and lib.rs changes included.
cd ../.. && cargo test -p ymemo-ffi   # check it compiles with the bindings in place
```

## Building and running on Android

```bash
# Rust to .so (needs the NDK and ANDROID_NDK_HOME).
# While developing, building the one ABI your device uses is much faster (arm64-v8a on most
# real phones, x86_64 on the emulator), and a debug build is faster still.
cargo ndk -t arm64-v8a -o android/app/src/main/jniLibs build -p ymemo-ffi

flutter run
```

A release APK needs all three ABIs:

```bash
cargo ndk -t arm64-v8a -t armeabi-v7a -t x86_64 \
  -o android/app/src/main/jniLibs build --release -p ymemo-ffi
flutter build apk
```

> Dart changes do not need the `.so` rebuilt, but **Rust changes always do** — gradle still
> knows nothing about Rust (no cargokit; see "Remaining work").

### Emulator (headless, verified on WSL2)

Running windowless and driving it over `adb` works without WSLg and uses less memory.

```bash
# 1. One-time setup. KVM acceleration needs access to /dev/kvm:
#    sudo usermod -aG kvm $USER   (takes effect after a WSL restart; sudo chmod 666 /dev/kvm for now)
sdkmanager "emulator" "system-images;android-36;google_apis;x86_64"
avdmanager create avd -n ymemo -k "system-images;android-36;google_apis;x86_64" -d pixel_6

# 2. Start it and wait for boot
emulator -avd ymemo -no-window -no-audio -no-boot-anim -gpu swiftshader_indirect -memory 4096 &
adb wait-for-device && until [ "$(adb shell getprop sys.boot_completed | tr -d '\r')" = 1 ]; do sleep 5; done

# 3. Install, launch, look at it
cargo ndk -t x86_64 -o android/app/src/main/jniLibs build -p ymemo-ffi
flutter build apk --debug --target-platform android-x64
adb install -r build/app/outputs/flutter-apk/app-debug.apk
adb shell am start -n dev.ymemo.ymemo_mobile/.MainActivity
adb exec-out screencap -p > /tmp/shot.png      # screenshot
adb shell input tap 540 1212                   # coordinates assume 1080x2400
adb shell input text "hunter2"                 # %s for spaces
```

To confirm the vault really landed on the device (debug builds only):

```bash
adb shell run-as dev.ymemo.ymemo_mobile ls -l app_flutter/vault app_flutter/vault/logs
# vault.json (salt + key_check) and logs/<uuid>.ymlog (the encrypted change log)
```

Shut it down with `adb emu kill`.

### Real devices from WSL2

- **Real device (recommended)** — wireless debugging on Android 11+: `adb pair`, then
  `adb connect`. Over USB you have to hand the device to WSL with `usbipd-win` on the Windows
  side.
- **Emulator** — `/dev/kvm` exists on this machine so it runs inside WSL, but it is
  memory-hungry. With less than 8GB assigned to WSL, raise it in `.wslconfig` or use a real
  device.

## Building for iOS (macOS)

iOS has not been started. The platform directory comes first
(`flutter create --platforms=ios .`) and gets committed for the same reason as `android/`.

```bash
rustup target add aarch64-apple-ios aarch64-apple-ios-sim
cargo build --release --target aarch64-apple-ios -p ymemo-ffi
# Link target/aarch64-apple-ios/release/libymemo_ffi.a into the Xcode project
# (Runner > Build Phases > Link Binary With Libraries, or preferably through cargokit)
flutter build ios --no-codesign
```

## Screens

- **Lock** — open the vault with the master password, creating it if needed.
- **List** — the memos; swipe to delete, + to add, and the sync button merges whatever logs
  have arrived (`sync_rebuild`).
- **Editor** — title and body, saved on back or the check mark, and skipped when nothing
  changed.

Strings are not kept in Dart. `mobile_strings()` hands over one set from the repo root's
`i18n/*.json`, so the screens and the core's error messages are always in the same language.
To add one, put a `mobile.*` key in both JSON files and a field in `FfiStrings` in
`crates/ymemo-ffi`.

## Releases (CI)

Pushing a `v*` tag runs the `android-app` job in `.github/workflows/release.yml`, which
builds the APKs and attaches them to the GitHub Release, **split per ABI**:

| File | For | Size |
|---|---|---|
| `ymemo-<version>-android-arm64-v8a.apk` | **most current phones** | ~29 MB |
| `ymemo-<version>-android-armeabi-v7a.apk` | older 32-bit phones | ~24 MB |
| `ymemo-<version>-android-x86_64.apk` | the emulator | ~32 MB |

One APK with all three would be 76 MB. Much of the size is the Flutter engine (~12 MB) and
ML Kit for QR scanning (`libbarhopper_v3.so`, ~5 MB); our Rust library is 3-5 MB per ABI.

### Signing

**Android decides whether an update may replace an install by comparing signatures.** The
debug key `flutter create` leaves behind is generated per machine, so a CI runner makes up a
new one every build: those APKs cannot be installed over each other, and the user's only way
forward is uninstall-and-reinstall, which takes their memos with it. The keystore has to exist
before the first release anyone is expected to update, and it has to be kept — losing it means
no future build can ever update those installs.

Create one (once, and back it up somewhere safe):

```bash
keytool -genkeypair -v -keystore ymemo.jks -keyalg RSA -keysize 2048 -validity 10000 \
  -alias ymemo
```

Local release builds read it from `apps/mobile/android/key.properties` (gitignored):

```properties
storeFile=/absolute/path/to/ymemo.jks
storePassword=…
keyAlias=ymemo
keyPassword=…
```

CI reads the same four values from repository secrets — `ANDROID_KEYSTORE_BASE64`
(`base64 -w0 ymemo.jks`), `ANDROID_KEYSTORE_PASSWORD`, `ANDROID_KEY_ALIAS`,
`ANDROID_KEY_PASSWORD` — and warns in the log when they are missing. Either way, a build
without a keystore still succeeds with the debug key, which is fine for testing and never for
a release.

Uninstalling on Android is clean by construction: the app runs no background service and
Android deletes its private directory with it, so nothing is left behind.

The Flutter and NDK versions CI uses are pinned in `env` at the top of `release.yml`. The NDK
must match `ndkVersion` in `android/app/build.gradle.kts`.

## Remaining work

- [ ] Mobile Syncthing. **Until then mobile is local-only**: nothing can deliver files into
      the vault directory, so the sync button has nothing to do.
- [ ] Group (folder) screen — the FFI (`group_*`) exists, only the Dart UI is missing.
- [ ] cargokit integration, so gradle and Xcode build the Rust automatically.
- [ ] Auto-lock and session policy, the equivalent of the desktop's `settings.rs`.
