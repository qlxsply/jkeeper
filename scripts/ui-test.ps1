# End-to-end mouse-message test against ONLY the generated target/smoke-13 fixture.
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$dir = Join-Path $root 'target\smoke-13'
if (!(Test-Path -LiteralPath (Join-Path $dir 'services\test02\test02.jar'))) { throw 'Run scripts/smoke.ps1 first.' }
Copy-Item -LiteralPath (Join-Path $root 'target\release\jkeeper.exe') -Destination (Join-Path $dir 'jkeeper.exe')
Add-Type @'
using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;
using System.Text;
public class UiTestWindow {
    [StructLayout(LayoutKind.Sequential)] public struct Rect { public int Left, Top, Right, Bottom; }
    [StructLayout(LayoutKind.Sequential)] public struct Point { public int X, Y; }
    public delegate bool EnumWindowsProc(IntPtr hwnd, IntPtr lParam);
    [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr hwnd, uint msg, IntPtr wp, IntPtr lp);
    [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr hwnd, int command);
    [DllImport("user32.dll")] public static extern uint GetDpiForWindow(IntPtr hwnd);
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr hwnd);
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr hwnd, out Rect rect);
    [DllImport("user32.dll")] public static extern bool ClientToScreen(IntPtr hwnd, ref Point point);
    [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
    [DllImport("user32.dll")] public static extern void mouse_event(uint flags, uint dx, uint dy, uint data, UIntPtr extra);
    [DllImport("user32.dll")] public static extern bool SetWindowPos(IntPtr hwnd, IntPtr insertAfter, int x, int y, int width, int height, uint flags);
    [DllImport("user32.dll")] public static extern bool GetClientRect(IntPtr hwnd, out Rect rect);
    [DllImport("user32.dll")] public static extern bool EnumWindows(EnumWindowsProc callback, IntPtr lParam);
    [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr hwnd, out uint processId);
    [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetClassName(IntPtr hwnd, StringBuilder text, int maxCount);
    [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr hwnd);
    public static string[] WindowClasses(int pid) {
        var classes = new List<string>();
        EnumWindows((hwnd, _) => {
            uint owner; GetWindowThreadProcessId(hwnd, out owner);
            if (owner == (uint)pid && IsWindowVisible(hwnd)) {
                var name = new StringBuilder(256); GetClassName(hwnd, name, name.Capacity); classes.Add(name.ToString());
            }
            return true;
        }, IntPtr.Zero);
        return classes.ToArray();
    }
    public static void CloseOwnedDialogs(int pid) {
        EnumWindows((hwnd, _) => {
            uint owner; GetWindowThreadProcessId(hwnd, out owner);
            if (owner == (uint)pid) {
                var name = new StringBuilder(256); GetClassName(hwnd, name, name.Capacity);
                if (name.ToString() == "#32770") PostMessage(hwnd, 0x0010, IntPtr.Zero, IntPtr.Zero);
            }
            return true;
        }, IntPtr.Zero);
    }
    public static void Click(IntPtr hwnd, int x, int y) {
        double scale = GetDpiForWindow(hwnd) / 96.0;
        int pos = ((int)(y * scale) << 16) | (int)(x * scale);
        PostMessage(hwnd, 0x0200, IntPtr.Zero, new IntPtr(pos));
        PostMessage(hwnd, 0x0201, new IntPtr(1), new IntPtr(pos));
        PostMessage(hwnd, 0x0202, IntPtr.Zero, new IntPtr(pos));
    }
    public static void DoubleClick(IntPtr hwnd, int x, int y) {
        Point origin = new Point(); ClientToScreen(hwnd, ref origin);
        double scale = GetDpiForWindow(hwnd) / 96.0;
        SetCursorPos(origin.X + (int)(x * scale), origin.Y + (int)(y * scale));
        mouse_event(0x0002, 0, 0, 0, UIntPtr.Zero); mouse_event(0x0004, 0, 0, 0, UIntPtr.Zero);
        System.Threading.Thread.Sleep(120);
        mouse_event(0x0002, 0, 0, 0, UIntPtr.Zero); mouse_event(0x0004, 0, 0, 0, UIntPtr.Zero);
    }
}
'@
$conf = Join-Path $dir 'conf.toml'
$original = [System.IO.File]::ReadAllText($conf)
if (!$original.Contains("manage_dir = '$dir\services'")) { throw 'Fixture configuration does not point to the isolated test directory.' }
[System.IO.File]::WriteAllText($conf, $original.Replace('start_check_seconds = 2', 'start_check_seconds = 5'), (New-Object System.Text.UTF8Encoding($false)))
$p = $null
$pid1 = Join-Path $dir 'services\test01\test01.pid'
$pid2 = Join-Path $dir 'services\test02\test02.pid'
$windowState = Join-Path $dir 'services\jkeeper-window.json'
function Open-TestManager {
    $process = Start-Process -FilePath (Join-Path $dir 'jkeeper.exe') -WindowStyle Normal -PassThru
    $process.WaitForInputIdle(15000) | Out-Null
    Start-Sleep -Seconds 3
    $process.Refresh()
    [UiTestWindow]::ShowWindow($process.MainWindowHandle, 9) | Out-Null
    return $process
}
try {
    Remove-Item -LiteralPath $windowState -Force -ErrorAction SilentlyContinue
    $p = Open-TestManager
    [UiTestWindow]::SetWindowPos($p.MainWindowHandle, [IntPtr]::Zero, 72, 64, 1500, 850, 0x0014) | Out-Null
    Start-Sleep -Milliseconds 800
    if (!(Test-Path -LiteralPath $windowState)) { throw 'Window geometry state was not persisted under manage_dir.' }
    $savedWindow = [System.IO.File]::ReadAllText($windowState) | ConvertFrom-Json
    if ($savedWindow.x -lt 60 -or $savedWindow.y -lt 50) { throw "Moved window coordinates were not saved: $savedWindow" }
    $second = Start-Process -FilePath (Join-Path $dir 'jkeeper.exe') -WindowStyle Normal -PassThru
    $second.WaitForInputIdle(10000) | Out-Null
    Start-Sleep -Milliseconds 250
    $classes = [UiTestWindow]::WindowClasses($second.Id)
    if ($classes.Count -eq 0 -or $classes | Where-Object { $_ -ne '#32770' }) { throw "Duplicate jkeeper created a non-error window before rejecting the instance: $($classes -join ', ')" }
    [UiTestWindow]::CloseOwnedDialogs($second.Id)
    if (!$second.WaitForExit(5000)) { $second.Kill(); throw 'Duplicate-instance error dialog did not close.' }
    [UiTestWindow]::Click($p.MainWindowHandle, 44, 158)
    [UiTestWindow]::Click($p.MainWindowHandle, 44, 203)
    Start-Sleep -Milliseconds 200
    [UiTestWindow]::Click($p.MainWindowHandle, 64, 64)
    $until = [DateTime]::UtcNow.AddSeconds(10)
    while (!(Test-Path -LiteralPath $pid1) -and [DateTime]::UtcNow -lt $until) { Start-Sleep -Milliseconds 100 }
    if (!(Test-Path -LiteralPath $pid1)) { throw 'Mouse-driven batch start did not create PID metadata.' }
    # A rapid repeat must not queue a second management task.
    [UiTestWindow]::Click($p.MainWindowHandle, 64, 64)
    $p.CloseMainWindow() | Out-Null
    Start-Sleep -Milliseconds 300
    if ($p.HasExited) { throw 'Window closed before its task completed.' }
    if (!$p.WaitForExit(20000)) { throw 'Window did not close after the batch completed.' }
    if (!(Test-Path -LiteralPath $windowState)) { throw 'Window state file was lost when the manager closed during a task.' }
    $one = [System.IO.File]::ReadAllText($pid1) | ConvertFrom-Json
    $two = [System.IO.File]::ReadAllText($pid2) | ConvertFrom-Json
    if ($one.created -ge $two.created) { throw 'Batch order is incorrect.' }
    foreach ($id in @($one, $two)) {
        $java = Get-Process -Id $id.pid -ErrorAction Stop
        if ($java.StartTime.ToUniversalTime().ToFileTimeUtc() -ne $id.created) { throw 'Java identity changed after manager exit.' }
    }
    [System.IO.File]::WriteAllText($conf, $original, (New-Object System.Text.UTF8Encoding($false)))
    $p = Open-TestManager
    $restoredRect = New-Object UiTestWindow+Rect
    [UiTestWindow]::GetWindowRect($p.MainWindowHandle, [ref]$restoredRect) | Out-Null
    $clientRect = New-Object UiTestWindow+Rect
    [UiTestWindow]::GetClientRect($p.MainWindowHandle, [ref]$clientRect) | Out-Null
    if ([Math]::Abs($restoredRect.Left - $savedWindow.x) -gt 3 -or [Math]::Abs($restoredRect.Top - $savedWindow.y) -gt 3 -or [Math]::Abs(($clientRect.Right - $clientRect.Left) - $savedWindow.width) -gt 8 -or [Math]::Abs(($clientRect.Bottom - $clientRect.Top) - $savedWindow.height) -gt 3) { throw "Window position/size was not restored. Saved=$($savedWindow | ConvertTo-Json -Compress); WindowRect=$($restoredRect | ConvertTo-Json -Compress); ClientRect=$($clientRect | ConvertTo-Json -Compress)" }
    [UiTestWindow]::SetForegroundWindow($p.MainWindowHandle) | Out-Null
    [UiTestWindow]::DoubleClick($p.MainWindowHandle, 100, 158)
    Start-Sleep -Milliseconds 500
    Add-Type -AssemblyName System.Drawing
    $rect = New-Object UiTestWindow+Rect
    [UiTestWindow]::GetWindowRect($p.MainWindowHandle, [ref]$rect) | Out-Null
    $bitmap = New-Object System.Drawing.Bitmap(($rect.Right - $rect.Left), ($rect.Bottom - $rect.Top))
    $graphics = [System.Drawing.Graphics]::FromImage($bitmap)
    $graphics.CopyFromScreen($rect.Left, $rect.Top, 0, 0, $bitmap.Size)
    $bitmap.Save((Join-Path $dir 'detail-window.png'), [System.Drawing.Imaging.ImageFormat]::Png)
    $graphics.Dispose(); $bitmap.Dispose()
    if (!$p.HasExited) {
        if (!(Test-Path -LiteralPath $pid1) -or !(Test-Path -LiteralPath $pid2)) { throw 'Opening details changed service state.' }
        Add-Type -AssemblyName System.Windows.Forms
        [System.Windows.Forms.SendKeys]::SendWait('{ESC}')
        Start-Sleep -Milliseconds 250
    }
    # A row operation button must run the operation, not route the click to details.
    [UiTestWindow]::Click($p.MainWindowHandle, 1168, 158)
    $until = [DateTime]::UtcNow.AddSeconds(10)
    while ((Test-Path -LiteralPath $pid1) -and [DateTime]::UtcNow -lt $until) { Start-Sleep -Milliseconds 100 }
    if (Test-Path -LiteralPath $pid1) { throw 'Stop button was intercepted by the row detail handler.' }
    [UiTestWindow]::Click($p.MainWindowHandle, 44, 203)
    Start-Sleep -Milliseconds 200
    [UiTestWindow]::Click($p.MainWindowHandle, 150, 64)
    $until = [DateTime]::UtcNow.AddSeconds(15)
    while (((Test-Path -LiteralPath $pid1) -or (Test-Path -LiteralPath $pid2)) -and [DateTime]::UtcNow -lt $until) { Start-Sleep -Milliseconds 100 }
    if ((Test-Path -LiteralPath $pid1) -or (Test-Path -LiteralPath $pid2)) { throw 'Mouse-driven batch stop failed.' }
    Write-Output 'PASS: batch start/stop, task exclusion, deferred close, service recovery, row double-click details, operation isolation, and Escape close.'
} finally {
    if ($p -and !$p.HasExited) { $p.CloseMainWindow() | Out-Null; if (!$p.WaitForExit(20000)) { $p.Kill() } }
    [System.IO.File]::WriteAllText($conf, $original, (New-Object System.Text.UTF8Encoding($false)))
    Remove-Item -LiteralPath $windowState -Force -ErrorAction SilentlyContinue
    # Clean up only fixture processes whose PID, exact creation time and executable are verified.
    foreach ($number in 1..13) {
        $name = 'test{0:d2}' -f $number
        $path = Join-Path $dir "services\$name\$name.pid"
        if (Test-Path -LiteralPath $path) {
            $id = [System.IO.File]::ReadAllText($path) | ConvertFrom-Json
            $java = Get-Process -Id $id.pid -ErrorAction SilentlyContinue
            if ($java -and $java.ProcessName -eq 'java' -and $java.StartTime.ToUniversalTime().ToFileTimeUtc() -eq $id.created -and $id.jar -eq (Join-Path $dir "services\$name\$name.jar").ToLowerInvariant()) { $java.Kill(); $java.WaitForExit(); [System.IO.File]::Delete($path) }
        }
    }
}
