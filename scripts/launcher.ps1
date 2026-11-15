param(
    [switch]$DryRun,
    [switch]$Dev,
    [switch]$ForceRebuild,
    [int]$VitePort = 1420
)

# =====================================================================
#  BUBBLE CHAT PRO - HIDDEN LAUNCHER (STANDALONE RELEASE DEFAULT)
#  1. Launches pre-built Release binary (<1s, zero Vite / localhost:1420)
#  2. Hides CMD completely (no flashing console window)
#  3. Rebuilds with a compact status splash ONLY if sources changed
#  4. Releases startup mutex immediately after spawn (subsequent clicks work)
#  5. Uses 100% pure 7-bit ASCII so PowerShell 5.1 never misreads UTF-8
# =====================================================================

$ErrorActionPreference = 'Continue'
$Root = Split-Path -Parent $PSScriptRoot
Set-Location $Root

# Select writable log path (AppData\Local -> Temp -> workspace\logs)
$LogDir = $null
foreach ($candidate in @(
    (Join-Path $env:LOCALAPPDATA 'BubbleChatPro\logs'),
    (Join-Path $env:TEMP 'BubbleChatPro\logs'),
    (Join-Path $Root 'logs')
)) {
    if ([string]::IsNullOrWhiteSpace($candidate)) { continue }
    try {
        New-Item -ItemType Directory -Force -Path $candidate -ErrorAction Stop | Out-Null
        $LogDir = $candidate
        break
    } catch {}
}
$LogFile = if ($LogDir) { Join-Path $LogDir 'launcher.log' } else { $null }
if ($LogFile) {
    try {
        Set-Content -Path $LogFile -Value "=== Henry Chat Pro launcher $(Get-Date -Format 'yyyy-MM-dd HH:mm:ss') ===" -Encoding UTF8 -ErrorAction Stop
    } catch { $LogFile = $null }
}

function Log([string]$msg) {
    if (-not $LogFile) { return }
    try {
        Add-Content -Path $LogFile -Value ("[{0}] {1}" -f (Get-Date -Format 'HH:mm:ss'), $msg) -Encoding UTF8 -ErrorAction SilentlyContinue
    } catch {}
}

function From-Utf8B64([string]$b64) {
    return [System.Text.Encoding]::UTF8.GetString([System.Convert]::FromBase64String($b64))
}

Add-Type -AssemblyName System.Windows.Forms -ErrorAction SilentlyContinue
Add-Type -AssemblyName System.Drawing -ErrorAction SilentlyContinue

function Show-Error([string]$msg) {
    Log "LOI: $msg"
    if ($DryRun) { Write-Output "LOI: $msg"; return }
    try {
        $title = From-Utf8B64 'SGVucnkgQ2hhdCBQcm8gLSBLaMO0bmcga2jhu59pIMSR4buZbmcgxJHGsOG7o2M='
        $extraPrefix = From-Utf8B64 'CgpDaGkgdGnhur90IGxvZzog'
        $extra = if ($LogFile) { "$extraPrefix$LogFile" } else { '' }
        [System.Windows.Forms.MessageBox]::Show(
            "$msg$extra",
            $title,
            [System.Windows.Forms.MessageBoxButtons]::OK,
            [System.Windows.Forms.MessageBoxIcon]::Error) | Out-Null
    } catch {}
}

