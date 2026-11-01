# Builds the exe that goes on a GitHub release into dist\.
#
# Rust bakes source paths into the binary (panic messages), and those would carry the builder's
# Windows user name, so they are remapped here and the result is checked before it is accepted.
# Builds in its own target folder, so a running dev copy is not touched.
#   powershell -File tools\release.ps1
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$dist = Join-Path $root 'dist'
$target = Join-Path $root 'target\dist'

$version = (Select-String -Path (Join-Path $root 'Cargo.toml') -Pattern '^version\s*=\s*"([^"]+)"' |
    Select-Object -First 1).Matches[0].Groups[1].Value
if (-not $version) { throw 'Could not read the version from Cargo.toml.' }

# Cargo needs the MSVC linker on PATH; Git's own link.exe would shadow it otherwise.
$vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
$vs = & $vswhere -all -products * -property installationPath | Select-Object -First 1
if (-not $vs) { throw 'Visual Studio Build Tools not found (needs the C++ workload and a Windows SDK).' }
$vcvars = Join-Path $vs 'VC\Auxiliary\Build\vcvars64.bat'
$env:PATH = "$(Split-Path -Parent $vswhere);$env:PATH"

# CARGO_ENCODED_RUSTFLAGS is split on 0x1f, so paths with spaces are safe.
$cargoHome = if ($env:CARGO_HOME) { $env:CARGO_HOME } else { Join-Path $env:USERPROFILE '.cargo' }
$rustupHome = if ($env:RUSTUP_HOME) { $env:RUSTUP_HOME } else { Join-Path $env:USERPROFILE '.rustup' }
$env:CARGO_ENCODED_RUSTFLAGS = (
    "--remap-path-prefix=$root=.",
    "--remap-path-prefix=$cargoHome=/cargo",
    "--remap-path-prefix=$rustupHome=/rustup"
) -join [char]0x1f
$env:CARGO_TARGET_DIR = $target

Push-Location $root
try {
    cmd /c "`"$vcvars`" >nul && cargo build --release --locked"
    if ($LASTEXITCODE -ne 0) { throw "cargo build failed ($LASTEXITCODE)" }
} finally {
    Pop-Location
}

$built = Join-Path $target 'release\claude-sync-tray.exe'
$bytes = [IO.File]::ReadAllBytes($built)
$text = [Text.Encoding]::GetEncoding(28591).GetString($bytes)
foreach ($leak in $env:USERNAME, $root) {
    if ($leak -and $text.IndexOf($leak, [StringComparison]::OrdinalIgnoreCase) -ge 0) {
        throw "The exe still contains '$leak'; not publishing it."
    }
}

New-Item -ItemType Directory -Force $dist | Out-Null
$name = "ClaudeSyncTray-v$version-windows-x64.exe"
$out = Join-Path $dist $name
Copy-Item $built $out -Force
$hash = (Get-FileHash $out -Algorithm SHA256).Hash.ToLower()
Set-Content -Path "$out.sha256" -Value "$hash  $name" -Encoding ascii
Write-Host "built $out"
Write-Host "sha256 $hash"
