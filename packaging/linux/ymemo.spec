# Ymemo RPM (Fedora). 미리 빌드한 바이너리를 담는 패키지라 %build 단계가 없다.
# build-rpm.sh 가 경로/버전을 --define 으로 넘긴다.
#
# .deb 와 같은 레이아웃: 앱과 (감춘) syncthing 을 함께 넣고, 제거 시 함께 지운다.
#   /usr/lib/ymemo/ymemo        앱 본체
#   /usr/lib/ymemo/ymemo-sync   syncthing (리네임)
#   /usr/bin/ymemo              → ../lib/ymemo/ymemo 심볼릭 링크

# 미리 빌드한 바이너리를 그대로 담는다: debuginfo 추출/스트립을 끈다
# (Go 로 만든 syncthing 은 스트립하면 깨질 수 있다).
%global debug_package %{nil}
%global __os_install_post %{nil}

Name:           ymemo
Version:        %{ymemo_version}
Release:        1%{?dist}
Summary:        로컬 우선 P2P 암호화 스티커 메모

License:        GPL-3.0-only
URL:            https://github.com/PfClaKr/Ymemo

# winit/렌더러가 런타임에 dlopen 하는 라이브러리는 ELF 스캔으로 안 잡히므로 명시한다.
# (fontconfig·glibc 등 직접 링크분은 rpmbuild 가 자동으로 Requires 에 넣는다)
Requires:       mesa-libGL
Requires:       libxkbcommon
# 한국어 등 CJK 표시용 폰트 (약한 의존 — 없어도 설치는 된다)
Recommends:     google-noto-sans-cjk-fonts

%description
Ymemo 는 자체 서버 없이 기기끼리 직접 동기화되는 E2E 암호화 메모 앱이다.
동기화 전송 계층(syncthing)을 함께 포함하며, 제거 시 함께 삭제된다.

%install
rm -rf %{buildroot}
install -Dm755 %{app_bin}      %{buildroot}/usr/lib/ymemo/ymemo
install -Dm755 %{sync_bin}     %{buildroot}/usr/lib/ymemo/ymemo-sync
mkdir -p %{buildroot}/usr/bin
ln -sf ../lib/ymemo/ymemo %{buildroot}/usr/bin/ymemo
install -Dm644 %{desktop_file} %{buildroot}/usr/share/applications/ymemo.desktop
for s in 16 32 48 64 128 256; do
  install -Dm644 %{assets_dir}/ymemo-$s.png \
    %{buildroot}/usr/share/icons/hicolor/${s}x${s}/apps/ymemo.png
done
install -Dm644 %{license_file} %{buildroot}/usr/share/licenses/ymemo/LICENSE

%files
%license /usr/share/licenses/ymemo/LICENSE
/usr/lib/ymemo/ymemo
/usr/lib/ymemo/ymemo-sync
/usr/bin/ymemo
/usr/share/applications/ymemo.desktop
/usr/share/icons/hicolor/*/apps/ymemo.png

%changelog
* Wed Jul 22 2026 PfClaKr <noreply@ymemo.dev> - 0.1.0-1
- CI 로 빌드한 릴리스 패키지.