# Compact dark splash shown ONLY when a rebuild/npm-install is required
function New-SplashForm([string]$titleText, [string]$subText) {
    try {
        $form = New-Object System.Windows.Forms.Form
        $form.Text = 'Henry Chat Pro'
        $form.FormBorderStyle = 'None'
        $form.StartPosition = 'CenterScreen'
        $form.Size = New-Object System.Drawing.Size(440, 128)
        $form.BackColor = [System.Drawing.Color]::FromArgb(15, 23, 42)
        $form.TopMost = $true

        $panel = New-Object System.Windows.Forms.Panel
        $panel.Dock = 'Fill'
        $panel.Padding = New-Object System.Windows.Forms.Padding(18)
        $panel.BackColor = [System.Drawing.Color]::FromArgb(15, 23, 42)

        $lblTitle = New-Object System.Windows.Forms.Label
        $lblTitle.Text = $titleText
        $lblTitle.ForeColor = [System.Drawing.Color]::FromArgb(248, 250, 252)
        $lblTitle.Font = New-Object System.Drawing.Font('Segoe UI', 11, [System.Drawing.FontStyle]::Bold)
        $lblTitle.Location = New-Object System.Drawing.Point(20, 20)
        $lblTitle.Size = New-Object System.Drawing.Size(400, 28)

        $lblSub = New-Object System.Windows.Forms.Label
        $lblSub.Text = $subText
        $lblSub.ForeColor = [System.Drawing.Color]::FromArgb(148, 163, 184)
        $lblSub.Font = New-Object System.Drawing.Font('Segoe UI', 9.5)
        $lblSub.Location = New-Object System.Drawing.Point(20, 52)
        $lblSub.Size = New-Object System.Drawing.Size(400, 26)

        $bar = New-Object System.Windows.Forms.ProgressBar
        $bar.Style = 'Marquee'
        $bar.MarqueeAnimationSpeed = 28
        $bar.Location = New-Object System.Drawing.Point(20, 88)
        $bar.Size = New-Object System.Drawing.Size(400, 14)

        $panel.Controls.Add($lblTitle)
        $panel.Controls.Add($lblSub)
        $panel.Controls.Add($bar)
        $form.Controls.Add($panel)
        $form.Show()
        [System.Windows.Forms.Application]::DoEvents()
        return @{ Form = $form; Label = $lblSub }
    } catch {
        return $null
    }
}

function Invoke-ProcessWithSplash([string]$filePath, [string]$arguments, [string]$splashSubText, $splash) {
    if ($splash -and $splash.Label) {
        try {
            $splash.Label.Text = $splashSubText
            [System.Windows.Forms.Application]::DoEvents()
        } catch {}
    }
    $outLog = if ($LogDir) { Join-Path $LogDir 'build_out.log' } else { Join-Path $env:TEMP 'bch_build_out.log' }
    $errLog = if ($LogDir) { Join-Path $LogDir 'build_err.log' } else { Join-Path $env:TEMP 'bch_build_err.log' }

    $resolved = $filePath
    try {
        $cmdInfo = Get-Command $filePath -ErrorAction SilentlyContinue
        if ($cmdInfo -and $cmdInfo.Source) { $resolved = $cmdInfo.Source }
    } catch {}

    $psi = New-Object System.Diagnostics.ProcessStartInfo
    $psi.FileName = $resolved
    $psi.Arguments = $arguments
    $psi.WorkingDirectory = $Root
    $psi.UseShellExecute = $false
    $psi.CreateNoWindow = $true
    $psi.RedirectStandardOutput = $true
    $psi.RedirectStandardError = $true
    $psi.StandardOutputEncoding = [System.Text.Encoding]::UTF8
    $psi.StandardErrorEncoding = [System.Text.Encoding]::UTF8

    $proc = New-Object System.Diagnostics.Process
    $proc.StartInfo = $psi
    try {
        [void]$proc.Start()
    } catch {
        try { $proc.Dispose() } catch {}
        throw ("Khong the chay lenh '$resolved': " + $_.Exception.Message)
    }

    $outTask = $proc.StandardOutput.ReadToEndAsync()
    $errTask = $proc.StandardError.ReadToEndAsync()

    while (-not $proc.HasExited) {
        if ($splash) {
            try { [System.Windows.Forms.Application]::DoEvents() } catch {}
        }
        Start-Sleep -Milliseconds 60
    }
    $proc.WaitForExit()
    $exitCode = [int]$proc.ExitCode

    [void]$outTask.Wait(5000)
    [void]$errTask.Wait(5000)
    $outText = if ($outTask.IsCompleted) { $outTask.Result } else { '' }
    $errText = if ($errTask.IsCompleted) { $errTask.Result } else { '' }

    $utf8NoBom = New-Object System.Text.UTF8Encoding($false)
    try { [System.IO.File]::WriteAllText($outLog, $outText, $utf8NoBom) } catch {}
    if ($exitCode -ne 0 -and [string]::IsNullOrWhiteSpace($errText) -and -not [string]::IsNullOrWhiteSpace($outText)) {
        try { [System.IO.File]::WriteAllText($errLog, $outText, $utf8NoBom) } catch {}
    } else {
        try { [System.IO.File]::WriteAllText($errLog, $errText, $utf8NoBom) } catch {}
    }
    $proc.Dispose()

    return @{ ExitCode = $exitCode; ErrLog = $errLog }
}

