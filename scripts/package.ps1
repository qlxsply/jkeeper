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
# Remove artifacts left by the previous extensionless / executable names.
foreach ($old in @('cis-cluster-manager.exe', 'cis-cluster-manager.next.exe', 'conf', 'conf.example', 'TESTING.md')) {
    Remove-Item -LiteralPath (Join-Path $release $old) -Force -ErrorAction SilentlyContinue
}
# Preserve a user-edited new-format config; seed it from the repository if absent.
if (!(Test-Path -LiteralPath (Join-Path $release 'conf.toml'))) {
    Copy-Item -LiteralPath (Join-Path $root 'conf.toml') -Destination (Join-Path $release 'conf.toml')
}
Copy-Item -LiteralPath (Join-Path $root 'conf.example.toml') -Destination $release
Copy-Item -LiteralPath (Join-Path $root 'README.md') -Destination $release
Write-Output "Packaged: $release"
