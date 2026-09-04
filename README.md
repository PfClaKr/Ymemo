# Ymemo

Sticky notes for your desk and your phone. They sync directly between your own devices —
end-to-end encrypted, with **no server of anyone's in between**, and no account to sign up for.

![Ymemo on the desktop](docs/screenshots/desktop.png)

- **Yours** — every device holds a complete copy, and the app works with the network off.
- **No middleman** — devices talk to each other. There is no cloud account, and nothing to
  cancel or be locked out of.
- **Encrypted end to end** — anything that leaves a device is ciphertext. Your master password
  never leaves it at all.
- **Nothing is lost** — two devices edited at once merge into one note instead of one winning.

## Getting it

| | Download | Notes |
|---|---|---|
| **Windows** | `ymemo-setup-x86_64.exe` | Installer; adds the tray app and its firewall rules. |
| **Debian / Ubuntu** | `ymemo_<version>_amd64.deb` | `sudo apt install ./ymemo_<version>_amd64.deb` |
| **Fedora** | `ymemo-<version>-*.x86_64.rpm` | `sudo dnf install ./ymemo-<version>-*.rpm` |
| **Android** | `ymemo-<version>-android-arm64-v8a.apk` | Most phones are `arm64-v8a`. Sideloaded, so Android asks once for permission to install it. |

All of them are on the [releases page](https://github.com/PfClaKr/Ymemo/releases/latest).
Nothing else has to be installed — the sync daemon ships inside.

Once it is running, the app checks about once a day whether a newer release exists and offers
**the one file for the machine it is on**, by name. It never downloads or installs anything on
its own; see [what it sends](#what-leaves-your-device) below.

## Starting out

On first run the app asks one question, and the answer matters:

<img src="docs/screenshots/setup.png" alt="The first-run screen" width="330">

- **Start fresh** — your first device. Choose a master password and start writing.
- **Connect to another device** — you already use Ymemo somewhere. This brings those memos
  over. Choosing "start fresh" here instead would give the new device a key of its own, and
  the two could never merge.

Right after creating a vault you get a **recovery code**. It is shown once, and with the
password it is one of only two things on earth that can open your memos — there is no reset
link, because there is nobody to send one.

Give the vault a name by clicking the heading (`Ymemo` until you do). The name travels with
the memos, so every device you connect shows the same one.

## What you can do with it

- **Stickies on the desktop** — the tray icon toggles the list, and each memo opens as a small
  frameless note. Typing saves it. Double-click the title bar to fold it away; drag it and it
  snaps to the screen edges and to other notes.
- **Colours and opacity** — per note, and they sync.
- **Photos** — drop one on a note and move or resize it anywhere on the paper. The size is
  stored in text-height multiples, so a photo shrunk on a phone is shrunk on the desktop too.
- **Folders** — nestable, drag and drop, with colours of their own.
- **History** — every past version of a note or folder, when it changed, which device changed
  it and what it said at the time. Any of them can be put back.

  ![Version history](docs/screenshots/history.png)

- **On your phone** — the same memos, the same folders, the same photos.

  <img src="docs/screenshots/mobile.png" alt="Ymemo on Android" width="620">

- **On your home screen** — three Android widgets: a write bar that opens straight into a new
  note (or into the camera), one memo pinned as a sticky in its own colour, and your folders
  and recent notes in a list you can tap into. They go blank the moment the app locks, so a
  home screen never shows what a password is supposed to be hiding.

- **Locking** — a master password, instant lock, idle auto-lock, and optionally staying
  unlocked for a set number of days. On the phone the app can close itself the moment you
  switch away, and stay out of the app switcher.
- **Connecting a device** — scan the other one's QR code, or type its pairing code; the other
  device is asked to allow it, and both screens show the same eight characters to compare. On
  one network, a 6-digit code links them outright. Linked devices can be revoked later.
- **Korean and English** — follows the system language, changeable in settings.

## What leaves your device

Your memos leave it only as ciphertext, only to the devices you have paired, and only over
connections those devices make between themselves.

The app makes exactly **one** request to a server of anyone's: a daily question to GitHub
about whether a newer release exists. It carries no vault data, no device id and nothing that
identifies you — but your address does reach GitHub, so it can be switched off in settings.

Syncing may pass through Syncthing's public relays when two devices cannot reach each other
directly. Relays forward encrypted bytes and cannot read them.

What the encryption does and does not cover — including that the local cache on each device is
**plaintext**, so a device someone else can read while it is unlocked has no secrets from them
— is written out in [SECURITY.md](SECURITY.md).

---

## For developers

### Stack

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

#### How syncing works

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

The logs already *are* the history, so version history costs no extra storage: it is read back
out of them rather than kept alongside. The vault's name lives in the document too, which is
why renaming it on one device renames it on all of them.

For what the encryption does and does not cover — including that **the local cache is
plaintext** — see [SECURITY.md](SECURITY.md).

### Repository layout

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

### Build and run

Requires **Rust >= 1.87** (`rustup update stable`).

#### Linux system dependencies

Slint links `fontconfig` on Linux:

```bash
sudo apt install libfontconfig1-dev
```

> Without the dev package, a pkg-config shim pointing at an installed `libfontconfig.so.1`
> plus `PKG_CONFIG_PATH` also works (local `.cargo/config.toml`, gitignored).

Displaying CJK text needs fonts: `sudo apt install fonts-noto-cjk`

#### Commands

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
data directory (`~/.local/share/ymemo` on Linux, `%APPDATA%\ymemo\Ymemo\data` on Windows),
and only the encrypted `vault/` is synced. Set `YMEMO_DATA_DIR` to put it somewhere else —
a portable install, or trying out a build without opening the vault you actually use.

During development the `syncthing` on your PATH is used as-is; releases ship it renamed to
`ymemo-sync` inside the installer (see below).

#### Packaging

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

### Roadmap

- [x] **Phase 0** — workspace scaffolding, `ymemo-core` SQLite CRUD, Slint sticky windows
- [x] **Phase 1** — Argon2id key derivation and encrypted change-log storage (RustCrypto)
- [x] **Phase 2** — CRDT (Automerge) merging into the SQLite cache and the UI
- [x] **Phase 3** — bundled Syncthing with REST control, device pairing
- [x] **Phase 4a** — desktop locking (manual, idle, timed auto-unlock), settings, ko/en
- [x] Packaging and CI — `.deb`, `.rpm` and a Windows installer from a release tag
- [x] **Phase 4b** — photo attachments (encrypted blobs, platform-independent display size)
- [x] Flutter mobile app, with the same bundled Syncthing and both pairing paths
- [x] Mobile update notice and a mobile settings screen to switch it off
- [x] Android home-screen widgets and launcher shortcuts
- [ ] macOS support (tray, packaging)

### License

GPL-3.0-only
