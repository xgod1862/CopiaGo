#ifndef Root
  #define Root ".."
#endif
#define Name "CopiaGo"
#define Version "0.3.5"
#ifndef DistDir
  #define DistDir Root + "\build\dist\iPhoneClipboard"
#endif

[Setup]
AppId={{D402F563-DC4B-402B-8F7A-240F830C76F5}
AppName={#Name}
AppVersion={#Version}
DefaultDirName={localappdata}\Programs\iPhoneClipboard
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
DisableProgramGroupPage=yes
DisableWelcomePage=yes
WizardStyle=modern
Compression=lzma2/ultra64
LZMAUseSeparateProcess=yes
SolidCompression=yes
OutputDir={#Root}\release
OutputBaseFilename={#Name}-Setup-{#Version}-x64
VersionInfoDescription={#Name} Setup
VersionInfoProductName={#Name}
SetupIconFile={#Root}\source\iphone_clip\assets\jade.ico
UninstallDisplayIcon={app}\iPhoneClipboard.exe
AppMutex=Local\iPhoneClipboardPrototype

[Languages]
Name: "chinesesimp"; MessagesFile: "ChineseSimplified.isl"

[Tasks]
Name: startup; Description: "登录 Windows 后自动启动"; Flags: checkedonce

[Files]
Source: "{#DistDir}\*"; DestDir: "{app}"; Flags: ignoreversion recursesubdirs createallsubdirs
Source: "{#Root}\source\README.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#Root}\source\LICENSE"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#Root}\source\licenses\*"; DestDir: "{app}\licenses"; Flags: ignoreversion recursesubdirs createallsubdirs

[InstallDelete]
; Remove shortcuts from previous display names.
Type: files; Name: "{userprograms}\iPhone 剪贴板.lnk"
Type: files; Name: "{userprograms}\ClipX.lnk"

[Icons]
Name: "{userprograms}\{#Name}"; Filename: "{app}\iPhoneClipboard.exe"

[Registry]
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueType: string; ValueName: "iPhoneClipboardPrototype"; ValueData: """{app}\iPhoneClipboard.exe"" --minimized"; Tasks: startup; Flags: uninsdeletevalue
; The app can also turn this on later, so remove it on uninstall even when the task was not chosen.
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueType: none; ValueName: "iPhoneClipboardPrototype"; Flags: uninsdeletevalue dontcreatekey

[Run]
Filename: "{app}\iPhoneClipboard.exe"; Description: "打开并连接 iPhone"; Flags: postinstall nowait skipifsilent runasoriginaluser

[Code]
procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
var
  DataDir: String;
begin
  if CurUninstallStep <> usPostUninstall then
    Exit;
  DataDir := ExpandConstant('{localappdata}\iPhoneClipboardPrototype');
  // Keep data by default so a reinstall can reuse the pairing.
  if DirExists(DataDir) and not UninstallSilent then
    if MsgBox('是否同时删除 iPhone 配对记录和剪贴板历史？' + #13#10 + '选择「否」会保留这些数据，重新安装后可直接使用。',
              mbConfirmation, MB_YESNO or MB_DEFBUTTON2) = IDYES then
      DelTree(DataDir, True, True, True);
end;

