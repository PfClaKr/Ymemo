#!/usr/bin/env bash
# Ymemo .rpm builder (Fedora), the RPM counterpart of build-deb.sh.
#
# The app and a hidden copy of syncthing go in one package and leave together on
# `dnf remove ymemo`. The binaries are prebuilt, so there is no %build (see ymemo.spec).
#
# Usage:
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
    *) echo "unknown argument: $1" >&2; exit 2;;
  esac
done
[ -n "$APP" ] && [ -f "$APP" ] || { echo "no --app binary: $APP" >&2; exit 2; }
[ -n "$SYNC" ] && [ -f "$SYNC" ] || { echo "no --sync binary: $SYNC" >&2; exit 2; }
[ -n "$VERSION" ] || { echo "--version is required" >&2; exit 2; }

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
echo "built: $(ls "$OUTDIR"/ymemo-*."$ARCH".rpm)"
