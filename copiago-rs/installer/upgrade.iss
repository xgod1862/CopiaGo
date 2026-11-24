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
  // Previous installation in the pre-rename default folder, moved to the CopiaGo folder.
  RelocateFrom: String;

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

function StartupValueIn(Value, Directory: String): Boolean;
var
  Command, Prefix: String;
begin
  // Match the exact commands written by either version in this directory.
  Prefix := '"' + AddBackslash(Directory);
  Result := RegQueryStringValue(HKCU, '{#StartupKey}', Value, Command) and
    ((Command = Prefix + '{#PythonExeName}" --minimized') or
     (Command = Prefix + '{#AppExeName}" --minimized'));
end;

function InstalledStartupIn(Directory: String): Boolean;
begin
  Result := StartupValueIn('{#StartupValue}', Directory) or
    StartupValueIn('{#LegacyStartupValue}', Directory);
end;

function HasLegacyStartup: Boolean;
begin
  Result := StartupValueIn('{#LegacyStartupValue}', ExpandConstant('{app}')) or
    ((RelocateFrom <> '') and StartupValueIn('{#LegacyStartupValue}', RelocateFrom));
end;

#ifndef TestId
function OldDefaultDir: String;
begin
  Result := ExpandConstant('{localappdata}\Programs\iPhoneClipboard');
end;

procedure InitializeWizard;
begin
  // UsePreviousAppDir proposes the old folder. Only its default location moves; a folder
  // the user picked stays where it is.
  if SamePath(RemoveBackslashUnlessRoot(WizardForm.DirEdit.Text), OldDefaultDir) then
  begin
    RelocateFrom := OldDefaultDir;
    WizardForm.DirEdit.Text := ExpandConstant('{localappdata}\Programs\CopiaGo');
    Log('Moving the installation from ' + RelocateFrom + ' to ' + WizardForm.DirEdit.Text + '.');
  end;
end;

procedure RelocateOldInstall;
var
  Old, NewData: String;
begin
  if RelocateFrom = '' then
    Exit;
  Old := AddBackslash(RelocateFrom);
  NewData := ExpandConstant('{app}\data');
  // Same volume, so the account folders move whole with their ACLs. Never merge. Whatever
  // stays behind is kept: CopiaGo copies an account's data from here on first start while
  // its new data folder is still empty, so a failed move recovers and can be retried.
  if DirExists(Old + 'data') then
    if DirExists(NewData) then
    begin
      Log('The destination already has data; keeping the old data folder.');
      if DirExists(NewData + '\' + ExpandConstant('{username}')) and
         DirExists(Old + 'data\' + ExpandConstant('{username}')) then
        SuppressibleMsgBox(FmtMessage(CustomMessage('OldDataKept'), [Old + 'data']),
          mbInformation, MB_OK, IDOK);
    end
    else if not RenameFile(Old + 'data', NewData) then
      Log('Moving the old data folder failed; CopiaGo copies it on first start.');
  DelTree(Old + '_internal', True, True, True);
  DelTree(Old + 'licenses', True, True, True);
  DeleteFile(Old + '{#PythonExeName}');
  DeleteFile(Old + '{#AppExeName}');
  DeleteFile(Old + 'LICENSE');
  DeleteFile(Old + 'unins000.exe');
  DeleteFile(Old + 'unins000.dat');
  // Removes the folder only when nothing else is left in it.
  RemoveDir(RelocateFrom);
end;
#endif

function HasInstalledStartup: Boolean;
begin
  Result := StartupValueIn('{#StartupValue}', ExpandConstant('{app}'));
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
  if FileExists(AddBackslash(Directory) + '{#PythonExeName}') or
     FileExists(AddBackslash(Directory) + '{#AppExeName}') then
    SelectStartup := InstalledStartupIn(Directory)
  else if RelocateFrom <> '' then
    // Keep the sign-in choice of the copy being moved.
    SelectStartup := InstalledStartupIn(RelocateFrom)
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
  // The user may have picked the old folder again on the directory page.
  if (RelocateFrom <> '') and SamePath(RemoveBackslashUnlessRoot(WizardDirValue), RelocateFrom) then
    RelocateFrom := '';
  if not CloseInstalledCopies(WizardDirValue) or
     ((RelocateFrom <> '') and not CloseInstalledCopies(RelocateFrom)) then
    Result := CustomMessage('CloseCopiaGoFailed');
end;

procedure CurStepChanged(CurStep: TSetupStep);
var
  PythonExe, RuntimeDir: String;
begin
  if CurStep <> ssPostInstall then
    Exit;
#ifndef TestId
  RelocateOldInstall;
#endif
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
