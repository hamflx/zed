[CmdletBinding()]
Param(
    [Parameter()][string]$Binary,
    [Parameter()][string]$WorkingDirectory,
    [Parameter()][string]$OutputDir,
    [Parameter()][string]$BaselineImage,
    [Parameter()][string]$UpdateBaselineImage,
    [Parameter()][int]$StartupTimeoutSeconds = 20,
    [Parameter()][int]$CaptureDelaySeconds = 4,
    [Parameter()][double]$MaxBaselineDifferentPixelRatio = 0.02,
    [Parameter()][double]$MaxBaselineAverageChannelDelta = 2.0,
    [Parameter()][int]$BaselinePixelTolerance = 4,
    [Parameter()][switch]$VerifySplitPane,
    [Parameter()][switch]$VerifyShortcuts,
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

if ($BaselineImage -and $UpdateBaselineImage) {
    throw "Use either -BaselineImage to compare against an existing baseline or -UpdateBaselineImage to write a new baseline, not both."
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
if ($VerifySplitPane -and $VerifyShortcuts) {
    throw "Use either -VerifySplitPane or -VerifyShortcuts, not both."
}

$Binary = [System.IO.Path]::GetFullPath($Binary)
if (-not $WorkingDirectory) {
    $WorkingDirectory = (Get-Location).Path
}
$WorkingDirectory = [System.IO.Path]::GetFullPath($WorkingDirectory)
$OutputDir = [System.IO.Path]::GetFullPath($OutputDir)
if ($BaselineImage) {
    $BaselineImage = [System.IO.Path]::GetFullPath($BaselineImage)
}
if ($UpdateBaselineImage) {
    $UpdateBaselineImage = [System.IO.Path]::GetFullPath($UpdateBaselineImage)
}

if (-not (Test-Path -LiteralPath $Binary -PathType Leaf)) {
    throw "zed-terminal binary not found: $Binary. Build it first with: cargo +stable build -p zed_terminal --bin zed-terminal"
}
if (-not (Test-Path -LiteralPath $WorkingDirectory -PathType Container)) {
    throw "Working directory not found: $WorkingDirectory"
}
if ($BaselineImage -and -not (Test-Path -LiteralPath $BaselineImage -PathType Leaf)) {
    throw "Baseline image not found: $BaselineImage"
}

New-Item -ItemType Directory -Force -Path $OutputDir | Out-Null

$timestamp = Get-Date -Format "yyyyMMdd-HHmmss-fff"
$runId = [guid]::NewGuid().ToString("N").Substring(0, 8)
$runDir = Join-Path $OutputDir "run-$timestamp-$runId"
$dataDir = Join-Path $runDir "data"
$configDir = Join-Path $runDir "config"
$probeReadyFile = Join-Path $runDir "probe-ready.txt"
$splitReadyFile = Join-Path $runDir "split-ready.txt"
$shortcutStartupConfigFile = $null
$shortcutSmokeDir = Join-Path $runDir "shortcut-smoke"
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

    [StructLayout(LayoutKind.Sequential)]
    private struct Point {
        public int X;
        public int Y;
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct KeyboardInput {
        public ushort VirtualKey;
        public ushort ScanCode;
        public uint Flags;
        public uint Time;
        public UIntPtr ExtraInfo;
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct MouseInput {
        public int Dx;
        public int Dy;
        public uint MouseData;
        public uint Flags;
        public uint Time;
        public UIntPtr ExtraInfo;
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct HardwareInput {
        public uint Message;
        public ushort ParamL;
        public ushort ParamH;
    }

    [StructLayout(LayoutKind.Explicit)]
    private struct InputUnion {
        [FieldOffset(0)]
        public KeyboardInput Keyboard;

        [FieldOffset(0)]
        public MouseInput Mouse;

        [FieldOffset(0)]
        public HardwareInput Hardware;
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct Input {
        public uint Type;
        public InputUnion Union;
    }

    public sealed class WindowInfo {
        public IntPtr Handle;
        public string Title;
        public string ClassName;
        public int Left;
        public int Top;
        public int Width;
        public int Height;
    }

    public sealed class ClientRegion {
        public int X;
        public int Y;
        public int Width;
        public int Height;
        public int Left;
        public int Top;
    }

    [DllImport("user32.dll")]
    private static extern bool EnumWindows(EnumWindowsProc lpEnumFunc, IntPtr lParam);

    [DllImport("user32.dll")]
    private static extern bool GetWindowRect(IntPtr hWnd, out Rect lpRect);

    [DllImport("user32.dll")]
    private static extern bool GetClientRect(IntPtr hWnd, out Rect lpRect);

    [DllImport("user32.dll")]
    private static extern bool ClientToScreen(IntPtr hWnd, ref Point lpPoint);

    [DllImport("user32.dll")]
    private static extern bool PrintWindow(IntPtr hwnd, IntPtr hdcBlt, uint nFlags);

    [DllImport("user32.dll", CharSet = CharSet.Unicode)]
    private static extern int GetWindowText(IntPtr hWnd, StringBuilder lpString, int nMaxCount);

    [DllImport("user32.dll", CharSet = CharSet.Unicode)]
    private static extern int GetWindowTextLength(IntPtr hWnd);

    [DllImport("user32.dll", CharSet = CharSet.Unicode)]
    private static extern int GetClassName(IntPtr hWnd, StringBuilder lpClassName, int nMaxCount);

    [DllImport("user32.dll")]
    private static extern uint GetWindowThreadProcessId(IntPtr hWnd, out uint lpdwProcessId);

    [DllImport("user32.dll")]
    private static extern bool IsWindowVisible(IntPtr hWnd);

    [DllImport("user32.dll")]
    private static extern bool BringWindowToTop(IntPtr hWnd);

    [DllImport("user32.dll")]
    private static extern bool SetForegroundWindow(IntPtr hWnd);

    [DllImport("user32.dll")]
    private static extern IntPtr GetForegroundWindow();

    [DllImport("user32.dll")]
    private static extern bool SetWindowPos(IntPtr hWnd, IntPtr hWndInsertAfter, int X, int Y, int cx, int cy, uint uFlags);

    [DllImport("user32.dll")]
    private static extern bool ShowWindow(IntPtr hWnd, int nCmdShow);

    [DllImport("user32.dll")]
    private static extern bool SetCursorPos(int X, int Y);

    [DllImport("user32.dll")]
    private static extern void mouse_event(uint dwFlags, uint dx, uint dy, uint dwData, UIntPtr dwExtraInfo);

    [DllImport("user32.dll")]
    private static extern uint MapVirtualKey(uint uCode, uint uMapType);

    [DllImport("user32.dll", SetLastError = true)]
    private static extern uint SendInput(uint nInputs, Input[] pInputs, int cbSize);

    private const uint INPUT_KEYBOARD = 1;
    private const uint KEYEVENTF_KEYUP = 0x0002;
    private const uint MAPVK_VK_TO_VSC = 0x0000;
    private const uint MOUSEEVENTF_LEFTDOWN = 0x0002;
    private const uint MOUSEEVENTF_LEFTUP = 0x0004;

    private static string GetTitle(IntPtr hWnd) {
        var length = GetWindowTextLength(hWnd);
        if (length <= 0) {
            return "";
        }

        var builder = new StringBuilder(length + 1);
        GetWindowText(hWnd, builder, builder.Capacity);
        return builder.ToString();
    }

    private static string GetClass(IntPtr hWnd) {
        var builder = new StringBuilder(256);
        var length = GetClassName(hWnd, builder, builder.Capacity);
        return length <= 0 ? "" : builder.ToString();
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
                ClassName = GetClass(hWnd),
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

    public static ClientRegion GetClientCaptureRegion(WindowInfo window) {
        Rect clientRect;
        if (!GetClientRect(window.Handle, out clientRect)) {
            throw new InvalidOperationException("GetClientRect failed for zed-terminal window.");
        }

        Point clientTopLeft = new Point { X = clientRect.Left, Y = clientRect.Top };
        if (!ClientToScreen(window.Handle, ref clientTopLeft)) {
            throw new InvalidOperationException("ClientToScreen failed for zed-terminal window.");
        }

        return new ClientRegion {
            X = clientTopLeft.X - window.Left,
            Y = clientTopLeft.Y - window.Top,
            Width = clientRect.Right - clientRect.Left,
            Height = clientRect.Bottom - clientRect.Top,
            Left = clientTopLeft.X,
            Top = clientTopLeft.Y
        };
    }

    public static string GetWindowTitle(IntPtr handle) {
        return GetTitle(handle);
    }

    public static void BringToFront(IntPtr handle) {
        ShowWindow(handle, SW_SHOW);
        BringWindowToTop(handle);
        SetForegroundWindow(handle);
    }

    public static void FocusClientArea(WindowInfo window) {
        ShowWindow(window.Handle, SW_SHOW);
        BringWindowToTop(window.Handle);
        SetForegroundWindow(window.Handle);

        var clientRegion = GetClientCaptureRegion(window);
        var x = clientRegion.Left + Math.Min(120, Math.Max(40, clientRegion.Width / 3));
        var y = clientRegion.Top + Math.Min(140, Math.Max(80, clientRegion.Height / 4));
        SetCursorPos(x, y);
        System.Threading.Thread.Sleep(80);
        mouse_event(MOUSEEVENTF_LEFTDOWN, 0, 0, 0, UIntPtr.Zero);
        System.Threading.Thread.Sleep(40);
        mouse_event(MOUSEEVENTF_LEFTUP, 0, 0, 0, UIntPtr.Zero);
        System.Threading.Thread.Sleep(160);
    }

    public static WindowInfo GetForegroundWindowInfo() {
        var handle = GetForegroundWindow();
        if (handle == IntPtr.Zero) {
            return null;
        }

        Rect rect;
        if (!GetWindowRect(handle, out rect)) {
            return new WindowInfo {
                Handle = handle,
                Title = GetTitle(handle),
                ClassName = GetClass(handle),
                Left = 0,
                Top = 0,
                Width = 0,
                Height = 0
            };
        }

        return new WindowInfo {
            Handle = handle,
            Title = GetTitle(handle),
            ClassName = GetClass(handle),
            Left = rect.Left,
            Top = rect.Top,
            Width = rect.Right - rect.Left,
            Height = rect.Bottom - rect.Top
        };
    }

    public static string DescribeWindow(WindowInfo window) {
        if (window == null) {
            return "<none>";
        }

        return window.Handle + ":title='" + window.Title + "':class='" + window.ClassName + "':bounds=" + window.Left + "," + window.Top + "," + window.Width + "," + window.Height;
    }

    public static void SendKeyChord(WindowInfo window, ushort[] modifiers, ushort key) {
        FocusClientArea(window);
        var foreground = GetForegroundWindowInfo();
        if (foreground == null || foreground.Handle != window.Handle) {
            throw new InvalidOperationException("zed-terminal window did not become the foreground window before shortcut injection. expected=" + DescribeWindow(window) + " foreground=" + DescribeWindow(foreground));
        }

        var inputs = new List<Input>();
        foreach (var modifier in modifiers) {
            inputs.Add(KeyInput(modifier, 0));
        }
        inputs.Add(KeyInput(key, 0));
        inputs.Add(KeyInput(key, KEYEVENTF_KEYUP));
        for (var i = modifiers.Length - 1; i >= 0; i--) {
            inputs.Add(KeyInput(modifiers[i], KEYEVENTF_KEYUP));
        }

        var sent = SendInput((uint)inputs.Count, inputs.ToArray(), Marshal.SizeOf(typeof(Input)));
        if (sent != inputs.Count) {
            throw new InvalidOperationException("SendInput sent " + sent + " of " + inputs.Count + " keyboard events. Win32 error: " + Marshal.GetLastWin32Error() + ".");
        }
    }

    private static Input KeyInput(ushort virtualKey, uint flags) {
        return new Input {
            Type = INPUT_KEYBOARD,
            Union = new InputUnion {
                Keyboard = new KeyboardInput {
                    VirtualKey = virtualKey,
                    ScanCode = (ushort)MapVirtualKey(virtualKey, MAPVK_VK_TO_VSC),
                    Flags = flags,
                    Time = 0,
                    ExtraInfo = UIntPtr.Zero
                }
            }
        };
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

function Assert-ZedTerminalWindowTitle {
    param(
        [Parameter(Mandatory = $true)]$Window,
        [Parameter(Mandatory = $true)][string]$ExpectedTitle
    )

    $Window.Title = [ZedTerminalVisualSmokeNative]::GetWindowTitle($Window.Handle)
    if ($Window.Title -ne $ExpectedTitle) {
        throw "zed-terminal window title mismatch. Expected '$ExpectedTitle', got '$($Window.Title)'."
    }
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

function Save-ImageRegion {
    param(
        [Parameter(Mandatory = $true)][string]$SourcePath,
        [Parameter(Mandatory = $true)][string]$DestinationPath,
        [Parameter(Mandatory = $true)][int]$X,
        [Parameter(Mandatory = $true)][int]$Y,
        [Parameter(Mandatory = $true)][int]$Width,
        [Parameter(Mandatory = $true)][int]$Height
    )

    $source = [System.Drawing.Bitmap]::FromFile($SourcePath)
    $destination = $null
    $graphics = $null
    try {
        if ($X -lt 0 -or $Y -lt 0 -or $Width -le 0 -or $Height -le 0) {
            throw "Invalid image region $X,$Y,$Width,$Height for $SourcePath."
        }
        if (($X + $Width) -gt $source.Width -or ($Y + $Height) -gt $source.Height) {
            throw "Image region $X,$Y,$Width,$Height exceeds $SourcePath dimensions $($source.Width)x$($source.Height)."
        }

        $destination = New-Object System.Drawing.Bitmap($Width, $Height)
        $graphics = [System.Drawing.Graphics]::FromImage($destination)
        $destinationRectangle = New-Object System.Drawing.Rectangle -ArgumentList 0, 0, $Width, $Height
        $sourceRectangle = New-Object System.Drawing.Rectangle -ArgumentList $X, $Y, $Width, $Height
        $graphics.DrawImage($source, $destinationRectangle, $sourceRectangle, [System.Drawing.GraphicsUnit]::Pixel)
        $destination.Save($DestinationPath, [System.Drawing.Imaging.ImageFormat]::Png)
    } finally {
        if ($graphics) {
            $graphics.Dispose()
        }
        if ($destination) {
            $destination.Dispose()
        }
        $source.Dispose()
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

function Compare-ImageToBaseline {
    param(
        [Parameter(Mandatory = $true)][string]$ActualPath,
        [Parameter(Mandatory = $true)][string]$BaselinePath,
        [Parameter(Mandatory = $true)][string]$DiffPath,
        [Parameter(Mandatory = $true)][int]$PixelTolerance
    )

    $actual = $null
    $baseline = $null
    $diff = $null
    try {
        $actual = [System.Drawing.Bitmap]::FromFile($ActualPath)
        $baseline = [System.Drawing.Bitmap]::FromFile($BaselinePath)
        if ($actual.Width -ne $baseline.Width -or $actual.Height -ne $baseline.Height) {
            throw "Screenshot dimensions $($actual.Width)x$($actual.Height) do not match baseline dimensions $($baseline.Width)x$($baseline.Height)."
        }

        $diff = New-Object System.Drawing.Bitmap($actual.Width, $actual.Height)
        $totalPixels = [int64]$actual.Width * [int64]$actual.Height
        $differentPixels = [int64]0
        $channelDeltaTotal = [double]0
        $maxChannelDelta = 0

        for ($y = 0; $y -lt $actual.Height; $y += 1) {
            for ($x = 0; $x -lt $actual.Width; $x += 1) {
                $actualColor = $actual.GetPixel($x, $y)
                $baselineColor = $baseline.GetPixel($x, $y)
                $redDelta = [Math]::Abs([int]$actualColor.R - [int]$baselineColor.R)
                $greenDelta = [Math]::Abs([int]$actualColor.G - [int]$baselineColor.G)
                $blueDelta = [Math]::Abs([int]$actualColor.B - [int]$baselineColor.B)
                $alphaDelta = [Math]::Abs([int]$actualColor.A - [int]$baselineColor.A)
                $pixelMaxDelta = [Math]::Max(
                    [Math]::Max($redDelta, $greenDelta),
                    [Math]::Max($blueDelta, $alphaDelta)
                )

                $maxChannelDelta = [Math]::Max($maxChannelDelta, $pixelMaxDelta)
                $channelDeltaTotal += (($redDelta + $greenDelta + $blueDelta) / 3.0)

                if ($pixelMaxDelta -gt $PixelTolerance) {
                    $differentPixels += 1
                    $diffIntensity = [Math]::Min(255, [Math]::Max(48, $pixelMaxDelta))
                    $diff.SetPixel($x, $y, [System.Drawing.Color]::FromArgb(255, $diffIntensity, 0, 0))
                } else {
                    $diff.SetPixel($x, $y, [System.Drawing.Color]::FromArgb(255, 0, 0, 0))
                }
            }
        }

        $diff.Save($DiffPath, [System.Drawing.Imaging.ImageFormat]::Png)
        return [PSCustomObject]@{
            BaselineFile = $BaselinePath
            DiffFile = $DiffPath
            Width = $actual.Width
            Height = $actual.Height
            TotalPixels = $totalPixels
            DifferentPixels = $differentPixels
            DifferentPixelRatio = if ($totalPixels -eq 0) { 0 } else { [double]$differentPixels / [double]$totalPixels }
            AverageChannelDelta = if ($totalPixels -eq 0) { 0 } else { $channelDeltaTotal / [double]$totalPixels }
            MaxChannelDelta = $maxChannelDelta
            PixelTolerance = $PixelTolerance
        }
    } finally {
        if ($diff) {
            $diff.Dispose()
        }
        if ($baseline) {
            $baseline.Dispose()
        }
        if ($actual) {
            $actual.Dispose()
        }
    }
}

function Get-PortableExecutableSubsystem {
    param([Parameter(Mandatory = $true)][string]$Path)

    $bytes = [System.IO.File]::ReadAllBytes($Path)
    if ($bytes.Length -lt 0x100) {
        throw "Binary is too small to be a PE file: $Path"
    }

    $peOffset = [BitConverter]::ToInt32($bytes, 0x3c)
    if ($peOffset -lt 0 -or ($peOffset + 0x5a) -ge $bytes.Length) {
        throw "Invalid PE header offset in binary: $Path"
    }
    if ($bytes[$peOffset] -ne 0x50 -or $bytes[$peOffset + 1] -ne 0x45 -or $bytes[$peOffset + 2] -ne 0 -or $bytes[$peOffset + 3] -ne 0) {
        throw "Missing PE signature in binary: $Path"
    }

    $optionalHeaderOffset = $peOffset + 24
    $subsystem = [BitConverter]::ToUInt16($bytes, $optionalHeaderOffset + 0x44)
    $name = switch ($subsystem) {
        2 { "Windows GUI" }
        3 { "Windows Console" }
        default { "Subsystem $subsystem" }
    }

    return [PSCustomObject]@{
        Value = $subsystem
        Name = $name
    }
}

function Assert-SingleVisibleProcessWindow {
    param([Parameter(Mandatory = $true)][System.Diagnostics.Process]$Process)

    $windows = [ZedTerminalVisualSmokeNative]::GetVisibleTopLevelWindowsForProcess($Process.Id)
    if ($windows.Length -ne 1) {
        $descriptions = $windows | ForEach-Object {
            "$($_.Handle):title='$($_.Title)':class='$($_.ClassName)':bounds=$($_.Left),$($_.Top),$($_.Width),$($_.Height)"
        }
        throw "Expected exactly one visible zed-terminal top-level window for process $($Process.Id), found $($windows.Length): $($descriptions -join '; ')"
    }

    return $windows[0]
}

function Save-ShortcutScreenshot {
    param(
        [Parameter(Mandatory = $true)]$Window,
        [Parameter(Mandatory = $true)][string]$Name,
        [Parameter(Mandatory = $true)][string]$RunDir
    )

    $path = Join-Path $RunDir "zed-terminal-shortcuts-$Name.png"
    Save-WindowScreenshot -Window $Window -Path $path
    $stats = Get-ImageSampleStats -Path $path
    if ($stats.FileBytes -lt 10000) {
        throw "Shortcut screenshot '$Name' is unexpectedly small: $($stats.FileBytes) bytes."
    }
    if ($stats.UniqueColors -lt 8) {
        throw "Shortcut screenshot '$Name' appears blank or nearly blank: $($stats.UniqueColors) sampled colors."
    }
    return [PSCustomObject]@{
        Name = $Name
        Path = $path
        Stats = $stats
    }
}

function Assert-ShortcutScreenshotChanged {
    param(
        [Parameter(Mandatory = $true)]$Before,
        [Parameter(Mandatory = $true)]$After,
        [Parameter(Mandatory = $true)][double]$MinimumDifferentPixelRatio,
        [Parameter(Mandatory = $true)][string]$RunDir
    )

    $diffPath = Join-Path $RunDir "zed-terminal-shortcuts-$($After.Name)-diff.png"
    $comparison = Compare-ImageToBaseline `
        -ActualPath $After.Path `
        -BaselinePath $Before.Path `
        -DiffPath $diffPath `
        -PixelTolerance 4
    if ($comparison.DifferentPixelRatio -lt $MinimumDifferentPixelRatio) {
        throw "Shortcut '$($After.Name)' did not visibly change the terminal enough. different_pixel_ratio=$($comparison.DifferentPixelRatio), minimum=$MinimumDifferentPixelRatio, before=$($Before.Path), after=$($After.Path), diff=$diffPath"
    }

    return $comparison
}

function Read-KeyValueFile {
    param([Parameter(Mandatory = $true)][string]$Path)

    $values = @{}
    foreach ($line in Get-Content -LiteralPath $Path) {
        if ($line -match '^([^:]+):\s*(.*)$') {
            $values[$Matches[1].Trim()] = $Matches[2].Trim()
        }
    }
    return $values
}

function Wait-ShortcutSmokeReady {
    param(
        [Parameter(Mandatory = $true)][System.Diagnostics.Process]$Process,
        [Parameter(Mandatory = $true)][string]$ShortcutSmokeDir,
        [Parameter(Mandatory = $true)][int]$TimeoutSeconds
    )

    $readyFile = Join-Path $ShortcutSmokeDir "shortcut-smoke-ready.txt"
    Wait-ProbeReadyFile -Process $Process -Path $readyFile -TimeoutSeconds $TimeoutSeconds
}

function Invoke-GpuiShortcutSmokeKeystroke {
    param(
        [Parameter(Mandatory = $true)][System.Diagnostics.Process]$Process,
        [Parameter(Mandatory = $true)][string]$ShortcutSmokeDir,
        [Parameter(Mandatory = $true)][string]$Keystroke,
        [Parameter(Mandatory = $true)][int]$Step,
        [Parameter(Mandatory = $true)][int]$TimeoutSeconds
    )

    $requestFile = Join-Path $ShortcutSmokeDir "shortcut-smoke-request.txt"
    $responseFile = Join-Path $ShortcutSmokeDir "shortcut-smoke-response.txt"
    $requestId = "{0:000}-{1}" -f $Step, ([guid]::NewGuid().ToString("N").Substring(0, 8))
    $requestText = "$requestId`n$Keystroke`n"
    $utf8NoBom = New-Object System.Text.UTF8Encoding($false)
    [System.IO.File]::WriteAllText($requestFile, $requestText, $utf8NoBom)

    $deadline = (Get-Date).AddSeconds($TimeoutSeconds)
    do {
        if ($Process.HasExited) {
            throw "zed-terminal exited while waiting for GPUI shortcut smoke response. Exit code: $($Process.ExitCode)"
        }

        if (Test-Path -LiteralPath $responseFile -PathType Leaf) {
            $response = Read-KeyValueFile -Path $responseFile
            if ($response["id"] -eq $requestId) {
                if ($response["status"] -ne "ok") {
                    throw "GPUI shortcut smoke failed for '$Keystroke': $($response["message"])"
                }
                return
            }
        }

        Start-Sleep -Milliseconds 50
    } while ((Get-Date) -lt $deadline)

    throw "Timed out waiting for GPUI shortcut smoke response for '$Keystroke'. request=$requestFile response=$responseFile"
}

function Invoke-GpuiShortcutSmokeStatus {
    param(
        [Parameter(Mandatory = $true)][System.Diagnostics.Process]$Process,
        [Parameter(Mandatory = $true)][string]$ShortcutSmokeDir,
        [Parameter(Mandatory = $true)][int]$Step,
        [Parameter(Mandatory = $true)][int]$TimeoutSeconds
    )

    $requestFile = Join-Path $ShortcutSmokeDir "shortcut-smoke-request.txt"
    $responseFile = Join-Path $ShortcutSmokeDir "shortcut-smoke-response.txt"
    $requestId = "{0:000}-status-{1}" -f $Step, ([guid]::NewGuid().ToString("N").Substring(0, 8))
    $requestText = "$requestId`nstatus`nstatus`n"
    $utf8NoBom = New-Object System.Text.UTF8Encoding($false)
    [System.IO.File]::WriteAllText($requestFile, $requestText, $utf8NoBom)

    $deadline = (Get-Date).AddSeconds($TimeoutSeconds)
    do {
        if ($Process.HasExited) {
            throw "zed-terminal exited while waiting for GPUI shortcut smoke status. Exit code: $($Process.ExitCode)"
        }

        if (Test-Path -LiteralPath $responseFile -PathType Leaf) {
            $response = Read-KeyValueFile -Path $responseFile
            if ($response["id"] -eq $requestId) {
                if ($response["status"] -ne "ok") {
                    throw "GPUI shortcut smoke status failed: $($response["message"])"
                }
                return $response["message"] | ConvertFrom-Json
            }
        }

        Start-Sleep -Milliseconds 50
    } while ((Get-Date) -lt $deadline)

    throw "Timed out waiting for GPUI shortcut smoke status. request=$requestFile response=$responseFile"
}

function Wait-ZedTerminalShortcutState {
    param(
        [Parameter(Mandatory = $true)][System.Diagnostics.Process]$Process,
        [Parameter(Mandatory = $true)][string]$ShortcutSmokeDir,
        [Parameter(Mandatory = $true)][string]$Name,
        [Parameter(Mandatory = $true)][int]$ExpectedOuterPaneCount,
        [Parameter(Mandatory = $true)][int]$ExpectedOuterTabCount,
        [Parameter(Mandatory = $true)][int]$ExpectedInnerPaneCount,
        [Parameter(Mandatory = $true)][bool]$ExpectedCommandPaletteOpen,
        [Parameter(Mandatory = $true)][int]$Step,
        [Parameter(Mandatory = $true)][int]$TimeoutSeconds
    )

    $deadline = (Get-Date).AddSeconds($TimeoutSeconds)
    $lastStatus = $null
    do {
        $lastStatus = Invoke-GpuiShortcutSmokeStatus `
            -Process $Process `
            -ShortcutSmokeDir $ShortcutSmokeDir `
            -Step $Step `
            -TimeoutSeconds $TimeoutSeconds

        if (
            $lastStatus.outer_pane_count -eq $ExpectedOuterPaneCount -and
            $lastStatus.active_outer_tab_count -eq $ExpectedOuterTabCount -and
            $lastStatus.active_terminal_inner_pane_count -eq $ExpectedInnerPaneCount -and
            [bool]$lastStatus.command_palette_open -eq $ExpectedCommandPaletteOpen
        ) {
            return [PSCustomObject]@{
                Name = $Name
                OuterPaneCount = [int]$lastStatus.outer_pane_count
                OuterTabCount = [int]$lastStatus.active_outer_tab_count
                InnerPaneCount = [int]$lastStatus.active_terminal_inner_pane_count
                CommandPaletteOpen = [bool]$lastStatus.command_palette_open
            }
        }

        Start-Sleep -Milliseconds 250
    } while ((Get-Date) -lt $deadline)

    $lastStatusJson = if ($lastStatus) { $lastStatus | ConvertTo-Json -Compress } else { "<none>" }
    throw "Timed out waiting for shortcut state '$Name'. Expected outer_pane_count=$ExpectedOuterPaneCount active_outer_tab_count=$ExpectedOuterTabCount active_terminal_inner_pane_count=$ExpectedInnerPaneCount command_palette_open=$ExpectedCommandPaletteOpen. Last status: $lastStatusJson"
}

function Invoke-ZedTerminalShortcutInput {
    param(
        [Parameter(Mandatory = $true)][System.Diagnostics.Process]$Process,
        [Parameter(Mandatory = $true)]$Window,
        [Parameter(Mandatory = $true)][AllowEmptyCollection()][System.UInt16[]]$Modifiers,
        [Parameter(Mandatory = $true)][System.UInt16]$Key,
        [Parameter(Mandatory = $true)][string]$Keystroke,
        [Parameter(Mandatory = $true)][string]$ShortcutSmokeDir,
        [Parameter(Mandatory = $true)][ref]$InputMode,
        [Parameter(Mandatory = $true)][int]$Step,
        [Parameter(Mandatory = $true)][int]$TimeoutSeconds
    )

    if ($InputMode.Value -eq "gpui-dispatch") {
        Invoke-GpuiShortcutSmokeKeystroke `
            -Process $Process `
            -ShortcutSmokeDir $ShortcutSmokeDir `
            -Keystroke $Keystroke `
            -Step $Step `
            -TimeoutSeconds $TimeoutSeconds
        return
    }

    try {
        [ZedTerminalVisualSmokeNative]::SendKeyChord($Window, $Modifiers, $Key)
        if (-not $InputMode.Value) {
            $InputMode.Value = "windows-sendinput"
        }
    } catch {
        if ($InputMode.Value -eq "windows-sendinput") {
            throw
        }

        $InputMode.Value = "gpui-dispatch"
        Invoke-GpuiShortcutSmokeKeystroke `
            -Process $Process `
            -ShortcutSmokeDir $ShortcutSmokeDir `
            -Keystroke $Keystroke `
            -Step $Step `
            -TimeoutSeconds $TimeoutSeconds
    }
}

function Invoke-ZedTerminalShortcutVerification {
    param(
        [Parameter(Mandatory = $true)][System.Diagnostics.Process]$Process,
        [Parameter(Mandatory = $true)]$Window,
        [Parameter(Mandatory = $true)][string]$RunDir,
        [Parameter(Mandatory = $true)][string]$ShortcutSmokeDir,
        [Parameter(Mandatory = $true)][int]$TimeoutSeconds
    )

    $VK_CONTROL = [System.UInt16]0x11
    $VK_SHIFT = [System.UInt16]0x10
    $VK_MENU = [System.UInt16]0x12
    $VK_ESCAPE = [System.UInt16]0x1b
    $VK_D = [System.UInt16][byte][char]'D'
    $VK_P = [System.UInt16][byte][char]'P'
    $VK_T = [System.UInt16][byte][char]'T'
    $VK_W = [System.UInt16][byte][char]'W'
    $VK_OEM_PLUS = [System.UInt16]0xbb
    $VK_OEM_MINUS = [System.UInt16]0xbd

    $captures = New-Object System.Collections.Generic.List[object]
    $comparisons = New-Object System.Collections.Generic.List[object]
    $states = New-Object System.Collections.Generic.List[object]

    $Window = Assert-SingleVisibleProcessWindow -Process $Process
    $states.Add((Wait-ZedTerminalShortcutState -Process $Process -ShortcutSmokeDir $ShortcutSmokeDir -Name "00-initial" -ExpectedOuterPaneCount 1 -ExpectedOuterTabCount 1 -ExpectedInnerPaneCount 1 -ExpectedCommandPaletteOpen $false -Step 100 -TimeoutSeconds $TimeoutSeconds))
    $initial = Save-ShortcutScreenshot -Window $Window -Name "00-initial" -RunDir $RunDir
    $captures.Add($initial)
    $inputMode = $null

    Invoke-ZedTerminalShortcutInput -Process $Process -Window $Window -Modifiers ([System.UInt16[]]@($VK_CONTROL, $VK_SHIFT)) -Key $VK_T -Keystroke "ctrl-shift-t" -ShortcutSmokeDir $ShortcutSmokeDir -InputMode ([ref]$inputMode) -Step 1 -TimeoutSeconds $TimeoutSeconds
    $states.Add((Wait-ZedTerminalShortcutState -Process $Process -ShortcutSmokeDir $ShortcutSmokeDir -Name "01-ctrl-shift-t-new-tab" -ExpectedOuterPaneCount 1 -ExpectedOuterTabCount 2 -ExpectedInnerPaneCount 1 -ExpectedCommandPaletteOpen $false -Step 101 -TimeoutSeconds $TimeoutSeconds))
    $Window = Assert-SingleVisibleProcessWindow -Process $Process
    $newTab = Save-ShortcutScreenshot -Window $Window -Name "01-ctrl-shift-t-new-tab" -RunDir $RunDir
    $captures.Add($newTab)
    $comparisons.Add((Assert-ShortcutScreenshotChanged -Before $initial -After $newTab -MinimumDifferentPixelRatio 0.001 -RunDir $RunDir))

    Invoke-ZedTerminalShortcutInput -Process $Process -Window $Window -Modifiers ([System.UInt16[]]@($VK_MENU, $VK_SHIFT)) -Key $VK_OEM_PLUS -Keystroke "alt-shift-plus" -ShortcutSmokeDir $ShortcutSmokeDir -InputMode ([ref]$inputMode) -Step 2 -TimeoutSeconds $TimeoutSeconds
    $states.Add((Wait-ZedTerminalShortcutState -Process $Process -ShortcutSmokeDir $ShortcutSmokeDir -Name "02-alt-shift-plus-split-right" -ExpectedOuterPaneCount 1 -ExpectedOuterTabCount 2 -ExpectedInnerPaneCount 2 -ExpectedCommandPaletteOpen $false -Step 102 -TimeoutSeconds $TimeoutSeconds))
    $Window = Assert-SingleVisibleProcessWindow -Process $Process
    $splitRight = Save-ShortcutScreenshot -Window $Window -Name "02-alt-shift-plus-split-right" -RunDir $RunDir
    $captures.Add($splitRight)
    $comparisons.Add((Assert-ShortcutScreenshotChanged -Before $newTab -After $splitRight -MinimumDifferentPixelRatio 0.003 -RunDir $RunDir))

    Invoke-ZedTerminalShortcutInput -Process $Process -Window $Window -Modifiers ([System.UInt16[]]@($VK_MENU, $VK_SHIFT)) -Key $VK_OEM_MINUS -Keystroke "alt-shift-minus" -ShortcutSmokeDir $ShortcutSmokeDir -InputMode ([ref]$inputMode) -Step 3 -TimeoutSeconds $TimeoutSeconds
    $states.Add((Wait-ZedTerminalShortcutState -Process $Process -ShortcutSmokeDir $ShortcutSmokeDir -Name "03-alt-shift-minus-split-down" -ExpectedOuterPaneCount 1 -ExpectedOuterTabCount 2 -ExpectedInnerPaneCount 3 -ExpectedCommandPaletteOpen $false -Step 103 -TimeoutSeconds $TimeoutSeconds))
    $Window = Assert-SingleVisibleProcessWindow -Process $Process
    $splitDown = Save-ShortcutScreenshot -Window $Window -Name "03-alt-shift-minus-split-down" -RunDir $RunDir
    $captures.Add($splitDown)
    $comparisons.Add((Assert-ShortcutScreenshotChanged -Before $splitRight -After $splitDown -MinimumDifferentPixelRatio 0.002 -RunDir $RunDir))

    Invoke-ZedTerminalShortcutInput -Process $Process -Window $Window -Modifiers ([System.UInt16[]]@($VK_MENU, $VK_SHIFT)) -Key $VK_D -Keystroke "alt-shift-d" -ShortcutSmokeDir $ShortcutSmokeDir -InputMode ([ref]$inputMode) -Step 4 -TimeoutSeconds $TimeoutSeconds
    $states.Add((Wait-ZedTerminalShortcutState -Process $Process -ShortcutSmokeDir $ShortcutSmokeDir -Name "04-alt-shift-d-duplicate-split" -ExpectedOuterPaneCount 1 -ExpectedOuterTabCount 2 -ExpectedInnerPaneCount 4 -ExpectedCommandPaletteOpen $false -Step 104 -TimeoutSeconds $TimeoutSeconds))
    $Window = Assert-SingleVisibleProcessWindow -Process $Process
    $duplicateSplit = Save-ShortcutScreenshot -Window $Window -Name "04-alt-shift-d-duplicate-split" -RunDir $RunDir
    $captures.Add($duplicateSplit)
    $comparisons.Add((Assert-ShortcutScreenshotChanged -Before $splitDown -After $duplicateSplit -MinimumDifferentPixelRatio 0.002 -RunDir $RunDir))

    Invoke-ZedTerminalShortcutInput -Process $Process -Window $Window -Modifiers ([System.UInt16[]]@($VK_CONTROL, $VK_SHIFT)) -Key $VK_P -Keystroke "ctrl-shift-p" -ShortcutSmokeDir $ShortcutSmokeDir -InputMode ([ref]$inputMode) -Step 5 -TimeoutSeconds $TimeoutSeconds
    $states.Add((Wait-ZedTerminalShortcutState -Process $Process -ShortcutSmokeDir $ShortcutSmokeDir -Name "05-ctrl-shift-p-command-palette" -ExpectedOuterPaneCount 1 -ExpectedOuterTabCount 2 -ExpectedInnerPaneCount 4 -ExpectedCommandPaletteOpen $true -Step 105 -TimeoutSeconds $TimeoutSeconds))
    $Window = Assert-SingleVisibleProcessWindow -Process $Process
    $commandPalette = Save-ShortcutScreenshot -Window $Window -Name "05-ctrl-shift-p-command-palette" -RunDir $RunDir
    $captures.Add($commandPalette)
    $comparisons.Add((Assert-ShortcutScreenshotChanged -Before $duplicateSplit -After $commandPalette -MinimumDifferentPixelRatio 0.005 -RunDir $RunDir))

    Invoke-ZedTerminalShortcutInput -Process $Process -Window $Window -Modifiers ([System.UInt16[]]@()) -Key $VK_ESCAPE -Keystroke "escape" -ShortcutSmokeDir $ShortcutSmokeDir -InputMode ([ref]$inputMode) -Step 6 -TimeoutSeconds $TimeoutSeconds
    $states.Add((Wait-ZedTerminalShortcutState -Process $Process -ShortcutSmokeDir $ShortcutSmokeDir -Name "06-escape-close-command-palette" -ExpectedOuterPaneCount 1 -ExpectedOuterTabCount 2 -ExpectedInnerPaneCount 4 -ExpectedCommandPaletteOpen $false -Step 106 -TimeoutSeconds $TimeoutSeconds))
    $Window = Assert-SingleVisibleProcessWindow -Process $Process
    $paletteClosed = Save-ShortcutScreenshot -Window $Window -Name "06-escape-close-command-palette" -RunDir $RunDir
    $captures.Add($paletteClosed)
    $comparisons.Add((Assert-ShortcutScreenshotChanged -Before $commandPalette -After $paletteClosed -MinimumDifferentPixelRatio 0.001 -RunDir $RunDir))

    Invoke-ZedTerminalShortcutInput -Process $Process -Window $Window -Modifiers ([System.UInt16[]]@($VK_CONTROL, $VK_SHIFT)) -Key $VK_W -Keystroke "ctrl-shift-w" -ShortcutSmokeDir $ShortcutSmokeDir -InputMode ([ref]$inputMode) -Step 7 -TimeoutSeconds $TimeoutSeconds
    $states.Add((Wait-ZedTerminalShortcutState -Process $Process -ShortcutSmokeDir $ShortcutSmokeDir -Name "07-ctrl-shift-w-close-pane" -ExpectedOuterPaneCount 1 -ExpectedOuterTabCount 2 -ExpectedInnerPaneCount 3 -ExpectedCommandPaletteOpen $false -Step 107 -TimeoutSeconds $TimeoutSeconds))
    $Window = Assert-SingleVisibleProcessWindow -Process $Process
    $closePane = Save-ShortcutScreenshot -Window $Window -Name "07-ctrl-shift-w-close-pane" -RunDir $RunDir
    $captures.Add($closePane)
    $comparisons.Add((Assert-ShortcutScreenshotChanged -Before $paletteClosed -After $closePane -MinimumDifferentPixelRatio 0.001 -RunDir $RunDir))

    return [PSCustomObject]@{
        Captures = $captures
        Comparisons = $comparisons
        States = $states
        FinalWindow = $Window
        InputMode = $inputMode
    }
}

function Write-SplitPaneStartupConfig {
    param(
        [Parameter(Mandatory = $true)][string]$ConfigDir,
        [Parameter(Mandatory = $true)][string]$ReadyFile
    )

    $startupConfigFile = Join-Path $ConfigDir "terminal.json"
    $splitProbeScript = @'
$Host.UI.RawUI.WindowTitle = "zed-terminal-split-smoke"
Set-Content -LiteralPath __SPLIT_READY_FILE__ -Value "ready"
Write-Host "ZED TERMINAL SPLIT SMOKE"
Write-Host "split: startup right"
while ($true) {
    Start-Sleep -Seconds 60
}
'@
    $splitProbeScript = $splitProbeScript.Replace(
        "__SPLIT_READY_FILE__",
        (Quote-PowerShellSingleQuotedString $ReadyFile)
    )
    $encodedSplitProbeScript = [Convert]::ToBase64String(
        [System.Text.Encoding]::Unicode.GetBytes($splitProbeScript)
    )
    $splitCommand = @(
        "powershell.exe",
        "-NoLogo",
        "-NoProfile",
        "-ExecutionPolicy", "Bypass",
        "-EncodedCommand", $encodedSplitProbeScript
    ) | ForEach-Object { Quote-ProcessArgument $_ }
    $splitCommand = $splitCommand -join " "
    $startupConfig = [ordered]@{
        tabs = @(
            [ordered]@{
                title = "Split Smoke"
                command = $splitCommand
                split = "right"
            }
        )
    } | ConvertTo-Json -Depth 6
    $utf8NoBom = New-Object System.Text.UTF8Encoding($false)
    [System.IO.File]::WriteAllText($startupConfigFile, $startupConfig, $utf8NoBom)
    return $startupConfigFile
}

function Write-ShortcutStartupConfig {
    param(
        [Parameter(Mandatory = $true)][string]$ConfigDir,
        [Parameter(Mandatory = $true)][string]$EncodedProbeScript
    )

    $startupConfigFile = Join-Path $ConfigDir "terminal.json"
    $shortcutCommand = @(
        "powershell.exe",
        "-NoLogo",
        "-NoProfile",
        "-ExecutionPolicy", "Bypass",
        "-EncodedCommand", $EncodedProbeScript
    ) | ForEach-Object { Quote-ProcessArgument $_ }
    $shortcutCommand = $shortcutCommand -join " "
    $startupConfig = [ordered]@{
        title = "Shortcut Smoke"
        command = $shortcutCommand
    } | ConvertTo-Json -Depth 6
    $utf8NoBom = New-Object System.Text.UTF8Encoding($false)
    [System.IO.File]::WriteAllText($startupConfigFile, $startupConfig, $utf8NoBom)
    return $startupConfigFile
}

$probeScript = @'
$Host.UI.RawUI.WindowTitle = "zed-terminal-visual-smoke"
Set-Content -LiteralPath __PROBE_READY_FILE__ -Value "ready"
Write-Host "ZED TERMINAL VISUAL SMOKE"
Write-Host "style: official zed terminal renderer"
Write-Host "tabs: shell startup path"
Write-Host "glyphs: ABC xyz 0123456789 <> [] {}"
Write-Host "cwd: isolated smoke workspace"
while ($true) {
    Start-Sleep -Seconds 60
}
'@
$probeScript = $probeScript.Replace(
    "__PROBE_READY_FILE__",
    (Quote-PowerShellSingleQuotedString $probeReadyFile)
)

$encodedProbeScript = [Convert]::ToBase64String([System.Text.Encoding]::Unicode.GetBytes($probeScript))
$startupConfigFile = $null
if ($VerifySplitPane) {
    $startupConfigFile = Write-SplitPaneStartupConfig -ConfigDir $configDir -ReadyFile $splitReadyFile
} elseif ($VerifyShortcuts) {
    $shortcutStartupConfigFile = Write-ShortcutStartupConfig -ConfigDir $configDir -EncodedProbeScript $encodedProbeScript
    $startupConfigFile = $shortcutStartupConfigFile
}

$arguments = if ($VerifyShortcuts) {
    $arguments = @(
        "--user-data-dir", $dataDir,
        "--config-dir", $configDir
    )
    $arguments
} else {
    $arguments = @(
        "--user-data-dir", $dataDir,
        "--config-dir", $configDir,
        "--title", "Visual Smoke",
        "--",
        "powershell.exe",
        "-NoLogo",
        "-NoProfile",
        "-ExecutionPolicy", "Bypass",
        "-EncodedCommand", $encodedProbeScript
    )
    if (-not $VerifySplitPane) {
        $arguments = @(
            "--user-data-dir", $dataDir,
            "--config-dir", $configDir,
            "--no-startup-config"
        ) + $arguments[4..($arguments.Length - 1)]
    }
    $arguments
}
$argumentLine = ($arguments | ForEach-Object { Quote-ProcessArgument $_ }) -join " "
$process = $null
$previousShortcutSmokeDir = $null

try {
    $subsystem = $null
    if ($VerifyShortcuts) {
        $subsystem = Get-PortableExecutableSubsystem -Path $Binary
        if ($subsystem.Value -ne 2) {
            throw "Shortcut E2E requires a Windows GUI subsystem binary so double-click does not create a console window. Actual subsystem: $($subsystem.Name). Build release first."
        }
        New-Item -ItemType Directory -Force -Path $shortcutSmokeDir | Out-Null
        $previousShortcutSmokeDir = [System.Environment]::GetEnvironmentVariable("ZED_TERMINAL_SHORTCUT_SMOKE_DIR", "Process")
        [System.Environment]::SetEnvironmentVariable("ZED_TERMINAL_SHORTCUT_SMOKE_DIR", $shortcutSmokeDir, "Process")
    }

    $process = Start-Process -FilePath $Binary -ArgumentList $argumentLine -WorkingDirectory $WorkingDirectory -PassThru
    $window = Get-ProcessWindow -Process $process -TimeoutSeconds $StartupTimeoutSeconds
    Wait-ProbeReadyFile -Process $process -Path $probeReadyFile -TimeoutSeconds $StartupTimeoutSeconds
    if ($VerifyShortcuts) {
        Wait-ShortcutSmokeReady `
            -Process $process `
            -ShortcutSmokeDir $shortcutSmokeDir `
            -TimeoutSeconds $StartupTimeoutSeconds
    }

    $splitPaneVerified = $false
    if ($VerifySplitPane) {
        Wait-ProbeReadyFile -Process $process -Path $splitReadyFile -TimeoutSeconds $StartupTimeoutSeconds
        $splitPaneVerified = $true
    }

    Start-Sleep -Seconds $CaptureDelaySeconds
    Assert-ZedTerminalWindowTitle -Window $window -ExpectedTitle "Zed Terminal"

    $screenshotPath = Join-Path $runDir "zed-terminal-visual-smoke.png"
    Save-WindowScreenshot -Window $window -Path $screenshotPath
    $windowStats = Get-ImageSampleStats -Path $screenshotPath

    $clientRegion = [ZedTerminalVisualSmokeNative]::GetClientCaptureRegion($window)
    if ($clientRegion.Width -lt 200 -or $clientRegion.Height -lt 120) {
        throw "Captured zed-terminal client area is unexpectedly small: $($clientRegion.Width)x$($clientRegion.Height)."
    }

    $clientScreenshotPath = Join-Path $runDir "zed-terminal-visual-smoke-client.png"
    Save-ImageRegion `
        -SourcePath $screenshotPath `
        -DestinationPath $clientScreenshotPath `
        -X $clientRegion.X `
        -Y $clientRegion.Y `
        -Width $clientRegion.Width `
        -Height $clientRegion.Height

    $comparisonTopInset = 15
    $comparisonX = $clientRegion.X
    $comparisonY = $clientRegion.Y + $comparisonTopInset
    $comparisonWidth = $clientRegion.Width
    $comparisonHeight = $clientRegion.Height - $comparisonTopInset
    if ($comparisonWidth -lt 200 -or $comparisonHeight -lt 120) {
        throw "Captured zed-terminal stable comparison area is unexpectedly small: $($comparisonWidth)x$($comparisonHeight)."
    }

    $comparisonScreenshotPath = Join-Path $runDir "zed-terminal-visual-smoke-comparison.png"
    Save-ImageRegion `
        -SourcePath $screenshotPath `
        -DestinationPath $comparisonScreenshotPath `
        -X $comparisonX `
        -Y $comparisonY `
        -Width $comparisonWidth `
        -Height $comparisonHeight
    $clientStats = Get-ImageSampleStats -Path $clientScreenshotPath
    $comparisonStats = Get-ImageSampleStats -Path $comparisonScreenshotPath

    if ($windowStats.FileBytes -lt 10000) {
        throw "Screenshot is unexpectedly small: $($windowStats.FileBytes) bytes."
    }
    if ($clientStats.FileBytes -lt 10000) {
        throw "Client screenshot is unexpectedly small: $($clientStats.FileBytes) bytes."
    }
    if ($comparisonStats.FileBytes -lt 10000) {
        throw "Stable comparison screenshot is unexpectedly small: $($comparisonStats.FileBytes) bytes."
    }
    if ($comparisonStats.UniqueColors -lt 8) {
        throw "Stable comparison screenshot appears blank or nearly blank: $($comparisonStats.UniqueColors) sampled colors."
    }

    $shortcutVerification = $null
    if ($VerifyShortcuts) {
        $shortcutVerification = Invoke-ZedTerminalShortcutVerification `
            -Process $process `
            -Window $window `
            -RunDir $runDir `
            -ShortcutSmokeDir $shortcutSmokeDir `
            -TimeoutSeconds $StartupTimeoutSeconds
        $window = $shortcutVerification.FinalWindow
    }

    $baselineComparison = $null
    $baselineComparisonOriginalFile = $null
    $baselineComparisonFile = $null
    $baselineUpdated = $false
    if ($BaselineImage) {
        $baselineComparisonOriginalFile = $BaselineImage
        $baselineComparisonFile = Join-Path $runDir "zed-terminal-visual-smoke-baseline-comparison.png"
        Save-ImageRegion `
            -SourcePath $BaselineImage `
            -DestinationPath $baselineComparisonFile `
            -X $comparisonX `
            -Y $comparisonY `
            -Width $comparisonWidth `
            -Height $comparisonHeight

        $diffPath = Join-Path $runDir "zed-terminal-visual-smoke-diff.png"
        $baselineComparison = Compare-ImageToBaseline `
            -ActualPath $comparisonScreenshotPath `
            -BaselinePath $baselineComparisonFile `
            -DiffPath $diffPath `
            -PixelTolerance $BaselinePixelTolerance

        if (
            $baselineComparison.DifferentPixelRatio -gt $MaxBaselineDifferentPixelRatio -or
            $baselineComparison.AverageChannelDelta -gt $MaxBaselineAverageChannelDelta
        ) {
            throw "Screenshot differs from baseline: different_pixel_ratio=$($baselineComparison.DifferentPixelRatio) max=$MaxBaselineDifferentPixelRatio average_channel_delta=$($baselineComparison.AverageChannelDelta) max=$MaxBaselineAverageChannelDelta diff_file=$($baselineComparison.DiffFile)"
        }
    } elseif ($UpdateBaselineImage) {
        $baselineParent = Split-Path -Parent $UpdateBaselineImage
        if ($baselineParent) {
            New-Item -ItemType Directory -Force -Path $baselineParent | Out-Null
        }
        Copy-Item -LiteralPath $screenshotPath -Destination $UpdateBaselineImage -Force
        $baselineUpdated = $true
    }

    Write-Output "status: ok"
    Write-Output "binary: $Binary"
    Write-Output "working_directory: $WorkingDirectory"
    Write-Output "process_id: $($process.Id)"
    Write-Output "window_handle: $($window.Handle)"
    Write-Output "window_title: $($window.Title)"
    Write-Output "window_bounds: $($window.Left),$($window.Top),$($window.Width),$($window.Height)"
    Write-Output "window_client_region: $($clientRegion.X),$($clientRegion.Y),$($clientRegion.Width),$($clientRegion.Height)"
    Write-Output "probe_ready_file: $probeReadyFile"
    if ($startupConfigFile) {
        Write-Output "startup_config_file: $startupConfigFile"
    }
    if ($VerifyShortcuts) {
        Write-Output "shortcut_e2e_verified: True"
        Write-Output "binary_subsystem: $($subsystem.Name)"
        Write-Output "binary_subsystem_value: $($subsystem.Value)"
        Write-Output "shortcut_input_mode: $($shortcutVerification.InputMode)"
        Write-Output "shortcut_startup_config_file: $shortcutStartupConfigFile"
        Write-Output "visible_top_level_windows: 1"
        foreach ($state in $shortcutVerification.States) {
            Write-Output "shortcut_state_$($state.Name)_outer_pane_count: $($state.OuterPaneCount)"
            Write-Output "shortcut_state_$($state.Name)_outer_tab_count: $($state.OuterTabCount)"
            Write-Output "shortcut_state_$($state.Name)_inner_pane_count: $($state.InnerPaneCount)"
            Write-Output "shortcut_state_$($state.Name)_command_palette_open: $($state.CommandPaletteOpen)"
        }
        foreach ($capture in $shortcutVerification.Captures) {
            Write-Output "shortcut_capture_$($capture.Name): $($capture.Path)"
            Write-Output "shortcut_capture_$($capture.Name)_bytes: $($capture.Stats.FileBytes)"
            Write-Output "shortcut_capture_$($capture.Name)_sampled_unique_colors: $($capture.Stats.UniqueColors)"
        }
        foreach ($comparison in $shortcutVerification.Comparisons) {
            Write-Output "shortcut_comparison_$([System.IO.Path]::GetFileNameWithoutExtension($comparison.DiffFile)): different_pixel_ratio=$($comparison.DifferentPixelRatio) average_channel_delta=$($comparison.AverageChannelDelta) diff=$($comparison.DiffFile)"
        }
    }
    if ($VerifySplitPane) {
        Write-Output "split_mode: startup"
        Write-Output "split_direction: right"
        Write-Output "split_ready_file: $splitReadyFile"
        Write-Output "split_pane_verified: $splitPaneVerified"
    }
    Write-Output "capture_method: PrintWindow(PW_RENDERFULLCONTENT)"
    Write-Output "screenshot_file: $screenshotPath"
    Write-Output "screenshot_bytes: $($windowStats.FileBytes)"
    Write-Output "client_screenshot_file: $clientScreenshotPath"
    Write-Output "client_screenshot_bytes: $($clientStats.FileBytes)"
    Write-Output "comparison_region: stable_client_area"
    Write-Output "comparison_top_inset: $comparisonTopInset"
    Write-Output "comparison_client_region: $comparisonX,$comparisonY,$comparisonWidth,$comparisonHeight"
    Write-Output "comparison_screenshot_file: $comparisonScreenshotPath"
    Write-Output "comparison_screenshot_bytes: $($comparisonStats.FileBytes)"
    Write-Output "sampled_pixels: $($comparisonStats.SampledPixels)"
    Write-Output "sampled_unique_colors: $($comparisonStats.UniqueColors)"
    if ($baselineComparison) {
        Write-Output "baseline_file: $baselineComparisonOriginalFile"
        Write-Output "baseline_comparison_file: $baselineComparisonFile"
        Write-Output "baseline_diff_file: $($baselineComparison.DiffFile)"
        Write-Output "baseline_pixels: $($baselineComparison.TotalPixels)"
        Write-Output "baseline_different_pixels: $($baselineComparison.DifferentPixels)"
        Write-Output "baseline_different_pixel_ratio: $($baselineComparison.DifferentPixelRatio)"
        Write-Output "baseline_average_channel_delta: $($baselineComparison.AverageChannelDelta)"
        Write-Output "baseline_max_channel_delta: $($baselineComparison.MaxChannelDelta)"
        Write-Output "baseline_pixel_tolerance: $($baselineComparison.PixelTolerance)"
        Write-Output "baseline_max_different_pixel_ratio: $MaxBaselineDifferentPixelRatio"
        Write-Output "baseline_max_average_channel_delta: $MaxBaselineAverageChannelDelta"
    }
    if ($baselineUpdated) {
        Write-Output "baseline_updated_file: $UpdateBaselineImage"
    }
} finally {
    if ($VerifyShortcuts) {
        [System.Environment]::SetEnvironmentVariable("ZED_TERMINAL_SHORTCUT_SMOKE_DIR", $previousShortcutSmokeDir, "Process")
    }
    if ($process -and -not $process.HasExited -and -not $KeepRunning) {
        Stop-Process -Id $process.Id -Force -ErrorAction SilentlyContinue
        Wait-Process -Id $process.Id -Timeout 5 -ErrorAction SilentlyContinue
    } elseif ($process -and -not $process.HasExited) {
        Write-Output "kept_process_id: $($process.Id)"
    }
}
