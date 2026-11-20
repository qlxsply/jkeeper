$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
if (!(Test-Path -LiteralPath (Join-Path $root 'Cargo.toml'))) { throw 'Run from a valid project checkout.' }
& cargo build --release --locked --manifest-path (Join-Path $root 'Cargo.toml')
if ($LASTEXITCODE -ne 0) { throw 'Release build failed.' }
$release = Join-Path $root 'release'
[System.IO.Directory]::CreateDirectory($release) | Out-Null
$exe = Join-Path $root 'target\release\jkeeper.exe'
$publishedExe = Join-Path $release 'jkeeper.exe'
try {
    Copy-Item -LiteralPath $exe -Destination $publishedExe -ErrorAction Stop
    Remove-Item -LiteralPath (Join-Path $release 'jkeeper.next.exe') -Force -ErrorAction SilentlyContinue
} catch {
    # A running GUI executable is locked by Windows. Preserve the running build and stage the new one.
    Copy-Item -LiteralPath $exe -Destination (Join-Path $release 'jkeeper.next.exe') -Force
    Write-Warning 'jkeeper.exe is currently running. Close it, then replace it with jkeeper.next.exe.'
}
# The project-only debug config is never published as a production cluster.
Remove-Item -LiteralPath (Join-Path $release 'conf.toml') -Force -ErrorAction SilentlyContinue
Remove-Item -LiteralPath (Join-Path $release 'conf-default.toml') -Force -ErrorAction SilentlyContinue
# Copy named cluster configs from the project root, preserving any release-specific edits.
Get-ChildItem -LiteralPath $root -File -Filter 'conf-*.toml' |
    Where-Object { $_.Name -ne 'conf-default.toml' } |
    ForEach-Object {
        $destination = Join-Path $release $_.Name
        if (!(Test-Path -LiteralPath $destination)) {
            Copy-Item -LiteralPath $_.FullName -Destination $destination
        }
    }
Copy-Item -LiteralPath (Join-Path $root 'conf.example.toml') -Destination $release
Copy-Item -LiteralPath (Join-Path $root 'README.md') -Destination $release
Write-Output "Packaged: $release"
