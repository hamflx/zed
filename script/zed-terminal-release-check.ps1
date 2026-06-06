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
    [Parameter()][switch]$SkipPackage,
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
$packageSmokeDir = Join-Path $runDir "package-smoke"
$visualSmokeDir = Join-Path $runDir "visual-smoke"
$splitVisualSmokeDir = Join-Path $runDir "visual-smoke-split"
$releaseLog = Join-Path $runDir "zed-terminal-release-check.log"
$summaryFile = Join-Path $runDir "zed-terminal-release-check.json"
$reportFile = Join-Path $runDir "zed-terminal-release-check.md"

New-Item -ItemType Directory -Force -Path $runDir, $cliDataDir, $cliConfigDir, $brokenCliDataDir, $brokenCliConfigDir, $mutationCliDataDir, $mutationCliConfigDir | Out-Null
Set-Content -LiteralPath $releaseLog -Value "" -Encoding utf8

$script:StepResults = New-Object System.Collections.Generic.List[object]
$script:PackageSmoke = $null
$script:VisualSmoke = $null
$script:SplitVisualSmoke = $null
$script:ReleaseSummaryPayload = $null
$script:SourceControl = $null

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

function Invoke-NativeCommandResult {
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

    return $result
}

function Invoke-NativeCommand {
    param(
        [Parameter(Mandatory = $true)][string]$FilePath,
        [Parameter(Mandatory = $true)][string[]]$Arguments,
        [Parameter()][string]$WorkingDirectory = $repoRoot
    )

    $null = Invoke-NativeCommandResult -FilePath $FilePath -Arguments $Arguments -WorkingDirectory $WorkingDirectory
}

