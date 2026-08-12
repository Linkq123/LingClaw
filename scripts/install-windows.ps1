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
$MinimumRustVersion = [Version]'1.90.0'
$RustWindowsSetupUrl = 'https://learn.microsoft.com/windows/dev-environment/rust/setup'
$CargoBuildJobs = 2

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
    $pathParts = @($env:Path -split ';' | ForEach-Object { $_.Trim() } | Where-Object {
        $_ -and $_.TrimEnd('\') -ine $trimmedDir
    })
    $env:Path = (@($Dir) + $pathParts) -join ';'
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

function Get-ActiveRustVersion {
    if (-not (Test-Tool 'rustc')) {
        return $null
    }

    $previousErrorActionPreference = $ErrorActionPreference
    try {
        $ErrorActionPreference = 'Continue'
        $versionOutput = @(& rustc --version 2>$null)
    } finally {
        $ErrorActionPreference = $previousErrorActionPreference
    }

    foreach ($line in $versionOutput) {
        if ($line.ToString() -match '^rustc\s+(\d+\.\d+\.\d+)') {
            return [Version]$Matches[1]
        }
    }
    return $null
}

function Update-RustStableToolchain {
    Write-Info "Updating the Rust stable toolchain (LingClaw requires rustc >= $MinimumRustVersion)."
    Invoke-Step -Program 'rustup' -Arguments @('toolchain', 'install', 'stable', '--profile', 'minimal') -WorkingDirectory $RootDir -Label 'rustup toolchain install stable'
    Invoke-Step -Program 'rustup' -Arguments @('default', 'stable') -WorkingDirectory $RootDir -Label 'rustup default stable'
    # A directory override may still select an older compiler. Keep this installer on the
    # verified stable toolchain without mutating the repository's rustup override.
    $env:RUSTUP_TOOLCHAIN = 'stable'
    Add-ToSessionPath (Get-CargoBinDir)
}

function Assert-CompatibleRust {
    if ((-not (Test-Tool 'cargo')) -or (-not (Test-Tool 'rustc'))) {
        throw 'Rust installation did not finish correctly. Please check rustup output and retry.'
    }

    $activeVersion = Get-ActiveRustVersion
    if ($null -eq $activeVersion) {
        throw "Unable to determine the active rustc version. LingClaw requires rustc >= $MinimumRustVersion."
    }
    if ($activeVersion -lt $MinimumRustVersion) {
        throw "The active Rust compiler is $activeVersion, but LingClaw requires rustc >= $MinimumRustVersion. Run 'rustup update stable' and retry."
    }

    Write-Info "Compatible Rust environment ready: $(& rustc --version)"
}

function Ensure-Rust {
    Add-ToSessionPath (Get-CargoBinDir)
    if ((Test-Tool 'cargo') -and (Test-Tool 'rustc')) {
        $activeVersion = Get-ActiveRustVersion
        if (($null -ne $activeVersion) -and ($activeVersion -ge $MinimumRustVersion)) {
            Write-Info "Rust environment already installed: $(& rustc --version)"
            Write-Info 'No additional Rust environment installation is required.'
            return
        }

        if (-not (Test-Tool 'rustup')) {
            $reportedVersion = if ($null -eq $activeVersion) { 'unknown' } else { $activeVersion.ToString() }
            throw "The active Rust compiler is $reportedVersion, but LingClaw requires rustc >= $MinimumRustVersion. This Rust installation is not managed by rustup; update it manually or install rustup from https://rustup.rs, then retry."
        }

        Update-RustStableToolchain
        Assert-CompatibleRust
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
    if (Test-Tool 'rustup') {
        Update-RustStableToolchain
    }
    Assert-CompatibleRust
}

function Get-RustHostTriple {
    $previousErrorActionPreference = $ErrorActionPreference
    try {
        $ErrorActionPreference = 'Continue'
        $versionOutput = @(& rustc -vV 2>$null)
    } finally {
        $ErrorActionPreference = $previousErrorActionPreference
    }

    foreach ($line in $versionOutput) {
        $text = $line.ToString()
        if ($text -match '^host:\s*(.+)$') {
            return $Matches[1].Trim()
        }
    }
    return $null
}

function Test-RustNativeToolchain {
    param([ref]$FailureOutput)

    $probeDir = Join-Path ([System.IO.Path]::GetTempPath()) (
        'lingclaw-rust-probe-{0}-{1}' -f $PID, [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds()
    )
    $sourcePath = Join-Path $probeDir 'main.rs'
    $binaryPath = Join-Path $probeDir 'lingclaw-rust-probe.exe'
    New-Item -ItemType Directory -Path $probeDir -Force | Out-Null
    [System.IO.File]::WriteAllText($sourcePath, "fn main() {}`r`n", [System.Text.Encoding]::ASCII)

    $previousErrorActionPreference = $ErrorActionPreference
    $probeOutput = @()
    $probeExitCode = -1
    try {
        $ErrorActionPreference = 'Continue'
        $probeOutput = @(& rustc $sourcePath --crate-name lingclaw_rust_probe -o $binaryPath 2>&1)
        $probeExitCode = $LASTEXITCODE
    } catch {
        $probeOutput += $_.Exception.Message
    } finally {
        $ErrorActionPreference = $previousErrorActionPreference
    }

    $FailureOutput.Value = ($probeOutput | ForEach-Object { $_.ToString() }) -join [Environment]::NewLine
    $success = ($probeExitCode -eq 0) -and (Test-Path -LiteralPath $binaryPath)
    Remove-Item -LiteralPath $probeDir -Recurse -Force -ErrorAction SilentlyContinue
    return $success
}

function Install-MsvcBuildTools {
    Write-Info 'Installing Microsoft C++ Build Tools. This is required by the Rust MSVC toolchain and may take several minutes.'
    $override = '--wait --passive --norestart --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended'
    $previousErrorActionPreference = $ErrorActionPreference
    $installExitCode = -1
    try {
        $ErrorActionPreference = 'Continue'
        & winget install --source winget --id Microsoft.VisualStudio.2022.BuildTools -e --accept-package-agreements --accept-source-agreements --override $override
        $installExitCode = $LASTEXITCODE
    } finally {
        $ErrorActionPreference = $previousErrorActionPreference
    }

    if ($installExitCode -ne 0) {
        throw "Microsoft C++ Build Tools installation failed with exit code $installExitCode. Install the 'Desktop development with C++' workload manually, then open a new PowerShell window. See $RustWindowsSetupUrl"
    }
}

function Ensure-RustNativeToolchain {
    $probeFailure = ''
    if (Test-RustNativeToolchain ([ref]$probeFailure)) {
        Write-Info 'Rust native linker check passed.'
        return
    }

    $hostTriple = Get-RustHostTriple
    if ($probeFailure -match '(?i)access is denied|os error 5|拒绝访问') {
        throw "Rust could not run a native build probe because Windows denied access. Check antivirus or Controlled Folder Access, unblock the repository files, and retry from a user-writable directory. Probe output:`n$probeFailure"
    }

    if ($hostTriple -and $hostTriple.EndsWith('-pc-windows-msvc')) {
        Write-Warn "The Rust MSVC linker probe failed. Microsoft C++ Build Tools with the 'Desktop development with C++' workload is required."
        if (-not (Test-Tool 'winget')) {
            throw "winget is unavailable. Install Microsoft C++ Build Tools manually, select 'Desktop development with C++', then open a new PowerShell window and retry. See $RustWindowsSetupUrl`nProbe output:`n$probeFailure"
        }
        if (-not (Prompt-YesNo 'Install Microsoft C++ Build Tools now? This requires administrator approval and several GB of disk space.')) {
            throw "Microsoft C++ Build Tools is required. Install the 'Desktop development with C++' workload and retry. See $RustWindowsSetupUrl`nProbe output:`n$probeFailure"
        }

        Install-MsvcBuildTools
        $probeFailure = ''
        if (Test-RustNativeToolchain ([ref]$probeFailure)) {
            Write-Info 'Rust native linker check passed after installing Microsoft C++ Build Tools.'
            return
        }
        throw "Microsoft C++ Build Tools was installed, but the Rust linker probe still fails. Open a new PowerShell window and rerun the installer. If it still fails, repair the 'Desktop development with C++' workload. See $RustWindowsSetupUrl`nProbe output:`n$probeFailure"
    }

    throw "The Rust native toolchain is incomplete for host '$hostTriple'. Repair the active rustup toolchain and its native linker, then retry.`nProbe output:`n$probeFailure"
}

function Get-CargoBuildFailureGuidance {
    param([string]$BuildLog)

    if ($BuildLog -match '(?i)linker.+link\.exe.+not found|msvc targets depend on the msvc linker|program not found.+link\.exe') {
        return "Microsoft C++ Build Tools is missing or incomplete. Install the 'Desktop development with C++' workload, then open a new PowerShell window. See $RustWindowsSetupUrl"
    }
    if ($BuildLog -match '(?i)access is denied|os error 5|拒绝访问') {
        return 'Windows blocked a generated build executable. Check antivirus or Controlled Folder Access, unblock the repository, and retry from a user-writable local directory.'
    }
    if ($BuildLog -match '(?i)paging file is too small|os error 1455|not enough memory|insufficient memory|页面文件太小|内存不足') {
        return "The build ran out of memory or paging-file capacity. Close memory-heavy applications, enable a system-managed paging file, and retry. The installer already limits Cargo to $CargoBuildJobs parallel jobs."
    }
    if ($BuildLog -match '(?i)no space left|disk full|os error 112|磁盘空间不足') {
        return 'The build drive is out of free space. Free several GB on the repository, TEMP, and Cargo drives, then retry.'
    }
    if ($BuildLog -match '(?i)failed to download|failed to fetch|could not resolve host|certificate verify failed|connection reset') {
        return 'Cargo could not download dependencies. Check proxy, TLS certificate, and crates.io access, then retry.'
    }
    return "Inspect the first error in the full Cargo log below; the final 'could not compile' lines are only a summary."
}

function Invoke-CargoBuild {
    $logPath = Join-Path ([System.IO.Path]::GetTempPath()) (
        'lingclaw-cargo-build-{0}-{1}.log' -f $PID, [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds()
    )
    $stdoutPath = "$logPath.stdout"
    $stderrPath = "$logPath.stderr"
    $buildExitCode = -1
    $startError = $null

    try {
        $cargoCommand = Get-Command 'cargo' -ErrorAction Stop
        $cargoProgram = if ($cargoCommand.Source) { $cargoCommand.Source } else { $cargoCommand.Path }
        $process = Start-Process -FilePath $cargoProgram `
            -ArgumentList @('build', '--release', '--locked', '--jobs', $CargoBuildJobs.ToString()) `
            -WorkingDirectory $RootDir `
            -NoNewWindow `
            -Wait `
            -PassThru `
            -RedirectStandardOutput $stdoutPath `
            -RedirectStandardError $stderrPath
        $buildExitCode = $process.ExitCode
    } catch {
        $startError = $_.Exception.Message
    }

    $stdoutText = if (Test-Path -LiteralPath $stdoutPath) {
        Get-Content -LiteralPath $stdoutPath -Raw -ErrorAction SilentlyContinue
    } else { '' }
    $stderrText = if (Test-Path -LiteralPath $stderrPath) {
        Get-Content -LiteralPath $stderrPath -Raw -ErrorAction SilentlyContinue
    } else { '' }
    $buildLog = @($stdoutText, $stderrText, $startError) | Where-Object { -not [string]::IsNullOrWhiteSpace($_) }
    $buildLog = $buildLog -join [Environment]::NewLine
    [System.IO.File]::WriteAllText($logPath, $buildLog, [System.Text.Encoding]::UTF8)

    if (-not [string]::IsNullOrWhiteSpace($stdoutText)) {
        Write-Host $stdoutText.TrimEnd()
    }
    if (-not [string]::IsNullOrWhiteSpace($stderrText)) {
        Write-Host $stderrText.TrimEnd()
    }

    Remove-Item -LiteralPath $stdoutPath -Force -ErrorAction SilentlyContinue
    Remove-Item -LiteralPath $stderrPath -Force -ErrorAction SilentlyContinue

    if ($buildExitCode -eq 0) {
        Remove-Item -LiteralPath $logPath -Force -ErrorAction SilentlyContinue
        return
    }

    $guidance = Get-CargoBuildFailureGuidance -BuildLog $buildLog
    throw "cargo build failed with exit code $buildExitCode.`n$guidance`nFull Cargo log: $logPath"
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

function Test-InstalledServiceRunning {
    param([string]$InstalledExe)

    # Windows PowerShell 5.1 can promote redirected native stderr to a
    # terminating NativeCommandError when ErrorActionPreference is Stop.
    # A stopped service is an expected probe result, not an installer error.
    $previousErrorActionPreference = $ErrorActionPreference
    $healthExitCode = -1
    try {
        $ErrorActionPreference = 'Continue'
        & $InstalledExe health *> $null
        $healthExitCode = $LASTEXITCODE
    } finally {
        $ErrorActionPreference = $previousErrorActionPreference
    }

    return $healthExitCode -eq 0
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

    if (Test-InstalledServiceRunning -InstalledExe $installedExe) {
        $wasRunning = $true
        Write-Info 'Stopping existing LingClaw service before installing.'
        $previousErrorActionPreference = $ErrorActionPreference
        $stopExitCode = -1
        try {
            $ErrorActionPreference = 'Continue'
            $stopOutput = @(& $installedExe stop 2>&1)
            $stopExitCode = $LASTEXITCODE
        } finally {
            $ErrorActionPreference = $previousErrorActionPreference
        }
        foreach ($line in $stopOutput) {
            Write-Host $line
        }
        if ($stopExitCode -ne 0) {
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

    # Reuse the release artifacts built above. Without an explicit target dir,
    # `cargo install` compiles the whole crate again in a temporary directory.
    Invoke-Step -Program 'cargo' -Arguments @(
        'install',
        '--path',
        '.',
        '--force',
        '--locked',
        '--offline',
        '--target-dir',
        (Join-Path $RootDir 'target')
    ) -WorkingDirectory $RootDir -Label 'cargo install'

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
Ensure-RustNativeToolchain
Build-Frontend

Write-Info 'Building LingClaw release binary.'
$oldExe = Rename-TargetExeForBuild
try {
    Invoke-CargoBuild
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
