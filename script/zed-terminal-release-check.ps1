[CmdletBinding()]
Param(
    [Parameter()][string]$Binary,
    [Parameter()][string]$OutputDir,
    [Parameter()][string]$VisualBaselineImage,
    [Parameter()][string]$SplitVisualBaselineImage,
    [Parameter()][int]$StartupTimeoutSeconds = 20,
    [Parameter()][int]$CaptureDelaySeconds = 4,
    [Parameter()][double]$MaxBaselineDifferentPixelRatio = 0.02,
    [Parameter()][double]$MaxBaselineAverageChannelDelta = 2.0,
    [Parameter()][int]$BaselinePixelTolerance = 4,
    [Parameter()][switch]$SkipCargo,
    [Parameter()][switch]$SkipRustTests,
    [Parameter()][switch]$SkipCliDiagnostics,
    [Parameter()][switch]$SkipVisualSmoke,
    [Parameter()][switch]$SkipVisualBaseline,
    [Parameter()][switch]$SkipSplitVisualSmoke
)

$ErrorActionPreference = "Stop"

$repoRoot = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot ".."))
if (-not $Binary) {
    $Binary = Join-Path $repoRoot "target\debug\zed-terminal.exe"
}
if (-not $OutputDir) {
    $OutputDir = Join-Path $repoRoot "target\zed-terminal-release-check"
}
if ($SkipVisualBaseline -and $VisualBaselineImage) {
    throw "-SkipVisualBaseline cannot be used with -VisualBaselineImage."
}
if ($SkipVisualBaseline -and $SplitVisualBaselineImage) {
    throw "-SkipVisualBaseline cannot be used with -SplitVisualBaselineImage."
}
if (-not $VisualBaselineImage -and -not $SkipVisualSmoke -and -not $SkipVisualBaseline) {
    $VisualBaselineImage = Join-Path $repoRoot "crates\zed_terminal\test_fixtures\visual\zed-terminal-default-windows.png"
}
if (-not $SplitVisualBaselineImage -and -not $SkipVisualSmoke -and -not $SkipSplitVisualSmoke -and -not $SkipVisualBaseline) {
    $SplitVisualBaselineImage = Join-Path $repoRoot "crates\zed_terminal\test_fixtures\visual\zed-terminal-split-windows.png"
}

$Binary = [System.IO.Path]::GetFullPath($Binary)
$OutputDir = [System.IO.Path]::GetFullPath($OutputDir)
if ($VisualBaselineImage) {
    $VisualBaselineImage = [System.IO.Path]::GetFullPath($VisualBaselineImage)
}
if ($SplitVisualBaselineImage) {
    $SplitVisualBaselineImage = [System.IO.Path]::GetFullPath($SplitVisualBaselineImage)
}

if ($StartupTimeoutSeconds -lt 1) {
    throw "-StartupTimeoutSeconds must be at least 1."
}
if ($CaptureDelaySeconds -lt 0) {
    throw "-CaptureDelaySeconds must be at least 0."
}
if ($MaxBaselineDifferentPixelRatio -lt 0 -or $MaxBaselineDifferentPixelRatio -gt 1) {
    throw "-MaxBaselineDifferentPixelRatio must be between 0 and 1."
}
if ($MaxBaselineAverageChannelDelta -lt 0 -or $MaxBaselineAverageChannelDelta -gt 255) {
    throw "-MaxBaselineAverageChannelDelta must be between 0 and 255."
}
if ($BaselinePixelTolerance -lt 0 -or $BaselinePixelTolerance -gt 255) {
    throw "-BaselinePixelTolerance must be between 0 and 255."
}
if ($VisualBaselineImage -and -not (Test-Path -LiteralPath $VisualBaselineImage -PathType Leaf)) {
    throw "Visual baseline image not found: $VisualBaselineImage"
}
if ($SplitVisualBaselineImage -and -not (Test-Path -LiteralPath $SplitVisualBaselineImage -PathType Leaf)) {
    throw "Split visual baseline image not found: $SplitVisualBaselineImage"
}

New-Item -ItemType Directory -Force -Path $OutputDir | Out-Null

