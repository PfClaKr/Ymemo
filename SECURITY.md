# Security notes (threat model)

What Ymemo does and does not protect. Written down here because design reasoning scattered
across code comments is easy to misread later as "this must be safe".

In one line: **everything that travels the sync path is ciphertext; everything inside a
device is not.**

## What it protects

1. Memo contents **never leave a device in the clear** — the sync transport (Syncthing nodes,
   relay servers, the network in between) only ever sees ciphertext.
2. Without the master password, an entire copy of the vault directory reveals nothing.
3. A locked desktop app shows no memos without the password.

## Cryptography

| Item | Value |
|---|---|
| Key derivation | Argon2id (`argon2` crate defaults), 16-byte salt |
| Cipher | XChaCha20-Poly1305 (AEAD), 24-byte nonce drawn from `OsRng` per record |
| Data key | a **single random 32-byte key**, stored wrapped; one user with many devices means no per-device keys |
| Randomness | `OsRng` (data key, salts, nonces, recovery and LAN pairing codes) |

Logs and blobs are encrypted with a random **data key**. The master password never encrypts
memos itself — it derives an Argon2id key that only *wraps* the data key inside `vault.json`.
Two consequences follow:

- Changing the password rewrites one wrapper. No log, blob or other device is touched, and
  the change is instant regardless of vault size.
- **There are two credentials that open the vault**, once a recovery code is issued: the
  master password and that code. Each wraps the same data key, so a leak of either is a
  full compromise. The recovery code carries 160 bits of entropy and cannot be guessed; it
  is displayed exactly once and only its wrapper is stored, so a lost code is gone.

`vault.json` holds both salts (not secret), the wrapped key or keys, and `key_check`, a fixed
canary encrypted with the data key. The canary makes a wrong password fail immediately — and
equally means **anyone holding the vault can try passwords offline without limit**. The only
defenses are Argon2id's cost and the strength of the chosen password: a short password holds
for a short time.

Vaults from before wrapping (`version: 1`, no `wrapped_key`) used the password key as the
data key directly. They still open, and the first password change upgrades them in place —
after which a version of the app that predates wrapping can no longer open that vault.

## What lands on disk

Relative to the app data directory (`~/.local/share/Ymemo` on Linux):

| File | Synced | Contents |
|---|---|---|
| `vault/vault.json` | yes | salts, wrapped data key(s) and key_check (plain JSON; every key in it is encrypted). The one synced file that is **rewritten** — by a password change or a new recovery code |
| `vault/logs/*.ymlog` | yes | per-device append-only log; every record is **encrypted** |
| `vault/blobs/*.ymblob` | yes | attached photos, **encrypted** (file name = sha256 of the plaintext) |
| `ymemo.db` | no | **plaintext SQLite cache** — memo bodies are in it as-is |
| `session.json` | no | only with "stay unlocked" on: the **raw 32-byte key** (hex) plus an expiry; 0600 on unix |
| `settings.json` | no | device-local settings (language, lock timeouts, ...) |

Two rows matter most:

- **`ymemo.db` is not encrypted.** It is a materialized view that can be rebuilt from the
  automerge document, but the memo bodies inside it are plaintext. So **anyone who can read
  the disk can read the memos, even while the app is locked.** Full-disk encryption (LUKS,
  BitLocker, FileVault) is the real defense at this layer.
- **`session.json` bypasses the master password.** Setting the stay-unlocked window to a day
  or more leaves the key on disk for that long. Set it to 0 to be asked every time. (The
  settings window says the same thing.)

## The sync path

Syncthing is a **courier for ciphertext** and is not trusted.

- Transfers are protected by Syncthing's TLS, with our own record encryption underneath, so
  even a relay sees only ciphertext.
- Its REST API binds to localhost and is protected by an API key. The GUI is never used.
- The vault folder is shared **only with paired devices**, and the settings window can list
  and revoke them.

## Pairing

