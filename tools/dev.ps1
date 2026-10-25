# Rebuilds the release exe in place while the tray is running, then starts it.
#
# Windows will not let cargo overwrite a running exe, but it does allow renaming one, so the
# running copy is moved aside first. Usage:  powershell -File tools\dev.ps1 [-NoRun]
param([switch]$NoRun)

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$exe = Join-Path $root 'target\release\claude-sync-tray.exe'
$old = "$exe.old"

# Cargo needs the MSVC linker on PATH; Git's own link.exe would shadow it otherwise.
$vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
$vs = & $vswhere -all -products * -property installationPath | Select-Object -First 1
if (-not $vs) { throw 'Visual Studio Build Tools not found (needs the C++ workload and a Windows SDK).' }
$vcvars = Join-Path $vs 'VC\Auxiliary\Build\vcvars64.bat'
$env:PATH = "$(Split-Path -Parent $vswhere);$env:PATH"

if (Test-Path $old) { Remove-Item $old -Force -ErrorAction SilentlyContinue }
if (Test-Path $exe) { Rename-Item $exe $old -Force }

Push-Location $root
try {
    cmd /c "`"$vcvars`" >nul && cargo build --release"
    if ($LASTEXITCODE -ne 0) { throw "cargo build failed ($LASTEXITCODE)" }
} catch {
    # Put the previous build back so the tray's own exe path keeps working.
    if ((Test-Path $old) -and -not (Test-Path $exe)) { Rename-Item $old $exe -Force }
    throw
} finally {
    Pop-Location
}

if (-not $NoRun) {
    # The old tray holds the single-instance lock, so it has to go first.
    Get-Process claude-sync-tray -ErrorAction SilentlyContinue | Stop-Process -Force
    Start-Sleep -Milliseconds 500
    Start-Process $exe
}
