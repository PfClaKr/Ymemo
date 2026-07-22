#!/usr/bin/env bash
# Ymemo .rpm 패키지 빌더 (Fedora). build-deb.sh 의 RPM 판.
#
# 앱과 (감춘) syncthing 을 한 패키지에 넣고, `dnf remove ymemo` 시 함께 지운다.
# 미리 빌드한 바이너리를 담으므로 rpmbuild 의 %build 는 없다 (ymemo.spec 참고).
#
# 사용법:
#   build-rpm.sh --app <ymemo-desktop> --sync <syncthing> --version <x.y.z> \
#                --outdir <dir> [--arch x86_64]
set -euo pipefail

APP="" ; SYNC="" ; VERSION="" ; OUTDIR="." ; ARCH="x86_64"
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
TOP="$(mktemp -d)"
trap 'rm -rf "$TOP"' EXIT
mkdir -p "$TOP"/{BUILD,BUILDROOT,RPMS,SOURCES,SPECS,SRPMS}

rpmbuild -bb \
  --define "_topdir $TOP" \
  --define "ymemo_version $VERSION" \
  --define "app_bin $(readlink -f "$APP")" \
  --define "sync_bin $(readlink -f "$SYNC")" \
  --define "assets_dir $(readlink -f "$ASSETS")" \
  --define "desktop_file $HERE/ymemo.desktop" \
  --define "license_file $(readlink -f "$HERE/../../LICENSE")" \
  --target "$ARCH" \
  "$HERE/ymemo.spec"

mkdir -p "$OUTDIR"
cp "$TOP"/RPMS/"$ARCH"/ymemo-*.rpm "$OUTDIR"/
echo "생성: $(ls "$OUTDIR"/ymemo-*."$ARCH".rpm)"
