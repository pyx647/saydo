# Run a cargo subcommand (default: check) with retries.
# AhnLab EPP etc. intermittently block freshly built build-scripts/.rmeta ("never executed",
# "extern location does not exist"). cargo caches finished crates, so retrying advances the build.
# Usage: ./build-retry.ps1            -> cargo check
#        ./build-retry.ps1 build      -> cargo build
#        ./build-retry.ps1 test       -> cargo test
param([string]$Sub = "check", [int]$Max = 20)
$ErrorActionPreference = "Continue"

$vcvars = @(
  "C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\VC\Auxiliary\Build\vcvars64.bat",
  "C:\Program Files\Microsoft Visual Studio\2022\BuildTools\VC\Auxiliary\Build\vcvars64.bat"
) | Where-Object { Test-Path $_ } | Select-Object -First 1
if (-not $vcvars) { Write-Host "vcvars64.bat not found"; exit 1 }
cmd /c "`"$vcvars`" >nul && set" | ForEach-Object {
  if ($_ -match '^(.*?)=(.*)$') { [System.Environment]::SetEnvironmentVariable($matches[1], $matches[2]) }
}
$env:Path = ($env:USERPROFILE + "\.cargo\bin;" + $env:Path)
$env:CARGO_TERM_PROGRESS_WHEN = "never"
Set-Location (Join-Path $PSScriptRoot "src-tauri")

for ($i = 1; $i -le $Max; $i++) {
  Write-Host "==== cargo $Sub attempt $i / $Max ===="
  & cargo $Sub
  if ($LASTEXITCODE -eq 0) { Write-Host "CARGO_OK after $i attempt(s)"; exit 0 }
  Write-Host "attempt $i failed (exit $LASTEXITCODE), retrying..."
  Start-Sleep -Seconds 2
}
Write-Host "CARGO_FAILED after $Max attempts"
exit 1
