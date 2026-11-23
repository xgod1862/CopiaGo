#ifndef Root
  #define Root ".."
#endif
#define Name "CopiaGo"
#define Version "1.2.0"
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
LanguageDetectionMethod=uilanguage
UsePreviousLanguage=no
ShowLanguageDialog=no
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
Name: "english"; MessagesFile: "compiler:Default.isl"
Name: "chinesesimp"; MessagesFile: "ChineseSimplified.isl"

[CustomMessages]
english.StartAtSignIn=Start CopiaGo when I sign in to Windows
chinesesimp.StartAtSignIn=登录 Windows 后自动启动
english.OpenCopiaGo=Open CopiaGo and connect an iPhone
chinesesimp.OpenCopiaGo=打开并连接 iPhone
english.DeleteClipData=Also remove iPhone pairing records and clipboard history?%nChoose No to keep these for a future installation.
chinesesimp.DeleteClipData=是否同时删除 iPhone 配对记录和剪贴板历史？%n选择「否」会保留这些数据，重新安装后可直接使用。

[Tasks]
Name: startup; Description: "{cm:StartAtSignIn}"; Flags: checkedonce

[Files]
Source: "{#DistDir}\*"; DestDir: "{app}"; Flags: ignoreversion recursesubdirs createallsubdirs
Source: "{#Root}\source\README.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#Root}\LICENSE"; DestDir: "{app}"; Flags: ignoreversion
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
Filename: "{app}\iPhoneClipboard.exe"; Description: "{cm:OpenCopiaGo}"; Flags: postinstall nowait skipifsilent runasoriginaluser

[Code]
procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
var
  DataDir, LegacyDir: String;
begin
  if CurUninstallStep <> usPostUninstall then
    Exit;
  // After 1.1.0 each Windows account keeps its data in {app}\data\<account name>; earlier
  // versions (and a move that failed) used LOCALAPPDATA. {username} matches the app's folder name.
  DataDir := ExpandConstant('{app}\data\{username}');
  LegacyDir := ExpandConstant('{localappdata}\iPhoneClipboardPrototype');
  // Keep data by default so a reinstall into the same folder can reuse the pairing.
  if (DirExists(DataDir) or DirExists(LegacyDir)) and not UninstallSilent then
    if MsgBox(CustomMessage('DeleteClipData'),
              mbConfirmation, MB_YESNO or MB_DEFBUTTON2) = IDYES then
    begin
      DelTree(DataDir, True, True, True);
      DelTree(LegacyDir, True, True, True);
    end;
  // The uninstaller skipped these folders while data was still inside. Removed only when empty.
  RemoveDir(ExpandConstant('{app}\data'));
  RemoveDir(ExpandConstant('{app}'));
end;

