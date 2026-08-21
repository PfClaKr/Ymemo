#!/usr/bin/env bash
# Ymemo .deb builder.
#
# Puts the app and a hidden copy of syncthing in one package:
#   /usr/lib/ymemo/ymemo          the app
#   /usr/lib/ymemo/ymemo-sync     syncthing, renamed so users never see it
#   /usr/bin/ymemo                symlink to ../lib/ymemo/ymemo, for PATH
#   /usr/share/applications/...   .desktop launcher
#   /usr/share/icons/hicolor/...  icons
#
# syncthing belongs to the package, so `apt remove ymemo` takes it too. User data in
# ~/.local/share/ymemo stays, as usual: even purge leaves the home directory alone. prerm
# prints where it is and that `ymemo --purge` removes it.
#
# Usage:
#   build-deb.sh --app <ymemo-desktop> --sync <syncthing> --version <x.y.z> \
#                --outdir <dir> [--arch amd64]
set -euo pipefail

APP="" ; SYNC="" ; VERSION="" ; OUTDIR="." ; ARCH="amd64"
while [ $# -gt 0 ]; do
  case "$1" in
    --app) APP="$2"; shift 2;;
    --sync) SYNC="$2"; shift 2;;
    --version) VERSION="$2"; shift 2;;
    --outdir) OUTDIR="$2"; shift 2;;
    --arch) ARCH="$2"; shift 2;;
    *) echo "unknown argument: $1" >&2; exit 2;;
  esac
done
[ -n "$APP" ] && [ -f "$APP" ] || { echo "no --app binary: $APP" >&2; exit 2; }
[ -n "$SYNC" ] && [ -f "$SYNC" ] || { echo "no --sync binary: $SYNC" >&2; exit 2; }
[ -n "$VERSION" ] || { echo "--version is required" >&2; exit 2; }

HERE="$(cd "$(dirname "$0")" && pwd)"
ASSETS="$HERE/../assets"
ROOT="$(mktemp -d)"
trap 'rm -rf "$ROOT"' EXIT

# ---- Lay out the files ----
install -Dm755 "$APP"  "$ROOT/usr/lib/ymemo/ymemo"
install -Dm755 "$SYNC" "$ROOT/usr/lib/ymemo/ymemo-sync"
mkdir -p "$ROOT/usr/bin"
ln -sf ../lib/ymemo/ymemo "$ROOT/usr/bin/ymemo"

install -Dm644 "$HERE/ymemo.desktop" "$ROOT/usr/share/applications/ymemo.desktop"
for s in 16 32 48 64 128 256; do
  install -Dm644 "$ASSETS/ymemo-$s.png" \
    "$ROOT/usr/share/icons/hicolor/${s}x${s}/apps/ymemo.png"
done
install -Dm644 "$HERE/../../LICENSE" "$ROOT/usr/share/doc/ymemo/copyright"

# ---- Dependencies ----
# dpkg-shlibdeps only sees DT_NEEDED, not the GL/xkb libraries winit and the renderer dlopen
# at runtime, so libgl1 and libxkbcommon0 are always added and duplicates dropped by name.
CALC="libc6, libfontconfig1"
if command -v dpkg-shlibdeps >/dev/null 2>&1; then
  SHLIB="$(mktemp -d)"
  mkdir -p "$SHLIB/debian"
  printf 'Source: ymemo\nPackage: ymemo\nArchitecture: %s\n' "$ARCH" > "$SHLIB/debian/control"
  if OUT="$( cd "$SHLIB" && dpkg-shlibdeps -O "$ROOT/usr/lib/ymemo/ymemo" 2>/dev/null )"; then
    GOT="${OUT#*shlibs:Depends=}"
    [ -n "$GOT" ] && CALC="$GOT"
  fi
  rm -rf "$SHLIB"
fi
# Merge the two lists, keeping the first entry's version constraint per package name.
DEPS="$(printf '%s, libgl1, libxkbcommon0' "$CALC" | awk -v RS=',' '
  { gsub(/^[ \t]+|[ \t]+$/, ""); if ($0=="") next;
    name=$0; sub(/[ \t].*$/, "", name);
    if (!(name in seen)) { seen[name]=1; out = out (out==""?"":", ") $0 } }
  END { print out }')"

# ---- control + maintainer scripts ----
mkdir -p "$ROOT/DEBIAN"
INSTALLED_KB=$(du -ks "$ROOT/usr" | cut -f1)
cat > "$ROOT/DEBIAN/control" <<EOF
Package: ymemo
Version: $VERSION
Section: utils
Priority: optional
Architecture: $ARCH
Depends: $DEPS
Recommends: fonts-noto-cjk
Installed-Size: $INSTALLED_KB
Maintainer: PfClaKr <noreply@ymemo.dev>
Homepage: https://github.com/PfClaKr/Ymemo
Description: local-first, P2P encrypted sticky notes
 Ymemo is an E2E encrypted memo app that syncs directly between devices with no server of
 its own. It bundles its sync transport (syncthing), which is removed along with it.
EOF

