; Ymemo Windows installer (Inno Setup).
;
; The app and a hidden copy of syncthing ship in one installer:
;   {app}\Ymemo.exe        the app
;   {app}\ymemo-sync.exe   syncthing, renamed so users never see it
; Both live under {app}, so uninstalling removes them together.
;
; Firewall rules are added on install and deleted on uninstall, so neither process ever
; triggers a firewall prompt: the sync daemon accepts connections from other devices, and the
; app itself listens for LAN pairing on UDP 21029 (ymemo_core::lan_pair).
;
; **Getting the running app out of the way** is what makes installing and uninstalling over a
; live installation work. Ymemo lives in the tray with a sync daemon behind it, and a locked
; ymemo-sync.exe is what leaves files behind or demands a reboot. So, before touching any
; file, StopYmemo (below) runs `Ymemo.exe --quit`, which asks the running instance to save,
; close and shut its daemon down, and waits for it. taskkill is only the fallback for an
; instance too old to understand --quit, and CloseApplications=force is the net under that.
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
; Windows compares this when a repair or downgrade is offered, and Explorer shows it on the
; file; without it the installer carries no version at all.
VersionInfoVersion={#AppVersion}
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
; Restart Manager: close anything still holding our files instead of asking for a reboot.
; `force` also terminates what will not close on its own — by then StopYmemo has already
; given the app its polite chance. RestartApplications=no because relaunching a tray app
; behind the user's back is worse than leaving it closed.
CloseApplications=force
RestartApplications=no

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
english.StoppingApp=Closing Ymemo...
korean.StoppingApp=Ymemo 종료 중...
english.RemoveData=Also delete your memos and settings?%n%nThey stay on this computer if you answer No, and a reinstall picks them up again. Answering Yes deletes them from this device; copies on your other devices are untouched.
korean.RemoveData=메모와 설정도 함께 삭제할까요?%n%n아니오를 선택하면 이 컴퓨터에 그대로 남아 다시 설치할 때 이어서 사용할 수 있습니다. 예를 선택하면 이 기기에서 삭제되며, 다른 기기의 사본은 그대로 유지됩니다.

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
; Inbound firewall rules; a failure does not stop the install.
;   Ymemo Sync — the sync daemon's transfers.
;   Ymemo LAN Pairing — the app's own UDP 21029 listener, which would otherwise pop the
;   Windows firewall dialog at first launch, right when the user is trying to pair.
Filename: "netsh"; \
  Parameters: "advfirewall firewall add rule name=""Ymemo Sync"" dir=in action=allow program=""{app}\ymemo-sync.exe"" enable=yes profile=any"; \
  Flags: runhidden; StatusMsg: "{cm:FirewallStatus}"
Filename: "netsh"; \
  Parameters: "advfirewall firewall add rule name=""Ymemo LAN Pairing"" dir=in action=allow program=""{app}\Ymemo.exe"" protocol=UDP localport=21029 enable=yes profile=any"; \
  Flags: runhidden; StatusMsg: "{cm:FirewallStatus}"
; Optionally launch after install. runasoriginaluser: Setup runs elevated, but Ymemo is a
; per-user tray app — started elevated it would put its vault in the elevating account's
; profile and stand apart from the copy the user starts later.
Filename: "{app}\Ymemo.exe"; Description: "{cm:LaunchApp}"; \
  Flags: nowait postinstall skipifsilent runasoriginaluser

[UninstallRun]
; Remove the firewall rules.
Filename: "netsh"; Parameters: "advfirewall firewall delete rule name=""Ymemo Sync"""; \
  Flags: runhidden; RunOnceId: "DelYmemoFwRule"
Filename: "netsh"; Parameters: "advfirewall firewall delete rule name=""Ymemo LAN Pairing"""; \
  Flags: runhidden; RunOnceId: "DelYmemoPairFwRule"

[UninstallDelete]
; Clean out what is left in the app folder. User data lives in %APPDATA%\ymemo\Ymemo and is
; only removed if the user says so on the way out (see CurUninstallStepChanged).
Type: filesandordirs; Name: "{app}"

[Code]
{ Stops a running Ymemo before its files are replaced or removed.

  Three steps, each a fallback for the one before:
    1. `Ymemo.exe --quit` — the app saves open memos, closes its windows and shuts the sync
       daemon down over its REST API. It returns only once the app is really gone.
    2. taskkill on Ymemo.exe — for an installed version older than --quit.
    3. taskkill on ymemo-sync.exe — for a daemon orphaned by a version that predates the job
       object that now ties it to the app's lifetime.
  Every step is best effort: nothing here may stop an install. }
procedure StopYmemo(AppExe: String);
var
  Code: Integer;
begin
  { As the original user: Setup is elevated, and the running app — with the data directory
    holding the port it listens on — belongs to whoever is logged in. }
  if FileExists(AppExe) then
    ExecAsOriginalUser(AppExe, '--quit', '', SW_HIDE, ewWaitUntilTerminated, Code);
  Exec(ExpandConstant('{sys}\taskkill.exe'), '/T /F /IM Ymemo.exe', '',
       SW_HIDE, ewWaitUntilTerminated, Code);
  Exec(ExpandConstant('{sys}\taskkill.exe'), '/T /F /IM ymemo-sync.exe', '',
       SW_HIDE, ewWaitUntilTerminated, Code);
  { The daemon releases its exe a moment after the process ends. }
  Sleep(500);
end;

function PrepareToInstall(var NeedsRestart: Boolean): String;
begin
  WizardForm.PreparingLabel.Caption := ExpandConstant('{cm:StoppingApp}');
  StopYmemo(ExpandConstant('{app}\Ymemo.exe'));
  Result := '';
end;

function InitializeUninstall(): Boolean;
begin
  StopYmemo(ExpandConstant('{app}\Ymemo.exe'));
  Result := True;
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
begin
  { Memos and settings are the user's, so they survive an uninstall unless asked otherwise —
    the default answer is No. Only the uninstalling account's data is reachable here; another
    user's copy under their own profile stays. }
  if CurUninstallStep = usPostUninstall then
    if not UninstallSilent then
      if MsgBox(ExpandConstant('{cm:RemoveData}'), mbConfirmation, MB_YESNO or MB_DEFBUTTON2) = IDYES then
        DelTree(ExpandConstant('{userappdata}\ymemo\Ymemo'), True, True, True);
end;
