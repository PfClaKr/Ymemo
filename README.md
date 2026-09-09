# Ymemo

**English** · [한국어](README.ko.md)

Sticky notes for your desk and your phone. They sync directly between your own devices —
end-to-end encrypted, with **no server of anyone's in between**, and no account to sign up for.

![Ymemo on the desktop](docs/screenshots/desktop.png)

- **Yours** — every device holds a complete copy, and the app works with the network off.
- **No middleman** — devices talk to each other. There is no cloud account to cancel or be
  locked out of.
- **Encrypted end to end** — anything that leaves a device is ciphertext. Your master password
  never leaves it at all.
- **Nothing is lost** — two devices edited at once merge into one note instead of one winning.

![A memo typed on one device appearing on the other](docs/screenshots/sync.gif)

## Getting it

| | Download | Notes |
|---|---|---|
| **Windows** | `ymemo-<version>-setup-x86_64.exe` | Installer; adds the tray app and its firewall rules. |
| **Debian / Ubuntu** | `ymemo_<version>_amd64.deb` | `sudo apt install ./ymemo_<version>_amd64.deb` |
| **Fedora** | `ymemo-<version>-*.x86_64.rpm` | `sudo dnf install ./ymemo-<version>-*.rpm` |
| **Android** | `ymemo-<version>-android-arm64-v8a.apk` | Most phones are `arm64-v8a`. Sideloaded, so Android asks once for permission to install it. |

All of them are on the [releases page](https://github.com/PfClaKr/Ymemo/releases/latest).
Nothing else has to be installed — the sync daemon ships inside.

The app checks about once a day whether a newer release exists and points at the one file for
the machine it is on. It never downloads or installs anything on its own, and the check can be
switched off; see [what leaves your device](#what-leaves-your-device).

## Starting out

On first run the app asks one question, and the answer matters:

<img src="docs/screenshots/setup.png" alt="The first-run screen" width="330">

- **Start fresh** — your first device. Choose a master password and start writing.
- **Connect to another device** — you already use Ymemo somewhere, and this brings those memos
  over. Choosing "start fresh" here instead would give the new device a key of its own, and the
  two could never merge.

Right after creating a vault you get a **recovery code**. It is shown once, and with the
password it is one of only two things on earth that can open your memos — there is no reset
link, because there is nobody to send one.

Click the heading to give the vault a name. It travels with the memos, so every device you
connect shows the same one.

## What you can do with it

- **Notes that behave like notes.** Frameless stickies that save as you type. Double-click the
  title bar to fold one; drag it and it snaps to the screen edges and to the other notes. They
  stay out of the taskbar, and the tray brings them back. Each carries its own colour and
  opacity, and the **pin** keeps one above everything.
- **Markdown where you ask for it.** Ordinary lines stay exactly as typed — ` **stars** ` in a
  shopping list are stars. Formatting turns on inside a bare ```` ``` ```` fence, and a fence
  that names a language, ```` ```rust ````, gives you a coloured code block.
- **Photos on the paper.** Drop one on a note and move or resize it, or give it a band of its
  own under the writing so it covers nothing. Sizes travel, so a photo you shrank on a phone
  looks the same next to the writing on a 27-inch monitor.
- **Folders.** Nestable, drag and drop, with colours of their own. Deleting one keeps what was
  in it and lifts it up a level.
- **An order that sticks.** Drag a memo between two rows to place it, or onto a folder to file
  it. The arrangement is per folder and it syncs.
- **Finding one again.** A find box that looks through every folder at once, and through what
  is written in a note rather than only its first line.
- **A way back.** Deleting offers the memo straight back for a few seconds instead of asking
  "are you sure". And every past version of every note and folder is kept — what it said, when,
  and which device changed it — and any of them can be put back.
- **Your phone too.** The same memos, folders, photos and history, plus three home-screen
  widgets and two shortcuts. They go blank the moment the app locks.
- **Locking.** A master password, an instant lock, an idle auto-lock, and optionally staying
  unlocked for a set number of days. Android can also reopen with your fingerprint.
- **Connecting a device.** Scan its QR code or type its pairing code; both screens show the
  same eight characters to compare. From the third device on, each one introduces the others,
  so they all reach each other directly. Removing a device is recorded in the vault, so every
  device drops it — and it stays removed until you connect it again. A change reaches the
  other device in about twenty seconds; Settings > Advanced trades battery for speed.
- **Korean and English.** Follows the system language, and can be changed in settings.

<img src="docs/screenshots/mobile.png" alt="Ymemo on Android" width="620">

![Version history](docs/screenshots/history.png)

## What leaves your device

Your memos leave it only as ciphertext, only to the devices you have paired, and only over
connections those devices make between themselves.

The app makes exactly **one** request to a server of anyone's: a daily question to GitHub about
whether a newer release exists. It carries no vault data, no device id and nothing that
identifies you — but your address does reach GitHub, so it can be switched off in settings.

Syncing may pass through Syncthing's public relays when two devices cannot reach each other
directly. Relays forward encrypted bytes and cannot read them.

What the encryption does and does not cover — including that the local cache on each device is
**plaintext**, so a device someone else can read while it is unlocked has no secrets from them
— is written out in [SECURITY.md](SECURITY.md).

---

## For developers

A Rust core (`ymemo-core`) with two thin UIs on it: Slint on the desktop and Flutter on
Android, four crates in one Cargo workspace.

**Requirements**

- Rust **>= 1.87**
- Linux: `libfontconfig1-dev` (Slint links fontconfig) and `fonts-noto-cjk` for Korean text
- Android only: Flutter and the Android SDK/NDK — see
  [apps/mobile/README.md](apps/mobile/README.md)

**Build and run**

```bash
cargo test --workspace
cargo run -p ymemo-desktop
```

App data lives in the platform data directory (`~/.local/share/ymemo` on Linux,
`%APPDATA%\ymemo\Ymemo\data` on Windows), of which only the encrypted `vault/` is synced. Set
`YMEMO_DATA_DIR` to put it somewhere else — which is how you try a build without opening the
vault you actually use. `ymemo --purge` deletes this device's copy and nothing on your other
devices.

Packaging — the `.deb`, the `.rpm`, the Inno Setup installer and the per-ABI APKs — is scripted
under `packaging/` and built from a `v*` tag by
[.github/workflows/release.yml](.github/workflows/release.yml).

### Still to come

- macOS: tray and packaging
- iOS

### License

GPL-3.0-only
