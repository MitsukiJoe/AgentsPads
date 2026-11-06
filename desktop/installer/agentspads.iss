#if VER < EncodeVer(6, 5, 0)
  #error Inno Setup 6.5 or later is required
#endif
#ifndef AppVersion
  #error AppVersion must be supplied by build-windows-installer.ps1
#endif
#ifndef AppExecutable
  #error AppExecutable must be supplied by build-windows-installer.ps1
#endif
#ifndef AppSha256
  #error AppSha256 must be supplied by build-windows-installer.ps1
#endif

[Setup]
AppId=AgentsPads.Managed.Stable
AppName=AgentsPads
AppVersion={#AppVersion}
AppPublisher=AgentsPads
AppPublisherURL=https://github.com/MitsukiJoe/AgentsPads
AppSupportURL=https://github.com/MitsukiJoe/AgentsPads/issues
DefaultDirName={code:GetDefaultInstallationDirectory}
DefaultGroupName=AgentsPads
DisableDirPage=no
DisableProgramGroupPage=yes
UsePreviousAppDir=yes
UsePreviousTasks=yes
PrivilegesRequired=admin
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0
SetupMutex=Global\AgentsPads.Managed.Install
AppMutex=Global\AgentsPads.Managed.Running,Global\AgentsPads.Managed.Maintenance
CloseApplications=yes
RestartApplications=no
CloseApplicationsFilter=*.exe
UninstallDisplayIcon={app}\agentspads.exe
SetupIconFile=..\crates\agentpad\agentpad.ico
LicenseFile=..\..\LICENSE
OutputBaseFilename=agentspads-windows-x64-setup
OutputDir=dist
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
SetupLogging=yes
UninstallLogging=yes
VersionInfoVersion={#AppVersion}.0
VersionInfoDescription=AgentsPads 安装程序

[Languages]
Name: "zhcn"; MessagesFile: "languages\ChineseSimplified.isl"

[Messages]
SetupAppRunningError=AgentsPads 当前仍在运行。%n%n请从系统托盘菜单选择“退出”，然后点击“确定”继续。关闭窗口只会隐藏到托盘。%n%n如果还有其他账户正在运行 AgentsPads，也需要先退出。
UninstallAppRunningError=AgentsPads 当前仍在运行。%n%n请从系统托盘菜单选择“退出”，然后点击“确定”继续。关闭窗口只会隐藏到托盘。%n%n卸载不会强制结束程序或留到重启后删除。
WelcomeLabel2=此安装程序将安装 AgentsPads {#AppVersion}。%n%n普通启动和管理员启动共用一份正式程序。%n%n继续之前，请从托盘退出正在运行的 AgentsPads。
SelectDirLabel3=请选择安装位置。首次安装会优先选择合适的非 C 盘。
WizardSelectTasks=选择启动方式
SelectTasksDesc=普通入口始终可用，管理员入口由你选择。
SelectTasksLabel2=请选择需要的功能：
FinishedLabel=AgentsPads 已安装。%n%n请从开始菜单打开“AgentsPads”（普通权限）。如果启用了管理员入口，也可打开“AgentsPads（管理员）”。%n%n安装程序不会自动启动应用。开机启动默认关闭，可在应用设置中开启。

[Tasks]
Name: "adminlaunch"; Description: "管理员启动 — 启用操控所有窗口的能力"; Flags: unchecked
Name: "desktopicon"; Description: "创建桌面快捷方式"; Flags: unchecked

[Files]
Source: "validate-install-root.ps1"; Flags: dontcopy
Source: "managed-install.json"; DestDir: "{app}"; Flags: ignoreversion uninsneveruninstall
Source: "operation.lock"; DestDir: "{app}"; Flags: onlyifdoesntexist
Source: "{#AppExecutable}"; DestDir: "{app}"; DestName: "agentspads.exe"; Flags: ignoreversion; BeforeInstall: PreserveInstalledExecutable; AfterInstall: VerifyInstalledExecutable
Source: "..\..\LICENSE"; DestDir: "{app}"; DestName: "LICENSE.txt"; Flags: ignoreversion

[Icons]
Name: "{group}\AgentsPads"; Filename: "{app}\agentspads.exe"; WorkingDir: "{app}"; AppUserModelID: "app.agentspads"
Name: "{group}\AgentsPads（管理员）"; Filename: "{app}\agentspads.exe"; Parameters: "--launch-admin"; WorkingDir: "{app}"; AppUserModelID: "app.agentspads.admin"; Tasks: adminlaunch
Name: "{commondesktop}\AgentsPads"; Filename: "{app}\agentspads.exe"; WorkingDir: "{app}"; AppUserModelID: "app.agentspads"; Tasks: desktopicon

[Code]
var
  AdminLaunchReady: Boolean;
  AdminLaunchFailed: Boolean;
  InstalledFileInvalid: Boolean;
  HadAdminShortcut: Boolean;
  DefaultDirectoryCalculated: Boolean;
  DefaultInstallationDirectory: String;
  ExistingInstallationDirectory: String;

function ReadLocationResult: String;
var
  Lines: TArrayOfString;
begin
  Result := '';
  if LoadStringsFromFile(ExpandConstant('{tmp}\install-location-result.txt'), Lines) and
    (GetArrayLength(Lines) > 0) then
    Result := Trim(Lines[0]);
end;

function RunLocationHelper(const Mode: String; const Directory: String): Boolean;
var
  ResultCode: Integer;
  Arguments: String;
begin
  Result := False;
  if (Pos('"', Directory) > 0) or (Pos(#13, Directory) > 0) or (Pos(#10, Directory) > 0) then
    Exit;
  DeleteFile(ExpandConstant('{tmp}\install-location-result.txt'));
  ExtractTemporaryFile('validate-install-root.ps1');
  Arguments := '-NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass -File "' +
    ExpandConstant('{tmp}\validate-install-root.ps1') + '" -Mode ' + Mode +
    ' -OutputPath "' + ExpandConstant('{tmp}\install-location-result.txt') + '"';
  if Directory <> '' then
    Arguments := Arguments + ' -InstallDirectory "' + RemoveBackslashUnlessRoot(Directory) + '"';
  if Exec(ExpandConstant('{sysnative}\WindowsPowerShell\v1.0\powershell.exe'),
    Arguments, ExpandConstant('{sys}'), SW_HIDE, ewWaitUntilTerminated, ResultCode) then
    Result := ResultCode = 0;
  if not Result then
    Log('Installation location check failed (' + Mode + '): ' + ReadLocationResult);
end;

function GetDefaultInstallationDirectory(Param: String): String;
begin
  if not DefaultDirectoryCalculated then begin
    DefaultInstallationDirectory := ExpandConstant('{autopf}\AgentsPads');
    if RunLocationHelper('Select', '') then begin
      if ReadLocationResult <> '' then
        DefaultInstallationDirectory := ReadLocationResult;
    end;
    DefaultDirectoryCalculated := True;
  end;
  Result := DefaultInstallationDirectory;
end;

procedure InitializeWizard;
begin
  if RegQueryStringValue(HKLM64,
    'SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\AgentsPads.Managed.Stable_is1',
    'InstallLocation', ExistingInstallationDirectory) then begin
    ExistingInstallationDirectory := RemoveBackslashUnlessRoot(ExistingInstallationDirectory);
    WizardForm.DirEdit.Text := ExistingInstallationDirectory;
    WizardForm.DirEdit.Enabled := False;
    WizardForm.DirBrowseButton.Enabled := False;
    WizardForm.SelectDirLabel.Caption := '重装和更新会沿用此位置。若要更换磁盘，请先卸载 AgentsPads。';
  end;
end;

function NextButtonClick(CurPageID: Integer): Boolean;
begin
  Result := True;
  if CurPageID = wpSelectDir then begin
    Result := RunLocationHelper('Validate', WizardDirValue);
    if not Result then
      SuppressibleMsgBox('此文件夹不适合安装 AgentsPads。请选择本地 NTFS 磁盘上未被占用的新文件夹，并确认有至少 256 MB 空间。普通程序不能具有修改该目录或其上级目录的权限。', mbError, MB_OK, IDOK);
  end;
end;

function PrepareToInstall(var NeedsRestart: Boolean): String;
var
  Marker: AnsiString;
begin
  Result := '';
  HadAdminShortcut := FileExists(ExpandConstant('{group}\AgentsPads（管理员）.lnk'));
  if CheckForMutexes('Global\AgentsPads.Managed.Running,Global\AgentsPads.Managed.Maintenance') then begin
    Result := 'AgentsPads 或其更新操作仍在运行。请等待维护完成，并从托盘退出程序后重试。';
    Exit;
  end;
  if not RunLocationHelper('Validate', ExpandConstant('{app}')) then begin
    Result := '无法使用所选安装位置。请选择本地 NTFS 磁盘上的受保护文件夹，或使用默认位置。更换已有安装的位置前，请先卸载。';
    Exit;
  end;
  if FileExists(ExpandConstant('{app}\run_as_admin.txt')) or
    FileExists(ExpandConstant('{app}\ordinary-exe-path.bin')) then begin
    Result := '发现旧版管理员副本。此次安装没有修改它。请先退出旧版，清理旧版计划任务和 Program Files\AgentsPads 目录，再重新安装。清理方法见项目 README。';
    Exit;
  end;
  if FileExists(ExpandConstant('{app}\managed-install.json')) then begin
    if not LoadStringFromFile(ExpandConstant('{app}\managed-install.json'), Marker) or
      (Trim(String(Marker)) <> '{"schema":1,"product":"AgentsPads","channel":"stable"}') then
      Result := '现有安装记录不属于受支持的 AgentsPads 正式渠道。请先检查现有安装，避免覆盖其他渠道或损坏的安装。';
  end else if FileExists(ExpandConstant('{app}\agentspads.exe')) then
    Result := '目标目录内已有来源未知的程序。此次安装没有修改它。请先退出并整理旧版 AgentsPads，再重新安装。';
  if (Result = '') and not RunLocationHelper('Prepare', ExpandConstant('{app}')) then
    Result := '无法准备所选安装目录，安装尚未开始。请重新选择文件夹后重试。';
end;

procedure PreserveInstalledExecutable;
var
  ExistingExe: String;
  BackupExe: String;
begin
  ExistingExe := ExpandConstant('{app}\agentspads.exe');
  BackupExe := ExpandConstant('{app}\agentspads.exe.installer-backup');
  if FileExists(ExistingExe) and not FileExists(BackupExe) then
    if not FileCopy(ExistingExe, BackupExe, True) then
      RaiseException('无法保留现有版本，安装已停止。请先退出 AgentsPads，并检查磁盘空间或安全软件提示后重试。');
end;

function InstalledExecutableMatches: Boolean;
begin
  Result := FileExists(ExpandConstant('{app}\agentspads.exe'));
  if Result then begin
    try
      Result := CompareText(GetSHA256OfFile(ExpandConstant('{app}\agentspads.exe')), '{#AppSha256}') = 0;
    except
      Result := False;
    end;
  end;
end;

procedure VerifyInstalledExecutable;
begin
  if not InstalledExecutableMatches then
    RaiseException('程序文件校验失败，无法确认已安装所选版本。请检查安全软件提示并重新安装；原版本恢复副本保留在安装目录。');
end;

procedure ConfigureAdminLaunch;
var
  ResultCode: Integer;
begin
  if not WizardIsTaskSelected('adminlaunch') then
    Exit;
  AdminLaunchReady := Exec(ExpandConstant('{app}\agentspads.exe'),
    '--register-admin-launch', ExpandConstant('{app}'), SW_HIDE,
    ewWaitUntilTerminated, ResultCode);
  AdminLaunchReady := AdminLaunchReady and (ResultCode = 0);
  AdminLaunchFailed := not AdminLaunchReady;
  if AdminLaunchFailed then begin
    if not HadAdminShortcut then
      DeleteFile(ExpandConstant('{group}\AgentsPads（管理员）.lnk'));
    Log('Administrator launch registration failed. Exit code: ' + IntToStr(ResultCode));
    SuppressibleMsgBox('普通程序已安装，但管理员入口未能启用。请根据刚才的错误提示处理，或从普通入口打开设置后重试。', mbError, MB_OK, IDOK);
  end;
end;

function ArchiveEarlierUpdate: Boolean;
var
  JournalPath: String;
  ArchivePath: String;
begin
  Result := True;
  JournalPath := ExpandConstant('{app}\updates\transaction.json');
  if not FileExists(JournalPath) then
    Exit;
  ArchivePath := ExpandConstant('{app}\updates\transaction.before-install-') +
    GetDateTimeString('yyyymmdd-hhnnss', '-', ':') + '.json';
  Result := not FileExists(ArchivePath);
  if Result then
    Result := RenameFile(JournalPath, ArchivePath);
end;

procedure CurStepChanged(CurStep: TSetupStep);
begin
  if (CurStep = ssInstall) and CheckForMutexes('Global\AgentsPads.Managed.Running,Global\AgentsPads.Managed.Maintenance') then begin
    SuppressibleMsgBox('AgentsPads 或其更新操作仍在运行，安装已停止，请退出后重试。', mbError, MB_OK, IDOK);
    Abort;
  end;
  if CurStep = ssPostInstall then begin
    InstalledFileInvalid := not InstalledExecutableMatches;
    if InstalledFileInvalid then begin
      if not HadAdminShortcut then
        DeleteFile(ExpandConstant('{group}\AgentsPads（管理员）.lnk'));
      SuppressibleMsgBox('安装后的程序与所选版本不一致，安装未能完成。请检查安全软件提示并重新安装；原版本恢复副本保留在安装目录。', mbError, MB_OK, IDOK);
    end else if not RunLocationHelper('ProtectRegistration', ExpandConstant('{app}')) then begin
      InstalledFileInvalid := True;
      if not HadAdminShortcut then
        DeleteFile(ExpandConstant('{group}\AgentsPads（管理员）.lnk'));
      SuppressibleMsgBox('安装位置登记未能完成，管理员入口尚未启用。请重新运行安装包修复。', mbError, MB_OK, IDOK);
    end else if not ArchiveEarlierUpdate then begin
      InstalledFileInvalid := True;
      SuppressibleMsgBox('所选程序已写入，但旧更新记录无法归档。为避免启动错误版本，请重新运行安装包修复；旧版恢复文件仍保留。', mbError, MB_OK, IDOK);
    end else
      ConfigureAdminLaunch;
  end;
end;

procedure CurPageChanged(CurPageID: Integer);
begin
  if (CurPageID = wpFinished) and InstalledFileInvalid then
    WizardForm.FinishedLabel.Caption := '安装未能完成。' + #13#10#13#10 +
      '请根据刚才的错误提示处理后重新安装，或按 README 的步骤恢复保留的旧版本。'
  else if (CurPageID = wpFinished) and AdminLaunchFailed then
    WizardForm.FinishedLabel.Caption := 'AgentsPads 普通程序已安装，管理员入口未能启用。' + #13#10#13#10 +
      '请从开始菜单的“AgentsPads”普通入口启动，再到设置中重试管理员授权。';
end;

function GetCustomSetupExitCode: Integer;
begin
  Result := 0;
  if AdminLaunchFailed or InstalledFileInvalid then
    Result := 1;
end;

function InitializeUninstall: Boolean;
begin
  Result := not CheckForMutexes('Global\AgentsPads.Managed.Install');
  if Result then
    CreateMutex('Global\AgentsPads.Managed.Install')
  else
    SuppressibleMsgBox('AgentsPads 安装或维护正在进行，请完成后再卸载。', mbError, MB_OK, IDOK);
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
var
  ResultCode: Integer;
begin
  if CurUninstallStep <> usUninstall then
    Exit;
  if CheckForMutexes('Global\AgentsPads.Managed.Running,Global\AgentsPads.Managed.Maintenance') then begin
    SuppressibleMsgBox('AgentsPads 或其更新操作仍在运行，卸载已停止，请退出后重试。', mbError, MB_OK, IDOK);
    Abort;
  end;
  if not Exec(ExpandConstant('{app}\agentspads.exe'), '--unregister-admin-launch',
    ExpandConstant('{app}'), SW_HIDE, ewWaitUntilTerminated, ResultCode) or
    (ResultCode <> 0) then begin
    SuppressibleMsgBox('未能移除 AgentsPads 的管理员启动任务，卸载已停止，程序文件仍保留。请退出 AgentsPads 后重试；若程序文件损坏，请先用安装包修复。', mbError, MB_OK, IDOK);
    Abort;
  end;
  if CheckForMutexes('Global\AgentsPads.Managed.Running,Global\AgentsPads.Managed.Maintenance') then begin
    SuppressibleMsgBox('启动任务已清理，但 AgentsPads 维护尚未结束。程序文件仍保留，请稍后重新卸载。', mbError, MB_OK, IDOK);
    Abort;
  end;
end;
