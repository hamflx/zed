[CmdletBinding()]
Param(
    [Parameter()][string]$Binary,
    [Parameter()][string]$OutputDir,
    [Parameter()][ValidateSet("debug", "release")][string]$BuildProfile = "release",
    [Parameter()][string]$Version,
    [Parameter()][string]$SummaryFile,
    [Parameter()][switch]$SkipBuild,
    [Parameter()][switch]$Zip
)

$ErrorActionPreference = "Stop"

$repoRoot = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot ".."))
if (-not $OutputDir) {
    $OutputDir = Join-Path $repoRoot "target\zed-terminal-package"
}

$OutputDir = [System.IO.Path]::GetFullPath($OutputDir)
New-Item -ItemType Directory -Force -Path $OutputDir | Out-Null

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

function Invoke-CheckedProcess {
    param(
        [Parameter(Mandatory = $true)][string]$FilePath,
        [Parameter(Mandatory = $true)][string[]]$Arguments,
        [Parameter()][string]$WorkingDirectory = $repoRoot,
        [Parameter()][switch]$EchoOutput
    )

    $result = Invoke-ProcessCapture -FilePath $FilePath -Arguments $Arguments -WorkingDirectory $WorkingDirectory
    if ($EchoOutput -and $result.Stdout) {
        Write-Host $result.Stdout.TrimEnd()
    }
    if ($EchoOutput -and $result.Stderr) {
        Write-Host $result.Stderr.TrimEnd()
    }
    if ($result.ExitCode -ne 0) {
        if ($result.Stdout) {
            Write-Host $result.Stdout.TrimEnd()
        }
        if ($result.Stderr) {
            Write-Host $result.Stderr.TrimEnd()
        }
        throw "Command failed with exit code $($result.ExitCode): $FilePath $($Arguments -join ' ')"
    }
    return $result
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

function Get-ZedTerminalVersion {
    if ($Version) {
        return $Version
    }

    $cargoToml = Join-Path $repoRoot "crates\zed_terminal\Cargo.toml"
    $insidePackage = $false
    foreach ($line in Get-Content -LiteralPath $cargoToml) {
        if ($line -match '^\s*\[package\]\s*$') {
            $insidePackage = $true
            continue
        }
        if ($insidePackage -and $line -match '^\s*\[') {
            break
        }
        if ($insidePackage -and $line -match '^\s*version\s*=\s*"([^"]+)"') {
            return $Matches[1]
        }
    }

    throw "failed to determine zed_terminal package version from $cargoToml"
}

function Get-PlatformName {
    if ([System.Runtime.InteropServices.RuntimeInformation]::IsOSPlatform([System.Runtime.InteropServices.OSPlatform]::Windows)) {
        return "windows"
    }
    if ([System.Runtime.InteropServices.RuntimeInformation]::IsOSPlatform([System.Runtime.InteropServices.OSPlatform]::OSX)) {
        return "macos"
    }
    if ([System.Runtime.InteropServices.RuntimeInformation]::IsOSPlatform([System.Runtime.InteropServices.OSPlatform]::Linux)) {
        return "linux"
    }

    return "unknown"
}

function Get-ArchitectureName {
    switch ([System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture) {
        "X64" { return "x86_64" }
        "Arm64" { return "aarch64" }
        "Arm" { return "arm" }
        "X86" { return "x86" }
        default { return [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture.ToString().ToLowerInvariant() }
    }
}

function Get-RelativePath {
    param(
        [Parameter(Mandatory = $true)][string]$Root,
        [Parameter(Mandatory = $true)][string]$Path
    )

    $rootPath = [System.IO.Path]::GetFullPath($Root)
    if (-not $rootPath.EndsWith([System.IO.Path]::DirectorySeparatorChar)) {
        $rootPath += [System.IO.Path]::DirectorySeparatorChar
    }
    $targetPath = [System.IO.Path]::GetFullPath($Path)
    $rootUri = [Uri]$rootPath
    $targetUri = [Uri]$targetPath
    $relative = [Uri]::UnescapeDataString($rootUri.MakeRelativeUri($targetUri).ToString())
    return $relative.Replace('/', [System.IO.Path]::DirectorySeparatorChar)
}

function Resolve-BinaryPath {
    if ($Binary) {
        return [System.IO.Path]::GetFullPath($Binary)
    }

    $binaryName = if ((Get-PlatformName) -eq "windows") { "zed-terminal.exe" } else { "zed-terminal" }
    return [System.IO.Path]::GetFullPath((Join-Path $repoRoot "target\$BuildProfile\$binaryName"))
}

function Copy-RequiredFile {
    param(
        [Parameter(Mandatory = $true)][string]$Source,
        [Parameter(Mandatory = $true)][string]$Destination
    )

    if (-not (Test-Path -LiteralPath $Source -PathType Leaf)) {
        throw "required package file not found: $Source"
    }
    Copy-Item -LiteralPath $Source -Destination $Destination -Force
}

function Normalize-PackageRelativePath {
    param([Parameter(Mandatory = $true)][string]$Path)

    return $Path.Replace('/', [System.IO.Path]::DirectorySeparatorChar).Replace('\', [System.IO.Path]::DirectorySeparatorChar)
}

function Test-PackageRelativePath {
    param([Parameter(Mandatory = $true)][AllowEmptyString()][string]$Path)

    if ([string]::IsNullOrWhiteSpace($Path)) {
        return $false
    }

    if ([System.IO.Path]::IsPathRooted($Path) -or $Path -match '^[A-Za-z]:[\\/]') {
        return $false
    }

    foreach ($segment in ($Path -split '[\\/]')) {
        if ([string]::IsNullOrWhiteSpace($segment) -or $segment -eq "." -or $segment -eq "..") {
            return $false
        }
    }

    return $true
}

function Resolve-PackageContentPath {
    param(
        [Parameter(Mandatory = $true)][string]$Root,
        [Parameter(Mandatory = $true)][string]$RelativePath
    )

    if (-not (Test-PackageRelativePath $RelativePath)) {
        throw "package manifest contains an invalid relative path: $RelativePath"
    }

    $rootPath = [System.IO.Path]::GetFullPath($Root)
    if (-not $rootPath.EndsWith([System.IO.Path]::DirectorySeparatorChar)) {
        $rootPath += [System.IO.Path]::DirectorySeparatorChar
    }

    $contentPath = [System.IO.Path]::GetFullPath((Join-Path $rootPath $RelativePath))
    if (-not $contentPath.StartsWith($rootPath, [System.StringComparison]::OrdinalIgnoreCase)) {
        throw "package manifest path escapes package directory: $RelativePath"
    }

    return $contentPath
}

function New-PackageReadme {
    param(
        [Parameter(Mandatory = $true)][string]$PackageName,
        [Parameter(Mandatory = $true)][string]$BinaryFileName
    )

    $template = @'
# Zed Terminal

Zed Terminal is a standalone terminal application built from Zed's terminal view and workspace components. It keeps terminal rendering, shell/task integration, themes, keymaps, and diagnostics aligned with Zed while using independent config, data, and log roots.

## Quick Start

Extract `{{PACKAGE}}.zip` into a writable directory, then start the app:

```powershell
.\{{BINARY}}
```

Initialize editable user config files when you want to customize startup profiles, key bindings, or settings:

```powershell
.\{{BINARY}} --init-config
```

Inspect the active standalone paths without opening a terminal window:

```powershell
.\{{BINARY}} --paths --paths-format json
```

The JSON path report includes the settings, startup, and keymap schema files plus the bundled default keymap reference path.

## Verify Download

When a release includes the sibling `{{PACKAGE}}.zip.sha256` file, verify the archive before extracting it. The checksum sidecar is shipped next to the zip file, not inside this package directory.

```powershell
Get-Content .\{{PACKAGE}}.zip.sha256
Get-FileHash .\{{PACKAGE}}.zip -Algorithm SHA256
```

The hash printed by `Get-FileHash` should match the hash in `{{PACKAGE}}.zip.sha256`.

## Run

Start with the default standalone config and data roots:

```powershell
.\{{BINARY}}
```

Run in portable mode with package-local `data/` and `config/` directories:

```powershell
.\{{BINARY}} --portable
.\{{BINARY}} --portable --paths --paths-format json
```

Run with explicit custom roots when you want the data and config directories elsewhere:

```powershell
.\{{BINARY}} --user-data-dir .\portable\data --config-dir .\portable\config
```

Preview the resolved startup layout without opening a window:

```powershell
.\{{BINARY}} --print-startup-layout
.\{{BINARY}} --print-startup-layout --startup-layout-format json
```

Inspect startup profiles and root startup configuration without opening a window:

```powershell
.\{{BINARY}} --list-profiles --list-profiles-format json
.\{{BINARY}} --list-profile-slots --list-profile-slots-format json
.\{{BINARY}} --describe-startup --describe-startup-format json
```

## Configuration

Use `--init-config` to create user-editable files under the active config directory. The packaged `config-template/` directory is a generated reference containing first-run config files, the default keymap, and JSON schemas for editor support.

Key files:

- `terminal.json`: startup layout, profiles, titles, split panes, working directories, and startup commands.
- `keymap.json`: user key bindings for the standalone app.
- `settings.json` and `global_settings.json`: Zed settings loaded by the standalone terminal.
- `settings.schema.json`, `terminal.schema.json`, and `keymap.schema.json`: generated JSON schemas.

## Diagnostics

Run a read-only health check:

```powershell
.\{{BINARY}} --doctor
```

Run a script-friendly health check:

```powershell
.\{{BINARY}} --doctor --doctor-format json
```

Validate settings without opening a terminal window:

```powershell
.\{{BINARY}} --validate-settings
.\{{BINARY}} --validate-settings --validate-settings-format json
```

Validate key bindings without opening a terminal window:

```powershell
.\{{BINARY}} --validate-keymap
.\{{BINARY}} --validate-keymap --validate-keymap-format json
```

Inspect the standalone action catalog and bundled default bindings without opening a terminal window:

```powershell
.\{{BINARY}} --list-keymap-actions --list-keymap-actions-format json
.\{{BINARY}} --describe-keymap-action zed_terminal::NewTerminalTabWithProfileSlot --describe-keymap-action-format json
.\{{BINARY}} --describe-keymap-binding ctrl-shift-1 --describe-keymap-binding-format json
```

Inspect active key bindings after user overrides without opening a terminal window:

```powershell
.\{{BINARY}} --describe-active-keymap-binding ctrl-shift-1 --describe-active-keymap-binding-format json
.\{{BINARY}} --list-active-keymap-bindings --list-active-keymap-bindings-format json
```

Validate startup configuration without opening a terminal window:

```powershell
.\{{BINARY}} --validate-startup-config
.\{{BINARY}} --validate-startup-config --validate-startup-config-format json
```

Back up, compare, and restore settings without opening a terminal window:

```powershell
.\{{BINARY}} --backup-settings --backup-settings-file settings.backup.json
.\{{BINARY}} --check-settings-backup --check-settings-backup-file settings.backup.json
.\{{BINARY}} --diff-settings-backup --diff-settings-backup-file settings.backup.json
.\{{BINARY}} --restore-settings --restore-settings-file settings.backup.json
```

Back up, compare, and restore startup configuration without opening a terminal window:

```powershell
.\{{BINARY}} --backup-startup-config --backup-startup-config-file terminal.backup.json
.\{{BINARY}} --check-startup-config-backup --check-startup-config-backup-file terminal.backup.json
.\{{BINARY}} --diff-startup-config-backup --diff-startup-config-backup-file terminal.backup.json
.\{{BINARY}} --restore-startup-config --restore-startup-config-file terminal.backup.json
```

Back up, compare, and restore key bindings without opening a terminal window:

```powershell
.\{{BINARY}} --backup-keymap --backup-keymap-file keymap.backup.json
.\{{BINARY}} --check-keymap-backup --check-keymap-backup-file keymap.backup.json
.\{{BINARY}} --diff-keymap-backup --diff-keymap-backup-file keymap.backup.json
.\{{BINARY}} --restore-keymap --restore-keymap-file keymap.backup.json
```

Back up, compare, and restore the complete user config set without opening a terminal window:

```powershell
.\{{BINARY}} --backup-config-bundle --backup-config-bundle-file zed-terminal-config.bundle.json
.\{{BINARY}} --check-config-bundle --check-config-bundle-file zed-terminal-config.bundle.json
.\{{BINARY}} --diff-config-bundle --diff-config-bundle-file zed-terminal-config.bundle.json
.\{{BINARY}} --restore-config-bundle --restore-config-bundle-file zed-terminal-config.bundle.json
.\{{BINARY}} --list-config-bundle-backups --list-config-bundle-backups-format json
```

Generate support information without opening a terminal window:

```powershell
.\{{BINARY}} --support-info > zed-terminal-support-info.txt
.\{{BINARY}} --support-bundle --support-bundle-dir zed-terminal-support-bundle --support-bundle-format json
```

## Included Files

- `{{BINARY}}`: standalone Zed Terminal executable.
- `default-keymap.json`: bundled default standalone keymap reference.
- `config-template/`: generated first-run config, default keymap, and JSON schemas.
- `zed-terminal-package.json`: package manifest with version/build metadata, validation status, file sizes, and SHA256 hashes.
- `LICENSE-GPL` and `LICENSE-APACHE`: repository license files.

The package is validated before release packaging: the binary must pass help, path inspection, config initialization, schema generation, default keymap generation, settings validation, startup config validation, keymap validation, default keymap discovery, active keymap discovery, settings backup/check/diff/restore, startup config backup/check/diff/restore, keymap backup/check/diff/restore, complete config bundle backup/check/diff/restore/list, doctor, support-info, redacted support bundle, README, license file, manifest, zip extraction, and checksum sidecar checks.
'@

    return $template.Replace("{{PACKAGE}}", $PackageName).Replace("{{BINARY}}", $BinaryFileName)
}

function Assert-PackageReadme {
    param(
        [Parameter(Mandatory = $true)][string]$PackageDir,
        [Parameter(Mandatory = $true)][string]$PackageName,
        [Parameter(Mandatory = $true)][string]$BinaryFileName
    )

    $readmeFile = Join-Path $PackageDir "README.md"
    if (-not (Test-Path -LiteralPath $readmeFile -PathType Leaf)) {
        throw "package README was not written: $readmeFile"
    }

    $readme = Get-Content -LiteralPath $readmeFile -Raw
    $requiredSnippets = @(
        "# Zed Terminal",
        "## Quick Start",
        "## Verify Download",
        "## Run",
        "## Configuration",
        "## Diagnostics",
        "## Included Files",
        ".\$BinaryFileName",
        ".\$BinaryFileName --paths --paths-format json",
        ".\$BinaryFileName --portable",
        ".\$BinaryFileName --portable --paths --paths-format json",
        ".\$BinaryFileName --init-config",
        ".\$BinaryFileName --print-startup-layout",
        ".\$BinaryFileName --print-startup-layout --startup-layout-format json",
        ".\$BinaryFileName --list-profiles --list-profiles-format json",
        ".\$BinaryFileName --list-profile-slots --list-profile-slots-format json",
        ".\$BinaryFileName --describe-startup --describe-startup-format json",
        ".\$BinaryFileName --doctor",
        ".\$BinaryFileName --validate-settings",
        ".\$BinaryFileName --validate-settings --validate-settings-format json",
        ".\$BinaryFileName --validate-keymap",
        ".\$BinaryFileName --validate-keymap --validate-keymap-format json",
        ".\$BinaryFileName --list-keymap-actions --list-keymap-actions-format json",
        ".\$BinaryFileName --describe-keymap-action zed_terminal::NewTerminalTabWithProfileSlot --describe-keymap-action-format json",
        ".\$BinaryFileName --describe-keymap-binding ctrl-shift-1 --describe-keymap-binding-format json",
        ".\$BinaryFileName --describe-active-keymap-binding ctrl-shift-1 --describe-active-keymap-binding-format json",
        ".\$BinaryFileName --list-active-keymap-bindings --list-active-keymap-bindings-format json",
        ".\$BinaryFileName --validate-startup-config",
        ".\$BinaryFileName --validate-startup-config --validate-startup-config-format json",
        ".\$BinaryFileName --backup-settings --backup-settings-file settings.backup.json",
        ".\$BinaryFileName --check-settings-backup --check-settings-backup-file settings.backup.json",
        ".\$BinaryFileName --diff-settings-backup --diff-settings-backup-file settings.backup.json",
        ".\$BinaryFileName --restore-settings --restore-settings-file settings.backup.json",
        ".\$BinaryFileName --backup-startup-config --backup-startup-config-file terminal.backup.json",
        ".\$BinaryFileName --check-startup-config-backup --check-startup-config-backup-file terminal.backup.json",
        ".\$BinaryFileName --diff-startup-config-backup --diff-startup-config-backup-file terminal.backup.json",
        ".\$BinaryFileName --restore-startup-config --restore-startup-config-file terminal.backup.json",
        ".\$BinaryFileName --backup-keymap --backup-keymap-file keymap.backup.json",
        ".\$BinaryFileName --check-keymap-backup --check-keymap-backup-file keymap.backup.json",
        ".\$BinaryFileName --diff-keymap-backup --diff-keymap-backup-file keymap.backup.json",
        ".\$BinaryFileName --restore-keymap --restore-keymap-file keymap.backup.json",
        ".\$BinaryFileName --backup-config-bundle --backup-config-bundle-file zed-terminal-config.bundle.json",
        ".\$BinaryFileName --check-config-bundle --check-config-bundle-file zed-terminal-config.bundle.json",
        ".\$BinaryFileName --diff-config-bundle --diff-config-bundle-file zed-terminal-config.bundle.json",
        ".\$BinaryFileName --restore-config-bundle --restore-config-bundle-file zed-terminal-config.bundle.json",
        ".\$BinaryFileName --list-config-bundle-backups --list-config-bundle-backups-format json",
        ".\$BinaryFileName --support-info",
        ".\$BinaryFileName --support-bundle --support-bundle-dir zed-terminal-support-bundle --support-bundle-format json",
        "$PackageName.zip.sha256",
        "config-template/",
        "settings.schema.json",
        "zed-terminal-package.json",
        "LICENSE-GPL",
        "LICENSE-APACHE"
    )

    foreach ($snippet in $requiredSnippets) {
        if ($readme.IndexOf($snippet, [System.StringComparison]::Ordinal) -lt 0) {
            throw "package README is missing required content: $snippet"
        }
    }
}

function Read-PackageJsonFile {
    param(
        [Parameter(Mandatory = $true)][string]$Path,
        [Parameter(Mandatory = $true)][string]$Label
    )

    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) {
        throw "package $Label file was not written: $Path"
    }

    $text = Get-Content -LiteralPath $Path -Raw
    if ([string]::IsNullOrWhiteSpace($text)) {
        throw "package $Label file was empty: $Path"
    }

    try {
        $json = $text | ConvertFrom-Json
    } catch {
        throw "package $Label file did not parse as JSON: $($_.Exception.Message)"
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

    $schema = Read-PackageJsonFile -Path $Path -Label $Label
    if ($schema.json.title -ne $ExpectedTitle -or $schema.json.type -ne $ExpectedType) {
        throw "package $Label schema did not report the expected $ExpectedTitle/$ExpectedType contract"
    }
    if (-not $schema.text.EndsWith("`n")) {
        throw "package $Label schema should end with a newline"
    }

    foreach ($propertyName in $RequiredProperties) {
        if (-not $schema.json.properties -or -not $schema.json.properties.PSObject.Properties[$propertyName]) {
            throw "package $Label schema is missing expected property: $propertyName"
        }
    }

    foreach ($snippet in $RequiredSnippets) {
        if ($schema.text.IndexOf($snippet, [System.StringComparison]::Ordinal) -lt 0) {
            throw "package $Label schema is missing expected content: $snippet"
        }
    }

    foreach ($snippet in $ForbiddenSnippets) {
        if ($schema.text.IndexOf($snippet, [System.StringComparison]::Ordinal) -ge 0) {
            throw "package $Label schema exposed forbidden content: $snippet"
        }
    }
}

function Assert-PackageConfigTemplateSchemas {
    param([Parameter(Mandatory = $true)][string]$ConfigTemplateDir)

    if (-not (Test-Path -LiteralPath $ConfigTemplateDir -PathType Container)) {
        throw "package config template directory was not written: $ConfigTemplateDir"
    }

    Assert-PackageJsonSchemaFile `
        -Path (Join-Path $ConfigTemplateDir "terminal.schema.json") `
        -Label "startup config" `
        -ExpectedTitle "TerminalStartupConfig" `
        -ExpectedType "object" `
        -RequiredProperties @("working_directory", "command", "shell", "env", "tabs", "default_profile", "profiles")

    Assert-PackageJsonSchemaFile `
        -Path (Join-Path $ConfigTemplateDir "settings.schema.json") `
        -Label "settings" `
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
        -Label "keymap" `
        -ExpectedTitle "KeymapFile" `
        -ExpectedType "array" `
        -RequiredSnippets @(
            "zed_terminal::NewTerminalTab",
            "zed_terminal::NewTerminalTabWithProfile",
            "zed_terminal::NewTerminalTabWithProfileSlot",
            "zed_terminal::NewTerminalWindowWithProfileSlot",
            "zed_terminal::NewTerminalSplitWithProfileSlot",
            "zed_terminal::OpenConfigBundleBackupFile",
            "zed_terminal::OpenConfigBundleBackupDirectory",
            "zed_terminal::OpenConfigBundleBackupsDirectory",
            "zed_terminal::OpenConfigBundleBackupsReport",
            "zed_terminal::OpenConfigInitializationReport",
            "zed_terminal::OpenKeymapToolsPicker",
            "zed_terminal::OpenSettingsSchemaFile",
            "zed_terminal::OpenSettingsToolsPicker",
            "zed_terminal::OpenStartupProfileConfig",
            "zed_terminal::OpenStartupProfilePicker",
            "zed_terminal::OpenStartupProfileSlotsReport",
            "zed_terminal::OpenSupportBundleManifestFile",
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

function Assert-PackageDefaultKeymapReferences {
    param([Parameter(Mandatory = $true)][string]$PackageDir)

    $rootDefaultKeymapFile = Join-Path $PackageDir "default-keymap.json"
    $templateDefaultKeymapFile = Join-Path (Join-Path $PackageDir "config-template") "default-keymap.json"
    if (-not (Test-Path -LiteralPath $rootDefaultKeymapFile -PathType Leaf)) {
        throw "package default keymap reference was not written: $rootDefaultKeymapFile"
    }
    if (-not (Test-Path -LiteralPath $templateDefaultKeymapFile -PathType Leaf)) {
        throw "package config template default keymap reference was not written: $templateDefaultKeymapFile"
    }

    $rootDefaultKeymapText = Get-Content -LiteralPath $rootDefaultKeymapFile -Raw
    $templateDefaultKeymapText = Get-Content -LiteralPath $templateDefaultKeymapFile -Raw
    if ([string]::IsNullOrWhiteSpace($rootDefaultKeymapText) -or [string]::IsNullOrWhiteSpace($templateDefaultKeymapText)) {
        throw "package default keymap references must not be empty"
    }

    $rootNormalized = $rootDefaultKeymapText.Replace("`r`n", "`n").TrimEnd()
    $templateNormalized = $templateDefaultKeymapText.Replace("`r`n", "`n").TrimEnd()
    if ($rootNormalized -ne $templateNormalized) {
        throw "package default keymap references did not describe the same keymap"
    }

    foreach ($snippet in @(
        "zed_terminal::NewTerminalTab",
        "zed_terminal::NewTerminalTabWithProfileSlot",
        '"ctrl-shift-1"',
        '"ctrl-shift-9"',
        "zed_terminal::DuplicateTerminalTab",
        "command_palette::Toggle",
        "terminal::Paste",
        "pane::CloseActiveItem"
    )) {
        if (
            $rootDefaultKeymapText.IndexOf($snippet, [System.StringComparison]::Ordinal) -lt 0 -or
            $templateDefaultKeymapText.IndexOf($snippet, [System.StringComparison]::Ordinal) -lt 0
        ) {
            throw "package default keymap references are missing expected binding content: $snippet"
        }
    }
}

function Assert-PackageLicenses {
    param([Parameter(Mandatory = $true)][string]$PackageDir)

    foreach ($license in @(
        [pscustomobject]@{
            Name = "LICENSE-GPL"
            RequiredSnippet = "GNU GENERAL PUBLIC LICENSE"
        },
        [pscustomobject]@{
            Name = "LICENSE-APACHE"
            RequiredSnippet = "Apache License"
        }
    )) {
        $repoLicenseFile = Join-Path $repoRoot $license.Name
        $packageLicenseFile = Join-Path $PackageDir $license.Name
        if (-not (Test-Path -LiteralPath $repoLicenseFile -PathType Leaf)) {
            throw "repository license file was not found: $repoLicenseFile"
        }
        if (-not (Test-Path -LiteralPath $packageLicenseFile -PathType Leaf)) {
            throw "package license file was not written: $packageLicenseFile"
        }

        $licenseText = Get-Content -LiteralPath $packageLicenseFile -Raw
        if ([string]::IsNullOrWhiteSpace($licenseText)) {
            throw "package license file must not be empty: $($license.Name)"
        }
        if ($licenseText.IndexOf($license.RequiredSnippet, [System.StringComparison]::Ordinal) -lt 0) {
            throw "package license file is missing expected content: $($license.Name)"
        }

        $repoHash = (Get-FileHash -LiteralPath $repoLicenseFile -Algorithm SHA256).Hash.ToLowerInvariant()
        $packageHash = (Get-FileHash -LiteralPath $packageLicenseFile -Algorithm SHA256).Hash.ToLowerInvariant()
        if ($packageHash -ne $repoHash) {
            throw "package license file did not match repository source: $($license.Name)"
        }
    }
}

function Assert-VersionInfoJson {
    param(
        [Parameter(Mandatory = $true)]$VersionInfo,
        [Parameter(Mandatory = $true)][string]$Version,
        [Parameter(Mandatory = $true)][string]$Platform,
        [Parameter(Mandatory = $true)][string]$Architecture
    )

    $expectedTargetOs = switch ($Platform) {
        "windows" { "windows" }
        "macos" { "macos" }
        "linux" { "linux" }
        default { $Platform }
    }

    $expectedTargetArch = switch ($Architecture) {
        "x86_64" { "x86_64" }
        "aarch64" { "aarch64" }
        "arm" { "arm" }
        "x86" { "x86" }
        default { $Architecture }
    }

    if (
        $VersionInfo.app_name -ne "Zed Terminal" -or
        $VersionInfo.binary_name -ne "zed-terminal" -or
        $VersionInfo.package_name -ne "zed_terminal" -or
        $VersionInfo.version -ne $Version -or
        $VersionInfo.target_os -ne $expectedTargetOs -or
        $VersionInfo.target_arch -ne $expectedTargetArch -or
        $null -eq $VersionInfo.debug_assertions
    ) {
        throw "zed-terminal --version-info did not report expected package metadata"
    }
}

function Assert-PortablePathsJson {
    param(
        [Parameter(Mandatory = $true)]$Paths,
        [Parameter(Mandatory = $true)][string]$ExpectedPackageDir
    )

    $expectedDataDir = Join-Path $ExpectedPackageDir "data"
    $expectedConfigDir = Join-Path $ExpectedPackageDir "config"

    Assert-PathsJson `
        -Paths $Paths `
        -ExpectedMode "portable" `
        -ExpectedDataDir $expectedDataDir `
        -ExpectedConfigDir $expectedConfigDir
}

function Assert-PathsJson {
    param(
        [Parameter(Mandatory = $true)]$Paths,
        [Parameter(Mandatory = $true)][ValidateSet("custom", "portable")][string]$ExpectedMode,
        [Parameter(Mandatory = $true)][string]$ExpectedDataDir,
        [Parameter(Mandatory = $true)][string]$ExpectedConfigDir,
        [Parameter()][switch]$RequireConfigAssets
    )

    $expectedLogsDir = Join-Path $ExpectedDataDir "logs"
    if (
        $Paths.mode -ne $ExpectedMode -or
        $Paths.config_dir -ne $expectedConfigDir -or
        $Paths.data_dir -ne $ExpectedDataDir -or
        $Paths.logs_dir -ne $expectedLogsDir -or
        $Paths.settings_file -ne (Join-Path $expectedConfigDir "settings.json") -or
        $Paths.settings_schema_file -ne (Join-Path $expectedConfigDir "settings.schema.json") -or
        $Paths.startup_config_file -ne (Join-Path $expectedConfigDir "terminal.json") -or
        $Paths.startup_config_schema_file -ne (Join-Path $expectedConfigDir "terminal.schema.json") -or
        $Paths.global_settings_file -ne (Join-Path $expectedConfigDir "global_settings.json") -or
        $Paths.keymap_file -ne (Join-Path $expectedConfigDir "keymap.json") -or
        $Paths.keymap_schema_file -ne (Join-Path $expectedConfigDir "keymap.schema.json") -or
        $Paths.default_keymap_reference_file -ne (Join-Path $expectedConfigDir "default-keymap.json") -or
        $Paths.themes_dir -ne (Join-Path $expectedConfigDir "themes") -or
        $Paths.log_file -ne (Join-Path $expectedLogsDir "Zed Terminal.log")
    ) {
        throw "zed-terminal --paths did not report expected $ExpectedMode standalone paths"
    }

    if ($RequireConfigAssets) {
        foreach ($path in @(
            $Paths.settings_file,
            $Paths.settings_schema_file,
            $Paths.startup_config_file,
            $Paths.startup_config_schema_file,
            $Paths.global_settings_file,
            $Paths.keymap_file,
            $Paths.keymap_schema_file,
            $Paths.default_keymap_reference_file
        )) {
            if (-not (Test-Path -LiteralPath $path -PathType Leaf)) {
                throw "zed-terminal --paths reported a missing config-template asset: $path"
            }
        }
    }
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

function Assert-KeymapValidationJson {
    param(
        [Parameter(Mandatory = $true)]$Report,
        [Parameter(Mandatory = $true)][string]$ExpectedKeymapFile
    )

    if (
        $Report.status -ne "ok" -or
        $Report.keymap_file -ne $ExpectedKeymapFile -or
        [int64]$Report.default_binding_count -le 0 -or
        $Report.user_keymap_source -ne "file" -or
        $null -eq $Report.user_binding_count -or
        [int64]$Report.user_binding_count -lt 0
    ) {
        throw "zed-terminal --validate-keymap did not report expected keymap validation status"
    }
}

function Assert-DefaultProfileSlotBindingMatch {
    param(
        [Parameter(Mandatory = $true)]$Matches,
        [Parameter(Mandatory = $true)][string]$Context
    )

    if (-not (@($Matches) | Where-Object {
        $_.keystrokes -eq "ctrl-shift-1" -and
        $_.match -eq "exact" -and
        $_.action -eq "zed_terminal::NewTerminalTabWithProfileSlot" -and
        $_.namespace -eq "zed_terminal" -and
        $null -eq $_.context -and
        $_.input -eq '{"slot":1}'
    } | Select-Object -First 1)) {
        throw "$Context is missing the bundled ctrl-shift-1 profile-slot binding"
    }
}

function Invoke-KeymapDiscoverySmoke {
    param(
        [Parameter(Mandatory = $true)][string]$Binary,
        [Parameter(Mandatory = $true)][string]$DataDir,
        [Parameter(Mandatory = $true)][string]$ConfigDir,
        [Parameter(Mandatory = $true)][string]$WorkingDirectory
    )

    $actions = Invoke-CheckedProcess -FilePath $Binary -Arguments @(
        "--user-data-dir", $DataDir,
        "--config-dir", $ConfigDir,
        "--list-keymap-actions",
        "--list-keymap-actions-format", "json"
    ) -WorkingDirectory $WorkingDirectory
    $actionsJson = $actions.Stdout | ConvertFrom-Json
    if (
        $actionsJson.status -ne "ok" -or
        $actionsJson.default_keymap -ne "keymaps/zed-terminal.json" -or
        [int64]$actionsJson.action_count -lt 1
    ) {
        throw "zed-terminal --list-keymap-actions did not report the expected packaged keymap action catalog contract"
    }

    $profileSlotAction = @($actionsJson.actions) | Where-Object { $_.name -eq "zed_terminal::NewTerminalTabWithProfileSlot" } | Select-Object -First 1
    if (-not $profileSlotAction -or $profileSlotAction.namespace -ne "zed_terminal" -or $profileSlotAction.input -ne "object") {
        throw "zed-terminal --list-keymap-actions is missing the profile-slot tab action contract"
    }
    if (-not (@($profileSlotAction.default_bindings) | Where-Object { $_.keystrokes -eq "ctrl-shift-1" -and $null -eq $_.context -and $_.input -eq '{"slot":1}' } | Select-Object -First 1)) {
        throw "zed-terminal --list-keymap-actions is missing the ctrl-shift-1 profile-slot default binding"
    }
    foreach ($actionName in @(
        "zed_terminal::OpenActiveKeymapBindingsReport",
        "zed_terminal::OpenConfigBundleBackupFile",
        "zed_terminal::OpenConfigBundleBackupsReport",
        "zed_terminal::OpenKeymapActionCatalogReport",
        "zed_terminal::OpenStartupProfileSlotsReport",
        "zed_terminal::OpenSupportBundleManifestFile",
        "terminal::Paste"
    )) {
        if (-not (@($actionsJson.actions) | Where-Object { $_.name -eq $actionName } | Select-Object -First 1)) {
            throw "zed-terminal --list-keymap-actions is missing expected action: $actionName"
        }
    }

    $actionDescription = Invoke-CheckedProcess -FilePath $Binary -Arguments @(
        "--user-data-dir", $DataDir,
        "--config-dir", $ConfigDir,
        "--describe-keymap-action", "zed_terminal::NewTerminalTabWithProfileSlot",
        "--describe-keymap-action-format", "json"
    ) -WorkingDirectory $WorkingDirectory
    $actionDescriptionJson = $actionDescription.Stdout | ConvertFrom-Json
    if (
        $actionDescriptionJson.status -ne "ok" -or
        $actionDescriptionJson.default_keymap -ne "keymaps/zed-terminal.json" -or
        $actionDescriptionJson.action.name -ne "zed_terminal::NewTerminalTabWithProfileSlot" -or
        $actionDescriptionJson.action.namespace -ne "zed_terminal" -or
        $actionDescriptionJson.action.input -ne "object"
    ) {
        throw "zed-terminal --describe-keymap-action did not report the expected profile-slot action contract"
    }
    if (-not (@($actionDescriptionJson.action.default_bindings) | Where-Object { $_.keystrokes -eq "ctrl-shift-1" -and $null -eq $_.context -and $_.input -eq '{"slot":1}' } | Select-Object -First 1)) {
        throw "zed-terminal --describe-keymap-action is missing the ctrl-shift-1 profile-slot default binding"
    }

    $bindingDescription = Invoke-CheckedProcess -FilePath $Binary -Arguments @(
        "--user-data-dir", $DataDir,
        "--config-dir", $ConfigDir,
        "--describe-keymap-binding", "ctrl-shift-1",
        "--describe-keymap-binding-format", "json"
    ) -WorkingDirectory $WorkingDirectory
    $bindingDescriptionJson = $bindingDescription.Stdout | ConvertFrom-Json
    if (
        $bindingDescriptionJson.status -ne "ok" -or
        $bindingDescriptionJson.default_keymap -ne "keymaps/zed-terminal.json" -or
        $bindingDescriptionJson.keystrokes -ne "ctrl-shift-1" -or
        [int64]$bindingDescriptionJson.match_count -lt 1
    ) {
        throw "zed-terminal --describe-keymap-binding did not report the expected profile-slot binding contract"
    }
    Assert-DefaultProfileSlotBindingMatch `
        -Matches $bindingDescriptionJson.matches `
        -Context "zed-terminal --describe-keymap-binding"
}

function Assert-ActiveProfileSlotBindingMatch {
    param(
        [Parameter(Mandatory = $true)]$Matches,
        [Parameter(Mandatory = $true)][string]$Context
    )

    if (-not (@($Matches) | Where-Object {
        $_.source -eq "Default" -and
        $_.keystrokes -eq "ctrl-shift-1" -and
        $_.action -eq "zed_terminal::NewTerminalTabWithProfileSlot" -and
        $_.namespace -eq "zed_terminal" -and
        $null -eq $_.context -and
        $_.input -eq '{"slot":1}'
    } | Select-Object -First 1)) {
        throw "$Context is missing the bundled ctrl-shift-1 profile-slot binding"
    }
}

function Invoke-ActiveKeymapDiscoverySmoke {
    param(
        [Parameter(Mandatory = $true)][string]$Binary,
        [Parameter(Mandatory = $true)][string]$DataDir,
        [Parameter(Mandatory = $true)][string]$ConfigDir,
        [Parameter(Mandatory = $true)][string]$WorkingDirectory
    )

    $expectedKeymapFile = Join-Path $ConfigDir "keymap.json"
    $description = Invoke-CheckedProcess -FilePath $Binary -Arguments @(
        "--user-data-dir", $DataDir,
        "--config-dir", $ConfigDir,
        "--describe-active-keymap-binding", "ctrl-shift-1",
        "--describe-active-keymap-binding-format", "json"
    ) -WorkingDirectory $WorkingDirectory
    $descriptionJson = $description.Stdout | ConvertFrom-Json
    if (
        $descriptionJson.status -ne "ok" -or
        $descriptionJson.default_keymap -ne "keymaps/zed-terminal.json" -or
        $descriptionJson.keymap_file -ne $expectedKeymapFile -or
        $descriptionJson.user_keymap_source -ne "file" -or
        $descriptionJson.keystrokes -ne "ctrl-shift-1" -or
        $descriptionJson.pending -ne $false -or
        [int64]$descriptionJson.match_count -lt 1
    ) {
        throw "zed-terminal --describe-active-keymap-binding did not report the expected packaged active keymap contract"
    }
    if (-not $descriptionJson.contexts -or $descriptionJson.contexts[0] -notmatch "Terminal") {
        throw "zed-terminal --describe-active-keymap-binding did not report the default Terminal context"
    }
    Assert-ActiveProfileSlotBindingMatch `
        -Matches $descriptionJson.matches `
        -Context "zed-terminal --describe-active-keymap-binding"

    $list = Invoke-CheckedProcess -FilePath $Binary -Arguments @(
        "--user-data-dir", $DataDir,
        "--config-dir", $ConfigDir,
        "--list-active-keymap-bindings",
        "--list-active-keymap-bindings-format", "json"
    ) -WorkingDirectory $WorkingDirectory
    $listJson = $list.Stdout | ConvertFrom-Json
    if (
        $listJson.status -ne "ok" -or
        $listJson.default_keymap -ne "keymaps/zed-terminal.json" -or
        $listJson.keymap_file -ne $expectedKeymapFile -or
        $listJson.user_keymap_source -ne "file" -or
        [int64]$listJson.binding_count -lt 1
    ) {
        throw "zed-terminal --list-active-keymap-bindings did not report the expected packaged active keymap contract"
    }
    if (-not $listJson.contexts -or $listJson.contexts[0] -notmatch "Terminal") {
        throw "zed-terminal --list-active-keymap-bindings did not report the default Terminal context"
    }
    $profileSlotEntry = @($listJson.bindings) | Where-Object { $_.keystrokes -eq "ctrl-shift-1" } | Select-Object -First 1
    if (-not $profileSlotEntry) {
        throw "zed-terminal --list-active-keymap-bindings is missing the ctrl-shift-1 profile-slot entry"
    }
    Assert-ActiveProfileSlotBindingMatch `
        -Matches $profileSlotEntry.matches `
        -Context "zed-terminal --list-active-keymap-bindings"
}

function Assert-StartupConfigValidationJson {
    param(
        [Parameter(Mandatory = $true)]$Report,
        [Parameter(Mandatory = $true)][string]$ExpectedStartupConfigFile
    )

    if (
        $Report.status -ne "ok" -or
        $Report.startup_config_file -ne $ExpectedStartupConfigFile -or
        [int64]$Report.layout_count -le 0 -or
        [int64]$Report.tab_count -le 0
    ) {
        throw "zed-terminal --validate-startup-config did not report expected startup config validation status"
    }
}

function Assert-StartupLayoutJson {
    param(
        [Parameter(Mandatory = $true)]$Report,
        [Parameter(Mandatory = $true)][string]$ExpectedStartupConfigFile
    )

    $tabs = @($Report.tabs)
    if (
        $Report.status -ne "ok" -or
        $Report.startup_config_file -ne $ExpectedStartupConfigFile -or
        $null -eq $Report.new_terminal_tab -or
        [int64]$Report.tab_count -le 0 -or
        $tabs.Count -ne [int64]$Report.tab_count
    ) {
        throw "zed-terminal --print-startup-layout did not report expected startup layout status"
    }

    if ($Report.new_terminal_tab.placement -ne "tab" -or $null -eq $Report.new_terminal_tab.env_count) {
        throw "zed-terminal --print-startup-layout reported an invalid new terminal tab template"
    }

    for ($index = 0; $index -lt $tabs.Count; $index++) {
        $tab = $tabs[$index]
        if (
            [int64]$tab.tab -ne ($index + 1) -or
            [string]::IsNullOrWhiteSpace([string]$tab.kind) -or
            [string]::IsNullOrWhiteSpace([string]$tab.placement) -or
            $null -eq $tab.env_count
        ) {
            throw "zed-terminal --print-startup-layout reported an invalid startup tab entry"
        }
    }
}

function Assert-StartupDiscoveryJson {
    param(
        [Parameter(Mandatory = $true)][string]$StartupDescriptionJson,
        [Parameter(Mandatory = $true)][string]$ProfileListJson,
        [string]$ProfileDescriptionJson,
        [Parameter(Mandatory = $true)][string]$ExpectedStartupConfigFile
    )

    $StartupDescription = ConvertFrom-Json -InputObject $StartupDescriptionJson
    $ProfileList = ConvertFrom-Json -InputObject $ProfileListJson
    $ProfileDescription = $null
    if (-not [string]::IsNullOrWhiteSpace($ProfileDescriptionJson)) {
        $ProfileDescription = ConvertFrom-Json -InputObject $ProfileDescriptionJson
    }

    $startupTabs = @($StartupDescription.tabs)
    $profiles = @($ProfileList.profiles)
    $startupTabCount = [int64]@($StartupDescription.tab_count)[0]
    $startupProfileCount = [int64]@($StartupDescription.profile_count)[0]
    $startupVisibleProfileCount = [int64]@($StartupDescription.visible_profile_count)[0]
    $startupHiddenProfileCount = [int64]@($StartupDescription.hidden_profile_count)[0]
    $totalProfileCount = [int64]@($ProfileList.total_count)[0]
    $visibleProfileCount = [int64]@($ProfileList.visible_count)[0]
    $hiddenProfileCount = [int64]@($ProfileList.hidden_count)[0]
    if (
        $StartupDescription.status -ne "ok" -or
        $StartupDescription.source -ne "file" -or
        $StartupDescription.startup_config_file -ne $ExpectedStartupConfigFile -or
        $null -eq $StartupDescription.shell -or
        $startupTabs.Count -ne $startupTabCount -or
        $startupProfileCount -lt 0 -or
        $startupVisibleProfileCount -lt 0 -or
        $startupHiddenProfileCount -lt 0
    ) {
        throw "zed-terminal --describe-startup did not report expected startup discovery status"
    }

    if (
        $ProfileList.startup_config_file -ne $ExpectedStartupConfigFile -or
        $ProfileList.include_hidden -ne $false -or
        $profiles.Count -ne $visibleProfileCount -or
        $totalProfileCount -lt $visibleProfileCount -or
        $hiddenProfileCount -lt 0
    ) {
        throw "zed-terminal --list-profiles did not report expected startup profile discovery status"
    }

    for ($index = 0; $index -lt $profiles.Count; $index++) {
        $profile = $profiles[$index]
        if ($profile.hidden -eq $true -or [int64]$profile.visible_slot -ne ($index + 1)) {
            throw "zed-terminal --list-profiles did not report stable visible profile slot metadata"
        }
        $expectedShortcut = if (($index + 1) -le 9) { "ctrl-shift-$($index + 1)" } else { $null }
        if ($profile.visible_slot_shortcut -ne $expectedShortcut) {
            throw "zed-terminal --list-profiles did not report stable visible profile shortcut metadata"
        }
    }

    if ($profiles.Count -gt 0) {
        $firstVisibleProfile = $profiles[0]
        if (
            $null -eq $ProfileDescription -or
            $ProfileDescription.status -ne "ok" -or
            $ProfileDescription.profile -ne $firstVisibleProfile.name -or
            $ProfileDescription.startup_config_file -ne $ExpectedStartupConfigFile -or
            $ProfileDescription.hidden -eq $true -or
            [int64]$ProfileDescription.visible_slot -ne [int64]$firstVisibleProfile.visible_slot -or
            $ProfileDescription.visible_slot_shortcut -ne $firstVisibleProfile.visible_slot_shortcut
        ) {
            throw "zed-terminal --describe-profile did not report stable visible profile slot metadata"
        }
    }

    if (
        $startupProfileCount -ne $totalProfileCount -or
        $startupVisibleProfileCount -ne $visibleProfileCount -or
        $startupHiddenProfileCount -ne $hiddenProfileCount
    ) {
        throw "zed-terminal startup discovery commands reported inconsistent profile counts"
    }
}

function Assert-SettingsBackupJson {
    param(
        [Parameter(Mandatory = $true)]$Report,
        [Parameter(Mandatory = $true)][string]$BackupFile
    )

    $files = @($Report.files)
    $labels = @($files | ForEach-Object { $_.label })
    if (
        $Report.status -ne "ok" -or
        $Report.backup_file -ne $BackupFile -or
        $files.Count -ne 2 -or
        $labels -notcontains "settings_file" -or
        $labels -notcontains "global_settings_file"
    ) {
        throw "zed-terminal --backup-settings did not report expected settings backup status"
    }

    foreach ($file in $files) {
        if ($null -eq $file.exists) {
            throw "zed-terminal --backup-settings reported an invalid settings backup file entry"
        }
    }
}

function Assert-SettingsBackupCheckJson {
    param(
        [Parameter(Mandatory = $true)]$Report,
        [Parameter(Mandatory = $true)][string]$BackupFile,
        [Parameter(Mandatory = $true)][bool]$ExpectedMatches
    )

    $files = @($Report.files)
    if ($Report.status -ne "ok" -or $Report.backup_file -ne $BackupFile -or $Report.matches -ne $ExpectedMatches -or $files.Count -ne 2) {
        throw "zed-terminal settings backup check/diff did not report expected status"
    }
}

function Assert-SettingsRestoreJson {
    param(
        [Parameter(Mandatory = $true)]$Report,
        [Parameter(Mandatory = $true)][string]$RestoreFile
    )

    $files = @($Report.files)
    if ($Report.status -ne "ok" -or $Report.restore_file -ne $RestoreFile -or $files.Count -ne 2) {
        throw "zed-terminal --restore-settings did not report expected settings restore status"
    }
}

function Assert-StartupConfigBackupJson {
    param(
        [Parameter(Mandatory = $true)]$Report,
        [Parameter(Mandatory = $true)][string]$StartupConfigFile,
        [Parameter(Mandatory = $true)][string]$BackupFile
    )

    if (
        $Report.status -ne "ok" -or
        $Report.startup_config_file -ne $StartupConfigFile -or
        $Report.backup_file -ne $BackupFile -or
        [int64]$Report.byte_count -le 0 -or
        [int64]$Report.layout_count -le 0 -or
        [int64]$Report.tab_count -le 0 -or
        [int64]$Report.profile_count -lt 0
    ) {
        throw "zed-terminal --backup-startup-config did not report expected startup config backup status"
    }
}

function Assert-StartupConfigBackupCheckJson {
    param(
        [Parameter(Mandatory = $true)]$Report,
        [Parameter(Mandatory = $true)][string]$StartupConfigFile,
        [Parameter(Mandatory = $true)][string]$BackupFile,
        [Parameter(Mandatory = $true)][bool]$ExpectedMatches
    )

    if (
        $Report.status -ne "ok" -or
        $Report.startup_config_file -ne $StartupConfigFile -or
        $Report.backup_file -ne $BackupFile -or
        $Report.matches -ne $ExpectedMatches -or
        [int64]$Report.startup_byte_count -le 0 -or
        [int64]$Report.backup_byte_count -le 0 -or
        [int64]$Report.startup_layout_count -le 0 -or
        [int64]$Report.backup_layout_count -le 0 -or
        [int64]$Report.startup_tab_count -le 0 -or
        [int64]$Report.backup_tab_count -le 0 -or
        [int64]$Report.startup_profile_count -lt 0 -or
        [int64]$Report.backup_profile_count -lt 0
    ) {
        throw "zed-terminal --check-startup-config-backup did not report expected startup config backup check status"
    }
}

function Assert-StartupConfigBackupDiffJson {
    param(
        [Parameter(Mandatory = $true)]$Report,
        [Parameter(Mandatory = $true)][string]$StartupConfigFile,
        [Parameter(Mandatory = $true)][string]$BackupFile,
        [Parameter(Mandatory = $true)][bool]$ExpectedTextMatches,
        [Parameter(Mandatory = $true)][bool]$ExpectedConfigMatches
    )

    if (
        $Report.status -ne "ok" -or
        $Report.startup_config_file -ne $StartupConfigFile -or
        $Report.backup_file -ne $BackupFile -or
        $Report.text_matches -ne $ExpectedTextMatches -or
        $Report.config_matches -ne $ExpectedConfigMatches -or
        [int64]$Report.startup_byte_count -le 0 -or
        [int64]$Report.backup_byte_count -le 0 -or
        [int64]$Report.startup_layout_count -le 0 -or
        [int64]$Report.backup_layout_count -le 0 -or
        [int64]$Report.startup_tab_count -le 0 -or
        [int64]$Report.backup_tab_count -le 0 -or
        [int64]$Report.startup_profile_count -lt 0 -or
        [int64]$Report.backup_profile_count -lt 0
    ) {
        throw "zed-terminal --diff-startup-config-backup did not report expected startup config backup diff status"
    }
}

function Assert-StartupConfigRestoreJson {
    param(
        [Parameter(Mandatory = $true)]$Report,
        [Parameter(Mandatory = $true)][string]$StartupConfigFile,
        [Parameter(Mandatory = $true)][string]$RestoreFile
    )

    if (
        $Report.status -ne "ok" -or
        $Report.startup_config_file -ne $StartupConfigFile -or
        $Report.restore_file -ne $RestoreFile -or
        [int64]$Report.byte_count -le 0 -or
        [int64]$Report.layout_count -le 0 -or
        [int64]$Report.tab_count -le 0 -or
        [int64]$Report.profile_count -lt 0
    ) {
        throw "zed-terminal --restore-startup-config did not report expected startup config restore status"
    }
}

function Assert-KeymapBackupJson {
    param(
        [Parameter(Mandatory = $true)]$Report,
        [Parameter(Mandatory = $true)][string]$KeymapFile,
        [Parameter(Mandatory = $true)][string]$BackupFile
    )

    if (
        $Report.status -ne "ok" -or
        $Report.keymap_file -ne $KeymapFile -or
        $Report.backup_file -ne $BackupFile -or
        [int64]$Report.byte_count -le 0 -or
        [int64]$Report.section_count -le 0 -or
        [int64]$Report.binding_count -lt 0 -or
        [int64]$Report.unbind_count -lt 0
    ) {
        throw "zed-terminal --backup-keymap did not report expected keymap backup status"
    }
}

function Assert-KeymapBackupCheckJson {
    param(
        [Parameter(Mandatory = $true)]$Report,
        [Parameter(Mandatory = $true)][string]$KeymapFile,
        [Parameter(Mandatory = $true)][string]$BackupFile,
        [Parameter(Mandatory = $true)][bool]$ExpectedMatches
    )

    if (
        $Report.status -ne "ok" -or
        $Report.keymap_file -ne $KeymapFile -or
        $Report.backup_file -ne $BackupFile -or
        $Report.matches -ne $ExpectedMatches -or
        [int64]$Report.keymap_byte_count -le 0 -or
        [int64]$Report.backup_byte_count -le 0 -or
        [int64]$Report.keymap_section_count -le 0 -or
        [int64]$Report.backup_section_count -le 0
    ) {
        throw "zed-terminal --check-keymap-backup did not report expected keymap backup check status"
    }
}

function Assert-KeymapBackupDiffJson {
    param(
        [Parameter(Mandatory = $true)]$Report,
        [Parameter(Mandatory = $true)][string]$KeymapFile,
        [Parameter(Mandatory = $true)][string]$BackupFile,
        [Parameter(Mandatory = $true)][bool]$ExpectedTextMatches,
        [Parameter(Mandatory = $true)][bool]$ExpectedKeymapMatches
    )

    if (
        $Report.status -ne "ok" -or
        $Report.keymap_file -ne $KeymapFile -or
        $Report.backup_file -ne $BackupFile -or
        $Report.text_matches -ne $ExpectedTextMatches -or
        $Report.keymap_matches -ne $ExpectedKeymapMatches -or
        [int64]$Report.keymap_byte_count -le 0 -or
        [int64]$Report.backup_byte_count -le 0 -or
        [int64]$Report.keymap_section_count -le 0 -or
        [int64]$Report.backup_section_count -le 0
    ) {
        throw "zed-terminal --diff-keymap-backup did not report expected keymap backup diff status"
    }
}

function Assert-KeymapRestoreJson {
    param(
        [Parameter(Mandatory = $true)]$Report,
        [Parameter(Mandatory = $true)][string]$KeymapFile,
        [Parameter(Mandatory = $true)][string]$RestoreFile
    )

    if (
        $Report.status -ne "ok" -or
        $Report.keymap_file -ne $KeymapFile -or
        $Report.restore_file -ne $RestoreFile -or
        [int64]$Report.byte_count -le 0 -or
        [int64]$Report.section_count -le 0 -or
        [int64]$Report.binding_count -lt 0 -or
        [int64]$Report.unbind_count -lt 0
    ) {
        throw "zed-terminal --restore-keymap did not report expected keymap restore status"
    }
}

function Assert-ConfigBundleJson {
    param(
        [Parameter(Mandatory = $true)]$Report,
        [Parameter(Mandatory = $true)][string]$BundleFile,
        [Parameter(Mandatory = $true)][bool]$HasMatches,
        [Parameter()][bool]$ExpectedMatches = $true
    )

    $files = @($Report.files)
    $labels = @($files | ForEach-Object { $_.label })
    if (
        $Report.status -ne "ok" -or
        $Report.bundle_file -ne $BundleFile -or
        $files.Count -ne 4 -or
        $labels -notcontains "startup_config_file" -or
        $labels -notcontains "keymap_file" -or
        $labels -notcontains "settings_file" -or
        $labels -notcontains "global_settings_file"
    ) {
        throw "zed-terminal config bundle command did not report expected status"
    }

    if ($HasMatches -and $Report.matches -ne $ExpectedMatches) {
        throw "zed-terminal config bundle command reported unexpected matches value"
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

function Assert-SupportBundleJson {
    param(
        [Parameter(Mandatory = $true)]$Report,
        [Parameter(Mandatory = $true)][string]$BundleDir
    )

    $files = @($Report.files)
    $expectedFiles = @{
        manifest = Join-Path $BundleDir "zed-terminal-support-bundle.json"
        support_info = Join-Path $BundleDir "zed-terminal-support-info.txt"
        diagnostics = Join-Path $BundleDir "zed-terminal-diagnostics.json"
        paths = Join-Path $BundleDir "zed-terminal-paths.json"
        file_metadata = Join-Path $BundleDir "zed-terminal-file-metadata.json"
        readme = Join-Path $BundleDir "README.txt"
    }
    if (
        $Report.status -ne "ok" -or
        $Report.format -ne "zed-terminal-support-bundle" -or
        $Report.version -ne 1 -or
        $Report.bundle_dir -ne $BundleDir -or
        $Report.diagnostics_status -ne "ok" -or
        $Report.file_count -ne 6 -or
        $files.Count -ne 6
    ) {
        throw "zed-terminal --support-bundle did not report expected status"
    }

    if ($Report.manifest_file -ne $expectedFiles["manifest"]) {
        throw "zed-terminal --support-bundle did not report the expected manifest path"
    }
    Assert-SupportBundleFileReports `
        -Files $files `
        -ExpectedFiles $expectedFiles `
        -Context "zed-terminal --support-bundle output"
    $manifestReport = @($files | Where-Object { $_.label -eq "manifest" })[0]
    if ([int64]$Report.manifest_byte_count -ne [int64]$manifestReport.byte_count) {
        throw "zed-terminal --support-bundle manifest byte count did not match its file report"
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

function Assert-SupportBundleArtifacts {
    param(
        [Parameter(Mandatory = $true)][string]$BundleDir,
        [Parameter(Mandatory = $true)][string]$DataDir,
        [Parameter(Mandatory = $true)][string]$ConfigDir,
        [Parameter(Mandatory = $true)][ValidateSet("custom", "portable")][string]$ExpectedPathMode,
        [Parameter()][string[]]$SensitiveText = @()
    )

    $manifestFile = Join-Path $BundleDir "zed-terminal-support-bundle.json"
    $metadataFile = Join-Path $BundleDir "zed-terminal-file-metadata.json"
    $supportInfoFile = Join-Path $BundleDir "zed-terminal-support-info.txt"
    $diagnosticsFile = Join-Path $BundleDir "zed-terminal-diagnostics.json"
    $pathsFile = Join-Path $BundleDir "zed-terminal-paths.json"
    $readmeFile = Join-Path $BundleDir "README.txt"
    $expectedFiles = @{
        "zed-terminal-support-bundle.json" = $manifestFile
        "zed-terminal-file-metadata.json" = $metadataFile
        "zed-terminal-support-info.txt" = $supportInfoFile
        "zed-terminal-diagnostics.json" = $diagnosticsFile
        "zed-terminal-paths.json" = $pathsFile
        "README.txt" = $readmeFile
    }

    $actualFiles = @(Get-ChildItem -LiteralPath $BundleDir -File)
    if ($actualFiles.Count -ne $expectedFiles.Count) {
        throw "zed-terminal support bundle wrote $($actualFiles.Count) files; expected $($expectedFiles.Count)"
    }
    foreach ($file in $actualFiles) {
        if (-not $expectedFiles.ContainsKey($file.Name)) {
            throw "zed-terminal support bundle wrote unexpected file: $($file.Name)"
        }
    }
    foreach ($file in $expectedFiles.Values) {
        if (-not (Test-Path -LiteralPath $file -PathType Leaf) -or (Get-Item -LiteralPath $file).Length -le 0) {
            throw "zed-terminal support bundle did not write expected nonempty file: $file"
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
        $manifest.path_mode -ne $ExpectedPathMode -or
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
    Assert-PathsJson `
        -Paths $paths `
        -ExpectedMode $ExpectedPathMode `
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
        $metadata.redaction.includes_environment_values -ne $false
    ) {
        throw "zed-terminal support bundle metadata did not report expected redaction policy"
    }
    $expectedMetadataCount = 14
    if (@($metadata.files).Count -ne $expectedMetadataCount) {
        throw "zed-terminal support bundle metadata reported $(@($metadata.files).Count) files; expected $expectedMetadataCount"
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
            throw "zed-terminal support bundle leaked redaction fixture text: $secret"
        }
    }
}

function Invoke-SupportBundleSmoke {
    param(
        [Parameter(Mandatory = $true)][string]$Binary,
        [Parameter(Mandatory = $true)][string]$DataDir,
        [Parameter(Mandatory = $true)][string]$ConfigDir,
        [Parameter(Mandatory = $true)][string]$BundleDir,
        [Parameter(Mandatory = $true)][string]$WorkingDirectory
    )

    New-Item -ItemType Directory -Force -Path $DataDir, $ConfigDir | Out-Null
    $initConfig = Invoke-CheckedProcess -FilePath $Binary -Arguments @(
        "--user-data-dir", $DataDir,
        "--config-dir", $ConfigDir,
        "--init-config",
        "--init-config-format", "json"
    ) -WorkingDirectory $WorkingDirectory
    Assert-ConfigInitializationJson `
        -Report ($initConfig.Stdout | ConvertFrom-Json) `
        -ConfigDir $ConfigDir
    New-Item -ItemType Directory -Force -Path (Join-Path $DataDir "logs") | Out-Null
    Add-Content -LiteralPath (Join-Path $ConfigDir "terminal.json") -Value "`n// do-not-log-package-startup-secret"
    Add-Content -LiteralPath (Join-Path $ConfigDir "settings.json") -Value "`n// do-not-log-package-settings-secret"
    Set-Content -LiteralPath (Join-Path (Join-Path $DataDir "logs") "Zed Terminal.log") -Value "do-not-log-package-log-secret" -Encoding utf8

    $result = Invoke-CheckedProcess -FilePath $Binary -Arguments @(
        "--user-data-dir", $DataDir,
        "--config-dir", $ConfigDir,
        "--support-bundle",
        "--support-bundle-dir", $BundleDir,
        "--support-bundle-format", "json"
    ) -WorkingDirectory $WorkingDirectory
    $report = $result.Stdout | ConvertFrom-Json
    Assert-SupportBundleJson $report $BundleDir
    Assert-SupportBundleArtifacts `
        -BundleDir $BundleDir `
        -DataDir $DataDir `
        -ConfigDir $ConfigDir `
        -ExpectedPathMode "custom" `
        -SensitiveText @(
            "do-not-log-package-startup-secret",
            "do-not-log-package-settings-secret",
            "do-not-log-package-log-secret"
        )
}

function Invoke-SettingsBackupSmoke {
    param(
        [Parameter(Mandatory = $true)][string]$Binary,
        [Parameter(Mandatory = $true)][string]$DataDir,
        [Parameter(Mandatory = $true)][string]$ConfigDir,
        [Parameter(Mandatory = $true)][string]$BackupFile,
        [Parameter(Mandatory = $true)][string]$WorkingDirectory
    )

    $backup = Invoke-CheckedProcess -FilePath $Binary -Arguments @(
        "--user-data-dir", $DataDir,
        "--config-dir", $ConfigDir,
        "--backup-settings",
        "--backup-settings-file", $BackupFile,
        "--backup-settings-format", "json"
    ) -WorkingDirectory $WorkingDirectory
    Assert-SettingsBackupJson ($backup.Stdout | ConvertFrom-Json) $BackupFile
    if (-not (Test-Path -LiteralPath $BackupFile -PathType Leaf)) {
        throw "zed-terminal --backup-settings did not write the requested backup file"
    }

    $check = Invoke-CheckedProcess -FilePath $Binary -Arguments @(
        "--user-data-dir", $DataDir,
        "--config-dir", $ConfigDir,
        "--check-settings-backup",
        "--check-settings-backup-file", $BackupFile,
        "--check-settings-backup-format", "json"
    ) -WorkingDirectory $WorkingDirectory
    Assert-SettingsBackupCheckJson ($check.Stdout | ConvertFrom-Json) $BackupFile $true

    $diff = Invoke-CheckedProcess -FilePath $Binary -Arguments @(
        "--user-data-dir", $DataDir,
        "--config-dir", $ConfigDir,
        "--diff-settings-backup",
        "--diff-settings-backup-file", $BackupFile,
        "--diff-settings-backup-format", "json"
    ) -WorkingDirectory $WorkingDirectory
    Assert-SettingsBackupCheckJson ($diff.Stdout | ConvertFrom-Json) $BackupFile $true

    Add-Content -LiteralPath (Join-Path $ConfigDir "settings.json") -Value "`n// package settings drift"
    $drift = Invoke-CheckedProcess -FilePath $Binary -Arguments @(
        "--user-data-dir", $DataDir,
        "--config-dir", $ConfigDir,
        "--diff-settings-backup",
        "--diff-settings-backup-file", $BackupFile,
        "--diff-settings-backup-format", "json"
    ) -WorkingDirectory $WorkingDirectory
    $driftJson = $drift.Stdout | ConvertFrom-Json
    Assert-SettingsBackupCheckJson $driftJson $BackupFile $false
    $settingsFileDiff = @($driftJson.files) | Where-Object { $_.label -eq "settings_file" } | Select-Object -First 1
    if (-not $settingsFileDiff -or $settingsFileDiff.text_matches -or -not $settingsFileDiff.settings_matches -or @($settingsFileDiff.categories) -notcontains "text") {
        throw "zed-terminal --diff-settings-backup did not distinguish text-only settings drift"
    }

    Set-Content -LiteralPath (Join-Path $ConfigDir "settings.json") -Value "{ broken settings" -NoNewline
    $restore = Invoke-CheckedProcess -FilePath $Binary -Arguments @(
        "--user-data-dir", $DataDir,
        "--config-dir", $ConfigDir,
        "--restore-settings",
        "--restore-settings-file", $BackupFile,
        "--restore-settings-format", "json"
    ) -WorkingDirectory $WorkingDirectory
    Assert-SettingsRestoreJson ($restore.Stdout | ConvertFrom-Json) $BackupFile

    $postRestore = Invoke-CheckedProcess -FilePath $Binary -Arguments @(
        "--user-data-dir", $DataDir,
        "--config-dir", $ConfigDir,
        "--check-settings-backup",
        "--check-settings-backup-file", $BackupFile,
        "--check-settings-backup-format", "json"
    ) -WorkingDirectory $WorkingDirectory
    Assert-SettingsBackupCheckJson ($postRestore.Stdout | ConvertFrom-Json) $BackupFile $true
}

function Invoke-StartupConfigBackupSmoke {
    param(
        [Parameter(Mandatory = $true)][string]$Binary,
        [Parameter(Mandatory = $true)][string]$DataDir,
        [Parameter(Mandatory = $true)][string]$ConfigDir,
        [Parameter(Mandatory = $true)][string]$BackupFile,
        [Parameter(Mandatory = $true)][string]$WorkingDirectory
    )

    $startupConfigFile = Join-Path $ConfigDir "terminal.json"
    $backup = Invoke-CheckedProcess -FilePath $Binary -Arguments @(
        "--user-data-dir", $DataDir,
        "--config-dir", $ConfigDir,
        "--backup-startup-config",
        "--backup-startup-config-file", $BackupFile,
        "--backup-startup-config-format", "json"
    ) -WorkingDirectory $WorkingDirectory
    Assert-StartupConfigBackupJson ($backup.Stdout | ConvertFrom-Json) $startupConfigFile $BackupFile
    if (-not (Test-Path -LiteralPath $BackupFile -PathType Leaf)) {
        throw "zed-terminal --backup-startup-config did not write the requested backup file"
    }

    $backupText = Get-Content -LiteralPath $BackupFile -Raw
    $startupConfigText = Get-Content -LiteralPath $startupConfigFile -Raw
    if ($backupText -ne $startupConfigText) {
        throw "zed-terminal --backup-startup-config did not preserve terminal.json exactly"
    }

    $check = Invoke-CheckedProcess -FilePath $Binary -Arguments @(
        "--user-data-dir", $DataDir,
        "--config-dir", $ConfigDir,
        "--check-startup-config-backup",
        "--check-startup-config-backup-file", $BackupFile,
        "--check-startup-config-backup-format", "json"
    ) -WorkingDirectory $WorkingDirectory
    Assert-StartupConfigBackupCheckJson ($check.Stdout | ConvertFrom-Json) $startupConfigFile $BackupFile $true

    $diff = Invoke-CheckedProcess -FilePath $Binary -Arguments @(
        "--user-data-dir", $DataDir,
        "--config-dir", $ConfigDir,
        "--diff-startup-config-backup",
        "--diff-startup-config-backup-file", $BackupFile,
        "--diff-startup-config-backup-format", "json"
    ) -WorkingDirectory $WorkingDirectory
    Assert-StartupConfigBackupDiffJson ($diff.Stdout | ConvertFrom-Json) $startupConfigFile $BackupFile $true $true

    Add-Content -LiteralPath $startupConfigFile -Value "`n// package startup config drift"
    $drift = Invoke-CheckedProcess -FilePath $Binary -Arguments @(
        "--user-data-dir", $DataDir,
        "--config-dir", $ConfigDir,
        "--diff-startup-config-backup",
        "--diff-startup-config-backup-file", $BackupFile,
        "--diff-startup-config-backup-format", "json"
    ) -WorkingDirectory $WorkingDirectory
    $driftJson = $drift.Stdout | ConvertFrom-Json
    Assert-StartupConfigBackupDiffJson $driftJson $startupConfigFile $BackupFile $false $true
    if (@($driftJson.categories) -notcontains "text") {
        throw "zed-terminal --diff-startup-config-backup did not distinguish text-only startup config drift"
    }

    Set-Content -LiteralPath $startupConfigFile -Value "{ broken startup config" -NoNewline
    $restore = Invoke-CheckedProcess -FilePath $Binary -Arguments @(
        "--user-data-dir", $DataDir,
        "--config-dir", $ConfigDir,
        "--restore-startup-config",
        "--restore-startup-config-file", $BackupFile,
        "--restore-startup-config-format", "json"
    ) -WorkingDirectory $WorkingDirectory
    Assert-StartupConfigRestoreJson ($restore.Stdout | ConvertFrom-Json) $startupConfigFile $BackupFile

    $restoredStartupConfigText = Get-Content -LiteralPath $startupConfigFile -Raw
    if ($restoredStartupConfigText -ne $backupText) {
        throw "zed-terminal --restore-startup-config did not restore terminal.json exactly"
    }

    $postRestore = Invoke-CheckedProcess -FilePath $Binary -Arguments @(
        "--user-data-dir", $DataDir,
        "--config-dir", $ConfigDir,
        "--check-startup-config-backup",
        "--check-startup-config-backup-file", $BackupFile,
        "--check-startup-config-backup-format", "json"
    ) -WorkingDirectory $WorkingDirectory
    Assert-StartupConfigBackupCheckJson ($postRestore.Stdout | ConvertFrom-Json) $startupConfigFile $BackupFile $true
}

function Invoke-KeymapBackupSmoke {
    param(
        [Parameter(Mandatory = $true)][string]$Binary,
        [Parameter(Mandatory = $true)][string]$DataDir,
        [Parameter(Mandatory = $true)][string]$ConfigDir,
        [Parameter(Mandatory = $true)][string]$BackupFile,
        [Parameter(Mandatory = $true)][string]$WorkingDirectory
    )

    $keymapFile = Join-Path $ConfigDir "keymap.json"
    $backup = Invoke-CheckedProcess -FilePath $Binary -Arguments @(
        "--user-data-dir", $DataDir,
        "--config-dir", $ConfigDir,
        "--backup-keymap",
        "--backup-keymap-file", $BackupFile,
        "--backup-keymap-format", "json"
    ) -WorkingDirectory $WorkingDirectory
    Assert-KeymapBackupJson ($backup.Stdout | ConvertFrom-Json) $keymapFile $BackupFile
    if (-not (Test-Path -LiteralPath $BackupFile -PathType Leaf)) {
        throw "zed-terminal --backup-keymap did not write the requested backup file"
    }

    $backupText = Get-Content -LiteralPath $BackupFile -Raw
    $keymapText = Get-Content -LiteralPath $keymapFile -Raw
    if ($backupText -ne $keymapText) {
        throw "zed-terminal --backup-keymap did not preserve keymap.json exactly"
    }

    $check = Invoke-CheckedProcess -FilePath $Binary -Arguments @(
        "--user-data-dir", $DataDir,
        "--config-dir", $ConfigDir,
        "--check-keymap-backup",
        "--check-keymap-backup-file", $BackupFile,
        "--check-keymap-backup-format", "json"
    ) -WorkingDirectory $WorkingDirectory
    Assert-KeymapBackupCheckJson ($check.Stdout | ConvertFrom-Json) $keymapFile $BackupFile $true

    $diff = Invoke-CheckedProcess -FilePath $Binary -Arguments @(
        "--user-data-dir", $DataDir,
        "--config-dir", $ConfigDir,
        "--diff-keymap-backup",
        "--diff-keymap-backup-file", $BackupFile,
        "--diff-keymap-backup-format", "json"
    ) -WorkingDirectory $WorkingDirectory
    Assert-KeymapBackupDiffJson ($diff.Stdout | ConvertFrom-Json) $keymapFile $BackupFile $true $true

    Add-Content -LiteralPath $keymapFile -Value "`n// package keymap drift"
    $drift = Invoke-CheckedProcess -FilePath $Binary -Arguments @(
        "--user-data-dir", $DataDir,
        "--config-dir", $ConfigDir,
        "--diff-keymap-backup",
        "--diff-keymap-backup-file", $BackupFile,
        "--diff-keymap-backup-format", "json"
    ) -WorkingDirectory $WorkingDirectory
    $driftJson = $drift.Stdout | ConvertFrom-Json
    Assert-KeymapBackupDiffJson $driftJson $keymapFile $BackupFile $false $true
    if (@($driftJson.categories) -notcontains "text") {
        throw "zed-terminal --diff-keymap-backup did not distinguish text-only keymap drift"
    }

    Set-Content -LiteralPath $keymapFile -Value "{ broken keymap" -NoNewline
    $restore = Invoke-CheckedProcess -FilePath $Binary -Arguments @(
        "--user-data-dir", $DataDir,
        "--config-dir", $ConfigDir,
        "--restore-keymap",
        "--restore-keymap-file", $BackupFile,
        "--restore-keymap-format", "json"
    ) -WorkingDirectory $WorkingDirectory
    Assert-KeymapRestoreJson ($restore.Stdout | ConvertFrom-Json) $keymapFile $BackupFile

    $restoredKeymapText = Get-Content -LiteralPath $keymapFile -Raw
    if ($restoredKeymapText -ne $backupText) {
        throw "zed-terminal --restore-keymap did not restore keymap.json exactly"
    }

    $postRestore = Invoke-CheckedProcess -FilePath $Binary -Arguments @(
        "--user-data-dir", $DataDir,
        "--config-dir", $ConfigDir,
        "--check-keymap-backup",
        "--check-keymap-backup-file", $BackupFile,
        "--check-keymap-backup-format", "json"
    ) -WorkingDirectory $WorkingDirectory
    Assert-KeymapBackupCheckJson ($postRestore.Stdout | ConvertFrom-Json) $keymapFile $BackupFile $true
}

function Invoke-StartupDiscoverySmoke {
    param(
        [Parameter(Mandatory = $true)][string]$Binary,
        [Parameter(Mandatory = $true)][string]$DataDir,
        [Parameter(Mandatory = $true)][string]$ConfigDir,
        [Parameter(Mandatory = $true)][string]$WorkingDirectory
    )

    $startupConfigFile = Join-Path $ConfigDir "terminal.json"
    $startupDescription = Invoke-CheckedProcess -FilePath $Binary -Arguments @(
        "--user-data-dir", $DataDir,
        "--config-dir", $ConfigDir,
        "--describe-startup",
        "--describe-startup-format", "json"
    ) -WorkingDirectory $WorkingDirectory

    $profileList = Invoke-CheckedProcess -FilePath $Binary -Arguments @(
        "--user-data-dir", $DataDir,
        "--config-dir", $ConfigDir,
        "--list-profiles",
        "--list-profiles-format", "json"
    ) -WorkingDirectory $WorkingDirectory

    $profileListJson = ConvertFrom-Json -InputObject $profileList.Stdout
    $profileSlots = Invoke-CheckedProcess -FilePath $Binary -Arguments @(
        "--user-data-dir", $DataDir,
        "--config-dir", $ConfigDir,
        "--list-profile-slots",
        "--list-profile-slots-format", "json"
    ) -WorkingDirectory $WorkingDirectory
    $profileSlotsJson = ConvertFrom-Json -InputObject $profileSlots.Stdout
    $profileSlotEntries = @($profileSlotsJson.slots)
    $expectedMappedProfileSlotCount = [Math]::Min([int64]$profileListJson.visible_count, [int64]9)
    if (
        $profileSlotsJson.startup_config_file -ne $startupConfigFile -or
        $profileSlotsJson.status -ne "ok" -or
        [int64]$profileSlotsJson.slot_count -ne 9 -or
        $profileSlotEntries.Count -ne 9 -or
        [int64]$profileSlotsJson.mapped_count -ne $expectedMappedProfileSlotCount
    ) {
        throw "zed-terminal --list-profile-slots did not report expected profile slot discovery status"
    }
    for ($index = 0; $index -lt $profileSlotEntries.Count; $index++) {
        $slot = $profileSlotEntries[$index]
        $expectedSlot = $index + 1
        if ([int64]$slot.slot -ne $expectedSlot -or $slot.shortcut -ne "ctrl-shift-$expectedSlot") {
            throw "zed-terminal --list-profile-slots did not report stable profile slot shortcut ordering"
        }
        $expectedProfile = @($profileListJson.profiles | Where-Object { [int64]$_.visible_slot -eq $expectedSlot } | Select-Object -First 1)
        if ($expectedProfile.Count -gt 0) {
            if ($null -eq $slot.profile -or $slot.profile.name -ne $expectedProfile[0].name -or $slot.profile.visible_slot_shortcut -ne $slot.shortcut) {
                throw "zed-terminal --list-profile-slots did not report the expected profile for a visible slot"
            }
        } elseif ($null -ne $slot.profile) {
            throw "zed-terminal --list-profile-slots reported a profile for an empty shortcut slot"
        }
    }

    $firstVisibleProfile = @($profileListJson.profiles) | Select-Object -First 1
    $profileDescriptionJson = $null
    if ($null -ne $firstVisibleProfile) {
        $profileDescription = Invoke-CheckedProcess -FilePath $Binary -Arguments @(
            "--user-data-dir", $DataDir,
            "--config-dir", $ConfigDir,
            "--describe-profile", $firstVisibleProfile.name,
            "--describe-profile-format", "json"
        ) -WorkingDirectory $WorkingDirectory
        $profileDescriptionJson = $profileDescription.Stdout
    }

    Assert-StartupDiscoveryJson `
        -StartupDescriptionJson $startupDescription.Stdout `
        -ProfileListJson $profileList.Stdout `
        -ProfileDescriptionJson $profileDescriptionJson `
        -ExpectedStartupConfigFile $startupConfigFile
}

function Invoke-ConfigBundleSmoke {
    param(
        [Parameter(Mandatory = $true)][string]$Binary,
        [Parameter(Mandatory = $true)][string]$DataDir,
        [Parameter(Mandatory = $true)][string]$ConfigDir,
        [Parameter(Mandatory = $true)][string]$BundleFile,
        [Parameter(Mandatory = $true)][string]$WorkingDirectory
    )

    $backup = Invoke-CheckedProcess -FilePath $Binary -Arguments @(
        "--user-data-dir", $DataDir,
        "--config-dir", $ConfigDir,
        "--backup-config-bundle",
        "--backup-config-bundle-file", $BundleFile,
        "--backup-config-bundle-format", "json"
    ) -WorkingDirectory $WorkingDirectory
    $backupJson = $backup.Stdout | ConvertFrom-Json
    Assert-ConfigBundleJson $backupJson $BundleFile $false
    if (-not (Test-Path -LiteralPath $BundleFile -PathType Leaf)) {
        throw "zed-terminal --backup-config-bundle did not write the requested bundle file"
    }
    if ($backupJson.bundle_byte_count -le 0) {
        throw "zed-terminal --backup-config-bundle reported an invalid bundle size"
    }

    $indexedBundleFile = Join-Path (Join-Path $DataDir "logs\zed-terminal-config-bundles") "zed-terminal-config-bundle-package-smoke.json"
    $indexedBackup = Invoke-CheckedProcess -FilePath $Binary -Arguments @(
        "--user-data-dir", $DataDir,
        "--config-dir", $ConfigDir,
        "--backup-config-bundle",
        "--backup-config-bundle-file", $indexedBundleFile,
        "--backup-config-bundle-format", "json"
    ) -WorkingDirectory $WorkingDirectory
    Assert-ConfigBundleJson ($indexedBackup.Stdout | ConvertFrom-Json) $indexedBundleFile $false

    $indexedBackups = Invoke-CheckedProcess -FilePath $Binary -Arguments @(
        "--user-data-dir", $DataDir,
        "--config-dir", $ConfigDir,
        "--list-config-bundle-backups",
        "--list-config-bundle-backups-format", "json"
    ) -WorkingDirectory $WorkingDirectory
    $indexedBackupsJson = $indexedBackups.Stdout | ConvertFrom-Json
    $indexedBackupEntries = @($indexedBackupsJson.backups)
    $indexedBackupEntry = $indexedBackupEntries | Where-Object { $_.path -eq $indexedBundleFile } | Select-Object -First 1
    if ($indexedBackupsJson.status -ne "ok" -or [int64]$indexedBackupsJson.backup_count -lt 1 -or [int64]$indexedBackupsJson.valid_count -lt 1 -or [int64]$indexedBackupsJson.invalid_count -ne 0 -or -not $indexedBackupEntry -or $indexedBackupEntry.valid -ne $true -or $indexedBackupEntry.format -ne "zed-terminal-config-bundle" -or [int64]$indexedBackupEntry.version -ne 1 -or [int64]$indexedBackupEntry.file_count -ne 4) {
        throw "zed-terminal --list-config-bundle-backups did not report the expected config bundle backup index"
    }

    $check = Invoke-CheckedProcess -FilePath $Binary -Arguments @(
        "--user-data-dir", $DataDir,
        "--config-dir", $ConfigDir,
        "--check-config-bundle",
        "--check-config-bundle-file", $BundleFile,
        "--check-config-bundle-format", "json"
    ) -WorkingDirectory $WorkingDirectory
    Assert-ConfigBundleJson ($check.Stdout | ConvertFrom-Json) $BundleFile $true $true

    Add-Content -LiteralPath (Join-Path $ConfigDir "terminal.json") -Value "`n// package startup drift"
    Add-Content -LiteralPath (Join-Path $ConfigDir "settings.json") -Value "`n// package settings drift"
    $drift = Invoke-CheckedProcess -FilePath $Binary -Arguments @(
        "--user-data-dir", $DataDir,
        "--config-dir", $ConfigDir,
        "--diff-config-bundle",
        "--diff-config-bundle-file", $BundleFile,
        "--diff-config-bundle-format", "json"
    ) -WorkingDirectory $WorkingDirectory
    $driftJson = $drift.Stdout | ConvertFrom-Json
    Assert-ConfigBundleJson $driftJson $BundleFile $true $false
    $startupDiff = @($driftJson.files) | Where-Object { $_.label -eq "startup_config_file" } | Select-Object -First 1
    $settingsDiff = @($driftJson.files) | Where-Object { $_.label -eq "settings_file" } | Select-Object -First 1
    if (-not $startupDiff -or $startupDiff.text_matches -or -not $startupDiff.config_matches -or @($startupDiff.categories) -notcontains "text") {
        throw "zed-terminal --diff-config-bundle did not distinguish text-only startup config drift"
    }
    if (-not $settingsDiff -or $settingsDiff.text_matches -or -not $settingsDiff.settings_matches -or @($settingsDiff.categories) -notcontains "text") {
        throw "zed-terminal --diff-config-bundle did not distinguish text-only settings drift"
    }

    Set-Content -LiteralPath (Join-Path $ConfigDir "terminal.json") -Value "{ broken startup" -NoNewline
    Set-Content -LiteralPath (Join-Path $ConfigDir "settings.json") -Value "{ broken settings" -NoNewline
    $restore = Invoke-CheckedProcess -FilePath $Binary -Arguments @(
        "--user-data-dir", $DataDir,
        "--config-dir", $ConfigDir,
        "--restore-config-bundle",
        "--restore-config-bundle-file", $BundleFile,
        "--restore-config-bundle-format", "json"
    ) -WorkingDirectory $WorkingDirectory
    Assert-ConfigBundleJson ($restore.Stdout | ConvertFrom-Json) $BundleFile $false

    $postRestore = Invoke-CheckedProcess -FilePath $Binary -Arguments @(
        "--user-data-dir", $DataDir,
        "--config-dir", $ConfigDir,
        "--check-config-bundle",
        "--check-config-bundle-file", $BundleFile,
        "--check-config-bundle-format", "json"
    ) -WorkingDirectory $WorkingDirectory
    Assert-ConfigBundleJson ($postRestore.Stdout | ConvertFrom-Json) $BundleFile $true $true
}

function Assert-PackageManifest {
    param(
        [Parameter(Mandatory = $true)][string]$PackageDir,
        [Parameter(Mandatory = $true)][string]$ManifestFile,
        [Parameter(Mandatory = $true)][string]$PackageName,
        [Parameter(Mandatory = $true)][string]$Version,
        [Parameter(Mandatory = $true)][string]$BuildProfile,
        [Parameter(Mandatory = $true)][string]$Platform,
        [Parameter(Mandatory = $true)][string]$Architecture,
        [Parameter(Mandatory = $true)][string]$BinaryFileName,
        [Parameter(Mandatory = $true)][string]$BinaryHash
    )

    if (-not (Test-Path -LiteralPath $ManifestFile -PathType Leaf)) {
        throw "package manifest was not written: $ManifestFile"
    }

    $manifest = Get-Content -LiteralPath $ManifestFile -Raw | ConvertFrom-Json
    if (
        $manifest.status -ne "ok" -or
        $manifest.app_name -ne "Zed Terminal" -or
        $manifest.package_name -ne $PackageName -or
        $manifest.version -ne $Version -or
        $manifest.build_profile -ne $BuildProfile -or
        $manifest.platform -ne $Platform -or
        $manifest.architecture -ne $Architecture -or
        $manifest.binary -ne $BinaryFileName -or
        $manifest.binary_sha256 -ne $BinaryHash -or
        -not $manifest.version_info -or
        -not $manifest.source_control -or
        $manifest.config_template_dir -ne "config-template"
    ) {
        throw "package manifest metadata did not match the package that was just built"
    }
    if ($manifest.source_control.git_commit -ne $manifest.git_commit) {
        throw "package manifest git commit did not match source control metadata"
    }
    if (
        $manifest.source_control.git_available -eq $true -and
        $manifest.source_control.git_commit -notmatch '^[a-f0-9]{40}$'
    ) {
        throw "package manifest source control commit was not a full SHA"
    }
    if (
        $manifest.source_control.git_available -eq $true -and
        $manifest.source_control.git_dirty -ne $true -and
        $manifest.source_control.git_dirty -ne $false
    ) {
        throw "package manifest source control dirty flag was not boolean"
    }
    if (
        $manifest.source_control.git_available -eq $true -and
        [int]$manifest.source_control.git_status_entry_count -lt 0
    ) {
        throw "package manifest source control status count was invalid"
    }

    Assert-VersionInfoJson `
        -VersionInfo $manifest.version_info `
        -Version $Version `
        -Platform $Platform `
        -Architecture $Architecture

    foreach ($validationName in @(
        "help",
        "version_info",
        "paths",
        "portable_paths",
        "init_config",
        "settings_schema",
        "startup_schema",
        "keymap_schema",
        "default_keymap",
        "default_keymap_reference",
        "licenses",
        "git_provenance",
        "startup_layout",
        "startup_discovery",
        "startup_validation",
        "settings_validation",
        "keymap_validation",
        "keymap_discovery",
        "active_keymap_discovery",
        "settings_backup",
        "startup_backup",
        "keymap_backup",
        "config_bundle",
        "doctor",
        "support_info",
        "support_bundle",
        "readme",
        "manifest"
    )) {
        if ($manifest.validation.$validationName -ne "ok") {
            throw "package manifest validation entry was not ok: $validationName"
        }
    }

    Assert-PackageReadme `
        -PackageDir $PackageDir `
        -PackageName $PackageName `
        -BinaryFileName $BinaryFileName

    $requiredContent = @(
        $BinaryFileName,
        "README.md",
        "LICENSE-GPL",
        "LICENSE-APACHE",
        "default-keymap.json",
        "config-template\settings.json",
        "config-template\settings.schema.json",
        "config-template\global_settings.json",
        "config-template\keymap.json",
        "config-template\default-keymap.json",
        "config-template\terminal.json",
        "config-template\terminal.schema.json",
        "config-template\keymap.schema.json"
    )

    $contentEntries = @($manifest.contents)
    if ($contentEntries.Count -eq 0) {
        throw "package manifest did not list any package contents"
    }

    $manifestContentByPath = @{}
    foreach ($entry in $contentEntries) {
        $relativePath = [string]$entry.path
        if ((Normalize-PackageRelativePath $relativePath) -eq "zed-terminal-package.json") {
            throw "package manifest contents must not include the manifest itself"
        }

        $contentPath = Resolve-PackageContentPath -Root $PackageDir -RelativePath $relativePath
        if (-not (Test-Path -LiteralPath $contentPath -PathType Leaf)) {
            throw "package manifest listed a missing file: $relativePath"
        }

        $normalizedPath = Normalize-PackageRelativePath $relativePath
        if ($manifestContentByPath.ContainsKey($normalizedPath)) {
            throw "package manifest listed duplicate content path: $relativePath"
        }
        $manifestContentByPath[$normalizedPath] = $entry

        $file = Get-Item -LiteralPath $contentPath
        $hash = (Get-FileHash -LiteralPath $contentPath -Algorithm SHA256).Hash.ToLowerInvariant()
        if ([int64]$entry.bytes -ne $file.Length -or $entry.sha256 -ne $hash) {
            throw "package manifest content hash or size mismatch: $relativePath"
        }
    }

    foreach ($relativePath in $requiredContent) {
        $normalizedPath = Normalize-PackageRelativePath $relativePath
        if (-not $manifestContentByPath.ContainsKey($normalizedPath)) {
            throw "package manifest is missing required content path: $relativePath"
        }
    }

    Assert-PackageConfigTemplateSchemas -ConfigTemplateDir (Join-Path $PackageDir "config-template")
    Assert-PackageDefaultKeymapReferences -PackageDir $PackageDir
    Assert-PackageLicenses -PackageDir $PackageDir

    $actualFiles = @(Get-ChildItem -LiteralPath $PackageDir -Recurse -File |
        Where-Object { $_.FullName -ne $ManifestFile }
    )
    foreach ($file in $actualFiles) {
        $relativePath = Normalize-PackageRelativePath (Get-RelativePath -Root $PackageDir -Path $file.FullName)
        if (-not $manifestContentByPath.ContainsKey($relativePath)) {
            throw "package manifest did not list package file: $relativePath"
        }
    }

    if ($actualFiles.Count -ne $contentEntries.Count) {
        throw "package manifest content count did not match package file count"
    }
}

function Assert-PackageZipArchive {
    param(
        [Parameter(Mandatory = $true)][string]$ZipFile,
        [Parameter(Mandatory = $true)][string]$ValidationRoot,
        [Parameter(Mandatory = $true)][string]$PackageName,
        [Parameter(Mandatory = $true)][string]$Version,
        [Parameter(Mandatory = $true)][string]$BuildProfile,
        [Parameter(Mandatory = $true)][string]$Platform,
        [Parameter(Mandatory = $true)][string]$Architecture,
        [Parameter(Mandatory = $true)][string]$BinaryFileName,
        [Parameter(Mandatory = $true)][string]$BinaryHash
    )

    if (-not (Test-Path -LiteralPath $ZipFile -PathType Leaf)) {
        throw "package zip archive was not written: $ZipFile"
    }

    $extractDir = Join-Path $ValidationRoot "zip-extract"
    if (Test-Path -LiteralPath $extractDir) {
        Remove-Item -LiteralPath $extractDir -Recurse -Force
    }
    New-Item -ItemType Directory -Force -Path $extractDir | Out-Null

    Expand-Archive -LiteralPath $ZipFile -DestinationPath $extractDir -Force

    $entries = @(Get-ChildItem -LiteralPath $extractDir -Force)
    if ($entries.Count -ne 1 -or -not $entries[0].PSIsContainer -or $entries[0].Name -ne $PackageName) {
        throw "package zip archive must contain one top-level package directory named $PackageName"
    }

    $extractedPackageDir = $entries[0].FullName
    Assert-PackageManifest `
        -PackageDir $extractedPackageDir `
        -ManifestFile (Join-Path $extractedPackageDir "zed-terminal-package.json") `
        -PackageName $PackageName `
        -Version $Version `
        -BuildProfile $BuildProfile `
        -Platform $Platform `
        -Architecture $Architecture `
        -BinaryFileName $BinaryFileName `
        -BinaryHash $BinaryHash

    $extractedBinary = Join-Path $extractedPackageDir $BinaryFileName
    $extractedConfigDir = Join-Path $extractedPackageDir "config-template"
    $extractedDataDir = Join-Path $ValidationRoot "zip-validation-data"
    New-Item -ItemType Directory -Force -Path $extractedDataDir | Out-Null

    $help = Invoke-CheckedProcess -FilePath $extractedBinary -Arguments @("--help") -WorkingDirectory $extractedPackageDir
    if ($help.Stdout -notmatch "Launch the standalone Zed terminal" -or $help.Stdout -notmatch "--init-config") {
        throw "extracted package zed-terminal --help did not expose expected standalone help text"
    }

    $versionInfo = Invoke-CheckedProcess -FilePath $extractedBinary -Arguments @(
        "--version-info",
        "--version-info-format", "json"
    ) -WorkingDirectory $extractedPackageDir
    $versionInfoJson = $versionInfo.Stdout | ConvertFrom-Json
    Assert-VersionInfoJson `
        -VersionInfo $versionInfoJson `
        -Version $Version `
        -Platform $Platform `
        -Architecture $Architecture

    $paths = Invoke-CheckedProcess -FilePath $extractedBinary -Arguments @(
        "--user-data-dir", $extractedDataDir,
        "--config-dir", $extractedConfigDir,
        "--paths",
        "--paths-format", "json"
    ) -WorkingDirectory $extractedPackageDir
    $pathsJson = $paths.Stdout | ConvertFrom-Json
    Assert-PathsJson `
        -Paths $pathsJson `
        -ExpectedMode "custom" `
        -ExpectedDataDir $extractedDataDir `
        -ExpectedConfigDir $extractedConfigDir `
        -RequireConfigAssets

    $portablePaths = Invoke-CheckedProcess -FilePath $extractedBinary -Arguments @(
        "--portable",
        "--paths",
        "--paths-format", "json"
    ) -WorkingDirectory $extractedPackageDir
    Assert-PortablePathsJson `
        -Paths ($portablePaths.Stdout | ConvertFrom-Json) `
        -ExpectedPackageDir $extractedPackageDir

    $doctor = Invoke-CheckedProcess -FilePath $extractedBinary -Arguments @(
        "--user-data-dir", $extractedDataDir,
        "--config-dir", $extractedConfigDir,
        "--doctor",
        "--doctor-format", "json"
    ) -WorkingDirectory $extractedPackageDir
    $doctorJson = $doctor.Stdout | ConvertFrom-Json
    if (
        $doctorJson.status -ne "ok" -or
        -not $doctorJson.directories -or
        -not $doctorJson.config_files -or
        $doctorJson.settings.status -ne "ok" -or
        $doctorJson.startup_config.status -ne "ok" -or
        $doctorJson.keymap.status -ne "ok"
    ) {
        throw "extracted package zed-terminal --doctor did not pass against the extracted config template"
    }

    $settingsValidation = Invoke-CheckedProcess -FilePath $extractedBinary -Arguments @(
        "--user-data-dir", $extractedDataDir,
        "--config-dir", $extractedConfigDir,
        "--validate-settings",
        "--validate-settings-format", "json"
    ) -WorkingDirectory $extractedPackageDir
    Assert-SettingsValidationJson ($settingsValidation.Stdout | ConvertFrom-Json)

    $startupValidation = Invoke-CheckedProcess -FilePath $extractedBinary -Arguments @(
        "--user-data-dir", $extractedDataDir,
        "--config-dir", $extractedConfigDir,
        "--validate-startup-config",
        "--validate-startup-config-format", "json"
    ) -WorkingDirectory $extractedPackageDir
    Assert-StartupConfigValidationJson `
        -Report ($startupValidation.Stdout | ConvertFrom-Json) `
        -ExpectedStartupConfigFile (Join-Path $extractedConfigDir "terminal.json")

    $startupLayout = Invoke-CheckedProcess -FilePath $extractedBinary -Arguments @(
        "--user-data-dir", $extractedDataDir,
        "--config-dir", $extractedConfigDir,
        "--print-startup-layout",
        "--startup-layout-format", "json"
    ) -WorkingDirectory $extractedPackageDir
    Assert-StartupLayoutJson `
        -Report ($startupLayout.Stdout | ConvertFrom-Json) `
        -ExpectedStartupConfigFile (Join-Path $extractedConfigDir "terminal.json")

    Invoke-StartupDiscoverySmoke `
        -Binary $extractedBinary `
        -DataDir $extractedDataDir `
        -ConfigDir $extractedConfigDir `
        -WorkingDirectory $extractedPackageDir

    $keymapValidation = Invoke-CheckedProcess -FilePath $extractedBinary -Arguments @(
        "--user-data-dir", $extractedDataDir,
        "--config-dir", $extractedConfigDir,
        "--validate-keymap",
        "--validate-keymap-format", "json"
    ) -WorkingDirectory $extractedPackageDir
    Assert-KeymapValidationJson `
        -Report ($keymapValidation.Stdout | ConvertFrom-Json) `
        -ExpectedKeymapFile (Join-Path $extractedConfigDir "keymap.json")

    Invoke-KeymapDiscoverySmoke `
        -Binary $extractedBinary `
        -DataDir $extractedDataDir `
        -ConfigDir $extractedConfigDir `
        -WorkingDirectory $extractedPackageDir

    Invoke-ActiveKeymapDiscoverySmoke `
        -Binary $extractedBinary `
        -DataDir $extractedDataDir `
        -ConfigDir $extractedConfigDir `
        -WorkingDirectory $extractedPackageDir

    Invoke-SettingsBackupSmoke `
        -Binary $extractedBinary `
        -DataDir $extractedDataDir `
        -ConfigDir $extractedConfigDir `
        -BackupFile (Join-Path $ValidationRoot "zip-settings.backup.json") `
        -WorkingDirectory $extractedPackageDir

    Invoke-StartupConfigBackupSmoke `
        -Binary $extractedBinary `
        -DataDir $extractedDataDir `
        -ConfigDir $extractedConfigDir `
        -BackupFile (Join-Path $ValidationRoot "zip-terminal.backup.json") `
        -WorkingDirectory $extractedPackageDir

    Invoke-KeymapBackupSmoke `
        -Binary $extractedBinary `
        -DataDir $extractedDataDir `
        -ConfigDir $extractedConfigDir `
        -BackupFile (Join-Path $ValidationRoot "zip-keymap.backup.json") `
        -WorkingDirectory $extractedPackageDir

    Invoke-ConfigBundleSmoke `
        -Binary $extractedBinary `
        -DataDir $extractedDataDir `
        -ConfigDir $extractedConfigDir `
        -BundleFile (Join-Path $ValidationRoot "zip-config.bundle.json") `
        -WorkingDirectory $extractedPackageDir

    Invoke-SupportBundleSmoke `
        -Binary $extractedBinary `
        -DataDir (Join-Path $ValidationRoot "zip-support-data") `
        -ConfigDir (Join-Path $ValidationRoot "zip-support-config") `
        -BundleDir (Join-Path $ValidationRoot "zip-support-bundle") `
        -WorkingDirectory $extractedPackageDir

    $supportInfo = Invoke-CheckedProcess -FilePath $extractedBinary -Arguments @(
        "--user-data-dir", $extractedDataDir,
        "--config-dir", $extractedConfigDir,
        "--support-info"
    ) -WorkingDirectory $extractedPackageDir
    if (
        $supportInfo.Stdout -notmatch "^Zed Terminal Support Info" -or
        $supportInfo.Stdout -notmatch "app_name: Zed Terminal" -or
        $supportInfo.Stdout -notmatch "status: ok" -or
        $supportInfo.Stdout -notmatch "diagnostics:"
    ) {
        throw "extracted package zed-terminal --support-info did not expose expected package diagnostics"
    }

    return (Get-FileHash -LiteralPath $ZipFile -Algorithm SHA256).Hash.ToLowerInvariant()
}

function Assert-ChecksumSidecar {
    param(
        [Parameter(Mandatory = $true)][string]$ChecksumFile,
        [Parameter(Mandatory = $true)][string]$ExpectedHash,
        [Parameter(Mandatory = $true)][string]$ExpectedFileName
    )

    if (-not (Test-Path -LiteralPath $ChecksumFile -PathType Leaf)) {
        throw "package checksum sidecar was not written: $ChecksumFile"
    }

    $content = (Get-Content -LiteralPath $ChecksumFile -Raw).Trim()
    $pattern = '^([a-f0-9]{64}) \*?(.+)$'
    if ($content -notmatch $pattern) {
        throw "package checksum sidecar has unexpected format: $ChecksumFile"
    }

    if ($Matches[1] -ne $ExpectedHash -or $Matches[2] -ne $ExpectedFileName) {
        throw "package checksum sidecar did not match zip hash or file name: $ChecksumFile"
    }
}

$version = Get-ZedTerminalVersion
$platform = Get-PlatformName
$architecture = Get-ArchitectureName
$binaryPath = Resolve-BinaryPath

if (-not $Binary -and -not $SkipBuild) {
    $cargoArgs = @("+stable", "build", "-p", "zed_terminal", "--bin", "zed-terminal")
    if ($BuildProfile -eq "release") {
        $cargoArgs += "--release"
    }
    Write-Host "building zed-terminal ($BuildProfile)"
    Invoke-CheckedProcess -FilePath "cargo" -Arguments $cargoArgs -EchoOutput | Out-Null
}

if (-not (Test-Path -LiteralPath $binaryPath -PathType Leaf)) {
    throw "zed-terminal binary not found: $binaryPath"
}

$timestamp = Get-Date -Format "yyyyMMdd-HHmmss-fff"
$runId = [guid]::NewGuid().ToString("N").Substring(0, 8)
$runDir = Join-Path $OutputDir "run-$timestamp-$runId"
$packageName = "zed-terminal-$version-$platform-$architecture"
$packageDir = Join-Path $runDir $packageName
$configTemplateDir = Join-Path $packageDir "config-template"
$validationDataDir = Join-Path $runDir "validation-data"
$validationConfigDir = Join-Path $runDir "validation-config"
$packageSummaryFile = if ($SummaryFile) {
    [System.IO.Path]::GetFullPath($SummaryFile)
} else {
    Join-Path $runDir "zed-terminal-package-summary.json"
}
New-Item -ItemType Directory -Force -Path $packageDir, $configTemplateDir, $validationDataDir, $validationConfigDir | Out-Null

$binaryFileName = Split-Path -Leaf $binaryPath
$packagedBinary = Join-Path $packageDir $binaryFileName
Copy-RequiredFile -Source $binaryPath -Destination $packagedBinary
Copy-RequiredFile -Source (Join-Path $repoRoot "LICENSE-GPL") -Destination (Join-Path $packageDir "LICENSE-GPL")
Copy-RequiredFile -Source (Join-Path $repoRoot "LICENSE-APACHE") -Destination (Join-Path $packageDir "LICENSE-APACHE")
Copy-RequiredFile -Source (Join-Path $repoRoot "assets\keymaps\zed-terminal.json") -Destination (Join-Path $packageDir "default-keymap.json")

$sourceControl = Get-GitSourceInfo
$gitCommit = $sourceControl.git_commit

$help = Invoke-CheckedProcess -FilePath $packagedBinary -Arguments @("--help")
if ($help.Stdout -notmatch "Launch the standalone Zed terminal" -or $help.Stdout -notmatch "--init-config") {
    throw "packaged zed-terminal --help did not expose expected standalone help text"
}

$versionInfo = Invoke-CheckedProcess -FilePath $packagedBinary -Arguments @(
    "--version-info",
    "--version-info-format", "json"
)
$versionInfoJson = $versionInfo.Stdout | ConvertFrom-Json
Assert-VersionInfoJson `
    -VersionInfo $versionInfoJson `
    -Version $version `
    -Platform $platform `
    -Architecture $architecture

$paths = Invoke-CheckedProcess -FilePath $packagedBinary -Arguments @(
    "--user-data-dir", $validationDataDir,
    "--config-dir", $validationConfigDir,
    "--paths",
    "--paths-format", "json"
)
$pathsJson = $paths.Stdout | ConvertFrom-Json
Assert-PathsJson `
    -Paths $pathsJson `
    -ExpectedMode "custom" `
    -ExpectedDataDir $validationDataDir `
    -ExpectedConfigDir $validationConfigDir

$portablePaths = Invoke-CheckedProcess -FilePath $packagedBinary -Arguments @(
    "--portable",
    "--paths",
    "--paths-format", "json"
)
Assert-PortablePathsJson `
    -Paths ($portablePaths.Stdout | ConvertFrom-Json) `
    -ExpectedPackageDir $packageDir

$initConfig = Invoke-CheckedProcess -FilePath $packagedBinary -Arguments @(
    "--user-data-dir", $validationDataDir,
    "--config-dir", $configTemplateDir,
    "--init-config",
    "--init-config-format", "json"
)
Assert-ConfigInitializationJson `
    -Report ($initConfig.Stdout | ConvertFrom-Json) `
    -ConfigDir $configTemplateDir

$startupSchema = Invoke-CheckedProcess -FilePath $packagedBinary -Arguments @(
    "--user-data-dir", $validationDataDir,
    "--config-dir", $configTemplateDir,
    "--print-startup-config-schema"
)
Set-Content -LiteralPath (Join-Path $configTemplateDir "terminal.schema.json") -Value $startupSchema.Stdout -Encoding utf8

$settingsSchema = Invoke-CheckedProcess -FilePath $packagedBinary -Arguments @(
    "--user-data-dir", $validationDataDir,
    "--config-dir", $configTemplateDir,
    "--print-settings-schema"
)
Set-Content -LiteralPath (Join-Path $configTemplateDir "settings.schema.json") -Value $settingsSchema.Stdout -Encoding utf8

$keymapSchema = Invoke-CheckedProcess -FilePath $packagedBinary -Arguments @(
    "--user-data-dir", $validationDataDir,
    "--config-dir", $configTemplateDir,
    "--print-keymap-schema"
)
Set-Content -LiteralPath (Join-Path $configTemplateDir "keymap.schema.json") -Value $keymapSchema.Stdout -Encoding utf8

$defaultKeymap = Invoke-CheckedProcess -FilePath $packagedBinary -Arguments @(
    "--user-data-dir", $validationDataDir,
    "--config-dir", $configTemplateDir,
    "--print-default-keymap"
)
Set-Content -LiteralPath (Join-Path $configTemplateDir "default-keymap.json") -Value $defaultKeymap.Stdout -Encoding utf8

foreach ($templateFile in @("settings.json", "settings.schema.json", "global_settings.json", "keymap.json", "default-keymap.json", "terminal.json", "terminal.schema.json", "keymap.schema.json")) {
    $path = Join-Path $configTemplateDir $templateFile
    if (-not (Test-Path -LiteralPath $path -PathType Leaf)) {
        throw "package config template is missing $templateFile"
    }
}
Assert-PackageConfigTemplateSchemas -ConfigTemplateDir $configTemplateDir

$configTemplatePaths = Invoke-CheckedProcess -FilePath $packagedBinary -Arguments @(
    "--user-data-dir", $validationDataDir,
    "--config-dir", $configTemplateDir,
    "--paths",
    "--paths-format", "json"
) -WorkingDirectory $packageDir
Assert-PathsJson `
    -Paths ($configTemplatePaths.Stdout | ConvertFrom-Json) `
    -ExpectedMode "custom" `
    -ExpectedDataDir $validationDataDir `
    -ExpectedConfigDir $configTemplateDir `
    -RequireConfigAssets

$doctor = Invoke-CheckedProcess -FilePath $packagedBinary -Arguments @(
    "--user-data-dir", $validationDataDir,
    "--config-dir", $configTemplateDir,
    "--doctor",
    "--doctor-format", "json"
) -WorkingDirectory $packageDir
$doctorJson = $doctor.Stdout | ConvertFrom-Json
if (
    $doctorJson.status -ne "ok" -or
    -not $doctorJson.directories -or
    -not $doctorJson.config_files -or
    $doctorJson.settings.status -ne "ok" -or
    $doctorJson.startup_config.status -ne "ok" -or
    $doctorJson.keymap.status -ne "ok"
) {
    throw "packaged zed-terminal --doctor did not pass against the generated config template"
}

$settingsValidation = Invoke-CheckedProcess -FilePath $packagedBinary -Arguments @(
    "--user-data-dir", $validationDataDir,
    "--config-dir", $configTemplateDir,
    "--validate-settings",
    "--validate-settings-format", "json"
) -WorkingDirectory $packageDir
Assert-SettingsValidationJson ($settingsValidation.Stdout | ConvertFrom-Json)

$startupValidation = Invoke-CheckedProcess -FilePath $packagedBinary -Arguments @(
    "--user-data-dir", $validationDataDir,
    "--config-dir", $configTemplateDir,
    "--validate-startup-config",
    "--validate-startup-config-format", "json"
) -WorkingDirectory $packageDir
Assert-StartupConfigValidationJson `
    -Report ($startupValidation.Stdout | ConvertFrom-Json) `
    -ExpectedStartupConfigFile (Join-Path $configTemplateDir "terminal.json")

$startupLayout = Invoke-CheckedProcess -FilePath $packagedBinary -Arguments @(
    "--user-data-dir", $validationDataDir,
    "--config-dir", $configTemplateDir,
    "--print-startup-layout",
    "--startup-layout-format", "json"
) -WorkingDirectory $packageDir
Assert-StartupLayoutJson `
    -Report ($startupLayout.Stdout | ConvertFrom-Json) `
    -ExpectedStartupConfigFile (Join-Path $configTemplateDir "terminal.json")

Invoke-StartupDiscoverySmoke `
    -Binary $packagedBinary `
    -DataDir $validationDataDir `
    -ConfigDir $configTemplateDir `
    -WorkingDirectory $packageDir

$keymapValidation = Invoke-CheckedProcess -FilePath $packagedBinary -Arguments @(
    "--user-data-dir", $validationDataDir,
    "--config-dir", $configTemplateDir,
    "--validate-keymap",
    "--validate-keymap-format", "json"
) -WorkingDirectory $packageDir
Assert-KeymapValidationJson `
    -Report ($keymapValidation.Stdout | ConvertFrom-Json) `
    -ExpectedKeymapFile (Join-Path $configTemplateDir "keymap.json")

Invoke-KeymapDiscoverySmoke `
    -Binary $packagedBinary `
    -DataDir $validationDataDir `
    -ConfigDir $configTemplateDir `
    -WorkingDirectory $packageDir

Invoke-ActiveKeymapDiscoverySmoke `
    -Binary $packagedBinary `
    -DataDir $validationDataDir `
    -ConfigDir $configTemplateDir `
    -WorkingDirectory $packageDir

Invoke-SettingsBackupSmoke `
    -Binary $packagedBinary `
    -DataDir $validationDataDir `
    -ConfigDir $configTemplateDir `
    -BackupFile (Join-Path $runDir "settings.backup.json") `
    -WorkingDirectory $packageDir

Invoke-StartupConfigBackupSmoke `
    -Binary $packagedBinary `
    -DataDir $validationDataDir `
    -ConfigDir $configTemplateDir `
    -BackupFile (Join-Path $runDir "terminal.backup.json") `
    -WorkingDirectory $packageDir

Invoke-KeymapBackupSmoke `
    -Binary $packagedBinary `
    -DataDir $validationDataDir `
    -ConfigDir $configTemplateDir `
    -BackupFile (Join-Path $runDir "keymap.backup.json") `
    -WorkingDirectory $packageDir

Invoke-ConfigBundleSmoke `
    -Binary $packagedBinary `
    -DataDir $validationDataDir `
    -ConfigDir $configTemplateDir `
    -BundleFile (Join-Path $runDir "zed-terminal-config.bundle.json") `
    -WorkingDirectory $packageDir

Invoke-SupportBundleSmoke `
    -Binary $packagedBinary `
    -DataDir (Join-Path $runDir "support-validation-data") `
    -ConfigDir (Join-Path $runDir "support-validation-config") `
    -BundleDir (Join-Path $runDir "zed-terminal-support-bundle") `
    -WorkingDirectory $packageDir

$supportInfo = Invoke-CheckedProcess -FilePath $packagedBinary -Arguments @(
    "--user-data-dir", $validationDataDir,
    "--config-dir", $configTemplateDir,
    "--support-info"
) -WorkingDirectory $packageDir
if (
    $supportInfo.Stdout -notmatch "^Zed Terminal Support Info" -or
    $supportInfo.Stdout -notmatch "app_name: Zed Terminal" -or
    $supportInfo.Stdout -notmatch "status: ok" -or
    $supportInfo.Stdout -notmatch "diagnostics:"
) {
    throw "packaged zed-terminal --support-info did not expose expected package diagnostics"
}

$binaryHash = (Get-FileHash -LiteralPath $packagedBinary -Algorithm SHA256).Hash.ToLowerInvariant()
$readme = New-PackageReadme -PackageName $packageName -BinaryFileName $binaryFileName
Set-Content -LiteralPath (Join-Path $packageDir "README.md") -Value $readme -Encoding utf8

$manifestFile = Join-Path $packageDir "zed-terminal-package.json"
$contents = Get-ChildItem -LiteralPath $packageDir -Recurse -File |
    Where-Object { $_.FullName -ne $manifestFile } |
    ForEach-Object {
        [pscustomobject]@{
            path = Get-RelativePath -Root $packageDir -Path $_.FullName
            bytes = $_.Length
            sha256 = (Get-FileHash -LiteralPath $_.FullName -Algorithm SHA256).Hash.ToLowerInvariant()
        }
    } | Sort-Object path

$manifest = [pscustomobject]@{
    status = "ok"
    app_name = "Zed Terminal"
    package_name = $packageName
    version = $version
    build_profile = $BuildProfile
    platform = $platform
    architecture = $architecture
    git_commit = $gitCommit
    source_control = $sourceControl
    created_at_utc = (Get-Date).ToUniversalTime().ToString("o")
    binary = $binaryFileName
    binary_sha256 = $binaryHash
    version_info = $versionInfoJson
    config_template_dir = "config-template"
    validation = [pscustomobject]@{
        help = "ok"
        version_info = "ok"
        paths = "ok"
        portable_paths = "ok"
        init_config = "ok"
        settings_schema = "ok"
        startup_schema = "ok"
        keymap_schema = "ok"
        default_keymap = "ok"
        startup_layout = "ok"
        startup_discovery = "ok"
        startup_validation = "ok"
        default_keymap_reference = "ok"
        licenses = "ok"
        git_provenance = "ok"
        settings_validation = "ok"
        keymap_validation = "ok"
        keymap_discovery = "ok"
        active_keymap_discovery = "ok"
        settings_backup = "ok"
        startup_backup = "ok"
        keymap_backup = "ok"
        config_bundle = "ok"
        doctor = "ok"
        support_info = "ok"
        support_bundle = "ok"
        readme = "ok"
        manifest = "ok"
    }
    contents = $contents
}
$manifest | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath $manifestFile -Encoding utf8

Assert-PackageManifest `
    -PackageDir $packageDir `
    -ManifestFile $manifestFile `
    -PackageName $packageName `
    -Version $version `
    -BuildProfile $BuildProfile `
    -Platform $platform `
    -Architecture $architecture `
    -BinaryFileName $binaryFileName `
    -BinaryHash $binaryHash

$zipFile = $null
$zipHash = $null
$zipChecksumFile = $null
if ($Zip) {
    $zipFile = Join-Path $runDir "$packageName.zip"
    $zipChecksumFile = "$zipFile.sha256"
    Compress-Archive -LiteralPath $packageDir -DestinationPath $zipFile -Force
    $zipHash = Assert-PackageZipArchive `
        -ZipFile $zipFile `
        -ValidationRoot $runDir `
        -PackageName $packageName `
        -Version $version `
        -BuildProfile $BuildProfile `
        -Platform $platform `
        -Architecture $architecture `
        -BinaryFileName $binaryFileName `
        -BinaryHash $binaryHash
    $zipFileName = Split-Path -Leaf $zipFile
    Set-Content -LiteralPath $zipChecksumFile -Value "$zipHash *$zipFileName" -Encoding ascii
    Assert-ChecksumSidecar `
        -ChecksumFile $zipChecksumFile `
        -ExpectedHash $zipHash `
        -ExpectedFileName $zipFileName
}

$summaryParent = Split-Path -Parent $packageSummaryFile
if ($summaryParent) {
    New-Item -ItemType Directory -Force -Path $summaryParent | Out-Null
}
$packageSummary = [pscustomobject]@{
    status = "ok"
    package_dir = $packageDir
    package_name = $packageName
    version = $version
    build_profile = $BuildProfile
    platform = $platform
    architecture = $architecture
    git_commit = $gitCommit
    source_control = $sourceControl
    manifest_file = $manifestFile
    readme_file = Join-Path $packageDir "README.md"
    config_template_dir = $configTemplateDir
    binary = $packagedBinary
    binary_sha256 = $binaryHash
    version_info = $versionInfoJson
    zip_file = $zipFile
    zip_sha256 = $zipHash
    zip_checksum_file = $zipChecksumFile
    content_count = @($contents).Count
    validation = $manifest.validation
}
$packageSummary | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath $packageSummaryFile -Encoding utf8

Write-Host "status: ok"
Write-Host "package_dir: $packageDir"
Write-Host "manifest_file: $manifestFile"
Write-Host "summary_file: $packageSummaryFile"
Write-Host "binary: $packagedBinary"
Write-Host "binary_sha256: $binaryHash"
if ($zipFile) {
    Write-Host "zip_file: $zipFile"
    Write-Host "zip_sha256: $zipHash"
    Write-Host "zip_checksum_file: $zipChecksumFile"
}
