# Fails if heapscope.exe statically imports a function that older supported Windows lacks.
# IsWow64Process2 (Windows 10 1709+) is looked up at run time instead, see commands/attach.rs.
#   pwsh scripts\check-imports.ps1 [-Exe path\to\heapscope.exe]
param([string]$Exe)
$ErrorActionPreference = 'Stop'
$root = Split-Path $PSScriptRoot -Parent
if (-not $Exe) {
    $Exe = Join-Path $root 'target\x86_64-pc-windows-msvc\release\heapscope.exe'
    if (-not (Test-Path $Exe)) { $Exe = Join-Path $root 'target\release\heapscope.exe' }
    if (-not (Test-Path $Exe)) { $Exe = Join-Path $root 'target\x86_64-pc-windows-msvc\debug\heapscope.exe' }
}
if (-not (Test-Path $Exe)) { throw "heapscope.exe not found: $Exe" }
$vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
$dumpbin = & $vswhere -latest -products * -find 'VC\Tools\MSVC\**\bin\Hostx64\x64\dumpbin.exe' | Select-Object -First 1
if (-not $dumpbin) { throw 'dumpbin.exe not found (install the MSVC build tools)' }
$imports = & $dumpbin /imports $Exe | Out-String
$forbidden = 'IsWow64Process2'
foreach ($name in $forbidden) {
    if ($imports -match "\b$name\b") { throw "$Exe imports $name statically" }
}
"ok: $Exe has no static import of $($forbidden -join ', ')"
