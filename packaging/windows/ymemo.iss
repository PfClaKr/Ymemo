; Ymemo Windows installer (Inno Setup).
;
; The app and a hidden copy of syncthing ship in one installer:
;   {app}\Ymemo.exe        the app
;   {app}\ymemo-sync.exe   syncthing, renamed so users never see it
; Both live under {app}, so uninstalling removes them together.
;
; A firewall rule is added on install and deleted on uninstall, so syncthing never triggers
; a firewall prompt and stays invisible to the user.
;
; Compile: iscc /DAppVersion=0.1.0 ymemo.iss
;   (Ymemo.exe and ymemo-sync.exe must sit next to this .iss)

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

; Code signing: passing /DSign and /Symemosign="<signtool command> $f" to ISCC signs the
; installer and uninstaller. Without a certificate this block is skipped and the build is
; unsigned; CI adds the flags only when the secrets are present.
#ifdef Sign
SignTool=ymemosign
SignedUninstaller=yes
#endif

[Languages]
Name: "korean"; MessagesFile: "compiler:Languages\Korean.isl"
Name: "english"; MessagesFile: "compiler:Default.isl"

[CustomMessages]
english.StartupTask=Start Ymemo when Windows starts
korean.StartupTask=Windows 시작 시 Ymemo 자동 실행
english.AdditionalTasks=Additional tasks:
korean.AdditionalTasks=추가 작업:
english.UninstallIcon=Uninstall Ymemo
korean.UninstallIcon=Ymemo 제거
english.FirewallStatus=Configuring network settings...
korean.FirewallStatus=네트워크 설정 구성 중...
english.LaunchApp=Launch Ymemo
korean.LaunchApp=Ymemo 실행

[Tasks]
Name: "startup"; Description: "{cm:StartupTask}"; GroupDescription: "{cm:AdditionalTasks}"

[Files]
Source: "Ymemo.exe";      DestDir: "{app}"; Flags: ignoreversion
Source: "ymemo-sync.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\..\LICENSE";  DestDir: "{app}"; DestName: "LICENSE.txt"; Flags: ignoreversion

[Icons]
Name: "{group}\Ymemo"; Filename: "{app}\Ymemo.exe"
Name: "{group}\{cm:UninstallIcon}"; Filename: "{uninstallexe}"
Name: "{autostartup}\Ymemo"; Filename: "{app}\Ymemo.exe"; Tasks: startup

[Run]
; Inbound firewall rule for syncthing; a failure does not stop the install.
Filename: "netsh"; \
  Parameters: "advfirewall firewall add rule name=""Ymemo Sync"" dir=in action=allow program=""{app}\ymemo-sync.exe"" enable=yes profile=any"; \
  Flags: runhidden; StatusMsg: "{cm:FirewallStatus}"
; Optionally launch after install.
Filename: "{app}\Ymemo.exe"; Description: "{cm:LaunchApp}"; Flags: nowait postinstall skipifsilent

[UninstallRun]
; Remove the firewall rule.
Filename: "netsh"; Parameters: "advfirewall firewall delete rule name=""Ymemo Sync"""; \
  Flags: runhidden; RunOnceId: "DelYmemoFwRule"

[UninstallDelete]
; Clean out what is left in the app folder; user data in %LOCALAPPDATA%\ymemo stays.
Type: filesandordirs; Name: "{app}"
