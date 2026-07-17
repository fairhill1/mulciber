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

function Invoke-NativeExpectedFailure {
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
    Write-Host "> $Command $($Arguments -join ' ') (expected failure)"
    $PreviousErrorActionPreference = $ErrorActionPreference
    try {
        $ErrorActionPreference = "Continue"
        & $Command @Arguments 2>&1 |
            ForEach-Object { $_.ToString() } |
            Tee-Object -FilePath $LogPath
        $ExitCode = $LASTEXITCODE
    }
    finally {
        $ErrorActionPreference = $PreviousErrorActionPreference
    }
    if ($ExitCode -eq 0) {
        throw "$Command unexpectedly succeeded; see $LogPath"
    }
    if (Select-String -Path $LogPath -Pattern "Vulkan validation") {
        throw "$Command emitted a validation message during expected failure; see $LogPath"
    }
}

function Read-Yes {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Prompt
    )

    while ($true) {
        $Answer = Read-Host $Prompt
        if ($Answer -match "^(?i:y|yes)$") {
            return $true
        }
        if ($Answer -match "^(?i:n|no)$") {
            return $false
        }
        Write-Host "Please answer only 'yes' or 'no'; descriptive details belong in the notes prompt." -ForegroundColor Yellow
    }
}

function Invoke-AutomatedResizeSmoke {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Probe,

        [string[]]$ProbeArguments = @(),

        [string]$LogPrefix = "resize-smoke",

        [ValidateRange(1, 100)]
        [int]$ResizeCycles = 1,

        [ValidateRange(1, 5000)]
        [int]$ResizeDelayMilliseconds = 450
    )

    if (-not ("MulciberValidationWin32" -as [type])) {
        Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;

public static class MulciberValidationWin32
{
    [DllImport("user32.dll")]
    public static extern bool SetWindowPos(
        IntPtr window, IntPtr insertAfter, int x, int y, int width, int height, uint flags);

    [DllImport("user32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    public static extern IntPtr SendMessageTimeout(
        IntPtr window, uint message, IntPtr wParam, IntPtr lParam,
        uint flags, uint timeoutMilliseconds, out IntPtr result);

    [DllImport("user32.dll")]
    public static extern bool PostMessage(IntPtr window, uint message, IntPtr wParam, IntPtr lParam);
}
'@
    }

    $StandardOutput = Join-Path $ArtifactDirectory "$LogPrefix.log"
    $StandardError = Join-Path $ArtifactDirectory "$LogPrefix.stderr.log"
    Write-Host "> $Probe $($ProbeArguments -join ' ') (automated resize smoke)"
    $StartInfo = New-Object System.Diagnostics.ProcessStartInfo
    $StartInfo.FileName = $Probe
    $StartInfo.Arguments = (($ProbeArguments | ForEach-Object {
                '"' + $_.Replace('"', '\"') + '"'
            }) -join ' ')
    $StartInfo.UseShellExecute = $false
    $StartInfo.RedirectStandardOutput = $true
    $StartInfo.RedirectStandardError = $true
    $Process = New-Object System.Diagnostics.Process
    $Process.StartInfo = $StartInfo
    if (-not $Process.Start()) {
        throw "automated resize smoke could not start the Vulkan probe"
    }
    $StandardOutputTask = $Process.StandardOutput.ReadToEndAsync()
    $StandardErrorTask = $Process.StandardError.ReadToEndAsync()
    try {
        $Window = [IntPtr]::Zero
        for ($Attempt = 0; $Attempt -lt 50 -and $Window -eq [IntPtr]::Zero; $Attempt++) {
            Start-Sleep -Milliseconds 100
            $Process.Refresh()
            if ($Process.HasExited) {
                break
            }
            $Window = $Process.MainWindowHandle
        }
        if ($Window -eq [IntPtr]::Zero) {
            throw "automated resize smoke could not find the Vulkan window"
        }

        for ($Cycle = 0; $Cycle -lt $ResizeCycles; $Cycle++) {
            foreach ($Size in @(@(640, 360), @(1200, 700), @(320, 240), @(960, 540))) {
                $Resized = [MulciberValidationWin32]::SetWindowPos(
                    $Window, [IntPtr]::Zero, 80, 80, $Size[0], $Size[1], 0x4014)
                if (-not $Resized) {
                    throw "SetWindowPos failed during automated resize smoke"
                }
                Start-Sleep -Milliseconds $ResizeDelayMilliseconds
                $MessageResult = [IntPtr]::Zero
                $Responsive = [MulciberValidationWin32]::SendMessageTimeout(
                    $Window, 0x0000, [IntPtr]::Zero, [IntPtr]::Zero,
                    0x0023, 5000, [ref]$MessageResult)
                if ($Responsive -eq [IntPtr]::Zero) {
                    throw "Vulkan probe stopped responding during automated resize smoke"
                }
            }
        }
        if (-not [MulciberValidationWin32]::PostMessage(
            $Window, 0x0010, [IntPtr]::Zero, [IntPtr]::Zero)) {
            throw "posting WM_CLOSE failed during automated resize smoke"
        }
        if (-not $Process.WaitForExit(10000)) {
            throw "Vulkan probe did not exit after automated resize smoke"
        }
        $Process.WaitForExit()
        if ($Process.ExitCode -ne 0) {
            throw "automated resize smoke exited with code $($Process.ExitCode)"
        }
    }
    finally {
        if (-not $Process.HasExited) {
            $Process.Kill()
            $Process.WaitForExit()
        }
        $StandardOutputTask.Wait()
        $StandardErrorTask.Wait()
        [System.IO.File]::WriteAllText($StandardOutput, $StandardOutputTask.Result)
        [System.IO.File]::WriteAllText($StandardError, $StandardErrorTask.Result)
        $StandardOutputTask.Result | Write-Host
        $StandardErrorTask.Result | Write-Host
        $Process.Dispose()
    }
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
        "mulciber-vulkan-info"
    ) "cargo-test-vulkan-info.log"
    Invoke-NativeLogged "cargo" @(
        "build",
        "-p",
        "mulciber-vulkan-info"
    ) "cargo-build-vulkan-info.log"
    $CapabilityProbe = Join-Path $RepositoryRoot "target\debug\mulciber-vulkan-info.exe"
    $env:VK_LOADER_DEBUG = "error,warn"
    Invoke-NativeLogged $CapabilityProbe @("--json") "mulciber-vulkan-info.json"
    $CapabilityReportPath = Join-Path $ArtifactDirectory "mulciber-vulkan-info.json"
    Get-Content -Path $CapabilityReportPath -Raw | ConvertFrom-Json | Out-Null

    Invoke-NativeLogged "cargo" @(
        "test",
        "-p",
        "mulciber-vulkan-triangle"
    ) "cargo-test.log"
    Invoke-NativeLogged "cargo" @(
        "build",
        "-p",
        "mulciber-vulkan-triangle"
    ) "cargo-build.log"

    Invoke-NativeLogged "cargo" @(
        "test",
        "-p",
        "mulciber-clear"
    ) "cargo-test-clear.log"
    Invoke-NativeLogged "cargo" @(
        "build",
        "-p",
        "mulciber-clear"
    ) "cargo-build-clear.log"
    $ClearExample = Join-Path $RepositoryRoot "target\debug\mulciber-clear.exe"
    Invoke-AutomatedResizeSmoke `
        -Probe $ClearExample `
        -LogPrefix "clear-resize"

    Invoke-NativeLogged "cargo" @(
        "test",
        "-p",
        "mulciber-api-clear"
    ) "cargo-test-api-clear.log"
    Invoke-NativeLogged "cargo" @(
        "build",
        "-p",
        "mulciber-api-clear"
    ) "cargo-build-api-clear.log"
    $ClearProbe = Join-Path $RepositoryRoot "target\debug\mulciber-api-clear.exe"
    $ClearFrames = [Math]::Min($Frames, 120)
    Invoke-NativeLogged $ClearProbe @(
        "--frames", $ClearFrames.ToString(),
        "--abandon-acquired-frame-once"
    ) "clear-abandon-recovery.log"

    Invoke-NativeLogged "cargo" @(
        "test",
        "-p",
        "mulciber-cube"
    ) "cargo-test-cube.log"
    Invoke-NativeLogged "cargo" @(
        "build",
        "-p",
        "mulciber-cube"
    ) "cargo-build-cube.log"
    $CubeExample = Join-Path $RepositoryRoot "target\debug\mulciber-cube.exe"
    Invoke-AutomatedResizeSmoke `
        -Probe $CubeExample `
        -LogPrefix "cube-resize"

    Invoke-NativeLogged "cargo" @(
        "test",
        "-p",
        "mulciber-postprocess-cube"
    ) "cargo-test-postprocess-cube.log"
    Invoke-NativeLogged "cargo" @(
        "build",
        "-p",
        "mulciber-postprocess-cube"
    ) "cargo-build-postprocess-cube.log"
    $PostprocessExample = Join-Path $RepositoryRoot "target\debug\mulciber-postprocess-cube.exe"
    Invoke-AutomatedResizeSmoke `
        -Probe $PostprocessExample `
        -LogPrefix "postprocess-cube-resize" `
        -ResizeCycles 25 `
        -ResizeDelayMilliseconds 10

    Invoke-NativeLogged "cargo" @(
        "test",
        "-p",
        "mulciber-api-cube"
    ) "cargo-test-api-cube.log"
    Invoke-NativeLogged "cargo" @(
        "build",
        "-p",
        "mulciber-api-cube"
    ) "cargo-build-api-cube.log"
    $CubeProbe = Join-Path $RepositoryRoot "target\debug\mulciber-api-cube.exe"
    $CubeFrames = [Math]::Min($Frames, 120)
    Invoke-NativeLogged $CubeProbe @(
        "--frames", $CubeFrames.ToString(),
        "--abandon-acquired-frame-once"
    ) "cube-4x-abandon-recovery.log"
    Invoke-NativeLogged $CubeProbe @(
        "--frames", $CubeFrames.ToString(),
        "--force-one-sample"
    ) "cube-1x.log"

    # Run the executable directly so runtime logs contain only Mulciber, Vulkan validation, and loader
    # output. Because VK_LOADER_DEBUG enables only error/warning classes, any such text is a failure.
    $Probe = Join-Path $RepositoryRoot "target\debug\mulciber-vulkan-triangle.exe"
    $PipelineCachePath = Join-Path $ArtifactDirectory "pipeline-cache.bin"
    $CacheFrames = [Math]::Min($Frames, 120)
    $env:VK_LOADER_DEBUG = "error,warn"
    Invoke-NativeLogged $Probe @(
        "--frames", $Frames.ToString(),
        "--pipeline-cache", $PipelineCachePath,
        "--rebuild-pipeline-cache"
    ) "finite-run.log"
    $env:MULCIBER_VULKAN_FORCE_MSAA_1X = "1"
    try {
        Invoke-NativeLogged $Probe @(
            "--frames", $Frames.ToString(),
            "--pipeline-cache", $PipelineCachePath
        ) "msaa-1x-fallback.log"
    }
    finally {
        Remove-Item Env:MULCIBER_VULKAN_FORCE_MSAA_1X -ErrorAction SilentlyContinue
    }
    $PipelineCacheHash = (Get-FileHash -Algorithm SHA256 $PipelineCachePath).Hash
    $AbandonmentFrames = [Math]::Min($Frames, 120)
    Invoke-NativeLogged $Probe @(
        "--frames", $AbandonmentFrames.ToString(),
        "--abandon-acquired-frame-once",
        "--pipeline-cache", $PipelineCachePath,
        "--require-pipeline-cache-hits"
    ) "abandon-acquired-frame.log"
    $env:MULCIBER_VULKAN_FORCE_SWAPCHAIN_FALLBACK = "1"
    try {
        Invoke-NativeLogged $Probe @(
            "--frames", $AbandonmentFrames.ToString(),
            "--abandon-acquired-frame-once",
            "--pipeline-cache", $PipelineCachePath,
            "--require-pipeline-cache-hits"
        ) "abandon-acquired-frame-fallback.log"
    }
    finally {
        Remove-Item Env:MULCIBER_VULKAN_FORCE_SWAPCHAIN_FALLBACK -ErrorAction SilentlyContinue
    }
    $MissingCachePath = Join-Path $ArtifactDirectory "pipeline-cache-missing.bin"
    Invoke-NativeExpectedFailure $Probe @(
        "--frames", "1",
        "--pipeline-cache", $MissingCachePath,
        "--require-pipeline-cache-hits"
    ) "pipeline-cache-strict-missing.log"
    Invoke-NativeLogged $Probe @(
        "--frames", $CacheFrames.ToString(),
        "--pipeline-cache", $PipelineCachePath,
        "--require-pipeline-cache-hits"
    ) "pipeline-cache-strict-4x.log"
    $env:MULCIBER_VULKAN_FORCE_MSAA_1X = "1"
    try {
        Invoke-NativeLogged $Probe @(
            "--frames", $CacheFrames.ToString(),
            "--pipeline-cache", $PipelineCachePath,
            "--require-pipeline-cache-hits"
        ) "pipeline-cache-strict-1x.log"
    }
    finally {
        Remove-Item Env:MULCIBER_VULKAN_FORCE_MSAA_1X -ErrorAction SilentlyContinue
    }
    Invoke-AutomatedResizeSmoke `
        -Probe $Probe `
        -ProbeArguments @(
            "--pipeline-cache", $PipelineCachePath,
            "--require-pipeline-cache-hits"
        ) `
        -LogPrefix "pipeline-cache-strict-resize"

    $PreviousTextureMode = $env:MULCIBER_VULKAN_TEXTURE_MODE
    $env:MULCIBER_VULKAN_TEXTURE_MODE = "rgba8"
    try {
        Invoke-NativeLogged $Probe @(
            "--frames", $Frames.ToString(),
            "--pipeline-cache", $PipelineCachePath,
            "--require-pipeline-cache-hits"
        ) "texture-rgba8-fallback-4x.log"
        $env:MULCIBER_VULKAN_FORCE_MSAA_1X = "1"
        try {
            Invoke-NativeLogged $Probe @(
                "--frames", $Frames.ToString(),
                "--pipeline-cache", $PipelineCachePath,
                "--require-pipeline-cache-hits"
            ) "texture-rgba8-fallback-1x.log"
        }
        finally {
            Remove-Item Env:MULCIBER_VULKAN_FORCE_MSAA_1X -ErrorAction SilentlyContinue
        }
        Invoke-AutomatedResizeSmoke `
            -Probe $Probe `
            -ProbeArguments @(
                "--pipeline-cache", $PipelineCachePath,
                "--require-pipeline-cache-hits"
            ) `
            -LogPrefix "texture-rgba8-fallback-resize"
    }
    finally {
        if ($null -eq $PreviousTextureMode) {
            Remove-Item Env:MULCIBER_VULKAN_TEXTURE_MODE -ErrorAction SilentlyContinue
        }
        else {
            $env:MULCIBER_VULKAN_TEXTURE_MODE = $PreviousTextureMode
        }
    }
    if ((Get-FileHash -Algorithm SHA256 $PipelineCachePath).Hash -ne $PipelineCacheHash) {
        throw "strict pipeline cache runs modified the read-only artifact"
    }

    [byte[]]$PipelineCacheBytes = [System.IO.File]::ReadAllBytes($PipelineCachePath)
    $TruncatedCachePath = Join-Path $ArtifactDirectory "pipeline-cache-truncated.bin"
    [System.IO.File]::WriteAllBytes($TruncatedCachePath, [byte[]]($PipelineCacheBytes[0..15]))
    Invoke-NativeLogged $Probe @(
        "--frames", $CacheFrames.ToString(),
        "--pipeline-cache", $TruncatedCachePath
    ) "pipeline-cache-truncated-recovery.log"

    $IncompatibleCachePath = Join-Path $ArtifactDirectory "pipeline-cache-incompatible.bin"
    [byte[]]$IncompatibleBytes = $PipelineCacheBytes.Clone()
    $IncompatibleBytes[8] = $IncompatibleBytes[8] -bxor 1
    [System.IO.File]::WriteAllBytes($IncompatibleCachePath, $IncompatibleBytes)
    Invoke-NativeLogged $Probe @(
        "--frames", $CacheFrames.ToString(),
        "--pipeline-cache", $IncompatibleCachePath
    ) "pipeline-cache-incompatible-recovery.log"

    $CorruptCachePath = Join-Path $ArtifactDirectory "pipeline-cache-corrupt.bin"
    [byte[]]$CorruptBytes = $PipelineCacheBytes.Clone()
    if ($CorruptBytes.Length -le 40) {
        throw "pipeline cache artifact is too small for payload-corruption evidence"
    }
    $CorruptBytes[40] = $CorruptBytes[40] -bxor 1
    [System.IO.File]::WriteAllBytes($CorruptCachePath, $CorruptBytes)
    Invoke-NativeLogged $Probe @(
        "--frames", $CacheFrames.ToString(),
        "--pipeline-cache", $CorruptCachePath
    ) "pipeline-cache-corrupt-recovery.log"
    Invoke-NativeLogged $Probe @(
        "--frames", $CacheFrames.ToString(),
        "--disable-pipeline-cache"
    ) "pipeline-cache-disabled.log"

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
        $TitleBarPassed = Read-Yes "Did resize recovery, minimize/restore, maximize/restore, and title-bar close work? [yes/no]"
        $LiveResizePassed = Read-Yes "Did the triangle keep rendering and resize during the drag? [yes/no]"
        $LiveResizeNotes = Read-Host "Live-resize/frame-pacing notes (or 'none')"
        if ([string]::IsNullOrWhiteSpace($LiveResizeNotes)) {
            $LiveResizeNotes = "not recorded"
        }
        $ManualLines += "titlebar_lifecycle_passed=$TitleBarPassed"
        $ManualLines += "live_resize_passed=$LiveResizePassed"
        $ManualLines += "live_resize_notes=$LiveResizeNotes"

        Write-Host ""
        Write-Host "Interactive lifecycle run 2: close the window with Alt+F4."
        Read-Host "Press Enter to launch" | Out-Null
        Invoke-NativeLogged -Command $Probe -Arguments @() -LogName "interactive-alt-f4.log"
        $AltF4Passed = Read-Yes "Did Alt+F4 close the probe cleanly? [yes/no]"
        $ManualLines += "alt_f4_passed=$AltF4Passed"

        $DisplayNotes = Read-Host "Display-change notes (or 'not tested')"
        if ([string]::IsNullOrWhiteSpace($DisplayNotes)) {
            $DisplayNotes = "not recorded"
        }
        $ManualLines += "display_notes=$DisplayNotes"
        $ManualPath = Join-Path $ArtifactDirectory "manual-observations.txt"
        $ManualLines | Set-Content -Path $ManualPath -Encoding UTF8

        if (-not $TitleBarPassed -or -not $LiveResizePassed -or -not $AltF4Passed) {
            throw "one or more interactive lifecycle checks failed"
        }
    }

    $RuntimeLogs = @(
        Join-Path $ArtifactDirectory "finite-run.log"
        Join-Path $ArtifactDirectory "clear-abandon-recovery.log"
        Join-Path $ArtifactDirectory "clear-resize.log"
        Join-Path $ArtifactDirectory "clear-resize.stderr.log"
        Join-Path $ArtifactDirectory "cube-resize.log"
        Join-Path $ArtifactDirectory "cube-resize.stderr.log"
        Join-Path $ArtifactDirectory "postprocess-cube-resize.log"
        Join-Path $ArtifactDirectory "postprocess-cube-resize.stderr.log"
        Join-Path $ArtifactDirectory "msaa-1x-fallback.log"
        Join-Path $ArtifactDirectory "abandon-acquired-frame.log"
        Join-Path $ArtifactDirectory "abandon-acquired-frame-fallback.log"
        Join-Path $ArtifactDirectory "pipeline-cache-strict-4x.log"
        Join-Path $ArtifactDirectory "pipeline-cache-strict-1x.log"
        Join-Path $ArtifactDirectory "pipeline-cache-strict-resize.log"
        Join-Path $ArtifactDirectory "pipeline-cache-strict-resize.stderr.log"
        Join-Path $ArtifactDirectory "texture-rgba8-fallback-4x.log"
        Join-Path $ArtifactDirectory "texture-rgba8-fallback-1x.log"
        Join-Path $ArtifactDirectory "texture-rgba8-fallback-resize.log"
        Join-Path $ArtifactDirectory "texture-rgba8-fallback-resize.stderr.log"
        Join-Path $ArtifactDirectory "pipeline-cache-truncated-recovery.log"
        Join-Path $ArtifactDirectory "pipeline-cache-incompatible-recovery.log"
        Join-Path $ArtifactDirectory "pipeline-cache-corrupt-recovery.log"
        Join-Path $ArtifactDirectory "pipeline-cache-disabled.log"
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
