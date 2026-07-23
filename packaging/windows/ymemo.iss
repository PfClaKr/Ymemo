; Ymemo Windows 인스톨러 (Inno Setup).
;
; 앱과 (감춘) syncthing 을 한 인스톨러에 담는다:
;   {app}\Ymemo.exe        앱 본체
;   {app}\ymemo-sync.exe   syncthing (리네임 — 사용자에게 감춤)
; 둘 다 {app} 안에 설치되므로 언인스톨 시 함께 제거된다.
;
; 방화벽 규칙을 설치/제거 시 자동 추가/삭제해 syncthing 이 네트워크를 쓸 때
; 방화벽 팝업이 뜨지 않게 한다 (사용자가 syncthing 존재를 눈치채지 못하도록).
;
; 컴파일: iscc /DAppVersion=0.1.0 ymemo.iss
;   (파일은 이 .iss 와 같은 폴더에 준비: Ymemo.exe, ymemo-sync.exe)

#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif

[Setup]
AppId={{7C1D6E2A-3B4F-4E8A-9C0D-59A1B2C3D4E5}
AppName=Ymemo
AppVersion={#AppVersion}
AppPublisher=PfClaKr
AppPublisherURL=https://github.com/PfClaKr/Ymemo
DefaultDirName={autopf}\Ymemo
DefaultGroupName=Ymemo
DisableProgramGroupPage=yes
UninstallDisplayIcon={app}\Ymemo.exe
UninstallDisplayName=Ymemo
OutputBaseFilename=ymemo-setup-x86_64
Compression=lzma2/max
SolidCompression=yes
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
PrivilegesRequired=admin
SetupIconFile=..\assets\ymemo.ico
WizardStyle=modern

; 코드 서명: ISCC 에 /DSign 과 /Symemosign="<signtool 명령> $f" 를 넘기면 인스톨러와
; 언인스톨러가 서명된다. 인증서가 없으면 이 블록은 건너뛰고 미서명으로 빌드된다.
; (CI 의 windows-desktop 잡이 시크릿이 있을 때만 이 플래그를 붙인다.)
#ifdef Sign
SignTool=ymemosign
SignedUninstaller=yes
#endif

[Languages]
Name: "korean"; MessagesFile: "compiler:Languages\Korean.isl"
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "startup"; Description: "Windows 시작 시 Ymemo 자동 실행"; GroupDescription: "추가 작업:"

[Files]
Source: "Ymemo.exe";      DestDir: "{app}"; Flags: ignoreversion
Source: "ymemo-sync.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\..\LICENSE";  DestDir: "{app}"; DestName: "LICENSE.txt"; Flags: ignoreversion

[Icons]
Name: "{group}\Ymemo"; Filename: "{app}\Ymemo.exe"
Name: "{group}\Ymemo 제거"; Filename: "{uninstallexe}"
Name: "{autostartup}\Ymemo"; Filename: "{app}\Ymemo.exe"; Tasks: startup

[Run]
; syncthing 방화벽 인바운드 허용 규칙 (실패해도 설치는 계속).
Filename: "netsh"; \
  Parameters: "advfirewall firewall add rule name=""Ymemo Sync"" dir=in action=allow program=""{app}\ymemo-sync.exe"" enable=yes profile=any"; \
  Flags: runhidden; StatusMsg: "네트워크 설정 구성 중..."
; 설치 마침 후 바로 실행 (선택).
Filename: "{app}\Ymemo.exe"; Description: "Ymemo 실행"; Flags: nowait postinstall skipifsilent

[UninstallRun]
; 방화벽 규칙 제거.
Filename: "netsh"; Parameters: "advfirewall firewall delete rule name=""Ymemo Sync"""; \
  Flags: runhidden; RunOnceId: "DelYmemoFwRule"

[UninstallDelete]
; 앱 폴더 잔여물까지 정리 (사용자 데이터 %LOCALAPPDATA%\ymemo 는 남긴다).
Type: filesandordirs; Name: "{app}"