function Get-GitSourceInfo {
    try {
        $commitResult = Invoke-ProcessCapture -FilePath "git" -Arguments @("rev-parse", "HEAD")
        if ($commitResult.ExitCode -ne 0) {
            throw "git rev-parse failed"
        }

        $branchResult = Invoke-ProcessCapture -FilePath "git" -Arguments @("rev-parse", "--abbrev-ref", "HEAD")
        $statusResult = Invoke-ProcessCapture -FilePath "git" -Arguments @("status", "--porcelain")
        $statusText = if ($statusResult.ExitCode -eq 0) { $statusResult.Stdout.TrimEnd() } else { "" }
        $statusEntries = if ([string]::IsNullOrWhiteSpace($statusText)) { @() } else { @($statusText -split "`r?`n") }

        return [pscustomobject]@{
            git_available = $true
            git_commit = $commitResult.Stdout.Trim()
            git_branch = if ($branchResult.ExitCode -eq 0) { $branchResult.Stdout.Trim() } else { $null }
            git_dirty = [bool]($statusEntries.Count -gt 0)
            git_status_entry_count = [int]$statusEntries.Count
        }
    } catch {
        return [pscustomobject]@{
            git_available = $false
            git_commit = $null
            git_branch = $null
            git_dirty = $null
            git_status_entry_count = $null
        }
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

    $normalizedText = (($text -split "\s+") -join " ")
    foreach ($pattern in $RequiredPatterns) {
        if ($text -notmatch $pattern -and $normalizedText -notmatch $pattern) {
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

function Assert-ConfigInitializationJson {
    param(
        [Parameter(Mandatory = $true)]$Report,
        [Parameter(Mandatory = $true)][string]$ConfigDir
    )

    $expectedFiles = @{
        settings_file = Join-Path $ConfigDir "settings.json"
        settings_schema_file = Join-Path $ConfigDir "settings.schema.json"
        global_settings_file = Join-Path $ConfigDir "global_settings.json"
        keymap_file = Join-Path $ConfigDir "keymap.json"
        keymap_schema_file = Join-Path $ConfigDir "keymap.schema.json"
        default_keymap_reference_file = Join-Path $ConfigDir "default-keymap.json"
        startup_config_file = Join-Path $ConfigDir "terminal.json"
        startup_config_schema_file = Join-Path $ConfigDir "terminal.schema.json"
    }
    $files = @($Report.files)
    if (
        $Report.status -ne "ok" -or
        [int64]$Report.file_count -ne $expectedFiles.Count -or
        [int64]$Report.created_count + [int64]$Report.existing_count -ne $expectedFiles.Count -or
        $files.Count -ne $expectedFiles.Count
    ) {
        throw "zed-terminal --init-config did not report expected initialization counts"
    }

    foreach ($label in $expectedFiles.Keys) {
        $matches = @($files | Where-Object { $_.label -eq $label })
        if ($matches.Count -ne 1) {
            throw "zed-terminal --init-config did not report exactly one '$label' entry"
        }
        $entry = $matches[0]
        if ($entry.path -ne $expectedFiles[$label]) {
            throw "zed-terminal --init-config reported unexpected path for '$label': $($entry.path)"
        }
        if ($entry.status -ne "created" -and $entry.status -ne "existing") {
            throw "zed-terminal --init-config reported unexpected status for '$label': $($entry.status)"
        }
        if (-not (Test-Path -LiteralPath $expectedFiles[$label] -PathType Leaf)) {
            throw "zed-terminal --init-config did not create expected file for '$label'"
        }
    }

    foreach ($schemaFile in @(
        (Join-Path $ConfigDir "settings.schema.json"),
        (Join-Path $ConfigDir "terminal.schema.json"),
        (Join-Path $ConfigDir "keymap.schema.json")
    )) {
        $schema = Get-Content -LiteralPath $schemaFile -Raw | ConvertFrom-Json
        if (-not $schema.title -or -not $schema.type) {
            throw "zed-terminal --init-config wrote an invalid schema file: $schemaFile"
        }
    }

    $defaultKeymap = Get-Content -LiteralPath (Join-Path $ConfigDir "default-keymap.json") -Raw
    if (
        $defaultKeymap -notmatch "zed_terminal::NewTerminalTab" -or
        $defaultKeymap -notmatch "terminal::Paste"
    ) {
        throw "zed-terminal --init-config wrote an invalid default keymap reference"
    }
}

function Assert-SettingsValidationJson {
    param([Parameter(Mandatory = $true)]$Report)

    $files = @($Report.files)
    $labels = @($files | ForEach-Object { $_.label })
    if (
        $Report.status -ne "ok" -or
        $files.Count -ne 2 -or
        $labels -notcontains "settings_file" -or
        $labels -notcontains "global_settings_file"
    ) {
        throw "zed-terminal --validate-settings did not report expected settings validation status"
    }

    foreach ($file in $files) {
        if ($file.status -eq "error" -or -not $file.parse_status -or -not $file.migration_status) {
            throw "zed-terminal --validate-settings reported an invalid settings file entry"
        }
    }
}

function Assert-SupportBundleFileReports {
    param(
        [Parameter(Mandatory = $true)][object[]]$Files,
        [Parameter(Mandatory = $true)][hashtable]$ExpectedFiles,
        [Parameter(Mandatory = $true)][string]$Context
    )

    if ($Files.Count -ne $ExpectedFiles.Count) {
        throw "$Context reported $($Files.Count) files; expected $($ExpectedFiles.Count)"
    }

    foreach ($label in $ExpectedFiles.Keys) {
        $matches = @($Files | Where-Object { $_.label -eq $label })
        if ($matches.Count -ne 1) {
            throw "$Context did not report exactly one '$label' file"
        }

        $expectedPath = $ExpectedFiles[$label]
        $file = $matches[0]
        if ($file.path -ne $expectedPath) {
            throw "$Context reported unexpected path for '$label': $($file.path)"
        }
        if (-not (Test-Path -LiteralPath $expectedPath -PathType Leaf)) {
            throw "$Context reported a missing file for '$label': $expectedPath"
        }

        $actualLength = (Get-Item -LiteralPath $expectedPath).Length
        if ([int64]$file.byte_count -ne [int64]$actualLength -or [int64]$file.byte_count -le 0) {
            throw "$Context reported an invalid byte count for '$label'"
        }
    }

    foreach ($file in $Files) {
        if (-not $ExpectedFiles.ContainsKey($file.label)) {
            throw "$Context reported unexpected file label: $($file.label)"
        }
    }
}

function Get-SupportBundleMetadataEntry {
    param(
        [Parameter(Mandatory = $true)]$Metadata,
        [Parameter(Mandatory = $true)][string]$Label
    )

    $matches = @($Metadata.files | Where-Object { $_.label -eq $Label })
    if ($matches.Count -ne 1) {
        throw "zed-terminal support bundle metadata did not report exactly one '$Label' entry"
    }
    return $matches[0]
}

function Assert-SupportBundleMetadataEntry {
    param(
        [Parameter(Mandatory = $true)]$Metadata,
        [Parameter(Mandatory = $true)][string]$Label,
        [Parameter(Mandatory = $true)][string]$ExpectedPath
    )

    $entry = Get-SupportBundleMetadataEntry -Metadata $Metadata -Label $Label
    if ($entry.path -ne $ExpectedPath) {
        throw "zed-terminal support bundle metadata reported unexpected path for '$Label': $($entry.path)"
    }

    if (Test-Path -LiteralPath $ExpectedPath -PathType Leaf) {
        $actual = Get-Item -LiteralPath $ExpectedPath
        if ($entry.exists -ne $true -or $entry.kind -ne "file" -or [int64]$entry.byte_count -ne [int64]$actual.Length) {
            throw "zed-terminal support bundle metadata did not match the file at '$Label'"
        }
        return
    }
    if (Test-Path -LiteralPath $ExpectedPath -PathType Container) {
        if ($entry.exists -ne $true -or $entry.kind -ne "directory" -or $null -ne $entry.byte_count) {
            throw "zed-terminal support bundle metadata did not match the directory at '$Label'"
        }
        return
    }

    if ($entry.exists -ne $false -or $entry.kind -ne "missing" -or $null -ne $entry.byte_count) {
        throw "zed-terminal support bundle metadata did not match the missing path at '$Label'"
    }
}

function Assert-SupportBundlePathReport {
    param(
        [Parameter(Mandatory = $true)]$Paths,
        [Parameter(Mandatory = $true)][string]$ExpectedDataDir,
        [Parameter(Mandatory = $true)][string]$ExpectedConfigDir
    )

    $expectedLogsDir = Join-Path $ExpectedDataDir "logs"
    if (
        $Paths.mode -ne "custom" -or
        $Paths.config_dir -ne $ExpectedConfigDir -or
        $Paths.data_dir -ne $ExpectedDataDir -or
        $Paths.logs_dir -ne $expectedLogsDir -or
        $Paths.settings_file -ne (Join-Path $ExpectedConfigDir "settings.json") -or
        $Paths.settings_schema_file -ne (Join-Path $ExpectedConfigDir "settings.schema.json") -or
        $Paths.startup_config_file -ne (Join-Path $ExpectedConfigDir "terminal.json") -or
        $Paths.startup_config_schema_file -ne (Join-Path $ExpectedConfigDir "terminal.schema.json") -or
        $Paths.global_settings_file -ne (Join-Path $ExpectedConfigDir "global_settings.json") -or
        $Paths.keymap_file -ne (Join-Path $ExpectedConfigDir "keymap.json") -or
        $Paths.keymap_schema_file -ne (Join-Path $ExpectedConfigDir "keymap.schema.json") -or
        $Paths.default_keymap_reference_file -ne (Join-Path $ExpectedConfigDir "default-keymap.json") -or
        $Paths.themes_dir -ne (Join-Path $ExpectedConfigDir "themes") -or
        $Paths.log_file -ne (Join-Path $expectedLogsDir "Zed Terminal.log")
    ) {
        throw "zed-terminal support bundle paths report did not match expected standalone paths"
    }
}

function Assert-SupportBundleArtifacts {
    param(
        [Parameter(Mandatory = $true)]$Report,
        [Parameter(Mandatory = $true)][string]$BundleDir,
        [Parameter(Mandatory = $true)][string]$DataDir,
        [Parameter(Mandatory = $true)][string]$ConfigDir,
        [Parameter()][string[]]$SensitiveText = @()
    )

    $manifestFile = Join-Path $BundleDir "zed-terminal-support-bundle.json"
    $metadataFile = Join-Path $BundleDir "zed-terminal-file-metadata.json"
    $supportInfoFile = Join-Path $BundleDir "zed-terminal-support-info.txt"
    $diagnosticsFile = Join-Path $BundleDir "zed-terminal-diagnostics.json"
    $pathsFile = Join-Path $BundleDir "zed-terminal-paths.json"
    $readmeFile = Join-Path $BundleDir "README.txt"
    $expectedReportFiles = @{
        manifest = $manifestFile
        support_info = $supportInfoFile
        diagnostics = $diagnosticsFile
        paths = $pathsFile
        file_metadata = $metadataFile
        readme = $readmeFile
    }

    if (
        $Report.status -ne "ok" -or
        $Report.format -ne "zed-terminal-support-bundle" -or
        $Report.version -ne 1 -or
        $Report.bundle_dir -ne $BundleDir -or
        $Report.manifest_file -ne $manifestFile -or
        $Report.diagnostics_status -ne "ok" -or
        $Report.file_count -ne 6
    ) {
        throw "zed-terminal --support-bundle did not report expected release diagnostics"
    }
    Assert-SupportBundleFileReports `
        -Files @($Report.files) `
        -ExpectedFiles $expectedReportFiles `
        -Context "zed-terminal --support-bundle output"
    $manifestReport = @($Report.files | Where-Object { $_.label -eq "manifest" })[0]
    if ([int64]$Report.manifest_byte_count -ne [int64]$manifestReport.byte_count) {
        throw "zed-terminal --support-bundle manifest byte count did not match its file report"
    }

    $actualFiles = @(Get-ChildItem -LiteralPath $BundleDir -File)
    if ($actualFiles.Count -ne 6) {
        throw "zed-terminal support bundle wrote $($actualFiles.Count) files; expected 6"
    }
    foreach ($file in $actualFiles) {
        if (-not @(
            "zed-terminal-support-bundle.json",
            "zed-terminal-support-info.txt",
            "zed-terminal-diagnostics.json",
            "zed-terminal-paths.json",
            "zed-terminal-file-metadata.json",
            "README.txt"
        ).Contains($file.Name)) {
            throw "zed-terminal support bundle wrote unexpected file: $($file.Name)"
        }
    }

    $manifest = Get-Content -LiteralPath $manifestFile -Raw | ConvertFrom-Json
    if (
        $manifest.format -ne "zed-terminal-support-bundle" -or
        $manifest.version -ne 1 -or
        $manifest.app_name -ne "Zed Terminal" -or
        $manifest.package_name -ne "zed_terminal" -or
        -not $manifest.package_version -or
        -not $manifest.target_os -or
        -not $manifest.target_arch -or
        $null -eq $manifest.debug_assertions -or
        $manifest.bundle_dir -ne $BundleDir -or
        $manifest.diagnostics_status -ne "ok" -or
        $manifest.path_mode -ne "custom" -or
        $manifest.redaction.includes_raw_config_contents -ne $false -or
        $manifest.redaction.includes_raw_log_contents -ne $false -or
        $manifest.redaction.includes_environment_values -ne $false -or
        $manifest.redaction.file_metadata_only -ne $true
    ) {
        throw "zed-terminal support bundle manifest did not report expected release metadata"
    }
    Assert-SupportBundleFileReports `
        -Files @($manifest.files) `
        -ExpectedFiles @{
            support_info = $supportInfoFile
            diagnostics = $diagnosticsFile
            paths = $pathsFile
            file_metadata = $metadataFile
            readme = $readmeFile
        } `
        -Context "zed-terminal support bundle manifest"

    $paths = Get-Content -LiteralPath $pathsFile -Raw | ConvertFrom-Json
    Assert-SupportBundlePathReport `
        -Paths $paths `
        -ExpectedDataDir $DataDir `
        -ExpectedConfigDir $ConfigDir

    $diagnostics = Get-Content -LiteralPath $diagnosticsFile -Raw | ConvertFrom-Json
    $doctorConfigLabels = @($diagnostics.config_files | ForEach-Object { $_.label })
    if (
        $diagnostics.status -ne "ok" -or
        $diagnostics.settings.status -ne "ok" -or
        $diagnostics.startup_config.status -ne "ok" -or
        $diagnostics.keymap.status -ne "ok" -or
        $doctorConfigLabels -notcontains "settings_schema_file" -or
        $doctorConfigLabels -notcontains "startup_config_schema_file" -or
        $doctorConfigLabels -notcontains "keymap_schema_file" -or
        $doctorConfigLabels -notcontains "default_keymap_reference_file"
    ) {
        throw "zed-terminal support bundle diagnostics did not report expected healthy config checks"
    }

    $metadata = Get-Content -LiteralPath $metadataFile -Raw | ConvertFrom-Json
    if (
        $metadata.status -ne "ok" -or
        $metadata.redaction.includes_raw_file_contents -ne $false -or
        $metadata.redaction.includes_environment_values -ne $false -or
        @($metadata.files).Count -ne 14
    ) {
        throw "zed-terminal support bundle metadata did not report expected redaction policy"
    }
    Assert-SupportBundleMetadataEntry -Metadata $metadata -Label "settings_file" -ExpectedPath $paths.settings_file
    Assert-SupportBundleMetadataEntry -Metadata $metadata -Label "settings_schema_file" -ExpectedPath $paths.settings_schema_file
    Assert-SupportBundleMetadataEntry -Metadata $metadata -Label "global_settings_file" -ExpectedPath $paths.global_settings_file
    Assert-SupportBundleMetadataEntry -Metadata $metadata -Label "startup_config_file" -ExpectedPath $paths.startup_config_file
    Assert-SupportBundleMetadataEntry -Metadata $metadata -Label "startup_config_schema_file" -ExpectedPath $paths.startup_config_schema_file
    Assert-SupportBundleMetadataEntry -Metadata $metadata -Label "keymap_file" -ExpectedPath $paths.keymap_file
    Assert-SupportBundleMetadataEntry -Metadata $metadata -Label "keymap_schema_file" -ExpectedPath $paths.keymap_schema_file
    Assert-SupportBundleMetadataEntry -Metadata $metadata -Label "default_keymap_reference_file" -ExpectedPath $paths.default_keymap_reference_file
    Assert-SupportBundleMetadataEntry -Metadata $metadata -Label "themes_dir" -ExpectedPath $paths.themes_dir
    Assert-SupportBundleMetadataEntry -Metadata $metadata -Label "log_file" -ExpectedPath $paths.log_file
    Assert-SupportBundleMetadataEntry -Metadata $metadata -Label "old_log_file" -ExpectedPath (Join-Path $paths.logs_dir "Zed Terminal.log.old")
    Assert-SupportBundleMetadataEntry -Metadata $metadata -Label "logs_dir" -ExpectedPath $paths.logs_dir
    Assert-SupportBundleMetadataEntry -Metadata $metadata -Label "config_dir" -ExpectedPath $paths.config_dir
    Assert-SupportBundleMetadataEntry -Metadata $metadata -Label "data_dir" -ExpectedPath $paths.data_dir

    $supportInfo = Get-Content -LiteralPath $supportInfoFile -Raw
    if (
        $supportInfo -notmatch "^Zed Terminal Support Info" -or
        $supportInfo -notmatch "app_name: Zed Terminal" -or
        $supportInfo -notmatch "paths:" -or
        $supportInfo -notmatch "diagnostics:" -or
        $supportInfo -notmatch "status: ok"
    ) {
        throw "zed-terminal support bundle support-info did not report expected diagnostics"
    }

    $readme = Get-Content -LiteralPath $readmeFile -Raw
    foreach ($text in @(
        "redacted diagnostics generated by zed-terminal",
        "does not include raw config files",
        "raw log contents",
        "shell environment values",
        "terminal buffer contents",
        "file existence and byte counts only",
        "zed-terminal-support-bundle.json",
        "zed-terminal-diagnostics.json",
        "zed-terminal-paths.json",
        "zed-terminal-file-metadata.json"
    )) {
        if (-not $readme.Contains($text)) {
            throw "zed-terminal support bundle README did not document expected redaction/file policy: $text"
        }
    }

    $bundleText = Get-ChildItem -LiteralPath $BundleDir -File | ForEach-Object {
        Get-Content -LiteralPath $_.FullName -Raw
    } | Out-String
    foreach ($secret in $SensitiveText) {
        if ($bundleText.Contains($secret)) {
            throw "zed-terminal support bundle leaked release redaction fixture text: $secret"
        }
    }
}

function Convert-KeyValueOutput {
    param([Parameter(Mandatory = $true)][object[]]$Output)

    $values = @{}
    foreach ($line in $Output) {
        if ($null -eq $line) {
            continue
        }

        $text = $line.ToString().Trim()
        if ($text -match '^([a-z0-9_]+):\s*(.*)$') {
            $values[$Matches[1]] = $Matches[2]
        }
    }

    return $values
}

function Get-RequiredOutputValue {
    param(
        [Parameter(Mandatory = $true)][hashtable]$Values,
        [Parameter(Mandatory = $true)][string]$Key,
        [Parameter(Mandatory = $true)][string]$Context
    )

    if (-not $Values.ContainsKey($Key) -or [string]::IsNullOrWhiteSpace($Values[$Key])) {
        throw "$Context output did not include $Key"
    }

    return $Values[$Key]
}

function Convert-OutputInt64 {
    param(
        [Parameter(Mandatory = $true)][string]$Value,
        [Parameter(Mandatory = $true)][string]$Name
    )

    $parsed = 0L
    if (-not [int64]::TryParse($Value, [ref]$parsed)) {
        throw "$Name was not a valid integer: $Value"
    }
    return $parsed
}

function Convert-OutputDouble {
    param(
        [Parameter(Mandatory = $true)][string]$Value,
        [Parameter(Mandatory = $true)][string]$Name
    )

    $parsed = 0.0
    if (-not [double]::TryParse(
            $Value,
            [System.Globalization.NumberStyles]::Float,
            [System.Globalization.CultureInfo]::InvariantCulture,
            [ref]$parsed
        )) {
        throw "$Name was not a valid number: $Value"
    }
    return $parsed
}

function Assert-VisualSmokeFile {
    param(
        [Parameter(Mandatory = $true)][string]$Path,
        [Parameter(Mandatory = $true)][int64]$ExpectedBytes,
        [Parameter(Mandatory = $true)][string]$Name
    )

    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) {
        throw "visual smoke output referenced a missing $Name file: $Path"
    }

    $actualBytes = (Get-Item -LiteralPath $Path).Length
    if ($actualBytes -ne $ExpectedBytes) {
        throw "visual smoke $Name byte count did not match the file length"
    }
}

function Read-ReleaseJsonFile {
    param(
        [Parameter(Mandatory = $true)][string]$Path,
        [Parameter(Mandatory = $true)][string]$Label
    )

    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) {
        throw "$Label file was not written: $Path"
    }

    $text = Get-Content -LiteralPath $Path -Raw
    if ([string]::IsNullOrWhiteSpace($text)) {
        throw "$Label file was empty: $Path"
    }

    try {
        $json = $text | ConvertFrom-Json
    } catch {
        throw "$Label file did not parse as JSON: $($_.Exception.Message)"
    }

    return [pscustomobject]@{
        text = $text
        json = $json
    }
}

function Assert-PackageJsonSchemaFile {
    param(
        [Parameter(Mandatory = $true)][string]$Path,
        [Parameter(Mandatory = $true)][string]$Label,
        [Parameter(Mandatory = $true)][string]$ExpectedTitle,
        [Parameter(Mandatory = $true)][string]$ExpectedType,
        [Parameter()][string[]]$RequiredProperties = @(),
        [Parameter()][string[]]$RequiredSnippets = @(),
        [Parameter()][string[]]$ForbiddenSnippets = @()
    )

    $schema = Read-ReleaseJsonFile -Path $Path -Label $Label
    if ($schema.json.title -ne $ExpectedTitle -or $schema.json.type -ne $ExpectedType) {
        throw "$Label schema did not report the expected $ExpectedTitle/$ExpectedType contract"
    }
    if (-not $schema.text.EndsWith("`n")) {
        throw "$Label schema should end with a newline"
    }

    foreach ($propertyName in $RequiredProperties) {
        if (-not $schema.json.properties -or -not $schema.json.properties.PSObject.Properties[$propertyName]) {
            throw "$Label schema is missing expected property: $propertyName"
        }
    }

    foreach ($snippet in $RequiredSnippets) {
        if ($schema.text.IndexOf($snippet, [System.StringComparison]::Ordinal) -lt 0) {
            throw "$Label schema is missing expected content: $snippet"
        }
    }

    foreach ($snippet in $ForbiddenSnippets) {
        if ($schema.text.IndexOf($snippet, [System.StringComparison]::Ordinal) -ge 0) {
            throw "$Label schema exposed forbidden content: $snippet"
        }
    }
}

function Assert-PackageConfigTemplateSchemas {
    param([Parameter(Mandatory = $true)][string]$ConfigTemplateDir)

    if (-not (Test-Path -LiteralPath $ConfigTemplateDir -PathType Container)) {
        throw "package config template directory does not exist: $ConfigTemplateDir"
    }

    Assert-PackageJsonSchemaFile `
        -Path (Join-Path $ConfigTemplateDir "terminal.schema.json") `
        -Label "package startup config" `
        -ExpectedTitle "TerminalStartupConfig" `
        -ExpectedType "object" `
        -RequiredProperties @("working_directory", "command", "shell", "env", "tabs", "default_profile", "profiles")

    Assert-PackageJsonSchemaFile `
        -Path (Join-Path $ConfigTemplateDir "settings.schema.json") `
        -Label "package settings" `
        -ExpectedTitle "UserSettingsContent" `
        -ExpectedType "object" `
        -RequiredProperties @("theme") `
        -RequiredSnippets @(
            "zed_terminal::OpenSettingsSchemaFile",
            "zed_terminal::OpenSettingsToolsPicker",
            "zed_terminal::OpenStartupConfigSchemaFile",
            "zed_terminal::OpenKeymapSchemaFile",
            "zed_terminal::NewTerminalTab"
        ) `
        -ForbiddenSnippets @("workspace::NewFile")

    Assert-PackageJsonSchemaFile `
        -Path (Join-Path $ConfigTemplateDir "keymap.schema.json") `
        -Label "package keymap" `
        -ExpectedTitle "KeymapFile" `
        -ExpectedType "array" `
        -RequiredSnippets @(
            "zed_terminal::NewTerminalTab",
            "zed_terminal::NewTerminalTabWithProfile",
            "zed_terminal::NewTerminalTabWithProfileSlot",
            "zed_terminal::NewTerminalWindowWithProfileSlot",
            "zed_terminal::NewTerminalSplitWithProfileSlot",
            "zed_terminal::OpenConfigBundleBackupDirectory",
            "zed_terminal::OpenConfigBundleBackupsDirectory",
            "zed_terminal::OpenConfigInitializationReport",
            "zed_terminal::OpenKeymapToolsPicker",
            "zed_terminal::OpenSettingsSchemaFile",
            "zed_terminal::OpenSettingsToolsPicker",
            "zed_terminal::OpenStartupProfileConfig",
            "zed_terminal::OpenStartupProfilePicker",
            "zed_terminal::OpenSupportToolsPicker",
            "zed_terminal::OpenStartupToolsPicker",
            "zed_terminal::OpenActiveKeymapBindingsReport",
            "zed_terminal::OpenStartupLayoutReport",
            "zed_terminal::OpenPathsReport",
            "zed_terminal::OpenVersionInfoReport",
            "terminal::Paste",
            "pane::CloseActiveItem",
            '"profile"',
            '"slot"'
        )
}

function Read-PackageSmokeSummary {
    param([Parameter(Mandatory = $true)][string]$SummaryFile)

    if (-not (Test-Path -LiteralPath $SummaryFile -PathType Leaf)) {
        throw "package smoke summary was not written: $SummaryFile"
    }

    $summary = Get-Content -LiteralPath $SummaryFile -Raw | ConvertFrom-Json
    if ($summary.status -ne "ok") {
        throw "package smoke did not report ok status"
    }

    $packageDir = [System.IO.Path]::GetFullPath([string]$summary.package_dir)
    $manifestFile = [System.IO.Path]::GetFullPath([string]$summary.manifest_file)
    $readmeFile = [System.IO.Path]::GetFullPath([string]$summary.readme_file)
    $configTemplateDir = [System.IO.Path]::GetFullPath([string]$summary.config_template_dir)
    $packageBinary = [System.IO.Path]::GetFullPath([string]$summary.binary)
    $zipFile = [System.IO.Path]::GetFullPath([string]$summary.zip_file)
    $zipChecksumFile = [System.IO.Path]::GetFullPath([string]$summary.zip_checksum_file)
    $binaryHash = [string]$summary.binary_sha256
    $zipHash = [string]$summary.zip_sha256
    $versionInfo = $summary.version_info

    if (-not (Test-Path -LiteralPath $packageDir -PathType Container)) {
        throw "package smoke package directory does not exist: $packageDir"
    }
    foreach ($file in @($manifestFile, $readmeFile, $packageBinary, $zipFile, $zipChecksumFile)) {
        if (-not (Test-Path -LiteralPath $file -PathType Leaf)) {
            throw "package smoke summary referenced a missing file: $file"
        }
    }
    if (-not (Test-Path -LiteralPath $configTemplateDir -PathType Container)) {
        throw "package smoke summary referenced a missing config template directory: $configTemplateDir"
    }

    foreach ($hash in @($binaryHash, $zipHash)) {
        if ($hash -notmatch '^[a-f0-9]{64}$') {
            throw "package smoke summary contained an invalid SHA256: $hash"
        }
    }

    $actualBinaryHash = (Get-FileHash -LiteralPath $packageBinary -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($actualBinaryHash -ne $binaryHash) {
        throw "package smoke binary SHA256 did not match the packaged binary"
    }

    $actualZipHash = (Get-FileHash -LiteralPath $zipFile -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($actualZipHash -ne $zipHash) {
        throw "package smoke zip SHA256 did not match the generated archive"
    }

    $zipFileName = Split-Path -Leaf $zipFile
    $checksumContent = (Get-Content -LiteralPath $zipChecksumFile -Raw).Trim()
    if ($checksumContent -ne "$zipHash *$zipFileName") {
        throw "package smoke checksum sidecar did not match the generated archive"
    }

    $manifest = Get-Content -LiteralPath $manifestFile -Raw | ConvertFrom-Json
    if (
        $manifest.status -ne "ok" -or
        $manifest.binary -ne (Split-Path -Leaf $packageBinary) -or
        $manifest.binary_sha256 -ne $binaryHash -or
        $manifest.validation.version_info -ne "ok" -or
        $manifest.validation.paths -ne "ok" -or
        $manifest.validation.portable_paths -ne "ok" -or
        $manifest.validation.settings_schema -ne "ok" -or
        $manifest.validation.startup_schema -ne "ok" -or
        $manifest.validation.keymap_schema -ne "ok" -or
        $manifest.validation.default_keymap_reference -ne "ok" -or
        $manifest.validation.licenses -ne "ok" -or
        $manifest.validation.git_provenance -ne "ok" -or
        $manifest.validation.startup_layout -ne "ok" -or
        $manifest.validation.startup_discovery -ne "ok" -or
        $manifest.validation.startup_validation -ne "ok" -or
        $manifest.validation.settings_validation -ne "ok" -or
        $manifest.validation.keymap_validation -ne "ok" -or
        $manifest.validation.keymap_discovery -ne "ok" -or
        $manifest.validation.active_keymap_discovery -ne "ok" -or
        $manifest.validation.settings_backup -ne "ok" -or
        $manifest.validation.startup_backup -ne "ok" -or
        $manifest.validation.keymap_backup -ne "ok" -or
        $manifest.validation.config_bundle -ne "ok" -or
        $manifest.validation.support_bundle -ne "ok" -or
        $manifest.validation.manifest -ne "ok" -or
        $manifest.validation.readme -ne "ok"
    ) {
        throw "package smoke manifest did not match the validated package output"
    }
    if (
        $summary.validation.version_info -ne "ok" -or
        $summary.validation.paths -ne "ok" -or
        $summary.validation.portable_paths -ne "ok" -or
        $summary.validation.settings_schema -ne "ok" -or
        $summary.validation.startup_schema -ne "ok" -or
        $summary.validation.keymap_schema -ne "ok" -or
        $summary.validation.default_keymap_reference -ne "ok" -or
        $summary.validation.licenses -ne "ok" -or
        $summary.validation.git_provenance -ne "ok" -or
        $summary.validation.startup_layout -ne "ok" -or
        $summary.validation.startup_discovery -ne "ok" -or
        $summary.validation.startup_validation -ne "ok" -or
        $summary.validation.settings_validation -ne "ok" -or
        $summary.validation.keymap_validation -ne "ok" -or
        $summary.validation.keymap_discovery -ne "ok" -or
        $summary.validation.active_keymap_discovery -ne "ok" -or
        $summary.validation.settings_backup -ne "ok" -or
        $summary.validation.startup_backup -ne "ok" -or
        $summary.validation.keymap_backup -ne "ok" -or
        $summary.validation.config_bundle -ne "ok" -or
        $summary.validation.support_bundle -ne "ok"
    ) {
        throw "package smoke summary did not report expected path/version/schema/license/git/startup/settings/keymap validation/keymap discovery/active keymap discovery/backup/config bundle/support bundle status"
    }

    if ($manifest.version -ne $summary.version -or $manifest.build_profile -ne $summary.build_profile -or $manifest.platform -ne $summary.platform -or $manifest.architecture -ne $summary.architecture) {
        throw "package smoke summary metadata did not match the validated manifest"
    }
    if (
        -not $manifest.source_control -or
        -not $summary.source_control -or
        $manifest.source_control.git_commit -ne $manifest.git_commit -or
        $summary.source_control.git_commit -ne $summary.git_commit -or
        $manifest.source_control.git_commit -ne $summary.source_control.git_commit -or
        $manifest.source_control.git_branch -ne $summary.source_control.git_branch -or
        $manifest.source_control.git_dirty -ne $summary.source_control.git_dirty -or
        [int]$manifest.source_control.git_status_entry_count -ne [int]$summary.source_control.git_status_entry_count
    ) {
        throw "package smoke source control metadata did not match the validated manifest"
    }
    if (
        $summary.source_control.git_available -eq $true -and
        $summary.source_control.git_commit -notmatch '^[a-f0-9]{40}$'
    ) {
        throw "package smoke source control commit was not a full SHA"
    }
    if (
        -not $versionInfo -or
        -not $manifest.version_info -or
        $versionInfo.app_name -ne "Zed Terminal" -or
        $versionInfo.binary_name -ne "zed-terminal" -or
        $versionInfo.package_name -ne "zed_terminal" -or
        $versionInfo.version -ne $summary.version -or
        $manifest.version_info.version -ne $summary.version -or
        $manifest.version_info.app_name -ne $versionInfo.app_name -or
        $manifest.version_info.binary_name -ne $versionInfo.binary_name -or
        $manifest.version_info.package_name -ne $versionInfo.package_name -or
        $manifest.version_info.target_os -ne $versionInfo.target_os -or
        $manifest.version_info.target_arch -ne $versionInfo.target_arch
    ) {
        throw "package smoke version-info metadata did not match the validated manifest"
    }
    if ([int64]$summary.content_count -ne @($manifest.contents).Count) {
        throw "package smoke summary content count did not match the validated manifest"
    }
    Assert-PackageConfigTemplateSchemas -ConfigTemplateDir $configTemplateDir

    return [pscustomobject]@{
        status = "ok"
        package_dir = $packageDir
        package_name = [string]$summary.package_name
        version = [string]$summary.version
        build_profile = [string]$summary.build_profile
        platform = [string]$summary.platform
        architecture = [string]$summary.architecture
        git_commit = [string]$summary.git_commit
        source_control = $summary.source_control
        manifest_file = $manifestFile
        readme_file = $readmeFile
        config_template_dir = $configTemplateDir
        binary = $packageBinary
        binary_sha256 = $binaryHash
        version_info = $versionInfo
        zip_file = $zipFile
        zip_sha256 = $zipHash
        zip_checksum_file = $zipChecksumFile
        summary_file = [System.IO.Path]::GetFullPath($SummaryFile)
        content_count = [int64]$summary.content_count
        validation = $summary.validation
    }
}

function Convert-VisualSmokeOutput {
    param(
        [Parameter(Mandatory = $true)][object[]]$Output,
        [Parameter(Mandatory = $true)][ValidateSet("default", "split")][string]$Mode,
        [Parameter(Mandatory = $true)][bool]$BaselineExpected
    )

    $values = Convert-KeyValueOutput -Output $Output

    $requiredKeys = @(
        "status",
        "binary",
        "process_id",
        "window_handle",
        "window_title",
        "window_bounds",
        "window_client_region",
        "probe_ready_file",
        "capture_method",
        "screenshot_file",
        "screenshot_bytes",
        "client_screenshot_file",
        "client_screenshot_bytes",
        "comparison_region",
        "comparison_top_inset",
        "comparison_client_region",
        "comparison_screenshot_file",
        "comparison_screenshot_bytes",
        "sampled_pixels",
        "sampled_unique_colors"
    )
    foreach ($key in $requiredKeys) {
        $null = Get-RequiredOutputValue -Values $values -Key $key -Context "$Mode visual smoke"
    }

    if ($values["status"] -ne "ok") {
        throw "$Mode visual smoke did not report ok status"
    }
    if ($values["window_title"] -ne "Zed Terminal") {
        throw "$Mode visual smoke reported an unexpected window title: $($values["window_title"])"
    }
    if ($values["capture_method"] -ne "PrintWindow(PW_RENDERFULLCONTENT)") {
        throw "$Mode visual smoke reported an unexpected capture method: $($values["capture_method"])"
    }

    $processId = Convert-OutputInt64 -Value $values["process_id"] -Name "$Mode visual smoke process_id"
    $windowHandle = Convert-OutputInt64 -Value $values["window_handle"] -Name "$Mode visual smoke window_handle"
    $screenshotBytes = Convert-OutputInt64 -Value $values["screenshot_bytes"] -Name "$Mode visual smoke screenshot_bytes"
    $clientScreenshotBytes = Convert-OutputInt64 -Value $values["client_screenshot_bytes"] -Name "$Mode visual smoke client_screenshot_bytes"
    $comparisonTopInset = Convert-OutputInt64 -Value $values["comparison_top_inset"] -Name "$Mode visual smoke comparison_top_inset"
    $comparisonScreenshotBytes = Convert-OutputInt64 -Value $values["comparison_screenshot_bytes"] -Name "$Mode visual smoke comparison_screenshot_bytes"
    $sampledPixels = Convert-OutputInt64 -Value $values["sampled_pixels"] -Name "$Mode visual smoke sampled_pixels"
    $sampledUniqueColors = Convert-OutputInt64 -Value $values["sampled_unique_colors"] -Name "$Mode visual smoke sampled_unique_colors"

    if ($processId -le 0 -or $windowHandle -le 0) {
        throw "$Mode visual smoke did not report a valid process/window handle"
    }
    if ($sampledPixels -le 0 -or $sampledUniqueColors -lt 8) {
        throw "$Mode visual smoke comparison image appears blank or undersampled"
    }

    $probeReadyFile = [System.IO.Path]::GetFullPath($values["probe_ready_file"])
    if (-not (Test-Path -LiteralPath $probeReadyFile -PathType Leaf)) {
        throw "$Mode visual smoke probe readiness file was missing: $probeReadyFile"
    }

    $screenshotFile = [System.IO.Path]::GetFullPath($values["screenshot_file"])
    $clientScreenshotFile = [System.IO.Path]::GetFullPath($values["client_screenshot_file"])
    $comparisonScreenshotFile = [System.IO.Path]::GetFullPath($values["comparison_screenshot_file"])
    Assert-VisualSmokeFile -Path $screenshotFile -ExpectedBytes $screenshotBytes -Name "window screenshot"
    Assert-VisualSmokeFile -Path $clientScreenshotFile -ExpectedBytes $clientScreenshotBytes -Name "client screenshot"
    Assert-VisualSmokeFile -Path $comparisonScreenshotFile -ExpectedBytes $comparisonScreenshotBytes -Name "comparison screenshot"

    $startupConfigFile = $null
    $splitMode = $null
    $splitDirection = $null
    $splitReadyFile = $null
    $splitPaneVerified = $null
    if ($Mode -eq "split") {
        foreach ($key in @("startup_config_file", "split_mode", "split_direction", "split_ready_file", "split_pane_verified")) {
            $null = Get-RequiredOutputValue -Values $values -Key $key -Context "split visual smoke"
        }
        if ($values["split_mode"] -ne "startup" -or $values["split_direction"] -ne "right") {
            throw "split visual smoke reported unexpected split metadata"
        }
        if ($values["split_pane_verified"] -ne "True") {
            throw "split visual smoke did not verify the split pane"
        }

        $startupConfigFile = [System.IO.Path]::GetFullPath($values["startup_config_file"])
        $splitReadyFile = [System.IO.Path]::GetFullPath($values["split_ready_file"])
        foreach ($file in @($startupConfigFile, $splitReadyFile)) {
            if (-not (Test-Path -LiteralPath $file -PathType Leaf)) {
                throw "split visual smoke referenced a missing file: $file"
            }
        }
        $splitMode = $values["split_mode"]
        $splitDirection = $values["split_direction"]
        $splitPaneVerified = $true
    }

    $baseline = $null
    if ($BaselineExpected) {
        $baselineKeys = @(
            "baseline_file",
            "baseline_comparison_file",
            "baseline_diff_file",
            "baseline_pixels",
            "baseline_different_pixels",
            "baseline_different_pixel_ratio",
            "baseline_average_channel_delta",
            "baseline_max_channel_delta",
            "baseline_pixel_tolerance",
            "baseline_max_different_pixel_ratio",
            "baseline_max_average_channel_delta"
        )
        foreach ($key in $baselineKeys) {
            $null = Get-RequiredOutputValue -Values $values -Key $key -Context "$Mode visual smoke baseline"
        }

        $baselineFile = [System.IO.Path]::GetFullPath($values["baseline_file"])
        $baselineComparisonFile = [System.IO.Path]::GetFullPath($values["baseline_comparison_file"])
        $baselineDiffFile = [System.IO.Path]::GetFullPath($values["baseline_diff_file"])
        foreach ($file in @($baselineFile, $baselineComparisonFile, $baselineDiffFile)) {
            if (-not (Test-Path -LiteralPath $file -PathType Leaf)) {
                throw "$Mode visual smoke baseline referenced a missing file: $file"
            }
        }

        $baselinePixels = Convert-OutputInt64 -Value $values["baseline_pixels"] -Name "$Mode visual smoke baseline_pixels"
        $baselineDifferentPixels = Convert-OutputInt64 -Value $values["baseline_different_pixels"] -Name "$Mode visual smoke baseline_different_pixels"
        $baselineDifferentPixelRatio = Convert-OutputDouble -Value $values["baseline_different_pixel_ratio"] -Name "$Mode visual smoke baseline_different_pixel_ratio"
        $baselineAverageChannelDelta = Convert-OutputDouble -Value $values["baseline_average_channel_delta"] -Name "$Mode visual smoke baseline_average_channel_delta"
        $baselineMaxChannelDelta = Convert-OutputInt64 -Value $values["baseline_max_channel_delta"] -Name "$Mode visual smoke baseline_max_channel_delta"
        $baselinePixelToleranceValue = Convert-OutputInt64 -Value $values["baseline_pixel_tolerance"] -Name "$Mode visual smoke baseline_pixel_tolerance"
        $baselineMaxDifferentPixelRatio = Convert-OutputDouble -Value $values["baseline_max_different_pixel_ratio"] -Name "$Mode visual smoke baseline_max_different_pixel_ratio"
        $baselineMaxAverageChannelDelta = Convert-OutputDouble -Value $values["baseline_max_average_channel_delta"] -Name "$Mode visual smoke baseline_max_average_channel_delta"

        if ($baselinePixels -le 0 -or $baselineDifferentPixels -lt 0 -or $baselineDifferentPixels -gt $baselinePixels) {
            throw "$Mode visual smoke baseline pixel counts were invalid"
        }
        if ($baselineDifferentPixelRatio -gt $baselineMaxDifferentPixelRatio -or $baselineAverageChannelDelta -gt $baselineMaxAverageChannelDelta) {
            throw "$Mode visual smoke baseline metrics exceeded release thresholds"
        }
        if ($baselinePixelToleranceValue -ne $BaselinePixelTolerance) {
            throw "$Mode visual smoke baseline pixel tolerance did not match the release check threshold"
        }

        $baseline = [pscustomobject]@{
            file = $baselineFile
            comparison_file = $baselineComparisonFile
            diff_file = $baselineDiffFile
            pixels = $baselinePixels
            different_pixels = $baselineDifferentPixels
            different_pixel_ratio = $baselineDifferentPixelRatio
            average_channel_delta = $baselineAverageChannelDelta
            max_channel_delta = $baselineMaxChannelDelta
            pixel_tolerance = $baselinePixelToleranceValue
            max_different_pixel_ratio = $baselineMaxDifferentPixelRatio
            max_average_channel_delta = $baselineMaxAverageChannelDelta
        }
    }

    return [pscustomobject]@{
        status = "ok"
        mode = $Mode
        binary = [System.IO.Path]::GetFullPath($values["binary"])
        process_id = $processId
        window_handle = $windowHandle
        window_title = $values["window_title"]
        window_bounds = $values["window_bounds"]
        window_client_region = $values["window_client_region"]
        probe_ready_file = $probeReadyFile
        startup_config_file = $startupConfigFile
        split_mode = $splitMode
        split_direction = $splitDirection
        split_ready_file = $splitReadyFile
        split_pane_verified = $splitPaneVerified
        capture_method = $values["capture_method"]
        screenshot_file = $screenshotFile
        screenshot_bytes = $screenshotBytes
        client_screenshot_file = $clientScreenshotFile
        client_screenshot_bytes = $clientScreenshotBytes
        comparison_region = $values["comparison_region"]
        comparison_top_inset = $comparisonTopInset
        comparison_client_region = $values["comparison_client_region"]
        comparison_screenshot_file = $comparisonScreenshotFile
        comparison_screenshot_bytes = $comparisonScreenshotBytes
        sampled_pixels = $sampledPixels
        sampled_unique_colors = $sampledUniqueColors
        baseline = $baseline
    }
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

function Get-SkippedReleaseChecks {
    $skipped = New-Object System.Collections.Generic.List[string]
    if ($SkipCargo) {
        $skipped.Add("cargo fmt")
        $skipped.Add("cargo test")
        $skipped.Add("cargo check")
        $skipped.Add("cargo build")
    } elseif ($SkipRustTests) {
        $skipped.Add("cargo test")
    }
    if ($SkipPackage) {
        $skipped.Add("package smoke")
    }
    if ($SkipCliDiagnostics) {
        $skipped.Add("CLI diagnostics")
    }
    if ($SkipVisualSmoke) {
        $skipped.Add("default visual smoke")
        $skipped.Add("split visual smoke")
    } else {
        if ($SkipVisualBaseline) {
            $skipped.Add("default visual baseline")
            $skipped.Add("split visual baseline")
        } else {
            if (-not $VisualBaselineImage) {
                $skipped.Add("default visual baseline")
            }
            if ($SkipSplitVisualSmoke) {
                $skipped.Add("split visual smoke")
            } elseif (-not $SplitVisualBaselineImage) {
                $skipped.Add("split visual baseline")
            }
        }
    }

    return @($skipped)
}

function Get-ReleaseBlockers {
    $blockers = New-Object System.Collections.Generic.List[string]
    $sourceControl = $script:SourceControl
    if (-not $sourceControl -or $sourceControl.git_available -ne $true) {
        $blockers.Add("source control metadata unavailable")
    } elseif ($sourceControl.git_dirty) {
        $blockers.Add("source tree has uncommitted changes")
    }

    if (
        $script:PackageSmoke -and
        $script:PackageSmoke.source_control -and
        $sourceControl -and
        $sourceControl.git_available -eq $true -and
        $script:PackageSmoke.source_control.git_available -eq $true -and
        $script:PackageSmoke.source_control.git_commit -ne $sourceControl.git_commit
    ) {
        $blockers.Add("package smoke commit does not match release check commit")
    }

    return @($blockers)
}

function Format-MarkdownValue {
    param([Parameter()][object]$Value)

    if ($null -eq $Value) {
        return ""
    }

    return ([string]$Value).Replace([string]"|", [string]"\|").Replace([string]"`r", [string]" ").Replace([string]"`n", [string]" ")
}

function New-ReleaseReportMarkdown {
    $Summary = $script:ReleaseSummaryPayload

    $lines = @()
    $lines += "# Zed Terminal Release Check"
    $lines += ""
    $lines += "| Field | Value |"
    $lines += "| --- | --- |"
    $lines += "| Status | $(Format-MarkdownValue $Summary.status) |"
    $lines += "| Release ready | $(Format-MarkdownValue $Summary.release_ready) |"
    $lines += "| Release mode | $(Format-MarkdownValue $Summary.release_mode) |"
    $lines += "| Run directory | $(Format-MarkdownValue $Summary.run_dir) |"
    $lines += "| Binary | $(Format-MarkdownValue $Summary.binary) |"
    $lines += "| Log file | $(Format-MarkdownValue $Summary.log_file) |"
    $lines += "| Summary file | $(Format-MarkdownValue $Summary.summary_file) |"
    $lines += "| Report file | $(Format-MarkdownValue $Summary.report_file) |"
    $lines += ""

    $lines += "## Source Control"
    $lines += ""
    $lines += "| Field | Value |"
    $lines += "| --- | --- |"
    if ($Summary.source_control) {
        $lines += "| Git available | $(Format-MarkdownValue $Summary.source_control.git_available) |"
        $lines += "| Commit | $(Format-MarkdownValue $Summary.source_control.git_commit) |"
        $lines += "| Branch | $(Format-MarkdownValue $Summary.source_control.git_branch) |"
        $lines += "| Dirty | $(Format-MarkdownValue $Summary.source_control.git_dirty) |"
        $lines += "| Status entries | $(Format-MarkdownValue $Summary.source_control.git_status_entry_count) |"
    }
    $lines += ""

    $lines += "## Skipped Release Checks"
    if ($null -eq $Summary.skipped_release_checks -or $Summary.skipped_release_checks.Count -eq 0) {
        $lines += ""
        $lines += "None."
    } else {
        foreach ($check in $Summary.skipped_release_checks) {
            $lines += "- $(Format-MarkdownValue $check)"
        }
    }
    $lines += ""

    $lines += "## Release Blockers"
    if ($null -eq $Summary.release_blockers -or $Summary.release_blockers.Count -eq 0) {
        $lines += ""
        $lines += "None."
    } else {
        foreach ($blocker in $Summary.release_blockers) {
            $lines += "- $(Format-MarkdownValue $blocker)"
        }
    }
    $lines += ""

    $lines += "## Package"
    if ($Summary.package_smoke) {
        $package = $Summary.package_smoke
        $lines += "| Field | Value |"
        $lines += "| --- | --- |"
        $lines += "| Status | $(Format-MarkdownValue $package.status) |"
        $lines += "| Package | $(Format-MarkdownValue $package.package_name) |"
        $lines += "| Version | $(Format-MarkdownValue $package.version) |"
        $lines += "| Build profile | $(Format-MarkdownValue $package.build_profile) |"
        if ($package.source_control) {
            $lines += "| Git commit | $(Format-MarkdownValue $package.source_control.git_commit) |"
            $lines += "| Git branch | $(Format-MarkdownValue $package.source_control.git_branch) |"
            $lines += "| Git dirty | $(Format-MarkdownValue $package.source_control.git_dirty) |"
        }
        if ($package.version_info) {
            $lines += "| Target | $(Format-MarkdownValue "$($package.version_info.target_os)-$($package.version_info.target_arch)") |"
            $lines += "| Debug assertions | $(Format-MarkdownValue $package.version_info.debug_assertions) |"
        }
        $lines += "| Manifest | $(Format-MarkdownValue $package.manifest_file) |"
        $lines += "| Package summary | $(Format-MarkdownValue $package.summary_file) |"
        $lines += "| Zip | $(Format-MarkdownValue $package.zip_file) |"
        $lines += "| Zip SHA256 | $(Format-MarkdownValue $package.zip_sha256) |"
        $lines += "| Zip checksum | $(Format-MarkdownValue $package.zip_checksum_file) |"
        $lines += "| Content count | $(Format-MarkdownValue $package.content_count) |"
    } else {
        $lines += ""
        $lines += "Package smoke was not run."
    }
    $lines += ""

    $lines += "## Visual Smoke"
    foreach ($visual in @($Summary.visual_smoke, $Summary.split_visual_smoke)) {
        if (-not $visual) {
            continue
        }

        $lines += "### $(Format-MarkdownValue $visual.mode)"
        $lines += ""
        $lines += "| Field | Value |"
        $lines += "| --- | --- |"
        $lines += "| Status | $(Format-MarkdownValue $visual.status) |"
        $lines += "| Window title | $(Format-MarkdownValue $visual.window_title) |"
        $lines += "| Screenshot | $(Format-MarkdownValue $visual.screenshot_file) |"
        $lines += "| Comparison screenshot | $(Format-MarkdownValue $visual.comparison_screenshot_file) |"
        $lines += "| Sampled colors | $(Format-MarkdownValue $visual.sampled_unique_colors) |"
        if ($visual.split_pane_verified -ne $null) {
            $lines += "| Split verified | $(Format-MarkdownValue $visual.split_pane_verified) |"
            $lines += "| Split direction | $(Format-MarkdownValue $visual.split_direction) |"
        }
        if ($visual.baseline) {
            $lines += "| Baseline | $(Format-MarkdownValue $visual.baseline.file) |"
            $lines += "| Baseline diff | $(Format-MarkdownValue $visual.baseline.diff_file) |"
            $lines += "| Different pixel ratio | $(Format-MarkdownValue $visual.baseline.different_pixel_ratio) |"
            $lines += "| Average channel delta | $(Format-MarkdownValue $visual.baseline.average_channel_delta) |"
        }
        $lines += ""
    }
    if (-not $Summary.visual_smoke -and -not $Summary.split_visual_smoke) {
        $lines += "Visual smoke was not run."
        $lines += ""
    }

    $lines += "## Steps"
    $lines += ""
    $lines += "| Step | Status | Seconds |"
    $lines += "| --- | --- | ---: |"
    if ($Summary.steps) {
        foreach ($step in $Summary.steps) {
            $lines += "| $(Format-MarkdownValue $step.name) | $(Format-MarkdownValue $step.status) | $(Format-MarkdownValue $step.seconds) |"
        }
    }

    return ($lines -join [Environment]::NewLine) + [Environment]::NewLine
}

function Write-ReleaseSummary {
    param([Parameter(Mandatory = $true)][string]$Status)

    $skippedReleaseChecks = @(Get-SkippedReleaseChecks)
    $releaseBlockers = @(Get-ReleaseBlockers)
    $releaseReady = $Status -eq "ok" -and $skippedReleaseChecks.Count -eq 0 -and $releaseBlockers.Count -eq 0
    $payload = [pscustomobject]@{
        status = $Status
        release_ready = $releaseReady
        release_mode = if ($releaseReady) { "full" } else { "partial" }
        source_control = $script:SourceControl
        skip_options = [pscustomobject]@{
            cargo = [bool]$SkipCargo
            rust_tests = [bool]$SkipRustTests
            cli_diagnostics = [bool]$SkipCliDiagnostics
            package = [bool]$SkipPackage
            visual_smoke = [bool]$SkipVisualSmoke
            visual_baseline = [bool]$SkipVisualBaseline
            split_visual_smoke = [bool]$SkipSplitVisualSmoke
        }
        skipped_release_checks = $skippedReleaseChecks
        release_blockers = $releaseBlockers
        run_dir = $runDir
        binary = $Binary
        log_file = $releaseLog
        summary_file = $summaryFile
        report_file = $reportFile
        visual_baseline_image = $VisualBaselineImage
        split_visual_baseline_image = $SplitVisualBaselineImage
        package_smoke_skipped = [bool]$SkipPackage
        package_smoke = $script:PackageSmoke
        visual_baseline_skipped = [bool]$SkipVisualBaseline
        visual_smoke = $script:VisualSmoke
        split_visual_smoke = $script:SplitVisualSmoke
        baseline_pixel_tolerance = $BaselinePixelTolerance
        baseline_max_different_pixel_ratio = $MaxBaselineDifferentPixelRatio
        baseline_max_average_channel_delta = $MaxBaselineAverageChannelDelta
        steps = $script:StepResults
    }
    $payload | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath $summaryFile -Encoding utf8
    $script:ReleaseSummaryPayload = $payload
    New-ReleaseReportMarkdown | Set-Content -LiteralPath $reportFile -Encoding utf8
}

try {
    $script:SourceControl = Get-GitSourceInfo
    Write-Host "zed-terminal release check"
    Write-Host "repo_root: $repoRoot"
    Write-Host "run_dir: $runDir"
    Write-Host "binary: $Binary"
    if ($script:SourceControl.git_available) {
        Write-Host "git_commit: $($script:SourceControl.git_commit)"
        Write-Host "git_branch: $($script:SourceControl.git_branch)"
        Write-Host "git_dirty: $($script:SourceControl.git_dirty)"
        Write-Host "git_status_entry_count: $($script:SourceControl.git_status_entry_count)"
    } else {
        Write-Host "git_source: unavailable"
    }
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

    Invoke-Step "PowerShell syntax: package" {
        Assert-PowerShellSyntax (Join-Path $repoRoot "script\zed-terminal-package.ps1")
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
            $previousRustFlags = $env:RUSTFLAGS
            try {
                if ([string]::IsNullOrWhiteSpace($previousRustFlags)) {
                    $env:RUSTFLAGS = "-D warnings"
                } else {
                    $env:RUSTFLAGS = "$previousRustFlags -D warnings"
                }
                Invoke-NativeCommand -FilePath "cargo" -Arguments @("+stable", "check", "-p", "zed_terminal")
            } finally {
                $env:RUSTFLAGS = $previousRustFlags
            }
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

    if (-not $SkipPackage) {
        Invoke-Step "package smoke" {
            $packageSummaryFile = Join-Path $packageSmokeDir "zed-terminal-package-summary.json"
            Invoke-NativeCommand -FilePath "powershell" -Arguments @(
                "-NoProfile",
                "-ExecutionPolicy", "Bypass",
                "-File", (Join-Path $repoRoot "script\zed-terminal-package.ps1"),
                "-Binary", $Binary,
                "-BuildProfile", "debug",
                "-SkipBuild",
                "-Zip",
                "-OutputDir", $packageSmokeDir,
                "-SummaryFile", $packageSummaryFile
            )
            $script:PackageSmoke = Read-PackageSmokeSummary -SummaryFile $packageSummaryFile
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
                "Config bundle backup and restore options:",
                "--backup-config-bundle --backup-config-bundle-file <FILE>",
                "--backup-config-bundle-format <text\|json>",
                "--check-config-bundle --check-config-bundle-file <FILE>",
                "--check-config-bundle-format <text\|json>",
                "--diff-config-bundle --diff-config-bundle-file <FILE>",
                "--diff-config-bundle-format <text\|json>",
                "--restore-config-bundle --restore-config-bundle-file <FILE>",
                "--restore-config-bundle-format <text\|json>",
                "Support bundle options:",
                "--support-bundle --support-bundle-dir <DIR>",
                "--support-bundle-format <text\|json>",
                "Startup config backup and restore options:",
                "--backup-startup-config --backup-startup-config-file <FILE>",
                "--backup-startup-config-format <text\|json>",
                "--check-startup-config-backup --check-startup-config-backup-file <FILE>",
                "--check-startup-config-backup-format <text\|json>",
                "--diff-startup-config-backup --diff-startup-config-backup-file <FILE>",
                "--diff-startup-config-backup-format <text\|json>",
                "--restore-startup-config --restore-startup-config-file <FILE>",
                "--restore-startup-config-format <text\|json>",
                "--validate-settings",
                "--validate-settings-format <text\|json>",
                "--print-settings-schema",
                "Settings backup and restore options:",
                "--backup-settings --backup-settings-file <FILE>",
                "--backup-settings-format <text\|json>",
                "--check-settings-backup --check-settings-backup-file <FILE>",
                "--check-settings-backup-format <text\|json>",
                "--diff-settings-backup --diff-settings-backup-file <FILE>",
                "--diff-settings-backup-format <text\|json>",
                "--restore-settings --restore-settings-file <FILE>",
                "--restore-settings-format <text\|json>",
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
                "--list-keymap-actions",
                "--list-keymap-actions-format <text\|json>",
                "--describe-keymap-action <ACTION>",
                "--describe-keymap-action-format <text\|json>",
                "--describe-keymap-binding <KEYSTROKES>",
                "--describe-keymap-binding-format <text\|json>",
                "--describe-active-keymap-binding <KEYSTROKES>",
                "--describe-active-keymap-binding-context <CONTEXT>",
                "--describe-active-keymap-binding-format <text\|json>",
                "--list-active-keymap-bindings",
                "--list-active-keymap-bindings-context <CONTEXT>",
                "--list-active-keymap-bindings-format <text\|json>",
                "--version-info",
                "--paths",
                "--paths-format <text\|json>",
                "--portable",
                "Use config and data directories next to the zed-terminal binary",
                "Profile transfer, startup config file, keymap file, version metadata, and path inspection options may be combined with --user-data-dir",
                "and --config-dir only."
            )
            $versionInfoJson = Invoke-NativeJsonCommandResult "version-info" @(
                "--version-info",
                "--version-info-format", "json"
            )
            if (
                $versionInfoJson.app_name -ne "Zed Terminal" -or
                $versionInfoJson.binary_name -ne "zed-terminal" -or
                $versionInfoJson.package_name -ne "zed_terminal" -or
                -not $versionInfoJson.version -or
                -not $versionInfoJson.target_os -or
                -not $versionInfoJson.target_arch -or
                $null -eq $versionInfoJson.debug_assertions
            ) {
                throw "zed-terminal --version-info did not report expected metadata"
            }
            $initConfig = Invoke-NativeJsonCommandResult "init-config" @(
                "--user-data-dir", $cliDataDir,
                "--config-dir", $cliConfigDir,
                "--init-config",
                "--init-config-format", "json"
            )
            Assert-ConfigInitializationJson `
                -Report $initConfig `
                -ConfigDir $cliConfigDir
            $pathsJson = Invoke-NativeJsonCommandResult "paths" @(
                "--user-data-dir", $cliDataDir,
                "--config-dir", $cliConfigDir,
                "--paths",
                "--paths-format", "json"
            )
            if (
                $pathsJson.mode -ne "custom" -or
                $pathsJson.data_dir -ne $cliDataDir -or
                $pathsJson.config_dir -ne $cliConfigDir -or
                $pathsJson.settings_file -ne (Join-Path $cliConfigDir "settings.json") -or
                $pathsJson.settings_schema_file -ne (Join-Path $cliConfigDir "settings.schema.json") -or
                $pathsJson.startup_config_schema_file -ne (Join-Path $cliConfigDir "terminal.schema.json") -or
                $pathsJson.keymap_schema_file -ne (Join-Path $cliConfigDir "keymap.schema.json")
            ) {
                throw "zed-terminal --paths did not report expected config discovery paths"
            }
            $portablePaths = Invoke-NativeJsonCommandResult "portable-paths" @(
                "--portable",
                "--paths",
                "--paths-format", "json"
            )
            $expectedPortableDataDir = Join-Path (Split-Path -Parent $Binary) "data"
            $expectedPortableConfigDir = Join-Path (Split-Path -Parent $Binary) "config"
            if (
                $portablePaths.mode -ne "portable" -or
                $portablePaths.data_dir -ne $expectedPortableDataDir -or
                $portablePaths.config_dir -ne $expectedPortableConfigDir -or
                $portablePaths.logs_dir -ne (Join-Path $expectedPortableDataDir "logs") -or
                $portablePaths.settings_schema_file -ne (Join-Path $expectedPortableConfigDir "settings.schema.json")
            ) {
                throw "zed-terminal --portable --paths did not report expected binary-local paths"
            }
            $doctorJson = Invoke-NativeJsonCommandResult "doctor" @(
                "--user-data-dir", $cliDataDir,
                "--config-dir", $cliConfigDir,
                "--doctor",
                "--doctor-format", "json"
            )
            if ($doctorJson.settings.status -ne "ok" -or @($doctorJson.settings.files).Count -ne 2) {
                throw "zed-terminal --doctor did not include expected settings validation diagnostics"
            }
            $doctorConfigLabels = @($doctorJson.config_files | ForEach-Object { $_.label })
            if ($doctorConfigLabels -notcontains "settings_schema_file") {
                throw "zed-terminal --doctor did not report settings schema file diagnostics"
            }
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
            $supportBundleDir = Join-Path $runDir "support-bundle"
            Add-Content -LiteralPath (Join-Path $cliConfigDir "terminal.json") -Value "`n// do-not-log-release-support-startup-secret"
            Add-Content -LiteralPath (Join-Path $cliConfigDir "settings.json") -Value "`n// do-not-log-release-support-settings-secret"
            New-Item -ItemType Directory -Force -Path (Join-Path $cliDataDir "logs") | Out-Null
            Set-Content -LiteralPath (Join-Path (Join-Path $cliDataDir "logs") "Zed Terminal.log") -Value "do-not-log-release-support-log-secret" -Encoding utf8
            $supportBundleJson = Invoke-NativeJsonCommandResult "support-bundle" @(
                "--user-data-dir", $cliDataDir,
                "--config-dir", $cliConfigDir,
                "--support-bundle",
                "--support-bundle-dir", $supportBundleDir,
                "--support-bundle-format", "json"
            )
            Assert-SupportBundleArtifacts `
                -Report $supportBundleJson `
                -BundleDir $supportBundleDir `
                -DataDir $cliDataDir `
                -ConfigDir $cliConfigDir `
                -SensitiveText @(
                    "do-not-log-release-support-startup-secret",
                    "do-not-log-release-support-settings-secret",
                    "do-not-log-release-support-log-secret"
                )
            Invoke-NativeJsonCommand "validate-keymap" @(
                "--user-data-dir", $cliDataDir,
                "--config-dir", $cliConfigDir,
                "--validate-keymap",
                "--validate-keymap-format", "json"
            )
            $settingsValidation = Invoke-NativeJsonCommandResult "validate-settings" @(
                "--user-data-dir", $cliDataDir,
                "--config-dir", $cliConfigDir,
                "--validate-settings",
                "--validate-settings-format", "json"
            )
            Assert-SettingsValidationJson $settingsValidation
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
            $settingsSchema = Invoke-NativeJsonCommandResult "print-settings-schema" @(
                "--user-data-dir", $cliDataDir,
                "--config-dir", $cliConfigDir,
                "--print-settings-schema"
            )
            if ($settingsSchema.title -ne "UserSettingsContent" -or $settingsSchema.type -ne "object") {
                throw "Settings schema did not report the UserSettingsContent object contract."
            }
            if (-not $settingsSchema.properties.PSObject.Properties["theme"]) {
                throw "Settings schema is missing expected property 'theme'."
            }
            $settingsSchemaText = $settingsSchema | ConvertTo-Json -Depth 100
            foreach ($actionName in @(
                "zed_terminal::OpenSettingsSchemaFile",
                "zed_terminal::OpenSettingsToolsPicker",
                "zed_terminal::OpenStartupConfigSchemaFile",
                "zed_terminal::OpenKeymapSchemaFile",
                "zed_terminal::NewTerminalTab"
            )) {
                if ($settingsSchemaText -notmatch [regex]::Escape($actionName)) {
                    throw "Settings schema is missing expected action '$actionName'."
                }
            }
            if ($settingsSchemaText -match [regex]::Escape("workspace::NewFile")) {
                throw "Settings schema exposed a non-terminal action."
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
            foreach ($actionName in @(
                "zed_terminal::NewTerminalTab",
                "zed_terminal::NewTerminalTabWithProfile",
                "zed_terminal::NewTerminalTabWithProfileSlot",
                "zed_terminal::NewTerminalWindowWithProfileSlot",
                "zed_terminal::NewTerminalSplitWithProfileSlot",
                "zed_terminal::OpenConfigBundleBackupDirectory",
                "zed_terminal::OpenConfigBundleBackupsDirectory",
                "zed_terminal::OpenConfigInitializationReport",
                "zed_terminal::OpenKeymapToolsPicker",
                "zed_terminal::OpenSettingsSchemaFile",
                "zed_terminal::OpenSettingsToolsPicker",
                "zed_terminal::OpenStartupProfileConfig",
                "zed_terminal::OpenStartupProfilePicker",
                "zed_terminal::OpenSupportToolsPicker",
                "zed_terminal::OpenStartupToolsPicker",
                "zed_terminal::OpenActiveKeymapBindingsReport",
                "zed_terminal::OpenStartupLayoutReport",
                "zed_terminal::OpenPathsReport",
                "zed_terminal::OpenVersionInfoReport",
                "terminal::Paste",
                "pane::CloseActiveItem"
            )) {
                if ($keymapSchemaText -notmatch [regex]::Escape($actionName)) {
                    throw "Keymap schema is missing expected action '$actionName'."
                }
            }
            $keymapActions = Invoke-NativeJsonCommandResult "list-keymap-actions" @(
                "--user-data-dir", $cliDataDir,
                "--config-dir", $cliConfigDir,
                "--list-keymap-actions",
                "--list-keymap-actions-format", "json"
            )
            if ($keymapActions.status -ne "ok" -or $keymapActions.default_keymap -ne "keymaps/zed-terminal.json" -or $keymapActions.action_count -lt 4) {
                throw "Keymap action list did not report the expected action catalog contract."
            }
            $newTabAction = $keymapActions.actions | Where-Object { $_.name -eq "zed_terminal::NewTerminalTab" } | Select-Object -First 1
            if (-not $newTabAction -or $newTabAction.namespace -ne "zed_terminal" -or $newTabAction.input -ne "none") {
                throw "Keymap action list is missing the NewTerminalTab action metadata."
            }
            if (-not ($newTabAction.default_bindings | Where-Object { $_.keystrokes -eq "ctrl-shift-T" -and $null -eq $_.context })) {
                throw "Keymap action list is missing the NewTerminalTab default binding."
            }
            $profileTabAction = $keymapActions.actions | Where-Object { $_.name -eq "zed_terminal::NewTerminalTabWithProfile" } | Select-Object -First 1
            if (-not $profileTabAction -or $profileTabAction.input -ne "object") {
                throw "Keymap action list did not mark NewTerminalTabWithProfile as an object-input action."
            }
            $profileSlotTabAction = $keymapActions.actions | Where-Object { $_.name -eq "zed_terminal::NewTerminalTabWithProfileSlot" } | Select-Object -First 1
            if (-not $profileSlotTabAction -or $profileSlotTabAction.namespace -ne "zed_terminal" -or $profileSlotTabAction.input -ne "object") {
                throw "Keymap action list did not mark NewTerminalTabWithProfileSlot as an object-input action."
            }
            if (-not ($profileSlotTabAction.default_bindings | Where-Object { $_.keystrokes -eq "ctrl-shift-1" -and $null -eq $_.context -and $_.input -eq '{"slot":1}' })) {
                throw "Keymap action list is missing the profile slot 1 default binding."
            }
            if (-not ($profileSlotTabAction.default_bindings | Where-Object { $_.keystrokes -eq "ctrl-shift-9" -and $null -eq $_.context -and $_.input -eq '{"slot":9}' })) {
                throw "Keymap action list is missing the profile slot 9 default binding."
            }
            $profileSlotSplitAction = $keymapActions.actions | Where-Object { $_.name -eq "zed_terminal::NewTerminalSplitWithProfileSlot" } | Select-Object -First 1
            if (-not $profileSlotSplitAction -or $profileSlotSplitAction.namespace -ne "zed_terminal" -or $profileSlotSplitAction.input -ne "object") {
                throw "Keymap action list did not mark NewTerminalSplitWithProfileSlot as an object-input action."
            }
            $profileSlotWindowAction = $keymapActions.actions | Where-Object { $_.name -eq "zed_terminal::NewTerminalWindowWithProfileSlot" } | Select-Object -First 1
            if (-not $profileSlotWindowAction -or $profileSlotWindowAction.namespace -ne "zed_terminal" -or $profileSlotWindowAction.input -ne "object") {
                throw "Keymap action list did not mark NewTerminalWindowWithProfileSlot as an object-input action."
            }
            $configBundleBackupAction = $keymapActions.actions | Where-Object { $_.name -eq "zed_terminal::OpenConfigBundleBackupDirectory" } | Select-Object -First 1
            if (-not $configBundleBackupAction -or $configBundleBackupAction.namespace -ne "zed_terminal" -or $configBundleBackupAction.input -ne "none") {
                throw "Keymap action list is missing the OpenConfigBundleBackupDirectory action metadata."
            }
            $configBundleBackupsAction = $keymapActions.actions | Where-Object { $_.name -eq "zed_terminal::OpenConfigBundleBackupsDirectory" } | Select-Object -First 1
            if (-not $configBundleBackupsAction -or $configBundleBackupsAction.namespace -ne "zed_terminal" -or $configBundleBackupsAction.input -ne "none") {
                throw "Keymap action list is missing the OpenConfigBundleBackupsDirectory action metadata."
            }
            $configInitializationReportAction = $keymapActions.actions | Where-Object { $_.name -eq "zed_terminal::OpenConfigInitializationReport" } | Select-Object -First 1
            if (-not $configInitializationReportAction -or $configInitializationReportAction.namespace -ne "zed_terminal" -or $configInitializationReportAction.input -ne "none") {
                throw "Keymap action list is missing the OpenConfigInitializationReport action metadata."
            }
            $profileConfigAction = $keymapActions.actions | Where-Object { $_.name -eq "zed_terminal::OpenStartupProfileConfig" } | Select-Object -First 1
            if (-not $profileConfigAction -or $profileConfigAction.namespace -ne "zed_terminal" -or $profileConfigAction.input -ne "object") {
                throw "Keymap action list did not report the expected OpenStartupProfileConfig object-input metadata."
            }
            $profilePickerAction = $keymapActions.actions | Where-Object { $_.name -eq "zed_terminal::OpenStartupProfilePicker" } | Select-Object -First 1
            if (-not $profilePickerAction -or $profilePickerAction.namespace -ne "zed_terminal" -or $profilePickerAction.input -ne "none") {
                throw "Keymap action list did not report the expected OpenStartupProfilePicker no-input metadata."
            }
            $startupToolsPickerAction = $keymapActions.actions | Where-Object { $_.name -eq "zed_terminal::OpenStartupToolsPicker" } | Select-Object -First 1
            if (-not $startupToolsPickerAction -or $startupToolsPickerAction.namespace -ne "zed_terminal" -or $startupToolsPickerAction.input -ne "none") {
                throw "Keymap action list did not report the expected OpenStartupToolsPicker no-input metadata."
            }
            $supportToolsPickerAction = $keymapActions.actions | Where-Object { $_.name -eq "zed_terminal::OpenSupportToolsPicker" } | Select-Object -First 1
            if (-not $supportToolsPickerAction -or $supportToolsPickerAction.namespace -ne "zed_terminal" -or $supportToolsPickerAction.input -ne "none") {
                throw "Keymap action list did not report the expected OpenSupportToolsPicker no-input metadata."
            }
            $settingsToolsPickerAction = $keymapActions.actions | Where-Object { $_.name -eq "zed_terminal::OpenSettingsToolsPicker" } | Select-Object -First 1
            if (-not $settingsToolsPickerAction -or $settingsToolsPickerAction.namespace -ne "zed_terminal" -or $settingsToolsPickerAction.input -ne "none") {
                throw "Keymap action list did not report the expected OpenSettingsToolsPicker no-input metadata."
            }
            $settingsSchemaFileAction = $keymapActions.actions | Where-Object { $_.name -eq "zed_terminal::OpenSettingsSchemaFile" } | Select-Object -First 1
            if (-not $settingsSchemaFileAction -or $settingsSchemaFileAction.namespace -ne "zed_terminal" -or $settingsSchemaFileAction.input -ne "none") {
                throw "Keymap action list did not report the expected OpenSettingsSchemaFile no-input metadata."
            }
            $keymapToolsPickerAction = $keymapActions.actions | Where-Object { $_.name -eq "zed_terminal::OpenKeymapToolsPicker" } | Select-Object -First 1
            if (-not $keymapToolsPickerAction -or $keymapToolsPickerAction.namespace -ne "zed_terminal" -or $keymapToolsPickerAction.input -ne "none") {
                throw "Keymap action list did not report the expected OpenKeymapToolsPicker no-input metadata."
            }
            $activeBindingsReportAction = $keymapActions.actions | Where-Object { $_.name -eq "zed_terminal::OpenActiveKeymapBindingsReport" } | Select-Object -First 1
            if (-not $activeBindingsReportAction -or $activeBindingsReportAction.namespace -ne "zed_terminal" -or $activeBindingsReportAction.input -ne "none") {
                throw "Keymap action list is missing the OpenActiveKeymapBindingsReport action metadata."
            }
            $startupLayoutReportAction = $keymapActions.actions | Where-Object { $_.name -eq "zed_terminal::OpenStartupLayoutReport" } | Select-Object -First 1
            if (-not $startupLayoutReportAction -or $startupLayoutReportAction.namespace -ne "zed_terminal" -or $startupLayoutReportAction.input -ne "none") {
                throw "Keymap action list is missing the OpenStartupLayoutReport action metadata."
            }
            $pathsReportAction = $keymapActions.actions | Where-Object { $_.name -eq "zed_terminal::OpenPathsReport" } | Select-Object -First 1
            if (-not $pathsReportAction -or $pathsReportAction.namespace -ne "zed_terminal" -or $pathsReportAction.input -ne "none") {
                throw "Keymap action list is missing the OpenPathsReport action metadata."
            }
            $versionInfoReportAction = $keymapActions.actions | Where-Object { $_.name -eq "zed_terminal::OpenVersionInfoReport" } | Select-Object -First 1
            if (-not $versionInfoReportAction -or $versionInfoReportAction.namespace -ne "zed_terminal" -or $versionInfoReportAction.input -ne "none") {
                throw "Keymap action list is missing the OpenVersionInfoReport action metadata."
            }
            $pasteAction = $keymapActions.actions | Where-Object { $_.name -eq "terminal::Paste" } | Select-Object -First 1
            if (-not ($pasteAction.default_bindings | Where-Object { $_.keystrokes -eq "ctrl-shift-V" -and $_.context -eq "Terminal" })) {
                throw "Keymap action list is missing the terminal Paste default binding."
            }
            $keymapActionsText = $keymapActions | ConvertTo-Json -Depth 20
            if ($keymapActionsText -match "do-not-log") {
                throw "Keymap action list output unexpectedly contained release fixture content."
            }
            $newTabActionDescription = Invoke-NativeJsonCommandResult "describe-keymap-action-new-tab" @(
                "--user-data-dir", $cliDataDir,
                "--config-dir", $cliConfigDir,
                "--describe-keymap-action", "zed_terminal::NewTerminalTab",
                "--describe-keymap-action-format", "json"
            )
            if ($newTabActionDescription.status -ne "ok" -or $newTabActionDescription.default_keymap -ne "keymaps/zed-terminal.json" -or $newTabActionDescription.action.name -ne "zed_terminal::NewTerminalTab" -or $newTabActionDescription.action.namespace -ne "zed_terminal" -or $newTabActionDescription.action.input -ne "none") {
                throw "Keymap action description did not report the expected NewTerminalTab metadata."
            }
            if (-not ($newTabActionDescription.action.default_bindings | Where-Object { $_.keystrokes -eq "ctrl-shift-T" -and $null -eq $_.context })) {
                throw "Keymap action description is missing the NewTerminalTab default binding."
            }
            $profileTabActionDescription = Invoke-NativeJsonCommandResult "describe-keymap-action-profile-tab" @(
                "--user-data-dir", $cliDataDir,
                "--config-dir", $cliConfigDir,
                "--describe-keymap-action", "zed_terminal::NewTerminalTabWithProfile",
                "--describe-keymap-action-format", "json"
            )
            if ($profileTabActionDescription.action.name -ne "zed_terminal::NewTerminalTabWithProfile" -or $profileTabActionDescription.action.input -ne "object") {
                throw "Keymap action description did not report the expected profile-tab input contract."
            }
            $profileSlotTabActionDescription = Invoke-NativeJsonCommandResult "describe-keymap-action-profile-slot-tab" @(
                "--user-data-dir", $cliDataDir,
                "--config-dir", $cliConfigDir,
                "--describe-keymap-action", "zed_terminal::NewTerminalTabWithProfileSlot",
                "--describe-keymap-action-format", "json"
            )
            if ($profileSlotTabActionDescription.action.name -ne "zed_terminal::NewTerminalTabWithProfileSlot" -or $profileSlotTabActionDescription.action.namespace -ne "zed_terminal" -or $profileSlotTabActionDescription.action.input -ne "object") {
                throw "Keymap action description did not report the expected profile-slot-tab input contract."
            }
            if (-not ($profileSlotTabActionDescription.action.default_bindings | Where-Object { $_.keystrokes -eq "ctrl-shift-1" -and $null -eq $_.context -and $_.input -eq '{"slot":1}' })) {
                throw "Keymap action description is missing the profile slot 1 default binding."
            }
            if (-not ($profileSlotTabActionDescription.action.default_bindings | Where-Object { $_.keystrokes -eq "ctrl-shift-9" -and $null -eq $_.context -and $_.input -eq '{"slot":9}' })) {
                throw "Keymap action description is missing the profile slot 9 default binding."
            }
            $profileSlotSplitActionDescription = Invoke-NativeJsonCommandResult "describe-keymap-action-profile-slot-split" @(
                "--user-data-dir", $cliDataDir,
                "--config-dir", $cliConfigDir,
                "--describe-keymap-action", "zed_terminal::NewTerminalSplitWithProfileSlot",
                "--describe-keymap-action-format", "json"
            )
            if ($profileSlotSplitActionDescription.action.name -ne "zed_terminal::NewTerminalSplitWithProfileSlot" -or $profileSlotSplitActionDescription.action.namespace -ne "zed_terminal" -or $profileSlotSplitActionDescription.action.input -ne "object") {
                throw "Keymap action description did not report the expected profile-slot-split input contract."
            }
            $profileSlotWindowActionDescription = Invoke-NativeJsonCommandResult "describe-keymap-action-profile-slot-window" @(
                "--user-data-dir", $cliDataDir,
                "--config-dir", $cliConfigDir,
                "--describe-keymap-action", "zed_terminal::NewTerminalWindowWithProfileSlot",
                "--describe-keymap-action-format", "json"
            )
            if ($profileSlotWindowActionDescription.action.name -ne "zed_terminal::NewTerminalWindowWithProfileSlot" -or $profileSlotWindowActionDescription.action.namespace -ne "zed_terminal" -or $profileSlotWindowActionDescription.action.input -ne "object") {
                throw "Keymap action description did not report the expected profile-slot-window input contract."
            }
            $configBundleBackupActionDescription = Invoke-NativeJsonCommandResult "describe-keymap-action-config-bundle-backup" @(
                "--user-data-dir", $cliDataDir,
                "--config-dir", $cliConfigDir,
                "--describe-keymap-action", "zed_terminal::OpenConfigBundleBackupDirectory",
                "--describe-keymap-action-format", "json"
            )
            if ($configBundleBackupActionDescription.action.name -ne "zed_terminal::OpenConfigBundleBackupDirectory" -or $configBundleBackupActionDescription.action.namespace -ne "zed_terminal" -or $configBundleBackupActionDescription.action.input -ne "none") {
                throw "Keymap action description did not report the expected config bundle backup action contract."
            }
            $configBundleBackupsActionDescription = Invoke-NativeJsonCommandResult "describe-keymap-action-config-bundle-backups" @(
                "--user-data-dir", $cliDataDir,
                "--config-dir", $cliConfigDir,
                "--describe-keymap-action", "zed_terminal::OpenConfigBundleBackupsDirectory",
                "--describe-keymap-action-format", "json"
            )
            if ($configBundleBackupsActionDescription.action.name -ne "zed_terminal::OpenConfigBundleBackupsDirectory" -or $configBundleBackupsActionDescription.action.namespace -ne "zed_terminal" -or $configBundleBackupsActionDescription.action.input -ne "none") {
                throw "Keymap action description did not report the expected config bundle backups directory action contract."
            }
            $configInitializationReportActionDescription = Invoke-NativeJsonCommandResult "describe-keymap-action-config-initialization-report" @(
                "--user-data-dir", $cliDataDir,
                "--config-dir", $cliConfigDir,
                "--describe-keymap-action", "zed_terminal::OpenConfigInitializationReport",
                "--describe-keymap-action-format", "json"
            )
            if ($configInitializationReportActionDescription.action.name -ne "zed_terminal::OpenConfigInitializationReport" -or $configInitializationReportActionDescription.action.namespace -ne "zed_terminal" -or $configInitializationReportActionDescription.action.input -ne "none") {
                throw "Keymap action description did not report the expected config initialization report action contract."
            }
            $profileConfigActionDescription = Invoke-NativeJsonCommandResult "describe-keymap-action-profile-config" @(
                "--user-data-dir", $cliDataDir,
                "--config-dir", $cliConfigDir,
                "--describe-keymap-action", "zed_terminal::OpenStartupProfileConfig",
                "--describe-keymap-action-format", "json"
            )
            if ($profileConfigActionDescription.action.name -ne "zed_terminal::OpenStartupProfileConfig" -or $profileConfigActionDescription.action.namespace -ne "zed_terminal" -or $profileConfigActionDescription.action.input -ne "object") {
                throw "Keymap action description did not report the expected profile config action contract."
            }
            $profilePickerActionDescription = Invoke-NativeJsonCommandResult "describe-keymap-action-profile-picker" @(
                "--user-data-dir", $cliDataDir,
                "--config-dir", $cliConfigDir,
                "--describe-keymap-action", "zed_terminal::OpenStartupProfilePicker",
                "--describe-keymap-action-format", "json"
            )
            if ($profilePickerActionDescription.action.name -ne "zed_terminal::OpenStartupProfilePicker" -or $profilePickerActionDescription.action.namespace -ne "zed_terminal" -or $profilePickerActionDescription.action.input -ne "none") {
                throw "Keymap action description did not report the expected profile picker action contract."
            }
            $startupToolsPickerActionDescription = Invoke-NativeJsonCommandResult "describe-keymap-action-startup-tools-picker" @(
                "--user-data-dir", $cliDataDir,
                "--config-dir", $cliConfigDir,
                "--describe-keymap-action", "zed_terminal::OpenStartupToolsPicker",
                "--describe-keymap-action-format", "json"
            )
            if ($startupToolsPickerActionDescription.action.name -ne "zed_terminal::OpenStartupToolsPicker" -or $startupToolsPickerActionDescription.action.namespace -ne "zed_terminal" -or $startupToolsPickerActionDescription.action.input -ne "none") {
                throw "Keymap action description did not report the expected startup tools picker action contract."
            }
            $supportToolsPickerActionDescription = Invoke-NativeJsonCommandResult "describe-keymap-action-support-tools-picker" @(
                "--user-data-dir", $cliDataDir,
                "--config-dir", $cliConfigDir,
                "--describe-keymap-action", "zed_terminal::OpenSupportToolsPicker",
                "--describe-keymap-action-format", "json"
            )
            if ($supportToolsPickerActionDescription.action.name -ne "zed_terminal::OpenSupportToolsPicker" -or $supportToolsPickerActionDescription.action.namespace -ne "zed_terminal" -or $supportToolsPickerActionDescription.action.input -ne "none") {
                throw "Keymap action description did not report the expected support tools picker action contract."
            }
            $settingsToolsPickerActionDescription = Invoke-NativeJsonCommandResult "describe-keymap-action-settings-tools-picker" @(
                "--user-data-dir", $cliDataDir,
                "--config-dir", $cliConfigDir,
                "--describe-keymap-action", "zed_terminal::OpenSettingsToolsPicker",
                "--describe-keymap-action-format", "json"
            )
            if ($settingsToolsPickerActionDescription.action.name -ne "zed_terminal::OpenSettingsToolsPicker" -or $settingsToolsPickerActionDescription.action.namespace -ne "zed_terminal" -or $settingsToolsPickerActionDescription.action.input -ne "none") {
                throw "Keymap action description did not report the expected settings tools picker action contract."
            }
            $settingsSchemaFileActionDescription = Invoke-NativeJsonCommandResult "describe-keymap-action-settings-schema-file" @(
                "--user-data-dir", $cliDataDir,
                "--config-dir", $cliConfigDir,
                "--describe-keymap-action", "zed_terminal::OpenSettingsSchemaFile",
                "--describe-keymap-action-format", "json"
            )
            if ($settingsSchemaFileActionDescription.action.name -ne "zed_terminal::OpenSettingsSchemaFile" -or $settingsSchemaFileActionDescription.action.namespace -ne "zed_terminal" -or $settingsSchemaFileActionDescription.action.input -ne "none") {
                throw "Keymap action description did not report the expected settings schema file action contract."
            }
            $keymapToolsPickerActionDescription = Invoke-NativeJsonCommandResult "describe-keymap-action-keymap-tools-picker" @(
                "--user-data-dir", $cliDataDir,
                "--config-dir", $cliConfigDir,
                "--describe-keymap-action", "zed_terminal::OpenKeymapToolsPicker",
                "--describe-keymap-action-format", "json"
            )
            if ($keymapToolsPickerActionDescription.action.name -ne "zed_terminal::OpenKeymapToolsPicker" -or $keymapToolsPickerActionDescription.action.namespace -ne "zed_terminal" -or $keymapToolsPickerActionDescription.action.input -ne "none") {
                throw "Keymap action description did not report the expected keymap tools picker action contract."
            }
            $activeBindingsReportActionDescription = Invoke-NativeJsonCommandResult "describe-keymap-action-active-bindings-report" @(
                "--user-data-dir", $cliDataDir,
                "--config-dir", $cliConfigDir,
                "--describe-keymap-action", "zed_terminal::OpenActiveKeymapBindingsReport",
                "--describe-keymap-action-format", "json"
            )
            if ($activeBindingsReportActionDescription.action.name -ne "zed_terminal::OpenActiveKeymapBindingsReport" -or $activeBindingsReportActionDescription.action.input -ne "none") {
                throw "Keymap action description did not report the expected active bindings report action contract."
            }
            $startupLayoutReportActionDescription = Invoke-NativeJsonCommandResult "describe-keymap-action-startup-layout-report" @(
                "--user-data-dir", $cliDataDir,
                "--config-dir", $cliConfigDir,
                "--describe-keymap-action", "zed_terminal::OpenStartupLayoutReport",
                "--describe-keymap-action-format", "json"
            )
            if ($startupLayoutReportActionDescription.action.name -ne "zed_terminal::OpenStartupLayoutReport" -or $startupLayoutReportActionDescription.action.namespace -ne "zed_terminal" -or $startupLayoutReportActionDescription.action.input -ne "none") {
                throw "Keymap action description did not report the expected startup layout report action contract."
            }
            $pathsReportActionDescription = Invoke-NativeJsonCommandResult "describe-keymap-action-paths-report" @(
                "--user-data-dir", $cliDataDir,
                "--config-dir", $cliConfigDir,
                "--describe-keymap-action", "zed_terminal::OpenPathsReport",
                "--describe-keymap-action-format", "json"
            )
            if ($pathsReportActionDescription.action.name -ne "zed_terminal::OpenPathsReport" -or $pathsReportActionDescription.action.namespace -ne "zed_terminal" -or $pathsReportActionDescription.action.input -ne "none") {
                throw "Keymap action description did not report the expected paths report action contract."
            }
            $versionInfoReportActionDescription = Invoke-NativeJsonCommandResult "describe-keymap-action-version-info-report" @(
                "--user-data-dir", $cliDataDir,
                "--config-dir", $cliConfigDir,
                "--describe-keymap-action", "zed_terminal::OpenVersionInfoReport",
                "--describe-keymap-action-format", "json"
            )
            if ($versionInfoReportActionDescription.action.name -ne "zed_terminal::OpenVersionInfoReport" -or $versionInfoReportActionDescription.action.namespace -ne "zed_terminal" -or $versionInfoReportActionDescription.action.input -ne "none") {
                throw "Keymap action description did not report the expected version info report action contract."
            }
            $pasteActionDescription = Invoke-NativeJsonCommandResult "describe-keymap-action-paste" @(
                "--user-data-dir", $cliDataDir,
                "--config-dir", $cliConfigDir,
                "--describe-keymap-action", "terminal::Paste",
                "--describe-keymap-action-format", "json"
            )
            if (-not ($pasteActionDescription.action.default_bindings | Where-Object { $_.keystrokes -eq "ctrl-shift-V" -and $_.context -eq "Terminal" })) {
                throw "Keymap action description is missing the terminal Paste default binding."
            }
            $keymapActionDescriptionText = @(
                $newTabActionDescription,
                $profileTabActionDescription,
                $profileSlotTabActionDescription,
                $profileSlotSplitActionDescription,
                $profileSlotWindowActionDescription,
                $configBundleBackupActionDescription,
                $configBundleBackupsActionDescription,
                $configInitializationReportActionDescription,
                $profileConfigActionDescription,
                $profilePickerActionDescription,
                $startupToolsPickerActionDescription,
                $supportToolsPickerActionDescription,
                $settingsToolsPickerActionDescription,
                $keymapToolsPickerActionDescription,
                $activeBindingsReportActionDescription,
                $startupLayoutReportActionDescription,
                $pathsReportActionDescription,
                $versionInfoReportActionDescription,
                $pasteActionDescription
            ) | ConvertTo-Json -Depth 20
            if ($keymapActionDescriptionText -match "do-not-log") {
                throw "Keymap action description output unexpectedly contained release fixture content."
            }
            $newTabBindingDescription = Invoke-NativeJsonCommandResult "describe-keymap-binding-new-tab" @(
                "--user-data-dir", $cliDataDir,
                "--config-dir", $cliConfigDir,
                "--describe-keymap-binding", "ctrl-shift-t",
                "--describe-keymap-binding-format", "json"
            )
            if ($newTabBindingDescription.status -ne "ok" -or $newTabBindingDescription.default_keymap -ne "keymaps/zed-terminal.json" -or $newTabBindingDescription.keystrokes -ne "ctrl-shift-t" -or $newTabBindingDescription.match_count -lt 1) {
                throw "Keymap binding description did not report the expected NewTerminalTab binding contract."
            }
            if (-not ($newTabBindingDescription.matches | Where-Object { $_.keystrokes -eq "ctrl-shift-T" -and $_.match -eq "exact" -and $_.action -eq "zed_terminal::NewTerminalTab" -and $null -eq $_.context })) {
                throw "Keymap binding description is missing the NewTerminalTab exact binding."
            }
            $profileSlotBindingDescription = Invoke-NativeJsonCommandResult "describe-keymap-binding-profile-slot" @(
                "--user-data-dir", $cliDataDir,
                "--config-dir", $cliConfigDir,
                "--describe-keymap-binding", "ctrl-shift-1",
                "--describe-keymap-binding-format", "json"
            )
            if ($profileSlotBindingDescription.status -ne "ok" -or $profileSlotBindingDescription.default_keymap -ne "keymaps/zed-terminal.json" -or $profileSlotBindingDescription.keystrokes -ne "ctrl-shift-1" -or $profileSlotBindingDescription.match_count -lt 1) {
                throw "Keymap binding description did not report the expected profile slot binding contract."
            }
            if (-not ($profileSlotBindingDescription.matches | Where-Object { $_.keystrokes -eq "ctrl-shift-1" -and $_.match -eq "exact" -and $_.action -eq "zed_terminal::NewTerminalTabWithProfileSlot" -and $null -eq $_.context -and $_.input -eq '{"slot":1}' })) {
                throw "Keymap binding description is missing the profile slot exact binding."
            }
            $pasteBindingDescription = Invoke-NativeJsonCommandResult "describe-keymap-binding-paste" @(
                "--user-data-dir", $cliDataDir,
                "--config-dir", $cliConfigDir,
                "--describe-keymap-binding", "ctrl-shift-v",
                "--describe-keymap-binding-format", "json"
            )
            if (-not ($pasteBindingDescription.matches | Where-Object { $_.keystrokes -eq "ctrl-shift-V" -and $_.match -eq "exact" -and $_.action -eq "terminal::Paste" -and $_.context -eq "Terminal" })) {
                throw "Keymap binding description is missing the terminal Paste exact binding."
            }
            $keymapBindingDescriptionText = @(
                $newTabBindingDescription,
                $profileSlotBindingDescription,
                $pasteBindingDescription
            ) | ConvertTo-Json -Depth 20
            if ($keymapBindingDescriptionText -match "do-not-log") {
                throw "Keymap binding description output unexpectedly contained release fixture content."
            }
            $defaultKeymap = Invoke-NativeTextCommandResult "print-default-keymap" @(
                "--user-data-dir", $cliDataDir,
                "--config-dir", $cliConfigDir,
                "--print-default-keymap"
            ) @(
                "^// Zed Terminal keymap",
                '"ctrl-shift-t": "zed_terminal::NewTerminalTab"',
                '"ctrl-shift-1": \["zed_terminal::NewTerminalTabWithProfileSlot", \{ "slot": 1 \}\]',
                '"ctrl-shift-9": \["zed_terminal::NewTerminalTabWithProfileSlot", \{ "slot": 9 \}\]',
                '"ctrl-shift-p": "command_palette::Toggle"',
                '"alt-shift-d": "zed_terminal::DuplicateTerminalSplitAuto"',
                '"alt-f4": "zed_terminal::CloseTerminalWindow"'
            )
            if ($defaultKeymap -match "do-not-log") {
                throw "Default keymap output unexpectedly contained release fixture content."
            }
            $mutationInitConfig = Invoke-NativeJsonCommandResult "mutation-init-config" @(
                "--user-data-dir", $mutationCliDataDir,
                "--config-dir", $mutationCliConfigDir,
                "--init-config",
                "--init-config-format", "json"
            )
            Assert-ConfigInitializationJson `
                -Report $mutationInitConfig `
                -ConfigDir $mutationCliConfigDir
            $mutationKeymapFile = Join-Path $mutationCliConfigDir "keymap.json"
            Set-Content -LiteralPath $mutationKeymapFile -Value @'
// release-check keymap do-not-log-keymap
[
  {
    "bindings": {
      "ctrl-shift-t": "zed_terminal::DuplicateTerminalTab",
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
            $activeNewTabBindingDescription = Invoke-NativeJsonCommandResult "describe-active-keymap-binding-new-tab" @(
                "--user-data-dir", $mutationCliDataDir,
                "--config-dir", $mutationCliConfigDir,
                "--describe-active-keymap-binding", "ctrl-shift-t",
                "--describe-active-keymap-binding-format", "json"
            )
            if ($activeNewTabBindingDescription.status -ne "ok" -or $activeNewTabBindingDescription.default_keymap -ne "keymaps/zed-terminal.json" -or $activeNewTabBindingDescription.keymap_file -ne $mutationKeymapFile -or $activeNewTabBindingDescription.user_keymap_source -ne "file" -or $activeNewTabBindingDescription.keystrokes -ne "ctrl-shift-t" -or $activeNewTabBindingDescription.pending) {
                throw "Active keymap binding description did not report the expected override contract."
            }
            if (-not $activeNewTabBindingDescription.contexts -or $activeNewTabBindingDescription.contexts[0] -notmatch "Terminal") {
                throw "Active keymap binding description did not report the expected Terminal context."
            }
            $activeNewTabMatches = @($activeNewTabBindingDescription.matches)
            if ($activeNewTabMatches.Count -lt 2 -or $activeNewTabMatches[0].source -ne "User" -or $activeNewTabMatches[0].action -ne "zed_terminal::DuplicateTerminalTab" -or $activeNewTabMatches[1].source -ne "Default" -or $activeNewTabMatches[1].action -ne "zed_terminal::NewTerminalTab") {
                throw "Active keymap binding description did not put the user override before the bundled default binding."
            }
            $activePasteBindingDescription = Invoke-NativeJsonCommandResult "describe-active-keymap-binding-paste" @(
                "--user-data-dir", $mutationCliDataDir,
                "--config-dir", $mutationCliConfigDir,
                "--describe-active-keymap-binding", "ctrl-shift-v",
                "--describe-active-keymap-binding-context", "Terminal",
                "--describe-active-keymap-binding-format", "json"
            )
            if (-not (@($activePasteBindingDescription.matches) | Where-Object { $_.source -eq "User" -and $_.keystrokes -eq "ctrl-shift-V" -and $_.action -eq "terminal::Paste" -and $_.context -eq "Terminal" })) {
                throw "Active keymap binding description is missing the user terminal Paste binding."
            }
            $activeProfileSlotBindingDescription = Invoke-NativeJsonCommandResult "describe-active-keymap-binding-profile-slot" @(
                "--user-data-dir", $mutationCliDataDir,
                "--config-dir", $mutationCliConfigDir,
                "--describe-active-keymap-binding", "ctrl-shift-1",
                "--describe-active-keymap-binding-format", "json"
            )
            if ($activeProfileSlotBindingDescription.status -ne "ok" -or $activeProfileSlotBindingDescription.keystrokes -ne "ctrl-shift-1" -or $activeProfileSlotBindingDescription.pending) {
                throw "Active keymap binding description did not report the expected profile slot query contract."
            }
            if (-not (@($activeProfileSlotBindingDescription.matches) | Where-Object { $_.source -eq "Default" -and $_.keystrokes -eq "ctrl-shift-1" -and $_.action -eq "zed_terminal::NewTerminalTabWithProfileSlot" -and $null -eq $_.context -and $_.input -eq '{"slot":1}' })) {
                throw "Active keymap binding description is missing the bundled profile slot binding."
            }
            $activeKeymapBindingDescriptionText = @(
                $activeNewTabBindingDescription,
                $activePasteBindingDescription,
                $activeProfileSlotBindingDescription
            ) | ConvertTo-Json -Depth 20
            if ($activeKeymapBindingDescriptionText -match "do-not-log-keymap") {
                throw "Active keymap binding description output leaked keymap file contents."
            }
            $activeKeymapBindings = Invoke-NativeJsonCommandResult "list-active-keymap-bindings" @(
                "--user-data-dir", $mutationCliDataDir,
                "--config-dir", $mutationCliConfigDir,
                "--list-active-keymap-bindings",
                "--list-active-keymap-bindings-format", "json"
            )
            if ($activeKeymapBindings.status -ne "ok" -or $activeKeymapBindings.default_keymap -ne "keymaps/zed-terminal.json" -or $activeKeymapBindings.keymap_file -ne $mutationKeymapFile -or $activeKeymapBindings.user_keymap_source -ne "file" -or $activeKeymapBindings.binding_count -lt 1) {
                throw "Active keymap binding list did not report the expected active keymap contract."
            }
            if (-not $activeKeymapBindings.contexts -or $activeKeymapBindings.contexts[0] -notmatch "Terminal") {
                throw "Active keymap binding list did not report the expected Terminal context."
            }
            $activeNewTabListEntry = @($activeKeymapBindings.bindings) | Where-Object { $_.keystrokes -eq "ctrl-shift-T" } | Select-Object -First 1
            if (-not $activeNewTabListEntry -or @($activeNewTabListEntry.matches).Count -lt 2 -or $activeNewTabListEntry.matches[0].source -ne "User" -or $activeNewTabListEntry.matches[0].action -ne "zed_terminal::DuplicateTerminalTab" -or $activeNewTabListEntry.matches[1].source -ne "Default" -or $activeNewTabListEntry.matches[1].action -ne "zed_terminal::NewTerminalTab") {
                throw "Active keymap binding list did not put the user override before the bundled default binding."
            }
            $activePasteListEntry = @($activeKeymapBindings.bindings) | Where-Object { $_.keystrokes -eq "ctrl-shift-V" } | Select-Object -First 1
            if (-not $activePasteListEntry -or -not (@($activePasteListEntry.matches) | Where-Object { $_.source -eq "User" -and $_.action -eq "terminal::Paste" -and $_.context -eq "Terminal" })) {
                throw "Active keymap binding list is missing the user terminal Paste binding."
            }
            $activeProfileSlotListEntry = @($activeKeymapBindings.bindings) | Where-Object { $_.keystrokes -eq "ctrl-shift-1" } | Select-Object -First 1
            if (-not $activeProfileSlotListEntry -or -not (@($activeProfileSlotListEntry.matches) | Where-Object { $_.source -eq "Default" -and $_.action -eq "zed_terminal::NewTerminalTabWithProfileSlot" -and $null -eq $_.context -and $_.input -eq '{"slot":1}' })) {
                throw "Active keymap binding list is missing the bundled profile slot binding."
            }
            $activeKeymapBindingsText = $activeKeymapBindings | ConvertTo-Json -Depth 20
            if ($activeKeymapBindingsText -match "do-not-log-keymap") {
                throw "Active keymap binding list output leaked keymap file contents."
            }
            $mutationSettingsFile = Join-Path $mutationCliConfigDir "settings.json"
            Set-Content -LiteralPath $mutationSettingsFile -Value @'
// release-check settings do-not-log-settings
{
  "theme": "One Dark",
  "buffer_font_size": 15
}
'@ -Encoding ascii
            $mutationSettingsBackupFile = Join-Path $mutationCliConfigDir "backups\settings.backup.json"
            $settingsBackup = Invoke-NativeJsonCommandResult "mutation-backup-settings" @(
                "--user-data-dir", $mutationCliDataDir,
                "--config-dir", $mutationCliConfigDir,
                "--backup-settings",
                "--backup-settings-file", $mutationSettingsBackupFile,
                "--backup-settings-format", "json"
            )
            $settingsBackupFiles = @($settingsBackup.files)
            if ($settingsBackup.status -ne "ok" -or $settingsBackup.backup_file -ne $mutationSettingsBackupFile -or $settingsBackupFiles.Count -ne 2 -or $settingsBackup.backup_byte_count -le 0) {
                throw "Settings backup did not report the expected mutated settings summary."
            }
            $settingsBackupText = $settingsBackup | ConvertTo-Json -Depth 10
            if ($settingsBackupText -match "do-not-log-settings" -or $settingsBackupText -match "One Dark" -or $settingsBackupText -match "buffer_font_size") {
                throw "Settings backup output leaked settings file contents."
            }
            $settingsBackupFileText = Get-Content -Raw -LiteralPath $mutationSettingsBackupFile
            if ($settingsBackupFileText -notmatch "do-not-log-settings") {
                throw "Settings backup package did not preserve the full settings payload."
            }
            $settingsBackupCheck = Invoke-NativeJsonCommandResult "mutation-check-settings-backup-match" @(
                "--user-data-dir", $mutationCliDataDir,
                "--config-dir", $mutationCliConfigDir,
                "--check-settings-backup",
                "--check-settings-backup-file", $mutationSettingsBackupFile,
                "--check-settings-backup-format", "json"
            )
            if (-not $settingsBackupCheck.matches -or $settingsBackupCheck.backup_file -ne $mutationSettingsBackupFile -or @($settingsBackupCheck.files).Count -ne 2) {
                throw "Settings backup check did not report the expected matching settings summary."
            }
            $settingsBackupCheckText = $settingsBackupCheck | ConvertTo-Json -Depth 10
            if ($settingsBackupCheckText -match "do-not-log-settings" -or $settingsBackupCheckText -match "One Dark") {
                throw "Settings backup check output leaked settings file contents."
            }
            Add-Content -LiteralPath $mutationSettingsFile -Value "`n// release-check settings drift"
            $settingsBackupTextDrift = Invoke-NativeJsonCommandResult "mutation-diff-settings-backup-text-drift" @(
                "--user-data-dir", $mutationCliDataDir,
                "--config-dir", $mutationCliConfigDir,
                "--diff-settings-backup",
                "--diff-settings-backup-file", $mutationSettingsBackupFile,
                "--diff-settings-backup-format", "json"
            )
            $settingsFileTextDrift = @($settingsBackupTextDrift.files) | Where-Object { $_.label -eq "settings_file" } | Select-Object -First 1
            if ($settingsBackupTextDrift.matches -or -not $settingsFileTextDrift -or $settingsFileTextDrift.text_matches -or -not $settingsFileTextDrift.settings_matches -or @($settingsFileTextDrift.categories) -notcontains "text") {
                throw "Settings backup diff did not distinguish text-only settings drift."
            }
            Set-Content -LiteralPath $mutationSettingsFile -Value @'
{
  "theme": "Ayu Dark",
  "buffer_font_size": 15
}
'@ -Encoding ascii
            $settingsBackupDiffDrift = Invoke-NativeJsonCommandResult "mutation-diff-settings-backup-drift" @(
                "--user-data-dir", $mutationCliDataDir,
                "--config-dir", $mutationCliConfigDir,
                "--diff-settings-backup",
                "--diff-settings-backup-file", $mutationSettingsBackupFile,
                "--diff-settings-backup-format", "json"
            )
            $settingsFileDrift = @($settingsBackupDiffDrift.files) | Where-Object { $_.label -eq "settings_file" } | Select-Object -First 1
            if ($settingsBackupDiffDrift.matches -or -not $settingsFileDrift -or $settingsFileDrift.settings_matches -or @($settingsFileDrift.categories) -notcontains "settings") {
                throw "Settings backup diff did not report semantic settings drift."
            }
            $settingsBackupDiffDriftText = $settingsBackupDiffDrift | ConvertTo-Json -Depth 10
            if ($settingsBackupDiffDriftText -match "Ayu Dark" -or $settingsBackupDiffDriftText -match "One Dark" -or $settingsBackupDiffDriftText -match "do-not-log-settings") {
                throw "Settings backup diff drift output leaked settings file contents."
            }
            Set-Content -LiteralPath $mutationSettingsFile -Value "{ broken settings" -NoNewline
            $settingsRestore = Invoke-NativeJsonCommandResult "mutation-restore-settings" @(
                "--user-data-dir", $mutationCliDataDir,
                "--config-dir", $mutationCliConfigDir,
                "--restore-settings",
                "--restore-settings-file", $mutationSettingsBackupFile,
                "--restore-settings-format", "json"
            )
            if ($settingsRestore.status -ne "ok" -or $settingsRestore.restore_file -ne $mutationSettingsBackupFile -or @($settingsRestore.files).Count -ne 2) {
                throw "Settings restore did not report the expected restored settings summary."
            }
            $settingsRestoreText = $settingsRestore | ConvertTo-Json -Depth 10
            if ($settingsRestoreText -match "do-not-log-settings" -or $settingsRestoreText -match "One Dark") {
                throw "Settings restore output leaked settings file contents."
            }
            $restoredSettingsFileText = Get-Content -Raw -LiteralPath $mutationSettingsFile
            if ($restoredSettingsFileText -notmatch "do-not-log-settings" -or $restoredSettingsFileText -notmatch '"theme": "One Dark"') {
                throw "Settings restore did not restore settings.json from the backup package."
            }
            $mutationConfigBundleFile = Join-Path $mutationCliConfigDir "backups\zed-terminal-config.bundle.json"
            $configBundle = Invoke-NativeJsonCommandResult "mutation-backup-config-bundle" @(
                "--user-data-dir", $mutationCliDataDir,
                "--config-dir", $mutationCliConfigDir,
                "--backup-config-bundle",
                "--backup-config-bundle-file", $mutationConfigBundleFile,
                "--backup-config-bundle-format", "json"
            )
            $configBundleFiles = @($configBundle.files)
            $configBundleLabels = @($configBundleFiles | ForEach-Object { $_.label })
            if ($configBundle.status -ne "ok" -or $configBundle.bundle_file -ne $mutationConfigBundleFile -or $configBundleFiles.Count -ne 4 -or $configBundle.bundle_byte_count -le 0 -or $configBundleLabels -notcontains "startup_config_file" -or $configBundleLabels -notcontains "keymap_file" -or $configBundleLabels -notcontains "settings_file" -or $configBundleLabels -notcontains "global_settings_file") {
                throw "Config bundle backup did not report the expected complete config summary."
            }
            $configBundleText = $configBundle | ConvertTo-Json -Depth 10
            if ($configBundleText -match "do-not-log-settings" -or $configBundleText -match "One Dark" -or $configBundleText -match "do-not-log-keymap") {
                throw "Config bundle backup output leaked config file contents."
            }
            $configBundleFileText = Get-Content -Raw -LiteralPath $mutationConfigBundleFile
            if ($configBundleFileText -notmatch "do-not-log-settings" -or $configBundleFileText -notmatch "do-not-log-keymap") {
                throw "Config bundle package did not preserve the full config payload."
            }
            $configBundleCheck = Invoke-NativeJsonCommandResult "mutation-check-config-bundle-match" @(
                "--user-data-dir", $mutationCliDataDir,
                "--config-dir", $mutationCliConfigDir,
                "--check-config-bundle",
                "--check-config-bundle-file", $mutationConfigBundleFile,
                "--check-config-bundle-format", "json"
            )
            if (-not $configBundleCheck.matches -or $configBundleCheck.bundle_file -ne $mutationConfigBundleFile -or @($configBundleCheck.files).Count -ne 4) {
                throw "Config bundle check did not report the expected matching config summary."
            }
            Add-Content -LiteralPath (Join-Path $mutationCliConfigDir "terminal.json") -Value "`n// release-check startup config drift"
            Add-Content -LiteralPath $mutationSettingsFile -Value "`n// release-check config bundle settings drift"
            $configBundleDiff = Invoke-NativeJsonCommandResult "mutation-diff-config-bundle-drift" @(
                "--user-data-dir", $mutationCliDataDir,
                "--config-dir", $mutationCliConfigDir,
                "--diff-config-bundle",
                "--diff-config-bundle-file", $mutationConfigBundleFile,
                "--diff-config-bundle-format", "json"
            )
            $configBundleStartupDiff = @($configBundleDiff.files) | Where-Object { $_.label -eq "startup_config_file" } | Select-Object -First 1
            $configBundleSettingsDiff = @($configBundleDiff.files) | Where-Object { $_.label -eq "settings_file" } | Select-Object -First 1
            if ($configBundleDiff.matches -or -not $configBundleStartupDiff -or $configBundleStartupDiff.text_matches -or -not $configBundleStartupDiff.config_matches -or @($configBundleStartupDiff.categories) -notcontains "text") {
                throw "Config bundle diff did not distinguish text-only startup config drift."
            }
            if (-not $configBundleSettingsDiff -or $configBundleSettingsDiff.text_matches -or -not $configBundleSettingsDiff.settings_matches -or @($configBundleSettingsDiff.categories) -notcontains "text") {
                throw "Config bundle diff did not distinguish text-only settings drift."
            }
            $configBundleDiffText = $configBundleDiff | ConvertTo-Json -Depth 10
            if ($configBundleDiffText -match "do-not-log-settings" -or $configBundleDiffText -match "One Dark" -or $configBundleDiffText -match "do-not-log-keymap") {
                throw "Config bundle diff output leaked config file contents."
            }
            Set-Content -LiteralPath (Join-Path $mutationCliConfigDir "terminal.json") -Value "{ broken startup" -NoNewline
            Set-Content -LiteralPath $mutationSettingsFile -Value "{ broken settings" -NoNewline
            $configBundleRestore = Invoke-NativeJsonCommandResult "mutation-restore-config-bundle" @(
                "--user-data-dir", $mutationCliDataDir,
                "--config-dir", $mutationCliConfigDir,
                "--restore-config-bundle",
                "--restore-config-bundle-file", $mutationConfigBundleFile,
                "--restore-config-bundle-format", "json"
            )
            if ($configBundleRestore.status -ne "ok" -or $configBundleRestore.bundle_file -ne $mutationConfigBundleFile -or @($configBundleRestore.files).Count -ne 4) {
                throw "Config bundle restore did not report the expected restored config summary."
            }
            $postConfigBundleCheck = Invoke-NativeJsonCommandResult "mutation-check-config-bundle-post-restore" @(
                "--user-data-dir", $mutationCliDataDir,
                "--config-dir", $mutationCliConfigDir,
                "--check-config-bundle",
                "--check-config-bundle-file", $mutationConfigBundleFile,
                "--check-config-bundle-format", "json"
            )
            if (-not $postConfigBundleCheck.matches) {
                throw "Config bundle restore did not restore the active config files to the bundle state."
            }
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
            if ([int64]$visibleProfileEntries[0].visible_slot -ne 1) {
                throw "Visible profile list did not report the expected visible profile slot."
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
            if ($null -ne $hiddenProfileEntries[0].visible_slot) {
                throw "All-profile list assigned a visible slot to a hidden profile."
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
            if ([int64]$profileDescription.visible_slot -ne 1) {
                throw "Profile description did not report the expected visible profile slot."
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
            $hiddenProfileDescription = Invoke-NativeJsonCommandResult "describe-profile-secret" @(
                "--user-data-dir", $cliDataDir,
                "--config-dir", $cliConfigDir,
                "--describe-profile", "secret",
                "--describe-profile-format", "json"
            )
            if ($hiddenProfileDescription.profile -ne "secret" -or -not $hiddenProfileDescription.hidden -or $null -ne $hiddenProfileDescription.visible_slot) {
                throw "Hidden profile description unexpectedly reported a visible profile slot."
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
            $visualResult = Invoke-NativeCommandResult -FilePath "powershell" -Arguments $visualSmokeArgs
            $script:VisualSmoke = Convert-VisualSmokeOutput `
                -Output (($visualResult.Stdout -split "`r?`n") | Where-Object { $_.Length -gt 0 }) `
                -Mode "default" `
                -BaselineExpected ([bool]$VisualBaselineImage)
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
                $splitVisualResult = Invoke-NativeCommandResult -FilePath "powershell" -Arguments $splitVisualSmokeArgs
                $script:SplitVisualSmoke = Convert-VisualSmokeOutput `
                    -Output (($splitVisualResult.Stdout -split "`r?`n") | Where-Object { $_.Length -gt 0 }) `
                    -Mode "split" `
                    -BaselineExpected ([bool]$SplitVisualBaselineImage)
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
    Write-Host "report_file: $reportFile"
} catch {
    Write-ReleaseSummary "failed"
    Write-Host "status: failed"
    Write-Host "run_dir: $runDir"
    Write-Host "log_file: $releaseLog"
    Write-Host "summary_file: $summaryFile"
    Write-Host "report_file: $reportFile"
    throw
}
