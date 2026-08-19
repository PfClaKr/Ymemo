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
# ~/.local/share/ymemo stays, as usual: even purge leaves the home directory alone.
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
chmod 755 "$ROOT/DEBIAN/postinst" "$ROOT/DEBIAN/postrm"

# ---- Build ----
mkdir -p "$OUTDIR"
DEB="$OUTDIR/ymemo_${VERSION}_${ARCH}.deb"
dpkg-deb --root-owner-group --build "$ROOT" "$DEB"
echo "built: $DEB"
