const
  PROCESS_TERMINATE = $0001;
  PROCESS_QUERY_LIMITED_INFORMATION = $1000;
  SYNCHRONIZE = $00100000;
  WAIT_OBJECT_0 = 0;
  WM_CLOSE = $0010;
  CSTR_EQUAL = 2;

function EnumProcesses(var FirstId: LongWord; BufferSize: LongWord; var BytesUsed: LongWord): BOOL;
external 'K32EnumProcesses@kernel32.dll stdcall';
function OpenProcess(Access: LongWord; Inherit: BOOL; ProcessId: LongWord): THandle;
external 'OpenProcess@kernel32.dll stdcall';
function QueryFullProcessImageName(Process: THandle; Flags: LongWord; Path: String; var Size: LongWord): BOOL;
external 'QueryFullProcessImageNameW@kernel32.dll stdcall';
function CloseHandle(Handle: THandle): BOOL;
external 'CloseHandle@kernel32.dll stdcall';
function WaitForSingleObject(Handle: THandle; Milliseconds: LongWord): LongWord;
external 'WaitForSingleObject@kernel32.dll stdcall';
function TerminateProcess(Process: THandle; ExitCode: LongWord): BOOL;
external 'TerminateProcess@kernel32.dll stdcall';
function EnumWindows(Callback: LongWord; Param: LongWord): BOOL;
external 'EnumWindows@user32.dll stdcall';
function GetWindowThreadProcessId(Window: HWND; var ProcessId: LongWord): LongWord;
external 'GetWindowThreadProcessId@user32.dll stdcall';
function PostMessage(Window: HWND; Msg, WParam, LParam: LongWord): BOOL;
external 'PostMessageW@user32.dll stdcall';
function CompareStringOrdinal(A: String; ALength: Integer; B: String; BLength: Integer; IgnoreCase: BOOL): Integer;
external 'CompareStringOrdinal@kernel32.dll stdcall';

var
  StartupTaskDir: String;
  DefaultStartupTask: Boolean;
  PythonCleanupFailed: Boolean;

function SamePath(A, B: String): Boolean;
begin
  Result := CompareStringOrdinal(A, Length(A), B, Length(B), True) = CSTR_EQUAL;
end;

function RequestWindowClose(Window: HWND; ProcessId: LongWord): BOOL;
var
  Owner: LongWord;
begin
  if (GetWindowThreadProcessId(Window, Owner) <> 0) and (Owner = ProcessId) then
    PostMessage(Window, WM_CLOSE, 0, 0);
  Result := True;
end;

function CloseCopy(ProcessId: LongWord; VerifiedProcess: THandle): Boolean;
var
  TerminationHandle: THandle;
begin
  Log('Closing the CopiaGo process in the destination directory (PID ' + IntToStr(ProcessId) + ').');
  EnumWindows(CreateCallback(@RequestWindowClose), ProcessId);
  Result := WaitForSingleObject(VerifiedProcess, 3000) = WAIT_OBJECT_0;
  if Result then
    Exit;

  // Keep the verified handle open until finished so that Windows cannot recycle this PID.
  TerminationHandle := OpenProcess(PROCESS_TERMINATE, False, ProcessId);
  if TerminationHandle <> 0 then
  try
    Log('CopiaGo did not close after WM_CLOSE; terminating the verified process.');
    TerminateProcess(TerminationHandle, 1);
  finally
    CloseHandle(TerminationHandle);
  end;
  Result := WaitForSingleObject(VerifiedProcess, 5000) = WAIT_OBJECT_0;
end;

function CloseInstalledCopies(Directory: String): Boolean;
var
  PythonExe, RustExe, Image: String;
  Ids: array of LongWord;
  BufferSize, BytesUsed, ImageSize, ProcessId: LongWord;
  Count, I: Integer;
  Process: THandle;
begin
  PythonExe := AddBackslash(Directory) + '{#PythonExeName}';
  RustExe := AddBackslash(Directory) + '{#AppExeName}';
  Result := True;
  if not (FileExists(PythonExe) or FileExists(RustExe)) then
    Exit;

  Count := 256;
  repeat
    SetArrayLength(Ids, Count);
    BufferSize := Count * SizeOf(Ids[0]);
    if not EnumProcesses(Ids[0], BufferSize, BytesUsed) then
    begin
      Result := False;
      Exit;
    end;
    Count := Count * 2;
  until BytesUsed < BufferSize;

  for I := 0 to (BytesUsed div SizeOf(Ids[0])) - 1 do
  begin
    ProcessId := Ids[I];
    Process := OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION or SYNCHRONIZE, False, ProcessId);
    if Process = 0 then
      Continue;
    try
      ImageSize := 32768;
      SetLength(Image, ImageSize);
      if QueryFullProcessImageName(Process, 0, Image, ImageSize) then
      begin
        SetLength(Image, ImageSize);
        if SamePath(Image, PythonExe) or SamePath(Image, RustExe) then
        begin
          Result := CloseCopy(ProcessId, Process);
          if not Result then
            Exit;
        end;
      end;
    finally
      CloseHandle(Process);
    end;
  end;
