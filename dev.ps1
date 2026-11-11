# SayDo dev launcher. Loads MSVC env (vcvars64) + cargo, then runs a command.
# Usage:
#   ./dev.ps1                 -> pnpm tauri dev
#   ./dev.ps1 cargo check     -> any command inside the MSVC env (run from src-tauri for cargo)
# NOTE: do NOT run cargo/tauri from Git Bash (MSYS link.exe shadows MSVC link.exe).

$ErrorActionPreference = "Stop"

$vcvars = $null
$candidates = @(
  "C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\VC\Auxiliary\Build\vcvars64.bat",
  "C:\Program Files\Microsoft Visual Studio\2022\BuildTools\VC\Auxiliary\Build\vcvars64.bat",
  "C:\Program Files (x86)\Microsoft Visual Studio\2022\Community\VC\Auxiliary\Build\vcvars64.bat",
  "C:\Program Files\Microsoft Visual Studio\2022\Community\VC\Auxiliary\Build\vcvars64.bat"
)
foreach ($c in $candidates) { if (Test-Path $c) { $vcvars = $c; break } }
if (-not $vcvars) { throw "vcvars64.bat not found. Install VS Build Tools (Desktop development with C++)." }

cmd /c "`"$vcvars`" >nul && set" | ForEach-Object {
  if ($_ -match '^(.*?)=(.*)$') { Set-Item -Path ("env:" + $matches[1]) -Value $matches[2] }
}
$env:Path = ($env:USERPROFILE + "\.cargo\bin;" + $env:Path)
$env:CARGO_TERM_PROGRESS_WHEN = "never"

if ($args.Count -gt 0) {
  & $args[0] @($args | Select-Object -Skip 1)
} else {
  Set-Location $PSScriptRoot
  pnpm tauri dev
}