# Stop a running Ymemo before its files go away. Same sequence as `%preun` in ymemo.spec —
# **keep the two in step** (the spec doubles the % in `stat -c %U`, which is its only
# difference).
#
# Removing the package while the app runs used to leave both processes alive: the app keeps
# an unlinked binary running and the sync daemon keeps syncing a vault whose app is gone,
# until the user logs out. `ymemo --quit` asks the app to save and exit, and the daemon
# follows it (PR_SET_PDEATHSIG, see ymemo_core::sync).
#
# Processes are found by their executable, not the command line — /usr/bin/ymemo is a symlink
# and argv[0] is whatever the .desktop or the shell used.
cat > "$ROOT/DEBIAN/prerm" <<'EOF'
#!/bin/sh
set -e

# Only on removal; an upgrade must not close a window the user is typing in.
case "$1" in remove|purge) ;; *) exit 0;; esac

# Memos live in each user's home directory, which a package may not touch (Debian Policy
# 6.6 and the Fedora guidelines both forbid it), and prerm runs as root with no safe way to
# tell which home belongs to whom. So say where the data is instead of guessing.
# `ymemo --purge` is the one-command equivalent of the Windows installer's "delete my data
# too" prompt; it still works here, since the binary is removed only after this script.
echo "Ymemo: memos and settings stay in ~/.local/share/ymemo for each user." >&2
echo "       To delete them as well, run 'ymemo --purge' now, or remove that" >&2
echo "       directory afterwards. Copies on your other devices are unaffected." >&2

# The stop sequence, in order of politeness. It only ever touches processes running the
# packaged binaries, found through /proc/PID/exe rather than the command line: /usr/bin/ymemo
# is a symlink and argv[0] is whatever the .desktop file or the shell used. An unlinked binary
# reads back with " (deleted)" appended, hence the second form in each pattern.
PIDS=""    # every ymemo process
OWNERS=""  # the users running the app itself
for proc in /proc/[0-9]*; do
  exe=$(readlink "$proc/exe" 2>/dev/null) || continue
  case "$exe" in
    /usr/lib/ymemo/ymemo|"/usr/lib/ymemo/ymemo (deleted)")
      owner=$(stat -c %U "$proc" 2>/dev/null) || owner=""
      case " $OWNERS " in
        *" $owner "*) ;;
        *) [ -z "$owner" ] || OWNERS="$OWNERS $owner";;
      esac
      ;;
    /usr/lib/ymemo/ymemo-sync|"/usr/lib/ymemo/ymemo-sync (deleted)") ;;
    *) continue;;
  esac
  PIDS="$PIDS ${proc#/proc/}"
done
[ -n "$PIDS" ] || exit 0

# Which of them are still there. `kill -0` is a shell builtin, so polling costs nothing.
ALIVE=""
refresh_alive() {
  ALIVE=""
  for pid in $PIDS; do
    if kill -0 "$pid" 2>/dev/null; then ALIVE="$ALIVE $pid"; fi
  done
}

# Wait up to $1 tenths of a second for everything to exit on its own.
wait_for_exit() {
  i=0
  while [ $i -lt "$1" ]; do
    refresh_alive
    [ -n "$ALIVE" ] || return 0
    sleep 0.1
    i=$((i + 1))
  done
  refresh_alive
}

# 1. Ask nicely. --quit reaches the running instance over a loopback socket whose port lives
#    in that user's data directory, so it has to run as the owner of the process.
for owner in $OWNERS; do
  su -s /bin/sh -c '/usr/lib/ymemo/ymemo --quit' "$owner" >/dev/null 2>&1 || true
done
wait_for_exit 30

# 2. SIGTERM whatever ignored that. The daemon exits with the app anyway (PR_SET_PDEATHSIG,
#    see ymemo_core::sync), so this is mostly for the app itself.
if [ -n "$ALIVE" ]; then
  kill -TERM $ALIVE 2>/dev/null || true
  wait_for_exit 30
fi

# 3. Last resort, so the removal never stalls on a hung process.
if [ -n "$ALIVE" ]; then
  kill -KILL $ALIVE 2>/dev/null || true
fi
exit 0
EOF

# Refresh the icon and desktop databases, where they exist.
cat > "$ROOT/DEBIAN/postinst" <<'EOF'
#!/bin/sh
set -e
if command -v update-desktop-database >/dev/null 2>&1; then
  update-desktop-database -q /usr/share/applications || true
fi
if command -v gtk-update-icon-cache >/dev/null 2>&1; then
  gtk-update-icon-cache -qtf /usr/share/icons/hicolor || true
fi
exit 0
EOF
cat > "$ROOT/DEBIAN/postrm" <<'EOF'
#!/bin/sh
set -e
if command -v update-desktop-database >/dev/null 2>&1; then
  update-desktop-database -q /usr/share/applications || true
fi
exit 0
EOF
chmod 755 "$ROOT/DEBIAN/prerm" "$ROOT/DEBIAN/postinst" "$ROOT/DEBIAN/postrm"

# ---- Build ----
mkdir -p "$OUTDIR"
DEB="$OUTDIR/ymemo_${VERSION}_${ARCH}.deb"
dpkg-deb --root-owner-group --build "$ROOT" "$DEB"
echo "built: $DEB"
