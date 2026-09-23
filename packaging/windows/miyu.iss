; Miyu for Windows —— Inno Setup 6 安装器脚本。
;
; 由 packaging/windows/package.ps1 调用（CI 里同样走它），三个值从命令行传进来：
;   /DAppVersion=0.6.1  /DStageDir=<暂存目录>  /DOutDir=<输出目录>
;
; 设计取舍：
; - 装到 %LOCALAPPDATA%\Programs\Miyu（PrivilegesRequired=lowest → 免管理员）。
; - 只写用户级 PATH（HKCU\Environment），ChangesEnvironment=yes 让新开的终端立刻认得。
; - 装/卸之前先停 daemon：Windows 上运行中的 exe 覆盖不了，这是实测踩过的坑。
; - 卸载保留 %USERPROFILE%\.miyu（配置、会话库、记忆都在里面，不能跟着删）。

#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif
#ifndef StageDir
  #define StageDir "..\..\out\windows\miyu-windows-x64"
#endif
#ifndef OutDir
  #define OutDir "..\..\out\windows"
#endif

[Setup]
AppId={{2F0A1C9E-6B7D-4E3A-9C2B-7A5D1E4F8C10}
AppName=Miyu
AppVersion={#AppVersion}
AppVerName=Miyu {#AppVersion}
AppPublisher=LanternFlower
AppPublisherURL=https://github.com/LanternFlower/Miyu-for-Windows
AppSupportURL=https://github.com/LanternFlower/Miyu-for-Windows
DefaultDirName={localappdata}\Programs\Miyu
DefaultGroupName=Miyu
DisableProgramGroupPage=yes
AllowNoIcons=yes
PrivilegesRequired=lowest
OutputDir={#OutDir}
OutputBaseFilename=MiyuSetup-{#AppVersion}-x64
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
ChangesEnvironment=yes
MinVersion=10.0
ArchitecturesAllowed=x64compatible
UninstallDisplayName=Miyu
UninstallDisplayIcon={app}\bin\miyu.exe
; 安装器 UI 用英文：官方 Inno 不带简体中文语言文件（需要 unofficial 的
; ChineseSimplified.isl）。想换中文见 packaging/windows/README.md。

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "powershellhook"; Description: "安装 PowerShell 终端集成（终端里直接打自然语言；之后可用 miyu remove-shell-hook 撤销）"; GroupDescription: "可选："; Flags: unchecked
Name: "desktopicon"; Description: "创建桌面快捷方式"; GroupDescription: "可选："; Flags: unchecked

[Files]
Source: "{#StageDir}\bin\*"; DestDir: "{app}\bin"; Flags: ignoreversion recursesubdirs createallsubdirs
Source: "{#StageDir}\share\*"; DestDir: "{app}\share"; Flags: ignoreversion recursesubdirs createallsubdirs

[Icons]
Name: "{autoprograms}\Miyu"; Filename: "{app}\bin\miyu.exe"; WorkingDir: "{app}"
Name: "{autodesktop}\Miyu"; Filename: "{app}\bin\miyu.exe"; WorkingDir: "{app}"; Tasks: desktopicon

[Registry]
; 只加用户级 PATH，交给 ChangesEnvironment 去广播 WM_SETTINGCHANGE。
Root: HKCU; Subkey: "Environment"; ValueType: expandsz; ValueName: "Path"; ValueData: "{olddata};{app}\bin"; Check: NeedsAddPath('{app}\bin')

[Run]
; 勾了任务就在安装收尾时静默装 hook（会写 $PROFILE，所以默认不勾）。
Filename: "{app}\bin\miyu.exe"; Parameters: "powershell-init"; Flags: runhidden; Tasks: powershellhook
; 装完顺手把 Miyu 起起来（TUI 会自己走新手引导）。
Filename: "{app}\bin\miyu.exe"; Description: "启动 Miyu"; WorkingDir: "{app}"; Flags: postinstall nowait skipifsilent

[UninstallRun]
Filename: "{app}\bin\miyu.exe"; Parameters: "daemon stop"; Flags: runhidden; RunOnceId: "StopDaemon"
Filename: "{sys}\taskkill.exe"; Parameters: "/IM miyu.exe /F"; Flags: runhidden; RunOnceId: "KillLeftovers"

[Code]
function NeedsAddPath(Param: string): Boolean;
var
  Path: string;
begin
  if not RegQueryStringValue(HKEY_CURRENT_USER, 'Environment', 'Path', Path) then
  begin
    Result := True;
    exit;
  end;
  Result := Pos(';' + Uppercase(Param) + ';', ';' + Uppercase(Path) + ';') = 0;
end;

procedure RemoveFromPath(Dir: string);
var
  Path: string;
  Upper: string;
  Index: Integer;
begin
  if not RegQueryStringValue(HKEY_CURRENT_USER, 'Environment', 'Path', Path) then
    exit;
  Upper := Uppercase(Path);
  Dir := Uppercase(Dir);
  Index := Pos(';' + Dir, Upper);
  if Index = 0 then
  begin
    if Upper = Dir then
      RegWriteExpandStringValue(HKEY_CURRENT_USER, 'Environment', 'Path', '');
    exit;
  end;
  Delete(Path, Index, Length(Dir) + 1);
  RegWriteExpandStringValue(HKEY_CURRENT_USER, 'Environment', 'Path', Path);
end;

// 覆盖安装前必须让旧进程松手：先礼后兵——`daemon stop`，再收掉还占着文件的。
procedure StopRunningMiyu;
var
  Code: Integer;
begin
  if FileExists(ExpandConstant('{app}\bin\miyu.exe')) then
    Exec(ExpandConstant('{app}\bin\miyu.exe'), 'daemon stop', '', SW_HIDE,
      ewWaitUntilTerminated, Code);
  Exec(ExpandConstant('{sys}\taskkill.exe'), '/IM miyu.exe /F', '', SW_HIDE,
    ewWaitUntilTerminated, Code);
end;

procedure CurStepChanged(CurStep: TSetupStep);
begin
  if CurStep = ssInstall then
    StopRunningMiyu;
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
begin
  if CurUninstallStep = usUninstall then
    RemoveFromPath(ExpandConstant('{app}\bin'));
end;
