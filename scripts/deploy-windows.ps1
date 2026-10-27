param(
    [switch]$NoLaunch
)

$ErrorActionPreference = 'Stop'
$repoRoot = Split-Path -Parent $PSScriptRoot
$logRoot = Join-Path $env:LOCALAPPDATA 'E-Assisant'
$logPath = Join-Path $logRoot 'deploy.log'

New-Item -ItemType Directory -Force -Path $logRoot | Out-Null

function Write-Step([string]$message) {
    $line = "[$(Get-Date -Format 'yyyy-MM-dd HH:mm:ss')] $message"
    Write-Host $line -ForegroundColor Cyan
    Add-Content -LiteralPath $logPath -Value $line -Encoding UTF8
}

function Invoke-Logged([string]$program, [string[]]$arguments) {
    # Windows PowerShell 5 can turn any native stderr line into a terminating
    # NativeCommandError when ErrorActionPreference is Stop. Tauri writes normal
    # progress messages to stderr, so collect them without treating them as failure.
    $previousPreference = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    & $program @arguments 2>&1 | ForEach-Object {
        $line = "$_"
        Write-Host $line
        Add-Content -LiteralPath $logPath -Value $line -Encoding UTF8
    }
    $exitCode = $LASTEXITCODE
    $ErrorActionPreference = $previousPreference
    if ($exitCode -ne 0) {
        throw "$program failed with exit code $exitCode"
    }
}

function Resolve-TargetDirectory {
    $cargoConfig = Join-Path $repoRoot '.cargo\config.toml'
    if (Test-Path -LiteralPath $cargoConfig) {
        $match = Select-String -LiteralPath $cargoConfig -Pattern '^\s*target-dir\s*=\s*"([^"]+)"' | Select-Object -First 1
        if ($match) {
            $configured = $match.Matches[0].Groups[1].Value.Replace('/', '\')
            if ([System.IO.Path]::IsPathRooted($configured)) { return $configured }
            return [System.IO.Path]::GetFullPath((Join-Path $repoRoot $configured))
        }
    }
    return Join-Path $repoRoot 'src-tauri\target'
}

function Set-AppShortcut([string]$shortcutPath, [string]$executable) {
    $parent = Split-Path -Parent $shortcutPath
    New-Item -ItemType Directory -Force -Path $parent | Out-Null
    $shell = New-Object -ComObject WScript.Shell
    $shortcut = $shell.CreateShortcut($shortcutPath)
    $shortcut.TargetPath = $executable
    $shortcut.WorkingDirectory = Split-Path -Parent $executable
    $shortcut.IconLocation = "$executable,0"
    $shortcut.Description = 'E-Assisant'
    $shortcut.Save()
}

function Remove-LegacyShortcut([string]$shortcutPath) {
    if (Test-Path -LiteralPath $shortcutPath -PathType Leaf) {
        Remove-Item -LiteralPath $shortcutPath -Force
        Write-Step "Removed legacy shortcut: $shortcutPath"
    }
}

try {
    Set-Location -LiteralPath $repoRoot
    Write-Step 'Starting E-Assisant deployment'

    $npm = (Get-Command npm.cmd -ErrorAction Stop).Source
    if (-not (Test-Path -LiteralPath (Join-Path $repoRoot 'node_modules'))) {
        Write-Step 'First run: installing frontend dependencies'
        Invoke-Logged $npm @('ci')
    }

    Write-Step 'Building frontend, Rust backend, and Windows installer'
    Invoke-Logged $npm @('run', 'tauri', 'build')

    $targetDirectory = Resolve-TargetDirectory
    $releaseExe = Join-Path $targetDirectory 'release\e-assisant.exe'
    if (-not (Test-Path -LiteralPath $releaseExe)) {
        throw "Build completed but executable was not found: $releaseExe"
    }

    $desktopShortcut = Join-Path ([Environment]::GetFolderPath('Desktop')) 'E-Assisant.lnk'
    $startMenuShortcut = Join-Path $env:APPDATA 'Microsoft\Windows\Start Menu\Programs\E-Assisant\E-Assisant.lnk'
    Set-AppShortcut $desktopShortcut $releaseExe
    Set-AppShortcut $startMenuShortcut $releaseExe

    # Update an existing taskbar pin. Windows does not allow apps to silently
    # create a new pin, so this only repairs a shortcut the user already pinned.
    $taskbarShortcut = Join-Path $env:APPDATA 'Microsoft\Internet Explorer\Quick Launch\User Pinned\TaskBar\E-Assisant.lnk'
    if (Test-Path -LiteralPath $taskbarShortcut) {
        Set-AppShortcut $taskbarShortcut $releaseExe
    }

    # The executable and user data migrate in place; only obsolete shortcuts
    # are removed so Windows no longer exposes the previous product name.
    Remove-LegacyShortcut (Join-Path ([Environment]::GetFolderPath('Desktop')) 'PC Guardian.lnk')
    Remove-LegacyShortcut (Join-Path $env:APPDATA 'Microsoft\Windows\Start Menu\Programs\PC Guardian\PC Guardian.lnk')
    Remove-LegacyShortcut (Join-Path $env:APPDATA 'Microsoft\Internet Explorer\Quick Launch\User Pinned\TaskBar\PC Guardian.lnk')

    Write-Step "Deployment complete: $releaseExe"
    Write-Step 'Desktop, Start menu, and existing taskbar shortcuts now use the bundled app'

    if (-not $NoLaunch) {
        Start-Process -FilePath $releaseExe -WorkingDirectory (Split-Path -Parent $releaseExe)
        Write-Step 'E-Assisant started'
    }
} catch {
    $message = "Deployment failed: $($_.Exception.Message)"
    Write-Host $message -ForegroundColor Red
    Add-Content -LiteralPath $logPath -Value $message -Encoding UTF8
    Write-Host "Deployment log: $logPath" -ForegroundColor Yellow
    exit 1
}
