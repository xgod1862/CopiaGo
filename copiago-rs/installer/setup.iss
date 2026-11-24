#define AppName "CopiaGo"
#define RepoRoot SourcePath + "\..\.."
#ifndef PayloadDir
  #error Define PayloadDir with the path to the staged release files.
#endif
#define AppExe PayloadDir + "\copiago.exe"
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
#else
  #ifdef TestDir
    #error TestId and TestDir must be defined together.
  #endif
#endif

[Setup]
#ifdef TestId
AppId=CopiaGo.Test.{#TestId}
AppName={#TestAppName}
DefaultDirName={#TestDirConstant}
DisableDirPage=yes
CloseApplications=no
RestartApplications=no
OutputBaseFilename={#AppName}-Test-{#AppVersion}-{#PackageArch}-{#TestId}
#else
AppId={{D402F563-DC4B-402B-8F7A-240F830C76F5}
AppName={#AppName}
DefaultDirName={localappdata}\Programs\iPhoneClipboard
UsePreviousAppDir=yes
AppMutex=Local\iPhoneClipboardPrototype
OutputBaseFilename={#AppName}-Setup-{#AppVersion}-{#PackageArch}
#endif
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
UninstallDisplayIcon={app}\copiago.exe
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

#ifdef TestId
[Code]
function PrepareToInstall(var NeedsRestart: Boolean): String;
var
  ExpectedDir, TempRoot: String;
begin
  ExpectedDir := RemoveBackslashUnlessRoot(ExpandFileName('{#TestDirCode}'));
  TempRoot := GetEnv('TEMP');
  Result := '';
  if (TempRoot = '') or
     (Pos(Lowercase(AddBackslash(ExpandFileName(TempRoot))), Lowercase(ExpectedDir)) <> 1) or
     (CompareText(RemoveBackslashUnlessRoot(ExpandFileName(WizardDirValue)), ExpectedDir) <> 0) then
    Result := 'This test installer is restricted to its build-time directory inside TEMP.';
end;
#else
[CustomMessages]
english.StartAtSignIn=Start CopiaGo when I sign in to Windows
chinesesimp.StartAtSignIn=登录 Windows 后自动启动
english.OpenCopiaGo=Open CopiaGo and connect an iPhone
chinesesimp.OpenCopiaGo=打开并连接 iPhone
english.DeleteClipData=Also remove iPhone pairing records and clipboard history?%nChoose No to keep these for a future installation.
chinesesimp.DeleteClipData=是否同时删除 iPhone 配对记录和剪贴板历史？%n选择「否」会保留这些数据，重新安装后可直接使用。

[Tasks]
Name: startup; Description: "{cm:StartAtSignIn}"; Flags: checkedonce

[InstallDelete]
; Remove shortcuts from previous display names.
Type: files; Name: "{userprograms}\iPhone 剪贴板.lnk"
Type: files; Name: "{userprograms}\ClipX.lnk"

[Icons]
Name: "{userprograms}\{#AppName}"; Filename: "{app}\copiago.exe"

[Registry]
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueType: string; ValueName: "iPhoneClipboardPrototype"; ValueData: """{app}\copiago.exe"" --minimized"; Tasks: startup; Flags: uninsdeletevalue
; The app can enable startup after setup.
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueType: none; ValueName: "iPhoneClipboardPrototype"; Flags: uninsdeletevalue dontcreatekey

[Run]
Filename: "{app}\copiago.exe"; Description: "{cm:OpenCopiaGo}"; Flags: postinstall nowait skipifsilent runasoriginaluser

[Code]
procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
var
  DataDir, LegacyDir: String;
begin
  if CurUninstallStep <> usPostUninstall then
    Exit;
  DataDir := ExpandConstant('{app}\data\{username}');
  LegacyDir := ExpandConstant('{localappdata}\iPhoneClipboardPrototype');
  if (DirExists(DataDir) or DirExists(LegacyDir)) and not UninstallSilent then
    if MsgBox(CustomMessage('DeleteClipData'),
              mbConfirmation, MB_YESNO or MB_DEFBUTTON2) = IDYES then
    begin
      DelTree(DataDir, True, True, True);
      DelTree(LegacyDir, True, True, True);
    end;
  RemoveDir(ExpandConstant('{app}\data'));
  RemoveDir(ExpandConstant('{app}'));
end;
#endif