$timestamp = Get-Date -Format "yyyyMMdd-HHmmss-fff"
$runId = [guid]::NewGuid().ToString("N").Substring(0, 8)
$runDir = Join-Path $OutputDir "run-$timestamp-$runId"
$cliDataDir = Join-Path $runDir "cli-data"
$cliConfigDir = Join-Path $runDir "cli-config"
$brokenCliDataDir = Join-Path $runDir "broken-cli-data"
$brokenCliConfigDir = Join-Path $runDir "broken-cli-config"
$visualSmokeDir = Join-Path $runDir "visual-smoke"
$splitVisualSmokeDir = Join-Path $runDir "visual-smoke-split"
$releaseLog = Join-Path $runDir "zed-terminal-release-check.log"
$summaryFile = Join-Path $runDir "zed-terminal-release-check.json"

New-Item -ItemType Directory -Force -Path $runDir, $cliDataDir, $cliConfigDir, $brokenCliDataDir, $brokenCliConfigDir | Out-Null
Set-Content -LiteralPath $releaseLog -Value "" -Encoding utf8

$script:StepResults = New-Object System.Collections.Generic.List[object]

function Write-ReleaseLog {
    param([Parameter(Mandatory = $true)][string]$Message)

    $line = "{0} {1}" -f (Get-Date -Format "yyyy-MM-dd HH:mm:ss.fff"), $Message
    Add-Content -LiteralPath $releaseLog -Value $line -Encoding utf8
}

function Write-StepOutput {
    param([Parameter()][object[]]$Output)

    foreach ($line in $Output) {
        if ($null -ne $line) {
            $text = $line.ToString()
            Write-Host $text
            Write-ReleaseLog $text
        }
    }
}

