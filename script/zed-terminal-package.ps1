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

Run with explicit portable roots:

```powershell
.\{{BINARY}} --user-data-dir .\portable\data --config-dir .\portable\config
```

Preview the resolved startup layout without opening a window:

```powershell
.\{{BINARY}} --print-startup-layout
```

## Configuration

Use `--init-config` to create user-editable files under the active config directory. The packaged `config-template/` directory is a generated reference containing first-run config files, the default keymap, and JSON schemas for editor support.

Key files:

- `terminal.json`: startup layout, profiles, titles, split panes, working directories, and startup commands.
- `keymap.json`: user key bindings for the standalone app.
- `settings.json` and `global_settings.json`: Zed settings loaded by the standalone terminal.
- `terminal.schema.json` and `keymap.schema.json`: generated JSON schemas.

## Diagnostics

Run a read-only health check:

```powershell
.\{{BINARY}} --doctor
```

Run a script-friendly health check:

```powershell
.\{{BINARY}} --doctor --doctor-format json
```

Generate support information without opening a terminal window:

```powershell
.\{{BINARY}} --support-info > zed-terminal-support-info.txt
```

## Included Files

- `{{BINARY}}`: standalone Zed Terminal executable.
- `default-keymap.json`: bundled default standalone keymap reference.
- `config-template/`: generated first-run config, default keymap, and JSON schemas.
- `zed-terminal-package.json`: package manifest with version/build metadata, validation status, file sizes, and SHA256 hashes.
- `LICENSE-GPL` and `LICENSE-APACHE`: repository license files.

The package is validated before release packaging: the binary must pass help, path inspection, config initialization, schema generation, default keymap generation, doctor, support-info, README, manifest, zip extraction, and checksum sidecar checks.
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
        ".\$BinaryFileName --init-config",
        ".\$BinaryFileName --doctor",
        ".\$BinaryFileName --support-info",
        "$PackageName.zip.sha256",
        "config-template/",
        "zed-terminal-package.json"
    )

    foreach ($snippet in $requiredSnippets) {
        if ($readme.IndexOf($snippet, [System.StringComparison]::Ordinal) -lt 0) {
            throw "package README is missing required content: $snippet"
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
        $manifest.config_template_dir -ne "config-template"
    ) {
        throw "package manifest metadata did not match the package that was just built"
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
        "init_config",
        "startup_schema",
        "keymap_schema",
        "default_keymap",
        "doctor",
        "support_info",
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
    if ($pathsJson.config_dir -ne $extractedConfigDir -or $pathsJson.data_dir -ne $extractedDataDir) {
        throw "extracted package zed-terminal --paths did not report the expected standalone paths"
    }

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
        $doctorJson.startup_config.status -ne "ok" -or
        $doctorJson.keymap.status -ne "ok"
    ) {
        throw "extracted package zed-terminal --doctor did not pass against the extracted config template"
    }

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

$gitCommit = $null
try {
    $gitCommit = (Invoke-ProcessCapture -FilePath "git" -Arguments @("rev-parse", "HEAD")).Stdout.Trim()
} catch {
    $gitCommit = $null
}

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
if ($pathsJson.config_dir -ne $validationConfigDir -or $pathsJson.data_dir -ne $validationDataDir) {
    throw "packaged zed-terminal --paths did not report the expected standalone paths"
}

Invoke-CheckedProcess -FilePath $packagedBinary -Arguments @(
    "--user-data-dir", $validationDataDir,
    "--config-dir", $configTemplateDir,
    "--init-config",
    "--init-config-format", "json"
) | Out-Null

$startupSchema = Invoke-CheckedProcess -FilePath $packagedBinary -Arguments @(
    "--user-data-dir", $validationDataDir,
    "--config-dir", $configTemplateDir,
    "--print-startup-config-schema"
)
Set-Content -LiteralPath (Join-Path $configTemplateDir "terminal.schema.json") -Value $startupSchema.Stdout -Encoding utf8

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

foreach ($templateFile in @("settings.json", "keymap.json", "default-keymap.json", "terminal.json", "terminal.schema.json", "keymap.schema.json")) {
    $path = Join-Path $configTemplateDir $templateFile
    if (-not (Test-Path -LiteralPath $path -PathType Leaf)) {
        throw "package config template is missing $templateFile"
    }
}

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
    $doctorJson.startup_config.status -ne "ok" -or
    $doctorJson.keymap.status -ne "ok"
) {
    throw "packaged zed-terminal --doctor did not pass against the generated config template"
}

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
    created_at_utc = (Get-Date).ToUniversalTime().ToString("o")
    binary = $binaryFileName
    binary_sha256 = $binaryHash
    version_info = $versionInfoJson
    config_template_dir = "config-template"
    validation = [pscustomobject]@{
        help = "ok"
        version_info = "ok"
        paths = "ok"
        init_config = "ok"
        startup_schema = "ok"
        keymap_schema = "ok"
        default_keymap = "ok"
        doctor = "ok"
        support_info = "ok"
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
