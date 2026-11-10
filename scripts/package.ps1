# Build portable package (Phase 6): exes + README + sample config -> dist/
# Usage: powershell -ExecutionPolicy Bypass -File scripts\package.ps1
# (ASCII only: Windows PowerShell 5.1 reads BOM-less scripts as ANSI,
#  which would garble non-ASCII text and break parsing.)

$ErrorActionPreference = "Stop"
Push-Location (Join-Path $PSScriptRoot "..")

cargo build --release
if ($LASTEXITCODE -ne 0) {
    Pop-Location
    Write-Output "!! cargo build failed"
    exit 1
}

$version = ((cargo metadata --no-deps --format-version 1 | ConvertFrom-Json).packages |
    Where-Object { $_.name -eq "sysmon-daemon" }).version
$binDir = "dist\sysmon-$version-windows-x64"
$zipPath = "dist\sysmon-$version-windows-x64.zip"

Remove-Item -Recurse -Force "dist" -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force -Path $binDir | Out-Null

foreach ($exe in "sysmon-daemon.exe", "sysmon-ui.exe", "sysmon-overlay.exe", "sysmon-tui.exe") {
    if (Test-Path "target\release\$exe") { Copy-Item "target\release\$exe" $binDir }
    else { Write-Output "!! missing $exe" }
}
Copy-Item "README.md" $binDir

# sample sysmon.toml — UTF-8 WITHOUT BOM (TOML parse fails with BOM)
cargo run -q -p sysmon-daemon -- --print-config |
    Out-String | ForEach-Object {
        [System.IO.File]::WriteAllText("$PWD\$binDir\sysmon.toml.sample", $_, (New-Object System.Text.UTF8Encoding($false)))
    }

Compress-Archive -Path $binDir -DestinationPath $zipPath -Force
Write-Output "done: $zipPath"

Pop-Location
