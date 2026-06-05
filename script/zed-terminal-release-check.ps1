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
$mutationCliDataDir = Join-Path $runDir "mutation-cli-data"
$mutationCliConfigDir = Join-Path $runDir "mutation-cli-config"
$visualSmokeDir = Join-Path $runDir "visual-smoke"
$splitVisualSmokeDir = Join-Path $runDir "visual-smoke-split"
$releaseLog = Join-Path $runDir "zed-terminal-release-check.log"
$summaryFile = Join-Path $runDir "zed-terminal-release-check.json"

New-Item -ItemType Directory -Force -Path $runDir, $cliDataDir, $cliConfigDir, $brokenCliDataDir, $brokenCliConfigDir, $mutationCliDataDir, $mutationCliConfigDir | Out-Null
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
            Invoke-NativeTextCommand "help-profile-transfer" @(
                "--help"
            ) @(
                "Profile transfer options:",
                "--export-profile <NAME> --export-profile-file <FILE>",
                "--export-profile-format <text\|json>",
                "--import-profile <NAME> --import-profile-file <FILE>",
                "--replace-profile",
                "--import-profile-format <text\|json>",
                "Startup config backup and restore options:",
                "--backup-startup-config --backup-startup-config-file <FILE>",
                "--backup-startup-config-format <text\|json>",
                "--check-startup-config-backup --check-startup-config-backup-file <FILE>",
                "--check-startup-config-backup-format <text\|json>",
                "--diff-startup-config-backup --diff-startup-config-backup-file <FILE>",
                "--diff-startup-config-backup-format <text\|json>",
                "--restore-startup-config --restore-startup-config-file <FILE>",
                "--restore-startup-config-format <text\|json>",
                "Keymap backup and restore options:",
                "--backup-keymap --backup-keymap-file <FILE>",
                "--backup-keymap-format <text\|json>",
                "--check-keymap-backup --check-keymap-backup-file <FILE>",
                "--check-keymap-backup-format <text\|json>",
                "--diff-keymap-backup --diff-keymap-backup-file <FILE>",
                "--diff-keymap-backup-format <text\|json>",
                "--restore-keymap --restore-keymap-file <FILE>",
                "--restore-keymap-format <text\|json>",
                "--print-keymap-schema",
                "Profile transfer, startup config file, and keymap file options may be combined with --user-data-dir",
                "and --config-dir only."
            )
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
            $keymapSchema = Invoke-NativeJsonCommandResult "print-keymap-schema" @(
                "--user-data-dir", $cliDataDir,
                "--config-dir", $cliConfigDir,
                "--print-keymap-schema"
            )
            if ($keymapSchema.title -ne "KeymapFile" -or $keymapSchema.type -ne "array") {
                throw "Keymap schema did not report the KeymapFile array contract."
            }
            $keymapSchemaText = $keymapSchema | ConvertTo-Json -Depth 100
            foreach ($actionName in @("zed_terminal::NewTerminalTab", "zed_terminal::NewTerminalTabWithProfile", "terminal::Paste", "pane::CloseActiveItem")) {
                if ($keymapSchemaText -notmatch [regex]::Escape($actionName)) {
                    throw "Keymap schema is missing expected action '$actionName'."
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
            Invoke-NativeJsonCommand "mutation-init-config" @(
                "--user-data-dir", $mutationCliDataDir,
                "--config-dir", $mutationCliConfigDir,
                "--init-config",
                "--init-config-format", "json"
            )
            $mutationKeymapFile = Join-Path $mutationCliConfigDir "keymap.json"
            Set-Content -LiteralPath $mutationKeymapFile -Value @'
// release-check keymap do-not-log-keymap
[
  {
    "bindings": {
      "ctrl-shift-t": "zed_terminal::NewTerminalTab",
      "ctrl-shift-w": "pane::CloseActiveItem"
    },
    "unbind": {
      "ctrl-j": "zed_terminal::NewTerminalTab"
    }
  },
  {
    "context": "Terminal",
    "bindings": {
      "ctrl-shift-v": "terminal::Paste"
    }
  }
]
'@ -Encoding ascii
            $mutationKeymapBackupFile = Join-Path $mutationCliConfigDir "backups\keymap.backup.json"
            $keymapBackup = Invoke-NativeJsonCommandResult "mutation-backup-keymap" @(
                "--user-data-dir", $mutationCliDataDir,
                "--config-dir", $mutationCliConfigDir,
                "--backup-keymap",
                "--backup-keymap-file", $mutationKeymapBackupFile,
                "--backup-keymap-format", "json"
            )
            if ($keymapBackup.keymap_file -ne $mutationKeymapFile -or $keymapBackup.backup_file -ne $mutationKeymapBackupFile -or $keymapBackup.section_count -ne 2 -or $keymapBackup.binding_count -ne 4 -or $keymapBackup.unbind_count -ne 1 -or $keymapBackup.byte_count -le 0) {
                throw "Keymap backup did not report the expected mutated keymap summary."
            }
            $keymapBackupText = $keymapBackup | ConvertTo-Json -Depth 10
            if ($keymapBackupText -match "do-not-log-keymap" -or $keymapBackupText -match "zed_terminal::NewTerminalTab" -or $keymapBackupText -match "terminal::Paste") {
                throw "Keymap backup output leaked keymap file contents."
            }
            $keymapBackupFileText = Get-Content -Raw -LiteralPath $mutationKeymapBackupFile
            $mutationKeymapFileText = Get-Content -Raw -LiteralPath $mutationKeymapFile
            if ($keymapBackupFileText -ne $mutationKeymapFileText) {
                throw "Keymap backup file did not match keymap.json exactly."
            }
            if ($keymapBackupFileText -notmatch "do-not-log-keymap") {
                throw "Keymap backup file did not preserve the full keymap payload."
            }
            $keymapBackupCheck = Invoke-NativeJsonCommandResult "mutation-check-keymap-backup-match" @(
                "--user-data-dir", $mutationCliDataDir,
                "--config-dir", $mutationCliConfigDir,
                "--check-keymap-backup",
                "--check-keymap-backup-file", $mutationKeymapBackupFile,
                "--check-keymap-backup-format", "json"
            )
            if (-not $keymapBackupCheck.matches -or $keymapBackupCheck.keymap_file -ne $mutationKeymapFile -or $keymapBackupCheck.backup_file -ne $mutationKeymapBackupFile -or $keymapBackupCheck.keymap_byte_count -ne $keymapBackup.byte_count -or $keymapBackupCheck.backup_byte_count -ne $keymapBackup.byte_count -or $keymapBackupCheck.keymap_section_count -ne 2 -or $keymapBackupCheck.backup_section_count -ne 2 -or $keymapBackupCheck.keymap_binding_count -ne 4 -or $keymapBackupCheck.backup_binding_count -ne 4 -or $keymapBackupCheck.keymap_unbind_count -ne 1 -or $keymapBackupCheck.backup_unbind_count -ne 1) {
                throw "Keymap backup check did not report the expected matching keymap summary."
            }
            $keymapBackupCheckText = $keymapBackupCheck | ConvertTo-Json -Depth 10
            if ($keymapBackupCheckText -match "do-not-log-keymap" -or $keymapBackupCheckText -match "terminal::Paste") {
                throw "Keymap backup check output leaked keymap file contents."
            }
            $keymapBackupDiffMatch = Invoke-NativeJsonCommandResult "mutation-diff-keymap-backup-match" @(
                "--user-data-dir", $mutationCliDataDir,
                "--config-dir", $mutationCliConfigDir,
                "--diff-keymap-backup",
                "--diff-keymap-backup-file", $mutationKeymapBackupFile,
                "--diff-keymap-backup-format", "json"
            )
            $keymapBackupDiffMatchCategories = @($keymapBackupDiffMatch.categories)
            if (-not $keymapBackupDiffMatch.text_matches -or -not $keymapBackupDiffMatch.keymap_matches -or $keymapBackupDiffMatchCategories.Count -ne 0 -or $keymapBackupDiffMatch.keymap_binding_count -ne 4 -or $keymapBackupDiffMatch.backup_binding_count -ne 4) {
                throw "Keymap backup diff did not report the expected matching keymap summary."
            }
            $keymapBackupDiffMatchText = $keymapBackupDiffMatch | ConvertTo-Json -Depth 10
            if ($keymapBackupDiffMatchText -match "do-not-log-keymap" -or $keymapBackupDiffMatchText -match "terminal::Paste") {
                throw "Keymap backup diff match output leaked keymap file contents."
            }
            Add-Content -LiteralPath $mutationKeymapFile -Value "`n// release-check keymap drift"
            $keymapBackupTextDrift = Invoke-NativeJsonCommandResult "mutation-diff-keymap-backup-text-drift" @(
                "--user-data-dir", $mutationCliDataDir,
                "--config-dir", $mutationCliConfigDir,
                "--diff-keymap-backup",
                "--diff-keymap-backup-file", $mutationKeymapBackupFile,
                "--diff-keymap-backup-format", "json"
            )
            $keymapBackupTextDriftCategories = @($keymapBackupTextDrift.categories)
            if ($keymapBackupTextDrift.text_matches -or -not $keymapBackupTextDrift.keymap_matches -or $keymapBackupTextDriftCategories.Count -ne 1 -or $keymapBackupTextDriftCategories -notcontains "text") {
                throw "Keymap backup diff did not distinguish text-only keymap drift."
            }
            Set-Content -LiteralPath $mutationKeymapFile -Value $keymapBackupFileText -NoNewline
            $changedKeymapFileText = $keymapBackupFileText -replace '"ctrl-shift-v": "terminal::Paste"', """ctrl-shift-v"": ""terminal::Copy"",`n      ""ctrl-shift-c"": ""terminal::Copy"""
            Set-Content -LiteralPath $mutationKeymapFile -Value $changedKeymapFileText -NoNewline
            $keymapBackupDiffDrift = Invoke-NativeJsonCommandResult "mutation-diff-keymap-backup-drift" @(
                "--user-data-dir", $mutationCliDataDir,
                "--config-dir", $mutationCliConfigDir,
                "--diff-keymap-backup",
                "--diff-keymap-backup-file", $mutationKeymapBackupFile,
                "--diff-keymap-backup-format", "json"
            )
            $keymapBackupDiffDriftCategories = @($keymapBackupDiffDrift.categories)
            if ($keymapBackupDiffDrift.text_matches -or $keymapBackupDiffDrift.keymap_matches -or $keymapBackupDiffDriftCategories -notcontains "text" -or $keymapBackupDiffDriftCategories -notcontains "keymap" -or $keymapBackupDiffDriftCategories -notcontains "binding_count" -or $keymapBackupDiffDrift.keymap_binding_count -ne 5 -or $keymapBackupDiffDrift.backup_binding_count -ne 4) {
                throw "Keymap backup diff did not report the expected binding drift."
            }
            $keymapBackupDiffDriftText = $keymapBackupDiffDrift | ConvertTo-Json -Depth 10
            if ($keymapBackupDiffDriftText -match "terminal::Copy" -or $keymapBackupDiffDriftText -match "terminal::Paste" -or $keymapBackupDiffDriftText -match "do-not-log-keymap") {
                throw "Keymap backup diff drift output leaked keymap file contents."
            }
            Set-Content -LiteralPath $mutationKeymapFile -Value "{ broken keymap" -NoNewline
            $keymapRestore = Invoke-NativeJsonCommandResult "mutation-restore-keymap" @(
                "--user-data-dir", $mutationCliDataDir,
                "--config-dir", $mutationCliConfigDir,
                "--restore-keymap",
                "--restore-keymap-file", $mutationKeymapBackupFile,
                "--restore-keymap-format", "json"
            )
            if ($keymapRestore.keymap_file -ne $mutationKeymapFile -or $keymapRestore.restore_file -ne $mutationKeymapBackupFile -or $keymapRestore.section_count -ne 2 -or $keymapRestore.binding_count -ne 4 -or $keymapRestore.unbind_count -ne 1 -or $keymapRestore.byte_count -ne $keymapBackup.byte_count) {
                throw "Keymap restore did not report the expected restored keymap summary."
            }
            $keymapRestoreText = $keymapRestore | ConvertTo-Json -Depth 10
            if ($keymapRestoreText -match "do-not-log-keymap" -or $keymapRestoreText -match "terminal::Paste") {
                throw "Keymap restore output leaked keymap file contents."
            }
            $restoredKeymapFileText = Get-Content -Raw -LiteralPath $mutationKeymapFile
            if ($restoredKeymapFileText -ne $keymapBackupFileText) {
                throw "Keymap restore did not restore keymap.json exactly from the backup file."
            }
            $createdProfile = Invoke-NativeJsonCommandResult "mutation-create-profile" @(
                "--user-data-dir", $mutationCliDataDir,
                "--config-dir", $mutationCliConfigDir,
                "--create-profile", "work",
                "--profile-display-name", "Work Shell",
                "--profile-description", "Project startup shell",
                "--profile-icon", "terminal",
                "--profile-color", "#0f766e",
                "--create-profile-format", "json"
            )
            if ($createdProfile.profile -ne "work" -or $createdProfile.display_name -ne "Work Shell" -or -not $createdProfile.changed -or $createdProfile.total_profile_count -ne 1) {
                throw "Profile creation mutation did not report the expected created work profile."
            }
            $defaultProfileUpdate = Invoke-NativeJsonCommandResult "mutation-set-default-profile" @(
                "--user-data-dir", $mutationCliDataDir,
                "--config-dir", $mutationCliConfigDir,
                "--set-default-profile", "work",
                "--default-profile-format", "json"
            )
            if ($defaultProfileUpdate.default_profile -ne "work" -or $null -ne $defaultProfileUpdate.previous_default_profile -or -not $defaultProfileUpdate.changed) {
                throw "Default profile mutation did not report work as the new default profile."
            }
            $profileStartupUpdate = Invoke-NativeJsonCommandResult "mutation-update-profile-startup" @(
                "--user-data-dir", $mutationCliDataDir,
                "--config-dir", $mutationCliConfigDir,
                "--update-profile-startup", "work",
                "--profile-command", "pwsh -NoLogo",
                "--profile-title", "Work Home",
                "--update-profile-startup-format", "json"
            )
            if ($profileStartupUpdate.profile -ne "work" -or $profileStartupUpdate.command -ne "pwsh -NoLogo" -or $profileStartupUpdate.title -ne "Work Home" -or -not $profileStartupUpdate.changed) {
                throw "Profile startup mutation did not report the expected command and title."
            }
            $profileEnvUpdate = Invoke-NativeJsonCommandResult "mutation-update-profile-env" @(
                "--user-data-dir", $mutationCliDataDir,
                "--config-dir", $mutationCliConfigDir,
                "--update-profile-env", "work",
                "--profile-env", "ZED_TERMINAL_MUTATION_TOKEN=release-check-value",
                "--update-profile-env-format", "json"
            )
            $profileEnvKeys = @($profileEnvUpdate.env_keys)
            if ($profileEnvUpdate.profile -ne "work" -or $profileEnvKeys -notcontains "ZED_TERMINAL_MUTATION_TOKEN" -or -not $profileEnvUpdate.changed) {
                throw "Profile environment mutation did not report the expected environment key."
            }
            $profileEnvUpdateText = $profileEnvUpdate | ConvertTo-Json -Depth 10
            if ($profileEnvUpdateText -match "release-check-value") {
                throw "Profile environment mutation output leaked an environment variable value."
            }
            $rootStartupTab = Invoke-NativeJsonCommandResult "mutation-add-startup-tab" @(
                "--user-data-dir", $mutationCliDataDir,
                "--config-dir", $mutationCliConfigDir,
                "--add-startup-tab",
                "--startup-tab-profile", "work",
                "--startup-tab-title", "Work Tab",
                "--startup-tab-split", "right",
                "--add-startup-tab-format", "json"
            )
            if ($rootStartupTab.tab -ne 1 -or $rootStartupTab.tab_config.profile -ne "work" -or $rootStartupTab.tab_config.title -ne "Work Tab" -or $rootStartupTab.tab_config.split -ne "right" -or -not $rootStartupTab.changed) {
                throw "Root startup tab mutation did not report the expected profile-backed split tab."
            }
            Invoke-NativeJsonCommand "mutation-validate-startup-config" @(
                "--user-data-dir", $mutationCliDataDir,
                "--config-dir", $mutationCliConfigDir,
                "--validate-startup-config",
                "--validate-startup-config-format", "json"
            )
            $mutationStartup = Invoke-NativeJsonCommandResult "mutation-describe-startup" @(
                "--user-data-dir", $mutationCliDataDir,
                "--config-dir", $mutationCliConfigDir,
                "--describe-startup",
                "--describe-startup-format", "json"
            )
            $mutationTabs = @($mutationStartup.tabs)
            if ($mutationStartup.default_profile -ne "work" -or $mutationStartup.profile_count -ne 1 -or $mutationStartup.visible_profile_count -ne 1 -or $mutationTabs.Count -ne 1) {
                throw "Mutated startup description did not report the expected default profile and tab counts."
            }
            if ($mutationTabs[0].profile -ne "work" -or $mutationTabs[0].title -ne "Work Tab" -or $mutationTabs[0].split -ne "right") {
                throw "Mutated startup description did not report the expected startup tab."
            }
            $mutationProfileDescription = Invoke-NativeJsonCommandResult "mutation-describe-profile-work" @(
                "--user-data-dir", $mutationCliDataDir,
                "--config-dir", $mutationCliConfigDir,
                "--describe-profile", "work",
                "--describe-profile-format", "json"
            )
            $mutationProfileEnvKeys = @($mutationProfileDescription.env_keys)
            if ($mutationProfileDescription.profile -ne "work" -or -not $mutationProfileDescription.is_default -or $mutationProfileDescription.command -ne "pwsh -NoLogo" -or $mutationProfileDescription.title -ne "Work Home" -or $mutationProfileEnvKeys -notcontains "ZED_TERMINAL_MUTATION_TOKEN") {
                throw "Mutated profile description did not report the expected default profile state."
            }
            $mutationProfileReferences = @($mutationProfileDescription.references)
            if ($mutationProfileDescription.reference_count -ne 2 -or $mutationProfileReferences.Count -ne 2 -or $mutationProfileReferences[0].kind -ne "default_profile" -or $mutationProfileReferences[1].kind -ne "root_tab" -or $mutationProfileReferences[1].tab -ne 1) {
                throw "Mutated profile description did not report the expected default/root tab references."
            }
            $mutationProfileDescriptionText = $mutationProfileDescription | ConvertTo-Json -Depth 10
            if ($mutationProfileDescriptionText -match "release-check-value") {
                throw "Mutated profile description leaked an environment variable value."
            }
            $mutationStartupConfigFile = Join-Path $mutationCliConfigDir "terminal.json"
            $mutationStartupBackupFile = Join-Path $mutationCliConfigDir "backups\terminal.backup.json"
            $startupBackup = Invoke-NativeJsonCommandResult "mutation-backup-startup-config" @(
                "--user-data-dir", $mutationCliDataDir,
                "--config-dir", $mutationCliConfigDir,
                "--backup-startup-config",
                "--backup-startup-config-file", $mutationStartupBackupFile,
                "--backup-startup-config-format", "json"
            )
            if ($startupBackup.startup_config_file -ne $mutationStartupConfigFile -or $startupBackup.backup_file -ne $mutationStartupBackupFile -or $startupBackup.layout_count -ne 2 -or $startupBackup.tab_count -ne 3 -or $startupBackup.profile_count -ne 1 -or $startupBackup.byte_count -le 0) {
                throw "Startup config backup did not report the expected mutated startup config summary."
            }
            $startupBackupText = $startupBackup | ConvertTo-Json -Depth 10
            if ($startupBackupText -match "release-check-value") {
                throw "Startup config backup output leaked an environment variable value."
            }
            $startupBackupFileText = Get-Content -Raw -LiteralPath $mutationStartupBackupFile
            $mutationStartupFileText = Get-Content -Raw -LiteralPath $mutationStartupConfigFile
            if ($startupBackupFileText -ne $mutationStartupFileText) {
                throw "Startup config backup file did not match terminal.json exactly."
            }
            if ($startupBackupFileText -notmatch "release-check-value") {
                throw "Startup config backup file did not preserve the full startup config payload."
            }
            $startupBackupCheck = Invoke-NativeJsonCommandResult "mutation-check-startup-config-backup-match" @(
                "--user-data-dir", $mutationCliDataDir,
                "--config-dir", $mutationCliConfigDir,
                "--check-startup-config-backup",
                "--check-startup-config-backup-file", $mutationStartupBackupFile,
                "--check-startup-config-backup-format", "json"
            )
            if (-not $startupBackupCheck.matches -or $startupBackupCheck.startup_config_file -ne $mutationStartupConfigFile -or $startupBackupCheck.backup_file -ne $mutationStartupBackupFile -or $startupBackupCheck.startup_byte_count -ne $startupBackup.byte_count -or $startupBackupCheck.backup_byte_count -ne $startupBackup.byte_count -or $startupBackupCheck.startup_layout_count -ne 2 -or $startupBackupCheck.backup_layout_count -ne 2 -or $startupBackupCheck.startup_tab_count -ne 3 -or $startupBackupCheck.backup_tab_count -ne 3 -or $startupBackupCheck.startup_profile_count -ne 1 -or $startupBackupCheck.backup_profile_count -ne 1) {
                throw "Startup config backup check did not report the expected matching startup config summary."
            }
            $startupBackupCheckText = $startupBackupCheck | ConvertTo-Json -Depth 10
            if ($startupBackupCheckText -match "release-check-value") {
                throw "Startup config backup check output leaked an environment variable value."
            }
            $startupBackupDiffMatch = Invoke-NativeJsonCommandResult "mutation-diff-startup-config-backup-match" @(
                "--user-data-dir", $mutationCliDataDir,
                "--config-dir", $mutationCliConfigDir,
                "--diff-startup-config-backup",
                "--diff-startup-config-backup-file", $mutationStartupBackupFile,
                "--diff-startup-config-backup-format", "json"
            )
            $startupBackupDiffMatchCategories = @($startupBackupDiffMatch.categories)
            if (-not $startupBackupDiffMatch.text_matches -or -not $startupBackupDiffMatch.config_matches -or $startupBackupDiffMatchCategories.Count -ne 0 -or $startupBackupDiffMatch.startup_layout_count -ne 2 -or $startupBackupDiffMatch.backup_layout_count -ne 2 -or $startupBackupDiffMatch.startup_tab_count -ne 3 -or $startupBackupDiffMatch.backup_tab_count -ne 3) {
                throw "Startup config backup diff did not report the expected matching startup config summary."
            }
            $startupBackupDiffMatchText = $startupBackupDiffMatch | ConvertTo-Json -Depth 10
            if ($startupBackupDiffMatchText -match "release-check-value") {
                throw "Startup config backup diff match output leaked an environment variable value."
            }
            Add-Content -LiteralPath $mutationStartupConfigFile -Value "`n// release-check drift"
            $startupBackupDriftCheck = Invoke-NativeJsonCommandResult "mutation-check-startup-config-backup-drift" @(
                "--user-data-dir", $mutationCliDataDir,
                "--config-dir", $mutationCliConfigDir,
                "--check-startup-config-backup",
                "--check-startup-config-backup-file", $mutationStartupBackupFile,
                "--check-startup-config-backup-format", "json"
            )
            if ($startupBackupDriftCheck.matches -or $startupBackupDriftCheck.startup_byte_count -le $startupBackupDriftCheck.backup_byte_count -or $startupBackupDriftCheck.startup_layout_count -ne 2 -or $startupBackupDriftCheck.backup_layout_count -ne 2) {
                throw "Startup config backup check did not report the expected drifted startup config summary."
            }
            Set-Content -LiteralPath $mutationStartupConfigFile -Value $startupBackupFileText -NoNewline
            Invoke-NativeJsonCommand "mutation-update-profile-env-for-backup-diff" @(
                "--user-data-dir", $mutationCliDataDir,
                "--config-dir", $mutationCliConfigDir,
                "--update-profile-env", "work",
                "--profile-env", "ZED_TERMINAL_MUTATION_TOKEN=release-check-updated",
                "--update-profile-env-format", "json"
            )
            $startupBackupDiffDrift = Invoke-NativeJsonCommandResult "mutation-diff-startup-config-backup-drift" @(
                "--user-data-dir", $mutationCliDataDir,
                "--config-dir", $mutationCliConfigDir,
                "--diff-startup-config-backup",
                "--diff-startup-config-backup-file", $mutationStartupBackupFile,
                "--diff-startup-config-backup-format", "json"
            )
            $startupBackupDiffDriftCategories = @($startupBackupDiffDrift.categories)
            $startupBackupDiffDriftProfiles = @($startupBackupDiffDrift.changed_profiles)
            if ($startupBackupDiffDrift.text_matches -or $startupBackupDiffDrift.config_matches -or $startupBackupDiffDriftCategories -notcontains "text" -or $startupBackupDiffDriftCategories -notcontains "profile_env" -or $startupBackupDiffDriftProfiles.Count -ne 1 -or $startupBackupDiffDriftProfiles[0].profile -ne "work" -or -not $startupBackupDiffDriftProfiles[0].env_changed -or $startupBackupDiffDriftProfiles[0].env_keys_changed) {
                throw "Startup config backup diff did not report the expected profile environment drift."
            }
            $startupBackupDiffDriftText = $startupBackupDiffDrift | ConvertTo-Json -Depth 10
            if ($startupBackupDiffDriftText -match "release-check-value" -or $startupBackupDiffDriftText -match "release-check-updated") {
                throw "Startup config backup diff drift output leaked an environment variable value."
            }
            Set-Content -LiteralPath $mutationStartupConfigFile -Value "{ broken terminal config" -NoNewline
            $startupRestore = Invoke-NativeJsonCommandResult "mutation-restore-startup-config" @(
                "--user-data-dir", $mutationCliDataDir,
                "--config-dir", $mutationCliConfigDir,
                "--restore-startup-config",
                "--restore-startup-config-file", $mutationStartupBackupFile,
                "--restore-startup-config-format", "json"
            )
            if ($startupRestore.startup_config_file -ne $mutationStartupConfigFile -or $startupRestore.restore_file -ne $mutationStartupBackupFile -or $startupRestore.layout_count -ne 2 -or $startupRestore.tab_count -ne 3 -or $startupRestore.profile_count -ne 1 -or $startupRestore.byte_count -ne $startupBackup.byte_count) {
                throw "Startup config restore did not report the expected restored startup config summary."
            }
            $startupRestoreText = $startupRestore | ConvertTo-Json -Depth 10
            if ($startupRestoreText -match "release-check-value") {
                throw "Startup config restore output leaked an environment variable value."
            }
            $restoredStartupFileText = Get-Content -Raw -LiteralPath $mutationStartupConfigFile
            if ($restoredStartupFileText -ne $startupBackupFileText) {
                throw "Startup config restore did not restore terminal.json exactly from the backup file."
            }
            $mutationProfileExportFile = Join-Path $mutationCliConfigDir "work-profile-export.json"
            $profileExport = Invoke-NativeJsonCommandResult "mutation-export-profile-work" @(
                "--user-data-dir", $mutationCliDataDir,
                "--config-dir", $mutationCliConfigDir,
                "--export-profile", "work",
                "--export-profile-file", $mutationProfileExportFile,
                "--export-profile-format", "json"
            )
            if ($profileExport.profile -ne "work" -or $profileExport.export_file -ne $mutationProfileExportFile -or $profileExport.tab_count -ne 0) {
                throw "Profile export mutation did not report the expected work profile export."
            }
            $profileExportEnvKeys = @($profileExport.env_keys)
            if ($profileExportEnvKeys -notcontains "ZED_TERMINAL_MUTATION_TOKEN") {
                throw "Profile export mutation did not report the expected environment key."
            }
            $profileExportText = $profileExport | ConvertTo-Json -Depth 10
            if ($profileExportText -match "release-check-value") {
                throw "Profile export mutation output leaked an environment variable value."
            }
            $profileExportFileText = Get-Content -Raw -LiteralPath $mutationProfileExportFile
            $profileExportFileJson = $profileExportFileText | ConvertFrom-Json
            if ($profileExportFileJson.format -ne "zed-terminal-startup-profile" -or $profileExportFileJson.version -ne 1 -or $profileExportFileJson.profile -ne "work" -or $profileExportFileJson.config.env.ZED_TERMINAL_MUTATION_TOKEN -ne "release-check-value") {
                throw "Profile export file did not contain the expected portable profile payload."
            }
            $profileImport = Invoke-NativeJsonCommandResult "mutation-import-profile-admin" @(
                "--user-data-dir", $mutationCliDataDir,
                "--config-dir", $mutationCliConfigDir,
                "--import-profile", "admin",
                "--import-profile-file", $mutationProfileExportFile,
                "--import-profile-format", "json"
            )
            if ($profileImport.source_profile -ne "work" -or $profileImport.profile -ne "admin" -or $profileImport.replaced -or -not $profileImport.changed -or $profileImport.total_profile_count -ne 2) {
                throw "Profile import mutation did not report the expected imported admin profile."
            }
            $profileImportEnvKeys = @($profileImport.env_keys)
            if ($profileImportEnvKeys -notcontains "ZED_TERMINAL_MUTATION_TOKEN") {
                throw "Profile import mutation did not report the expected environment key."
            }
            $profileImportText = $profileImport | ConvertTo-Json -Depth 10
            if ($profileImportText -match "release-check-value") {
                throw "Profile import mutation output leaked an environment variable value."
            }
            Invoke-NativeJsonCommand "mutation-validate-startup-config-after-import" @(
                "--user-data-dir", $mutationCliDataDir,
                "--config-dir", $mutationCliConfigDir,
                "--validate-startup-config",
                "--validate-startup-config-format", "json"
            )
            $mutationAdminDescription = Invoke-NativeJsonCommandResult "mutation-describe-profile-admin" @(
                "--user-data-dir", $mutationCliDataDir,
                "--config-dir", $mutationCliConfigDir,
                "--describe-profile", "admin",
                "--describe-profile-format", "json"
            )
            $mutationAdminEnvKeys = @($mutationAdminDescription.env_keys)
            if ($mutationAdminDescription.profile -ne "admin" -or $mutationAdminDescription.is_default -or $mutationAdminDescription.command -ne "pwsh -NoLogo" -or $mutationAdminDescription.title -ne "Work Home" -or $mutationAdminEnvKeys -notcontains "ZED_TERMINAL_MUTATION_TOKEN") {
                throw "Imported admin profile description did not report the expected imported state."
            }
            $mutationAdminReferences = @($mutationAdminDescription.references)
            if ($mutationAdminDescription.reference_count -ne 0 -or $mutationAdminReferences.Count -ne 0) {
                throw "Imported admin profile description unexpectedly reported inbound references."
            }
            $mutationAdminDescriptionText = $mutationAdminDescription | ConvertTo-Json -Depth 10
            if ($mutationAdminDescriptionText -match "release-check-value") {
                throw "Imported admin profile description leaked an environment variable value."
            }
            $adminStartupTab = Invoke-NativeJsonCommandResult "mutation-add-admin-startup-tab" @(
                "--user-data-dir", $mutationCliDataDir,
                "--config-dir", $mutationCliConfigDir,
                "--add-startup-tab",
                "--startup-tab-profile", "admin",
                "--startup-tab-title", "Admin Tab",
                "--add-startup-tab-format", "json"
            )
            if ($adminStartupTab.tab -ne 2 -or $adminStartupTab.tab_config.profile -ne "admin" -or $adminStartupTab.tab_config.title -ne "Admin Tab" -or -not $adminStartupTab.changed) {
                throw "Admin startup tab mutation did not report the expected profile-backed tab."
            }
            $adminProfileRemoval = Invoke-NativeJsonCommandResult "mutation-remove-profile-admin" @(
                "--user-data-dir", $mutationCliDataDir,
                "--config-dir", $mutationCliConfigDir,
                "--remove-profile", "admin",
                "--remove-profile-references",
                "--remove-profile-format", "json"
            )
            if ($adminProfileRemoval.profile -ne "admin" -or -not $adminProfileRemoval.changed -or $adminProfileRemoval.remaining_profile_count -ne 1 -or $adminProfileRemoval.removed_reference_count -ne 1 -or $adminProfileRemoval.removed_root_tab_count -ne 1 -or $adminProfileRemoval.removed_profile_tab_count -ne 0 -or $adminProfileRemoval.cleared_default_profile) {
                throw "Admin profile removal did not report the expected reference cleanup."
            }
            Invoke-NativeJsonCommand "mutation-validate-startup-config-after-admin-removal" @(
                "--user-data-dir", $mutationCliDataDir,
                "--config-dir", $mutationCliConfigDir,
                "--validate-startup-config",
                "--validate-startup-config-format", "json"
            )
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
            if ($visibleProfileEntries.Count -ne 1 -or $visibleProfileEntries[0].name -ne "work" -or -not $visibleProfileEntries[0].is_default -or $visibleProfileEntries[0].reference_count -ne 1) {
                throw "Visible profile list did not report only the referenced default work profile."
            }
            $allProfiles = Invoke-NativeJsonCommandResult "list-profiles-all" @(
                "--user-data-dir", $cliDataDir,
                "--config-dir", $cliConfigDir,
                "--list-profiles",
                "--all-profiles",
                "--list-profiles-format", "json"
            )
            $hiddenProfileEntries = @($allProfiles.profiles | Where-Object { $_.name -eq "secret" })
            if ($hiddenProfileEntries.Count -ne 1 -or -not $hiddenProfileEntries[0].hidden -or $hiddenProfileEntries[0].reference_count -ne 0) {
                throw "All-profile list did not include the unreferenced hidden secret profile."
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
            $profileDescriptionReferences = @($profileDescription.references)
            if ($profileDescription.reference_count -ne 1 -or $profileDescriptionReferences.Count -ne 1 -or $profileDescriptionReferences[0].kind -ne "default_profile") {
                throw "Profile description did not report the expected default profile reference."
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
