# Ymemo

A memo app that runs on several devices (Linux, Windows, Android) and **syncs between them
with no server of its own**. It stores text notes and photos.

## Design: local-first

Every device holds a **complete local copy**, and syncing sits on top of that.

- **Local-first** — fully usable offline; the data is on your own devices.
- **P2P** — devices sync directly, with no central server.
- **E2E encrypted** — anything that travels the sync path is ciphertext.
- **CRDT** — concurrent edits on several devices merge automatically, losing nothing.

## Stack

| Layer | Choice | Notes |
|---|---|---|
| Shared core | **Rust** (`ymemo-core`) | data model, storage, CRDT, crypto, sync |
| Desktop UI | **Slint** (`ymemo-desktop`) | pure Rust GUI, no webview, tray-resident sticky notes |
| Mobile UI | **Flutter** (`apps/mobile`, in progress) | Android/iOS, core over `flutter_rust_bridge` (`ymemo-ffi`) |
| Local store | **SQLite** (`rusqlite`, bundled) | a materialized view per device |
| CRDT | **Automerge** | order-independent merging, which is what P2P needs |
| Crypto | **RustCrypto** | XChaCha20-Poly1305 + Argon2id; pure Rust, so cross-compiling needs no system libraries |
| i18n | **own catalog** (`ymemo-i18n`) | one set of `i18n/*.json` shared by core, desktop and mobile (ko/en) |
| Sync transport | **bundled Syncthing** | delegates discovery, NAT traversal and relaying; shipped as a binary (gomobile library on mobile) and driven over its REST API |

### How syncing works

Syncthing is only the **courier for encrypted files**; everything above it is `ymemo-core`.

```
[ymemo-core / Rust]  memos, CRDT merging, E2E encryption
        |  writes encrypted change logs into the vault directory
[Syncthing]          carries that directory between devices (discovery, NAT, relay, TLS)
```

Vault layout:

```
vault/
├── vault.json              # Argon2id salts + the wrapped data key + a key-check canary
├── logs/<device-id>.ymlog  # per-device append-only encrypted log; a record is an automerge change
└── blobs/<sha256>.ymblob   # attached photos, encrypted. The name is the content hash, so nothing conflicts.
```

Logs and blobs are encrypted with a random **data key**, and the master password only wraps
that key inside `vault.json`. So changing the password rewrites one small field instead of
re-encrypting every memo, and the other devices carry on undisturbed — they just ask for the
new password the next time they unlock.

A device only ever appends to **its own** log file, so two devices never touch the same file:
zero file-level conflicts, with the CRDT merging the contents. Photos work the same way —
named by content hash and therefore immutable, and encrypted convergently, so the same photo
attached on two devices ends up as one byte-identical file.

For what the encryption does and does not cover — including that **the local cache is
plaintext** — see [SECURITY.md](SECURITY.md).

## Repository layout

```
Ymemo/
├── Cargo.toml            # Rust workspace
├── rust-toolchain.toml   # stable (>= 1.87 required)
├── i18n/                 # translation catalog: ko.json (source) and en.json
├── crates/
│   ├── ymemo-core/       # shared core: model, SQLite cache, crypto, automerge vault, Syncthing, pairing
│   ├── ymemo-desktop/    # Slint desktop app
│   │   ├── ui/           # one .slint per screen (app = entry; lock/list/sticky/settings/pairing/theme)
│   │   └── src/          # main (wiring), state, sticky, list, lock, pairing, sync, tray
│   ├── ymemo-i18n/       # catalog loader (the t! macro)
│   └── ymemo-ffi/        # FFI for mobile (flutter_rust_bridge)
├── apps/mobile/          # Flutter app (Android in progress)
└── packaging/            # .deb / .rpm / Inno Setup scripts and icons
```

## Build and run

Requires **Rust >= 1.87** (`rustup update stable`).

### Linux system dependencies

Slint links `fontconfig` on Linux:

```bash
sudo apt install libfontconfig1-dev
```

> Without the dev package, a pkg-config shim pointing at an installed `libfontconfig.so.1`
> plus `PKG_CONFIG_PATH` also works (local `.cargo/config.toml`, gitignored).

Displaying CJK text needs fonts: `sudo apt install fonts-noto-cjk`

