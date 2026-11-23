#define AppName "CopiaGo"
#define RepoRoot SourcePath + "\..\.."
#define PythonExeName "iPhoneClipboard.exe"
#define AppExeName "copiago.exe"
#define StartupValue "iPhoneClipboardPrototype"
#ifndef PayloadDir
  #error Define PayloadDir with the path to the staged release files.
#endif
#define AppExe PayloadDir + "\" + AppExeName
#define AppVersion GetStringFileInfo(AppExe, "ProductVersion")
#define FileVersion GetVersionNumbersString(AppExe)
#ifndef PackageArch
  #define PackageArch "x64"
#endif
#if PackageArch == "x64"
  #define InnoArch "x64compatible"
#elif PackageArch == "arm64"
  #define InnoArch "arm64"
#else
  #error Unsupported package architecture.
#endif
#ifdef TestId
  #ifndef TestDir
    #error TestId and TestDir must be defined together.
  #endif
  #if Len(TestId) != 32
    #error TestId must be a GUID without separators.
  #endif
  #define TestAppName AppName + " Test " + TestId
  #define TestDirConstant StringChange(TestDir, "{", "{{")
  #define TestDirCode StringChange(TestDir, "'", "''")
  #define StartupKey "Software\CopiaGo\Test\" + TestId
#else
  #ifdef TestDir
    #error TestId and TestDir must be defined together.
  #endif
  #define StartupKey "Software\Microsoft\Windows\CurrentVersion\Run"
#endif

[Setup]
#ifdef TestId
AppId=CopiaGo.Test.{#TestId}
AppName={#TestAppName}
DefaultDirName={#TestDirConstant}
DisableDirPage=yes
OutputBaseFilename={#AppName}-Test-{#AppVersion}-{#PackageArch}-{#TestId}
#else
AppId={{D402F563-DC4B-402B-8F7A-240F830C76F5}
AppName={#AppName}
DefaultDirName={localappdata}\Programs\iPhoneClipboard
UsePreviousAppDir=yes
AppMutex={code:UninstallAppMutex}
OutputBaseFilename={#AppName}-Setup-{#AppVersion}-{#PackageArch}
#endif
CloseApplications=no
RestartApplications=no
AppVersion={#AppVersion}
PrivilegesRequired=lowest
MinVersion=10.0.19044
ArchitecturesAllowed={#InnoArch}
ArchitecturesInstallIn64BitMode={#InnoArch}
DisableProgramGroupPage=yes
DisableWelcomePage=yes
WizardStyle=modern
LanguageDetectionMethod=uilanguage
UsePreviousLanguage=no
ShowLanguageDialog=no
Compression=lzma2/ultra64
LZMAUseSeparateProcess=yes
SolidCompression=yes
VersionInfoDescription={#AppName} Setup
VersionInfoProductName={#AppName}
VersionInfoVersion={#FileVersion}
VersionInfoTextVersion={#AppVersion}
VersionInfoProductVersion={#FileVersion}
VersionInfoProductTextVersion={#AppVersion}
SetupIconFile={#RepoRoot}\source\iphone_clip\assets\jade.ico
UninstallDisplayIcon={app}\{#AppExeName}
#ifdef SignRelease
SignTool=CopiaGoRelease
#endif

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"
Name: "chinesesimp"; MessagesFile: "{#RepoRoot}\source\ChineseSimplified.isl"

[Files]
#ifdef SignRelease
Source: "{#AppExe}"; DestDir: "{app}"; Flags: ignoreversion sign
#else
Source: "{#AppExe}"; DestDir: "{app}"; Flags: ignoreversion
#endif
Source: "{#PayloadDir}\LICENSE"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#PayloadDir}\licenses\*"; DestDir: "{app}\licenses"; Flags: ignoreversion

[CustomMessages]
english.StartAtSignIn=Start CopiaGo when I sign in to Windows
chinesesimp.StartAtSignIn=登录 Windows 后自动启动
english.CloseCopiaGoFailed=Setup could not close CopiaGo in this installation directory. Exit that copy of CopiaGo and run Setup again.
chinesesimp.CloseCopiaGoFailed=安装程序无法关闭此安装目录中的 CopiaGo。请退出该副本后重新运行安装程序。
english.RemovePythonFailed=CopiaGo was updated, but Setup could not remove all Python program files. Exit the old copy and run Setup again to finish cleanup.
chinesesimp.RemovePythonFailed=CopiaGo 已更新，但安装程序未能清理所有 Python 程序文件。请退出旧版后重新运行安装程序以完成清理。
english.OpenCopiaGo=Open CopiaGo and connect an iPhone
chinesesimp.OpenCopiaGo=打开并连接 iPhone
english.DeleteClipData=Also remove iPhone pairing records and clipboard history?%nChoose No to keep these for a future installation.
chinesesimp.DeleteClipData=是否同时删除 iPhone 配对记录和剪贴板历史？%n选择「否」会保留这些数据，重新安装后可直接使用。

[Tasks]
Name: startup; Description: "{cm:StartAtSignIn}"; Flags: checkedonce

[Registry]
Root: HKCU; Subkey: "{#StartupKey}"; ValueType: string; ValueName: "{#StartupValue}"; ValueData: """{app}\{#AppExeName}"" --minimized"; Tasks: startup; Flags: uninsdeletevalue
Root: HKCU; Subkey: "{#StartupKey}"; ValueType: none; ValueName: "{#StartupValue}"; Tasks: not startup; Check: HasInstalledStartup; Flags: deletevalue dontcreatekey
; The app can enable startup after setup.
Root: HKCU; Subkey: "{#StartupKey}"; ValueType: none; ValueName: "{#StartupValue}"; Flags: uninsdeletevalue dontcreatekey

#ifndef TestId
[InstallDelete]
; Remove shortcuts from previous display names.
Type: files; Name: "{userprograms}\iPhone 剪贴板.lnk"
Type: files; Name: "{userprograms}\ClipX.lnk"

[Icons]
Name: "{userprograms}\{#AppName}"; Filename: "{app}\{#AppExeName}"

[Run]
Filename: "{app}\{#AppExeName}"; Description: "{cm:OpenCopiaGo}"; Flags: postinstall nowait skipifsilent runasoriginaluser
#endif

[Code]
#include "upgrade.iss"
#ifndef TestId
function UninstallAppMutex(Param: String): String;
begin
  Result := '';
  if IsUninstaller then
    Result := 'Local\iPhoneClipboardPrototype';
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
var
  DataDir, LegacyDir, BackupDir: String;
begin
  if CurUninstallStep <> usPostUninstall then
    Exit;
  DataDir := ExpandConstant('{app}\data\{username}');
  LegacyDir := ExpandConstant('{localappdata}\iPhoneClipboardPrototype');
  BackupDir := LegacyDir + '.before-rust-upgrade';
  if (DirExists(DataDir) or DirExists(LegacyDir) or DirExists(BackupDir)) and not UninstallSilent then
    if MsgBox(CustomMessage('DeleteClipData'),
              mbConfirmation, MB_YESNO or MB_DEFBUTTON2) = IDYES then
    begin
      DelTree(DataDir, True, True, True);
      DelTree(LegacyDir, True, True, True);
      DelTree(BackupDir, True, True, True);
    end;
  RemoveDir(ExpandConstant('{app}\data'));
  RemoveDir(ExpandConstant('{app}'));
end;
#endif
