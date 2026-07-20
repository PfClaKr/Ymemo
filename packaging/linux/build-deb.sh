#!/usr/bin/env bash
# Ymemo .deb 패키지 빌더.
#
# 앱 바이너리와 (감춘) syncthing 을 한 패키지에 넣는다:
#   /usr/lib/ymemo/ymemo        앱 본체
#   /usr/lib/ymemo/ymemo-sync   syncthing (리네임 — 사용자에게 감춤)
#   /usr/bin/ymemo              → ../lib/ymemo/ymemo 심볼릭 링크 (PATH 노출)
#   /usr/share/applications/... .desktop 런처
#   /usr/share/icons/hicolor/...  아이콘
#
# syncthing 이 패키지 파일이므로 `apt remove ymemo` / `dpkg -r ymemo` 시 함께 지워진다.
# 사용자 데이터(~/.local/share/ymemo)는 남는다(표준 동작; purge 로도 사용자 홈은 안 지움).
#
# 사용법:
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
    *) echo "알 수 없는 인자: $1" >&2; exit 2;;
  esac
done
[ -n "$APP" ] && [ -f "$APP" ] || { echo "--app 바이너리 없음: $APP" >&2; exit 2; }
[ -n "$SYNC" ] && [ -f "$SYNC" ] || { echo "--sync 바이너리 없음: $SYNC" >&2; exit 2; }
[ -n "$VERSION" ] || { echo "--version 필요" >&2; exit 2; }

HERE="$(cd "$(dirname "$0")" && pwd)"
ASSETS="$HERE/../assets"
ROOT="$(mktemp -d)"
trap 'rm -rf "$ROOT"' EXIT

# ---- 파일 배치 ----
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

# ---- 의존성 계산 ----
# dpkg-shlibdeps 는 DT_NEEDED(직접 링크)만 잡는다. winit/렌더러가 런타임에 dlopen 하는
# GL/xkb 는 안 잡히므로 안전 하한(libgl1, libxkbcommon0)을 항상 더하고 패키지명으로 중복 제거.
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
# CALC + 안전 하한을 합쳐 패키지명 기준 중복 제거 (첫 항목의 버전 제약 유지).
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
Description: 로컬 우선 P2P 암호화 스티커 메모
 Ymemo 는 자체 서버 없이 기기끼리 직접 동기화되는 E2E 암호화 메모 앱이다.
 동기화 전송 계층(syncthing)을 함께 포함하며, 제거 시 함께 삭제된다.
EOF

# 아이콘/데스크탑 DB 갱신 (있을 때만).
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

# ---- 빌드 ----
mkdir -p "$OUTDIR"
DEB="$OUTDIR/ymemo_${VERSION}_${ARCH}.deb"
dpkg-deb --root-owner-group --build "$ROOT" "$DEB"
echo "생성: $DEB"
