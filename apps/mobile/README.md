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

`screencap` returns a **black** frame whenever `lock_on_background` is on, because that is
what `FLAG_SECURE` is for. Write the setting off before concluding the app is broken:

```bash
adb shell 'run-as dev.ymemo.ymemo_mobile sh -c "cat > app_flutter/settings.json"' <<'JSON'
{"lang":"ko","unlock_days":0,"lock_on_background":false,"update_check":false,"last_update_check":0}
JSON
```

Biometrics **do** work on the emulator, which is worth knowing before hand-testing the
fingerprint unlock. Enrol one once, then `finger touch` stands in for the sensor:

```bash
adb shell locksettings set-pin 1234       # a screen lock has to exist first
adb shell am start -a android.settings.SECURITY_SETTINGS
#   Device unlock > Pixel Imprint, then re-enter the PIN and agree
adb emu finger touch 1                    # ~20 times to enrol, once per prompt afterwards
```

The enrolment screens and `BiometricPrompt` itself are `FLAG_SECURE`, so `screencap` is black
throughout. `adb shell uiautomator dump /sdcard/ui.xml` reads them anyway — it goes through
the accessibility tree — and is the way to find out what the prompt is currently saying.
Tapping the prompt's *Cancel* with `input tap` does not register; `input keyevent KEYCODE_BACK`
dismisses it.

Widgets are worth driving from the launcher rather than reasoning about. There is no adb
command that places one, so: long-press the wallpaper, **Widgets**, search "Ymemo", expand,
then drag a preview out with `input motionevent` (a plain `input swipe` is too fast to
register as a long press). The home screen itself is not `FLAG_SECURE`, so it screenshots
even while the app will not. What the widgets are currently drawing is:

```bash
adb shell run-as dev.ymemo.ymemo_mobile cat shared_prefs/dev.ymemo.widget.xml
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

## Plugin constraints

Two of the plugins reach into the platform side, so a change there can break them:

- **`local_auth` needs a `FragmentActivity`.** Its prompt is a `BiometricPrompt`, which is a
  fragment, so `MainActivity` extends `FlutterFragmentActivity` rather than the template's
  `FlutterActivity`. Nothing else depends on the difference — but changing it back would break
  the fingerprint unlock at runtime, not at compile time.
- **`local_auth_android` is a direct dependency**, unusually for a platform package, and only
  for `AndroidAuthMessages`: the prompt's own title and cancel button are the plugin's English
  defaults otherwise, on a dialog whose other half comes from our catalog. It moves with
  `local_auth`; bump the two together.

## Screens

- **Lock** — open the vault with the master password, creating it if needed. Reachable from
  here, before any password: **Sync devices**, because a new install has to pair and receive
  `vault.json` before there is anything to unlock. With biometric unlock on, the fingerprint
  prompt comes up by itself as the screen opens — reaching for a finger is why the setting was
  turned on — and a button under the password field asks again if it was dismissed.
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
- **Settings** — language, locking (including the fingerprint switch, which is where the key
  it releases is stored), updates, and the running version. Everything applies as it is
  changed; Rust sanitizes on write and the screen shows what was actually kept.

## Home-screen widgets

Three, in `android/app/src/main/kotlin/dev/ymemo/ymemo_mobile/widget/`:

| Widget | Size | What it is |
|---|---|---|
| **Quick note** | 4x1 | A write bar. Tapping it opens a new memo with the keyboard up; the camera button opens one straight into the photo picker. |
| **Sticky** | 2x2, resizable | One memo, in its own paper colour. Which one is asked when it is added (`NoteConfigureActivity`) and changed again from the "..." on the widget; the default is "whatever was edited last". |
| **Memos** | 4x2, resizable | The folders, then every memo, most recently edited first. Tapping a row opens that memo or folder; the header adds one. |

Long-pressing the launcher icon offers the same two "new memo" actions as shortcuts
(`res/xml/shortcuts.xml`), for people who would rather not give up a home-screen row.

### Why the widgets read a copy

**A widget is drawn by the launcher, in the launcher's process, while this app may not be
running at all.** It cannot open the vault — that needs the master key — and it must not read
`ymemo.db` either: `Vault::rebuild()` clears that cache and re-materializes it from the logs
on every merge, so a widget reading it mid-rebuild would show an empty list.

So the app **publishes a snapshot** (`lib/home_widgets.dart` -> `widgetPublish` on the
`dev.ymemo/native` channel -> `WidgetStore`, a private SharedPreferences file) after every
reload, which is after every change to a memo or folder. The widgets only ever read the last
one published. Three consequences worth knowing:

- It is a **second plaintext copy** of titles and body previews, next to the plaintext cache.
  It is emptied whenever the vault closes, so a locked app leaves nothing on the home screen —
  see SECURITY.md.
- `updatePeriodMillis` is `0` on all three. Android's own period is capped at half an hour and
  would wake the app to redraw something that has not changed; the app says when instead.
  `WidgetRefreshReceiver` covers the two cases it cannot: the app being replaced (which
  otherwise leaves every widget on the launcher's "updating" placeholder until it is next
  opened) and the system language changing.
- The widget picker's labels and everything a widget draws that is not memo text live in
  `res/values/strings.xml` and `res/values-ko/`, **not** in `i18n/*.json` — Android reads them
  without the app running, so they follow the *system* language rather than the app's setting.
  It is the one place in the app where a string is written twice.

In the other direction a tapped widget starts `MainActivity` with an action in its **extras**
(a collection widget's rows share one `PendingIntent` template and can differ only by their
fill-in intent's extras, so everything uses extras for consistency; launcher shortcuts cannot
carry extras and arrive as actions of their own). Dart collects it with `takeWidgetAction` at
startup, and through `widgetAction` while it is running. **A request that arrives while the
vault is locked waits** in `pendingWidgetRequest` through the password screen rather than
being dropped.

### The launcher icon

The same dog-eared note every other platform shows. Two files here:

- `res/drawable/ic_launcher_foreground.xml` — the adaptive icon (API 26+), on the flat gold of
  `ic_launcher_background`, plus `ic_launcher_monochrome.xml` for Android 13's themed icons.
  This vector is the source of the geometry.
- `res/mipmap-<density>/ic_launcher.png` — API 24-25, so the same phone cannot end up with two
  different icons. **Generated**, together with the desktop's `packaging/assets`, by
  `packaging/gen_icons.py` at the repo root; re-run it after changing the vector, and keep
  `crates/ymemo-desktop/src/icon.rs` (which draws the same picture in code for the tray) in
  step by hand.

`tool/gen_widget_previews.py` draws the widget-picker previews for a similar reason:
Android 12+ renders the real layout (`previewLayout`), everything older needs a PNG.

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

**The key for this project was created on 2026-09-05** (RSA 2048, alias `ymemo`, valid
until 2054), before the first release anyone could update from. It is **not** in the
repository and never will be:

```
~/.ymemo-signing/ymemo.jks           the keystore
~/.ymemo-signing/password.txt        its password, used for both the store and the key
~/.ymemo-signing/ymemo.jks.base64    the same file, ready to paste as a CI secret
```

Its certificate is `SHA-256 42:8E:05:37:CA:8F:D0:2C:05:0D:F4:56:53:09:C1:CC:9F:CA:A3:B9:97:CA:FA:69:47:DE:47:8F:42:9B:04:BA`
— `apksigner verify --print-certs <apk>` on any release build should print that digest, and a
build that prints anything else was signed with the debug key.

**Back all three up somewhere that is not this machine.** Losing them means no future build
can ever update the installs that exist; there is no way to re-issue an Android signing key.
Once they are safe, the password belongs in a password manager rather than in a file beside
the key it opens.

Making one, if it ever has to be done again for a different project:

```bash
keytool -genkeypair -v -keystore ymemo.jks -keyalg RSA -keysize 2048 -validity 10000 \
  -alias ymemo -dname "CN=Ymemo, O=Ymemo, C=KR"
```

Local release builds read it from `apps/mobile/android/key.properties` (gitignored), which is
already written on this machine:

```properties
storeFile=/absolute/path/to/ymemo.jks
storePassword=…
keyAlias=ymemo
keyPassword=…
```

CI reads the same four values from repository secrets — `ANDROID_KEYSTORE_BASE64` (the
contents of `ymemo.jks.base64`), `ANDROID_KEYSTORE_PASSWORD`, `ANDROID_KEY_ALIAS` (`ymemo`),
`ANDROID_KEY_PASSWORD` — and warns in the log when they are missing. Either way a build
without a keystore still succeeds, falling back to the debug key: fine for testing, never for
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

- **Unlock with a fingerprint** (default off) is the third, and it is the same trade again
  rather than a different one. Biometrics cannot derive a key, so the switch stores a second
  copy of the data key — in the same keystore-backed storage, under `biometric_vault_key`,
  **with no expiry** — and `local_auth` puts the device's own prompt in front of reading it.
  It is turned on from inside the unlocked app, which is the only moment there is a key to
  store, and the fingerprint is checked once there so a switch that cannot work never ends up
  looking on. Turning it off deletes the copy, and so does resetting the vault.

  The key deliberately survives locking, unlike the session: opening the lock again is the
  entire point. A stored key that no longer opens the vault is dropped and the password takes
  over, exactly as a diverged session key is.

**While a stored key exists the master password buys nothing** — anyone who can unlock the
phone can read the memos. That is the inherent cost of not typing it every time, the same one
the desktop pays, and it is why 0 days stays the default and the setting says so on screen.

Cloud backup and device transfer are **off** (`allowBackup="false"` plus
`res/xml/data_extraction_rules.xml`). Android's default would have copied the app's private
directory — including the *plaintext* SQLite cache of every memo — into the user's cloud
backup. A restored backup would also plant a stale vault beside a live one; pairing is how a
new device is supposed to get the memos.

## Remaining work

- [ ] Show this device's pairing code as a QR too, so the desktop is not left with typing.
- [ ] cargokit integration, so gradle and Xcode build the Rust automatically.
- [ ] Idle auto-lock while the app is open (the desktop's `idle_lock_minutes`) — **decided
      against for now**: on a phone the screen lock already covers walking away, and leaving
      it out keeps one switch rather than two doing nearly the same thing.