# Ensure cargo / nodejs are on PATH when launched from Windows Explorer
$cargoBin = Join-Path $env:USERPROFILE '.cargo\bin'
if ((Test-Path $cargoBin) -and ($env:PATH -notlike "*$cargoBin*")) { $env:PATH = "$cargoBin;$env:PATH" }
foreach ($nodeCandidate in @((Join-Path $env:ProgramFiles 'nodejs'), (Join-Path ${env:ProgramFiles(x86)} 'nodejs'))) {
    if ($nodeCandidate -and (Test-Path $nodeCandidate) -and ($env:PATH -notlike "*$nodeCandidate*")) {
        $env:PATH = "$nodeCandidate;$env:PATH"
    }
}

# ---------- 1. Serialize concurrent launcher clicks during startup/build ----------
$script:mutex = New-Object System.Threading.Mutex($false, 'Local\BubbleChatPro_Launcher_Mutex')
$script:hasLock = $false
try { $script:hasLock = $script:mutex.WaitOne(0) } catch [System.Threading.AbandonedMutexException] { $script:hasLock = $true }
if (-not $script:hasLock) {
    Log 'Another launcher instance is currently spawning/building -> exiting.'
    exit 0
}

$ExeName = 'henry-chat-pro'
$ReleaseExe = Join-Path $Root "src-tauri\target\release\$ExeName.exe"
$DebugExe = Join-Path $Root "src-tauri\target\debug\$ExeName.exe"
$ExePath = if ($Dev) { $DebugExe } else { $ReleaseExe }

$script:splash = $null
$viteProc = $null

function Release-LauncherLock {
    if ($script:hasLock -and $script:mutex) {
        try { $script:mutex.ReleaseMutex() } catch {}
        $script:hasLock = $false
    }
    if ($script:mutex) {
        try { $script:mutex.Dispose() } catch {}
        $script:mutex = $null
    }
}

function Close-Splash {
    if ($script:splash -and $script:splash.Form) {
        try { $script:splash.Form.Close(); $script:splash.Form.Dispose() } catch {}
        $script:splash = $null
    }
}

function Test-PortOpen([int]$port) {
    foreach ($addr in @('127.0.0.1', '::1')) {
        $client = $null
        try {
            $af = if ($addr -eq '::1') { [System.Net.Sockets.AddressFamily]::InterNetworkV6 } else { [System.Net.Sockets.AddressFamily]::InterNetwork }
            $client = New-Object System.Net.Sockets.TcpClient($af)
            $iar = $client.BeginConnect($addr, $port, $null, $null)
            if ($iar.AsyncWaitHandle.WaitOne(600) -and $client.Connected) {
                $client.EndConnect($iar)
                return $true
            }
        } catch {
        } finally {
            if ($client) { $client.Close() }
        }
    }
    return $false
}