end;

function PythonStartupIn(Directory: String): Boolean;
var
  Command: String;
begin
  // Match the Python settings page's exact command comparison.
  Result := RegQueryStringValue(HKCU, '{#StartupKey}', '{#StartupValue}', Command) and
    (Command = '"' + AddBackslash(Directory) + '{#PythonExeName}" --minimized');
end;

function HasPythonStartup: Boolean;
begin
  Result := PythonStartupIn(ExpandConstant('{app}'));
end;

procedure CurPageChanged(CurPageID: Integer);
var
  Arg, Directory: String;
  I: Integer;
  SelectStartup: Boolean;
begin
  if CurPageID <> wpSelectTasks then
    Exit;
  for I := 1 to ParamCount do
  begin
    Arg := Uppercase(ParamStr(I));
    if (Pos('/TASKS=', Arg) = 1) or (Pos('/MERGETASKS=', Arg) = 1) then
      Exit;
  end;

  Directory := WizardDirValue;
  if SamePath(Directory, StartupTaskDir) then
    Exit;
  if StartupTaskDir = '' then
    DefaultStartupTask := WizardIsTaskSelected('startup');
  StartupTaskDir := Directory;
  if FileExists(AddBackslash(Directory) + '{#PythonExeName}') then
    SelectStartup := PythonStartupIn(Directory)
  else
    SelectStartup := DefaultStartupTask;
  if SelectStartup then
    WizardSelectTasks('startup')
  else
    WizardSelectTasks('!startup');
end;

#ifdef TestId
const
  DirectoryAttribute = $10;
  ReparsePointAttribute = $400;
  InvalidFileAttributes = $FFFFFFFF;

function GetFileAttributes(Name: String): LongWord;
external 'GetFileAttributesW@kernel32.dll stdcall';

function TestInstallDirIsSafe: Boolean;
var
  ExpectedDir, TempRoot, Directory: String;
  Attributes: LongWord;
begin
  Result := False;
  TempRoot := GetEnv('TEMP');
  if TempRoot = '' then
    Exit;
  TempRoot := RemoveBackslashUnlessRoot(ExpandFileName(TempRoot));
  ExpectedDir := RemoveBackslashUnlessRoot(ExpandFileName('{#TestDirCode}'));
  Directory := RemoveBackslashUnlessRoot(ExpandFileName(WizardDirValue));
  if not SamePath(Directory, ExpectedDir) or
     (Pos(Lowercase(AddBackslash(TempRoot)), Lowercase(Directory)) <> 1) then
    Exit;

  repeat
    Attributes := GetFileAttributes(Directory);
    // An existing component must be an ordinary directory.
    if (Attributes <> InvalidFileAttributes) and
       (((Attributes and DirectoryAttribute) = 0) or
        ((Attributes and ReparsePointAttribute) <> 0)) then
      Exit;
    if SamePath(Directory, TempRoot) then
    begin
      Result := True;
      Exit;
    end;
    Directory := ExtractFileDir(Directory);
  until Directory = '';
end;
#endif

function PrepareToInstall(var NeedsRestart: Boolean): String;
begin
  Result := '';
#ifdef TestId
  if not TestInstallDirIsSafe then
  begin
    Result := 'This test installer is restricted to its build-time directory inside TEMP.';
    Exit;
  end;
#endif
  if not CloseInstalledCopies(WizardDirValue) then
    Result := CustomMessage('CloseCopiaGoFailed');
end;

procedure CurStepChanged(CurStep: TSetupStep);
var
  PythonExe, RuntimeDir: String;
begin
  if CurStep <> ssPostInstall then
    Exit;
  PythonExe := ExpandConstant('{app}\{#PythonExeName}');
  if not FileExists(PythonExe) then
    Exit;

  // Only after installing the new program. Keep the old executable as a retry marker
  // if the old runtime could not be removed.
  RuntimeDir := ExpandConstant('{app}\_internal');
  if DirExists(RuntimeDir) then
    PythonCleanupFailed := not DelTree(RuntimeDir, True, True, True);
  if not PythonCleanupFailed then
    PythonCleanupFailed := not DeleteFile(PythonExe);
  if PythonCleanupFailed then
  begin
    Log('Python program-file cleanup failed.');
    SuppressibleMsgBox(CustomMessage('RemovePythonFailed'), mbError, MB_OK, IDOK);
  end;
end;

function GetCustomSetupExitCode: Integer;
begin
  Result := 0;
  if PythonCleanupFailed then
    Result := 100;
end;
