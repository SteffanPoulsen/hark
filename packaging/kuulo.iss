; kuulo's Windows installer: per user into %LOCALAPPDATA%\Programs with no admin prompt, a
; Start menu entry, kuulo-cli on the user's PATH, and an uninstall entry that leaves the
; library, settings and copies in place. Built from the release folder the workflow stages:
;   ISCC.exe /DVersion=<version> packaging\kuulo.iss

#ifndef Version
  #error Pass the version: ISCC.exe /DVersion=<version> packaging\kuulo.iss
#endif
#define Staged "dist\kuulo-" + Version + "-windows-x86_64"

[Setup]
AppId=dk.steffanpoulsen.kuulo
AppName=kuulo
AppVersion={#Version}
AppPublisher=Steffan Poulsen
AppPublisherURL=https://github.com/SteffanPoulsen/kuulo
AppSupportURL=https://github.com/SteffanPoulsen/kuulo/issues
AppUpdatesURL=https://github.com/SteffanPoulsen/kuulo/releases
SourceDir=..
OutputDir=dist
OutputBaseFilename=kuulo-{#Version}-windows-x86_64-setup
SetupIconFile=crates\kuulo\assets\icon\kuulo.ico
UninstallDisplayIcon={app}\kuulo.exe
UninstallDisplayName=kuulo
DefaultDirName={autopf}\kuulo
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
ChangesEnvironment=yes
WizardStyle=modern
Compression=lzma2
SolidCompression=yes

[Tasks]
Name: path; Description: "Add kuulo-cli to PATH"
Name: desktopicon; Description: "{cm:CreateDesktopIcon}"; Flags: unchecked

[Files]
Source: "{#Staged}\kuulo.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#Staged}\kuulo-cli.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#Staged}\README.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#Staged}\LICENSE"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\kuulo"; Filename: "{app}\kuulo.exe"
Name: "{autodesktop}\kuulo"; Filename: "{app}\kuulo.exe"; Tasks: desktopicon

[Registry]
Root: HKCU; Subkey: "Environment"; ValueType: expandsz; ValueName: "Path"; ValueData: "{olddata};{app}"; Tasks: path; Check: NotOnPath(ExpandConstant('{app}'))

[Run]
Filename: "{app}\kuulo.exe"; Description: "{cm:LaunchProgram,kuulo}"; Flags: nowait postinstall skipifsilent
; kuulo installs its own update silently and passes /LAUNCH=1 to be started again.
Filename: "{app}\kuulo.exe"; Flags: nowait; Check: LaunchRequested

[Code]
function LaunchRequested: Boolean;
begin
  Result := ExpandConstant('{param:LAUNCH|0}') = '1';
end;

function PathPos(Dir: string; var Path: string): Integer;
begin
  if not RegQueryStringValue(HKCU, 'Environment', 'Path', Path) then
    Path := '';
  Result := Pos(';' + Uppercase(Dir) + ';', ';' + Uppercase(Path) + ';');
end;

function NotOnPath(Dir: string): Boolean;
var
  Path: string;
begin
  Result := PathPos(Dir, Path) = 0;
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
var
  Path, Dir: string;
  P: Integer;
begin
  if CurUninstallStep <> usPostUninstall then
    Exit;
  Dir := ExpandConstant('{app}');
  P := PathPos(Dir, Path);
  if P = 0 then
    Exit;
  { P counts from a ';' put in front of Path, so it is the index of the separator before Dir. }
  if P = 1 then
    Delete(Path, 1, Length(Dir) + 1)
  else
    Delete(Path, P - 1, Length(Dir) + 1);
  RegWriteExpandStringValue(HKCU, 'Environment', 'Path', Path);
end;
