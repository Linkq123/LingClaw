#requires -Version 5.1

[CmdletBinding()]
param(
    [ValidateSet('Prompt', 'Install', 'InstallDaemon', 'SkipForNow')]
    [string]$Mode = 'Prompt'
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$RootDir = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$MinimumNodeVersion = [Version]'20.19.0'

function Write-Info {
    param([string]$Message)
    Write-Host "[LingClaw] $Message"
}

function Write-Warn {
    param([string]$Message)
    Write-Warning "[LingClaw] $Message"
}

function Test-Tool {
    param([string]$Name)
    return $null -ne (Get-Command $Name -ErrorAction SilentlyContinue)
}

function Prompt-YesNo {
    param([string]$Prompt)
    $answer = Read-Host "$Prompt [y/N]"
    return $answer -match '^(?i:y(?:es)?)$'
}

function Get-CargoHome {
    if ($env:CARGO_HOME) {
        return $env:CARGO_HOME
    }
    return (Join-Path $HOME '.cargo')
}

function Get-CargoBinDir {
    return (Join-Path (Get-CargoHome) 'bin')
}

function Add-ToSessionPath {
    param([string]$Dir)

    if (-not (Test-Path -LiteralPath $Dir)) {
        return
    }

    $trimmedDir = $Dir.TrimEnd('\')
    $pathParts = $env:Path -split ';' | ForEach-Object { $_.Trim() } | Where-Object { $_ }
    foreach ($part in $pathParts) {
        if ($part.TrimEnd('\') -ieq $trimmedDir) {
            return
        }
    }

    if ([string]::IsNullOrWhiteSpace($env:Path)) {
        $env:Path = $Dir
    } else {
        $env:Path = "$Dir;$($env:Path)"
    }
}

function Invoke-Step {
    param(
        [string]$Program,
        [string[]]$Arguments,
        [string]$WorkingDirectory,
        [string]$Label
    )

    Push-Location $WorkingDirectory
    try {
        & $Program @Arguments
        if ($LASTEXITCODE -ne 0) {
            throw "$Label failed with exit code $LASTEXITCODE."
        }
    } finally {
        Pop-Location
    }
}

function Copy-DirectoryContents {
    param(
        [string]$Source,
        [string]$Destination
    )

    if (-not (Test-Path -LiteralPath $Source)) {
        return
    }

    if (Test-Path -LiteralPath $Destination) {
        Remove-Item -LiteralPath $Destination -Recurse -Force
    }
    New-Item -ItemType Directory -Path $Destination -Force | Out-Null

    foreach ($entry in Get-ChildItem -LiteralPath $Source -Force) {
        Copy-Item -LiteralPath $entry.FullName -Destination (Join-Path $Destination $entry.Name) -Recurse -Force
    }
}

function Ensure-Rust {
    if ((Test-Tool 'cargo') -and (Test-Tool 'rustc')) {
        Write-Info "Rust environment already installed: $(& rustc --version)"
        Write-Info 'No additional Rust environment installation is required.'
        Add-ToSessionPath (Get-CargoBinDir)
        return
    }

    if (-not (Test-Tool 'winget')) {
        throw 'Rust environment not found and winget is unavailable. Install rustup from https://rustup.rs and re-run the installer.'
    }

    Write-Info 'Rust environment not found. Installing via winget.'
    & winget install --id Rustlang.Rustup -e --accept-package-agreements --accept-source-agreements
    if ($LASTEXITCODE -ne 0) {
        throw "winget install Rustlang.Rustup failed with exit code $LASTEXITCODE."
    }

    Add-ToSessionPath (Get-CargoBinDir)
    if ((-not (Test-Tool 'cargo')) -or (-not (Test-Tool 'rustc'))) {
        throw 'Rust installation did not finish correctly. Please check rustup output and retry.'
    }

    Write-Info "Rust environment installed: $(& rustc --version)"
}

function Get-NpmProgram {
    $candidates = @()
    if ($env:ProgramFiles) {
        $candidates += (Join-Path $env:ProgramFiles 'nodejs\npm.cmd')
    }
    if (${env:ProgramFiles(x86)}) {
        $candidates += (Join-Path ${env:ProgramFiles(x86)} 'nodejs\npm.cmd')
    }
    if ($env:LOCALAPPDATA) {
        $candidates += (Join-Path $env:LOCALAPPDATA 'Programs\nodejs\npm.cmd')
    }
    foreach ($name in 'npm.cmd', 'npm') {
        $command = Get-Command $name -ErrorAction SilentlyContinue
        if ($command) {
            $path = $command.Source
            if ([string]::IsNullOrWhiteSpace($path)) {
                $path = $command.Path
            }
            if (-not [string]::IsNullOrWhiteSpace($path)) {
                $candidates += $path
            }
        }
    }

    foreach ($candidate in $candidates) {
        if (-not [string]::IsNullOrWhiteSpace($candidate) -and (Test-Path -LiteralPath $candidate)) {
            return $candidate
        }
    }
    return $null
}

function Get-StaticIndexPath {
    return (Join-Path $RootDir 'static\index.html')
}

function Get-NodeExecutablePath {
    $candidates = @()
    if ($env:ProgramFiles) {
        $candidates += (Join-Path $env:ProgramFiles 'nodejs\node.exe')
    }
    if (${env:ProgramFiles(x86)}) {
        $candidates += (Join-Path ${env:ProgramFiles(x86)} 'nodejs\node.exe')
    }
    if ($env:LOCALAPPDATA) {
        $candidates += (Join-Path $env:LOCALAPPDATA 'Programs\nodejs\node.exe')
    }

    $command = Get-Command 'node' -ErrorAction SilentlyContinue
    if ($command) {
        $path = $command.Source
        if ([string]::IsNullOrWhiteSpace($path)) {
            $path = $command.Path
        }
        if (-not [string]::IsNullOrWhiteSpace($path)) {
            $candidates += $path
        }
    }

    foreach ($candidate in $candidates) {
        if (-not [string]::IsNullOrWhiteSpace($candidate) -and (Test-Path -LiteralPath $candidate)) {
            return $candidate
        }
    }
    return $null
}

function Get-NodeVersion {
    $nodeExecutable = Get-NodeExecutablePath
    if (-not $nodeExecutable) {
        return $null
    }

    try {
        $raw = (& $nodeExecutable --version).Trim()
        if ([string]::IsNullOrWhiteSpace($raw)) {
            return $null
        }
        return [Version]($raw.TrimStart('v'))
    } catch {
        return $null
    }
}

function Refresh-SessionPathFromRegistry {
    $segments = New-Object System.Collections.Generic.List[string]
    foreach ($scope in 'Machine', 'User') {
        $value = [Environment]::GetEnvironmentVariable('Path', $scope)
        if ([string]::IsNullOrWhiteSpace($value)) {
            continue
        }
        foreach ($segment in ($value -split ';')) {
            $trimmed = $segment.Trim()
            if ([string]::IsNullOrWhiteSpace($trimmed)) {
                continue
            }
            if (-not $segments.Contains($trimmed)) {
                $segments.Add($trimmed)
            }
        }
    }

    foreach ($segment in ($env:Path -split ';')) {
        $trimmed = $segment.Trim()
        if ([string]::IsNullOrWhiteSpace($trimmed)) {
            continue
        }
        if (-not $segments.Contains($trimmed)) {
            $segments.Add($trimmed)
        }
    }

    $env:Path = ($segments -join ';')
}

function Add-NodeInstallPaths {
    $candidates = @()
    if ($env:ProgramFiles) {
        $candidates += (Join-Path $env:ProgramFiles 'nodejs')
    }
    if (${env:ProgramFiles(x86)}) {
        $candidates += (Join-Path ${env:ProgramFiles(x86)} 'nodejs')
    }
    if ($env:LOCALAPPDATA) {
        $candidates += (Join-Path $env:LOCALAPPDATA 'Programs\nodejs')
    }

    foreach ($candidate in $candidates) {
        Add-ToSessionPath $candidate
    }
}

function Ensure-Node {
    $npmProgram = Get-NpmProgram
    $nodeVersion = Get-NodeVersion
    if ($nodeVersion -and $npmProgram -and $nodeVersion -ge $MinimumNodeVersion) {
        return $true
    }

    if ($nodeVersion -and $nodeVersion -lt $MinimumNodeVersion) {
        Write-Warn "Node.js $nodeVersion is below the required minimum $MinimumNodeVersion. Attempting automatic upgrade."
    }

    if (-not (Test-Tool 'winget')) {
        Write-Warn 'Node.js / npm not found and winget is unavailable. Falling back to the existing static bundle.'
        return $false
    }

    Write-Info 'Node.js / npm not found. Installing Node.js LTS via winget.'
    & winget install --source winget --id OpenJS.NodeJS.LTS -e --accept-package-agreements --accept-source-agreements
    if ($LASTEXITCODE -ne 0) {
        Write-Warn "winget install OpenJS.NodeJS.LTS failed with exit code $LASTEXITCODE. Falling back to the existing static bundle."
        return $false
    }

    Refresh-SessionPathFromRegistry
    Add-NodeInstallPaths

    $npmProgram = Get-NpmProgram
    $nodeVersion = Get-NodeVersion
    $nodeExecutable = Get-NodeExecutablePath
    if ($nodeVersion -and $npmProgram -and $nodeVersion -ge $MinimumNodeVersion -and $nodeExecutable) {
        Write-Info "Node.js environment installed: $(& $nodeExecutable --version)"
        return $true
    }

    Write-Warn "Node.js installation finished but the current shell still does not have a compatible Node.js runtime (need >= $MinimumNodeVersion). Falling back to the existing static bundle."
    return $false
}

function Build-Frontend {
    $nodeReady = Ensure-Node
    $npmProgram = Get-NpmProgram
    $nodeExecutable = Get-NodeExecutablePath
    if (($nodeReady -ne $true) -or (-not $npmProgram) -or (-not $nodeExecutable)) {
        $staticIndex = Get-StaticIndexPath
        if (Test-Path -LiteralPath $staticIndex) {
            Write-Warn "Using existing frontend bundle: $staticIndex"
            return
        }
        throw 'Node.js / npm could not be prepared and static/index.html is missing.'
    }

    $frontendDir = Join-Path $RootDir 'frontend'
    Write-Info "Building frontend assets (Node.js $(& $nodeExecutable --version), npm $(& $npmProgram --version))."
    Invoke-Step -Program $npmProgram -Arguments @('ci') -WorkingDirectory $frontendDir -Label 'frontend dependency install'
    Invoke-Step -Program $npmProgram -Arguments @('run', 'build') -WorkingDirectory $frontendDir -Label 'frontend build'
    Write-Info 'Frontend build complete: static/'
}

function Rename-TargetExeForBuild {
    $targetExe = Join-Path $RootDir 'target\release\lingclaw.exe'
    if (-not (Test-Path -LiteralPath $targetExe)) {
        return $null
    }

    $backupExe = '{0}.old.{1}.{2}' -f $targetExe, $PID, [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds()

    try {
        Move-Item -LiteralPath $targetExe -Destination $backupExe -Force
        return @{
            TargetExe = $targetExe
            BackupExe = $backupExe
        }
    } catch {
        Write-Warn "Could not move the existing release binary out of the way: $($_.Exception.Message)"
        return $null
    }
}

function Restore-TargetExe {
    param($RenameState)

    if (-not $RenameState) {
        return
    }
    if (-not (Test-Path -LiteralPath $RenameState.BackupExe)) {
        return
    }

    Move-Item -LiteralPath $RenameState.BackupExe -Destination $RenameState.TargetExe -Force
}

function Remove-StaleTargetExe {
    param($RenameState)

    if ($RenameState -and (Test-Path -LiteralPath $RenameState.BackupExe)) {
        Remove-Item -LiteralPath $RenameState.BackupExe -Force
    }
}

function Test-FileUnlocked {
    param([string]$Path)

    if (-not (Test-Path -LiteralPath $Path)) {
        return $true
    }

    $stream = $null
    try {
        $stream = [System.IO.File]::Open($Path, [System.IO.FileMode]::Open, [System.IO.FileAccess]::ReadWrite, [System.IO.FileShare]::None)
        return $true
    } catch {
        return $false
    } finally {
        if ($stream) {
            $stream.Dispose()
        }
    }
}

function Wait-ForFileUnlock {
    param(
        [string]$Path,
        [int]$Attempts = 20
    )

    for ($i = 0; $i -lt $Attempts; $i++) {
        if (Test-FileUnlocked $Path) {
            return $true
        }
        Start-Sleep -Milliseconds 500
    }
    return $false
}

function Get-InstalledBinaryPath {
    return (Join-Path (Get-CargoBinDir) 'lingclaw.exe')
}

function Stop-InstalledServiceIfNeeded {
    $installedExe = Get-InstalledBinaryPath
    $wasRunning = $false

    if (-not (Test-Path -LiteralPath $installedExe)) {
        return @{
            WasRunning = $false
            InstalledExe = $installedExe
        }
    }

    & $installedExe health *> $null
    if ($LASTEXITCODE -eq 0) {
        $wasRunning = $true
        Write-Info 'Stopping existing LingClaw service before installing.'
        & $installedExe stop
        if ($LASTEXITCODE -ne 0) {
            Write-Warn 'Stop command returned a non-zero exit code. The install may fail if the old binary is still locked.'
        }
    }

    if (-not (Wait-ForFileUnlock $installedExe)) {
        Write-Warn "Installed binary is still locked: $installedExe"
    }

    return @{
        WasRunning = $wasRunning
        InstalledExe = $installedExe
    }
}

function Restart-InstalledServiceIfNeeded {
    param($ServiceState)

    if (-not $ServiceState) {
        return
    }
    if (-not $ServiceState.WasRunning) {
        return
    }
    if (-not (Test-Path -LiteralPath $ServiceState.InstalledExe)) {
        return
    }

    Write-Info 'Restarting LingClaw service.'
    & $ServiceState.InstalledExe start
    if ($LASTEXITCODE -ne 0) {
        Write-Warn 'Failed to restart LingClaw automatically. Start it manually with `lingclaw start`.'
    }
}

function Install-Release {
    $serviceState = Stop-InstalledServiceIfNeeded
    $cargoBin = Get-CargoBinDir
    $configDir = Join-Path $HOME '.lingclaw'

    Invoke-Step -Program 'cargo' -Arguments @('install', '--path', '.', '--force') -WorkingDirectory $RootDir -Label 'cargo install'

    $staticSource = Join-Path $RootDir 'static'
    if (Test-Path -LiteralPath $staticSource) {
        Copy-DirectoryContents -Source $staticSource -Destination (Join-Path $cargoBin 'static')
        Write-Info "Installed frontend assets to $(Join-Path $cargoBin 'static')"
    } else {
        Write-Warn 'Static frontend assets directory not found; web UI may return 404.'
    }

    $skillsSource = Join-Path $RootDir 'docs\reference\skills'
    if (Test-Path -LiteralPath $skillsSource) {
        Copy-DirectoryContents -Source $skillsSource -Destination (Join-Path $configDir 'system-skills')
        Write-Info "Installed system skills to $(Join-Path $configDir 'system-skills')"
    }

    $agentsSource = Join-Path $RootDir 'docs\reference\agents'
    if (Test-Path -LiteralPath $agentsSource) {
        Copy-DirectoryContents -Source $agentsSource -Destination (Join-Path $configDir 'system-agents')
        Write-Info "Installed system agents to $(Join-Path $configDir 'system-agents')"
    }

    Add-ToSessionPath $cargoBin
    return $serviceState
}

function Post-Install-SelfCheck {
    $cargoBin = Get-CargoBinDir
    $lingclawBin = Join-Path $cargoBin 'lingclaw.exe'
    $staticIndex = Join-Path $cargoBin 'static\index.html'
    $failed = $false

    Write-Info 'Running post-install self-check.'

    if (Test-Path -LiteralPath $lingclawBin) {
        Write-Info "Binary check passed: $lingclawBin"
    } else {
        Write-Warn "Binary check failed: $lingclawBin is missing."
        $failed = $true
    }

    if (Test-Path -LiteralPath $staticIndex) {
        Write-Info "Frontend asset check passed: $staticIndex"
    } else {
        Write-Warn "Frontend asset check failed: $staticIndex is missing. Web UI may return 404."
        $failed = $true
    }

    if (-not $failed) {
        & $lingclawBin --version *> $null
        if ($LASTEXITCODE -eq 0) {
            Write-Info 'CLI self-check passed: lingclaw --version'
            Write-Info 'Install self-check passed.'
            return
        }
        Write-Warn 'CLI self-check failed: lingclaw --version returned a non-zero exit code.'
        $failed = $true
    }

    if ($failed) {
        throw 'Install self-check failed. Re-run the installer or manually verify ~/.cargo/bin and ~/.cargo/bin/static.'
    }
}

function Read-InstallChoice {
    while ($true) {
        Write-Host '1. Install'
        Write-Host '2. Install-daemon'
        Write-Host '3. Skip for now'
        $answer = Read-Host 'Select the next step [1-3]'
        switch ($answer) {
            '1' { return 'Install' }
            '2' { return 'InstallDaemon' }
            '3' { return 'SkipForNow' }
            default { Write-Warn 'Please choose a valid option.' }
        }
    }
}

Ensure-Rust
Build-Frontend

Write-Info 'Building LingClaw release binary.'
$oldExe = Rename-TargetExeForBuild
try {
    Invoke-Step -Program 'cargo' -Arguments @('build', '--release') -WorkingDirectory $RootDir -Label 'cargo build'
    Remove-StaleTargetExe $oldExe
} catch {
    Restore-TargetExe $oldExe
    throw
}
Write-Info 'Build complete: target\release\lingclaw.exe'

$selectedMode = if ($Mode -eq 'Prompt') { Read-InstallChoice } else { $Mode }
$serviceState = $null
$restartService = $false

try {
    switch ($selectedMode) {
        'Install' {
            Write-Info 'Installing LingClaw into the global cargo bin directory.'
            $serviceState = Install-Release
            Post-Install-SelfCheck

            if (Prompt-YesNo 'Add LingClaw to PATH for future shells?') {
                & (Get-InstalledBinaryPath) path-install
                if ($LASTEXITCODE -ne 0) {
                    throw 'PATH registration failed.'
                }
            }

            $restartService = $serviceState.WasRunning
        }
        'InstallDaemon' {
            Write-Info 'Installing LingClaw and launching the setup wizard.'
            $serviceState = Install-Release
            Post-Install-SelfCheck

            & (Get-InstalledBinaryPath) --install-daemon
            if ($LASTEXITCODE -ne 0) {
                $restartService = $serviceState.WasRunning
                throw 'Setup wizard launch failed.'
            }
        }
        'SkipForNow' {
            Write-Info 'Skipping cargo install. Release binary remains at target\release\lingclaw.exe.'
        }
        default {
            throw "Unknown install mode: $selectedMode"
        }
    }
} catch {
    if ($serviceState -and $serviceState.WasRunning) {
        Restart-InstalledServiceIfNeeded $serviceState
    }
    throw
}

if ($restartService) {
    Restart-InstalledServiceIfNeeded $serviceState
}
