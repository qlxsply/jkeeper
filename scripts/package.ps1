$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
if (!(Test-Path -LiteralPath (Join-Path $root 'Cargo.toml'))) { throw 'Run from a valid project checkout.' }
& cargo build --release --locked --manifest-path (Join-Path $root 'Cargo.toml')
if ($LASTEXITCODE -ne 0) { throw 'Release build failed.' }
$release = Join-Path $root 'release'
[System.IO.Directory]::CreateDirectory($release) | Out-Null
$exe = Join-Path $root 'target\release\cis-cluster-manager.exe'
$publishedExe = Join-Path $release 'cis-cluster-manager.exe'
try {
    Copy-Item -LiteralPath $exe -Destination $publishedExe -ErrorAction Stop
    Remove-Item -LiteralPath (Join-Path $release 'cis-cluster-manager.next.exe') -Force -ErrorAction SilentlyContinue
} catch {
    # A running GUI executable is locked by Windows. Preserve the running build and stage the new one.
    Copy-Item -LiteralPath $exe -Destination (Join-Path $release 'cis-cluster-manager.next.exe') -Force
    Write-Warning 'cis-cluster-manager.exe is currently running. Close it, then replace it with cis-cluster-manager.next.exe.'
}
# Never overwrite an existing deployment configuration.
if (!(Test-Path -LiteralPath (Join-Path $release 'conf'))) {
    Copy-Item -LiteralPath (Join-Path $root 'conf') -Destination (Join-Path $release 'conf')
}
Copy-Item -LiteralPath (Join-Path $root 'conf.example') -Destination $release
Copy-Item -LiteralPath (Join-Path $root 'README.md') -Destination $release
Copy-Item -LiteralPath (Join-Path $root 'TESTING.md') -Destination $release
Write-Output "Packaged: $release"
