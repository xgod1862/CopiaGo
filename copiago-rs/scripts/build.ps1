[CmdletBinding()]
param(
    [string]$IsccPath,
    [string]$OutputDir,
    [ValidateSet('x86_64-pc-windows-msvc', 'aarch64-pc-windows-msvc')]
    [string]$Target = 'x86_64-pc-windows-msvc',
    [ValidatePattern('^[0-9a-fA-F]{32}$')]
    [string]$TestId,
    [string]$TestInstallDir,
    [string]$SignToolCommand
)

$ErrorActionPreference = 'Stop'
$workspace = (Get-Item -LiteralPath (Join-Path $PSScriptRoot '..')).FullName
$repo = Split-Path -Parent $workspace
$targetDir = Join-Path $workspace 'target'
if (!$OutputDir) { $OutputDir = Join-Path $targetDir 'packages' }

if ([bool]$TestId -ne [bool]$TestInstallDir) {
    throw 'Supply -TestId and -TestInstallDir together.'
}
if ($TestId) {
    if ($SignToolCommand) { throw 'Signing requires a release build without -TestId.' }
    $tempRoot = [IO.Path]::GetFullPath([IO.Path]::GetTempPath()).TrimEnd('\')
    $TestInstallDir = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($TestInstallDir).TrimEnd('\')
    if (!$TestInstallDir.StartsWith("$tempRoot\", [StringComparison]::OrdinalIgnoreCase)) {
        throw "-TestInstallDir must be inside $tempRoot."
    }
    for ($path = $TestInstallDir; $path -ne $tempRoot; $path = Split-Path -Parent $path) {
        if (Test-Path -LiteralPath $path) {
            $item = Get-Item -LiteralPath $path -Force
            if (!$item.PSIsContainer -or ($item.Attributes -band [IO.FileAttributes]::ReparsePoint)) {
                throw "Invalid test directory: $path"
            }
        }
    }
}

if (!$IsccPath) {
    $IsccPath = (Get-Command ISCC.exe -CommandType Application -ErrorAction SilentlyContinue |
        Select-Object -First 1).Source
}
if (!$IsccPath) {
    $candidates = @(
        (Join-Path $repo 'toolchain\InnoSetup\ISCC.exe')
        (Join-Path ([Environment]::GetEnvironmentVariable('ProgramFiles(x86)')) 'Inno Setup 6\ISCC.exe')
    )
    $IsccPath = $candidates | Where-Object { Test-Path -LiteralPath $_ -PathType Leaf } |
        Select-Object -First 1
}
if (!$IsccPath) { throw 'Inno Setup 6.5+ not found. Supply -IsccPath.' }
$IsccPath = (Get-Item -LiteralPath $IsccPath).FullName
$arch = if ($Target -eq 'x86_64-pc-windows-msvc') { 'x64' } else { 'arm64' }

Push-Location -LiteralPath $workspace
try {
    & cargo build --release --locked --package copiago-app --bin copiago --target $Target --target-dir $targetDir
    if ($LASTEXITCODE -ne 0) { throw "cargo build failed (exit $LASTEXITCODE)." }
} finally {
    Pop-Location
}
$exe = Get-Item -LiteralPath (Join-Path $targetDir "$Target\release\copiago.exe")
$version = $exe.VersionInfo.ProductVersion
if (!$version) { throw 'copiago.exe is missing its product version resource.' }

$outputPath = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($OutputDir)
$outputPath = [IO.Directory]::CreateDirectory($outputPath).FullName
$stagePath = Join-Path $outputPath ('.package-' + [guid]::NewGuid().ToString('N'))
$stage = New-Item -ItemType Directory -Path $stagePath
try {
    $payload = New-Item -ItemType Directory -Path (Join-Path $stage.FullName 'payload')
    Copy-Item -LiteralPath $exe.FullName,(Join-Path $repo 'LICENSE') -Destination $payload.FullName
    $licenses = New-Item -ItemType Directory -Path (Join-Path $payload.FullName 'licenses')
    $chineseLicense = Join-Path $repo 'source\licenses\inno-setup-zh-cn\LICENSE'
    Copy-Item -LiteralPath $chineseLicense -Destination (Join-Path $licenses.FullName 'inno-setup-zh-cn.txt')
    Copy-Item -LiteralPath (Join-Path $workspace 'crates\copiago-ddi\src\setup\idevice-LICENSE.txt'),
        (Join-Path $workspace 'crates\copiago-win\src\update\Unicode-LICENSE.txt') -Destination $licenses.FullName

    $isccArgs = @("/DPayloadDir=$($payload.FullName)", "/DPackageArch=$arch", "/O$($stage.FullName)")
    $setupName = "CopiaGo-Setup-$version-$arch.exe"
    if ($TestId) {
        $isccArgs += "/DTestId=$TestId", "/DTestDir=$TestInstallDir"
        $setupName = "CopiaGo-Test-$version-$arch-$TestId.exe"
    }
    if ($SignToolCommand) {
        $isccArgs += '/DSignRelease', "/SCopiaGoRelease=$SignToolCommand"
    }
    & $IsccPath @isccArgs (Join-Path $workspace 'installer\setup.iss')
    if ($LASTEXITCODE -ne 0) { throw "ISCC failed (exit $LASTEXITCODE)." }

    # ISCC signs the staged EXE when requested, so the ZIP uses that same copy.
    $stagedExe = Get-Item -LiteralPath (Join-Path $payload.FullName 'copiago.exe')
    $setup = Get-Item -LiteralPath (Join-Path $stage.FullName $setupName)
    $payloadBytes = (Get-ChildItem -LiteralPath $payload.FullName -File -Recurse |
        Measure-Object -Property Length -Sum).Sum
    if ($stagedExe.Length -gt 6000000) { throw "copiago.exe exceeds 6000000 bytes: $($stagedExe.Length)" }
    if ($setup.Length -gt 6000000) { throw "Installer exceeds 6000000 bytes: $($setup.Length)" }

    $packages = @($setup)
    if (!$TestId) {
        [IO.File]::WriteAllBytes((Join-Path $payload.FullName 'portable-data'), [byte[]]@())
        $zipPath = Join-Path $stage.FullName "CopiaGo-Portable-$version-$arch.zip"
        Compress-Archive -LiteralPath (Get-ChildItem -LiteralPath $payload.FullName).FullName -DestinationPath $zipPath -CompressionLevel Optimal
        $packages += Get-Item -LiteralPath $zipPath
    }

    Write-Host ("copiago.exe: {0} bytes" -f $stagedExe.Length)
    Write-Host ("Installer payload (before uninstall files): {0} bytes" -f $payloadBytes)
    foreach ($package in $packages) {
        $destination = Join-Path $outputPath $package.Name
        $length = $package.Length
        Move-Item -LiteralPath $package.FullName -Destination $destination -Force
        Write-Host ("{0}: {1} bytes" -f $destination, $length)
    }
} finally {
    $current = Get-Item -LiteralPath $stage.FullName -Force
    if ($current.FullName -ne $stagePath -or ($current.Attributes -band [IO.FileAttributes]::ReparsePoint)) {
        throw "Unexpected package staging directory: $($current.FullName)"
    }
    Remove-Item -LiteralPath $current.FullName -Recurse -Force
}
