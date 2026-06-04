[CmdletBinding()]
Param(
    [Parameter()][string]$Binary,
    [Parameter()][string]$OutputDir,
    [Parameter()][int]$StartupTimeoutSeconds = 20,
    [Parameter()][int]$CaptureDelaySeconds = 4,
    [Parameter()][switch]$KeepRunning
)

$ErrorActionPreference = "Stop"

if (-not [System.Runtime.InteropServices.RuntimeInformation]::IsOSPlatform([System.Runtime.InteropServices.OSPlatform]::Windows)) {
    throw "zed-terminal visual smoke currently requires Windows because it captures a real desktop window."
}

if (-not $Binary) {
    $Binary = Join-Path $PSScriptRoot "..\target\debug\zed-terminal.exe"
}
if (-not $OutputDir) {
    $OutputDir = Join-Path $PSScriptRoot "..\target\zed-terminal-visual-smoke"
}

$Binary = [System.IO.Path]::GetFullPath($Binary)
$OutputDir = [System.IO.Path]::GetFullPath($OutputDir)

if (-not (Test-Path -LiteralPath $Binary -PathType Leaf)) {
    throw "zed-terminal binary not found: $Binary. Build it first with: cargo +stable build -p zed_terminal --bin zed-terminal"
}

New-Item -ItemType Directory -Force -Path $OutputDir | Out-Null

