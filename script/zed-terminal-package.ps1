[CmdletBinding()]
Param(
    [Parameter()][string]$Binary,
    [Parameter()][string]$OutputDir,
    [Parameter()][ValidateSet("debug", "release")][string]$BuildProfile = "release",
    [Parameter()][string]$Version,
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

$binaryHash = (Get-FileHash -LiteralPath $packagedBinary -Algorithm SHA256).Hash.ToLowerInvariant()
$readme = @"
# Zed Terminal

This package contains the standalone `zed-terminal` binary.

## Run

On Windows:

```powershell
.\$binaryFileName
```

Inspect standalone paths without opening a terminal window:

```powershell
.\$binaryFileName --paths
```

Initialize user-editable config files:

```powershell
.\$binaryFileName --init-config
```

## Included Files

- `$binaryFileName`: standalone Zed Terminal executable.
- `default-keymap.json`: bundled default standalone keymap reference.
- `config-template/`: generated first-run config, default keymap, and JSON schemas.
- `zed-terminal-package.json`: package manifest with SHA256 hashes and validation status.
- `LICENSE-GPL` and `LICENSE-APACHE`: repository license files.

The app uses standalone config, data, and log roots by default. Use `--user-data-dir` and `--config-dir` for portable launches or isolated validation.
"@
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
    config_template_dir = "config-template"
    validation = [pscustomobject]@{
        help = "ok"
        paths = "ok"
        init_config = "ok"
        startup_schema = "ok"
        keymap_schema = "ok"
        default_keymap = "ok"
    }
    contents = $contents
}
$manifest | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath $manifestFile -Encoding utf8

$zipFile = $null
if ($Zip) {
    $zipFile = Join-Path $runDir "$packageName.zip"
    Compress-Archive -LiteralPath $packageDir -DestinationPath $zipFile -Force
}

Write-Host "status: ok"
Write-Host "package_dir: $packageDir"
Write-Host "manifest_file: $manifestFile"
Write-Host "binary: $packagedBinary"
Write-Host "binary_sha256: $binaryHash"
if ($zipFile) {
    Write-Host "zip_file: $zipFile"
}