### Commands

```bash
cargo test --workspace
```

```bash
cargo run -p ymemo-desktop
```

```bash
# Deletes this device's memos, settings and vault, and nothing on your other devices.
# The Windows uninstaller offers the same thing; on Linux a package may not touch $HOME.
ymemo --purge
```

Choosing a master password on first run creates the vault. App data lives in the platform
data directory (`~/.local/share/Ymemo` on Linux), and only the encrypted `vault/` is synced.

During development the `syncthing` on your PATH is used as-is; releases ship it renamed to
`ymemo-sync` inside the installer (see below).

### Packaging

syncthing is bundled inside the installer, so **users never install it separately** and it is
removed with the app. They never have to know it is there: no GUI is opened and the process
shows up as `ymemo-sync`.

```bash
# Debian/Ubuntu .deb (local test); pass the path to a syncthing binary.
packaging/linux/build-deb.sh \
  --app target/release/ymemo-desktop --sync /path/to/syncthing \
  --version 0.1.0 --outdir dist
```

```bash
# Fedora .rpm — needs rpmbuild, so build it in a Fedora container.
packaging/linux/build-rpm.sh \
  --app target/release/ymemo-desktop --sync /path/to/syncthing \
  --version 0.1.0 --outdir dist
```

Windows uses Inno Setup (`packaging/windows/ymemo.iss`) to produce
`ymemo-setup-x86_64.exe`. CI builds all three from a release tag (`v*`), see
`.github/workflows/release.yml`. The same tag also produces **Android APKs**, split per ABI —
most phones want `ymemo-<version>-android-arm64-v8a.apk`. They are still signed with a debug
key, so they are not for the Play Store (see `apps/mobile/README.md`). The Fedora job builds
the desktop inside a Fedora container for library compatibility.

## Features (desktop)

- **Tray-resident stickies** — the tray icon toggles the list, and each memo gets a frameless
  sticky window. The body is the editor (autosaved), double-clicking the title bar folds it,
  and dragging a window snaps it to the screen edges and to other stickies.
- **Appearance** — per-memo color palette and window opacity.
- **Photos** — attach one and resize it. The display size syncs in **em (font multiples)**
  rather than pixels, so shrinking it on a phone shrinks it proportionally on the desktop.
- **Groups** — a nestable folder tree with drag and drop. Folders carry a colour of their
  own, and it syncs like a memo's.
- **Locking** — master password, instant lock from the tray, idle auto-lock, and optionally
  staying unlocked for a set period (a device-local session key, never synced).
- **Password and recovery** — change the master password from the settings window, and keep a
  recovery code for the day it is forgotten. There is no other way back: nothing but the
  password and that code can decrypt a vault, on any device. Failing both, the lock screen
  can wipe this device and start over.
- **Device linking** — by QR/pairing code, or by a **6-digit code** on the same LAN. Linked
  devices can be listed and revoked.
- **Korean and English** — detected from the system locale, changeable in settings.
- **Update check** — asks GitHub about once a day whether a newer release exists, and says so
  in the list window and settings. It only ever tells you: nothing is downloaded and nothing
  is installed. This is the **only** request the app makes to anyone's server — no vault data,
  no device id, nothing identifying travels with it — but your address does reach GitHub, so
  it can be switched off in settings.

## Roadmap

- [x] **Phase 0** — workspace scaffolding, `ymemo-core` SQLite CRUD, Slint sticky windows
- [x] **Phase 1** — Argon2id key derivation and encrypted change-log storage (RustCrypto)
- [x] **Phase 2** — CRDT (Automerge) merging into the SQLite cache and the UI
- [x] **Phase 3** — bundled Syncthing with REST control, device pairing
- [x] **Phase 4a** — desktop locking (manual, idle, timed auto-unlock), settings, ko/en
- [x] Packaging and CI — `.deb`, `.rpm` and a Windows installer from a release tag
- [x] **Phase 4b** — photo attachments (encrypted blobs, platform-independent display size)
- [x] Flutter mobile app, with the same bundled Syncthing and both pairing paths
- [ ] Mobile update notice and a mobile settings screen to switch it off
- [ ] macOS support (tray, packaging)

## License

GPL-3.0-only
