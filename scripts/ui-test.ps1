# End-to-end mouse-message test against ONLY the generated target/smoke-13 fixture.
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$dir = Join-Path $root 'target\smoke-13'
if (!(Test-Path -LiteralPath (Join-Path $dir 'services\test02\test02.jar'))) { throw 'Run scripts/smoke.ps1 first.' }
Copy-Item -LiteralPath (Join-Path $root 'target\release\cis-cluster-manager.exe') -Destination (Join-Path $dir 'cis-cluster-manager.exe')
Add-Type @'
using System;
using System.Runtime.InteropServices;
public class UiTestWindow {
    [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr hwnd, uint msg, IntPtr wp, IntPtr lp);
    [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr hwnd, int command);
    [DllImport("user32.dll")] public static extern uint GetDpiForWindow(IntPtr hwnd);
    public static void Click(IntPtr hwnd, int x, int y) {
        double scale = GetDpiForWindow(hwnd) / 96.0;
        int pos = ((int)(y * scale) << 16) | (int)(x * scale);
        PostMessage(hwnd, 0x0200, IntPtr.Zero, new IntPtr(pos));
        PostMessage(hwnd, 0x0201, new IntPtr(1), new IntPtr(pos));
        PostMessage(hwnd, 0x0202, IntPtr.Zero, new IntPtr(pos));
    }
}
'@
$conf = Join-Path $dir 'conf'
$original = [System.IO.File]::ReadAllText($conf)
if (!$original.Contains("manage_dir = '$dir\services'")) { throw 'Fixture configuration does not point to the isolated test directory.' }
[System.IO.File]::WriteAllText($conf, $original.Replace('start_check_seconds = 2', 'start_check_seconds = 5'), (New-Object System.Text.UTF8Encoding($false)))
$p = $null
function Open-TestManager {
    $process = Start-Process -FilePath (Join-Path $dir 'cis-cluster-manager.exe') -WindowStyle Normal -PassThru
    $process.WaitForInputIdle(15000) | Out-Null
    Start-Sleep -Seconds 3
    $process.Refresh()
    [UiTestWindow]::ShowWindow($process.MainWindowHandle, 9) | Out-Null
    return $process
}
try {
    $p = Open-TestManager
    [UiTestWindow]::Click($p.MainWindowHandle, 29, 158)
    [UiTestWindow]::Click($p.MainWindowHandle, 29, 203)
    Start-Sleep -Milliseconds 200
    [UiTestWindow]::Click($p.MainWindowHandle, 55, 63)
    $pid1 = Join-Path $dir 'services\test01\test01.pid'
    $pid2 = Join-Path $dir 'services\test02\test02.pid'
    $until = [DateTime]::UtcNow.AddSeconds(10)
    while (!(Test-Path -LiteralPath $pid1) -and [DateTime]::UtcNow -lt $until) { Start-Sleep -Milliseconds 100 }
    if (!(Test-Path -LiteralPath $pid1)) { throw 'Mouse-driven batch start did not create PID metadata.' }
    # A rapid repeat must not queue a second management task.
    [UiTestWindow]::Click($p.MainWindowHandle, 55, 63)
    $p.CloseMainWindow() | Out-Null
    Start-Sleep -Milliseconds 300
    if ($p.HasExited) { throw 'Window closed before its task completed.' }
    if (!$p.WaitForExit(20000)) { throw 'Window did not close after the batch completed.' }
    $one = [System.IO.File]::ReadAllText($pid1) | ConvertFrom-Json
    $two = [System.IO.File]::ReadAllText($pid2) | ConvertFrom-Json
    if ($one.created -ge $two.created) { throw 'Batch order is incorrect.' }
    foreach ($id in @($one, $two)) {
        $java = Get-Process -Id $id.pid -ErrorAction Stop
        if ($java.StartTime.ToUniversalTime().ToFileTimeUtc() -ne $id.created) { throw 'Java identity changed after manager exit.' }
    }
    [System.IO.File]::WriteAllText($conf, $original, (New-Object System.Text.UTF8Encoding($false)))
    $p = Open-TestManager
    [UiTestWindow]::Click($p.MainWindowHandle, 29, 158)
    [UiTestWindow]::Click($p.MainWindowHandle, 29, 203)
    Start-Sleep -Milliseconds 200
    [UiTestWindow]::Click($p.MainWindowHandle, 135, 63)
    $until = [DateTime]::UtcNow.AddSeconds(15)
    while (((Test-Path -LiteralPath $pid1) -or (Test-Path -LiteralPath $pid2)) -and [DateTime]::UtcNow -lt $until) { Start-Sleep -Milliseconds 100 }
    if ((Test-Path -LiteralPath $pid1) -or (Test-Path -LiteralPath $pid2)) { throw 'Mouse-driven batch stop failed.' }
    Write-Output 'PASS: mouse batch start, exclusive submission, name order, deferred close, detached Java survival, recovery and mouse batch stop.'
} finally {
    if ($p -and !$p.HasExited) { $p.CloseMainWindow() | Out-Null; if (!$p.WaitForExit(20000)) { $p.Kill() } }
    [System.IO.File]::WriteAllText($conf, $original, (New-Object System.Text.UTF8Encoding($false)))
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
