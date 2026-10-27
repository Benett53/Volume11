; Inno Setup script for Volume11.
;
; Per-user install by default, so no administrator rights are needed. That
; matches an application whose autostart entry and settings are per-user
; anyway. Running the setup elevated installs to Program Files instead.
;
; Build:  ISCC.exe /DAppVersion=0.2.1 installer\volume11.iss

#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif

#define AppName "Volume11"
#define AppExe  "Volume11.exe"

[Setup]
; Fixed across releases: this is what lets a new version replace an old one
; in place instead of installing beside it.
AppId={{4F2C1E6A-93D5-4B7A-9C1E-6D3A8B5F0271}
AppName={#AppName}
AppVersion={#AppVersion}
VersionInfoVersion={#AppVersion}
AppPublisher={#AppName}
WizardStyle=modern
DisableProgramGroupPage=yes
DisableDirPage=auto
DefaultDirName={autopf}\{#AppName}
DefaultGroupName={#AppName}
UninstallDisplayName={#AppName}
UninstallDisplayIcon={app}\{#AppExe}
OutputDir=..\dist
OutputBaseFilename=Volume11-Setup
SetupIconFile=..\assets\icon.ico
Compression=lzma2/max
SolidCompression=yes
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
; Backstop in case the running instance does not respond to the quit request.
CloseApplications=force
RestartApplications=no

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "Create a desktop shortcut"; Flags: unchecked

[Files]
Source: "..\dist\{#AppExe}"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{group}\{#AppName}"; Filename: "{app}\{#AppExe}"
Name: "{autodesktop}\{#AppName}"; Filename: "{app}\{#AppExe}"; Tasks: desktopicon

[Run]
Filename: "{app}\{#AppExe}"; Description: "Start {#AppName}"; Flags: nowait postinstall skipifsilent
Filename: "{app}\{#AppExe}"; Parameters: "--updated"; Flags: nowait runasoriginaluser; Check: RestartAfterUpdate

[UninstallDelete]
; The application writes no files next to its executable, but an upgrade from a
; portable copy may have left one.
Type: dirifempty; Name: "{app}"

[Code]
{ HWND_BROADCAST is already defined by Inno Setup itself. }

function RegisterWindowMessageW(lpString: string): LongWord;
  external 'RegisterWindowMessageW@user32.dll stdcall';

function PostMessageW(hWnd: LongInt; Msg: LongWord; wParam, lParam: LongInt): BOOL;
  external 'PostMessageW@user32.dll stdcall';

{ A running Volume11 holds its own executable open, so it has to go before the
  files are replaced or removed. It listens for this message and shuts down
  cleanly, saving its configuration on the way out. }
procedure AskRunningInstanceToQuit();
var
  Message: LongWord;
begin
  Message := RegisterWindowMessageW('Volume11.QuitNow');
  if Message <> 0 then
  begin
    PostMessageW(HWND_BROADCAST, Message, 0, 0);
    { Give it a moment to save and release the file. }
    Sleep(1200);
  end;
end;

{ Volume11 passes /restartapp=1 when it installs an update of itself, so it
  comes back after the silent install instead of staying closed. }
function RestartAfterUpdate(): Boolean;
begin
  Result := WizardSilent() and (ExpandConstant('{param:restartapp|0}') = '1');
end;

function PrepareToInstall(var NeedsRestart: Boolean): String;
begin
  AskRunningInstanceToQuit();
  Result := '';
end;

function InitializeUninstall(): Boolean;
begin
  AskRunningInstanceToQuit();
  Result := True;
end;

{ An autostart entry written by an earlier copy - a downloaded executable, a
  portable folder - keeps pointing there after this install. If that file is
  later deleted, Windows starts nothing while the settings still say autostart
  is on. The entry is repointed here; the Task Manager status byte is left as
  it is, so a disabled entry stays disabled. The application does the same on
  every start, but a silent update never starts it. }
procedure AdoptAutostartEntry();
var
  Existing: String;
begin
  if RegQueryStringValue(HKEY_CURRENT_USER,
       'Software\Microsoft\Windows\CurrentVersion\Run', 'Volume11', Existing) then
    RegWriteStringValue(HKEY_CURRENT_USER,
      'Software\Microsoft\Windows\CurrentVersion\Run', 'Volume11',
      '"' + ExpandConstant('{app}\{#AppExe}') + '"');
end;

procedure CurStepChanged(CurStep: TSetupStep);
begin
  if CurStep = ssPostInstall then
    AdoptAutostartEntry();
end;

{ Autostart is owned by the application, not by this installer: it writes the
  Run command and the Task Manager status byte itself. Uninstalling has to
  remove both, otherwise Windows keeps a dead startup entry. }
procedure RemoveAutostartEntries();
begin
  RegDeleteValue(HKEY_CURRENT_USER,
    'Software\Microsoft\Windows\CurrentVersion\Run', 'Volume11');
  RegDeleteValue(HKEY_CURRENT_USER,
    'Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run',
    'Volume11');
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
var
  Settings: String;
begin
  if CurUninstallStep = usPostUninstall then
  begin
    RemoveAutostartEntries();

    { Saved volumes are the user's data, so removing them is asked, not assumed.
      Keeping them means reinstalling restores the whole setup. }
    Settings := ExpandConstant('{userappdata}\Volume11');
    if DirExists(Settings) then
    begin
      if SuppressibleMsgBox('Remove saved volumes and settings as well?',
           mbConfirmation, MB_YESNO or MB_DEFBUTTON2, IDNO) = IDYES then
        DelTree(Settings, True, True, True);
    end;
  end;
end;
