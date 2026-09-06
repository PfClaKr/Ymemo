# Ymemo

**English** · [한국어](README.ko.md)

Sticky notes for your desk and your phone. They sync directly between your own devices —
end-to-end encrypted, with **no server of anyone's in between**, and no account to sign up for.

![Ymemo on the desktop](docs/screenshots/desktop.png)

- **Yours** — every device holds a complete copy, and the app works with the network off.
- **No middleman** — devices talk to each other. There is no cloud account, and nothing to
  cancel or be locked out of.
- **Encrypted end to end** — anything that leaves a device is ciphertext. Your master password
  never leaves it at all.
- **Nothing is lost** — two devices edited at once merge into one note instead of one winning.

![A memo typed on one device appearing on the other](docs/screenshots/sync.gif)

*Two paired devices, one vault, nothing in between. The pause is shortened here: with the
default settings a change reaches the other device in about twenty seconds, and
Settings > Advanced trades battery for speed.*

## Getting it

| | Download | Notes |
|---|---|---|
| **Windows** | `ymemo-<version>-setup-x86_64.exe` | Installer; adds the tray app and its firewall rules. |
| **Debian / Ubuntu** | `ymemo_<version>_amd64.deb` | `sudo apt install ./ymemo_<version>_amd64.deb` |
| **Fedora** | `ymemo-<version>-*.x86_64.rpm` | `sudo dnf install ./ymemo-<version>-*.rpm` |
| **Android** | `ymemo-<version>-android-arm64-v8a.apk` | Most phones are `arm64-v8a`. Sideloaded, so Android asks once for permission to install it. |

All of them are on the [releases page](https://github.com/PfClaKr/Ymemo/releases/latest).
Nothing else has to be installed — the sync daemon ships inside.

Once it is running, the app checks about once a day whether a newer release exists and offers
**the one file for the machine it is on**, by name. It never downloads or installs anything on
its own; see [what leaves your device](#what-leaves-your-device) below.

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

### Notes that behave like notes

The tray icon brings your open notes forward, and each memo opens as a small frameless sticky.
Typing saves it — there is no save button, and nothing to lose by closing the window.
Double-click the title bar to fold a note down to that bar; drag it and it snaps to the screen
edges and to the other notes, so a wall of them stays a wall rather than a pile.

Notes stay out of the taskbar, because a desk with eight notes on it is not eight
applications — and they sit among your other windows rather than over them, because a note
you have to move to read what is underneath is a note in the way. The **pin** on the title
bar is what puts one above everything, for the one you are keeping in view on purpose; the
tray brings the rest forward when they have slipped behind something. (Keeping notes out of
the taskbar needs Windows or X11; a native Wayland session has no way to ask for it, so there
they keep their buttons.)

Each note carries its own **colour** and **opacity**, and both travel with it: a note you
turned blue on the desktop is blue on the phone. The fade lifts while you are writing in a
note and comes back when you leave it, so you are never reading your own sentence through
the desktop.

### Photos on the paper

Drop a photo onto a note and move or resize it anywhere on it. The size is stored in
text-height multiples rather than pixels, so a photo you shrank on a phone is the same size
relative to the writing when you open that note on a 27-inch monitor.

Photos are encrypted like everything else and stored by content, so attaching the same picture
on two devices leaves one file rather than two.

### Folders

Nestable, drag and drop, with colours of their own. On the desktop they are a tree; on the
phone you go into one at a time, which is what a small screen has room for. Deleting a folder
keeps what was in it and lifts it up a level.

### Every past version

Every edit to a note or a folder is kept: when it changed, which device changed it, and what it
said at the time. Any of them can be put back — and putting one back is itself just another
edit, so nothing you stepped over is lost either.

![Version history](docs/screenshots/history.png)

It costs no extra storage. The encrypted logs your devices exchange *are* the history, and the
app reads it back out of them.

### On your phone

The same memos, the same folders, the same photos.

<img src="docs/screenshots/mobile.png" alt="Ymemo on Android" width="620">

### On your home screen

Three Android widgets, plus two shortcuts behind a long press on the app icon:

- a **write bar** that opens straight into a new note — or, from its camera button, into a new
  note with the photo picker already up;
- a **sticky**, one memo kept on the home screen in its own colour: either a note you picked or
  whichever you edited last;
- a **list** of your folders and your most recent notes, tap one to open it.

They go blank the moment the app locks, so a home screen never shows what a password is
supposed to be hiding.

### Locking

A master password, an instant lock, an idle auto-lock, and optionally staying unlocked for a
set number of days. On the phone the app can close itself the moment you switch away and stay
out of the app switcher — and it can reopen with your **fingerprint** instead of the password,
which trades a little of one for the other and says so where you turn it on.

### Connecting a device

Scan the other one's QR code, or type its pairing code. The other device is asked to allow it,
and both screens show the same eight characters to compare, so you can tell you paired with
what you meant to. On one network a 6-digit code links them outright. Devices can be revoked
later from either end.

### Korean and English

Follows the system language and can be changed in settings. The screens and the core's error
messages come from one catalog, so they are never in two different languages at once.

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

## Under the hood

A Rust core (`ymemo-core`) holds the data model, the storage, the merging and the crypto, and
the two UIs on top of it are thin: **Slint** on the desktop (`ymemo-desktop`, pure Rust, no
webview) and **Flutter** on Android (`apps/mobile`, reaching the core through
`flutter_rust_bridge`). Merging is **Automerge**, so two devices that both edited end up the
same whatever order the edits arrive in. Encryption is **RustCrypto** — XChaCha20-Poly1305 with
Argon2id — and the per-device **SQLite** cache is a disposable view rather than the truth.

**Syncthing carries the files and is never trusted with what is in them.** It is bundled, so
nobody has to install it separately, and it is driven over its localhost REST API:

```
[ymemo-core]   memos, CRDT merging, end-to-end encryption
      |        writes encrypted change logs into the vault directory
[Syncthing]    carries that directory between devices (discovery, NAT traversal, relays, TLS)
```

```
vault/
├── vault.json              # Argon2id salts + the wrapped data key + a key-check canary
├── logs/<device-id>.ymlog  # per-device append-only encrypted log; a record is an automerge change
└── blobs/<sha256>.ymblob   # attached photos, encrypted; the name is the content hash
```

Two properties do most of the work. Logs and blobs are encrypted with a random **data key**
that the master password only *wraps*, so changing the password rewrites one small field
instead of re-encrypting every memo. And a device only ever appends to **its own** log, so no
two devices write the same file: the transport never produces a conflict, and the CRDT merges
the contents.

### Building it

Rust **>= 1.87**, and on Linux `libfontconfig1-dev` (Slint links fontconfig) plus
`fonts-noto-cjk` for Korean text.

```bash
cargo test --workspace
cargo run -p ymemo-desktop
```

App data lives in the platform data directory (`~/.local/share/ymemo` on Linux,
`%APPDATA%\ymemo\Ymemo\data` on Windows), of which only the encrypted `vault/` is synced. Set
`YMEMO_DATA_DIR` to put it somewhere else — a portable install, or trying a build without
opening the vault you actually use. `ymemo --purge` deletes this device's copy and nothing on
your other devices.

The Android app has its own setup, build and testing notes in
[apps/mobile/README.md](apps/mobile/README.md). Packaging — the `.deb`, the `.rpm`, the Inno
Setup installer and the per-ABI APKs — is scripted under `packaging/` and built from a `v*` tag
by [.github/workflows/release.yml](.github/workflows/release.yml).

### Still to come

- macOS: tray and packaging
- iOS

### License

GPL-3.0-only