Two paths. Neither ever moves a key: a newly linked device receives the encrypted vault and
derives the same key from the synced salt plus the master password, which the user types.

- **Pairing code / QR** (`YMEMO1:<syncthing-device-id>`) — carried by the user, and **not a
  secret**: it is shown on a screen, and a Syncthing device id is public by design. Scanning
  one only registers this side. The other device sees an unknown caller, files it as a
  pending request, and asks its own user to allow or refuse it — so the link needs a
  deliberate act on **both** devices, and copying somebody's pairing code off a screen buys
  nothing on its own.
  - **Verification code.** Both screens show eight characters derived from the two device ids
    (`sha256` over the sorted pair, rendered in the recovery alphabet). It carries no secret;
    it exists so the person approving can check the request comes from the device in front of
    them rather than from someone replaying a copied pairing code. It is the same
    numeric-comparison step Bluetooth pairing uses, and skipping the comparison degrades the
    QR path to what it was before: trust that nobody else has the code.
  - A refusal is remembered only until the app restarts. Syncthing files the caller again on
    every retry, so this is what stops the prompt repeating; it is not a block list, and a
    mis-tapped "reject" is never permanent.
- **6-digit LAN code** — only a million possibilities, so the code itself derives an Argon2
  key that encrypts the exchange; a successful decryption proves the peer knows the code.
  Also: the code rotates every minute, attempts are throttled to 200ms, and the listener only
  runs while pairing mode is on. Even a wrong pairing reveals nothing, since the vault stays
  encrypted without the master password. This path needs no approval step: the host turned
  pairing mode on deliberately, and the code is a rotating shared secret rather than a public
  id.

## What it does not protect (known limits)

- **A compromised device.** Malware, a keylogger or root in another account wins. While
  unlocked, the key and plaintext are in process memory, and the key is not zeroized.
- **Metadata.** Anyone who can see the vault directory learns the number of devices (log
  files), how active each is (log sizes), when they changed (mtimes) and their Syncthing
  device ids. Only the contents are hidden.
- **An unsolicited pairing prompt.** A device id is public, so anyone holding one can make a
  request appear on that device — a nuisance, and a phishing surface if the user approves
  without comparing the verification code. Approving hands over the encrypted vault, though
  not the master password that opens it.
- **Photo metadata.** Blobs are stored **at original size**, so the number of photos and each
  file's size are visible. Encryption is also convergent (same plaintext, same ciphertext), so
  someone holding a photo can check whether that photo is in the vault by file name alone.
  This was the price of collapsing the same photo from two devices into one file: no sync
  conflicts, at this cost.
- **Deletion leaves history.** The logs are append-only, so deleting a memo leaves the old
  records in place (automerge merely marks them deleted). There is no "erase completely".
- **No device revocation.** A device that paired once has already derived the key. Unsharing
  stops further syncing but does not recall what it already has.
- **A password change does not re-key the vault.** It replaces the wrapper, not the data
  key, so a leaked *password* is closed off but a leaked *data key* — anything read out of
  `session.json` or process memory — still opens everything. Only a fresh vault sheds that.
- **A recovery code is a second full credential.** Issuing one widens the attack surface by
  design; not issuing one means a forgotten password destroys the memos on that device.
- **Wiping is local.** "Delete everything and start over" removes this device's vault after
  unsharing the folder, so paired devices keep their copies. It is not a remote wipe.
- **No forward secrecy.** One key opens the vault's past and present alike.
- **No author authentication.** Any device with the key can write any record. That is intended
  for a single-user, multi-device model, but a compromised device's forged changes are
  indistinguishable.
- **Not audited.** This is a personal project's own combination of standard algorithms; there
  is no guarantee the combination and the implementation are free of mistakes.

## Reporting a vulnerability

Please open a [GitHub issue](https://github.com/PfClaKr/Ymemo/issues). For anything sensitive,
contact the repository owner directly instead of filing a public issue.
