# Ymemo RPM (Fedora). The binaries are prebuilt, so there is no %build step; build-rpm.sh
# passes the paths and version with --define.
#
# Same layout as the .deb: the app plus a hidden syncthing, removed together.
#   /usr/lib/ymemo/ymemo        the app
#   /usr/lib/ymemo/ymemo-sync   syncthing, renamed
#   /usr/bin/ymemo              symlink to ../lib/ymemo/ymemo

# The binaries ship as built, so debuginfo extraction and stripping are off; stripping the
# Go-built syncthing can break it.
%global debug_package %{nil}
%global __os_install_post %{nil}

Name:           ymemo
Version:        %{ymemo_version}
Release:        1%{?dist}
Summary:        Local-first, P2P encrypted sticky notes

License:        GPL-3.0-only
URL:            https://github.com/PfClaKr/Ymemo

# Libraries winit and the renderer dlopen at runtime are invisible to the ELF scan, so they
# are listed here; rpmbuild picks up the directly linked ones itself.
Requires:       mesa-libGL
Requires:       libxkbcommon
# CJK fonts, a weak dependency: it installs fine without them.
Recommends:     google-noto-sans-cjk-fonts

%description
Ymemo is an E2E encrypted memo app that syncs directly between devices with no server of its
own. It bundles its sync transport (syncthing), which is removed along with it.

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

# Refresh the icon and desktop databases. Current Fedora does this through file triggers,
# but older or minimal installs may not, so run them where they exist.
%post
command -v gtk-update-icon-cache >/dev/null 2>&1 && gtk-update-icon-cache -qtf /usr/share/icons/hicolor >/dev/null 2>&1 || :
command -v update-desktop-database >/dev/null 2>&1 && update-desktop-database -q /usr/share/applications >/dev/null 2>&1 || :

%postun
command -v gtk-update-icon-cache >/dev/null 2>&1 && gtk-update-icon-cache -qtf /usr/share/icons/hicolor >/dev/null 2>&1 || :
command -v update-desktop-database >/dev/null 2>&1 && update-desktop-database -q /usr/share/applications >/dev/null 2>&1 || :

%files
%license /usr/share/licenses/ymemo/LICENSE
/usr/lib/ymemo/ymemo
/usr/lib/ymemo/ymemo-sync
/usr/bin/ymemo
/usr/share/applications/ymemo.desktop
/usr/share/icons/hicolor/*/apps/ymemo.png

%changelog
* Wed Jul 22 2026 PfClaKr <noreply@ymemo.dev> - 0.1.0-1
- Release package built by CI.
