; Inno Setup script for the Windows installer. CI passes /DAppVersion=x.y.z.
; Build locally with: iscc /DAppVersion=0.1.0 installer\screenshot.iss

#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif

[Setup]
AppId={{6F0B2A1E-3C4D-4B5A-9E8F-71C2D3A4B5C6}
AppName=Screenshot
AppVersion={#AppVersion}
AppPublisher=Tim
AppPublisherURL=https://github.com/TimWJT/screenshot
AppSupportURL=https://github.com/TimWJT/screenshot/issues
DefaultDirName={localappdata}\Programs\Screenshot
DefaultGroupName=Screenshot
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
LicenseFile=..\LICENSE
OutputDir=..\dist
OutputBaseFilename=Screenshot_{#AppVersion}_x64-setup
SetupIconFile=..\assets\icon.ico
UninstallDisplayIcon={app}\screenshot.exe
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
; The app holds this mutex while running, so setup asks to close it before updating.
AppMutex=TimWJT.Screenshot.Instance
CloseApplications=yes

[Tasks]
Name: "startup"; Description: "Start with Windows (stays in the tray)"; GroupDescription: "Options:"

[Files]
Source: "..\target\release\screenshot.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\LICENSE"; DestDir: "{app}"; DestName: "LICENSE.txt"; Flags: ignoreversion

[Icons]
Name: "{group}\Screenshot"; Filename: "{app}\screenshot.exe"
Name: "{group}\Uninstall Screenshot"; Filename: "{uninstallexe}"

[Registry]
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueType: string; ValueName: "Screenshot"; ValueData: """{app}\screenshot.exe"" --tray"; Flags: uninsdeletevalue; Tasks: startup
; Settings can turn "start with Windows" on later, so remove the value on uninstall either way.
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueType: none; ValueName: "Screenshot"; Flags: uninsdeletevalue dontcreatekey

[Run]
Filename: "{app}\screenshot.exe"; Parameters: "--tray"; Description: "Start Screenshot now"; Flags: nowait postinstall skipifsilent

[UninstallRun]
; Closes this app's tray icon, pins and any open capture (not every process that happens to be
; called screenshot.exe) and waits for them to exit.
Filename: "{app}\screenshot.exe"; Parameters: "--quit"; Flags: runhidden waituntilterminated; RunOnceId: "StopScreenshot"

[UninstallDelete]
Type: filesandordirs; Name: "{userappdata}\Screenshot"
