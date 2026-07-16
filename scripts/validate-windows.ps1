[CmdletBinding()]
param(
    [ValidateRange(1, 1000000)]
    [int]$Frames = 600,

    [string]$OutputRoot = "validation-artifacts",

    [switch]$SkipInteractive
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"

$RepositoryRoot = Split-Path -Parent $PSScriptRoot
Set-Location $RepositoryRoot

$Timestamp = Get-Date -Format "yyyyMMdd-HHmmss"
$ArtifactDirectory = Join-Path $OutputRoot "windows-vulkan-$Timestamp"
$ArchivePath = "$ArtifactDirectory.zip"
New-Item -ItemType Directory -Path $ArtifactDirectory -Force | Out-Null

function Invoke-NativeLogged {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Command,

        [Parameter(Mandatory = $true)]
        [AllowEmptyCollection()]
        [string[]]$Arguments,

        [Parameter(Mandatory = $true)]
        [string]$LogName
    )

    $LogPath = Join-Path $ArtifactDirectory $LogName
    Write-Host "> $Command $($Arguments -join ' ')"
    $PreviousErrorActionPreference = $ErrorActionPreference
    try {
        # Windows PowerShell 5.1 turns a native process's stderr into non-terminating ErrorRecords.
        # Cargo writes ordinary progress there, so judge native success by its exit code instead.
        $ErrorActionPreference = "Continue"
        & $Command @Arguments 2>&1 |
            ForEach-Object { $_.ToString() } |
            Tee-Object -FilePath $LogPath
        $ExitCode = $LASTEXITCODE
    }
    finally {
        $ErrorActionPreference = $PreviousErrorActionPreference
    }
    if ($ExitCode -ne 0) {
        throw "$Command exited with code $ExitCode; see $LogPath"
    }
}

function Read-Yes {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Prompt
    )

    $Answer = Read-Host $Prompt
    return $Answer -match "^(y|yes)$"
}

function Write-SystemReport {
    $ReportPath = Join-Path $ArtifactDirectory "system.txt"
    @(
        "captured_at=$((Get-Date).ToString('o'))"
        "computer_name=$env:COMPUTERNAME"
        "powershell=$($PSVersionTable.PSVersion)"
        "frames=$Frames"
    ) | Set-Content -Path $ReportPath -Encoding UTF8

    "`nOperating system:" | Add-Content -Path $ReportPath -Encoding UTF8
    Get-CimInstance Win32_OperatingSystem |
        Select-Object Caption, Version, BuildNumber, OSArchitecture |
        Format-List |
        Out-String |
        Add-Content -Path $ReportPath -Encoding UTF8

    "Video controllers:" | Add-Content -Path $ReportPath -Encoding UTF8
    Get-CimInstance Win32_VideoController |
        Select-Object Name, DriverVersion, AdapterRAM, VideoProcessor |
        Format-List |
        Out-String |
        Add-Content -Path $ReportPath -Encoding UTF8
}

