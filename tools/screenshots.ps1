# Regenerates docs/screenshots/*.png from a throwaway demo environment.
#
# The app reads everything from %APPDATA%, %LOCALAPPDATA% and %USERPROFILE%, so the script points
# those at a fake tree seeded with invented accounts and sessions. Nothing from the real machine
# is read or shown. Build first (tools\dev.ps1 -NoRun), then:
#   powershell -File tools\screenshots.ps1
param(
    [string]$Exe = (Join-Path (Split-Path -Parent $PSScriptRoot) 'target\release\claude-sync-tray.exe'),
    [string]$Out = (Join-Path (Split-Path -Parent $PSScriptRoot) 'docs\screenshots'),
    [int]$Width = 1120,
    [int]$Height = 760
)

$ErrorActionPreference = 'Stop'
if (-not (Test-Path $Exe)) { throw "Build the app first: $Exe not found." }

Add-Type -AssemblyName System.Drawing
Add-Type -AssemblyName System.Windows.Forms
Add-Type @'
using System;
using System.Runtime.InteropServices;
public static class Win {
    [StructLayout(LayoutKind.Sequential)] public struct RECT { public int L, T, R, B; }
    [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
    [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h, int cmd);
    [DllImport("user32.dll")] public static extern bool SetWindowPos(IntPtr h, IntPtr after, int x, int y, int w, int cy, uint flags);
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
    [DllImport("dwmapi.dll")] public static extern int DwmGetWindowAttribute(IntPtr h, int attr, out RECT r, int size);
    [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
    [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
    [DllImport("user32.dll")] public static extern void mouse_event(uint flags, int dx, int dy, uint data, UIntPtr extra);
}
'@
[void][Win]::SetProcessDPIAware()

# ---------------------------------------------------------------------------------------------
# Demo data
# ---------------------------------------------------------------------------------------------
# A short, neutral root: the app prints these paths, and %TEMP% would show the Windows user name.
$demo = Join-Path $env:SystemDrive 'demo'
if (Test-Path $demo) { throw "$demo already exists; remove it first so nothing real is deleted." }
$roaming = Join-Path $demo 'AppData\Roaming'
$local = Join-Path $demo 'AppData\Local'
$home_ = Join-Path $demo 'Home'
$appRoot = Join-Path $roaming 'ClaudeSyncTray'
foreach ($dir in $roaming, $local, $home_, $appRoot) { New-Item -ItemType Directory -Force $dir | Out-Null }

function Write-Json($path, $value) {
    New-Item -ItemType Directory -Force (Split-Path -Parent $path) | Out-Null
    [IO.File]::WriteAllText($path, ($value | ConvertTo-Json -Depth 8), (New-Object Text.UTF8Encoding $false))
}

function Add-Bulk($path, [long]$bytes) {
    New-Item -ItemType Directory -Force (Split-Path -Parent $path) | Out-Null
    $stream = [IO.File]::Create($path)
    $stream.SetLength($bytes)
    $stream.Close()
}

$titles = @(
    'Refactor the billing module', 'Fix flaky CI on Windows', 'Add dark mode to settings', 'Write migration for orders table',
    'Explain this stack trace', 'Speed up the search index', 'Review the auth middleware', 'Port the CLI to async',
    'Draft release notes for 2.4', 'Clean up unused feature flags', 'Debug memory leak in worker', 'Add pagination to the API',
    'Rename config keys safely', 'Set up nightly backups', 'Tidy the onboarding copy', 'Profile the cold start'
)

function New-Account($name, $dir, $accountId, $orgId, $email, $sessionCount, $seed) {
    $profile = Join-Path $appRoot "profiles\$dir"
    Write-Json (Join-Path $profile 'config.json') @{ lastKnownAccountUuid = $accountId }
    Write-Json (Join-Path $profile 'claude_code_auth\oauth_account.json') @{
        oauthAccount = @{ accountUuid = $accountId; organizationUuid = $orgId; emailAddress = $email }
    }
    Write-Json (Join-Path $profile 'claude_code_auth\.credentials.json') @{ claudeAiOauth = @{ accessToken = 'demo' } }
    Write-Json (Join-Path $profile 'claude_desktop_config.json') @{
        mcpServers = @{ filesystem = @{ command = 'demo' }; github = @{ command = 'demo' }; notes = @{ command = 'demo' } }
    }
    New-Item -ItemType Directory -Force (Join-Path $profile 'Local Storage\leveldb') | Out-Null
    Add-Bulk (Join-Path $profile 'Local Storage\leveldb\000003.log') 2MB
    Add-Bulk (Join-Path $profile 'Cache\Cache_Data\data_1') 38MB
    Add-Bulk (Join-Path $profile 'Code Cache\js\index') 12MB
    Add-Bulk (Join-Path $profile 'vm_bundles\claudevm.bundle\rootfs.vhdx') 1700MB
    Add-Bulk (Join-Path $profile 'claude-code\2.1.0\claude.exe') 210MB

    $now = [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds()
    for ($i = 0; $i -lt $sessionCount; $i++) {
        $id = '{0}{1:D3}' -f $seed, $i
        $title = $titles[($i + $seed.Length) % $titles.Count]
        if ($i -ge $titles.Count) { $title = "$title ($([int]($i / $titles.Count) + 1))" }
        Write-Json (Join-Path $profile "claude-code-sessions\$accountId\$orgId\local_$id.json") @{
            sessionId = "local_$id"; title = $title; isArchived = ($i % 9 -eq 8)
            lastActivityAt = $now - ($i * 3600000L) - 600000L
        }
    }
    @{ name = $name; dir = "profiles\$dir" }
}

$personal = New-Account 'Personal' 'account_1' 'a1b2c3d4-0000-4000-8000-000000000001' 'f0000000-0000-4000-8000-000000000001' 'alex@example.com' 48 'p'
$work = New-Account 'Work' 'account_2' 'e5f6a7b8-0000-4000-8000-000000000002' 'f0000000-0000-4000-8000-000000000002' 'sam@example.com' 9 'w'

# Claude Desktop's data folder, linked to the first account like a real install.
New-Item -ItemType Junction -Path (Join-Path $roaming 'Claude') -Target (Join-Path $appRoot 'profiles\account_1') | Out-Null

Write-Json (Join-Path $appRoot 'config.json') @{
    version = 1; active = 0; profiles = @($personal, $work)
    share_mcp_config = $true; sync_claude_code = $true; share_sessions = $false
}

# ---------------------------------------------------------------------------------------------
# Drive the app
# ---------------------------------------------------------------------------------------------
$saved = @{ APPDATA = $env:APPDATA; LOCALAPPDATA = $env:LOCALAPPDATA; USERPROFILE = $env:USERPROFILE }
$env:APPDATA = $roaming; $env:LOCALAPPDATA = $local; $env:USERPROFILE = $home_
$app = Start-Process $Exe -PassThru
foreach ($name in $saved.Keys) { Set-Item "env:$name" $saved[$name] }

try {
    $handle = [IntPtr]::Zero
    for ($i = 0; $i -lt 60 -and $handle -eq [IntPtr]::Zero; $i++) {
        Start-Sleep -Milliseconds 250
        $app.Refresh()
        $handle = $app.MainWindowHandle
    }
    if ($handle -eq [IntPtr]::Zero) { throw 'The app window never appeared.' }

    [void][Win]::ShowWindow($handle, 9)
    # Topmost, so nothing else on the desktop can end up in the capture.
    [void][Win]::SetWindowPos($handle, [IntPtr](-1), 80, 40, $Width, $Height, 0x0040)
    Start-Sleep -Milliseconds 800

    function Focus {
        [void][Win]::SetForegroundWindow($handle)
        Start-Sleep -Milliseconds 300
        if ([Win]::GetForegroundWindow() -ne $handle) { Write-Warning 'The app window is not in the foreground; clicks may miss.' }
    }

    # The visible frame, without the invisible resize border Windows adds around it.
    function Frame {
        $rect = New-Object Win+RECT
        [void][Win]::DwmGetWindowAttribute($handle, 9, [ref]$rect, [Runtime.InteropServices.Marshal]::SizeOf($rect))
        $rect
    }

    # Coordinates are measured from the top-left corner of the frame.
    function Click($x, $y) {
        $rect = Frame
        [void][Win]::SetCursorPos($rect.L + $x, $rect.T + $y)
        Start-Sleep -Milliseconds 120
        [Win]::mouse_event(0x02, 0, 0, 0, [UIntPtr]::Zero)
        [Win]::mouse_event(0x04, 0, 0, 0, [UIntPtr]::Zero)
        # Park the pointer where it cannot paint a hover state over the shot.
        [void][Win]::SetCursorPos($rect.R + 40, $rect.B + 40)
        Start-Sleep -Milliseconds 700
    }

    function Shot($file) {
        $rect = Frame
        $w = $rect.R - $rect.L; $h = $rect.B - $rect.T
        $bitmap = New-Object Drawing.Bitmap $w, $h
        $graphics = [Drawing.Graphics]::FromImage($bitmap)
        $graphics.CopyFromScreen($rect.L, $rect.T, 0, 0, (New-Object Drawing.Size $w, $h))
        $graphics.Dispose()
        New-Item -ItemType Directory -Force $Out | Out-Null
        $bitmap.Save((Join-Path $Out $file), [Drawing.Imaging.ImageFormat]::Png)
        $bitmap.Dispose()
        Write-Host "saved $file"
    }

    $navAccounts = @(70, 74); $navSettings = @(70, 110); $navStorage = @(70, 146)
    Focus

    Click $navSettings[0] $navSettings[1]
    Shot 'settings.png'

    Click $navStorage[0] $navStorage[1]
    Start-Sleep -Milliseconds 1500
    Shot 'storage.png'

    Click $navAccounts[0] $navAccounts[1]
    Shot 'accounts.png'

    # The shared-history switch: turning it on shows what will change before anything is touched.
    Click 1038 322
    Shot 'history-preview.png'
} finally {
    if (-not $app.HasExited) { Stop-Process -Id $app.Id -Force }
    Start-Sleep -Milliseconds 300
    # Drop the junction first so the recursive delete never follows it.
    cmd /c rmdir "`"$(Join-Path $roaming 'Claude')`"" 2>$null
    Remove-Item $demo -Recurse -Force -ErrorAction SilentlyContinue
}
