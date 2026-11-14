param([int[]]$ServiceCounts = @(13, 50), [int]$Seconds = 15, [int]$ResizeWidth = 0)
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$exe = Join-Path $root 'target\release\jkeeper.exe'
if (!(Test-Path -LiteralPath $exe)) { throw 'Build the release executable first.' }
Add-Type -AssemblyName System.Drawing
Add-Type @'
using System;
using System.Runtime.InteropServices;
public class SmokeWindow {
    [StructLayout(LayoutKind.Sequential)] public struct Rect { public int Left, Top, Right, Bottom; }
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr hwnd, out Rect rect);
    [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr hwnd, IntPtr hdc, uint flags);
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr hwnd);
    [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr hwnd, int command);
    [DllImport("user32.dll")] public static extern bool IsIconic(IntPtr hwnd);
    [DllImport("user32.dll")] public static extern uint GetDpiForWindow(IntPtr hwnd);
    [DllImport("user32.dll")] public static extern bool SetWindowPos(IntPtr hwnd, IntPtr insertAfter, int x, int y, int width, int height, uint flags);
}
'@
$java = (Get-Command java.exe).Source
$results = @()
foreach ($count in $ServiceCounts) {
    $dir = Join-Path $root "target\smoke-$count"
    $services = Join-Path $dir 'services'
    [System.IO.Directory]::CreateDirectory($services) | Out-Null
    Copy-Item -LiteralPath $exe -Destination (Join-Path $dir 'jkeeper.exe')
    Copy-Item -LiteralPath (Join-Path $root 'tests\fixtures\TestService.java') -Destination $dir
    & javac --release 8 -d $dir (Join-Path $dir 'TestService.java')
    if ($LASTEXITCODE -ne 0) { throw 'javac failed' }
    & jar cfe (Join-Path $dir 'source.jar') TestService -C $dir TestService.class
    if ($LASTEXITCODE -ne 0) { throw 'jar failed' }
    $sourceJar = Join-Path $dir 'source.jar'
    $text = "[general]`nmanage_dir = '$services'`njava_path = '$java'`njava_vm_args = '-Xms32m -Xmx96m'`njava_system_args = ['-Dfile.encoding=utf-8']`njava_agent_args = ''`njava_spring_args = ['--spring.config.location=file:./application.yml,file:./bootstrap.yaml']`nstart_check_seconds = 2`n"
    for ($i = 1; $i -le $count; $i++) {
        $name = 'test{0:d2}' -f $i
        $appdir = Join-Path $services $name
        [System.IO.Directory]::CreateDirectory($appdir) | Out-Null
        Copy-Item -LiteralPath (Join-Path $dir 'source.jar') -Destination (Join-Path $appdir "$name.jar")
        [System.IO.File]::WriteAllText((Join-Path $appdir 'application.yml'), '# isolated smoke test')
        [System.IO.File]::WriteAllText((Join-Path $appdir 'bootstrap.yaml'), '# isolated smoke test')
        $text += "`n[[apps]]`nname = '$name'`ntype = 'java'`nsource_files = ['$sourceJar']`n"
    }
    [System.IO.File]::WriteAllText((Join-Path $dir 'conf.toml'), $text, (New-Object System.Text.UTF8Encoding($false)))
    $watch = [System.Diagnostics.Stopwatch]::StartNew()
    $p = Start-Process -FilePath (Join-Path $dir 'jkeeper.exe') -WindowStyle Normal -PassThru
    try {
        if (!$p.WaitForInputIdle(15000)) { throw 'Window input-idle timeout' }
        if ($ResizeWidth -gt 0) { [SmokeWindow]::SetWindowPos($p.MainWindowHandle, [IntPtr]::Zero, 0, 0, $ResizeWidth, 850, 0x0006) | Out-Null }
        $watch.Stop()
        Start-Sleep -Seconds 3
        $p.Refresh()
        if ($p.HasExited) { throw 'Manager exited unexpectedly' }
        [SmokeWindow]::ShowWindow($p.MainWindowHandle, 9) | Out-Null
        $startCpu = $p.TotalProcessorTime.TotalMilliseconds
        $samples = @()
        $minimizedSamples = 0
        for ($s = 0; $s -lt $Seconds; $s++) {
            Start-Sleep -Seconds 1
            $p.Refresh()
            $samples += $p.WorkingSet64
            if ([SmokeWindow]::IsIconic($p.MainWindowHandle)) { $minimizedSamples++ }
        }
        $cpu = $p.TotalProcessorTime.TotalMilliseconds - $startCpu
        $rect = New-Object SmokeWindow+Rect
        [SmokeWindow]::ShowWindow($p.MainWindowHandle, 9) | Out-Null
        [SmokeWindow]::SetForegroundWindow($p.MainWindowHandle) | Out-Null
        Start-Sleep -Milliseconds 500
        [SmokeWindow]::GetWindowRect($p.MainWindowHandle, [ref]$rect) | Out-Null
        $bitmap = New-Object System.Drawing.Bitmap(($rect.Right - $rect.Left), ($rect.Bottom - $rect.Top))
        $graphics = [System.Drawing.Graphics]::FromImage($bitmap)
        [SmokeWindow]::SetForegroundWindow($p.MainWindowHandle) | Out-Null
        Start-Sleep -Milliseconds 500
        $graphics.CopyFromScreen($rect.Left, $rect.Top, 0, 0, $bitmap.Size)
        $bitmap.Save((Join-Path $dir 'window.png'), [System.Drawing.Imaging.ImageFormat]::Png)
        $graphics.Dispose(); $bitmap.Dispose()
        $results += [pscustomobject]@{
            MinimizedSamples = $minimizedSamples
            WindowWidth = $rect.Right - $rect.Left; WindowHeight = $rect.Bottom - $rect.Top
            WindowDpi = [SmokeWindow]::GetDpiForWindow($p.MainWindowHandle)
            Services = $count; State = 'stopped'; InputIdleMs = $watch.ElapsedMilliseconds
            WorkingSetMiB = [Math]::Round($p.WorkingSet64 / 1MB, 2)
            PeakSampleMiB = [Math]::Round(($samples | Measure-Object -Maximum).Maximum / 1MB, 2)
            PrivateMiB = [Math]::Round($p.PrivateMemorySize64 / 1MB, 2)
            CpuOneCorePercent = [Math]::Round($cpu / ($Seconds * 1000) * 100, 3)
            Screenshot = Join-Path $dir 'window.png'
        }
    } finally {
        if (!$p.HasExited) { $p.CloseMainWindow() | Out-Null; if (!$p.WaitForExit(10000)) { $p.Kill() } }
    }
}
$results | Format-List
[System.IO.File]::WriteAllText((Join-Path $root 'target\smoke-results.json'), ($results | ConvertTo-Json), (New-Object System.Text.UTF8Encoding($false)))
