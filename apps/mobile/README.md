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

### The sync daemon

Syncthing is bundled the same way the desktop bundles it — one separate process, driven over
its REST API by `ymemo_core::sync` — with one platform quirk: since Android 10 an app may
only execute a binary from its **native library directory**, so it ships as
`jniLibs/<abi>/libsyncthing.so`. It is an executable, not a library; the name is what the
platform requires. `MainActivity` hands the path to Dart over the `dev.ymemo/native` channel.

`jniLibs/` is gitignored, so build it like the Rust library — once per checkout, and again
when `SYNCTHING_VERSION` in `release.yml` moves:

```bash
# From a syncthing source tree at the version release.yml pins:
NDK=$ANDROID_NDK_HOME/toolchains/llvm/prebuilt/linux-x86_64/bin
CGO_ENABLED=1 GOOS=android GOARCH=arm64 CC=$NDK/aarch64-linux-android24-clang \
  go build -buildmode=pie -tags "noupgrade noassets" -ldflags "-s -w -checklinkname=0" \
  -o <repo>/apps/mobile/android/app/src/main/jniLibs/arm64-v8a/libsyncthing.so ./cmd/syncthing
# x86_64 for the emulator: GOARCH=amd64, CC=$NDK/x86_64-linux-android24-clang
```

`file` on the result must say **`pie executable … interpreter /system/bin/linker64`**;
anything else will not start on a device. The workflow explains what each flag is for — none
of them is optional. A build without `libsyncthing.so` still runs: the app reports sync as
unavailable and works local-only.

**The daemon runs only while the app is in the foreground** (`lib/sync.dart`). Android freezes
background processes anyway, so the alternative is a foreground service and a permanent
notification; a memo app can sync while it is open. It starts *before* unlocking, exactly as
on the desktop, because a new device must receive `vault.json` before it has a password to
check. If the app is killed, the daemon goes with it (`PR_SET_PDEATHSIG` in the core).

Two ways to pair, both on the **Sync devices** screen:

- **Same network (6 digits)** — each device shows a code that rotates every minute; type the
  other one's into either device. One exchange registers **both** sides, so there is nothing
  to do on the other device afterwards. The screen holds a wifi multicast lock while it is
  open, because the wifi stack otherwise drops the other device's broadcast; it is released
  as soon as the screen closes or the app is backgrounded.
- **QR / code** — scan the desktop's pairing QR, or copy this device's long code into the
  desktop's manual-entry field. This half only opens *this* side; the other device has to be
  given this device's code too, which is what the message after scanning says.

### Emulator (headless)

What an emulator does and does not prove: the app, its settings, locking, the keystore session
and the update check all run there faithfully. **Sync does not** — the emulator is behind NAT,
so LAN pairing broadcasts never reach the host and a peer cannot connect back.

A run without `libsyncthing.so` is worth doing on purpose: the app must report sync as
unavailable and stay usable, which is what a debug build or an unbundled ABI looks like.

Running windowless and driving it over `adb` needs no display and less memory. Do not build
while it runs: gradle's JVM and the emulator together will get one of them killed on a 16GB
machine.

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

- **Lock** — open the vault with the master password, creating it if needed. Reachable from
  here, before any password: **Sync devices**, because a new install has to pair and receive
  `vault.json` before there is anything to unlock.
- **List** — one folder at a time: subfolders first, then its memos. Tapping a folder goes
  into it, long-pressing one offers rename and delete (delete keeps the contents and lifts
  them up a level), and long-pressing a memo moves it to another folder. Swipe to delete, +
  adds a memo **to the folder on screen**. Logs that have arrived are merged every 15s
  (`sync_rebuild`), and the sync button does it now.

  Folders are navigated rather than drawn as a tree: a phone has no room for indentation, and
  drilling down means the screen only ever asks the core for one level, which no cycle in the
  group graph can turn into an endless walk.
- **Editor** — title and body, saved on back or the check mark, and skipped when nothing
  changed.
- **Sync devices** — the 6-digit LAN code and a field for the other device's, this device's
  long pairing code (copyable), QR scanning, and the paired devices with their connection
  state.
- **Settings** — language, locking, updates, and the running version. Everything applies as it
  is changed; Rust sanitizes on write and the screen shows what was actually kept.

Strings are not kept in Dart. `mobile_strings()` hands over one set from the repo root's
`i18n/*.json`, so the screens and the core's error messages are always in the same language.
To add one, put a `mobile.*` key in both JSON files and a field in `FfiStrings` in
`crates/ymemo-ffi`.

## Releases (CI)

Pushing a `v*` tag runs the `android-app` job in `.github/workflows/release.yml`, which
builds the APKs and attaches them to the GitHub Release, **split per ABI**:

| File | For | Size |
|---|---|---|
| `ymemo-<version>-android-arm64-v8a.apk` | **most current phones** | ~50 MB |
| `ymemo-<version>-android-armeabi-v7a.apk` | older 32-bit phones | ~45 MB |
| `ymemo-<version>-android-x86_64.apk` | the emulator | ~53 MB |

One APK with all three would be far larger still. The bundled syncthing is the biggest single
piece (~20 MB per ABI, already without its web GUI), then the Flutter engine (~12 MB) and ML
Kit for QR scanning (`libbarhopper_v3.so`, ~5 MB); our Rust library is 3-5 MB.

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

## Locking and the stored key

Two settings, deliberately separate:

- **Lock when the app is left** (default on) closes the vault the moment the app goes to the
  background, and sets `FLAG_SECURE` so the app-switcher thumbnail and screenshots show
  nothing — the thumbnail is captured on the way out, so the flag has to be set in advance
  rather than at that moment. It **keeps** the stored key: this is not the user saying "ask me
  again". Turning it off drops both protections, which is the point of turning it off.
- **Stay unlocked for N days** (default 0 — ask every time) is what decides when the password
  is really needed. The key derived from it goes to `flutter_secure_storage`
  (EncryptedSharedPreferences behind a keystore-held key), never to a plain file. The expiry is
  fixed at the moment the password was typed and never extended by use. Locking from the
  settings screen, shortening the period, or the expiry passing all delete it.

**While a stored key exists the master password buys nothing** — anyone who can unlock the
phone can read the memos. That is the inherent cost of not typing it every time, the same one
the desktop pays, and it is why 0 days stays the default and the setting says so on screen.

Cloud backup and device transfer are **off** (`allowBackup="false"` plus
`res/xml/data_extraction_rules.xml`). Android's default would have copied the app's private
directory — including the *plaintext* SQLite cache of every memo — into the user's cloud
backup. A restored backup would also plant a stale vault beside a live one; pairing is how a
new device is supposed to get the memos.

## Remaining work

- [ ] **Verify sync on real hardware.** The lock, settings and update paths have been run on
      an emulator (see below), but the daemon and pairing have not: an emulator sits behind
      NAT, so it cannot exchange LAN broadcasts with anything, and this build did not bundle
      `libsyncthing.so`. Two real devices, or a device and the desktop, are what is left.
- [ ] Show this device's pairing code as a QR too, so the desktop is not left with typing.
- [ ] cargokit integration, so gradle and Xcode build the Rust automatically.
- [ ] Idle auto-lock while the app is open (the desktop's `idle_lock_minutes`) — **decided
      against for now**: on a phone the screen lock already covers walking away, and leaving
      it out keeps one switch rather than two doing nearly the same thing.
