param(
    [string]$Python = 'python',
    [string]$DistPath = 'dist',
    [string]$BuildPath = 'build'
)
$ErrorActionPreference = 'Stop'
$icon = Join-Path $PSScriptRoot 'iphone_clip\assets\jade.ico'
# pymobiledevice3 pulls in its interactive shell, tunnel server and media tooling; the app uses none of them.
$excludes = @(
    'IPython', 'jedi', 'parso', 'traitlets', 'matplotlib_inline', 'stack_data', 'asttokens', 'executing', 'pure_eval',
    'fastapi', 'starlette', 'uvicorn', 'av', 'opentelemetry', 'asgiwebdav', 'PIL._avif'
) | ForEach-Object { '--exclude-module', $_ }
& $Python -m PyInstaller --noconfirm --onedir --windowed --name iPhoneClipboard `
    --distpath $DistPath --workpath $BuildPath --specpath $BuildPath `
    --icon $icon --add-data "${icon}:iphone_clip/assets" `
    --collect-data pymobiledevice3 --recursive-copy-metadata pymobiledevice3 --collect-binaries pytun_pmd3 `
    --copy-metadata developer-disk-image --hidden-import pystray._win32 `
    --hidden-import pillow_heif --hidden-import win32timezone --hidden-import win32crypt @excludes main.py
if ($LASTEXITCODE -ne 0) { throw 'Build failed' }
# Tcl time zones and Tk message catalogs are unused: the app has no Tcl clock code and uses native dialogs.
$distRoot = (Resolve-Path -LiteralPath $DistPath).ProviderPath
$internal = Join-Path $distRoot 'iPhoneClipboard\_internal'
foreach ($folder in @('_tcl_data\tzdata', '_tcl_data\msgs', '_tk_data\msgs')) {
    $target = Join-Path $internal $folder
    if (Test-Path -LiteralPath $target -PathType Container) {
        $resolved = (Resolve-Path -LiteralPath $target).ProviderPath
        if ($resolved -ne $target -or (Get-Item -LiteralPath $resolved).Attributes -band [IO.FileAttributes]::ReparsePoint) {
            throw "Unexpected build output path: $resolved"
        }
        Remove-Item -LiteralPath $resolved -Recurse -Force
    }
}
