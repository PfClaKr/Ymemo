# Windows 패키징 · 코드 서명

`ymemo.iss` (Inno Setup) 가 `Ymemo.exe` + 감춘 `ymemo-sync.exe` 를 하나의 설치
프로그램으로 묶는다. 앱 아이콘은 두 경로로 들어간다:

- **설치 마법사/제거 항목 아이콘** — `.iss` 의 `SetupIconFile`/`UninstallDisplayIcon`.
- **exe 자체 아이콘**(탐색기·작업표시줄) — `crates/ymemo-desktop/build.rs` 가 빌드 시
  `packaging/assets/ymemo.ico` 를 exe 에 리소스로 박는다(`winresource`). 별도 설정 불필요.

## 코드 서명 (SmartScreen 경고 줄이기)

서명은 **선택**이다. 인증서 없이도 설치 프로그램은 정상 동작하지만, 서명이 없으면
Windows SmartScreen 이 "알 수 없는 게시자" 경고를 띄운다.

CI(`.github/workflows/release.yml` 의 `windows-desktop` 잡)는 다음 저장소 시크릿이
**둘 다 있을 때만** 바이너리·설치 프로그램·제거 프로그램을 서명한다:

| 시크릿 | 값 |
| --- | --- |
| `WINDOWS_CERT_PFX_BASE64` | 코드서명 PFX(.pfx) 파일을 base64 로 인코딩한 문자열 |
| `WINDOWS_CERT_PASSWORD` | 그 PFX 의 암호 |

PFX 를 base64 로 만드는 법:

```powershell
# Windows PowerShell
[Convert]::ToBase64String([IO.File]::ReadAllBytes("mycert.pfx")) | Set-Clipboard
```
```bash
# Linux/macOS
base64 -w0 mycert.pfx
```

두 시크릿을 GitHub 저장소 → Settings → Secrets and variables → Actions 에 등록하면,
다음 `v*` 태그 릴리스부터 서명된 설치 프로그램이 나온다. 시크릿이 없으면 미서명으로
빌드된다(로그에 "미서명 빌드" 표시).

### 로컬에서 서명해 컴파일하기

```powershell
$sign = '"C:\Path\To\signtool.exe" sign /fd sha256 /f mycert.pfx /p PASSWORD /tr http://timestamp.digicert.com /td sha256 $f'
ISCC /DAppVersion=0.1.0 /DSign "/Symemosign=$sign" ymemo.iss
```

`/DSign` 이 없으면 `.iss` 의 서명 블록(`SignTool=ymemosign`, `SignedUninstaller`)은
건너뛰고 미서명으로 컴파일된다.

### 경고: 서명 ≠ 즉시 무경고

- **OV(Organization Validation) 인증서**: 서명은 되지만 평판(reputation)이 쌓이기 전엔
  SmartScreen 경고가 한동안 뜰 수 있다.
- **EV(Extended Validation) 인증서**: 즉시 SmartScreen 을 통과한다(하드웨어 토큰 필요,
  이 경우 CI 자동 서명 대신 토큰이 꽂힌 셀프호스트 러너나 클라우드 HSM 서명 서비스가 필요).

즉 "경고를 완전히 없애려면" EV 인증서가 사실상 필수다. 이 배선은 OV/EV 어느 쪽 PFX 든
받아 서명까지는 해 준다.