try {
    # ---------- 2. Stop previous app instances cleanly before launching ----------
    if (-not $DryRun) {
        Get-Process -Name $ExeName -ErrorAction SilentlyContinue | ForEach-Object {
            Log "Closing existing app instance PID=$($_.Id)"
            Stop-Process -Id $_.Id -Force -ErrorAction SilentlyContinue
            try { [void]$_.WaitForExit(1500) } catch {}
        }
        Start-Sleep -Milliseconds 350
    }

    # ---------- 3. Check if Release/Debug binary needs rebuilding ----------
    $needBuild = $ForceRebuild -or (-not (Test-Path $ExePath))
    if (-not $needBuild) {
        $exeTime = (Get-Item $ExePath).LastWriteTime.AddSeconds(2)
        $watch = @(
            (Join-Path $Root 'src'),
            (Join-Path $Root 'index.html'),
            (Join-Path $Root 'package.json'),
            (Join-Path $Root 'vite.config.ts'),
            (Join-Path $Root 'tsconfig.json'),
            (Join-Path $Root 'src-tauri\src'),
            (Join-Path $Root 'src-tauri\capabilities'),
            (Join-Path $Root 'src-tauri\Cargo.toml'),
            (Join-Path $Root 'src-tauri\Cargo.lock'),
            (Join-Path $Root 'src-tauri\build.rs'),
            (Join-Path $Root 'src-tauri\tauri.conf.json')
        )
        foreach ($w in $watch) {
            if (-not (Test-Path $w)) { continue }
            $item = Get-Item $w -ErrorAction SilentlyContinue
            if (-not $item) { continue }
            if ($item.PSIsContainer) {
                $newer = Get-ChildItem -Path $w -Recurse -File -ErrorAction SilentlyContinue |
                    Where-Object { $_.LastWriteTime -gt $exeTime } | Select-Object -First 1
                if ($newer) { $needBuild = $true; Log "Source modified: $($newer.FullName)"; break }
            } elseif ($item.LastWriteTime -gt $exeTime) {
                $needBuild = $true; Log "Source modified: $($item.FullName)"; break
            }
        }
    }

    if ($DryRun) {
        $modeLabel = if ($Dev) { "dev (Vite:$VitePort)" } else { 'standalone-release' }
        Write-Output "DRYRUN OK: mode=$modeLabel exe=$ExePath exists=$(Test-Path $ExePath) needBuild=$needBuild"
        Log "DryRun OK: mode=$modeLabel exe=$ExePath needBuild=$needBuild"
        Release-LauncherLock
        exit 0
    }

    # ---------- 4. Build only when missing or source changed ----------
    if ($needBuild) {
        $script:splash = New-SplashForm 'Henry Chat Pro' (From-Utf8B64 'xJBhbmcgY2h14bqpbiBi4buLIGJpw6puIGThu4tjaCBwaGnDqm4gYuG6o24gdOG7kWkgxrB1Li4u')
        $seeLog = From-Utf8B64 'KS4gWGVtIGxvZzog'

        if (-not (Test-Path (Join-Path $Root 'node_modules\vite\bin\vite.js'))) {
            Log 'Running npm.cmd install...'
            $res = Invoke-ProcessWithSplash 'cmd.exe' '/c npm.cmd install' (From-Utf8B64 'xJBhbmcgY8OgaSDEkeG6t3QgdGjGsCB2aeG7h24gZ2lhbyBkaeG7h24gbOG6p24gxJHhuqd1IChucG0gaW5zdGFsbCkuLi4=') $script:splash
            if ($res.ExitCode -ne 0) {
                throw ((From-Utf8B64 'bnBtIGluc3RhbGwgdGjhuqV0IGLhuqFpIChtw6Mg') + $res.ExitCode + $seeLog + $res.ErrLog)
            }
        }

        if ($Dev) {
            Log 'Running cargo build (debug)...'
            $res = Invoke-ProcessWithSplash 'cargo' 'build --manifest-path src-tauri\Cargo.toml' (From-Utf8B64 'xJBhbmcgYmnDqm4gZOG7i2NoIG5ow6JuIFJ1c3QgKERlYnVnKS4uLg==') $script:splash
            if ($res.ExitCode -ne 0) {
                throw ((From-Utf8B64 'QmnDqm4gZOG7i2NoIFJ1c3QgRGVidWcgdGjhuqV0IGLhuqFpIChtw6Mg') + $res.ExitCode + $seeLog + $res.ErrLog)
            }
        } else {
            Log 'Running npm.cmd run build...'
            $resFe = Invoke-ProcessWithSplash 'cmd.exe' '/c npm.cmd run build' (From-Utf8B64 'xJBhbmcgxJHDs25nIGfDs2kgZ2lhbyBkaeG7h24gUmVhY3QvVml0ZSB04buRaSDGsHUgdOG7kWMgxJHhu5kuLi4=') $script:splash
            if ($resFe.ExitCode -ne 0) {
                throw ((From-Utf8B64 'xJDDs25nIGfDs2kgZ2lhbyBkaeG7h24gdGjhuqV0IGLhuqFpIChtw6Mg') + $resFe.ExitCode + $seeLog + $resFe.ErrLog)
            }

            Log 'Running tauri release build (--no-bundle)...'
            $resBe = Invoke-ProcessWithSplash 'cargo' 'build --release --features tauri/custom-protocol --manifest-path src-tauri\Cargo.toml' (From-Utf8B64 'xJBhbmcgYmnDqm4gZOG7i2NoIOG7qW5nIGThu6VuZyBSZWxlYXNlIMSR4buZYyBs4bqtcCAoMS0yIHBow7p0KS4uLg==') $script:splash
            if ($resBe.ExitCode -ne 0) {
                throw ((From-Utf8B64 'QmnDqm4gZOG7i2NoIFJlbGVhc2UgdGjhuqV0IGLhuqFpIChtw6Mg') + $resBe.ExitCode + $seeLog + $resBe.ErrLog)
            }
        }
    } else {
        Log 'Release build is up to date -> instant launch.'
    }

    if (-not (Test-Path $ExePath)) {
        throw ((From-Utf8B64 'S2jDtG5nIHTDrG0gdGjhuqV5IGZpbGUgdGjhu7FjIHRoaTog') + $ExePath)
    }

    # ---------- 5. Start Vite ONLY when -Dev is explicitly requested ----------
    if ($Dev) {
        if (-not (Test-PortOpen $VitePort)) {
            Log "Starting Vite dev server on port $VitePort..."
            $viteOut = if ($LogDir) { Join-Path $LogDir 'vite.log' } else { Join-Path $env:TEMP 'bch_vite.log' }
            $viteErr = if ($LogDir) { Join-Path $LogDir 'vite_err.log' } else { Join-Path $env:TEMP 'bch_vite_err.log' }
            $viteArgs = @('node_modules\vite\bin\vite.js', '--port', "$VitePort")
            $viteProc = Start-Process -FilePath 'node' -ArgumentList $viteArgs -WorkingDirectory $Root `
                -WindowStyle Hidden -PassThru `
                -RedirectStandardOutput $viteOut -RedirectStandardError $viteErr
            for ($i = 0; $i -lt 50; $i++) {
                Start-Sleep -Milliseconds 300
                if ($viteProc.HasExited -or (Test-PortOpen $VitePort)) { break }
            }
            if (-not (Test-PortOpen $VitePort)) {
                throw ((From-Utf8B64 'Vml0ZSBEZXYgU2VydmVyIGtow7RuZyBt4bufIMSRxrDhu6NjIGPhu5VuZyA=') + $VitePort)
            }
        }
    }

    Close-Splash

    # ---------- 6. Launch Release binary and release Mutex immediately ----------
    Log "Launching app: $ExePath"
    $appPsi = New-Object System.Diagnostics.ProcessStartInfo
    $appPsi.FileName = $ExePath
    $appPsi.WorkingDirectory = $Root
    $appPsi.UseShellExecute = $false
    $app = [System.Diagnostics.Process]::Start($appPsi)
    Start-Sleep -Milliseconds 250
    if ($app -and $app.HasExited) {
        $appCode = $app.ExitCode
        if ($null -ne $appCode -and [int]$appCode -ne 0) {
            throw ((From-Utf8B64 '4buobmcgZOG7pW5nIHbhu6thIG3hu58gxJHDoyB04buxIHRob8OhdCB24bubaSBtw6Mg') + $appCode)
        }
    }

    Release-LauncherLock

    if ($Dev -and $viteProc) {
        $app.WaitForExit()
        if (-not $viteProc.HasExited) {
            Stop-Process -Id $viteProc.Id -Force -ErrorAction SilentlyContinue
        }
    }
} catch {
    Close-Splash
    Release-LauncherLock
    Show-Error $_.Exception.Message
    exit 1
} finally {
    Close-Splash
    Release-LauncherLock
    Log 'Launcher finished.'
}
