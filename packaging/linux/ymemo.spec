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

# Stop a running Ymemo before its files go away. Same sequence as the .deb's prerm in
# build-deb.sh — **keep the two in step.** `$1 == 0` is an uninstall; on an upgrade the app
# is left alone rather than closing a window the user is typing in.
#
# Without this, removing the package leaves both processes alive: the app runs on from an
# unlinked binary and the sync daemon keeps syncing a vault whose app is gone. `ymemo --quit`
# asks the app to save and exit, and the daemon follows it (PR_SET_PDEATHSIG, see
# ymemo_core::sync). Processes are matched by their executable, not their command line —
# /usr/bin/ymemo is a symlink and argv[0] is whatever the .desktop or the shell used.
#
# `stat -c %%U` is spec-file escaping: rpm turns %% into a single % before the shell sees it.
%preun
[ "$1" = 0 ] || exit 0

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
      owner=$(stat -c %%U "$proc" 2>/dev/null) || owner=""
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