$Failure = $null
try {
    Write-SystemReport
    Invoke-NativeLogged "git" @("rev-parse", "HEAD") "git-revision.log"
    Invoke-NativeLogged "git" @("status", "--short", "--branch") "git-status.log"
    Invoke-NativeLogged "rustc" @("--version", "--verbose") "rustc.log"

    # OBS registers a global implicit Vulkan capture layer whose older advertised API version can
    # produce loader warnings. Its manifest defines this opt-out, which keeps third-party capture
    # machinery out of validation without asking the loader to force-disable a layer (itself a
    # warning).
    $env:DISABLE_VULKAN_OBS_CAPTURE = "1"

    # Full output works with Vulkan SDK releases that predate `vulkaninfo --summary` and preserves
    # the device properties/features needed for later capability comparisons.
    Invoke-NativeLogged -Command "vulkaninfo" -Arguments @() -LogName "vulkaninfo.log"

    Invoke-NativeLogged "cargo" @(
        "test",
        "-p",
        "zinc-vulkan-win32-triangle"
    ) "cargo-test.log"
    Invoke-NativeLogged "cargo" @(
        "build",
        "-p",
        "zinc-vulkan-win32-triangle"
    ) "cargo-build.log"

    # Run the executable directly so runtime logs contain only Zinc, Vulkan validation, and loader
    # output. Because VK_LOADER_DEBUG enables only error/warning classes, any such text is a failure.
    $Probe = Join-Path $RepositoryRoot "target\debug\zinc-vulkan-win32-triangle.exe"
    $env:VK_LOADER_DEBUG = "error,warn"
    Invoke-NativeLogged $Probe @("--frames", $Frames.ToString()) "finite-run.log"

    $ManualLines = @()
    if (-not $SkipInteractive) {
        Write-Host ""
        Write-Host "Interactive lifecycle run 1:"
        Write-Host "  1. Resize continuously, including very small sizes."
        Write-Host "  2. Minimize for several seconds and restore."
        Write-Host "  3. Maximize and restore."
        Write-Host "  4. Move between displays if another display is available."
        Write-Host "  5. Close with the title-bar button."
        Read-Host "Press Enter to launch" | Out-Null
        Invoke-NativeLogged -Command $Probe -Arguments @() -LogName "interactive-titlebar.log"
        $TitleBarPassed = Read-Yes "Did the triangle remain stable and close cleanly? [yes/no]"
        $ManualLines += "titlebar_lifecycle_passed=$TitleBarPassed"

        Write-Host ""
        Write-Host "Interactive lifecycle run 2: close the window with Alt+F4."
        Read-Host "Press Enter to launch" | Out-Null
        Invoke-NativeLogged -Command $Probe -Arguments @() -LogName "interactive-alt-f4.log"
        $AltF4Passed = Read-Yes "Did Alt+F4 close the probe cleanly? [yes/no]"
        $ManualLines += "alt_f4_passed=$AltF4Passed"

        $DisplayNotes = Read-Host "Display-change notes (or 'not tested')"
        $ManualLines += "display_notes=$DisplayNotes"
        $ManualPath = Join-Path $ArtifactDirectory "manual-observations.txt"
        $ManualLines | Set-Content -Path $ManualPath -Encoding UTF8

        if (-not $TitleBarPassed -or -not $AltF4Passed) {
            throw "one or more interactive lifecycle checks failed"
        }
    }

    $RuntimeLogs = @(
        Join-Path $ArtifactDirectory "finite-run.log"
        Join-Path $ArtifactDirectory "interactive-titlebar.log"
        Join-Path $ArtifactDirectory "interactive-alt-f4.log"
    ) | Where-Object { Test-Path $_ }
    $ValidationMessages = Select-String -Path $RuntimeLogs -Pattern "(?i)\b(warning|error)\b"
    if ($null -ne $ValidationMessages) {
        $ValidationPath = Join-Path $ArtifactDirectory "validation-messages.txt"
        $ValidationMessages | Out-String | Set-Content -Path $ValidationPath -Encoding UTF8
        throw "Vulkan validation emitted warning/error messages"
    }
}
catch {
    $Failure = $_.Exception.Message
}
finally {
    $ResultPath = Join-Path $ArtifactDirectory "RESULT.txt"
    if ($null -eq $Failure) {
        @(
            "PASS"
            "captured_at=$((Get-Date).ToString('o'))"
            "frames=$Frames"
            "interactive=$(-not $SkipInteractive)"
        ) | Set-Content -Path $ResultPath -Encoding UTF8
    }
    else {
        @(
            "FAIL"
            "captured_at=$((Get-Date).ToString('o'))"
            "reason=$Failure"
        ) | Set-Content -Path $ResultPath -Encoding UTF8
    }

    if (Test-Path $ArchivePath) {
        Remove-Item $ArchivePath -Force
    }
    Compress-Archive -Path (Join-Path $ArtifactDirectory "*") -DestinationPath $ArchivePath
}

if ($null -ne $Failure) {
    Write-Error "$Failure`nEvidence: $ArchivePath"
    exit 1
}

Write-Host "Windows/Vulkan validation passed."
Write-Host "Evidence: $ArchivePath"