$timestamp = Get-Date -Format "yyyyMMdd-HHmmss"
$runDir = Join-Path $OutputDir "run-$timestamp"
$dataDir = Join-Path $runDir "data"
$configDir = Join-Path $runDir "config"
$probeReadyFile = Join-Path $runDir "probe-ready.txt"
New-Item -ItemType Directory -Force -Path $dataDir, $configDir | Out-Null

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
    $backslashCount = 0

    foreach ($char in $Argument.ToCharArray()) {
        if ($char -eq '\') {
            $backslashCount += 1
        } elseif ($char -eq '"') {
            [void]$builder.Append('\' * (($backslashCount * 2) + 1))
            [void]$builder.Append('"')
            $backslashCount = 0
        } else {
            if ($backslashCount -gt 0) {
                [void]$builder.Append('\' * $backslashCount)
                $backslashCount = 0
            }
            [void]$builder.Append($char)
        }
    }

    if ($backslashCount -gt 0) {
        [void]$builder.Append('\' * ($backslashCount * 2))
    }

    [void]$builder.Append('"')
    return $builder.ToString()
}

function Quote-PowerShellSingleQuotedString {
    param([Parameter(Mandatory = $true)][AllowEmptyString()][string]$Value)

    return "'" + ($Value -replace "'", "''") + "'"
}

Add-Type -AssemblyName System.Drawing
Add-Type -AssemblyName System.Windows.Forms

Add-Type -TypeDefinition @"
using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;
using System.Text;

public static class ZedTerminalVisualSmokeNative {
    private delegate bool EnumWindowsProc(IntPtr hWnd, IntPtr lParam);
    private static readonly IntPtr HWND_TOPMOST = new IntPtr(-1);
    private const int SW_SHOW = 5;
    private const uint PW_RENDERFULLCONTENT = 0x00000002;
    private const uint SWP_SHOWWINDOW = 0x0040;

    [StructLayout(LayoutKind.Sequential)]
    private struct Rect {
        public int Left;
        public int Top;
        public int Right;
        public int Bottom;
    }

    public sealed class WindowInfo {
        public IntPtr Handle;
        public string Title;
        public int Left;
        public int Top;
        public int Width;
        public int Height;
    }

    [DllImport("user32.dll")]
    private static extern bool EnumWindows(EnumWindowsProc lpEnumFunc, IntPtr lParam);

    [DllImport("user32.dll")]
    private static extern bool GetWindowRect(IntPtr hWnd, out Rect lpRect);

    [DllImport("user32.dll")]
    private static extern bool PrintWindow(IntPtr hwnd, IntPtr hdcBlt, uint nFlags);

    [DllImport("user32.dll", CharSet = CharSet.Unicode)]
    private static extern int GetWindowText(IntPtr hWnd, StringBuilder lpString, int nMaxCount);

    [DllImport("user32.dll", CharSet = CharSet.Unicode)]
    private static extern int GetWindowTextLength(IntPtr hWnd);

    [DllImport("user32.dll")]
    private static extern uint GetWindowThreadProcessId(IntPtr hWnd, out uint lpdwProcessId);

    [DllImport("user32.dll")]
    private static extern bool IsWindowVisible(IntPtr hWnd);

    [DllImport("user32.dll")]
    private static extern bool BringWindowToTop(IntPtr hWnd);

    [DllImport("user32.dll")]
    private static extern bool SetForegroundWindow(IntPtr hWnd);

    [DllImport("user32.dll")]
    private static extern bool SetWindowPos(IntPtr hWnd, IntPtr hWndInsertAfter, int X, int Y, int cx, int cy, uint uFlags);

    [DllImport("user32.dll")]
    private static extern bool ShowWindow(IntPtr hWnd, int nCmdShow);

    private static string GetTitle(IntPtr hWnd) {
        var length = GetWindowTextLength(hWnd);
        if (length <= 0) {
            return "";
        }

        var builder = new StringBuilder(length + 1);
        GetWindowText(hWnd, builder, builder.Capacity);
        return builder.ToString();
    }

    public static WindowInfo[] GetVisibleTopLevelWindowsForProcess(int processId) {
        var windows = new List<WindowInfo>();
        EnumWindows((hWnd, lParam) => {
            uint windowProcessId;
            GetWindowThreadProcessId(hWnd, out windowProcessId);
            if (windowProcessId != processId || !IsWindowVisible(hWnd)) {
                return true;
            }

            Rect rect;
            if (!GetWindowRect(hWnd, out rect)) {
                return true;
            }

            var width = rect.Right - rect.Left;
            var height = rect.Bottom - rect.Top;
            if (width < 200 || height < 120) {
                return true;
            }

            windows.Add(new WindowInfo {
                Handle = hWnd,
                Title = GetTitle(hWnd),
                Left = rect.Left,
                Top = rect.Top,
                Width = width,
                Height = height
            });
            return true;
        }, IntPtr.Zero);

        windows.Sort((left, right) => (right.Width * right.Height).CompareTo(left.Width * left.Height));
        return windows.ToArray();
    }

    public static void PositionForCapture(WindowInfo window, int left, int top) {
        ShowWindow(window.Handle, SW_SHOW);
        SetWindowPos(window.Handle, HWND_TOPMOST, left, top, window.Width, window.Height, SWP_SHOWWINDOW);
        BringWindowToTop(window.Handle);
        SetForegroundWindow(window.Handle);
        window.Left = left;
        window.Top = top;
    }

    public static void BringToFront(IntPtr handle) {
        ShowWindow(handle, SW_SHOW);
        BringWindowToTop(handle);
        SetForegroundWindow(handle);
    }

    public static bool PrintWindowContent(IntPtr handle, IntPtr hdc) {
        return PrintWindow(handle, hdc, PW_RENDERFULLCONTENT);
    }
}
"@

function Get-ProcessWindow {
    param(
        [Parameter(Mandatory = $true)][System.Diagnostics.Process]$Process,
        [Parameter(Mandatory = $true)][int]$TimeoutSeconds
    )

    $deadline = (Get-Date).AddSeconds($TimeoutSeconds)
    do {
        if ($Process.HasExited) {
            throw "zed-terminal exited before creating a window. Exit code: $($Process.ExitCode)"
        }

        $windows = [ZedTerminalVisualSmokeNative]::GetVisibleTopLevelWindowsForProcess($Process.Id)
        if ($windows.Length -gt 0) {
            return $windows[0]
        }

        Start-Sleep -Milliseconds 250
    } while ((Get-Date) -lt $deadline)

    throw "Timed out waiting for a visible zed-terminal window for process $($Process.Id)."
}

function Wait-ProbeReadyFile {
    param(
        [Parameter(Mandatory = $true)][System.Diagnostics.Process]$Process,
        [Parameter(Mandatory = $true)][string]$Path,
        [Parameter(Mandatory = $true)][int]$TimeoutSeconds
    )

    $deadline = (Get-Date).AddSeconds($TimeoutSeconds)
    do {
        if ($Process.HasExited) {
            throw "zed-terminal exited before the terminal probe became ready. Exit code: $($Process.ExitCode)"
        }

        if (Test-Path -LiteralPath $Path -PathType Leaf) {
            return
        }

        Start-Sleep -Milliseconds 250
    } while ((Get-Date) -lt $deadline)

    throw "Timed out waiting for terminal probe readiness marker: $Path"
}

function Save-WindowScreenshot {
    param(
        [Parameter(Mandatory = $true)]$Window,
        [Parameter(Mandatory = $true)][string]$Path
    )

    $workingArea = [System.Windows.Forms.Screen]::PrimaryScreen.WorkingArea
    $captureLeft = $workingArea.Left + 32
    $captureTop = $workingArea.Top + 32

    [ZedTerminalVisualSmokeNative]::PositionForCapture($Window, $captureLeft, $captureTop)
    Start-Sleep -Milliseconds 900

    $bitmap = New-Object System.Drawing.Bitmap($Window.Width, $Window.Height)
    $graphics = [System.Drawing.Graphics]::FromImage($bitmap)
    try {
        $hdc = $graphics.GetHdc()
        try {
            $captured = [ZedTerminalVisualSmokeNative]::PrintWindowContent($Window.Handle, $hdc)
        } finally {
            $graphics.ReleaseHdc($hdc)
        }

        if (-not $captured) {
            throw "PrintWindow failed for zed-terminal window handle $($Window.Handle)."
        }

        $bitmap.Save($Path, [System.Drawing.Imaging.ImageFormat]::Png)
    } finally {
        $graphics.Dispose()
        $bitmap.Dispose()
    }
}

function Get-ImageSampleStats {
    param([Parameter(Mandatory = $true)][string]$Path)

    $bitmap = [System.Drawing.Bitmap]::FromFile($Path)
    try {
        $uniqueColors = New-Object "System.Collections.Generic.HashSet[string]"
        $xStep = [Math]::Max(1, [Math]::Floor($bitmap.Width / 64))
        $yStep = [Math]::Max(1, [Math]::Floor($bitmap.Height / 64))
        $sampledPixels = 0

        for ($y = 0; $y -lt $bitmap.Height; $y += $yStep) {
            for ($x = 0; $x -lt $bitmap.Width; $x += $xStep) {
                $color = $bitmap.GetPixel($x, $y)
                [void]$uniqueColors.Add("$($color.A),$($color.R),$($color.G),$($color.B)")
                $sampledPixels += 1
            }
        }

        return [PSCustomObject]@{
            Width = $bitmap.Width
            Height = $bitmap.Height
            SampledPixels = $sampledPixels
            UniqueColors = $uniqueColors.Count
            FileBytes = (Get-Item -LiteralPath $Path).Length
        }
    } finally {
        $bitmap.Dispose()
    }
}

$probeScript = @'
$Host.UI.RawUI.WindowTitle = "zed-terminal-visual-smoke"
Set-Content -LiteralPath __PROBE_READY_FILE__ -Value "ready"
Write-Host "ZED TERMINAL VISUAL SMOKE"
Write-Host "style: official zed terminal renderer"
Write-Host "tabs: shell startup path"
Write-Host "glyphs: ABC xyz 0123456789 <> [] {}"
Write-Host "cwd: $PWD"
while ($true) {
    Start-Sleep -Seconds 60
}
'@
$probeScript = $probeScript.Replace(
    "__PROBE_READY_FILE__",
    (Quote-PowerShellSingleQuotedString $probeReadyFile)
)

$encodedProbeScript = [Convert]::ToBase64String([System.Text.Encoding]::Unicode.GetBytes($probeScript))
$arguments = @(
    "--user-data-dir", $dataDir,
    "--config-dir", $configDir,
    "--no-startup-config",
    "--title", "Visual Smoke",
    "--",
    "powershell.exe",
    "-NoLogo",
    "-NoProfile",
    "-ExecutionPolicy", "Bypass",
    "-EncodedCommand", $encodedProbeScript
)
$argumentLine = ($arguments | ForEach-Object { Quote-ProcessArgument $_ }) -join " "
$process = $null

try {
    $process = Start-Process -FilePath $Binary -ArgumentList $argumentLine -PassThru
    $window = Get-ProcessWindow -Process $process -TimeoutSeconds $StartupTimeoutSeconds
    Wait-ProbeReadyFile -Process $process -Path $probeReadyFile -TimeoutSeconds $StartupTimeoutSeconds

    Start-Sleep -Seconds $CaptureDelaySeconds

    $screenshotPath = Join-Path $runDir "zed-terminal-visual-smoke.png"
    Save-WindowScreenshot -Window $window -Path $screenshotPath
    $stats = Get-ImageSampleStats -Path $screenshotPath

    if ($stats.FileBytes -lt 10000) {
        throw "Screenshot is unexpectedly small: $($stats.FileBytes) bytes."
    }
    if ($stats.UniqueColors -lt 8) {
        throw "Screenshot appears blank or nearly blank: $($stats.UniqueColors) sampled colors."
    }

    Write-Output "status: ok"
    Write-Output "binary: $Binary"
    Write-Output "process_id: $($process.Id)"
    Write-Output "window_handle: $($window.Handle)"
    Write-Output "window_title: $($window.Title)"
    Write-Output "window_bounds: $($window.Left),$($window.Top),$($window.Width),$($window.Height)"
    Write-Output "probe_ready_file: $probeReadyFile"
    Write-Output "capture_method: PrintWindow(PW_RENDERFULLCONTENT)"
    Write-Output "screenshot_file: $screenshotPath"
    Write-Output "screenshot_bytes: $($stats.FileBytes)"
    Write-Output "sampled_pixels: $($stats.SampledPixels)"
    Write-Output "sampled_unique_colors: $($stats.UniqueColors)"
} finally {
    if ($process -and -not $process.HasExited -and -not $KeepRunning) {
        Stop-Process -Id $process.Id -Force -ErrorAction SilentlyContinue
        Wait-Process -Id $process.Id -Timeout 5 -ErrorAction SilentlyContinue
    } elseif ($process -and -not $process.HasExited) {
        Write-Output "kept_process_id: $($process.Id)"
    }
}