function Quote-ProcessArgument {
    param([Parameter(Mandatory = $true)][AllowEmptyString()][string]$Argument)

    if ($Argument.Length -eq 0) {
        return '""'
    }

    if ($Argument -notmatch '[\s"]') {
        return $Argument
    }

    $builder = New-Object System.Text.StringBuilder
    [void]$builder.Append('"')

    $backslashes = 0
    foreach ($char in $Argument.ToCharArray()) {
        if ($char -eq '\') {
            $backslashes++
            continue
        }

        if ($char -eq '"') {
            [void]$builder.Append('\' * ($backslashes * 2 + 1))
            [void]$builder.Append('"')
            $backslashes = 0
            continue
        }

        if ($backslashes -gt 0) {
            [void]$builder.Append('\' * $backslashes)
            $backslashes = 0
        }
        [void]$builder.Append($char)
    }

    if ($backslashes -gt 0) {
        [void]$builder.Append('\' * ($backslashes * 2))
    }

    [void]$builder.Append('"')
    return $builder.ToString()
}

function Invoke-ProcessCapture {
    param(
        [Parameter(Mandatory = $true)][string]$FilePath,
        [Parameter(Mandatory = $true)][string[]]$Arguments,
        [Parameter()][string]$WorkingDirectory = $repoRoot
    )

    $startInfo = New-Object System.Diagnostics.ProcessStartInfo
    $startInfo.FileName = $FilePath
    $startInfo.Arguments = ($Arguments | ForEach-Object { Quote-ProcessArgument $_ }) -join " "
    $startInfo.WorkingDirectory = $WorkingDirectory
    $startInfo.UseShellExecute = $false
    $startInfo.RedirectStandardOutput = $true
    $startInfo.RedirectStandardError = $true
    $startInfo.CreateNoWindow = $true

    $process = New-Object System.Diagnostics.Process
    $process.StartInfo = $startInfo

    if (-not $process.Start()) {
        throw "failed to start process: $FilePath"
    }
    $stdout = $process.StandardOutput.ReadToEndAsync()
    $stderr = $process.StandardError.ReadToEndAsync()
    $process.WaitForExit()

    return [pscustomobject]@{
        ExitCode = $process.ExitCode
        Stdout = $stdout.Result
        Stderr = $stderr.Result
    }
}

function Invoke-Step {
    param(
        [Parameter(Mandatory = $true)][string]$Name,
        [Parameter(Mandatory = $true)][scriptblock]$ScriptBlock
    )

    Write-Host "==> $Name"
    Write-ReleaseLog "START $Name"
    $timer = [System.Diagnostics.Stopwatch]::StartNew()
    try {
        & $ScriptBlock
        $timer.Stop()
        $seconds = [Math]::Round($timer.Elapsed.TotalSeconds, 3)
        $script:StepResults.Add([pscustomobject]@{
            name = $Name
            status = "ok"
            seconds = $seconds
        })
        Write-Host ("ok: {0} ({1}s)" -f $Name, $seconds)
        Write-ReleaseLog ("OK {0} ({1}s)" -f $Name, $seconds)
    } catch {
        $timer.Stop()
        $seconds = [Math]::Round($timer.Elapsed.TotalSeconds, 3)
        $script:StepResults.Add([pscustomobject]@{
            name = $Name
            status = "failed"
            seconds = $seconds
            error = $_.Exception.Message
        })
        Write-Host ("failed: {0} ({1}s)" -f $Name, $seconds)
        Write-ReleaseLog ("FAILED {0} ({1}s): {2}" -f $Name, $seconds, $_.Exception.Message)
        throw
    }
}

function Invoke-NativeCommand {
    param(
        [Parameter(Mandatory = $true)][string]$FilePath,
        [Parameter(Mandatory = $true)][string[]]$Arguments,
        [Parameter()][string]$WorkingDirectory = $repoRoot
    )

    Write-ReleaseLog ("RUN {0} {1}" -f $FilePath, ($Arguments -join " "))
    $result = Invoke-ProcessCapture -FilePath $FilePath -Arguments $Arguments -WorkingDirectory $WorkingDirectory

    Write-StepOutput (($result.Stdout -split "`r?`n") | Where-Object { $_.Length -gt 0 })
    Write-StepOutput (($result.Stderr -split "`r?`n") | Where-Object { $_.Length -gt 0 })
    if ($result.ExitCode -ne 0) {
        throw "Command failed with exit code $($result.ExitCode)`: $FilePath $($Arguments -join ' ')"
    }
}

function Invoke-NativeJsonCommandResult {
    param(
        [Parameter(Mandatory = $true)][string]$Name,
        [Parameter(Mandatory = $true)][string[]]$Arguments
    )

    $stderrFile = Join-Path $runDir "$Name.stderr.log"
    Write-ReleaseLog ("RUN {0} {1}" -f $Binary, ($Arguments -join " "))
    $stdout = & $Binary @Arguments 2> $stderrFile
    $exitCode = $LASTEXITCODE
    $stderr = @()
    if (Test-Path -LiteralPath $stderrFile -PathType Leaf) {
        $stderr = Get-Content -LiteralPath $stderrFile
    }

    Write-StepOutput (($stdout -split "`r?`n") | Where-Object { $_.Length -gt 0 })
    Write-StepOutput $stderr
    if ($exitCode -ne 0) {
        throw "Command failed with exit code $exitCode`: $Binary $($Arguments -join ' ')"
    }

    $jsonText = ($stdout | Out-String).Trim()
    if (-not $jsonText) {
        throw "Command did not produce JSON output: $Binary $($Arguments -join ' ')"
    }

    try {
        $json = $jsonText | ConvertFrom-Json
    } catch {
        throw "Command produced invalid JSON for $Name`: $($_.Exception.Message)"
    }

    $statusProperty = $json.PSObject.Properties["status"]
    if ($statusProperty -and $statusProperty.Value -ne "ok") {
        throw "Command reported status '$($statusProperty.Value)' for $Name`: $Binary $($Arguments -join ' ')"
    }

    return $json
}

function Invoke-NativeJsonCommand {
    param(
        [Parameter(Mandatory = $true)][string]$Name,
        [Parameter(Mandatory = $true)][string[]]$Arguments
    )

    $null = Invoke-NativeJsonCommandResult -Name $Name -Arguments $Arguments
}

function Invoke-NativeTextCommandResult {
    param(
        [Parameter(Mandatory = $true)][string]$Name,
        [Parameter(Mandatory = $true)][string[]]$Arguments,
        [Parameter()][string[]]$RequiredPatterns = @()
    )

    $stderrFile = Join-Path $runDir "$Name.stderr.log"
    Write-ReleaseLog ("RUN {0} {1}" -f $Binary, ($Arguments -join " "))
    $stdout = & $Binary @Arguments 2> $stderrFile
    $exitCode = $LASTEXITCODE
    $stderr = @()
    if (Test-Path -LiteralPath $stderrFile -PathType Leaf) {
        $stderr = Get-Content -LiteralPath $stderrFile
    }

    Write-StepOutput (($stdout -split "`r?`n") | Where-Object { $_.Length -gt 0 })
    Write-StepOutput $stderr
    if ($exitCode -ne 0) {
        throw "Command failed with exit code $exitCode`: $Binary $($Arguments -join ' ')"
    }

    $text = ($stdout | Out-String)
    if (-not $text.Trim()) {
        throw "Command did not produce text output: $Binary $($Arguments -join ' ')"
    }

    foreach ($pattern in $RequiredPatterns) {
        if ($text -notmatch $pattern) {
            throw "Command output for $Name did not match required pattern '$pattern'"
        }
    }

    return $text
}

function Invoke-NativeTextCommand {
    param(
        [Parameter(Mandatory = $true)][string]$Name,
        [Parameter(Mandatory = $true)][string[]]$Arguments,
        [Parameter(Mandatory = $true)][string[]]$RequiredPatterns
    )

    $null = Invoke-NativeTextCommandResult -Name $Name -Arguments $Arguments -RequiredPatterns $RequiredPatterns
}

function Assert-PowerShellSyntax {
    param([Parameter(Mandatory = $true)][string]$Path)

    $errors = $null
    $text = Get-Content -Raw -LiteralPath $Path
    [System.Management.Automation.PSParser]::Tokenize($text, [ref]$errors) | Out-Null
    if ($errors) {
        $errors | Format-List | Out-String | ForEach-Object { Write-ReleaseLog $_ }
        throw "PowerShell syntax check failed: $Path"
    }
}

function Write-ReleaseSummary {
    param([Parameter(Mandatory = $true)][string]$Status)

    $payload = [pscustomobject]@{
        status = $Status
        run_dir = $runDir
        binary = $Binary
        log_file = $releaseLog
        visual_baseline_image = $VisualBaselineImage
        split_visual_baseline_image = $SplitVisualBaselineImage
        visual_baseline_skipped = [bool]$SkipVisualBaseline
        baseline_pixel_tolerance = $BaselinePixelTolerance
        baseline_max_different_pixel_ratio = $MaxBaselineDifferentPixelRatio
        baseline_max_average_channel_delta = $MaxBaselineAverageChannelDelta
        steps = $script:StepResults
    }
    $payload | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath $summaryFile -Encoding utf8
}

try {
    Write-Host "zed-terminal release check"
    Write-Host "repo_root: $repoRoot"
    Write-Host "run_dir: $runDir"
    Write-Host "binary: $Binary"
    if ($VisualBaselineImage) {
        Write-Host "visual_baseline_image: $VisualBaselineImage"
    } elseif ($SkipVisualBaseline) {
        Write-Host "visual_baseline_image: skipped"
    }
    if ($SplitVisualBaselineImage) {
        Write-Host "split_visual_baseline_image: $SplitVisualBaselineImage"
    } elseif ($SkipVisualBaseline) {
        Write-Host "split_visual_baseline_image: skipped"
    }

    Invoke-Step "PowerShell syntax: visual smoke" {
        Assert-PowerShellSyntax (Join-Path $repoRoot "script\zed-terminal-visual-smoke.ps1")
    }

    Invoke-Step "PowerShell syntax: release check" {
        Assert-PowerShellSyntax $PSCommandPath
    }

    if (-not $SkipCargo) {
        Invoke-Step "cargo fmt zed_terminal check" {
            Invoke-NativeCommand -FilePath "cargo" -Arguments @("+stable", "fmt", "--package", "zed_terminal", "--check")
        }

        if (-not $SkipRustTests) {
            Invoke-Step "cargo test zed_terminal" {
                Invoke-NativeCommand -FilePath "cargo" -Arguments @("+stable", "test", "-p", "zed_terminal", "--bin", "zed-terminal")
            }
        }

        Invoke-Step "cargo check zed_terminal" {
            Invoke-NativeCommand -FilePath "cargo" -Arguments @("+stable", "check", "-p", "zed_terminal")
        }

        Invoke-Step "cargo build zed_terminal" {
            Invoke-NativeCommand -FilePath "cargo" -Arguments @("+stable", "build", "-p", "zed_terminal", "--bin", "zed-terminal")
        }
    }

    Invoke-Step "binary exists" {
        if (-not (Test-Path -LiteralPath $Binary -PathType Leaf)) {
            throw "zed-terminal binary not found: $Binary"
        }
    }

    if (-not $SkipCliDiagnostics) {
        Invoke-Step "CLI diagnostics" {
            Invoke-NativeJsonCommand "init-config" @(
                "--user-data-dir", $cliDataDir,
                "--config-dir", $cliConfigDir,
                "--init-config",
                "--init-config-format", "json"
            )
            Invoke-NativeJsonCommand "paths" @(
                "--user-data-dir", $cliDataDir,
                "--config-dir", $cliConfigDir,
                "--paths",
                "--paths-format", "json"
            )
            Invoke-NativeJsonCommand "doctor" @(
                "--user-data-dir", $cliDataDir,
                "--config-dir", $cliConfigDir,
                "--doctor",
                "--doctor-format", "json"
            )
            Invoke-NativeTextCommand "support-info" @(
                "--user-data-dir", $cliDataDir,
                "--config-dir", $cliConfigDir,
                "--support-info"
            ) @(
                "^Zed Terminal Support Info",
                "app_name: Zed Terminal",
                "paths:",
                "diagnostics:"
            )
            Set-Content -LiteralPath (Join-Path $brokenCliConfigDir "terminal.json") -Value "{ broken terminal config" -Encoding utf8
            Invoke-NativeTextCommand "support-info-broken-startup" @(
                "--user-data-dir", $brokenCliDataDir,
                "--config-dir", $brokenCliConfigDir,
                "--support-info"
            ) @(
                "^Zed Terminal Support Info",
                "status: error",
                "startup_config:",
                "message:"
            )
            Invoke-NativeJsonCommand "validate-keymap" @(
                "--user-data-dir", $cliDataDir,
                "--config-dir", $cliConfigDir,
                "--validate-keymap",
                "--validate-keymap-format", "json"
            )
            Invoke-NativeJsonCommand "validate-startup-config" @(
                "--user-data-dir", $cliDataDir,
                "--config-dir", $cliConfigDir,
                "--validate-startup-config",
                "--validate-startup-config-format", "json"
            )
            Invoke-NativeJsonCommand "print-startup-layout" @(
                "--user-data-dir", $cliDataDir,
                "--config-dir", $cliConfigDir,
                "--print-startup-layout",
                "--startup-layout-format", "json"
            )
            $startupSchema = Invoke-NativeJsonCommandResult "print-startup-config-schema" @(
                "--user-data-dir", $cliDataDir,
                "--config-dir", $cliConfigDir,
                "--print-startup-config-schema"
            )
            if ($startupSchema.title -ne "TerminalStartupConfig" -or $startupSchema.type -ne "object") {
                throw "Startup config schema did not report the TerminalStartupConfig object contract."
            }
            foreach ($propertyName in @("working_directory", "command", "shell", "env", "tabs", "default_profile", "profiles")) {
                if (-not $startupSchema.properties.PSObject.Properties[$propertyName]) {
                    throw "Startup config schema is missing expected property '$propertyName'."
                }
            }
            $defaultKeymap = Invoke-NativeTextCommandResult "print-default-keymap" @(
                "--user-data-dir", $cliDataDir,
                "--config-dir", $cliConfigDir,
                "--print-default-keymap"
            ) @(
                "^// Zed Terminal keymap",
                '"ctrl-shift-t": "zed_terminal::NewTerminalTab"',
                '"ctrl-shift-p": "command_palette::Toggle"',
                '"alt-shift-d": "zed_terminal::DuplicateTerminalSplitAuto"',
                '"alt-f4": "zed_terminal::CloseTerminalWindow"'
            )
            if ($defaultKeymap -match "do-not-log") {
                throw "Default keymap output unexpectedly contained release fixture content."
            }
            Set-Content -LiteralPath (Join-Path $cliConfigDir "terminal.json") -Value @'
{
  "default_profile": "work",
  "profiles": {
    "work": {
      "display_name": "Work Shell",
      "description": "Project startup shell",
      "icon": "terminal",
      "color": "#0f766e",
      "command": "pwsh -NoLogo",
      "env": {
        "ZED_TERMINAL_RELEASE_SECRET": "do-not-log"
      },
      "tabs": [
        {
          "title": "Logs",
          "command": "pwsh -NoLogo",
          "env": {
            "LOG_TOKEN": "do-not-log"
          }
        }
      ]
    },
    "secret": {
      "display_name": "Secret",
      "hidden": true
    }
  }
}
'@ -Encoding ascii
            $visibleProfiles = Invoke-NativeJsonCommandResult "list-profiles-visible" @(
                "--user-data-dir", $cliDataDir,
                "--config-dir", $cliConfigDir,
                "--list-profiles",
                "--list-profiles-format", "json"
            )
            if ($visibleProfiles.total_count -ne 2 -or $visibleProfiles.visible_count -ne 1 -or $visibleProfiles.hidden_count -ne 1) {
                throw "Visible profile list reported unexpected counts."
            }
            $visibleProfileEntries = @($visibleProfiles.profiles)
            if ($visibleProfileEntries.Count -ne 1 -or $visibleProfileEntries[0].name -ne "work" -or -not $visibleProfileEntries[0].is_default) {
                throw "Visible profile list did not report only the default work profile."
            }
            $allProfiles = Invoke-NativeJsonCommandResult "list-profiles-all" @(
                "--user-data-dir", $cliDataDir,
                "--config-dir", $cliConfigDir,
                "--list-profiles",
                "--all-profiles",
                "--list-profiles-format", "json"
            )
            $hiddenProfileEntries = @($allProfiles.profiles | Where-Object { $_.name -eq "secret" })
            if ($hiddenProfileEntries.Count -ne 1 -or -not $hiddenProfileEntries[0].hidden) {
                throw "All-profile list did not include the hidden secret profile."
            }
            $profileDescription = Invoke-NativeJsonCommandResult "describe-profile-work" @(
                "--user-data-dir", $cliDataDir,
                "--config-dir", $cliConfigDir,
                "--describe-profile", "work",
                "--describe-profile-format", "json"
            )
            if ($profileDescription.profile -ne "work" -or $profileDescription.display_name -ne "Work Shell" -or -not $profileDescription.is_default) {
                throw "Profile description did not report the expected work profile metadata."
            }
            $profileEnvKeys = @($profileDescription.env_keys)
            $profileTabEnvKeys = @($profileDescription.tabs[0].env_keys)
            if ($profileEnvKeys -notcontains "ZED_TERMINAL_RELEASE_SECRET" -or $profileTabEnvKeys -notcontains "LOG_TOKEN") {
                throw "Profile description did not report expected environment key names."
            }
            $profileDescriptionText = $profileDescription | ConvertTo-Json -Depth 10
            if ($profileDescriptionText -match "do-not-log") {
                throw "Profile description leaked an environment variable value."
            }
        }
    }

    if (-not $SkipVisualSmoke) {
        $visualSmokeArgs = @(
            "-NoProfile",
            "-ExecutionPolicy", "Bypass",
            "-File", (Join-Path $repoRoot "script\zed-terminal-visual-smoke.ps1"),
            "-Binary", $Binary,
            "-OutputDir", $visualSmokeDir,
            "-StartupTimeoutSeconds", "$StartupTimeoutSeconds",
            "-CaptureDelaySeconds", "$CaptureDelaySeconds"
        )
        if ($VisualBaselineImage) {
            $visualSmokeArgs += @(
                "-BaselineImage", $VisualBaselineImage,
                "-MaxBaselineDifferentPixelRatio", "$MaxBaselineDifferentPixelRatio",
                "-MaxBaselineAverageChannelDelta", "$MaxBaselineAverageChannelDelta",
                "-BaselinePixelTolerance", "$BaselinePixelTolerance"
            )
        }

        Invoke-Step "visual smoke" {
            Invoke-NativeCommand -FilePath "powershell" -Arguments $visualSmokeArgs
        }

        if (-not $SkipSplitVisualSmoke) {
            $splitVisualSmokeArgs = @(
                "-NoProfile",
                "-ExecutionPolicy", "Bypass",
                "-File", (Join-Path $repoRoot "script\zed-terminal-visual-smoke.ps1"),
                "-Binary", $Binary,
                "-OutputDir", $splitVisualSmokeDir,
                "-StartupTimeoutSeconds", "$StartupTimeoutSeconds",
                "-CaptureDelaySeconds", "$CaptureDelaySeconds",
                "-VerifySplitPane"
            )
            if ($SplitVisualBaselineImage) {
                $splitVisualSmokeArgs += @(
                    "-BaselineImage", $SplitVisualBaselineImage,
                    "-MaxBaselineDifferentPixelRatio", "$MaxBaselineDifferentPixelRatio",
                    "-MaxBaselineAverageChannelDelta", "$MaxBaselineAverageChannelDelta",
                    "-BaselinePixelTolerance", "$BaselinePixelTolerance"
                )
            }
            Invoke-Step "visual smoke split pane" {
                Invoke-NativeCommand -FilePath "powershell" -Arguments $splitVisualSmokeArgs
            }
        }
    }

    Invoke-Step "git diff check" {
        Invoke-NativeCommand -FilePath "git" -Arguments @("diff", "--check")
    }

    Write-ReleaseSummary "ok"
    Write-Host "status: ok"
    Write-Host "run_dir: $runDir"
    Write-Host "log_file: $releaseLog"
    Write-Host "summary_file: $summaryFile"
} catch {
    Write-ReleaseSummary "failed"
    Write-Host "status: failed"
    Write-Host "run_dir: $runDir"
    Write-Host "log_file: $releaseLog"
    Write-Host "summary_file: $summaryFile"
    throw
}
