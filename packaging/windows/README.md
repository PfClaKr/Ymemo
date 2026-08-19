# Windows packaging and code signing

`ymemo.iss` (Inno Setup) wraps `Ymemo.exe` and the hidden `ymemo-sync.exe` into one
installer. The app icon gets in through two paths:

- **Wizard and uninstall-entry icon** — `SetupIconFile` and `UninstallDisplayIcon` in the
  `.iss`.
- **The exe's own icon** (Explorer, taskbar) — `crates/ymemo-desktop/build.rs` embeds
  `packaging/assets/ymemo.ico` as a resource at build time (`winresource`). Nothing to
  configure.

## Code signing (fewer SmartScreen warnings)

Signing is **optional**: the installer works without a certificate, but unsigned, Windows
SmartScreen warns about an "unknown publisher".

The `windows-desktop` job in `.github/workflows/release.yml` signs the binaries, the installer
and the uninstaller **only when both** of these repository secrets are set:

| Secret | Value |
| --- | --- |
| `WINDOWS_CERT_PFX_BASE64` | the code-signing PFX (.pfx), base64 encoded |
| `WINDOWS_CERT_PASSWORD` | its password |

Encoding a PFX:

```powershell
# Windows PowerShell
[Convert]::ToBase64String([IO.File]::ReadAllBytes("mycert.pfx")) | Set-Clipboard
```

```bash
# Linux/macOS
base64 -w0 mycert.pfx
```

Add both under Settings > Secrets and variables > Actions, and the next `v*` release produces
a signed installer. Without them the build is unsigned and says so in the log.

### Signing a local build

```powershell
$sign = '"C:\Path\To\signtool.exe" sign /fd sha256 /f mycert.pfx /p PASSWORD /tr http://timestamp.digicert.com /td sha256 $f'
ISCC /DAppVersion=0.1.0 /DSign "/Symemosign=$sign" ymemo.iss
```

Without `/DSign` the signing block (`SignTool=ymemosign`, `SignedUninstaller`) is skipped and
the build is unsigned.

### Signed is not the same as warning-free

- **OV (Organization Validation)**: signs fine, but SmartScreen may keep warning until the
  binary builds reputation.
- **EV (Extended Validation)**: passes SmartScreen immediately, but requires a hardware token
  — which means a self-hosted runner with the token attached, or a cloud HSM signing service,
  rather than plain CI signing.

So removing the warning outright effectively requires an EV certificate. This setup will sign
with either kind of PFX.
